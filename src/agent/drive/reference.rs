//! A reference driver: one drain layer wired to real runtime backends.
//!
//! [`drain`] is generic over any [`HandlerScope`]. This module supplies the
//! concrete "single-layer" scope a host uses to run a turn against a live
//! [`LlmClient`], [`ToolRegistry`], and interaction backend, plus the
//! [`drive_turn`] convenience that drains a machine to the end of one turn with
//! no outer layer (migration doc §10, stage 2). It is the effect-model
//! counterpart of [`DefaultAgentLoop`](crate::agent::DefaultAgentLoop): the same
//! client / registry / approval wiring, reached through the sans-io
//! [`AgentMachine`] + [`drain`] path rather than a self-driving loop.
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

use super::{HandlerScope, InteractionHandler, LlmHandler, ToolHandler, TurnDone, drain};
use crate::{
    agent::{
        AgentError, AgentInput, AgentMachine, ApprovalDecision, ApprovalResponse, LlmStepMode,
        RequirementResult, RunContext, ToolRegistry,
        interaction::{Interaction, InteractionKind, InteractionResponse},
    },
    client::{ChatRequest, ClientError, LlmClient},
    conversation::ToolCallId,
    model::tool::ToolCall,
    stream::accumulator::{CollectError, collect},
};
use async_trait::async_trait;
use std::sync::Arc;

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
/// Execution failures are carried inside [`RequirementResult::Tool`]'s `Err`;
/// the machine then applies its
/// [`ToolFailurePolicy`](crate::agent::ToolFailurePolicy) on the return path.
#[derive(Clone)]
pub struct ToolRegistryHandler {
    registry: Arc<dyn ToolRegistry>,
}

impl ToolRegistryHandler {
    /// Wraps `registry` as a [`ToolHandler`].
    #[must_use]
    pub fn new(registry: Arc<dyn ToolRegistry>) -> Self {
        Self { registry }
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
        RequirementResult::Tool(self.registry.execute(call_id, call.clone()).await)
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
/// optional [`ApprovalInteractionHandler`]. With an interaction backend the
/// layer is attended (approvals resolve here); without one it is headless and
/// approvals pop outward. Pass it to [`drain`] (or [`drive_turn`]).
pub struct ReferenceScope {
    llm: LlmClientHandler,
    tool: ToolRegistryHandler,
    interaction: Option<ApprovalInteractionHandler>,
}

impl ReferenceScope {
    /// Wires `client` and `registry` into a scope with no interaction backend.
    #[must_use]
    pub fn new(client: Arc<dyn LlmClient>, registry: Arc<dyn ToolRegistry>) -> Self {
        Self {
            llm: LlmClientHandler::new(client),
            tool: ToolRegistryHandler::new(registry),
            interaction: None,
        }
    }

    /// Attaches an interaction backend, making the layer attended.
    #[must_use]
    pub fn with_interaction(mut self, interaction: ApprovalInteractionHandler) -> Self {
        self.interaction = Some(interaction);
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
