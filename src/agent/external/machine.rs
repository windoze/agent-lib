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
//! # What this machine covers (M3-2, M3-3, M3-4)
//!
//! - `step(External(UserMessage))` opens a Conversation turn and blocks on one
//!   `NeedExternalSession`, choosing
//!   [`Start`](ExternalSessionInput::Start) when no session exists yet and
//!   [`Continue`](ExternalSessionInput::Continue) to advance an established one.
//! - `step(Resume(ExternalSession(Completed)))` records the resumable session
//!   facts, folds the runtime's terminal output into committed history, and
//!   settles the cursor on [`Done`](ExternalAgentCursor::Done).
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
//! events on the resuming step, deduplicated against
//! [`ExternalSessionRef::last_event_seq`] so a replayed decision point is not
//! double-emitted (design §5.5).

use std::sync::Arc;

use serde_json::Map;

use crate::{
    agent::{
        AgentInput, AgentMachine, AgentUserInput, CursorRequirement, Interaction, LoopCursor,
        LoopDoneReason, Notification, Requirement, RequirementId, RequirementIds, RequirementKind,
        RequirementKindTag, RequirementResolution, RequirementResult, StepId, StepInput,
        StepOutcome,
        external::{
            ExternalAgentCursor, ExternalAgentEvent, ExternalAgentOutput, ExternalAgentState,
            ExternalSessionInput, ExternalSessionRef, ExternalSessionRequest,
            ExternalSessionResult,
        },
    },
    client::Response,
    conversation::{CancelDisposition, MessageId, TurnMeta},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
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

/// Sans-io machine that drives one external coding-agent session step at a time.
///
/// See the [external-agent module docs](crate::agent::external) for the
/// effect-model contract, and this module's own docs for the scope of this
/// milestone.
#[derive(Debug)]
pub struct ExternalAgentMachine {
    state: ExternalAgentState,
    requirement_ids: Arc<dyn RequirementIds>,
    /// Driver-facing [`LoopCursor`] view, kept in lockstep with
    /// [`ExternalAgentState::cursor`]. `AgentMachine::cursor` must return a
    /// `&LoopCursor`, so the machine maintains this mapped mirror rather than
    /// re-deriving it on every call.
    loop_cursor: LoopCursor,
    /// Non-serialized scratch for the in-flight turn; `Some` only between opening
    /// a turn and settling it.
    in_flight: Option<InFlight>,
}

impl ExternalAgentMachine {
    /// Creates a machine over `state`, using `requirement_ids` to stamp the
    /// reified `NeedExternalSession` requirements it hands back.
    #[must_use]
    pub fn new(state: ExternalAgentState, requirement_ids: Arc<dyn RequirementIds>) -> Self {
        let loop_cursor = initial_loop_cursor(state.cursor());
        Self {
            state,
            requirement_ids,
            loop_cursor,
            in_flight: None,
        }
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
    /// resuming step's [`StepOutcome`]. Dedup uses
    /// [`ExternalSessionRef::last_event_seq`]: the events already consumed on an
    /// earlier resume are skipped so a replayed result does not double-emit — see
    /// [`observe`](Self::observe).
    fn fold_session_result(&mut self, result: ExternalSessionResult) -> StepOutcome {
        match result {
            ExternalSessionResult::Completed {
                session,
                output,
                observations,
            } => {
                let notifications = self.observe(session.last_event_seq, observations);
                self.complete_session(session, output, notifications)
            }
            ExternalSessionResult::PausedForInteraction {
                session,
                action_id,
                request,
                observations,
            } => {
                let notifications = self.observe(session.last_event_seq, observations);
                self.pause_for_interaction(session, action_id, request, notifications)
            }
            ExternalSessionResult::Failed {
                session,
                error,
                observations,
            } => {
                let notifications = self.observe(
                    session.as_ref().and_then(|s| s.last_event_seq),
                    observations,
                );
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
    /// once. It uses [`ExternalSessionRef::last_event_seq`] as the alignment
    /// cursor: the last consumed sequence is recorded in the retained
    /// [`session`](ExternalAgentState::session) facts, so this reads that recorded
    /// value *before* the caller stores the incoming session. When the incoming
    /// result reports a `last_event_seq` at or below the recorded one, its
    /// observations were already emitted on an earlier resume (a replayed or
    /// duplicated result), so nothing is emitted. When either sequence is absent
    /// the events cannot be aligned and are emitted as-is.
    fn observe(
        &self,
        incoming_seq: Option<u64>,
        observations: Vec<ExternalAgentEvent>,
    ) -> Vec<Notification> {
        if observations.is_empty() {
            return Vec::new();
        }
        let consumed = self
            .state
            .session()
            .and_then(|session| session.last_event_seq);
        if let (Some(incoming), Some(consumed)) = (incoming_seq, consumed)
            && incoming <= consumed
        {
            return Vec::new();
        }
        observations
            .into_iter()
            .map(Notification::ExternalAgent)
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
    /// - When abandoning an outstanding session/interaction step
    ///   ([`AwaitingSession`](ExternalAgentCursor::AwaitingSession) /
    ///   [`AwaitingInteraction`](ExternalAgentCursor::AwaitingInteraction)) a
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
        // An outstanding session/interaction step means the runtime may have a
        // live session the handle layer must force-close (§6.4). Idle/terminal
        // cursors have nothing in flight, so there is nothing to sweep.
        if self.state.cursor().requirement().is_some() {
            self.state.mark_cleanup_required();
        }
        if self.state.conversation().pending().is_some() {
            let _ = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn);
        }
        self.in_flight = None;
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
/// awaiting state has no step scratch to rebuild a streaming-step view, so it
/// falls back to [`LoopCursor::Idle`]; faithfully rehydrating the driver-facing
/// view of a mid-flight external machine is a persistence concern beyond
/// milestone 3's scope.
fn initial_loop_cursor(cursor: &ExternalAgentCursor) -> LoopCursor {
    match cursor {
        ExternalAgentCursor::Idle
        | ExternalAgentCursor::AwaitingSession { .. }
        | ExternalAgentCursor::AwaitingInteraction { .. } => LoopCursor::Idle,
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
