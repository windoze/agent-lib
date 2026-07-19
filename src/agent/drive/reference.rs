//! A reference driver: one drain layer wired to real runtime backends.
//!
//! [`drain`] is generic over any [`HandlerScope`]. This module supplies the
//! concrete "single-layer" scope a host uses to run a turn against a live
//! [`LlmClient`], [`ToolRegistry`], and interaction backend, plus the
//! [`drive_turn`] convenience that drains a machine to the end of one turn with
//! no outer layer (migration doc §10, stage 2). It is the canonical driver for
//! the [`AgentMachine`] + [`drain`] path: client / registry / approval wiring
//! reached through sans-io requirements rather than a self-driving loop.
//!
//! # The three handlers
//!
//! - [`LlmClientHandler`] fulfills a `NeedLlm` by calling an [`LlmClient`]
//!   ([`chat`](LlmClient::chat) for [`NonStreaming`](LlmStepMode::NonStreaming),
//!   or [`chat_stream`](LlmClient::chat_stream) folded with
//!   [`collect`](crate::stream::accumulator::collect) for
//!   [`Streaming`](LlmStepMode::Streaming)). The requested mode rides in on the
//!   requirement.
//! - [`ToolRegistryHandler`] fulfills a `NeedTool` by executing the call through
//!   a [`ToolRegistry`].
//! - [`ApprovalInteractionHandler`] fulfills a `NeedInteraction` (approval) by
//!   answering with a fixed [`ApprovalDecision`]. This is the reference
//!   interaction backend: an attended UI that grants or refuses, or an
//!   unattended default disposition. Which tool calls even reach it is decided
//!   *upstream* by the machine's own
//!   [`ToolApprovalPolicy`](crate::agent::ToolApprovalPolicy) (the auto-approve
//!   vs require-approval split), exactly as in the legacy loop.
//!
//! # Run mode is scope wiring
//!
//! [`ReferenceScope`] leaves `interaction` optional. Attaching an interaction
//! backend makes the layer *attended* (approvals resolve here); leaving it off
//! makes the layer *headless*, so any `NeedInteraction` pops to an outer layer
//! (migration doc §4.4 / §6). This module only builds the top, total layer;
//! nested layers land with the subagent handler in M5.

use super::{
    HandlerScope, InteractionHandler, LlmHandler, ReconfigHandler, ToolHandler, TurnDone, drain,
};
use crate::{
    agent::{
        AgentError, AgentInput, AgentMachine, ApprovalDecision, ApprovalResponse,
        DefaultAgentMachine, LlmStepMode, NoToolRegistryResolver, PermissionResponse,
        RequirementResult, RunContext, ToolRegistry, ToolRegistryResolver, ToolRuntimeError,
        ToolSetRef,
        interaction::{Interaction, InteractionKind, InteractionResponse},
    },
    client::{ChatRequest, ClientError, LlmClient},
    conversation::ToolCallId,
    model::tool::ToolCall,
    stream::accumulator::{CollectError, collect},
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

/// Shared, swappable active tool registry.
///
/// The [`ToolRegistryHandler`] reads the current registry from this slot for
/// every tool step, and the [`ReconfigRegistryHandler`] installs a new registry
/// into it when a queued reconfiguration changes the active tool set. Sharing
/// one slot is how a turn-boundary registry swap becomes visible to subsequent
/// tool steps.
type SharedRegistry = Arc<Mutex<Arc<dyn ToolRegistry>>>;

/// Fulfills a `NeedLlm` by running one generation on a shared [`LlmClient`].
///
/// The transport mode arrives on the requirement, so one handler serves both
/// streaming and non-streaming steps. A streaming step is folded back into a
/// complete [`Response`](crate::client::Response) with
/// [`collect`](crate::stream::accumulator::collect); transport failures are
/// carried inside [`RequirementResult::Llm`]'s `Err`, never by returning the
/// wrong result family.
///
/// # Cancellation
///
/// The in-flight LLM call races the run context's cancellation token (M4-5 /
/// M-ERR-2): when the token fires, the HTTP request future is dropped and the
/// handler returns immediately instead of waiting out the response, so cancel
/// latency is bounded by signal delivery rather than by model latency. The
/// placeholder error carried in that early result is always discarded — the
/// driver re-checks the token after every batch and settles the requirement as
/// a never-resume instead of resuming with it. (Tool executions are *not*
/// interrupted mid-flight: a half-finished side effect is worse than a bounded
/// wait for a usually-quick tool call.)
#[derive(Clone)]
pub struct LlmClientHandler {
    client: Arc<dyn LlmClient>,
}

impl LlmClientHandler {
    /// Wraps `client` as an [`LlmHandler`].
    #[must_use]
    pub fn new(client: Arc<dyn LlmClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl LlmHandler for LlmClientHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        let client = self.client.clone();
        let call = async move {
            let mut request = request.clone();
            match mode {
                LlmStepMode::NonStreaming => {
                    request.stream = false;
                    client.chat(request).await
                }
                LlmStepMode::Streaming => {
                    request.stream = true;
                    match client.chat_stream(request).await {
                        Ok(stream) => collect(stream).await.map_err(|error| match error {
                            CollectError::Stream(err) => err,
                            CollectError::Accumulator(err) => {
                                ClientError::Protocol(err.to_string())
                            }
                        }),
                        Err(err) => Err(err),
                    }
                }
            }
        };
        // Biased: an already-cancelled token short-circuits before the client
        // is even called. The placeholder error never reaches the machine —
        // the driver's post-batch cancel re-check settles this requirement as
        // a never-resume and discards the resolution (see `drain`).
        tokio::select! {
            biased;
            _ = ctx.cancellation().cancelled() => RequirementResult::Llm(Err(
                ClientError::Other("llm call interrupted: run context cancelled".to_owned()),
            )),
            result = call => RequirementResult::Llm(result),
        }
    }
}

/// Fulfills a `NeedTool` by executing one call through a shared [`ToolRegistry`].
///
/// The registry is held behind a `SharedRegistry` slot so a turn-boundary
/// reconfiguration ([`ReconfigRegistryHandler`]) can swap it out between steps;
/// each tool step reads the currently-installed registry. Execution failures are
/// carried inside [`RequirementResult::Tool`]'s `Err`; the machine then applies
/// its [`ToolFailurePolicy`](crate::agent::ToolFailurePolicy) on the return path.
#[derive(Clone)]
pub struct ToolRegistryHandler {
    registry: SharedRegistry,
}

impl ToolRegistryHandler {
    /// Wraps `registry` as a [`ToolHandler`] over a fresh, swappable slot.
    #[must_use]
    pub fn new(registry: Arc<dyn ToolRegistry>) -> Self {
        Self {
            registry: Arc::new(Mutex::new(registry)),
        }
    }

    /// Wraps `registry` and returns the matching reconfiguration handler.
    ///
    /// Both handlers share one slot: tool execution reads the currently installed
    /// registry, while a fulfilled `NeedReconfigRegistry` resolves and swaps the
    /// slot through `resolver` before the machine resumes.
    #[must_use]
    pub fn with_reconfig_resolver(
        registry: Arc<dyn ToolRegistry>,
        resolver: Arc<dyn ToolRegistryResolver>,
    ) -> (Self, ReconfigRegistryHandler) {
        let slot: SharedRegistry = Arc::new(Mutex::new(registry));
        (
            Self::from_slot(slot.clone()),
            ReconfigRegistryHandler::new(resolver, slot),
        )
    }

    /// Wraps a shared registry slot, so swaps made through it are observed here.
    fn from_slot(registry: SharedRegistry) -> Self {
        Self { registry }
    }

    /// Returns the currently-installed registry, cloning the handle out from
    /// under the lock so no guard is held across the tool `await`.
    ///
    /// Crate-internal so wrapping handlers (the facade's delegation router) can
    /// consult the *live* slot — a turn-boundary reconfig swap is observed here
    /// immediately, with no separate view to keep in sync.
    pub(crate) fn current(&self) -> Arc<dyn ToolRegistry> {
        self.registry
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

#[async_trait]
impl ToolHandler for ToolRegistryHandler {
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let registry = self.current();
        RequirementResult::Tool(registry.execute(call_id, call.clone()).await)
    }
}

/// Fulfills a `NeedReconfigRegistry` by resolving and installing a new registry.
///
/// When a queued reconfiguration changes the active tool set, the machine parks
/// on a `NeedReconfigRegistry` requirement rather than swapping a registry
/// itself. This handler resolves the requested [`ToolSetRef`] through a
/// [`ToolRegistryResolver`], validates that the resolved registry's declarations
/// match the requested set, and installs it into the `SharedRegistry` slot the
/// [`ToolRegistryHandler`] reads. The confirmation (or a resolution /
/// declaration-mismatch failure) rides back inside
/// [`RequirementResult::Reconfig`].
///
/// The resolver is the *same instance* the machine validated the queued change
/// with: [`ReferenceScope`] derives it from the machine via
/// [`with_machine_tool_resolver`](ReferenceScope::with_machine_tool_resolver),
/// so queue-time validation and apply-time resolution cannot be configured to
/// disagree (M-ERR-3).
#[derive(Clone)]
pub struct ReconfigRegistryHandler {
    resolver: Arc<dyn ToolRegistryResolver>,
    registry: SharedRegistry,
}

impl ReconfigRegistryHandler {
    /// Wires `resolver` and the shared registry slot into a reconfig handler.
    fn new(resolver: Arc<dyn ToolRegistryResolver>, registry: SharedRegistry) -> Self {
        Self { resolver, registry }
    }

    /// Resolves `tool_set`, validates its declarations, and installs the registry.
    fn resolve_and_install(&self, tool_set: &ToolSetRef) -> Result<(), ToolRuntimeError> {
        let registry = self.resolver.resolve_tool_set(tool_set)?;
        if registry.declarations() != tool_set.tools() {
            return Err(ToolRuntimeError::InvalidRegistry {
                message: format!(
                    "registry declarations for tool set {} do not match requested ToolSetRef",
                    tool_set.id()
                ),
            });
        }
        let mut slot = self
            .registry
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = registry;
        Ok(())
    }
}

#[async_trait]
impl ReconfigHandler for ReconfigRegistryHandler {
    async fn fulfill(&self, tool_set: &ToolSetRef, _ctx: &RunContext) -> RequirementResult {
        RequirementResult::Reconfig(self.resolve_and_install(tool_set))
    }
}

/// Fulfills a `NeedInteraction` (approval) with a fixed [`ApprovalDecision`].
///
/// This is the reference interaction backend. It models the return-path
/// decision an attended UI or an unattended default would make: it answers
/// every approval this layer receives with the configured decision. The machine
/// has already decided *which* calls require approval (via its
/// [`ToolApprovalPolicy`](crate::agent::ToolApprovalPolicy)); this handler only
/// resolves the ones that do.
///
/// Non-approval interactions (open [`Question`](InteractionKind::Question) or
/// [`Choice`](InteractionKind::Choice)) are answered with a trivial in-family
/// response so the result still type-aligns with its requirement (an empty
/// answer, or option index `0`); the
/// [`DefaultAgentMachine`](crate::agent::DefaultAgentMachine) never emits them,
/// and an [`ExternalAgentMachine`](crate::agent::ExternalAgentMachine) validates
/// the answer against the pending interaction before relaying it, so this
/// reference backend is only a stand-in suitable for tests and headless
/// defaults — an attended layer should supply a real interaction UI.
///
/// A [`Permission`](InteractionKind::Permission) interaction — surfaced by an
/// external agent runtime rather than the default machine — is answered by
/// mapping the configured [`ApprovalDecision`] onto a
/// [`PermissionResponse`](crate::agent::PermissionResponse) echoing the
/// request's `action_id` (a [`Timeout`](ApprovalDecision::Timeout) folds into a
/// deny). A headless layer that wants a safe default should construct this
/// handler with [`deny`](Self::deny), yielding deny-by-default for every
/// permission ask.
#[derive(Clone, Debug)]
pub struct ApprovalInteractionHandler {
    decision: ApprovalDecision,
    message: Option<String>,
}

impl ApprovalInteractionHandler {
    /// Creates a handler that answers approvals with `decision` and `message`.
    #[must_use]
    pub fn new(decision: ApprovalDecision, message: Option<String>) -> Self {
        Self { decision, message }
    }

    /// Creates a handler that approves every approval interaction.
    #[must_use]
    pub fn approve() -> Self {
        Self::new(ApprovalDecision::Approve, None)
    }

    /// Creates a handler that denies every approval interaction.
    #[must_use]
    pub fn deny(message: Option<String>) -> Self {
        Self::new(ApprovalDecision::Deny, message)
    }
}

#[async_trait]
impl InteractionHandler for ApprovalInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        let response = match request.kind() {
            InteractionKind::Approval { call_id, .. } => {
                InteractionResponse::Approval(ApprovalResponse::new(
                    request.step_id(),
                    *call_id,
                    self.decision,
                    self.message.clone(),
                ))
            }
            InteractionKind::Question { .. } => InteractionResponse::answer(String::new()),
            InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
            InteractionKind::Permission { request } => InteractionResponse::Permission(match self
                .decision
            {
                ApprovalDecision::Approve => {
                    PermissionResponse::approve(request.action_id().to_owned())
                }
                ApprovalDecision::Deny | ApprovalDecision::Timeout => {
                    PermissionResponse::deny(request.action_id().to_owned(), self.message.clone())
                }
                ApprovalDecision::Cancel => {
                    PermissionResponse::cancel(request.action_id().to_owned())
                }
            }),
        };
        RequirementResult::Interaction(response)
    }
}

/// One drain layer wired to live runtime backends.
///
/// Wraps an [`LlmClient`] and a [`ToolRegistry`] into their handlers, plus an
/// optional [`ApprovalInteractionHandler`]. The tool registry lives in a shared,
/// swappable slot so a turn-boundary reconfiguration is fulfilled by resolving
/// and installing a new registry through the machine's own
/// [`ToolRegistryResolver`] (wired via
/// [`with_machine_tool_resolver`](Self::with_machine_tool_resolver); until then
/// the fail-closed [`NoToolRegistryResolver`] rejects any tool-set swap with an
/// explicit error, so an unwired scope can never install a registry the machine
/// did not validate). With an interaction backend the layer is attended
/// (approvals resolve here); without one it is headless and approvals pop
/// outward. Pass it to [`drain`] (or [`drive_turn`]).
pub struct ReferenceScope {
    llm: LlmClientHandler,
    tool: ToolRegistryHandler,
    reconfig: ReconfigRegistryHandler,
    interaction: Option<ApprovalInteractionHandler>,
}

impl ReferenceScope {
    /// Wires `client` and `registry` into a scope with no interaction backend.
    ///
    /// The registry is installed into a shared slot; queued reconfigurations
    /// resolve through the machine's resolver once
    /// [`with_machine_tool_resolver`](Self::with_machine_tool_resolver) clones it
    /// in, and fail with an explicit [`ToolRuntimeError::UnknownToolSet`] until
    /// then.
    #[must_use]
    pub fn new(client: Arc<dyn LlmClient>, registry: Arc<dyn ToolRegistry>) -> Self {
        let slot: SharedRegistry = Arc::new(Mutex::new(registry));
        Self {
            llm: LlmClientHandler::new(client),
            tool: ToolRegistryHandler::from_slot(slot.clone()),
            reconfig: ReconfigRegistryHandler::new(Arc::new(NoToolRegistryResolver), slot),
            interaction: None,
        }
    }

    /// Attaches an interaction backend, making the layer attended.
    #[must_use]
    pub fn with_interaction(mut self, interaction: ApprovalInteractionHandler) -> Self {
        self.interaction = Some(interaction);
        self
    }

    /// Sets the resolver used to fulfill `NeedReconfigRegistry` requirements to
    /// the machine's own resolver.
    ///
    /// This is the only way to arm registry swaps on this scope, and it takes
    /// the resolver from the machine rather than a free-standing value: the
    /// machine validated the queued tool-set change with this same instance, so
    /// the apply-time re-resolution here can never disagree with the queue-time
    /// validation (M-ERR-3).
    #[must_use]
    pub fn with_machine_tool_resolver(mut self, machine: &DefaultAgentMachine) -> Self {
        self.reconfig.resolver = machine.tool_registry_resolver();
        self
    }
}

impl HandlerScope for ReferenceScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        Some(&self.llm)
    }

    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&self.tool)
    }

    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        self.interaction
            .as_ref()
            .map(|handler| handler as &dyn InteractionHandler)
    }

    fn reconfig(&self) -> Option<&dyn ReconfigHandler> {
        Some(&self.reconfig)
    }
}

/// Drives `machine` from a fresh `input` to the end of one turn through `scope`.
///
/// A thin wrapper over [`drain`] with no outer layer: `scope` is the top, total
/// layer, so any requirement it does not handle is an
/// [`AgentError::UnhandledRequirement`](crate::agent::AgentError::UnhandledRequirement).
///
/// # Errors
///
/// Propagates every error [`drain`] can return.
pub async fn drive_turn<M>(
    machine: &mut M,
    input: AgentInput,
    scope: &ReferenceScope,
    ctx: &RunContext,
) -> Result<TurnDone, AgentError>
where
    M: AgentMachine + ?Sized,
{
    drain(machine, input, scope, None, ctx).await
}

#[cfg(test)]
mod tests {
    use super::{ApprovalInteractionHandler, LlmClientHandler};
    use crate::{
        agent::{
            AgentId, ApprovalDecision, ApprovalRequirement, InteractionHandler, LlmHandler,
            LlmStepMode, PermissionCategory, PermissionDecision, PermissionRequest, PermissionRisk,
            RequirementResult,
            context::{BudgetLimits, RunContext, TraceNodeId},
            id::{RunId, StepId},
            interaction::{Interaction, InteractionResponse},
        },
        client::{Capability, ChatRequest, ClientError, LlmClient, Response},
        conversation::ToolCallId,
        stream::StreamEvent,
    };
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use std::sync::Arc;
    use std::time::Duration;

    fn run_id() -> RunId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f1"
            .parse()
            .expect("run id")
    }

    fn step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f2"
            .parse()
            .expect("step id")
    }

    fn agent_id() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f3"
            .parse()
            .expect("agent id")
    }

    fn call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f4"
            .parse()
            .expect("tool call id")
    }

    fn context() -> RunContext {
        RunContext::new_root(run_id(), BudgetLimits::default(), TraceNodeId::new("root"))
    }

    fn permission_request(action_id: &str) -> PermissionRequest {
        PermissionRequest::new(
            action_id.to_owned(),
            agent_id(),
            PermissionCategory::Shell,
            "run `cargo test`".to_owned(),
            serde_json::json!({ "command": "cargo test" }),
            PermissionRisk::Medium,
            Some("verify the refactor".to_owned()),
        )
    }

    /// Fulfills `interaction` through `handler`, asserting an in-family response
    /// the pending interaction accepts, and returns it.
    async fn fulfilled(
        handler: &ApprovalInteractionHandler,
        interaction: &Interaction,
    ) -> InteractionResponse {
        let ctx = context();
        let response = match handler.fulfill(interaction, &ctx).await {
            RequirementResult::Interaction(response) => response,
            other => panic!("interaction handler must return an Interaction result, got {other:?}"),
        };
        interaction
            .accepts_response(&response)
            .expect("the reference handler's response satisfies its interaction");
        response
    }

    #[tokio::test]
    async fn approval_interaction_handler_approves_permission() {
        // `approve()` maps onto a permission approve echoing the request's action.
        let handler = ApprovalInteractionHandler::approve();
        let interaction = Interaction::permission(step_id(), permission_request("act-1"));

        let response = fulfilled(&handler, &interaction).await;

        match response {
            InteractionResponse::Permission(permission) => {
                assert_eq!(permission.action_id(), "act-1");
                assert_eq!(permission.decision(), &PermissionDecision::Approve);
            }
            other => panic!("expected a permission response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_interaction_handler_denies_permission() {
        // `deny()` maps onto a permission deny echoing the action and rationale.
        let handler = ApprovalInteractionHandler::deny(Some("blocked by policy".to_owned()));
        let interaction = Interaction::permission(step_id(), permission_request("act-1"));

        let response = fulfilled(&handler, &interaction).await;

        match response {
            InteractionResponse::Permission(permission) => {
                assert_eq!(permission.action_id(), "act-1");
                assert_eq!(
                    permission.decision(),
                    &PermissionDecision::Deny {
                        reason: Some("blocked by policy".to_owned()),
                    }
                );
            }
            other => panic!("expected a permission response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_interaction_handler_maps_timeout_and_cancel_to_permission_non_approval() {
        // A `Timeout` folds into a permission deny; a `Cancel` into a permission
        // cancel — both non-approving, both echoing the pending action.
        for (decision, expected) in [
            (
                ApprovalDecision::Timeout,
                PermissionDecision::Deny { reason: None },
            ),
            (ApprovalDecision::Cancel, PermissionDecision::Cancel),
        ] {
            let handler = ApprovalInteractionHandler::new(decision, None);
            let interaction = Interaction::permission(step_id(), permission_request("act-1"));

            let response = fulfilled(&handler, &interaction).await;

            match response {
                InteractionResponse::Permission(permission) => {
                    assert_eq!(permission.action_id(), "act-1");
                    assert_eq!(permission.decision(), &expected);
                }
                other => panic!("expected a permission response, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn approval_interaction_handler_answers_question_and_choice_trivially() {
        // Non-approval families get a trivial in-family answer that still aligns
        // with the requirement (empty answer, option index 0).
        let handler = ApprovalInteractionHandler::approve();

        let question = Interaction::question(step_id(), "Which branch?".to_owned());
        assert_eq!(
            fulfilled(&handler, &question).await,
            InteractionResponse::answer(String::new())
        );

        let choice = Interaction::choice(
            step_id(),
            "Pick a branch.".to_owned(),
            vec!["main".to_owned(), "release".to_owned()],
        );
        assert_eq!(
            fulfilled(&handler, &choice).await,
            InteractionResponse::Choice(0)
        );
    }

    /// An [`LlmClient`] whose calls never complete, so a fulfill only returns
    /// through the cancellation race.
    struct BlockingClient {
        capability: Capability,
    }

    #[async_trait]
    impl LlmClient for BlockingClient {
        fn capability(&self) -> &Capability {
            &self.capability
        }

        async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
            futures::future::pending().await
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
            futures::future::pending().await
        }
    }

    fn chat_request() -> ChatRequest {
        ChatRequest {
            model: "test-model".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            system: None,
            max_tokens: 16,
            temperature: None,
            stream: false,
            provider_extras: None,
        }
    }

    #[tokio::test]
    async fn llm_client_handler_returns_promptly_when_cancelled_mid_flight() {
        // The client never answers: without the cancellation race the fulfill
        // would hang forever (M4-5: cancel latency is bounded by signal
        // delivery, not by model latency).
        let handler = LlmClientHandler::new(Arc::new(BlockingClient {
            capability: Capability::default(),
        }));
        let ctx = context();
        let canceller = ctx.cancellation().clone();
        let wake = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            canceller.cancel();
        });

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            handler.fulfill(&chat_request(), LlmStepMode::NonStreaming, &ctx),
        )
        .await
        .expect("a mid-flight cancel must unblock the fulfill");
        wake.await.expect("canceller task");

        let RequirementResult::Llm(Err(error)) = result else {
            panic!("a cancelled call returns an in-family Llm error, got {result:?}");
        };
        assert!(
            error.to_string().contains("cancelled"),
            "the early return explains the cancellation, got: {error}"
        );
    }

    #[tokio::test]
    async fn llm_client_handler_short_circuits_a_pre_cancelled_context() {
        // An already-cancelled token must not even start the client call.
        let handler = LlmClientHandler::new(Arc::new(BlockingClient {
            capability: Capability::default(),
        }));
        let ctx = context();
        ctx.cancellation().cancel();

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            handler.fulfill(&chat_request(), LlmStepMode::Streaming, &ctx),
        )
        .await
        .expect("a pre-cancelled context short-circuits");

        assert!(
            matches!(result, RequirementResult::Llm(Err(_))),
            "a pre-cancelled context returns an in-family Llm error, got {result:?}"
        );
    }

    #[tokio::test]
    async fn approval_interaction_handler_answers_approval_addressing_step_and_call() {
        // A degenerate approval is answered addressing the interaction's own step
        // and tool call, carrying the configured decision.
        let handler = ApprovalInteractionHandler::approve();
        let interaction = Interaction::approval(
            step_id(),
            call_id(),
            ApprovalRequirement::required(Some("touches src/".to_owned())),
        );

        let response = fulfilled(&handler, &interaction).await;

        match response {
            InteractionResponse::Approval(approval) => {
                assert_eq!(approval.step_id(), step_id());
                assert_eq!(approval.call_id(), call_id());
                assert_eq!(approval.decision(), ApprovalDecision::Approve);
            }
            other => panic!("expected an approval response, got {other:?}"),
        }
    }
}
