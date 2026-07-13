//! Concrete sans-io Agent machine for the LLM and tool steps.
//!
//! [`DefaultAgentMachine`] is the effect-model counterpart of the legacy
//! [`DefaultAgentLoop`](crate::agent::DefaultAgentLoop): instead of awaiting the
//! client, tools, or approvals internally, it *requests* each effect by handing
//! back a [`Requirement`] and parking on the matching [`LoopCursor`]. A driver
//! fulfils the requirement and feeds the result back through
//! [`StepInput::Resume`], at which point the machine folds it into the single
//! active Conversation using the same checked pending boundary the legacy loop
//! uses (`start_assistant_response` → `finish_assistant` →
//! `register_tool_calls` / `append_tool_response` → `commit_pending`).
//!
//! Two milestones build this up:
//!
//! - **M2-3** — the text-only turn end to end
//!   (`begin_turn → NeedLlm → fold Response → commit → quiescent`).
//! - **M2-4** — the tool step: a tool-use response opens a tool phase that emits
//!   [`RequirementKind::NeedInteraction`] (approval) and
//!   [`RequirementKind::NeedTool`] batches, folds each
//!   [`RequirementResult`](crate::agent::RequirementResult) back, then asks for
//!   the next [`RequirementKind::NeedLlm`] until the model returns a final
//!   text response (`tool → llm → … → text → commit`).
//!
//! Cancellation lands here in **M4-1**: [`StepInput::Abandon`] is a never-resume
//! close (migration doc §7) that drives the in-flight turn's Conversation
//! through [`cancel_pending`](crate::conversation::Conversation::cancel_pending)
//! and settles the cursor back to a feedable [`LoopCursor::Idle`]. Pivot
//! injection remains out of scope here and lands in a later M4 task; until then
//! it resolves to a classified error cursor rather than being silently ignored.
//!
//! The machine is pure: [`step`](AgentMachine::step) never `await`s and never
//! touches a client, tool, or process. The non-serialized fields are the
//! host-supplied identity/policy handles (the same "library never mints ids"
//! boundary as [`ToolExecutionIds`](crate::agent::ToolExecutionIds)) plus the
//! [`InFlight`] scratch state carried across the steps of one in-flight turn
//! (mirroring the legacy segment locals); the serializable machine state is the
//! wrapped [`AgentState`], whose [`LoopCursor`] records the outstanding
//! [`RequirementId`](crate::agent::RequirementId)(s).

mod tools;

use crate::{
    agent::{
        AgentInput, AgentMachine, AgentState, AgentUserInput, CancelRecoveryReason,
        CursorRequirement, LlmStepMode, LoopCursor, LoopDoneReason, NoApprovalPolicy,
        NoToolExecutionIds, Notification, Requirement, RequirementId, RequirementIds,
        RequirementKind, RequirementKindTag, RequirementResolution, RequirementResult,
        StepBoundary, StepId, StepInput, StepOutcome, ToolApprovalPolicy, ToolExecutionIds,
        request::build_chat_request,
    },
    client::Response,
    conversation::{AssistantFinish, CancelDisposition, TurnMeta},
};
use std::sync::Arc;

use tools::InFlight;

/// Which never-resume close an [`abandon`](DefaultAgentMachine::abandon) takes,
/// selected by the parked cursor before the borrow of the cursor is released.
#[derive(Clone, Copy)]
enum AbandonKind {
    /// Only an LLM step is outstanding — discard the pending turn wholesale.
    Llm,
    /// A tool batch or approval is outstanding — resume with synthesized
    /// `Cancelled` results to close the dangling tool_use.
    Tool,
}

/// Sans-io Agent machine that drives text and tool turns.
///
/// See the [`machine`](crate::agent::machine) module docs for the effect-model
/// contract and scope.
#[derive(Debug)]
pub struct DefaultAgentMachine {
    state: AgentState,
    mode: LlmStepMode,
    requirement_ids: Arc<dyn RequirementIds>,
    /// Host-supplied identity source for tool-call bookkeeping (framework call
    /// ids, tool-result message ids, and the next assistant/step ids). Defaults
    /// to [`NoToolExecutionIds`]; a machine that must run tools supplies a real
    /// source via [`with_tool_execution_ids`](Self::with_tool_execution_ids).
    tool_ids: Arc<dyn ToolExecutionIds>,
    /// Pure approval policy consulted per tool call to split auto-approved calls
    /// from those that must first emit a `NeedInteraction`. Defaults to
    /// [`NoApprovalPolicy`] (never pauses).
    approval_policy: Arc<dyn ToolApprovalPolicy>,
    /// Scratch state for the turn currently in flight: the current step's
    /// assistant message id, the count of LLM steps started this turn (for the
    /// step limit), and the active tool phase, if any. This mirrors the legacy
    /// segment's stack locals — it lives only while a turn is unfinished and is
    /// therefore not part of the serializable [`AgentState`]. The cursor still
    /// records *which* requirement the machine is stuck on.
    in_flight: Option<InFlight>,
}

impl DefaultAgentMachine {
    /// Creates a machine over `state`, using `mode` for the LLM transport and
    /// `requirement_ids` to stamp reified requirements.
    ///
    /// Tool orchestration defaults to [`NoToolExecutionIds`] (no host id source)
    /// and [`NoApprovalPolicy`] (never pauses). A machine that must run tools
    /// supplies real handles via
    /// [`with_tool_execution_ids`](Self::with_tool_execution_ids) and
    /// [`with_approval_policy`](Self::with_approval_policy).
    #[must_use]
    pub fn new(
        state: AgentState,
        mode: LlmStepMode,
        requirement_ids: Arc<dyn RequirementIds>,
    ) -> Self {
        Self {
            state,
            mode,
            requirement_ids,
            tool_ids: Arc::new(NoToolExecutionIds),
            approval_policy: Arc::new(NoApprovalPolicy),
            in_flight: None,
        }
    }

    /// Sets the host-supplied identity source used for tool-call bookkeeping.
    #[must_use]
    pub fn with_tool_execution_ids(mut self, tool_ids: Arc<dyn ToolExecutionIds>) -> Self {
        self.tool_ids = tool_ids;
        self
    }

    /// Sets the pure approval policy consulted for each tool call.
    #[must_use]
    pub fn with_approval_policy(mut self, approval_policy: Arc<dyn ToolApprovalPolicy>) -> Self {
        self.approval_policy = approval_policy;
        self
    }

    /// Returns the LLM transport mode requested by this machine.
    #[must_use]
    pub const fn mode(&self) -> LlmStepMode {
        self.mode
    }

    /// Returns a read-only view of the wrapped serializable Agent state.
    #[must_use]
    pub const fn state(&self) -> &AgentState {
        &self.state
    }

    /// Consumes the machine and returns its serializable Agent state.
    #[must_use]
    pub fn into_state(self) -> AgentState {
        self.state
    }

    /// Opens a fresh user turn and blocks on one `NeedLlm` requirement.
    fn begin_user_turn(&mut self, user: AgentUserInput) -> StepOutcome {
        // A never-resume abandon of a tool batch leaves a *coherent* pending turn
        // (its dangling tool_use closed by synthesized `Cancelled` results) with
        // the cursor settled at `Idle`. A new user turn supersedes that
        // interrupted turn, so discard the leftover transaction before
        // `begin_turn` opens the next one (which rejects a second open pending).
        if matches!(self.state.loop_cursor(), LoopCursor::Idle)
            && self.state.conversation().pending().is_some()
            && let Err(error) = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn)
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        if let Err(error) = self.state.conversation_mut().begin_turn(
            user.turn_id(),
            user.message_id(),
            user.message().clone(),
        ) {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        self.in_flight = Some(InFlight::new(user.assistant_message_id()));
        self.block_on_llm(user.step_id(), Vec::new())
    }

    /// Builds the next generation request and parks on one `NeedLlm` requirement.
    ///
    /// Shared by the opening user turn and by the post-tool continuation: both
    /// allocate an LLM requirement, render the [`ChatRequest`](crate::client::ChatRequest)
    /// from current state, and transition to [`LoopCursor::StreamingStep`]. The
    /// caller supplies any notifications produced earlier in the same step (for
    /// example a tool step boundary) to emit alongside the requirement.
    fn block_on_llm(&mut self, step_id: StepId, notifications: Vec<Notification>) -> StepOutcome {
        let requirement_id = match self
            .requirement_ids
            .next_requirement_id(RequirementKindTag::Llm)
        {
            Ok(id) => id,
            Err(error) => return self.fail(format!("requirement id unavailable: {error}")),
        };

        let tools = self.state.current_tool_set().tools().to_vec();
        let request = build_chat_request(&self.state, tools, self.mode.request_stream_flag());

        let cursor =
            LoopCursor::streaming_step(step_id, Some(CursorRequirement::root(requirement_id)));
        if let Err(error) = self.state.transition_cursor(cursor) {
            return self.fail(format!("cursor transition failed: {error}"));
        }

        let requirement = Requirement::at_root(
            requirement_id,
            RequirementKind::NeedLlm {
                request,
                mode: self.mode,
            },
        );
        StepOutcome::new(notifications, vec![requirement], true)
    }

    /// Feeds a fulfilled requirement result back into the in-flight turn.
    ///
    /// The cursor selects the return path: an outstanding LLM step folds a
    /// [`Response`], while a tool batch or a pending approval route into the tool
    /// phase (see [`tools`]).
    fn resume(&mut self, resolution: RequirementResolution) -> StepOutcome {
        match self.state.loop_cursor() {
            LoopCursor::StreamingStep(cursor) => {
                let step_id = cursor.step_id();
                let expected = cursor.requirement_id();
                self.resume_llm(step_id, expected, resolution)
            }
            LoopCursor::AwaitingTool(_) => self.resume_tool(resolution),
            LoopCursor::AwaitingApproval(cursor) => {
                let expected = cursor.requirement_id();
                self.resume_approval(expected, resolution)
            }
            other => {
                let kind = other.kind();
                self.fail(format!(
                    "resume received while cursor is `{kind:?}`, no outstanding requirement"
                ))
            }
        }
    }

    /// Feeds a fulfilled `NeedLlm` result back into the in-flight LLM step.
    fn resume_llm(
        &mut self,
        step_id: StepId,
        expected_id: Option<RequirementId>,
        resolution: RequirementResolution,
    ) -> StepOutcome {
        if let Some(expected) = expected_id
            && resolution.id != expected
        {
            return self.fail(format!(
                "resume targets requirement {}, but the machine awaits {expected}",
                resolution.id
            ));
        }

        match resolution.result {
            RequirementResult::Llm(Ok(response)) => self.fold_llm_response(step_id, response),
            RequirementResult::Llm(Err(error)) => {
                self.fail(format!("client operation failed: {error}"))
            }
            other => self.fail(format!(
                "NeedLlm requirement cannot accept a `{}` result",
                other.tag()
            )),
        }
    }

    /// Folds a complete assistant response into the pending turn.
    ///
    /// A tool-free response commits the turn; a tool-use response opens the tool
    /// phase (M2-4) rather than being rejected.
    fn fold_llm_response(&mut self, step_id: StepId, response: Response) -> StepOutcome {
        let Some(assistant_message_id) =
            self.in_flight.as_ref().map(InFlight::assistant_message_id)
        else {
            return self.fail("missing in-flight assistant message id for the LLM response");
        };

        if let Err(error) = self
            .state
            .conversation_mut()
            .start_assistant_response(response)
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        let finish = match self
            .state
            .conversation_mut()
            .finish_assistant(assistant_message_id)
        {
            Ok(finish) => finish,
            Err(error) => return self.fail(format!("conversation operation failed: {error}")),
        };

        match finish {
            AssistantFinish::ReadyToCommit => self.commit_text_turn(step_id),
            AssistantFinish::RequiresToolCallMappings => self.begin_tool_phase(step_id),
        }
    }

    /// Commits a tool-free turn and emits its step-boundary notification.
    fn commit_text_turn(&mut self, step_id: StepId) -> StepOutcome {
        if let Err(error) = self
            .state
            .conversation_mut()
            .commit_pending(TurnMeta::default())
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        let boundary = self.state.conversation().head();

        if let Err(error) = self
            .state
            .transition_cursor(LoopCursor::done(LoopDoneReason::Completed))
        {
            return self.fail(format!("cursor transition failed: {error}"));
        }

        self.in_flight = None;

        let notification = Notification::StepBoundary(StepBoundary::new(step_id, boundary, None));
        StepOutcome::new(vec![notification], Vec::new(), true)
    }

    /// Abandons the outstanding requirement `id` on the never-resume path.
    ///
    /// cancel is not a distinct mechanism (migration doc §7): it is a
    /// never-resume handler. The machine never folds a fabricated result back.
    /// Instead it closes the in-flight turn's single Conversation pending
    /// transaction via
    /// [`Conversation::cancel_pending`](crate::conversation::Conversation::cancel_pending),
    /// parks briefly on [`LoopCursor::CancelRecovery`], and settles back to
    /// [`LoopCursor::Idle`] so a fresh
    /// [`AgentInput::UserMessage`](crate::agent::AgentInput::UserMessage) can open
    /// the next turn. The disposition is chosen by the parked cursor:
    ///
    /// - [`LoopCursor::StreamingStep`] (only an LLM step is outstanding) →
    ///   [`CancelDisposition::DiscardTurn`], reason
    ///   [`CancelRecoveryReason::LlmInterrupted`].
    /// - [`LoopCursor::AwaitingTool`] / [`LoopCursor::AwaitingApproval`] (a tool
    ///   batch or approval is outstanding) →
    ///   [`CancelDisposition::ResumeTurn`] carrying a synthesized `Cancelled`
    ///   result for every still-open call, reason
    ///   [`CancelRecoveryReason::ToolInterrupted`].
    fn abandon(&mut self, id: RequirementId) -> StepOutcome {
        let cursor = self.state.loop_cursor();
        let outstanding = cursor.pending_requirement_ids();
        let plan = match cursor {
            LoopCursor::StreamingStep(cursor) => Some((AbandonKind::Llm, cursor.step_id())),
            LoopCursor::AwaitingTool(cursor) => Some((AbandonKind::Tool, cursor.step_id())),
            LoopCursor::AwaitingApproval(cursor) => Some((AbandonKind::Tool, cursor.step_id())),
            _ => None,
        };

        let Some((kind, step_id)) = plan else {
            let cursor_kind = self.state.loop_cursor().kind();
            return self.fail(format!(
                "abandon received while cursor is `{cursor_kind:?}`, no outstanding requirement"
            ));
        };

        if !outstanding.contains(&id) {
            return self.fail(format!(
                "abandon targets requirement {id}, which is not outstanding this step"
            ));
        }

        match kind {
            AbandonKind::Llm => self.abandon_llm_step(step_id),
            AbandonKind::Tool => self.abandon_tool_phase(step_id),
        }
    }

    /// Never-resume close for an outstanding LLM step: discard the pending turn
    /// wholesale, since no tool_use has been committed for it yet.
    fn abandon_llm_step(&mut self, step_id: StepId) -> StepOutcome {
        if self.state.conversation().pending().is_some()
            && let Err(error) = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn)
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }
        self.finish_cancel(step_id, CancelRecoveryReason::LlmInterrupted)
    }

    /// Shared cancel wrap-up: drop the in-flight scratch and step the cursor
    /// through the transient [`LoopCursor::CancelRecovery`] to a feedable
    /// [`LoopCursor::Idle`] rest state, returning a quiescent outcome.
    fn finish_cancel(&mut self, step_id: StepId, reason: CancelRecoveryReason) -> StepOutcome {
        self.in_flight = None;
        if let Err(error) = self
            .state
            .transition_cursor(LoopCursor::cancel_recovery(Some(step_id), reason))
        {
            return self.fail(format!("cursor transition failed: {error}"));
        }
        if let Err(error) = self.state.transition_cursor(LoopCursor::Idle) {
            return self.fail(format!("cursor transition failed: {error}"));
        }
        StepOutcome::new(Vec::new(), Vec::new(), true)
    }

    /// Discards any dangling pending turn and parks the machine on a classified
    /// error cursor. `step` cannot return `Result`, so runtime failures during a
    /// step surface as an [`LoopCursor::Error`] with a quiescent outcome.
    fn fail(&mut self, message: impl Into<String>) -> StepOutcome {
        self.fail_with_notifications(Vec::new(), message)
    }

    /// Like [`fail`](Self::fail) but preserves notifications produced earlier in
    /// the same step (for example a tool step boundary emitted before a step
    /// limit was hit).
    fn fail_with_notifications(
        &mut self,
        notifications: Vec<Notification>,
        message: impl Into<String>,
    ) -> StepOutcome {
        if self.state.conversation().pending().is_some() {
            let _ = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn);
        }
        self.in_flight = None;
        if let Ok(cursor) = LoopCursor::error(message) {
            let _ = self.state.transition_cursor(cursor);
        }
        StepOutcome::new(notifications, Vec::new(), true)
    }
}

impl AgentMachine for DefaultAgentMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        match input {
            StepInput::External(AgentInput::UserMessage(user)) => self.begin_user_turn(user),
            StepInput::External(AgentInput::Pivot(_)) => {
                self.fail("pivot injection is implemented in M4")
            }
            // Legacy queued-pivot and opaque cursor-resume inputs are not part of
            // the sans-io contract; a driver feeds `StepInput::Resume` instead.
            StepInput::External(_) => self.fail(
                "legacy queued-pivot and cursor-resume inputs are not supported by the \
                 sans-io machine; feed StepInput::Resume with a requirement result",
            ),
            StepInput::Resume(resolution) => self.resume(resolution),
            StepInput::Abandon(id) => self.abandon(id),
        }
    }

    fn cursor(&self) -> &LoopCursor {
        self.state.loop_cursor()
    }
}

#[cfg(test)]
mod tests;
