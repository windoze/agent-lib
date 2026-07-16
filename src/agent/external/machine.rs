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
//!   facts and the paused action id, emits one
//!   [`NeedInteraction`](RequirementKind::NeedInteraction) for the standard
//!   interaction pop rules to serve, and parks on
//!   [`AwaitingInteraction`](ExternalAgentCursor::AwaitingInteraction). The
//!   resolved [`InteractionResponse`](crate::agent::InteractionResponse) then
//!   re-enters the session as a
//!   [`RespondInteraction`](ExternalSessionInput::RespondInteraction) that
//!   echoes the paused action id, reparking on
//!   [`AwaitingSession`](ExternalAgentCursor::AwaitingSession) so a turn can loop
//!   pause↔respond until it completes or fails.
//! - `step(Resume(ExternalSession(PausedForToolCalls)))` records the session
//!   facts and bridges every runtime [`ExternalToolCall`](super::ExternalToolCall)
//!   into one [`NeedTool`](RequirementKind::NeedTool) requirement (minting a host
//!   [`ToolCallId`](crate::conversation::ToolCallId) from the injected
//!   [`ToolExecutionIds`] and a [`RequirementId`] per call), parking on
//!   [`AwaitingTool`](ExternalAgentCursor::AwaitingTool) with the batch's
//!   volatile per-call correlation held in non-serialized scratch. Each host
//!   tool result then resumes the machine on its own hop; results are collected
//!   in the scratch and, once the whole batch is answered, relayed straight back
//!   to the runtime as one
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
//!   held in the serializable cursor. The child is driven by the host's own
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
        RequirementResult, StepId, StepInput, StepOutcome, ToolExecutionIds, ToolWaitRequirements,
        external::{
            ExternalAgentCursor, ExternalAgentOutput, ExternalAgentState, ExternalObservedEvent,
            ExternalSessionInput, ExternalSessionRef, ExternalSessionRequest,
            ExternalSessionResult, ExternalSubagentOutput, ExternalSubagentRequest,
            ExternalSubagentRequestId, ExternalToolBatchId, ExternalToolCall, ExternalToolResult,
        },
    },
    client::Response,
    conversation::{CancelDisposition, MessageId, ToolCallId, TurnMeta},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::ToolCall,
    },
};

/// Which awaiting cursor a [`resume`](ExternalAgentMachine::resume) is folding
/// into, resolved once from the borrowed cursor so the mutable transition is
/// free to run.
enum Awaiting {
    /// Parked on an outstanding `NeedExternalSession`.
    Session(RequirementId),
    /// Parked on an outstanding `NeedInteraction`, carrying the paused action id
    /// echoed back through `RespondInteraction`.
    Interaction {
        requirement: RequirementId,
        pending_action: String,
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
/// emits one [`NeedTool`](RequirementKind::NeedTool) per call and parks on
/// [`AwaitingTool`](ExternalAgentCursor::AwaitingTool). The serializable cursor
/// records only the resumable addressing (`ToolCallId -> RequirementId`); this
/// scratch holds the volatile per-call facts a completed batch needs to feed
/// [`RespondToolResults`](ExternalSessionInput::RespondToolResults) back: the
/// [`ExternalToolBatchId`], the original [`ExternalToolCall`] order, the map from
/// each host [`RequirementId`] back to its runtime `provider_call_id` (so an
/// out-of-order resume routes to the right call), and the
/// [`ExternalToolResult`] values collected so far. Like [`InFlight`], it lives
/// only while a turn is unfinished and is deliberately absent from
/// [`ExternalAgentState`], so a mid-turn restore (which recovers the cursor but
/// not this scratch) cannot resume a partially answered batch.
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
    /// Maps each host `RequirementId` back to its runtime `provider_call_id`,
    /// so an out-of-order resume routes to the right call.
    requirement_to_call: BTreeMap<RequirementId, String>,
    /// Results collected so far, keyed by `provider_call_id`.
    results: BTreeMap<String, ExternalToolResult>,
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

    /// Opens a fresh Conversation turn and blocks on one `NeedExternalSession`.
    fn begin_user_turn(&mut self, user: AgentUserInput) -> StepOutcome {
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
    fn block_on_session(&mut self, step_id: StepId, input: ExternalSessionInput) -> StepOutcome {
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
            } => Ok(Awaiting::Interaction {
                requirement: requirement.id(),
                pending_action: pending_action.clone(),
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
            }) => self.resume_interaction(requirement, pending_action, resolution),
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
            },
            LoopCursor::streaming_step(in_flight.step_id, Some(cursor_requirement)),
        );

        let requirement =
            Requirement::at_root(requirement_id, RequirementKind::NeedInteraction { request });
        StepOutcome::new(notifications, vec![requirement], true)
    }

    /// Parks on a batch of host tool calls the runtime paused for and emits one
    /// `NeedTool` per call.
    ///
    /// The handler surfaced the runtime's pending tool calls as provider-neutral
    /// [`ExternalToolCall`] values under a runtime-assigned
    /// [`ExternalToolBatchId`]. The machine records the resumable session facts,
    /// bridges each call into a [`NeedTool`](RequirementKind::NeedTool)
    /// requirement — allocating a host [`ToolCallId`] via the injected
    /// [`ToolExecutionIds`] and a [`RequirementId`] per call — and parks on
    /// [`AwaitingTool`](ExternalAgentCursor::AwaitingTool), whose driver-facing
    /// [`LoopCursor::awaiting_tool`] view carries every outstanding requirement
    /// id. The volatile per-call correlation (`RequirementId` →
    /// `provider_call_id`) and the initially empty result set live in the
    /// non-serialized [`PendingExternalToolBatch`] scratch so a completed batch
    /// can feed [`RespondToolResults`](ExternalSessionInput::RespondToolResults)
    /// back in the original call order (see [`resume_tool`](Self::resume_tool)).
    /// The in-flight turn stays open across the pause so the eventual results
    /// fold back into the same turn.
    ///
    /// No tool result is ever written into the [`Conversation`] — the external
    /// runtime is the consumer of tool results, not host history; the machine
    /// only relays host results back to the runtime.
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

        self.state.set_session(Some(session));

        // Bridge each runtime tool call into a `NeedTool` requirement, allocating
        // a host tool-call id and a requirement id per call. `ids` addresses the
        // driver-facing awaiting-tool cursor; the `requirement_to_call` map
        // addresses the eventual `RespondToolResults` fan-out (kept in the batch
        // scratch) so an out-of-order resume routes to the right call.
        let mut requirements = Vec::with_capacity(calls.len());
        let mut ids: BTreeMap<ToolCallId, RequirementId> = BTreeMap::new();
        let mut requirement_to_call: BTreeMap<RequirementId, String> = BTreeMap::new();
        for call in &calls {
            let tool_call: ToolCall = call.to_tool_call();
            let call_id = match self.tool_ids.tool_call_id(&tool_call) {
                Ok(id) => id,
                Err(error) => {
                    return self.fail_with(format!("tool id unavailable: {error}"), notifications);
                }
            };
            let requirement_id = match self
                .requirement_ids
                .next_requirement_id(RequirementKindTag::Tool)
            {
                Ok(id) => id,
                Err(error) => {
                    return self.fail_with(
                        format!("requirement id unavailable: {error}"),
                        notifications,
                    );
                }
            };
            requirements.push(Requirement::at_root(
                requirement_id,
                RequirementKind::NeedTool {
                    call_id,
                    call: tool_call,
                },
            ));
            ids.insert(call_id, requirement_id);
            requirement_to_call.insert(requirement_id, call.provider_call_id.clone());
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
            requirement_to_call,
            results: BTreeMap::new(),
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

    /// Folds one fulfilled `NeedTool` result into the pending tool batch,
    /// relaying the whole batch back to the runtime once every call is answered.
    ///
    /// Each host tool result arrives on its own resume. The result is routed to
    /// its runtime `provider_call_id` through the
    /// [`PendingExternalToolBatch`] scratch (keyed by [`RequirementId`], so a
    /// parallel batch may resolve out of order) and collected there:
    ///
    /// - A [`RequirementResult::Tool(Ok(response))`](RequirementResult::Tool) is
    ///   bridged into an [`ExternalToolResult`] preserving the host's four-state
    ///   status and multimodal content, re-keyed to the batch's authoritative
    ///   `provider_call_id`.
    /// - A [`RequirementResult::Tool(Err(error))`](RequirementResult::Tool) is
    ///   returned to the runtime as a failed tool result carrying the framework's
    ///   stable diagnostic (the fixed *return-error-to-runtime* policy — the
    ///   external runtime, not the host, decides how to react to a failed call),
    ///   never stopping the host turn.
    /// - Any other result family is a protocol violation and settles the machine
    ///   on a classified [`Error`](ExternalAgentCursor::Error) cursor.
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
        let Some(provider_call_id) = batch.requirement_to_call.get(&resolution.id).cloned() else {
            return self.fail(format!(
                "resume targets requirement {}, which is not part of the pending tool batch",
                resolution.id
            ));
        };

        // A second result for the same call is a duplicate resume.
        if batch.results.contains_key(&provider_call_id) {
            return self.fail(format!(
                "resume targets requirement {}, whose tool result was already collected",
                resolution.id
            ));
        }

        // Bridge the host result into the runtime-facing tool result. A runtime
        // error is returned to the runtime as a failed tool result (the fixed
        // return-error-to-runtime policy) rather than stopping the turn. The
        // `provider_call_id` is taken from the batch mapping, the authoritative
        // correlation the runtime paused on.
        let tool_result = match resolution.result {
            RequirementResult::Tool(Ok(response)) => {
                let mut result = ExternalToolResult::from_tool_response(&response);
                result.provider_call_id = provider_call_id.clone();
                result
            }
            RequirementResult::Tool(Err(error)) => {
                ExternalToolResult::from_tool_runtime_error(provider_call_id.clone(), &error)
            }
            other => {
                return self.fail(format!(
                    "NeedTool requirement cannot accept a `{}` result",
                    other.tag()
                ));
            }
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
            in_flight.step_id,
            ExternalSessionInput::RespondToolResults {
                batch_id: batch.batch_id,
                results,
            },
        )
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
    /// The resolved [`InteractionResponse`] is handed to the runtime as a fresh
    /// [`RespondInteraction`](ExternalSessionInput::RespondInteraction) that
    /// echoes the `pending_action` the pause carried, reparking on
    /// [`AwaitingSession`](ExternalAgentCursor::AwaitingSession) so the session
    /// can advance to its next decision point (another pause, completion, or
    /// failure) within the same turn.
    fn resume_interaction(
        &mut self,
        expected: RequirementId,
        pending_action: String,
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
