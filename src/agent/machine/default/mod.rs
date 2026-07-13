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
//! injection lands in **M4-2**: `step(External(AgentInput::Pivot(..)))` appends a
//! `Role::User` message at a checked step boundary and re-renders the
//! outstanding LLM request so the pivot reaches the model (migration doc §2.2),
//! replacing the removed pivot queue.
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
        AgentError, AgentInput, AgentMachine, AgentState, AgentUserInput, CancelRecoveryReason,
        CursorRequirement, DeclaredOnlyToolRegistryResolver, LlmStepMode, LoopCursor,
        LoopDoneReason, NoApprovalPolicy, NoToolExecutionIds, Notification, PivotMessage,
        ReconfigRequest, Requirement, RequirementId, RequirementIds, RequirementKind,
        RequirementKindTag, RequirementResolution, RequirementResult, StepBoundary, StepId,
        StepInput, StepOutcome, ToolApprovalPolicy, ToolExecutionIds, ToolRegistryResolver,
        ToolRuntimeError,
        request::build_chat_request,
        state::{ReconfigApplication, reconfig_boundary_metadata, reconfig_boundary_records},
    },
    client::Response,
    conversation::{AssistantFinish, CancelDisposition, TurnMeta},
};
use serde_json::{Map, Value};
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
    /// A queued reconfiguration is parked on a registry requirement — discard any
    /// pending turn and drop the deferred reconfiguration scratch.
    Reconfig,
}

/// Deferred turn-boundary reconfiguration parked on a `NeedReconfigRegistry`
/// requirement while the driver resolves a live registry for the new tool set.
///
/// Like [`InFlight`], this is mid-turn scratch: it lives only while the machine
/// is parked on [`LoopCursor::AwaitingReconfig`] and is intentionally *not* part
/// of the serializable [`AgentState`]. A cross-process restore of a machine
/// parked here re-drives the boundary from the persisted queue rather than this
/// scratch.
#[derive(Debug)]
enum PendingReconfig {
    /// A reconfiguration queued while idle, applied before a fresh user turn
    /// opens. No step boundary is written for a start-of-turn application.
    BeginTurn {
        /// The user turn to open once the registry is resolved.
        user: AgentUserInput,
        /// The planned application to fold into state after resolution.
        application: ReconfigApplication,
    },
    /// A reconfiguration queued during a turn, applied at the committing text
    /// boundary. The boundary records the applied reconfiguration in metadata.
    Commit {
        /// The committing assistant step whose boundary carries the metadata.
        step_id: StepId,
        /// The planned application to fold into state after resolution.
        application: ReconfigApplication,
        /// Pre-rendered "applied" records for the step-boundary metadata.
        records: Vec<Value>,
    },
}

impl PendingReconfig {
    /// Returns the planned application awaiting a resolved registry.
    fn application(&self) -> &ReconfigApplication {
        match self {
            Self::BeginTurn { application, .. } | Self::Commit { application, .. } => application,
        }
    }

    /// Returns the committing step, if this is a during-turn reconfiguration.
    const fn step_id(&self) -> Option<StepId> {
        match self {
            Self::BeginTurn { .. } => None,
            Self::Commit { step_id, .. } => Some(*step_id),
        }
    }
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
    /// Host-supplied resolver used only by the host-facing
    /// [`reconfigure`](Self::reconfigure) entry to validate a queued tool-set
    /// change (resolve the requested set and confirm its declarations) before
    /// admitting it to the queue. The apply-time registry swap is reified as a
    /// [`RequirementKind::NeedReconfigRegistry`] effect, so the machine itself
    /// never holds a live registry. Defaults to
    /// [`DeclaredOnlyToolRegistryResolver`].
    tool_registry_resolver: Arc<dyn ToolRegistryResolver>,
    /// Scratch state for the turn currently in flight: the current step's
    /// assistant message id, the count of LLM steps started this turn (for the
    /// step limit), and the active tool phase, if any. This mirrors the legacy
    /// segment's stack locals — it lives only while a turn is unfinished and is
    /// therefore not part of the serializable [`AgentState`]. The cursor still
    /// records *which* requirement the machine is stuck on.
    in_flight: Option<InFlight>,
    /// Deferred turn-boundary reconfiguration parked on a registry requirement.
    /// Like [`in_flight`](Self::in_flight) this is non-serialized mid-turn
    /// scratch; it is `Some` only while the cursor is
    /// [`LoopCursor::AwaitingReconfig`].
    pending_reconfig: Option<PendingReconfig>,
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
            tool_registry_resolver: Arc::new(DeclaredOnlyToolRegistryResolver),
            in_flight: None,
            pending_reconfig: None,
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

    /// Sets the resolver used by [`reconfigure`](Self::reconfigure) to validate a
    /// queued tool-set change before it is admitted to the queue.
    #[must_use]
    pub fn with_tool_registry_resolver(
        mut self,
        tool_registry_resolver: Arc<dyn ToolRegistryResolver>,
    ) -> Self {
        self.tool_registry_resolver = tool_registry_resolver;
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

    /// Queues a turn-boundary reconfiguration, validating it eagerly.
    ///
    /// This is a host-facing entry (not part of the sans-io
    /// [`step`](AgentMachine::step)): it mirrors the legacy loop's
    /// `queue_reconfig`. The request is first planned against current state
    /// (skill/overlay/tool-name checks), then — when it changes the active tool
    /// set — the [`ToolRegistryResolver`] is consulted to confirm the requested
    /// set resolves to a registry whose declarations match. Only a
    /// fully-validated request is admitted to the queue, so a conflicting or
    /// unresolvable reconfiguration is rejected here and leaves the queue
    /// unchanged. The apply-time registry swap is deferred to the
    /// [`RequirementKind::NeedReconfigRegistry`] effect emitted at the next turn
    /// boundary.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::State`] when the request conflicts with current
    /// state (for example a duplicate skill or a stale overlay version) and
    /// [`AgentError::Tool`] when the new tool set cannot be resolved or its
    /// registry declarations do not match the requested set.
    pub fn reconfigure(&mut self, request: ReconfigRequest) -> Result<(), AgentError> {
        let application = self.state.plan_reconfig_with(&request)?;
        self.validate_reconfig_registry(&application)?;
        self.state.queue_prevalidated_reconfig(request);
        Ok(())
    }

    /// Confirms a queued tool-set change resolves to a matching registry.
    ///
    /// A reconfiguration that does not change the active tool set needs no
    /// registry, so it validates trivially. Otherwise the resolver must produce
    /// a registry whose declarations equal the requested set (the same
    /// queue-time check the legacy loop performs), else the change is rejected.
    fn validate_reconfig_registry(
        &self,
        application: &ReconfigApplication,
    ) -> Result<(), AgentError> {
        if application.current_tool_set() == self.state.current_tool_set() {
            return Ok(());
        }
        let registry = self
            .tool_registry_resolver
            .resolve_tool_set(application.current_tool_set())?;
        if registry.declarations() != application.current_tool_set().tools() {
            return Err(AgentError::Tool(ToolRuntimeError::InvalidRegistry {
                message: format!(
                    "registry declarations for tool set {} do not match requested ToolSetRef",
                    application.current_tool_set().id()
                ),
            }));
        }
        Ok(())
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

        // Apply (or defer) any queued reconfiguration at the turn boundary before
        // the turn opens. A start-of-turn application writes no step-boundary
        // metadata (mirroring the legacy `apply_queued_reconfigs_before_turn`).
        // A tool-set change parks on a registry effect; the turn opens on resume.
        let application = match self.state.queued_reconfig_application() {
            Ok(application) => application,
            Err(error) => return self.fail(format!("agent state operation failed: {error}")),
        };
        match application {
            None => self.open_user_turn(user),
            Some(application)
                if application.current_tool_set() == self.state.current_tool_set() =>
            {
                self.state.apply_reconfig_application(application);
                self.open_user_turn(user)
            }
            Some(application) => {
                self.emit_reconfig_effect(PendingReconfig::BeginTurn { user, application })
            }
        }
    }

    /// Opens a fresh user turn's Conversation transaction and blocks on the first
    /// `NeedLlm` requirement. Shared by the direct turn open and the resume of a
    /// deferred start-of-turn reconfiguration.
    fn open_user_turn(&mut self, user: AgentUserInput) -> StepOutcome {
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

    /// Injects a `Role::User` pivot at a checked step boundary (migration doc §2.2).
    ///
    /// A pivot is a *soft turn*: the driver feeds an extra `Role::User` message
    /// between two steps instead of queueing it. Injection is only legal while
    /// the machine is parked on a [`LoopCursor::StreamingStep`] whose pending
    /// turn has closed a tool-result batch and is awaiting the next assistant —
    /// exactly the boundary
    /// [`Conversation::inject_user_message`](crate::conversation::Conversation::inject_user_message)
    /// accepts, so the shared role-sequence validation is reused rather than
    /// duplicated. Open tool calls ([`LoopCursor::AwaitingTool`] /
    /// [`LoopCursor::AwaitingApproval`]), a fresh user turn's first LLM step
    /// (whose pending has no closed tool-result step yet), and every terminal or
    /// idle cursor reject the pivot, so it never breaks an in-flight tool phase.
    ///
    /// After appending the pivot, the outstanding LLM request is re-rendered
    /// from the updated pending turn (so the pivot reaches the model on the next
    /// fulfillment) and re-emitted under the *same* requirement id. This is the
    /// same LLM step, so the cursor does not move.
    fn inject_pivot(&mut self, pivot: PivotMessage) -> StepOutcome {
        let LoopCursor::StreamingStep(cursor) = self.state.loop_cursor() else {
            let kind = self.state.loop_cursor().kind();
            return self.fail(format!(
                "pivot injection requires a streaming step boundary, but cursor is `{kind:?}`"
            ));
        };
        let Some(requirement_id) = cursor.requirement_id() else {
            return self.fail(
                "streaming step has no outstanding LLM requirement to re-render for a pivot",
            );
        };

        // Reject non-user pivot payloads up front (mirrors the queued-pivot role
        // check); the injection entry re-validates the role as a second guard.
        if let Err(error) = pivot.validate() {
            return self.fail(format!("agent state operation failed: {error}"));
        }

        let boundary = self.state.conversation().head();
        if let Err(error) = self.state.conversation_mut().inject_user_message(
            boundary,
            pivot.message_id(),
            pivot.message().clone(),
            pivot.message_meta(),
        ) {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        // Re-render the outstanding LLM request so the pivot is part of the next
        // generation. Same step id and requirement id, re-rendered request, so
        // the cursor stays on the current `StreamingStep`.
        let tools = self.state.current_tool_set().tools().to_vec();
        let request = build_chat_request(&self.state, tools, self.mode.request_stream_flag());
        let requirement = Requirement::at_root(
            requirement_id,
            RequirementKind::NeedLlm {
                request,
                mode: self.mode,
            },
        );
        StepOutcome::new(Vec::new(), vec![requirement], true)
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
            LoopCursor::AwaitingReconfig(cursor) => {
                let expected = cursor.requirement_id();
                self.resume_reconfig(expected, resolution)
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

    /// Commits a tool-free turn, applying any queued reconfiguration at the
    /// boundary and emitting its step-boundary notification.
    ///
    /// When a queued reconfiguration changes the active tool set, the commit is
    /// deferred: the machine parks on [`LoopCursor::AwaitingReconfig`] and emits
    /// a [`RequirementKind::NeedReconfigRegistry`] effect so the driver resolves
    /// and swaps in the new registry *before* the turn is committed (mirroring
    /// the legacy loop resolving the registry before `commit_pending`). A
    /// reconfiguration that leaves the tool set unchanged, or no queued
    /// reconfiguration at all, commits immediately.
    fn commit_text_turn(&mut self, step_id: StepId) -> StepOutcome {
        let application = match self.state.queued_reconfig_application() {
            Ok(application) => application,
            Err(error) => return self.fail(format!("agent state operation failed: {error}")),
        };
        match application {
            None => self.finalize_text_commit(step_id, None),
            Some(application)
                if application.current_tool_set() == self.state.current_tool_set() =>
            {
                let records = reconfig_boundary_records(application.requests());
                self.finalize_text_commit(step_id, Some((application, records)))
            }
            Some(application) => {
                let records = reconfig_boundary_records(application.requests());
                self.emit_reconfig_effect(PendingReconfig::Commit {
                    step_id,
                    application,
                    records,
                })
            }
        }
    }

    /// Commits the pending turn, folds an optional reconfiguration into state,
    /// and emits the terminal step boundary (with reconfig metadata if applied).
    fn finalize_text_commit(
        &mut self,
        step_id: StepId,
        reconfig: Option<(ReconfigApplication, Vec<Value>)>,
    ) -> StepOutcome {
        if let Err(error) = self
            .state
            .conversation_mut()
            .commit_pending(TurnMeta::default())
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        let boundary = self.state.conversation().head();
        let metadata = match &reconfig {
            Some((_, records)) => reconfig_boundary_metadata(records.clone()),
            None => Map::new(),
        };
        if let Some((application, _)) = reconfig {
            self.state.apply_reconfig_application(application);
        }

        if let Err(error) = self
            .state
            .transition_cursor(LoopCursor::done(LoopDoneReason::Completed))
        {
            return self.fail(format!("cursor transition failed: {error}"));
        }

        self.in_flight = None;
        self.pending_reconfig = None;

        let notification = Notification::StepBoundary(StepBoundary::with_metadata(
            step_id, boundary, None, metadata,
        ));
        StepOutcome::new(vec![notification], Vec::new(), true)
    }

    /// Emits a `NeedReconfigRegistry` requirement and parks on
    /// [`LoopCursor::AwaitingReconfig`], stashing the deferred application.
    ///
    /// The requested tool set is taken from the planned application; the driver
    /// resolves it to a live registry, validates its declarations, swaps it in,
    /// and confirms with a [`RequirementResult::Reconfig`]. The pending
    /// application is folded into state on resume.
    fn emit_reconfig_effect(&mut self, pending: PendingReconfig) -> StepOutcome {
        let requirement_id = match self
            .requirement_ids
            .next_requirement_id(RequirementKindTag::Reconfig)
        {
            Ok(id) => id,
            Err(error) => return self.fail(format!("requirement id unavailable: {error}")),
        };

        let tool_set = pending.application().current_tool_set().clone();
        let step_id = pending.step_id();
        self.pending_reconfig = Some(pending);

        let cursor =
            LoopCursor::awaiting_reconfig(step_id, Some(CursorRequirement::root(requirement_id)));
        if let Err(error) = self.state.transition_cursor(cursor) {
            return self.fail(format!("cursor transition failed: {error}"));
        }

        let requirement = Requirement::at_root(
            requirement_id,
            RequirementKind::NeedReconfigRegistry { tool_set },
        );
        StepOutcome::new(Vec::new(), vec![requirement], true)
    }

    /// Feeds a fulfilled `NeedReconfigRegistry` result back into the parked
    /// turn boundary.
    ///
    /// A confirming `Ok` [`RequirementResult::Reconfig`] applies the deferred
    /// application and either opens the pending user turn (start-of-turn
    /// reconfiguration) or commits the deferred text turn (during-turn
    /// reconfiguration). A driver-reported registry error fails the boundary.
    fn resume_reconfig(
        &mut self,
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
            RequirementResult::Reconfig(Ok(())) => {}
            RequirementResult::Reconfig(Err(error)) => {
                return self.fail(format!("tool runtime operation failed: {error}"));
            }
            other => {
                return self.fail(format!(
                    "NeedReconfigRegistry requirement cannot accept a `{}` result",
                    other.tag()
                ));
            }
        }

        let Some(pending) = self.pending_reconfig.take() else {
            return self.fail("reconfig resume with no deferred reconfiguration in flight");
        };

        match pending {
            PendingReconfig::BeginTurn { user, application } => {
                self.state.apply_reconfig_application(application);
                self.open_user_turn(user)
            }
            PendingReconfig::Commit {
                step_id,
                application,
                records,
            } => self.finalize_text_commit(step_id, Some((application, records))),
        }
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
        let plan: Option<(AbandonKind, Option<StepId>)> = match cursor {
            LoopCursor::StreamingStep(cursor) => Some((AbandonKind::Llm, Some(cursor.step_id()))),
            LoopCursor::AwaitingTool(cursor) => Some((AbandonKind::Tool, Some(cursor.step_id()))),
            LoopCursor::AwaitingApproval(cursor) => {
                Some((AbandonKind::Tool, Some(cursor.step_id())))
            }
            LoopCursor::AwaitingReconfig(cursor) => Some((AbandonKind::Reconfig, cursor.step_id())),
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
            AbandonKind::Reconfig => self.abandon_reconfig(step_id),
        }
    }

    /// Never-resume close for an outstanding LLM step: discard the pending turn
    /// wholesale, since no tool_use has been committed for it yet.
    fn abandon_llm_step(&mut self, step_id: Option<StepId>) -> StepOutcome {
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

    /// Never-resume close for a deferred reconfiguration parked on a registry
    /// requirement: drop the pending turn (if any) and the deferred
    /// reconfiguration scratch, then settle back to a feedable `Idle`.
    ///
    /// A start-of-turn reconfiguration has not opened a turn yet (no pending),
    /// while a during-turn reconfiguration folded but did not commit its text
    /// turn — both are closed by discarding any pending transaction.
    fn abandon_reconfig(&mut self, step_id: Option<StepId>) -> StepOutcome {
        if self.state.conversation().pending().is_some()
            && let Err(error) = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn)
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }
        self.pending_reconfig = None;
        self.finish_cancel(step_id, CancelRecoveryReason::Cancelled)
    }

    /// Shared cancel wrap-up: drop the in-flight scratch and step the cursor
    /// through the transient [`LoopCursor::CancelRecovery`] to a feedable
    /// [`LoopCursor::Idle`] rest state, returning a quiescent outcome.
    fn finish_cancel(
        &mut self,
        step_id: Option<StepId>,
        reason: CancelRecoveryReason,
    ) -> StepOutcome {
        self.in_flight = None;
        self.pending_reconfig = None;
        if let Err(error) = self
            .state
            .transition_cursor(LoopCursor::cancel_recovery(step_id, reason))
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
            StepInput::External(AgentInput::Pivot(pivot)) => self.inject_pivot(pivot),
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
