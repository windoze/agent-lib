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
        DeclaredOnlyToolRegistryResolver, LlmStepMode, RequirementResult, RunContext, ToolRegistry,
        ToolRegistryResolver, ToolRuntimeError, ToolSetRef,
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
                        CollectError::Accumulator(err) => ClientError::Protocol(err.to_string()),
                    }),
                    Err(err) => Err(err),
                }
            }
        };
        RequirementResult::Llm(result)
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

    /// Wraps a shared registry slot, so swaps made through it are observed here.
    fn from_slot(registry: SharedRegistry) -> Self {
        Self { registry }
    }

    /// Returns the currently-installed registry, cloning the handle out from
    /// under the lock so no guard is held across the tool `await`.
    fn current(&self) -> Arc<dyn ToolRegistry> {
        self.registry.lock().expect("tool registry slot").clone()
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
        *self.registry.lock().expect("tool registry slot") = registry;
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
/// response so the result still type-aligns with its requirement; the
/// [`DefaultAgentMachine`](crate::agent::DefaultAgentMachine) never emits them.
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
        };
        RequirementResult::Interaction(response)
    }
}

/// One drain layer wired to live runtime backends.
///
/// Wraps an [`LlmClient`] and a [`ToolRegistry`] into their handlers, plus an
/// optional [`ApprovalInteractionHandler`]. The tool registry lives in a shared,
/// swappable slot so a turn-boundary reconfiguration is fulfilled by resolving
/// and installing a new registry through a [`ToolRegistryResolver`] (defaulting
/// to [`DeclaredOnlyToolRegistryResolver`]; override with
/// [`with_tool_registry_resolver`](Self::with_tool_registry_resolver)). With an
/// interaction backend the layer is attended (approvals resolve here); without
/// one it is headless and approvals pop outward. Pass it to [`drain`] (or
/// [`drive_turn`]).
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
    /// resolve through a [`DeclaredOnlyToolRegistryResolver`] until a stricter
    /// resolver is supplied via
    /// [`with_tool_registry_resolver`](Self::with_tool_registry_resolver).
    #[must_use]
    pub fn new(client: Arc<dyn LlmClient>, registry: Arc<dyn ToolRegistry>) -> Self {
        let slot: SharedRegistry = Arc::new(Mutex::new(registry));
        Self {
            llm: LlmClientHandler::new(client),
            tool: ToolRegistryHandler::from_slot(slot.clone()),
            reconfig: ReconfigRegistryHandler::new(
                Arc::new(DeclaredOnlyToolRegistryResolver),
                slot,
            ),
            interaction: None,
        }
    }

    /// Attaches an interaction backend, making the layer attended.
    #[must_use]
    pub fn with_interaction(mut self, interaction: ApprovalInteractionHandler) -> Self {
        self.interaction = Some(interaction);
        self
    }

    /// Sets the resolver used to fulfill `NeedReconfigRegistry` requirements.
    #[must_use]
    pub fn with_tool_registry_resolver(mut self, resolver: Arc<dyn ToolRegistryResolver>) -> Self {
        self.reconfig.resolver = resolver;
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
mod tests;
