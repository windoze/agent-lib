//! Sans-io external-agent machine for the basic session-advance path.
//!
//! [`ExternalAgentMachine`] is the external-agent counterpart of
//! [`DefaultAgentMachine`](crate::agent::DefaultAgentMachine): instead of driving
//! an LLM turn it drives a single blocking [external session
//! effect](crate::agent::external). It never awaits, never touches a runtime, and
//! never does IO — it advances its own [`ExternalAgentState`], *requests* the
//! session step by handing back a [`RequirementKind::NeedExternalSession`], and
//! parks on the matching [`ExternalAgentCursor`]. A driver's
//! [`ExternalSessionHandler`](crate::agent::ExternalSessionHandler) advances the
//! real runtime to its next decision point and feeds the
//! [`ExternalSessionResult`] back through [`StepInput::Resume`].
//!
//! # What this machine covers
//!
//! - `step(External(UserMessage))` opens a Conversation turn and blocks on one
//!   `NeedExternalSession`, choosing
//!   [`Start`](ExternalSessionInput::Start) when no session exists yet and
//!   [`Continue`](ExternalSessionInput::Continue) to advance an established one.
//! - `step(Resume(ExternalSession(Completed)))` records the resumable session
//!   facts, folds the runtime's terminal output into committed history, records
//!   the reported [`ExternalArtifactRef`](super::ExternalArtifactRef) list into
//!   the retained trace via
//!   [`ExternalAgentState::record_artifacts`](ExternalAgentState::record_artifacts)
//!   (references only, never inline content — design §11, §12), and settles the
//!   cursor on [`Done`](ExternalAgentCursor::Done).
//! - `step(Resume(ExternalSession(Failed)))` records any retained session facts
//!   and settles the cursor on [`Error`](ExternalAgentCursor::Error).
//! - `step(Resume(ExternalSession(PausedForInteraction)))` records the session
//!   facts, the paused action id, and the neutral
//!   [`Interaction`](crate::agent::Interaction) itself, emits one
//!   [`NeedInteraction`](RequirementKind::NeedInteraction) for the standard
//!   interaction pop rules to serve, and parks on
//!   [`AwaitingInteraction`](ExternalAgentCursor::AwaitingInteraction). The
//!   host's [`InteractionResponse`](crate::agent::InteractionResponse) is
//!   validated against that pending interaction
//!   ([`Interaction::accepts_response`](crate::agent::Interaction::accepts_response))
//!   before it re-enters the session as a
//!   [`RespondInteraction`](ExternalSessionInput::RespondInteraction) that
//!   echoes the paused action id, reparking on
//!   [`AwaitingSession`](ExternalAgentCursor::AwaitingSession) so a turn can loop
//!   pause↔respond until it completes or fails. A response of the wrong family,
//!   an out-of-range choice index, or a mismatched permission `action_id` is
//!   rejected into an [`Error`](ExternalAgentCursor::Error) cursor and never
//!   forwarded to the runtime.
//! - `step(Resume(ExternalSession(PausedForToolCalls)))` records the session
//!   facts and bridges every runtime [`ExternalToolCall`](super::ExternalToolCall)
//!   into a host requirement (minting a host
//!   [`ToolCallId`](crate::conversation::ToolCallId) from the injected
//!   [`ToolExecutionIds`] and a [`RequirementId`] per bridged call), parking on
//!   [`AwaitingTool`](ExternalAgentCursor::AwaitingTool) with the batch's
//!   volatile per-call correlation held in non-serialized scratch. A plain call
//!   becomes a [`NeedTool`](RequirementKind::NeedTool); a `spawn_agent` call is a
//!   scope-deepening operation that instead bridges into a standard
//!   [`NeedSubagent`](RequirementKind::NeedSubagent) (parsed with
//!   [`SpawnAgentRequest`](crate::agent::collab::SpawnAgentRequest), design §8.3)
//!   whose child summary folds back into the same batch as a tool result — so a
//!   mixed batch of tools and spawns parks under one cursor, plain tools
//!   fulfilling concurrently while each subagent is driven serially by the host.
//!   A malformed `spawn_agent` input mints no requirement: a runtime-visible
//!   error result is pre-seeded (return-error-to-runtime, design §8.4). Each host
//!   result then resumes the machine on its own hop; results are collected in the
//!   scratch and, once the whole batch is answered, relayed straight back to the
//!   runtime as one
//!   [`RespondToolResults`](ExternalSessionInput::RespondToolResults) in the
//!   original call order — never written into the Conversation — reparking on
//!   [`AwaitingSession`](ExternalAgentCursor::AwaitingSession) so the turn can
//!   advance to its next decision point.
//! - `step(Resume(ExternalSession(PausedForSubagent)))` records the session
//!   facts and bridges the runtime's
//!   [`ExternalSubagentRequest`](super::ExternalSubagentRequest) into one
//!   [`NeedSubagent`](RequirementKind::NeedSubagent) requirement (reusing its
//!   `spec_ref`, `brief`, and `result_schema` unchanged), parking on
//!   [`AwaitingSubagent`](ExternalAgentCursor::AwaitingSubagent) with the
//!   runtime's [`ExternalSubagentRequestId`](super::ExternalSubagentRequestId)
//!   held in the serializable cursor. This native subagent pause is distinct from
//!   the `spawn_agent` tool bridge above: the child's output relays back through a
//!   dedicated [`RespondSubagent`](ExternalSessionInput::RespondSubagent), not a
//!   `RespondToolResults`. The child is driven by the host's own
//!   [`DrivingSubagentHandler`](crate::agent) (depth / budget / cancel
//!   accounting, outward pop of the child's unhandled requirements); the machine
//!   only reifies the requirement. Its
//!   [`SubagentOutput`](crate::agent::SubagentOutput) then resumes the machine,
//!   is bridged into an
//!   [`ExternalSubagentOutput`](super::ExternalSubagentOutput), and relayed back
//!   to the runtime as one
//!   [`RespondSubagent`](ExternalSessionInput::RespondSubagent) echoing the
//!   request id — never written into the Conversation — reparking on
//!   [`AwaitingSession`](ExternalAgentCursor::AwaitingSession) so the turn can
//!   advance to its next decision point.
//! - `step(Abandon)` is the never-resume cancel close (design §6.4): it discards
//!   the dangling turn, flags
//!   [`ExternalAgentState::mark_cleanup_required`](ExternalAgentState::mark_cleanup_required)
//!   so the handle layer force-closes any live session, and settles back to a
//!   feedable [`Idle`](ExternalAgentCursor::Idle). It never emits a `Shutdown`
//!   effect — cleanup and its
//!   [`ExternalSessionShutdown`](super::ExternalSessionShutdown) disposition live
//!   at the handle layer, not in this sans-io step.
//!
//! # Mounting under a subagent hierarchy
//!
//! `ExternalAgentMachine` is a plain [`AgentMachine`], so a
//! [`SubagentHandler`](crate::agent::SubagentHandler) can mount it as the child
//! of a `NeedSubagent`: the reference
//! [`DrivingSubagentHandler`](crate::agent::DrivingSubagentHandler) opens a
//! nested drain layer for it under a derived child
//! [`RunContext`](crate::agent::RunContext), so it advances Start→Completed just
//! like any other child machine while inheriting depth / budget / cancel from the
//! parent.
//!
//! # Persistence boundary
//!
//! The serializable machine state is the wrapped [`ExternalAgentState`], whose
//! [`ExternalAgentCursor`] records the outstanding
//! [`RequirementId`](crate::agent::RequirementId). The non-serialized fields are
//! the host-supplied [`RequirementIds`] source, the [`LoopCursor`] *view*
//! returned to the driver (kept in lockstep with the external cursor), and the
//! mid-turn scratch ([`InFlight`]) that mirrors the in-flight turn's assistant
//! identity the way [`DefaultAgentMachine`](crate::agent::DefaultAgentMachine)
//! keeps its own turn scratch. Observations buffered by the handler are threaded
//! through the resume path and converted into
//! [`Notification::ExternalAgent`](crate::agent::Notification::ExternalAgent)
//! events on the resuming step, deduplicated per event against
//! [`ExternalSessionRef::last_event_seq`] so a replayed or overlapping decision
//! point re-emits only its unseen suffix (design §5.5).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Map;

use crate::{
    agent::{
        AgentInput, AgentMachine, AgentUserInput, CursorRequirement, Interaction, LoopCursor,
        LoopDoneReason, NoToolExecutionIds, Notification, Requirement, RequirementId,
        RequirementIds, RequirementKind, RequirementKindTag, RequirementResolution,
        RequirementResult, StepId, StepInput, StepOutcome, ToolExecutionIds, ToolRuntimeError,
        ToolWaitRequirements,
        collab::{SPAWN_AGENT, SpawnAgentRequest},
        external::{
            ExternalAgentCursor, ExternalAgentError, ExternalAgentMachineConfig,
            ExternalAgentOutput, ExternalAgentState, ExternalCapability, ExternalObservedEvent,
            ExternalSessionInput, ExternalSessionRef, ExternalSessionRequest,
            ExternalSessionResult, ExternalSubagentOutput, ExternalSubagentRequest,
            ExternalSubagentRequestId, ExternalToolBatchId, ExternalToolCall,
            ExternalToolFailurePolicy, ExternalToolResult,
        },
        spec::ToolSetRef,
    },
    client::Response,
    conversation::{CancelDisposition, MessageId, ToolCallId, TurnMeta},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::{ToolCall, ToolStatus},
    },
};

/// Which awaiting cursor a [`resume`](ExternalAgentMachine::resume) is folding
/// into, resolved once from the borrowed cursor so the mutable transition is
/// free to run.
enum Awaiting {
    /// Parked on an outstanding `NeedExternalSession`.
    Session(RequirementId),
    /// Parked on an outstanding `NeedInteraction`, carrying the paused action id
    /// echoed back through `RespondInteraction` and the neutral [`Interaction`]
    /// the host's response is validated against before it is relayed.
    Interaction {
        requirement: RequirementId,
        pending_action: String,
        interaction: Interaction,
    },
    /// Parked on an outstanding batch of `NeedTool` requirements. The volatile
    /// per-call correlation the resume routes against lives in the
    /// [`PendingExternalToolBatch`] scratch, so no addressing is carried here.
    Tool,
    /// Parked on an outstanding `NeedSubagent`, carrying the runtime's subagent
    /// spawn request id echoed back through `RespondSubagent`.
    Subagent {
        requirement: RequirementId,
        request_id: ExternalSubagentRequestId,
    },
}

/// Mid-turn scratch for the external session step currently in flight.
///
/// Like [`DefaultAgentMachine`](crate::agent::DefaultAgentMachine)'s turn
/// scratch, this lives only while a turn is unfinished and is deliberately *not*
/// part of the serializable [`ExternalAgentState`]; the cursor still records
/// which requirement the machine is stuck on. It carries the host-supplied
/// assistant identity used to freeze the turn on completion, plus the turn's
/// step identity reused for the driver-facing cursor view across the turn's
/// pause↔respond hops.
#[derive(Clone, Copy, Debug)]
struct InFlight {
    /// Step identity of the turn, reused for the driver-facing cursor view
    /// across every external-session and interaction hop the turn takes.
    step_id: StepId,
    assistant_message_id: MessageId,
}

/// Non-serialized scratch for a batch of host tool calls a paused session is
/// waiting on.
///
/// When a runtime pauses on
/// [`PausedForToolCalls`](ExternalSessionResult::PausedForToolCalls) the machine
/// bridges every runtime [`ExternalToolCall`] into a host requirement and parks
/// on [`AwaitingTool`](ExternalAgentCursor::AwaitingTool). A plain call becomes a
/// [`NeedTool`](RequirementKind::NeedTool); a `spawn_agent` call is a
/// scope-deepening operation and instead becomes a
/// [`NeedSubagent`](RequirementKind::NeedSubagent) whose child output folds back
/// into this same batch as a tool result (design §8.3). The serializable cursor
/// records only the resumable addressing (`ToolCallId -> RequirementId`); this
/// scratch holds the volatile per-call facts a completed batch needs to feed
/// [`RespondToolResults`](ExternalSessionInput::RespondToolResults) back: the
/// [`ExternalToolBatchId`], the original [`ExternalToolCall`] order, the map from
/// each outstanding host [`RequirementId`] back to the call it fulfills (its
/// runtime `provider_call_id` and whether it was bridged as a tool or a
/// subagent, so an out-of-order resume routes to the right call and is validated
/// against the right result family), and the [`ExternalToolResult`] values
/// collected so far. Like [`InFlight`], it lives only while a turn is unfinished
/// and is deliberately absent from [`ExternalAgentState`], so a mid-turn restore
/// (which recovers the cursor but not this scratch) cannot resume a partially
/// answered batch.
///
/// A malformed `spawn_agent` input never mints a requirement: its
/// runtime-visible error [`ExternalToolResult`] is pre-seeded into
/// [`results`](Self::results) at pause time (return-error-to-runtime, design
/// §8.4), so it counts toward batch completion without an outstanding
/// requirement.
///
/// This scratch is populated when a session pauses for tool calls (the
/// [`PausedForToolCalls`](ExternalSessionResult::PausedForToolCalls) fold) and
/// drained as each result arrives and the completed batch feeds
/// [`RespondToolResults`](ExternalSessionInput::RespondToolResults) back — see
/// [`resume_tool`](ExternalAgentMachine::resume_tool).
#[derive(Clone, Debug)]
struct PendingExternalToolBatch {
    /// Runtime-assigned batch token echoed back through `RespondToolResults`.
    batch_id: ExternalToolBatchId,
    /// Original tool calls in the order the runtime emitted them, so the
    /// collected results can be returned in that same stable order.
    calls: Vec<ExternalToolCall>,
    /// Maps each outstanding host `RequirementId` back to the runtime call it
    /// fulfills (provider call id + bridge kind), so an out-of-order resume
    /// routes to the right call and validates against the right result family.
    pending: BTreeMap<RequirementId, PendingBridgeCall>,
    /// Results collected so far, keyed by `provider_call_id`. Pre-seeded with a
    /// runtime-visible error for any malformed `spawn_agent` call.
    results: BTreeMap<String, ExternalToolResult>,
}

/// How a paused runtime tool call was bridged into a host requirement, recorded
/// so a resume can be validated against the matching result family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExternalBridgeCallKind {
    /// Bridged into a [`NeedTool`](RequirementKind::NeedTool); answered by a
    /// host [`Tool`](RequirementResult::Tool) result.
    Tool,
    /// A `spawn_agent` call bridged into a
    /// [`NeedSubagent`](RequirementKind::NeedSubagent); answered by a host
    /// [`Subagent`](RequirementResult::Subagent) result folded into an
    /// [`ExternalToolResult`] (design §8.3).
    Subagent,
}

/// The outstanding runtime call a bridged host requirement is fulfilling.
#[derive(Clone, Debug)]
struct PendingBridgeCall {
    /// Runtime correlation id this requirement answers, used to key the result
    /// back into the batch under the runtime's own call id.
    provider_call_id: String,
    /// Which family the bridged requirement (and therefore its resume) belongs
    /// to.
    kind: ExternalBridgeCallKind,
}

/// When a host-requested external reconfiguration takes effect.
///
/// A managed external runtime is driven one whole session step at a time, so the
/// tool set carried by a `NeedExternalSession` request is fixed for the duration
/// of that step. This selector chooses how a reconfiguration interacts with a
/// step that may still be in flight (design §19):
///
/// - [`NextBoundary`](Self::NextBoundary) is the safe default: applied
///   immediately when the machine rests at a turn boundary, and otherwise queued
///   and folded in when the next turn opens, never touching the live session.
/// - [`Hot`](Self::Hot) asks for a live, mid-turn swap of the running session's
///   tool set. The first-version machine only reconfigures at boundaries, so an
///   in-flight hot request is rejected with
///   [`UnsupportedCapability`](ExternalAgentError::UnsupportedCapability)
///   ([`Reconfigure`](ExternalCapability::Reconfigure)) and leaves the live
///   session untouched. At a turn boundary it behaves exactly like
///   [`NextBoundary`](Self::NextBoundary).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExternalReconfigTiming {
    /// Apply at the next turn boundary (immediately if already at a boundary).
    #[default]
    NextBoundary,
    /// Apply immediately to the live in-flight session (requires runtime hot
    /// reconfiguration support, which the first-version machine does not offer).
    Hot,
}

/// Outcome of a host-requested external reconfiguration
/// ([`ExternalAgentMachine::reconfigure`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalReconfigOutcome {
    /// The requested tool set is now the active set; the next
    /// `NeedExternalSession(Start/Continue)` request carries it.
    Applied,
    /// The machine was mid-turn, so the change was queued and will be folded in
    /// when the next turn opens. The live in-flight session is left unchanged.
    Queued,
}

/// Sans-io machine that drives one external coding-agent session step at a time.
///
/// See the [external-agent module docs](crate::agent::external) for the
/// effect-model contract, and this module's own docs for the scope of this
/// milestone.
#[derive(Debug)]
pub struct ExternalAgentMachine {
    state: ExternalAgentState,
    requirement_ids: Arc<dyn RequirementIds>,
    /// Host-supplied identity source used to mint the [`ToolCallId`] each
    /// bridged [`NeedTool`](RequirementKind::NeedTool) requirement carries.
    /// Defaults to [`NoToolExecutionIds`]; a machine whose runtime pauses for
    /// host tool calls must inject a real source via
    /// [`with_tool_execution_ids`](Self::with_tool_execution_ids), or a
    /// tool-call pause settles on a classified error.
    tool_ids: Arc<dyn ToolExecutionIds>,
    /// Driver-facing [`LoopCursor`] view, kept in lockstep with
    /// [`ExternalAgentState::cursor`]. `AgentMachine::cursor` must return a
    /// `&LoopCursor`, so the machine maintains this mapped mirror rather than
    /// re-deriving it on every call.
    loop_cursor: LoopCursor,
    /// Non-serialized scratch for the in-flight turn; `Some` only between opening
    /// a turn and settling it.
    in_flight: Option<InFlight>,
    /// Non-serialized scratch for a tool-call batch a paused session is waiting
    /// on; `Some` only while the machine is parked on
    /// [`AwaitingTool`](ExternalAgentCursor::AwaitingTool) mid-turn. It is
    /// populated when a session pauses for tool calls and cleared when the turn
    /// settles (completion, failure, or abandon). It cannot be rebuilt from a
    /// restored [`ExternalAgentState`], so a mid-turn restore leaves it `None`
    /// (design: the serializable cursor keeps the resumable addressing; this
    /// scratch keeps the volatile per-call correlation).
    pending_tool_batch: Option<PendingExternalToolBatch>,
    /// Machine-local policy bundle (tool-failure handling, required
    /// capabilities, decision-loop bound). Plain data injected via
    /// [`with_external_config`](Self::with_external_config); it never enters the
    /// serializable [`ExternalAgentState`]. Defaults to the permissive
    /// [`ExternalAgentMachineConfig::default`], preserving pre-M4-3 behavior.
    config: ExternalAgentMachineConfig,
}

impl ExternalAgentMachine {
    /// Creates a machine over `state`, using `requirement_ids` to stamp the
    /// reified `NeedExternalSession` requirements it hands back.
    ///
    /// Tool orchestration defaults to [`NoToolExecutionIds`] (no host id
    /// source); a machine whose runtime pauses for host tool calls supplies a
    /// real source via [`with_tool_execution_ids`](Self::with_tool_execution_ids).
    #[must_use]
    pub fn new(state: ExternalAgentState, requirement_ids: Arc<dyn RequirementIds>) -> Self {
        let loop_cursor = initial_loop_cursor(state.cursor());
        Self {
            state,
            requirement_ids,
            tool_ids: Arc::new(NoToolExecutionIds),
            loop_cursor,
            in_flight: None,
            pending_tool_batch: None,
            config: ExternalAgentMachineConfig::default(),
        }
    }

    /// Sets the host-supplied identity source used to mint the [`ToolCallId`]
    /// each bridged [`NeedTool`](RequirementKind::NeedTool) requirement carries
    /// when a runtime pauses for host tool calls.
    #[must_use]
    pub fn with_tool_execution_ids(mut self, tool_ids: Arc<dyn ToolExecutionIds>) -> Self {
        self.tool_ids = tool_ids;
        self
    }

    /// Replaces the machine-local policy bundle.
    ///
    /// The [`ExternalAgentMachineConfig`] carries the tool-failure policy, the
    /// capabilities this run requires, and the decision-loop bound the machine
    /// enforces. It is plain data and never enters the serializable
    /// [`ExternalAgentState`]; the live identity sources stay behind their own
    /// builder injections ([`with_tool_execution_ids`](Self::with_tool_execution_ids)).
    #[must_use]
    pub fn with_external_config(mut self, config: ExternalAgentMachineConfig) -> Self {
        self.config = config;
        self
    }

    /// Sets how a failed bridged host tool call is handled, leaving the rest of
    /// the machine-local config untouched.
    #[must_use]
    pub fn with_tool_failure_policy(mut self, policy: ExternalToolFailurePolicy) -> Self {
        self.config = self.config.with_tool_failure_policy(policy);
        self
    }

    /// Sets the decision-loop bound (`None` clears it), leaving the rest of the
    /// machine-local config untouched.
    #[must_use]
    pub fn with_max_decision_loops(mut self, max: Option<u32>) -> Self {
        self.config = self.config.with_max_decision_loops(max);
        self
    }

    /// Returns a read-only view of the wrapped serializable external-agent state.
    #[must_use]
    pub const fn state(&self) -> &ExternalAgentState {
        &self.state
    }

    /// Consumes the machine and returns its serializable external-agent state.
    #[must_use]
    pub fn into_state(self) -> ExternalAgentState {
        self.state
    }

    /// Requests a turn-boundary reconfiguration of the active tool set.
    ///
    /// This is a host-facing entry (not part of the sans-io
    /// [`step`](AgentMachine::step)), the external-agent counterpart of
    /// [`DefaultAgentMachine::reconfigure`](crate::agent::DefaultAgentMachine).
    /// Because a managed runtime advances one whole session step at a time, the
    /// tool set carried by an outstanding `NeedExternalSession` request cannot be
    /// changed underneath it; the reconfiguration policy therefore keys on
    /// whether a turn is in flight (design §19):
    ///
    /// - **At a turn boundary** (no turn in flight — the cursor rests
    ///   [`Idle`](ExternalAgentCursor::Idle) / [`Done`](ExternalAgentCursor::Done)
    ///   / [`Error`](ExternalAgentCursor::Error)) the new set becomes active
    ///   immediately and any stale queued reconfiguration is dropped. The next
    ///   [`Start`](ExternalSessionInput::Start) /
    ///   [`Continue`](ExternalSessionInput::Continue) request carries it. Returns
    ///   [`Applied`](ExternalReconfigOutcome::Applied) regardless of `timing`.
    /// - **Mid-turn with [`NextBoundary`](ExternalReconfigTiming::NextBoundary)**
    ///   the requested set is queued in the serializable state and folded into
    ///   the active set when the next turn opens; the live session is untouched.
    ///   Returns [`Queued`](ExternalReconfigOutcome::Queued).
    /// - **Mid-turn with [`Hot`](ExternalReconfigTiming::Hot)** the caller asked
    ///   to swap the *live* session's tools, which the first-version machine does
    ///   not support. Nothing is changed — the live session, the active set, and
    ///   any previously queued reconfiguration all stay exactly as they were —
    ///   and the request fails loudly (see Errors) so the change never silently
    ///   alters an in-flight session.
    ///
    /// # Errors
    ///
    /// Returns
    /// [`ExternalAgentError::UnsupportedCapability`](ExternalAgentError::UnsupportedCapability)
    /// naming [`ExternalCapability::Reconfigure`] when a mid-turn
    /// [`Hot`](ExternalReconfigTiming::Hot) reconfiguration is requested, since a
    /// live session's tool set cannot be hot-swapped by this machine.
    #[allow(clippy::result_large_err)]
    pub fn reconfigure(
        &mut self,
        active_tools: ToolSetRef,
        timing: ExternalReconfigTiming,
    ) -> Result<ExternalReconfigOutcome, ExternalAgentError> {
        // `in_flight` is `Some` only between opening a turn and settling it, so
        // its absence is exactly the "at a turn boundary" condition.
        if self.in_flight.is_none() {
            self.state.clear_pending_reconfig();
            self.state.set_active_tools(active_tools);
            return Ok(ExternalReconfigOutcome::Applied);
        }

        match timing {
            ExternalReconfigTiming::NextBoundary => {
                self.state.set_pending_reconfig(active_tools);
                Ok(ExternalReconfigOutcome::Queued)
            }
            ExternalReconfigTiming::Hot => Err(ExternalAgentError::UnsupportedCapability {
                runtime: self.state.spec().runtime().clone(),
                capability: ExternalCapability::Reconfigure,
                detail: "external machine reconfigures the active tool set only at a turn \
                         boundary; a live session's tool set cannot be hot-swapped mid-turn"
                    .to_owned(),
            }),
        }
    }

    /// Opens a fresh Conversation turn and blocks on one `NeedExternalSession`.
    fn begin_user_turn(&mut self, user: AgentUserInput) -> StepOutcome {
        // Fold a reconfiguration queued while a previous turn was in flight into
        // the active tool set before building this turn's request, so the fresh
        // `Start`/`Continue` carries the new tools (design §19).
        if let Some(tools) = self.state.take_pending_reconfig() {
            self.state.set_active_tools(tools);
        }
        if let Err(error) = self.state.conversation_mut().begin_turn(
            user.turn_id(),
            user.message_id(),
            user.message().clone(),
        ) {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        self.in_flight = Some(InFlight {
            step_id: user.step_id(),
            assistant_message_id: user.assistant_message_id(),
        });

        // An established session continues; a fresh one starts. The user text is
        // handed to the runtime as opaque prompt/message data.
        let text = message_text(user.message());
        let input = if self.state.session().is_some() {
            ExternalSessionInput::Continue { message: text }
        } else {
            ExternalSessionInput::Start { prompt: text }
        };

        self.block_on_session(user.step_id(), input)
    }

    /// Reifies one external session effect and parks on
    /// [`AwaitingSession`](ExternalAgentCursor::AwaitingSession).
    ///
    /// Every runtime round-trip funnels through here (the initial
    /// `Start`/`Continue`, and each `RespondToolResults` / `RespondInteraction` /
    /// `RespondSubagent`), so it is the single place the machine bounds the
    /// managed loop: it records another decision loop and, when the injected
    /// [`ExternalAgentMachineConfig::max_decision_loops`] cap is exceeded, fails
    /// with a classified [`LimitExceeded`](ExternalAgentError::LimitExceeded)
    /// before minting another `NeedExternalSession` — so an unbounded
    /// pause/respond loop stops loudly instead of spinning (design §6.3).
    ///
    /// The spec's [`ExternalSessionPolicy::max_turns`] is enforced here too,
    /// uniformly across runtimes: one decision loop is one runtime round-trip,
    /// so exceeding the policy cap fails with the same classified
    /// [`LimitExceeded`](ExternalAgentError::LimitExceeded) rather than relying
    /// on a CLI-specific flag (M2-7 / M-PROM-5).
    fn block_on_session(&mut self, step_id: StepId, input: ExternalSessionInput) -> StepOutcome {
        let loops = self.state.record_decision_loop();
        if let Some(limit) = self.config.max_decision_loops()
            && loops > limit
        {
            return self.fail(
                ExternalAgentError::LimitExceeded {
                    limit: format!("max external decision loops ({limit}) exceeded"),
                }
                .to_string(),
            );
        }
        if let Some(max_turns) = self.state.spec().session_policy().max_turns
            && loops > max_turns
        {
            return self.fail(
                ExternalAgentError::LimitExceeded {
                    limit: format!(
                        "session policy max_turns ({max_turns}) exceeded after {loops} runtime round-trips"
                    ),
                }
                .to_string(),
            );
        }

        let requirement_id = match self
            .requirement_ids
            .next_requirement_id(RequirementKindTag::ExternalSession)
        {
            Ok(id) => id,
            Err(error) => return self.fail(format!("requirement id unavailable: {error}")),
        };

        let request = self.build_request(input);
        let cursor_requirement = CursorRequirement::root(requirement_id);
        self.settle(
            ExternalAgentCursor::AwaitingSession {
                requirement: cursor_requirement.clone(),
            },
            LoopCursor::streaming_step(step_id, Some(cursor_requirement)),
        );

        let requirement = Requirement::at_root(
            requirement_id,
            RequirementKind::NeedExternalSession { request },
        );
        StepOutcome::new(Vec::new(), vec![requirement], true)
    }

    /// Builds the provider-neutral request the handler advances this step.
    fn build_request(&self, input: ExternalSessionInput) -> ExternalSessionRequest {
        let spec = self.state.spec();
        ExternalSessionRequest {
            agent_id: spec.id(),
            runtime: spec.runtime().clone(),
            worktree: spec.worktree().clone(),
            // The session layer (registry worktree preparation) resolves the
            // effective session directory; the machine mints requests without one.
            session_dir: None,
            session: self.state.session().cloned(),
            input,
            tools: self.state.active_tools().tools().to_vec(),
            policy: *spec.session_policy(),
        }
    }

    /// Feeds a fulfilled requirement result back into the parked machine.
    fn resume(&mut self, resolution: RequirementResolution) -> StepOutcome {
        // Read the outstanding requirement (and, for an interaction, the paused
        // action it answers) before releasing the borrow of the cursor so the
        // mutable transitions below are free to run.
        let awaiting = match self.state.cursor() {
            ExternalAgentCursor::AwaitingSession { requirement } => {
                Ok(Awaiting::Session(requirement.id()))
            }
            ExternalAgentCursor::AwaitingInteraction {
                requirement,
                pending_action,
                interaction,
            } => Ok(Awaiting::Interaction {
                requirement: requirement.id(),
                pending_action: pending_action.clone(),
                interaction: interaction.clone(),
            }),
            ExternalAgentCursor::AwaitingTool { .. } => Ok(Awaiting::Tool),
            ExternalAgentCursor::AwaitingSubagent {
                requirement,
                request_id,
            } => Ok(Awaiting::Subagent {
                requirement: requirement.id(),
                request_id: request_id.clone(),
            }),
            other => Err(format!(
                "resume received while cursor is `{}`, no outstanding external requirement",
                cursor_label(other)
            )),
        };

        match awaiting {
            Ok(Awaiting::Session(expected)) => self.resume_session(expected, resolution),
            Ok(Awaiting::Interaction {
                requirement,
                pending_action,
                interaction,
            }) => self.resume_interaction(requirement, pending_action, interaction, resolution),
            Ok(Awaiting::Tool) => self.resume_tool(resolution),
            Ok(Awaiting::Subagent {
                requirement,
                request_id,
            }) => self.resume_subagent(requirement, request_id, resolution),
            Err(message) => self.fail(message),
        }
    }

    /// Folds a fulfilled `NeedExternalSession` result into the in-flight turn.
    fn resume_session(
        &mut self,
        expected: RequirementId,
        resolution: RequirementResolution,
    ) -> StepOutcome {
        if resolution.id != expected {
            return self.fail(format!(
                "resume targets requirement {}, but the machine awaits {expected}",
                resolution.id
            ));
        }

        match resolution.result {
            RequirementResult::ExternalSession(result) => self.fold_session_result(*result),
            other => self.fail(format!(
                "NeedExternalSession requirement cannot accept a `{}` result",
                other.tag()
            )),
        }
    }

    /// Routes a decision-point [`ExternalSessionResult`] to its transition.
    ///
    /// Buffered `observations` are converted into
    /// [`Notification::ExternalAgent`] events (design §5.5) and carried out on the
    /// resuming step's [`StepOutcome`]. Dedup is per-event: each observation's
    /// [`seq`](ExternalObservedEvent::seq) is compared against the last one
    /// already consumed (recorded in [`ExternalSessionRef::last_event_seq`]), so a
    /// replayed or overlapping result only re-emits its unseen suffix — see
    /// [`observe`](Self::observe).
    fn fold_session_result(&mut self, result: ExternalSessionResult) -> StepOutcome {
        match result {
            ExternalSessionResult::Completed {
                session,
                output,
                observations,
            } => {
                let notifications = self.observe(observations);
                self.complete_session(session, output, notifications)
            }
            ExternalSessionResult::PausedForInteraction {
                session,
                action_id,
                request,
                observations,
            } => {
                let notifications = self.observe(observations);
                self.pause_for_interaction(session, action_id, request, notifications)
            }
            ExternalSessionResult::PausedForToolCalls {
                session,
                batch_id,
                calls,
                observations,
            } => {
                let notifications = self.observe(observations);
                self.pause_for_tool_calls(session, batch_id, calls, notifications)
            }
            ExternalSessionResult::PausedForSubagent {
                session,
                request,
                observations,
            } => {
                let notifications = self.observe(observations);
                self.pause_for_subagent(session, request, notifications)
            }
            ExternalSessionResult::Failed {
                session,
                error,
                observations,
            } => {
                let notifications = self.observe(observations);
                if session.is_some() {
                    self.state.set_session(session);
                }
                self.fail_with(error.to_string(), notifications)
            }
        }
    }

    /// Converts buffered `observations` into `Notification::ExternalAgent`
    /// events, skipping any already consumed on a prior resume.
    ///
    /// Per design §5.5 the machine replays a decision point's observations exactly
    /// once. Each [`ExternalObservedEvent`] carries its own runtime
    /// [`seq`](ExternalObservedEvent::seq); the machine compares it against the
    /// last consumed sequence recorded in the retained
    /// [`session`](ExternalAgentState::session) facts
    /// ([`ExternalSessionRef::last_event_seq`]), reading that value *before* the
    /// caller stores the incoming session. Only events whose `seq` is strictly
    /// greater than the consumed high-water mark are emitted, so a replayed
    /// result (whose events all fall at or below the mark) emits nothing while an
    /// overlapping result replays only its unseen suffix. When no session has been
    /// consumed yet the events cannot be aligned and are all emitted as-is.
    fn observe(&self, observations: Vec<ExternalObservedEvent>) -> Vec<Notification> {
        let consumed = self
            .state
            .session()
            .and_then(|session| session.last_event_seq);
        observations
            .into_iter()
            .filter(|observed| consumed.is_none_or(|consumed| observed.seq > consumed))
            .map(|observed| Notification::ExternalAgent(observed.event))
            .collect()
    }

    /// Parks on an interaction the runtime paused for and emits `NeedInteraction`.
    ///
    /// The handler translated the runtime's permission/clarification prompt into
    /// a neutral [`Interaction`]; the machine records the resumable session facts
    /// and the runtime's `action_id`, reifies one `NeedInteraction` for the
    /// standard interaction pop rules to serve, and parks on
    /// [`AwaitingInteraction`](ExternalAgentCursor::AwaitingInteraction). The
    /// in-flight turn stays open across the pause so the resolved answer folds
    /// back into the same turn.
    fn pause_for_interaction(
        &mut self,
        session: ExternalSessionRef,
        action_id: String,
        request: Interaction,
        notifications: Vec<Notification>,
    ) -> StepOutcome {
        let Some(in_flight) = self.in_flight else {
            return self.fail("external session paused without an in-flight turn");
        };

        self.state.set_session(Some(session));

        let requirement_id = match self
            .requirement_ids
            .next_requirement_id(RequirementKindTag::Interaction)
        {
            Ok(id) => id,
            Err(error) => return self.fail(format!("requirement id unavailable: {error}")),
        };

        let cursor_requirement = CursorRequirement::root(requirement_id);
        self.settle(
            ExternalAgentCursor::AwaitingInteraction {
                requirement: cursor_requirement.clone(),
                pending_action: action_id,
                interaction: request.clone(),
            },
            LoopCursor::streaming_step(in_flight.step_id, Some(cursor_requirement)),
        );

        let requirement =
            Requirement::at_root(requirement_id, RequirementKind::NeedInteraction { request });
        StepOutcome::new(notifications, vec![requirement], true)
    }

    /// Parks on a batch of host tool calls the runtime paused for, bridging each
    /// call into a host requirement.
    ///
    /// The handler surfaced the runtime's pending tool calls as provider-neutral
    /// [`ExternalToolCall`] values under a runtime-assigned
    /// [`ExternalToolBatchId`]. The machine records the resumable session facts
    /// and bridges each call, minting a host [`ToolCallId`] via the injected
    /// [`ToolExecutionIds`] and a [`RequirementId`] per bridged call:
    ///
    /// - A plain tool call becomes one [`NeedTool`](RequirementKind::NeedTool)
    ///   requirement.
    /// - A `spawn_agent` call is a scope-deepening operation that cannot run as
    ///   an inline tool, so it is parsed with
    ///   [`SpawnAgentRequest::parse`](crate::agent::collab::SpawnAgentRequest::parse)
    ///   and bridged into a standard
    ///   [`NeedSubagent`](RequirementKind::NeedSubagent) instead — reusing the
    ///   host's own subagent machinery (depth / budget / cancel / trace), never
    ///   spawning the child inline. Its child output later folds back into *this*
    ///   batch as an [`ExternalToolResult`] so the runtime sees the spawn as an
    ///   ordinary tool call that returned a summary (design §8.3).
    /// - A malformed `spawn_agent` input mints no requirement: a runtime-visible
    ///   error [`ExternalToolResult`] is pre-seeded into the batch
    ///   (return-error-to-runtime, design §8.4), so the runtime learns its call
    ///   was ill-formed while the rest of the batch proceeds and the turn stays
    ///   alive.
    ///
    /// The machine parks on
    /// [`AwaitingTool`](ExternalAgentCursor::AwaitingTool), whose driver-facing
    /// [`LoopCursor::awaiting_tool`] view carries every outstanding requirement
    /// id (tool *and* subagent), so a mixed batch parks under one cursor. The
    /// volatile per-call correlation (`RequirementId` → provider call id + bridge
    /// kind) and the collected results live in the non-serialized
    /// [`PendingExternalToolBatch`] scratch so a completed batch can feed
    /// [`RespondToolResults`](ExternalSessionInput::RespondToolResults) back in
    /// the original call order (see [`resume_tool`](Self::resume_tool)). The
    /// in-flight turn stays open across the pause so the eventual results fold
    /// back into the same turn.
    ///
    /// When every call was a malformed `spawn_agent` (so no requirement is
    /// minted) the batch is already complete: the machine skips the
    /// [`AwaitingTool`](ExternalAgentCursor::AwaitingTool) park and relays the
    /// pre-seeded error results straight back in call order.
    ///
    /// No tool or subagent result is ever written into the [`Conversation`] — the
    /// external runtime is the consumer of tool results, not host history; the
    /// machine only relays results back to the runtime.
    ///
    /// When no [`ToolExecutionIds`] source was injected (the default
    /// [`NoToolExecutionIds`]) the machine cannot mint a host tool-call
    /// identity, so it settles on a classified
    /// [`Error`](ExternalAgentCursor::Error) cursor and discards the in-flight
    /// turn. Buffered `notifications` still ride out on the failing step so a
    /// paused decision point replays its observations exactly once (design
    /// §5.5).
    ///
    /// [`Conversation`]: crate::conversation::Conversation
    fn pause_for_tool_calls(
        &mut self,
        session: ExternalSessionRef,
        batch_id: ExternalToolBatchId,
        calls: Vec<ExternalToolCall>,
        notifications: Vec<Notification>,
    ) -> StepOutcome {
        let Some(in_flight) = self.in_flight else {
            return self.fail_with(
                "external session paused for tool calls without an in-flight turn",
                notifications,
            );
        };

        if calls.is_empty() {
            return self.fail_with(
                "external session paused for tool calls with an empty batch",
                notifications,
            );
        }

        self.state.set_session(Some(session));

        // Bridge each runtime tool call into a host requirement, allocating a
        // host tool-call id and a requirement id per bridged call. `ids`
        // addresses the driver-facing awaiting-tool cursor; the `pending` map
        // addresses the eventual `RespondToolResults` fan-out (kept in the batch
        // scratch) so an out-of-order resume routes to the right call and is
        // validated against the right result family. A malformed `spawn_agent`
        // mints no requirement — its runtime-visible error result is pre-seeded
        // into `results` (return-error-to-runtime, design §8.4).
        let mut requirements = Vec::with_capacity(calls.len());
        let mut ids: BTreeMap<ToolCallId, RequirementId> = BTreeMap::new();
        let mut pending: BTreeMap<RequirementId, PendingBridgeCall> = BTreeMap::new();
        let mut results: BTreeMap<String, ExternalToolResult> = BTreeMap::new();
        for call in &calls {
            let tool_call: ToolCall = call.to_tool_call();

            // `spawn_agent` is a scope-deepening operation that cannot run as an
            // inline tool: parse it up front so a malformed call is answered with
            // a runtime-visible error instead of minting a subagent requirement
            // the machine cannot build.
            let (kind_tag, requirement_kind) = if SpawnAgentRequest::matches(&call.name) {
                match SpawnAgentRequest::parse(&tool_call) {
                    Ok(request) => (
                        RequirementKindTag::Subagent,
                        request.into_requirement_kind(in_flight.step_id),
                    ),
                    Err(error) => {
                        // Return-error-to-runtime (design §8.4): pre-seed the
                        // batch with a runtime-visible failure and mint no
                        // requirement for this call.
                        results.insert(
                            call.provider_call_id.clone(),
                            ExternalToolResult::from_tool_runtime_error(
                                call.provider_call_id.clone(),
                                &ToolRuntimeError::ExecutionFailed {
                                    tool_name: SPAWN_AGENT.to_owned(),
                                    message: error.to_string(),
                                },
                            ),
                        );
                        continue;
                    }
                }
            } else {
                (
                    RequirementKindTag::Tool,
                    RequirementKind::NeedTool {
                        // The tool-call id is minted below; a placeholder here keeps
                        // the two bridge arms symmetric.
                        call_id: match self.tool_ids.tool_call_id(&tool_call) {
                            Ok(id) => id,
                            Err(error) => {
                                return self.fail_tool_id_unavailable(
                                    ExternalCapability::HostTools,
                                    &error,
                                    notifications,
                                );
                            }
                        },
                        call: tool_call.clone(),
                    },
                )
            };

            // Every bridged call (tool or subagent) is addressed under a host
            // tool-call id: it is the key the `AwaitingTool` cursor binds its
            // requirement under, so the whole mixed batch parks on one cursor and
            // recovers uniformly. A `NeedTool` already carries the minted id;
            // mint a fresh one for a subagent bridge.
            let call_id = match &requirement_kind {
                RequirementKind::NeedTool { call_id, .. } => *call_id,
                _ => match self.tool_ids.tool_call_id(&tool_call) {
                    Ok(id) => id,
                    Err(error) => {
                        return self.fail_tool_id_unavailable(
                            ExternalCapability::HostSubagents,
                            &error,
                            notifications,
                        );
                    }
                },
            };
            let requirement_id = match self.requirement_ids.next_requirement_id(kind_tag) {
                Ok(id) => id,
                Err(error) => {
                    return self.fail_with(
                        format!("requirement id unavailable: {error}"),
                        notifications,
                    );
                }
            };
            requirements.push(Requirement::at_root(requirement_id, requirement_kind));
            ids.insert(call_id, requirement_id);
            pending.insert(
                requirement_id,
                PendingBridgeCall {
                    provider_call_id: call.provider_call_id.clone(),
                    kind: if kind_tag == RequirementKindTag::Subagent {
                        ExternalBridgeCallKind::Subagent
                    } else {
                        ExternalBridgeCallKind::Tool
                    },
                },
            );
        }

        // Every call was a malformed `spawn_agent`: no requirement is
        // outstanding, so the batch is already complete and relays its
        // pre-seeded error results straight back in the original call order.
        if requirements.is_empty() {
            let batch = PendingExternalToolBatch {
                batch_id,
                calls,
                pending,
                results,
            };
            return self.respond_with_tool_batch(in_flight.step_id, batch);
        }

        let call_ids: Vec<ToolCallId> = ids.keys().copied().collect();
        let requirements_addr = ToolWaitRequirements::root(ids);
        let loop_cursor = match LoopCursor::awaiting_tool(
            in_flight.step_id,
            call_ids,
            Some(requirements_addr.clone()),
        ) {
            Ok(cursor) => cursor,
            Err(error) => {
                return self.fail_with(
                    format!("tool-wait cursor build failed: {error}"),
                    notifications,
                );
            }
        };

        self.pending_tool_batch = Some(PendingExternalToolBatch {
            batch_id: batch_id.clone(),
            calls,
            pending,
            results,
        });
        self.settle(
            ExternalAgentCursor::AwaitingTool {
                batch_id,
                requirements: requirements_addr,
            },
            loop_cursor,
        );

        StepOutcome::new(notifications, requirements, true)
    }

    /// Folds one fulfilled batch requirement into the pending tool batch,
    /// relaying the whole batch back to the runtime once every call is answered.
    ///
    /// Each host result arrives on its own resume. The result is routed to its
    /// runtime `provider_call_id` through the [`PendingExternalToolBatch`]
    /// scratch (keyed by [`RequirementId`], so a parallel batch may resolve out
    /// of order) and collected there, validated against the family the call was
    /// bridged as:
    ///
    /// - A [`Tool`](ExternalBridgeCallKind::Tool) bridge accepts a
    ///   [`RequirementResult::Tool(Ok(response))`](RequirementResult::Tool) —
    ///   bridged into an [`ExternalToolResult`] preserving the host's four-state
    ///   status and multimodal content, re-keyed to the batch's authoritative
    ///   `provider_call_id` — or a
    ///   [`RequirementResult::Tool(Err(error))`](RequirementResult::Tool),
    ///   returned to the runtime as a failed tool result carrying the framework's
    ///   stable diagnostic (the fixed *return-error-to-runtime* policy — the
    ///   external runtime, not the host, decides how to react to a failed call),
    ///   never stopping the host turn.
    /// - A [`Subagent`](ExternalBridgeCallKind::Subagent) bridge (a `spawn_agent`
    ///   call) accepts a
    ///   [`RequirementResult::Subagent(Ok(output))`](RequirementResult::Subagent),
    ///   folding the child's summary into a successful [`ExternalToolResult`] so
    ///   the runtime sees the spawn as a tool call that returned a summary
    ///   (design §8.3). A
    ///   [`RequirementResult::Subagent(Err(error))`](RequirementResult::Subagent)
    ///   is a host-orchestration failure (depth / budget / cancel / internal),
    ///   symmetric with the standalone
    ///   [`resume_subagent`](Self::resume_subagent) path: it settles the machine
    ///   on a classified [`Error`](ExternalAgentCursor::Error) cursor and stops
    ///   the turn rather than fabricating a runtime result.
    /// - A result whose family does not match how its call was bridged is a
    ///   protocol violation and settles on a classified error cursor.
    ///
    /// While the batch is incomplete the machine stays parked on
    /// [`AwaitingTool`](ExternalAgentCursor::AwaitingTool) with a quiescent, empty
    /// outcome — it emits no new requirement and does not advance the session.
    /// When the final result lands the collected [`ExternalToolResult`] values
    /// are assembled in the runtime's original call order (never completion
    /// order) and fed back through one
    /// [`RespondToolResults`](ExternalSessionInput::RespondToolResults) under the
    /// paused [`ExternalToolBatchId`], reparking on
    /// [`AwaitingSession`](ExternalAgentCursor::AwaitingSession) so the session
    /// can advance to its next decision point within the same turn.
    ///
    /// A resume with no live batch scratch (for example after a mid-turn restore
    /// that recovered the cursor but not the volatile scratch), an unknown
    /// requirement id, or a duplicate result for an already-answered call is a
    /// protocol violation and settles on a classified error cursor.
    fn resume_tool(&mut self, resolution: RequirementResolution) -> StepOutcome {
        // The batch scratch holds the volatile per-call correlation the resume
        // routes against. A mid-turn restore recovers the cursor but not this
        // scratch, so a resume with no scratch cannot reassemble the batch.
        let Some(batch) = self.pending_tool_batch.as_ref() else {
            return self.fail("tool result resumed without a pending tool batch");
        };
        let Some(in_flight) = self.in_flight else {
            return self.fail("tool result resumed without an in-flight turn");
        };

        // Route by requirement id: an id outside the batch is not one this pause
        // is waiting on.
        let Some(bridged) = batch.pending.get(&resolution.id).cloned() else {
            return self.fail(format!(
                "resume targets requirement {}, which is not part of the pending tool batch",
                resolution.id
            ));
        };
        let PendingBridgeCall {
            provider_call_id,
            kind,
        } = bridged;

        // A second result for the same call is a duplicate resume.
        if batch.results.contains_key(&provider_call_id) {
            return self.fail(format!(
                "resume targets requirement {}, whose result was already collected",
                resolution.id
            ));
        }

        // Bridge the host result into the runtime-facing tool result, validated
        // against the family this call was bridged as. The `provider_call_id` is
        // taken from the batch mapping, the authoritative correlation the runtime
        // paused on.
        let tool_result = match kind {
            ExternalBridgeCallKind::Tool => match resolution.result {
                // A runtime error is returned to the runtime as a failed tool
                // result (the default return-error-to-runtime policy) unless the
                // machine is configured to stop the run, in which case the host
                // turn fails on a classified error cursor instead of relaying the
                // failure (design §8.4).
                RequirementResult::Tool(Ok(response)) => {
                    let mut result = ExternalToolResult::from_tool_response(&response);
                    result.provider_call_id = provider_call_id.clone();
                    result
                }
                RequirementResult::Tool(Err(error)) => {
                    if self.config.tool_failure() == ExternalToolFailurePolicy::StopRun {
                        return self.fail(format!(
                            "external host tool failed under stop-run policy: {error}"
                        ));
                    }
                    ExternalToolResult::from_tool_runtime_error(provider_call_id.clone(), &error)
                }
                other => {
                    return self.fail(format!(
                        "NeedTool requirement cannot accept a `{}` result",
                        other.tag()
                    ));
                }
            },
            ExternalBridgeCallKind::Subagent => match resolution.result {
                // The child's summary folds back as a successful tool result so
                // the runtime sees the spawn as a tool call that returned a
                // summary (design §8.3).
                RequirementResult::Subagent(Ok(output)) => {
                    let ExternalSubagentOutput { summary, .. } =
                        ExternalSubagentOutput::from(output);
                    ExternalToolResult {
                        provider_call_id: provider_call_id.clone(),
                        status: ToolStatus::Ok,
                        content: vec![ContentBlock::Text {
                            text: summary,
                            extra: Map::new(),
                        }],
                        error: None,
                        raw: None,
                    }
                }
                // A subagent drive failure is a host-orchestration failure,
                // symmetric with the standalone subagent path: stop the turn on a
                // classified error cursor rather than fabricating a runtime result.
                RequirementResult::Subagent(Err(error)) => {
                    return self.fail(format!("external spawn_agent subagent failed: {error}"));
                }
                other => {
                    return self.fail(format!(
                        "spawn_agent bridge (NeedSubagent) requirement cannot accept a `{}` result",
                        other.tag()
                    ));
                }
            },
        };

        let batch = self
            .pending_tool_batch
            .as_mut()
            .expect("pending tool batch present after the immutable borrow above");
        batch.results.insert(provider_call_id, tool_result);
        if batch.results.len() < batch.calls.len() {
            // The batch is still incomplete: stay parked on AwaitingTool without
            // emitting a new requirement or advancing the session.
            return StepOutcome::new(Vec::new(), Vec::new(), true);
        }

        // Every call is answered: assemble the results in the runtime's original
        // call order and feed them back under the paused batch id.
        let batch = self
            .pending_tool_batch
            .take()
            .expect("pending tool batch present after the collection above");
        self.respond_with_tool_batch(in_flight.step_id, batch)
    }

    /// Assembles a fully answered batch in the runtime's original call order and
    /// relays it back as one
    /// [`RespondToolResults`](ExternalSessionInput::RespondToolResults).
    ///
    /// The results are ordered by the runtime's original
    /// [`ExternalToolCall`] sequence (never completion order), reparking on
    /// [`AwaitingSession`](ExternalAgentCursor::AwaitingSession) so the session
    /// advances to its next decision point within the same turn. A call with no
    /// collected result is a protocol violation and settles on a classified
    /// error cursor.
    fn respond_with_tool_batch(
        &mut self,
        step_id: StepId,
        batch: PendingExternalToolBatch,
    ) -> StepOutcome {
        let mut results = Vec::with_capacity(batch.calls.len());
        for call in &batch.calls {
            match batch.results.get(&call.provider_call_id) {
                Some(result) => results.push(result.clone()),
                None => {
                    return self.fail(format!(
                        "tool batch completed without a result for call `{}`",
                        call.provider_call_id
                    ));
                }
            }
        }

        self.block_on_session(
            step_id,
            ExternalSessionInput::RespondToolResults {
                batch_id: batch.batch_id,
                results,
            },
        )
    }

    /// Fails a tool-call pause that could not mint a host tool-call id.
    ///
    /// The missing id source is the same defect either way, but the diagnostic
    /// is sharpened when the run *declared* it requires the corresponding managed
    /// capability: a configured [`HostTools`](ExternalCapability::HostTools) or
    /// [`HostSubagents`](ExternalCapability::HostSubagents) requirement surfaces
    /// a classified [`UnsupportedCapability`](ExternalAgentError::UnsupportedCapability)
    /// naming the runtime and capability (design §15), so a scheduler avoids
    /// re-dispatching that worker. Without the requirement the generic
    /// id-unavailable failure is preserved, matching pre-M4-3 behavior. The
    /// detail is a stable diagnostic and carries no prompt or tool input.
    fn fail_tool_id_unavailable(
        &mut self,
        capability: ExternalCapability,
        id_error: &ToolRuntimeError,
        notifications: Vec<Notification>,
    ) -> StepOutcome {
        let message = if self.config.requires(capability) {
            ExternalAgentError::UnsupportedCapability {
                runtime: self.state.spec().runtime().clone(),
                capability,
                detail: "host provided no tool-call id source for runtime-initiated calls"
                    .to_owned(),
            }
            .to_string()
        } else {
            format!("tool id unavailable: {id_error}")
        };
        self.fail_with(message, notifications)
    }

    /// Parks on a subagent spawn the runtime paused for and emits one
    /// `NeedSubagent`.
    ///
    /// The handler surfaced the runtime's native child-task request as a
    /// provider-neutral [`ExternalSubagentRequest`]. The machine records the
    /// resumable session facts, bridges the request into a standard
    /// [`NeedSubagent`](RequirementKind::NeedSubagent) requirement — reusing its
    /// [`spec_ref`](ExternalSubagentRequest::spec_ref),
    /// [`brief`](ExternalSubagentRequest::brief), and
    /// [`result_schema`](ExternalSubagentRequest::result_schema) unchanged, never
    /// spawning the child outside the host's own subagent machinery (design §4,
    /// §5.2) — and parks on
    /// [`AwaitingSubagent`](ExternalAgentCursor::AwaitingSubagent). The child is
    /// driven by the host's [`DrivingSubagentHandler`](crate::agent), which owns
    /// the depth / budget / cancel accounting and pops the child's unhandled
    /// requirements out past the subagent handler; the machine only reifies the
    /// requirement. The runtime's [`ExternalSubagentRequestId`] is held in the
    /// serializable cursor so the eventual output feeds a
    /// [`RespondSubagent`](ExternalSessionInput::RespondSubagent) echoing it (see
    /// [`resume_subagent`](Self::resume_subagent)). The in-flight turn stays open
    /// across the pause so the child's result folds back into the same turn.
    ///
    /// The provider [`raw`](ExternalSubagentRequest::raw) escape hatch is not
    /// carried into the host requirement — it holds unmodeled provider fields
    /// that must not drive stable host logic (design §5.3).
    ///
    /// No subagent output is ever written into the [`Conversation`] — the
    /// external runtime is the consumer of the child's result, not host history;
    /// the machine only relays the summary back to the runtime.
    ///
    /// [`Conversation`]: crate::conversation::Conversation
    fn pause_for_subagent(
        &mut self,
        session: ExternalSessionRef,
        request: ExternalSubagentRequest,
        notifications: Vec<Notification>,
    ) -> StepOutcome {
        let Some(in_flight) = self.in_flight else {
            return self.fail_with(
                "external session paused for a subagent without an in-flight turn",
                notifications,
            );
        };

        self.state.set_session(Some(session));

        let requirement_id = match self
            .requirement_ids
            .next_requirement_id(RequirementKindTag::Subagent)
        {
            Ok(id) => id,
            Err(error) => {
                return self.fail_with(
                    format!("requirement id unavailable: {error}"),
                    notifications,
                );
            }
        };

        let ExternalSubagentRequest {
            request_id,
            spec_ref,
            brief,
            result_schema,
            raw: _,
        } = request;

        let cursor_requirement = CursorRequirement::root(requirement_id);
        self.settle(
            ExternalAgentCursor::AwaitingSubagent {
                requirement: cursor_requirement.clone(),
                request_id,
            },
            LoopCursor::streaming_step(in_flight.step_id, Some(cursor_requirement)),
        );

        let requirement = Requirement::at_root(
            requirement_id,
            RequirementKind::NeedSubagent {
                spec_ref,
                brief,
                result_schema,
            },
        );
        StepOutcome::new(notifications, vec![requirement], true)
    }

    /// Feeds a resolved subagent result back into the paused session.
    ///
    /// The host drove the child under its own subagent machinery and delivered a
    /// [`RequirementResult::Subagent`]:
    ///
    /// - A [`RequirementResult::Subagent(Ok(output))`](RequirementResult::Subagent)
    ///   is bridged into an [`ExternalSubagentOutput`] and fed back to the runtime
    ///   as a fresh [`RespondSubagent`](ExternalSessionInput::RespondSubagent)
    ///   that echoes the `request_id` the pause carried, reparking on
    ///   [`AwaitingSession`](ExternalAgentCursor::AwaitingSession) so the session
    ///   can advance to its next decision point within the same turn.
    /// - A [`RequirementResult::Subagent(Err(error))`](RequirementResult::Subagent)
    ///   settles the machine on a classified
    ///   [`Error`](ExternalAgentCursor::Error) cursor. A runtime-visible child
    ///   error payload is deferred (design: this first version stops the host
    ///   turn rather than fabricating a `RespondSubagent` error the runtime has
    ///   no contract for).
    /// - Any other result family is a protocol violation and settles on a
    ///   classified error cursor.
    ///
    /// A wrong requirement id is likewise a protocol violation and settles on an
    /// error cursor.
    fn resume_subagent(
        &mut self,
        expected: RequirementId,
        request_id: ExternalSubagentRequestId,
        resolution: RequirementResolution,
    ) -> StepOutcome {
        if resolution.id != expected {
            return self.fail(format!(
                "resume targets requirement {}, but the machine awaits {expected}",
                resolution.id
            ));
        }

        let output = match resolution.result {
            RequirementResult::Subagent(Ok(output)) => output,
            RequirementResult::Subagent(Err(error)) => {
                return self.fail(format!("external subagent failed: {error}"));
            }
            other => {
                return self.fail(format!(
                    "NeedSubagent requirement cannot accept a `{}` result",
                    other.tag()
                ));
            }
        };

        let Some(in_flight) = self.in_flight else {
            return self.fail("subagent resolved without an in-flight turn");
        };

        self.block_on_session(
            in_flight.step_id,
            ExternalSessionInput::RespondSubagent {
                request_id,
                output: ExternalSubagentOutput::from(output),
            },
        )
    }

    /// Feeds a resolved interaction back into the paused session.
    ///
    /// The host's [`InteractionResponse`] is first validated against the
    /// [`Interaction`] the runtime paused for via
    /// [`Interaction::accepts_response`]: the response family must match the
    /// request family, a [`Choice`](crate::agent::InteractionKind::Choice) index
    /// must fall within the offered options, and a
    /// [`Permission`](crate::agent::InteractionKind::Permission) response must
    /// carry the same `action_id` as the pending request. A response that fails
    /// this check is a protocol violation: the machine settles on a classified
    /// [`Error`](ExternalAgentCursor::Error) cursor and *never* forwards the
    /// invalid answer to the runtime.
    ///
    /// Only a validated [`InteractionResponse`] is handed to the runtime as a
    /// fresh [`RespondInteraction`](ExternalSessionInput::RespondInteraction)
    /// that echoes the `pending_action` the pause carried, reparking on
    /// [`AwaitingSession`](ExternalAgentCursor::AwaitingSession) so the session
    /// can advance to its next decision point (another pause, completion, or
    /// failure) within the same turn.
    ///
    /// A wrong requirement id, or a non-interaction result family, is likewise a
    /// protocol violation that settles on an error cursor.
    ///
    /// [`InteractionResponse`]: crate::agent::InteractionResponse
    fn resume_interaction(
        &mut self,
        expected: RequirementId,
        pending_action: String,
        interaction: Interaction,
        resolution: RequirementResolution,
    ) -> StepOutcome {
        if resolution.id != expected {
            return self.fail(format!(
                "resume targets requirement {}, but the machine awaits {expected}",
                resolution.id
            ));
        }

        let response = match resolution.result {
            RequirementResult::Interaction(response) => response,
            other => {
                return self.fail(format!(
                    "NeedInteraction requirement cannot accept a `{}` result",
                    other.tag()
                ));
            }
        };

        // The runtime has no contract for an ill-formed answer, so validate the
        // response family/shape against the pending interaction before relaying
        // it; a rejected response settles on an error cursor rather than being
        // forwarded. The `InteractionError` `Display` is a stable diagnostic
        // (families, indices, opaque action ids) and carries no transcript.
        if let Err(error) = interaction.accepts_response(&response) {
            return self.fail(format!("external interaction response rejected: {error}"));
        }

        let Some(in_flight) = self.in_flight else {
            return self.fail("interaction resolved without an in-flight turn");
        };

        self.block_on_session(
            in_flight.step_id,
            ExternalSessionInput::RespondInteraction {
                action_id: pending_action,
                response,
            },
        )
    }

    /// Records the terminal output, commits the turn, and settles on `Done`.
    ///
    /// The runtime's reported [`ExternalArtifactRef`](super::ExternalArtifactRef)
    /// list is folded into the retained trace via
    /// [`ExternalAgentState::record_artifacts`] before the turn commits. Only the
    /// artifact references (kind, summary, opaque path/reference) are stored — the
    /// underlying diff/log/blob is never inlined — keeping the persisted state
    /// redaction-safe (design §11, §12).
    fn complete_session(
        &mut self,
        session: ExternalSessionRef,
        output: ExternalAgentOutput,
        notifications: Vec<Notification>,
    ) -> StepOutcome {
        let Some(in_flight) = self.in_flight.take() else {
            return self.fail("external session completed without an in-flight turn to commit");
        };

        self.state.set_session(Some(session));

        let response = assistant_response(&output);
        self.state.record_artifacts(output.artifacts);
        if let Err(error) = self
            .state
            .conversation_mut()
            .start_assistant_response(response)
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }
        if let Err(error) = self
            .state
            .conversation_mut()
            .finish_assistant(in_flight.assistant_message_id)
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }
        if let Err(error) = self
            .state
            .conversation_mut()
            .commit_pending(TurnMeta::default())
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        self.settle(
            ExternalAgentCursor::Done,
            LoopCursor::done(LoopDoneReason::Completed),
        );
        StepOutcome::new(notifications, Vec::new(), true)
    }

    /// Handles a never-resume [`StepInput::Abandon`] (cancel, design §6.4).
    ///
    /// Cancellation of an external agent is **never-resume**: once the driver
    /// abandons the continuation the machine is not stepped again, so it can
    /// never emit a graceful
    /// [`Shutdown`](ExternalSessionInput::Shutdown). Closing the live session
    /// (killing the CLI process, dropping the SDK connection, aborting the
    /// background reader task) is therefore **the handle layer's job**, not the
    /// machine's — see [`ExternalRuntimeHandles`](super::ExternalRuntimeHandles).
    /// This step only settles the pure state:
    ///
    /// - When abandoning an outstanding session/interaction/tool/subagent step
    ///   ([`AwaitingSession`](ExternalAgentCursor::AwaitingSession) /
    ///   [`AwaitingInteraction`](ExternalAgentCursor::AwaitingInteraction) /
    ///   [`AwaitingTool`](ExternalAgentCursor::AwaitingTool) /
    ///   [`AwaitingSubagent`](ExternalAgentCursor::AwaitingSubagent)) a
    ///   runtime session may be live, so it flags
    ///   [`ExternalAgentState::mark_cleanup_required`] for the handle layer to
    ///   sweep; the resumable [`session`](ExternalAgentState::session) facts stay
    ///   recorded so the runtime can still be resumed if it supports it.
    /// - The dangling pending turn is discarded and the cursor settles back to a
    ///   feedable [`Idle`](ExternalAgentCursor::Idle), so a fresh
    ///   [`AgentInput::UserMessage`](crate::agent::AgentInput::UserMessage) can
    ///   open the next turn.
    ///
    /// It emits no new requirement: the abandon does not perform a
    /// `Shutdown` effect. The forced-close disposition
    /// ([`ExternalSessionShutdown`](super::ExternalSessionShutdown)) is recorded
    /// by the handle layer into the trace, not here.
    fn abandon(&mut self, _id: RequirementId) -> StepOutcome {
        // An outstanding session/interaction/tool/subagent step means the
        // runtime may have a live session the handle layer must force-close
        // (§6.4). Idle/terminal cursors have nothing in flight, so there is
        // nothing to sweep.
        if self.state.cursor().has_outstanding_requirement() {
            self.state.mark_cleanup_required();
        }
        if self.state.conversation().pending().is_some() {
            let _ = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn);
        }
        self.in_flight = None;
        self.pending_tool_batch = None;
        self.settle(ExternalAgentCursor::Idle, LoopCursor::Idle);
        StepOutcome::new(Vec::new(), Vec::new(), true)
    }

    /// Discards any dangling pending turn and parks on a classified error cursor.
    ///
    /// `step` cannot return `Result`, so a runtime failure during a step surfaces
    /// as an [`Error`](ExternalAgentCursor::Error) cursor with a quiescent
    /// outcome, mirroring
    /// [`DefaultAgentMachine`](crate::agent::DefaultAgentMachine).
    fn fail(&mut self, message: impl Into<String>) -> StepOutcome {
        self.fail_with(message, Vec::new())
    }

    /// Like [`fail`](Self::fail), but carries observation notifications collected
    /// before the failure so a failed decision point still replays its buffered
    /// events (design §5.5).
    fn fail_with(
        &mut self,
        message: impl Into<String>,
        notifications: Vec<Notification>,
    ) -> StepOutcome {
        let message = {
            let message = message.into();
            if message.is_empty() {
                "external agent machine failed".to_owned()
            } else {
                message
            }
        };
        if self.state.conversation().pending().is_some() {
            let _ = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn);
        }
        self.in_flight = None;
        self.pending_tool_batch = None;
        let loop_cursor = LoopCursor::error(message.clone()).unwrap_or(LoopCursor::Idle);
        self.settle(ExternalAgentCursor::Error { message }, loop_cursor);
        StepOutcome::new(notifications, Vec::new(), true)
    }

    /// Sets the serializable external cursor and its mirrored driver-facing view
    /// together so the two never drift.
    fn settle(&mut self, external: ExternalAgentCursor, loop_cursor: LoopCursor) {
        self.state.set_cursor(external);
        self.loop_cursor = loop_cursor;
    }
}

impl AgentMachine for ExternalAgentMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        match input {
            StepInput::External(AgentInput::UserMessage(user)) => self.begin_user_turn(user),
            StepInput::External(AgentInput::Pivot(_)) => {
                self.fail("external agent machine does not accept pivot input")
            }
            StepInput::Resume(resolution) => self.resume(resolution),
            StepInput::Abandon(id) => self.abandon(id),
        }
    }

    fn cursor(&self) -> &LoopCursor {
        &self.loop_cursor
    }

    fn interrupt_budget_exhausted(&mut self) -> StepOutcome {
        if self.state.cursor().has_outstanding_requirement() {
            self.state.mark_cleanup_required();
        }
        if self.state.conversation().pending().is_some() {
            let _ = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn);
        }
        self.in_flight = None;
        self.pending_tool_batch = None;
        self.settle(
            ExternalAgentCursor::Done,
            LoopCursor::done(LoopDoneReason::BudgetExhausted),
        );
        StepOutcome::new(Vec::new(), Vec::new(), true)
    }
}

/// Maps an [`ExternalAgentCursor`] to the driver-facing [`LoopCursor`] view a
/// freshly constructed or restored machine starts from.
///
/// A fresh machine is [`Idle`](ExternalAgentCursor::Idle); the terminal states
/// map to their [`LoopCursor`] equivalents. A machine restored while parked on an
/// awaiting state — including the
/// [`AwaitingTool`](ExternalAgentCursor::AwaitingTool) batch and the
/// [`AwaitingSubagent`](ExternalAgentCursor::AwaitingSubagent) spawn — has no
/// step scratch to rebuild a streaming-step or tool-wait view, so it falls back
/// to the non-terminal [`LoopCursor::Idle`] rather than misreporting a terminal
/// outcome. Faithfully rehydrating the driver-facing view of a mid-flight
/// external machine is the persistence concern tracked in `PLAN.md` under the
/// "恢复 mid-turn scratch" risk (lift the pending tool/subagent facts into the
/// serializable cursor and add restore coverage); it is out of scope for the
/// parity milestones that only land the cursor phases here.
fn initial_loop_cursor(cursor: &ExternalAgentCursor) -> LoopCursor {
    match cursor {
        ExternalAgentCursor::Idle
        | ExternalAgentCursor::AwaitingSession { .. }
        | ExternalAgentCursor::AwaitingInteraction { .. }
        | ExternalAgentCursor::AwaitingTool { .. }
        | ExternalAgentCursor::AwaitingSubagent { .. } => LoopCursor::Idle,
        ExternalAgentCursor::Done => LoopCursor::done(LoopDoneReason::Completed),
        ExternalAgentCursor::Error { message } => {
            LoopCursor::error(message.clone()).unwrap_or(LoopCursor::Idle)
        }
    }
}

/// Returns the snake-case label of an external cursor for diagnostics.
const fn cursor_label(cursor: &ExternalAgentCursor) -> &'static str {
    match cursor {
        ExternalAgentCursor::Idle => "idle",
        ExternalAgentCursor::AwaitingSession { .. } => "awaiting_session",
        ExternalAgentCursor::AwaitingInteraction { .. } => "awaiting_interaction",
        ExternalAgentCursor::AwaitingTool { .. } => "awaiting_tool",
        ExternalAgentCursor::AwaitingSubagent { .. } => "awaiting_subagent",
        ExternalAgentCursor::Done => "done",
        ExternalAgentCursor::Error { .. } => "error",
    }
}

/// Concatenates the text blocks of a user message into an opaque prompt string.
fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Builds a text-only assistant [`Response`] from an external session's terminal
/// output, folding through the runtime's reported usage when present.
fn assistant_response(output: &ExternalAgentOutput) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: output.summary.clone(),
                extra: Map::new(),
            }],
        },
        usage: output.usage.clone().unwrap_or_default(),
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

#[cfg(test)]
mod tests;
