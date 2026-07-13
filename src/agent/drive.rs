//! Handler scope and effect handlers for driving an
//! [`AgentMachine`].
//!
//! [`AgentMachine::step`] is a pure state
//! machine: it never performs IO, it only *reifies* the IO it needs into
//! [`Requirement`]s. Something outside the machine
//! must actually fulfill those requirements. This module defines that
//! *mechanism* (migration doc §6): a single drain layer is one set of
//! requirement handlers, exposed through a [`HandlerScope`], and the default
//! behavior for any requirement a scope does not handle is to *pop* it to the
//! outer scope.
//!
//! # Scope and handlers
//!
//! A [`HandlerScope`] offers up to four handlers, one per
//! [`RequirementKind`] family. Each accessor
//! defaults to `None`, meaning "this layer cannot fulfill that family" — such a
//! requirement pops outward. A layer overrides only the accessors it can serve:
//!
//! - [`LlmHandler`] fulfills a `NeedLlm` by talking to an
//!   [`LlmClient`](crate::client::LlmClient).
//! - [`ToolHandler`] fulfills a `NeedTool` through a
//!   [`ToolRegistry`](crate::agent::ToolRegistry).
//! - [`InteractionHandler`] fulfills a `NeedInteraction` from an interaction
//!   backend — a human UI (attended) or a
//!   [`ToolApprovalPolicy`](crate::agent::ToolApprovalPolicy) (unattended).
//! - [`SubagentHandler`] fulfills a `NeedSubagent` by deriving and driving a
//!   child agent. Only its *signature* is defined in this stage; the
//!   implementation lands in M5.
//!
//! Handlers are `async` because they perform the real IO the sans-io machine
//! deferred; all `await`ing lives here, never in `step`.
//!
//! # Return-path type alignment
//!
//! Every handler hands back a [`RequirementResult`]. The result *family* must
//! match the requirement kind it fulfills: an [`LlmHandler`] returns
//! [`RequirementResult::Llm`], a [`ToolHandler`] returns
//! [`RequirementResult::Tool`], and so on. Failures are encoded inside the
//! result (for example `RequirementResult::Llm(Err(..))`), not by returning the
//! wrong family. The driver validates this alignment with
//! [`RequirementKind::accepts`] before
//! resuming the machine.
//!
//! # What this module defines
//!
//! [`HandlerScope`] and the four handler traits give one drain layer its effect
//! handlers (M3-1). [`drain`] is the reference driver loop: it pulls an
//! [`AgentMachine`] one [`step`](AgentMachine::step) at a time, fulfills each
//! [`Requirement`] it hands back through the scope
//! (falling back to [`Pop`] routing to an outer layer), and resumes the machine
//! until the turn reaches a terminal cursor (M3-2). A requirement that pops past
//! the top scope with no handler surfaces as
//! [`AgentError::UnhandledRequirement`]. A reference driver that wraps a real
//! client / registry / policy into a single scope and replays the existing loop
//! integration tests lands in M3-3.
//!
//! # Pop routing
//!
//! A layer fulfills only the requirement families its scope handles; anything
//! else *pops* to the outer layer through a [`Pop`]. The routing rules
//! (migration doc §4.2 / §4.3 / §7.3) are:
//!
//! 1. The emitting layer's scope has a matching handler → fulfill in place and
//!    resume; the requirement is invisible to outer layers.
//! 2. No handler at this layer → pop to the parent; each layer the requirement
//!    passes through only forwards it, never reinterprets it.
//! 3. Pop lookup starts at the *outer* layer of the emitter, skipping the
//!    emitter's own scope, so a handler that performs the same requirement
//!    family it fulfills does not immediately re-enter itself.
//! 4. The top layer (`parent = None`) with no handler is a classified error
//!    ([`AgentError::UnhandledRequirement`]), never a silent skip or hang.
//!
//! An outer layer is represented as a [`ScopePop`], which fulfills a popped
//! requirement against its own scope and, failing that, pops further outward.

use crate::{
    agent::{
        AgentError, AgentInput, AgentMachine, LlmStepMode, LoopCursor, LoopCursorKind,
        Notification, Requirement, RequirementKind, RequirementResolution, RequirementResult,
        RunContext, StepInput,
        interaction::Interaction,
        requirement::{AgentSpecRef, RequirementKindTag},
    },
    client::ChatRequest,
    conversation::ToolCallId,
    model::tool::ToolCall,
};
use async_trait::async_trait;
use futures::{StreamExt, stream::FuturesUnordered};
use serde_json::Value;

mod reference;

pub use reference::{
    ApprovalInteractionHandler, LlmClientHandler, ReferenceScope, ToolRegistryHandler, drive_turn,
};

/// One drain layer's set of effect handlers.
///
/// A scope exposes up to four handlers, one per requirement family. Each
/// accessor defaults to `None`, so an empty scope handles nothing and every
/// requirement pops to the outer scope. A layer overrides only the families it
/// can fulfill. See the [module docs](self) for how scopes compose into a drain.
pub trait HandlerScope: Send + Sync {
    /// Returns this layer's [`LlmHandler`], if it fulfills `NeedLlm`.
    fn llm(&self) -> Option<&dyn LlmHandler> {
        None
    }

    /// Returns this layer's [`ToolHandler`], if it fulfills `NeedTool`.
    fn tool(&self) -> Option<&dyn ToolHandler> {
        None
    }

    /// Returns this layer's [`InteractionHandler`], if it fulfills
    /// `NeedInteraction`.
    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        None
    }

    /// Returns this layer's [`SubagentHandler`], if it fulfills `NeedSubagent`.
    fn subagent(&self) -> Option<&dyn SubagentHandler> {
        None
    }
}

/// Fulfills a `NeedLlm` requirement by running one LLM generation.
///
/// The returned [`RequirementResult`] must be a [`RequirementResult::Llm`];
/// transport failures are carried inside its `Err`.
#[async_trait]
pub trait LlmHandler: Send + Sync {
    /// Runs `request` in the requested `mode` and returns the folded result.
    async fn fulfill(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult;
}

/// Fulfills a `NeedTool` requirement by executing one tool call.
///
/// The returned [`RequirementResult`] must be a [`RequirementResult::Tool`];
/// execution failures are carried inside its `Err`.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Executes `call` under the framework `call_id` and returns its result.
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        ctx: &RunContext,
    ) -> RequirementResult;
}

/// Fulfills a `NeedInteraction` requirement from an interaction backend.
///
/// The backend may be a human UI (attended) or a
/// [`ToolApprovalPolicy`](crate::agent::ToolApprovalPolicy) (unattended). The
/// returned [`RequirementResult`] must be a [`RequirementResult::Interaction`]
/// whose response family matches the interaction request.
#[async_trait]
pub trait InteractionHandler: Send + Sync {
    /// Presents `request` to the backend and returns the resolved response.
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult;
}

/// Fulfills a `NeedSubagent` requirement by deriving and driving a child agent.
///
/// This is the only scope-deepening handler: fulfilling it opens another drain
/// layer for the child machine. Only the signature is defined in this stage
/// (M3-1); the derivation, nested drain, and scope enforcement land in M5. The
/// returned [`RequirementResult`] must be a [`RequirementResult::Subagent`].
#[async_trait]
pub trait SubagentHandler: Send + Sync {
    /// Derives the child agent named by `spec_ref`, drives it against `brief`
    /// (optionally constrained by `result_schema`), and returns its result.
    async fn fulfill(
        &self,
        spec_ref: &AgentSpecRef,
        brief: &Interaction,
        result_schema: Option<&Value>,
        ctx: &RunContext,
    ) -> RequirementResult;
}

/// Outcome of draining one machine to the end of a turn.
///
/// Carries the notifications produced across the whole drain (the driver simply
/// forwards them; see migration doc §12 decision C) and the terminal
/// [`LoopCursor`] the machine came to rest on ([`LoopCursor::Done`] or
/// [`LoopCursor::Error`]).
#[derive(Clone, Debug)]
pub struct TurnDone {
    notifications: Vec<Notification>,
    cursor: LoopCursor,
}

impl TurnDone {
    /// Creates a turn result from the drained notifications and terminal cursor.
    #[must_use]
    pub const fn new(notifications: Vec<Notification>, cursor: LoopCursor) -> Self {
        Self {
            notifications,
            cursor,
        }
    }

    /// Returns the notifications produced over the whole drain, in order.
    #[must_use]
    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    /// Returns the terminal cursor the machine came to rest on.
    #[must_use]
    pub const fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }

    /// Consumes the result and returns the drained notifications.
    #[must_use]
    pub fn into_notifications(self) -> Vec<Notification> {
        self.notifications
    }
}

/// Transfers a requirement one layer outward and returns its fulfilled result.
///
/// [`drain`] receives an `Option<&mut dyn Pop>` for the parent layer. A layer
/// that cannot fulfill a requirement hands it to its parent's `pop`; the parent
/// resolves it against the *outer* scope (see the [module docs](self#pop-routing)).
/// The concrete outer layer is a [`ScopePop`].
#[async_trait]
pub trait Pop: Send {
    /// Fulfills `requirement` at this outer layer (or pops it further outward),
    /// returning a type-aligned [`RequirementResult`].
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::UnhandledRequirement`] when the requirement reaches
    /// the top layer with no handler, or a propagated handler error.
    async fn pop(
        &mut self,
        requirement: &Requirement,
        ctx: &RunContext,
    ) -> Result<RequirementResult, AgentError>;
}

/// An outer drain layer viewed as a [`Pop`] target.
///
/// Pairs an outer [`HandlerScope`] with *its own* parent. When a popped
/// requirement arrives, it is fulfilled against this scope if possible, and
/// otherwise popped further outward — so the requirement never re-enters the
/// scope it originally popped from (migration doc §7.3).
pub struct ScopePop<'a> {
    scope: &'a dyn HandlerScope,
    parent: Option<&'a mut dyn Pop>,
}

impl<'a> ScopePop<'a> {
    /// Wraps `scope` (and its own `parent`) as an outer [`Pop`] target.
    #[must_use]
    pub fn new(scope: &'a dyn HandlerScope, parent: Option<&'a mut dyn Pop>) -> Self {
        Self { scope, parent }
    }
}

#[async_trait]
impl Pop for ScopePop<'_> {
    async fn pop(
        &mut self,
        requirement: &Requirement,
        ctx: &RunContext,
    ) -> Result<RequirementResult, AgentError> {
        resolve_requirement(requirement, self.scope, self.parent.as_deref_mut(), ctx).await
    }
}

/// Drives `machine` from a fresh external `input` to the end of one turn.
///
/// The loop is the reference driver (migration doc §6): call
/// [`step`](AgentMachine::step), fulfill every [`Requirement`] the machine hands
/// back — locally through `scope`, or by [`Pop`]ing to `parent` — validate each
/// result's family with [`RequirementKind::accepts`],
/// [`resume`](StepInput::Resume) the machine with it, and repeat until the
/// machine is quiescent with no outstanding requirements and a terminal cursor.
///
/// A single step may hand back a *batch* of requirements (migration decision B);
/// those this layer can fulfill are run concurrently and resumed in completion
/// order, while popped ones are resolved in turn.
///
/// `parent` is the outer layer (`None` at the top). The top layer must be
/// *total*: a requirement with no handler and no parent is an
/// [`AgentError::UnhandledRequirement`].
///
/// # Errors
///
/// Returns [`AgentError::UnhandledRequirement`] when a requirement reaches the
/// top layer unhandled, a propagated handler/pop error, or
/// [`AgentError::Other`] when a handler returns a result whose family does not
/// match the requirement, or when the machine quiesces without a terminal
/// cursor or outstanding requirement.
pub async fn drain<M>(
    machine: &mut M,
    input: AgentInput,
    scope: &dyn HandlerScope,
    mut parent: Option<&mut (dyn Pop + '_)>,
    ctx: &RunContext,
) -> Result<TurnDone, AgentError>
where
    M: AgentMachine + ?Sized,
{
    let mut notifications = Vec::new();

    let mut outcome = machine.step(StepInput::External(input));
    notifications.append(&mut outcome.notifications);
    let mut pending = outcome.requirements;

    loop {
        if pending.is_empty() {
            if is_terminal(machine.cursor()) {
                break;
            }
            return Err(AgentError::Other(format!(
                "machine quiesced without a terminal cursor or outstanding requirement \
                 (cursor: {:?})",
                machine.cursor().kind()
            )));
        }

        let resolutions = fulfill_batch(&pending, scope, parent.as_deref_mut(), ctx).await?;

        pending = Vec::new();
        for resolution in resolutions {
            let mut outcome = machine.step(StepInput::Resume(resolution));
            notifications.append(&mut outcome.notifications);
            pending.extend(outcome.requirements);
        }
    }

    Ok(TurnDone::new(notifications, machine.cursor().clone()))
}

/// Returns whether `cursor` marks the end of a turn.
fn is_terminal(cursor: &LoopCursor) -> bool {
    matches!(cursor.kind(), LoopCursorKind::Done | LoopCursorKind::Error)
}

/// Returns whether `scope` offers a handler for the given requirement family.
fn scope_handles(scope: &dyn HandlerScope, tag: RequirementKindTag) -> bool {
    match tag {
        RequirementKindTag::Llm => scope.llm().is_some(),
        RequirementKindTag::Tool => scope.tool().is_some(),
        RequirementKindTag::Interaction => scope.interaction().is_some(),
        RequirementKindTag::Subagent => scope.subagent().is_some(),
    }
}

/// Fulfills `requirement` with this scope's handler, if it has one.
///
/// Returns `None` when the scope does not offer a handler for the requirement's
/// family (the caller then pops it outward).
async fn fulfill_with_scope(
    requirement: &Requirement,
    scope: &dyn HandlerScope,
    ctx: &RunContext,
) -> Option<RequirementResult> {
    match &requirement.kind {
        RequirementKind::NeedLlm { request, mode } => {
            Some(scope.llm()?.fulfill(request, *mode, ctx).await)
        }
        RequirementKind::NeedTool { call_id, call } => {
            Some(scope.tool()?.fulfill(*call_id, call, ctx).await)
        }
        RequirementKind::NeedInteraction { request } => {
            Some(scope.interaction()?.fulfill(request, ctx).await)
        }
        RequirementKind::NeedSubagent {
            spec_ref,
            brief,
            result_schema,
        } => Some(
            scope
                .subagent()?
                .fulfill(spec_ref, brief, result_schema.as_ref(), ctx)
                .await,
        ),
    }
}

/// Checks that a handler's result family matches the requirement it fulfilled.
fn validate(requirement: &Requirement, result: &RequirementResult) -> Result<(), AgentError> {
    requirement.kind.accepts(result).map_err(|error| {
        AgentError::Other(format!(
            "handler returned a result misaligned with requirement {}: {error}",
            requirement.id
        ))
    })
}

/// Resolves a single requirement: fulfill it in `scope`, else pop to `parent`.
///
/// The top layer (`parent = None`) with no matching handler yields
/// [`AgentError::UnhandledRequirement`].
async fn resolve_requirement(
    requirement: &Requirement,
    scope: &dyn HandlerScope,
    parent: Option<&mut (dyn Pop + '_)>,
    ctx: &RunContext,
) -> Result<RequirementResult, AgentError> {
    if let Some(result) = fulfill_with_scope(requirement, scope, ctx).await {
        validate(requirement, &result)?;
        return Ok(result);
    }

    match parent {
        Some(pop) => pop.pop(requirement, ctx).await,
        None => Err(AgentError::UnhandledRequirement {
            kind: requirement.tag(),
            origin: requirement.origin.clone(),
        }),
    }
}

/// Fulfills a batch of requirements against `scope`, popping the rest.
///
/// Requirements this scope handles are run concurrently and collected in
/// completion order (migration decision B); requirements it cannot handle are
/// popped to `parent` one at a time, since a [`Pop`] target is `&mut`.
async fn fulfill_batch(
    requirements: &[Requirement],
    scope: &dyn HandlerScope,
    mut parent: Option<&mut (dyn Pop + '_)>,
    ctx: &RunContext,
) -> Result<Vec<RequirementResolution>, AgentError> {
    let mut local = FuturesUnordered::new();
    let mut popped: Vec<&Requirement> = Vec::new();

    for requirement in requirements {
        if scope_handles(scope, requirement.tag()) {
            local.push(async move {
                let result = fulfill_with_scope(requirement, scope, ctx)
                    .await
                    .expect("scope_handles confirmed a handler for this family");
                validate(requirement, &result)?;
                Ok::<_, AgentError>(RequirementResolution::new(requirement.id, result))
            });
        } else {
            popped.push(requirement);
        }
    }

    let mut resolutions = Vec::with_capacity(requirements.len());
    while let Some(resolution) = local.next().await {
        resolutions.push(resolution?);
    }

    for requirement in popped {
        let result = match parent.as_deref_mut() {
            Some(pop) => pop.pop(requirement, ctx).await?,
            None => {
                return Err(AgentError::UnhandledRequirement {
                    kind: requirement.tag(),
                    origin: requirement.origin.clone(),
                });
            }
        };
        resolutions.push(RequirementResolution::new(requirement.id, result));
    }

    Ok(resolutions)
}

#[cfg(test)]
mod tests {
    use super::{HandlerScope, InteractionHandler, LlmHandler, ScopePop, ToolHandler, drain};
    use crate::{
        agent::{
            AgentError, AgentErrorKind, AgentInput, AgentMachine, ApprovalDecision,
            ApprovalRequirement, ApprovalResponse, BudgetLimits, LlmStepMode, LoopCursor,
            LoopCursorKind, LoopDoneReason, Requirement, RequirementId, RunContext, RunId,
            StepInput, StepOutcome, ToolApprovalPolicy, TraceNodeId,
            interaction::{Interaction, InteractionKind, InteractionResponse},
            requirement::{RequirementKind, RequirementKindTag, RequirementResult},
            tool::{ToolRegistry, ToolRuntimeError},
        },
        client::{Capability, ChatRequest, ClientError, LlmClient, Response},
        conversation::{MessageId, ToolCallId, TurnId},
        model::{
            content::ContentBlock,
            message::{Message, Role},
            tool::{Tool, ToolCall, ToolResponse, ToolStatus},
        },
        stream::{
            StreamEvent,
            accumulator::{CollectError, collect},
        },
    };
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use serde_json::{Map, Value, json};
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn run_context() -> RunContext {
        let run_id: RunId = "018f0d9c-7b6a-7c12-8f31-1234567890a1"
            .parse()
            .expect("run id");
        RunContext::new_root(run_id, BudgetLimits::default(), TraceNodeId::new("root"))
    }

    fn step_id() -> crate::agent::StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890e9"
            .parse()
            .expect("step id")
    }

    fn tool_call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-1234567890c1"
            .parse()
            .expect("tool call id")
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

    fn tool_call() -> ToolCall {
        ToolCall {
            id: "call-weather".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
        }
    }

    fn response() -> Response {
        serde_json::from_value(json!({
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "hi" }]
            },
            "usage": { "input": 1, "output": 1 },
            "stop_reason": { "value": "end_turn", "raw": "end_turn" }
        }))
        .expect("response")
    }

    /// Minimal [`LlmClient`] that always returns a fixed complete response.
    #[derive(Debug)]
    struct FakeClient;

    #[async_trait]
    impl LlmClient for FakeClient {
        fn capability(&self) -> &Capability {
            &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
        }

        async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
            Ok(response())
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
            Err(ClientError::Other(
                "streaming not used in fixture".to_owned(),
            ))
        }
    }

    /// Minimal [`ToolRegistry`] that echoes an `Ok` response for any call.
    #[derive(Debug)]
    struct FakeRegistry;

    #[async_trait]
    impl ToolRegistry for FakeRegistry {
        fn declarations(&self) -> Vec<Tool> {
            Vec::new()
        }

        async fn execute(
            &self,
            _call_id: ToolCallId,
            call: ToolCall,
        ) -> Result<ToolResponse, ToolRuntimeError> {
            Ok(ToolResponse {
                tool_call_id: call.id,
                content: Vec::new(),
                status: ToolStatus::Ok,
                extra: Map::new(),
            })
        }
    }

    /// Wraps an [`LlmClient`] into an [`LlmHandler`].
    struct LlmClientHandler {
        client: Arc<dyn LlmClient>,
    }

    #[async_trait]
    impl LlmHandler for LlmClientHandler {
        async fn fulfill(
            &self,
            request: &ChatRequest,
            mode: LlmStepMode,
            _ctx: &RunContext,
        ) -> RequirementResult {
            let mut request = request.clone();
            let result = match mode {
                LlmStepMode::NonStreaming => {
                    request.stream = false;
                    self.client.chat(request).await
                }
                LlmStepMode::Streaming => {
                    request.stream = true;
                    match self.client.chat_stream(request).await {
                        Ok(stream) => collect(stream).await.map_err(|error| match error {
                            CollectError::Stream(err) => err,
                            CollectError::Accumulator(err) => {
                                ClientError::Protocol(err.to_string())
                            }
                        }),
                        Err(err) => Err(err),
                    }
                }
            };
            RequirementResult::Llm(result)
        }
    }

    /// Wraps a [`ToolRegistry`] into a [`ToolHandler`].
    #[derive(Debug)]
    struct ToolRegistryHandler {
        registry: Arc<dyn ToolRegistry>,
    }

    #[async_trait]
    impl ToolHandler for ToolRegistryHandler {
        async fn fulfill(
            &self,
            call_id: ToolCallId,
            call: &ToolCall,
            _ctx: &RunContext,
        ) -> RequirementResult {
            RequirementResult::Tool(self.registry.execute(call_id, call.clone()).await)
        }
    }

    /// Wraps a [`ToolApprovalPolicy`] into an unattended [`InteractionHandler`].
    #[derive(Debug)]
    struct PolicyInteractionHandler {
        policy: Arc<dyn ToolApprovalPolicy>,
        call: ToolCall,
    }

    #[async_trait]
    impl InteractionHandler for PolicyInteractionHandler {
        async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
            let response = match request.kind() {
                InteractionKind::Approval { call_id, .. } => {
                    let decision = match self.policy.approval_requirement(*call_id, &self.call) {
                        ApprovalRequirement::AutoApprove => ApprovalDecision::Approve,
                        ApprovalRequirement::RequireApproval { .. } => ApprovalDecision::Deny,
                    };
                    InteractionResponse::Approval(ApprovalResponse::new(
                        request.step_id(),
                        *call_id,
                        decision,
                        None,
                    ))
                }
                InteractionKind::Question { .. } => InteractionResponse::answer("ok".to_owned()),
                InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
            };
            RequirementResult::Interaction(response)
        }
    }

    /// Scope with no overrides: every accessor keeps the `None` default.
    struct EmptyScope;

    impl HandlerScope for EmptyScope {}

    /// Scope wiring the three implemented handler families (no subagent yet).
    struct WrappedScope {
        llm: LlmClientHandler,
        tool: ToolRegistryHandler,
        interaction: PolicyInteractionHandler,
    }

    impl HandlerScope for WrappedScope {
        fn llm(&self) -> Option<&dyn LlmHandler> {
            Some(&self.llm)
        }

        fn tool(&self) -> Option<&dyn ToolHandler> {
            Some(&self.tool)
        }

        fn interaction(&self) -> Option<&dyn InteractionHandler> {
            Some(&self.interaction)
        }
    }

    fn wrapped_scope() -> WrappedScope {
        WrappedScope {
            llm: LlmClientHandler {
                client: Arc::new(FakeClient),
            },
            tool: ToolRegistryHandler {
                registry: Arc::new(FakeRegistry),
            },
            interaction: PolicyInteractionHandler {
                policy: Arc::new(crate::agent::NoApprovalPolicy),
                call: tool_call(),
            },
        }
    }

    #[test]
    fn empty_scope_handles_no_requirement_family() {
        let scope = EmptyScope;
        assert!(scope.llm().is_none());
        assert!(scope.tool().is_none());
        assert!(scope.interaction().is_none());
        assert!(scope.subagent().is_none());
    }

    #[test]
    fn wrapped_scope_exposes_implemented_families_only() {
        let scope = wrapped_scope();
        assert!(scope.llm().is_some());
        assert!(scope.tool().is_some());
        assert!(scope.interaction().is_some());
        // SubagentHandler stays unimplemented until M5.
        assert!(scope.subagent().is_none());
    }

    #[tokio::test]
    async fn llm_handler_result_is_accepted_by_its_requirement() {
        let scope = wrapped_scope();
        let ctx = run_context();
        let request = chat_request();
        let mode = LlmStepMode::NonStreaming;

        let result = scope
            .llm()
            .expect("llm handler")
            .fulfill(&request, mode, &ctx)
            .await;

        assert!(matches!(result, RequirementResult::Llm(Ok(_))));
        let kind = RequirementKind::NeedLlm { request, mode };
        kind.accepts(&result).expect("llm result aligns with kind");
    }

    #[tokio::test]
    async fn tool_handler_result_is_accepted_by_its_requirement() {
        let scope = wrapped_scope();
        let ctx = run_context();
        let call = tool_call();
        let call_id = tool_call_id();

        let result = scope
            .tool()
            .expect("tool handler")
            .fulfill(call_id, &call, &ctx)
            .await;

        assert!(matches!(result, RequirementResult::Tool(Ok(_))));
        let kind = RequirementKind::NeedTool { call_id, call };
        kind.accepts(&result).expect("tool result aligns with kind");
    }

    #[tokio::test]
    async fn interaction_handler_result_is_accepted_by_its_requirement() {
        let scope = wrapped_scope();
        let ctx = run_context();
        let request =
            Interaction::approval(step_id(), tool_call_id(), ApprovalRequirement::AutoApprove);

        let result = scope
            .interaction()
            .expect("interaction handler")
            .fulfill(&request, &ctx)
            .await;

        assert!(matches!(result, RequirementResult::Interaction(_)));
        let kind = RequirementKind::NeedInteraction { request };
        kind.accepts(&result)
            .expect("interaction result aligns with kind");
    }

    // ----- M3-2: drain + pop routing fixtures and tests -----

    fn requirement_id_n(n: u8) -> RequirementId {
        RequirementId::parse_str(&format!("018f0d9c-7b6a-7c12-8f31-1234567890{n:02x}"))
            .expect("requirement id")
    }

    fn tool_call_id_n(n: u8) -> ToolCallId {
        format!("018f0d9c-7b6a-7c12-8f31-123456789{n:03x}")
            .parse()
            .expect("tool call id")
    }

    fn ok_tool_response(call: &ToolCall) -> ToolResponse {
        ToolResponse {
            tool_call_id: call.id.clone(),
            content: Vec::new(),
            status: ToolStatus::Ok,
            extra: Map::new(),
        }
    }

    /// A `NeedTool` requirement carrying an optional `delay` (yield count) so a
    /// concurrent batch can be forced to complete out of emission order.
    fn tool_requirement(n: u8, delay: u64) -> Requirement {
        Requirement::at_root(
            requirement_id_n(n),
            RequirementKind::NeedTool {
                call_id: tool_call_id_n(n),
                call: ToolCall {
                    id: format!("call-{n}"),
                    name: "get_weather".to_owned(),
                    input: json!({ "delay": delay }),
                },
            },
        )
    }

    fn interaction_requirement(n: u8) -> Requirement {
        Requirement::at_root(
            requirement_id_n(n),
            RequirementKind::NeedInteraction {
                request: Interaction::approval(
                    step_id(),
                    tool_call_id_n(n),
                    ApprovalRequirement::AutoApprove,
                ),
            },
        )
    }

    /// A minimal machine that emits a fixed requirement batch on the external
    /// input, then completes once every requirement in the batch is resumed.
    ///
    /// It routes results by id (so an out-of-order batch resume is fine) and
    /// records the resume order for assertions.
    struct BatchMachine {
        cursor: LoopCursor,
        batch: Vec<Requirement>,
        outstanding: BTreeSet<RequirementId>,
        resume_order: Vec<RequirementId>,
        resume_tags: Vec<RequirementKindTag>,
    }

    impl BatchMachine {
        fn new(batch: Vec<Requirement>) -> Self {
            Self {
                cursor: LoopCursor::default(),
                batch,
                outstanding: BTreeSet::new(),
                resume_order: Vec::new(),
                resume_tags: Vec::new(),
            }
        }
    }

    impl AgentMachine for BatchMachine {
        fn step(&mut self, input: StepInput) -> StepOutcome {
            match input {
                StepInput::External(_) => {
                    self.outstanding = self
                        .batch
                        .iter()
                        .map(|requirement| requirement.id)
                        .collect();
                    self.cursor = LoopCursor::streaming_step(step_id(), None);
                    StepOutcome::new(Vec::new(), self.batch.clone(), true)
                }
                StepInput::Resume(resolution) => {
                    self.resume_order.push(resolution.id);
                    self.resume_tags.push(resolution.result.tag());
                    self.outstanding.remove(&resolution.id);
                    if self.outstanding.is_empty() {
                        self.cursor = LoopCursor::done(LoopDoneReason::Completed);
                    }
                    StepOutcome::new(Vec::new(), Vec::new(), true)
                }
                StepInput::Abandon(_) => StepOutcome::default(),
            }
        }

        fn cursor(&self) -> &LoopCursor {
            &self.cursor
        }
    }

    fn external_input() -> AgentInput {
        let turn_id: TurnId = "018f0d9c-7b6a-7c12-8f31-1234567890f2"
            .parse()
            .expect("turn id");
        let message_id: MessageId = "018f0d9c-7b6a-7c12-8f31-1234567890f3"
            .parse()
            .expect("message id");
        let assistant_message_id: MessageId = "018f0d9c-7b6a-7c12-8f31-1234567890f6"
            .parse()
            .expect("assistant message id");
        let message = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_owned(),
                extra: Map::new(),
            }],
        };
        AgentInput::user_message(
            turn_id,
            message_id,
            message,
            assistant_message_id,
            step_id(),
        )
        .expect("user input")
    }

    /// Counts fulfillments and echoes an `Ok` tool response.
    #[derive(Clone, Default)]
    struct CountingToolHandler {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ToolHandler for CountingToolHandler {
        async fn fulfill(
            &self,
            _call_id: ToolCallId,
            call: &ToolCall,
            _ctx: &RunContext,
        ) -> RequirementResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            RequirementResult::Tool(Ok(ok_tool_response(call)))
        }
    }

    /// Counts fulfillments and approves any approval interaction.
    #[derive(Clone, Default)]
    struct CountingInteractionHandler {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl InteractionHandler for CountingInteractionHandler {
        async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let response = match request.kind() {
                InteractionKind::Approval { call_id, .. } => {
                    InteractionResponse::Approval(ApprovalResponse::new(
                        request.step_id(),
                        *call_id,
                        ApprovalDecision::Approve,
                        None,
                    ))
                }
                InteractionKind::Question { .. } => InteractionResponse::answer("ok".to_owned()),
                InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
            };
            RequirementResult::Interaction(response)
        }
    }

    /// Records the completion order of a concurrent tool batch, delaying each
    /// fulfillment by the `delay` yield count carried in the tool call input.
    #[derive(Clone, Default)]
    struct DelayToolHandler {
        completed: Arc<std::sync::Mutex<Vec<ToolCallId>>>,
    }

    #[async_trait]
    impl ToolHandler for DelayToolHandler {
        async fn fulfill(
            &self,
            call_id: ToolCallId,
            call: &ToolCall,
            _ctx: &RunContext,
        ) -> RequirementResult {
            let delay = call.input.get("delay").and_then(Value::as_u64).unwrap_or(0);
            for _ in 0..delay {
                tokio::task::yield_now().await;
            }
            self.completed.lock().expect("completion log").push(call_id);
            RequirementResult::Tool(Ok(ok_tool_response(call)))
        }
    }

    /// A flexible scope whose handlers are wired à la carte per test.
    #[derive(Default)]
    struct TestScope {
        tool: Option<CountingToolHandler>,
        interaction: Option<CountingInteractionHandler>,
        delay_tool: Option<DelayToolHandler>,
    }

    impl HandlerScope for TestScope {
        fn tool(&self) -> Option<&dyn ToolHandler> {
            if let Some(handler) = self.delay_tool.as_ref() {
                return Some(handler as &dyn ToolHandler);
            }
            self.tool
                .as_ref()
                .map(|handler| handler as &dyn ToolHandler)
        }

        fn interaction(&self) -> Option<&dyn InteractionHandler> {
            self.interaction
                .as_ref()
                .map(|handler| handler as &dyn InteractionHandler)
        }
    }

    #[tokio::test]
    async fn drain_fulfills_locally_without_popping() {
        let tool = CountingToolHandler::default();
        let scope = TestScope {
            tool: Some(tool.clone()),
            ..TestScope::default()
        };
        let mut machine = BatchMachine::new(vec![tool_requirement(1, 0), tool_requirement(2, 0)]);
        let ctx = run_context();

        let done = drain(&mut machine, external_input(), &scope, None, &ctx)
            .await
            .expect("drain completes");

        assert!(matches!(done.cursor(), LoopCursor::Done(_)));
        assert_eq!(tool.calls.load(Ordering::SeqCst), 2);
        assert_eq!(machine.resume_order.len(), 2);
        assert!(
            machine
                .resume_tags
                .iter()
                .all(|tag| *tag == RequirementKindTag::Tool)
        );
    }

    #[tokio::test]
    async fn drain_pops_to_parent_when_local_scope_lacks_handler() {
        // Inner layer handles tools only; the outer layer handles interaction.
        let inner_tool = CountingToolHandler::default();
        let inner = TestScope {
            tool: Some(inner_tool.clone()),
            ..TestScope::default()
        };
        let outer_interaction = CountingInteractionHandler::default();
        let outer = TestScope {
            interaction: Some(outer_interaction.clone()),
            ..TestScope::default()
        };
        let mut parent = ScopePop::new(&outer, None);
        let mut machine = BatchMachine::new(vec![interaction_requirement(3)]);
        let ctx = run_context();

        let done = drain(
            &mut machine,
            external_input(),
            &inner,
            Some(&mut parent),
            &ctx,
        )
        .await
        .expect("drain completes");

        assert!(matches!(done.cursor(), LoopCursor::Done(_)));
        // The interaction popped to the outer layer; the inner tool was untouched.
        assert_eq!(outer_interaction.calls.load(Ordering::SeqCst), 1);
        assert_eq!(inner_tool.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn drain_top_scope_without_handler_is_unhandled_requirement() {
        let scope = TestScope::default();
        let mut machine = BatchMachine::new(vec![interaction_requirement(4)]);
        let ctx = run_context();

        let error = drain(&mut machine, external_input(), &scope, None, &ctx)
            .await
            .expect_err("top scope cannot fulfill the interaction");

        assert_eq!(error.kind(), AgentErrorKind::UnhandledRequirement);
        match error {
            AgentError::UnhandledRequirement { kind, origin } => {
                assert_eq!(kind, RequirementKindTag::Interaction);
                assert!(origin.is_root());
            }
            other => panic!("expected UnhandledRequirement, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pop_starts_from_outer_scope_skipping_the_emitter() {
        // §7.3: a requirement the emitting (inner) layer cannot fulfill pops to
        // the outer layer; it is resolved there and never re-enters the inner
        // scope. Modeled as a headless inner drain (no interaction handler)
        // whose interaction request is served by the attended outer layer.
        let inner_tool = CountingToolHandler::default();
        let inner = TestScope {
            tool: Some(inner_tool.clone()),
            ..TestScope::default()
        };
        let outer_interaction = CountingInteractionHandler::default();
        let outer_tool = CountingToolHandler::default();
        let outer = TestScope {
            tool: Some(outer_tool.clone()),
            interaction: Some(outer_interaction.clone()),
            ..TestScope::default()
        };
        let mut parent = ScopePop::new(&outer, None);
        let mut machine = BatchMachine::new(vec![interaction_requirement(5)]);
        let ctx = run_context();

        let done = drain(
            &mut machine,
            external_input(),
            &inner,
            Some(&mut parent),
            &ctx,
        )
        .await
        .expect("drain completes");

        assert!(matches!(done.cursor(), LoopCursor::Done(_)));
        // Resolved once, by the outer interaction handler.
        assert_eq!(outer_interaction.calls.load(Ordering::SeqCst), 1);
        // Neither the inner nor the outer tool handler was reached: the popped
        // interaction did not loop back through any tool handler.
        assert_eq!(inner_tool.calls.load(Ordering::SeqCst), 0);
        assert_eq!(outer_tool.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn drain_resolves_a_concurrent_batch_out_of_order() {
        let delay_tool = DelayToolHandler::default();
        let scope = TestScope {
            delay_tool: Some(delay_tool.clone()),
            ..TestScope::default()
        };
        // Emission order is [1, 2, 3]; delays force completion order [3, 2, 1].
        let batch = vec![
            tool_requirement(1, 2),
            tool_requirement(2, 1),
            tool_requirement(3, 0),
        ];
        let mut machine = BatchMachine::new(batch);
        let ctx = run_context();

        let done = drain(&mut machine, external_input(), &scope, None, &ctx)
            .await
            .expect("drain completes");

        assert!(matches!(done.cursor(), LoopCursor::Done(_)));

        // Every requirement was fulfilled exactly once, regardless of order.
        let completed = delay_tool.completed.lock().expect("completion log");
        let completed_set: BTreeSet<ToolCallId> = completed.iter().copied().collect();
        let expected_set: BTreeSet<ToolCallId> =
            [tool_call_id_n(1), tool_call_id_n(2), tool_call_id_n(3)]
                .into_iter()
                .collect();
        assert_eq!(completed_set, expected_set);

        // The batch was fulfilled concurrently and completed out of emission
        // order, and the machine resumed each result in completion order.
        assert_eq!(
            *completed,
            vec![tool_call_id_n(3), tool_call_id_n(2), tool_call_id_n(1)]
        );
        assert_eq!(
            machine.resume_order,
            vec![
                requirement_id_n(3),
                requirement_id_n(2),
                requirement_id_n(1)
            ]
        );

        // Terminal state is reached regardless of the reordering.
        assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    }
}
