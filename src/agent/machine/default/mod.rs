//! Concrete sans-io Agent machine for the LLM and tool steps.
//!
//! [`DefaultAgentMachine`] is the effect-model Agent runtime: instead of
//! awaiting the client, tools, or approvals internally, it *requests* each
//! effect by handing back a [`Requirement`] and parking on the matching
//! [`LoopCursor`]. A driver fulfils the requirement and feeds the result back
//! through [`StepInput::Resume`], at which point the machine folds it into the
//! single active Conversation using the checked pending boundary
//! (`start_assistant_response` → `finish_assistant` →
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

mod error;
mod tools;

use error::StepError;

use crate::{
    agent::{
        AgentError, AgentInput, AgentMachine, AgentPath, AgentState, AgentUserInput,
        CancelRecoveryReason, CursorRequirement, DeclaredOnlyToolRegistryResolver, LlmStepMode,
        LoopCursor, LoopDoneReason, NoApprovalPolicy, NoToolExecutionIds, Notification,
        PivotMessage, ReconfigRequest, Requirement, RequirementId, RequirementIds, RequirementKind,
        RequirementKindTag, RequirementResolution, RequirementResult, StepBoundary, StepId,
        StepInput, StepOutcome, StepRejectReason, ToolApprovalPolicy, ToolExecutionIds,
        ToolRegistryResolver, ToolRuntimeError,
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
    /// A queued reconfiguration is parked on a registry requirement — commit any
    /// folded-but-uncommitted text turn and drop the deferred reconfiguration
    /// scratch.
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

/// Single mid-turn scratch whose phase is isomorphic to the [`LoopCursor`]
/// phase, replacing the two free-standing non-serialized `Option`s
/// (`in_flight` + `pending_reconfig`) that were previously kept aligned with the
/// cursor by implicit convention (effect-refine doc §3, 落点 2).
///
/// Like its [`InFlight`] and [`PendingReconfig`] payloads, this is intentionally
/// *not* part of the serializable [`AgentState`]: a cross-process restore of a
/// machine parked mid-turn re-drives the scratch from the persisted Conversation
/// pending transaction and reconfiguration queue rather than deserializing it.
#[derive(Debug)]
enum TurnScratch {
    /// No turn is in flight: the cursor rests in [`LoopCursor::Idle`],
    /// [`LoopCursor::Done`], or [`LoopCursor::Error`] (or the transient
    /// [`LoopCursor::CancelRecovery`]).
    None,
    /// A turn is in flight: the cursor is on [`LoopCursor::StreamingStep`],
    /// [`LoopCursor::AwaitingTool`], or [`LoopCursor::AwaitingApproval`].
    InTurn(InFlight),
    /// A turn-boundary reconfiguration is parked on a registry requirement: the
    /// cursor is on [`LoopCursor::AwaitingReconfig`].
    Reconfig(PendingReconfig),
}

impl TurnScratch {
    /// Reports whether this scratch's phase matches `cursor`'s phase, i.e. the
    /// "cursor and scratch stay aligned" invariant that the single-enum shape
    /// guarantees. Wired into `debug_assert!`s on the `resume` / `abandon`
    /// dispatch and the pivot injection path so the alignment the type enforces
    /// is also checked at runtime in debug builds (effect-refine doc §3.1/§3.2).
    fn matches_cursor(&self, cursor: &LoopCursor) -> bool {
        match self {
            Self::None => !matches!(
                cursor,
                LoopCursor::StreamingStep(_)
                    | LoopCursor::AwaitingTool(_)
                    | LoopCursor::AwaitingApproval(_)
                    | LoopCursor::AwaitingReconfig(_)
            ),
            Self::InTurn(_) => matches!(
                cursor,
                LoopCursor::StreamingStep(_)
                    | LoopCursor::AwaitingTool(_)
                    | LoopCursor::AwaitingApproval(_)
            ),
            Self::Reconfig(_) => matches!(cursor, LoopCursor::AwaitingReconfig(_)),
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
    /// Single mid-turn scratch whose phase is isomorphic to the [`LoopCursor`]
    /// phase (effect-refine doc §3, 落点 2). It carries the current turn's
    /// [`InFlight`] state (assistant message id, LLM step count, active tool
    /// phase) while a turn runs, and the deferred [`PendingReconfig`] while a
    /// turn-boundary reconfiguration is parked, replacing the two free-standing
    /// `Option`s that were previously aligned with the cursor only by implicit
    /// convention. Like its payloads this mirrors the legacy segment's stack
    /// locals: it lives only while a turn is unfinished and is therefore not
    /// part of the serializable [`AgentState`]. The cursor still records *which*
    /// requirement the machine is stuck on.
    scratch: TurnScratch,
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
            scratch: TurnScratch::None,
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

    /// Re-stamps this machine's cursor requirement binding to the absolute
    /// [`AgentPath`] `base`.
    ///
    /// A standalone machine always stamps its cursor at the root; a nested
    /// machine that places this node at `base` calls this so the persisted
    /// cursor records the node's real path in the tree (migration doc §7.1).
    pub(crate) fn rebase_cursor_origin(&mut self, base: &AgentPath) {
        self.state.rebase_cursor_origin(base);
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
    /// state (for example a duplicate skill or a stale overlay version), when
    /// the cursor is [`LoopCursor::AwaitingReconfig`] — a request admitted
    /// during the park would be silently dropped by the resume's queue clear,
    /// so it is rejected up front and may be retried once the outstanding
    /// reconfig requirement resolves (H-STATE-5 / M4-2) — and
    /// [`AgentError::Tool`] when the new tool set cannot be resolved or its
    /// registry declarations do not match the requested set.
    pub fn reconfigure(&mut self, request: ReconfigRequest) -> Result<(), AgentError> {
        self.state.ensure_reconfig_admission()?;
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

    /// Returns the in-flight turn scratch, or `None` when no turn is running.
    ///
    /// The [`InFlight`] payload lives only in the [`TurnScratch::InTurn`] phase,
    /// so this is the single read path replacing the former `self.in_flight`
    /// field access.
    fn in_flight(&self) -> Option<&InFlight> {
        match &self.scratch {
            TurnScratch::InTurn(in_flight) => Some(in_flight),
            TurnScratch::None | TurnScratch::Reconfig(_) => None,
        }
    }

    /// Returns a mutable view of the in-flight turn scratch, or `None` when no
    /// turn is running.
    fn in_flight_mut(&mut self) -> Option<&mut InFlight> {
        match &mut self.scratch {
            TurnScratch::InTurn(in_flight) => Some(in_flight),
            TurnScratch::None | TurnScratch::Reconfig(_) => None,
        }
    }

    /// Takes the deferred reconfiguration parked on a registry requirement,
    /// resetting the scratch to [`TurnScratch::None`].
    ///
    /// Returns `None` (leaving the scratch untouched) when no reconfiguration is
    /// parked, replacing the former `self.pending_reconfig.take()`.
    fn take_pending_reconfig(&mut self) -> Option<PendingReconfig> {
        match std::mem::replace(&mut self.scratch, TurnScratch::None) {
            TurnScratch::Reconfig(pending) => Some(pending),
            other => {
                self.scratch = other;
                None
            }
        }
    }

    /// Reconstructs the mid-turn [`TurnScratch`] from the durable
    /// [`AgentState`] so it is aligned to the current [`LoopCursor`] phase.
    ///
    /// The scratch (`InFlight` / `PendingReconfig`) is intentionally *not*
    /// serialized (落点 2): a machine parked mid-turn re-derives it from the
    /// persistent [`Conversation`](crate::conversation::Conversation) pending
    /// transaction and the reconfiguration queue rather than from a deserialized
    /// scratch (effect-refine doc §3.4). This makes that re-derivation an
    /// explicit, testable operation instead of the implicit "cursor and scratch
    /// stay aligned" convention: after it runs, [`TurnScratch::matches_cursor`]
    /// holds for every phase whose scratch is derivable from committed facts.
    ///
    /// Reconstruction is phase-directed:
    ///
    /// - [`StreamingStep`](LoopCursor::StreamingStep) /
    ///   [`AwaitingTool`](LoopCursor::AwaitingTool) /
    ///   [`AwaitingApproval`](LoopCursor::AwaitingApproval): rebuild an
    ///   [`InFlight`] anchored on the pending turn's frozen assistant messages
    ///   via [`InFlight::rebuild_from_pending`]. A `StreamingStep` awaits a fresh
    ///   (still un-frozen) assistant, so `awaiting_unfrozen_assistant` is `true`
    ///   there and `false` for the tool/approval parks whose step is already
    ///   frozen.
    /// - [`AwaitingReconfig`](LoopCursor::AwaitingReconfig): replan the deferred
    ///   application from the persisted [`queued_reconfigs`](AgentState::queued_reconfigs)
    ///   and rebuild the [`PendingReconfig`]. The cursor's committing `step_id`
    ///   distinguishes a during-turn [`PendingReconfig::Commit`] (whose
    ///   `records` are re-rendered from the application, so they need no
    ///   persistence) from a start-of-turn `BeginTurn`.
    /// - [`Idle`](LoopCursor::Idle) /
    ///   [`CancelRecovery`](LoopCursor::CancelRecovery) /
    ///   [`Done`](LoopCursor::Done) / [`Error`](LoopCursor::Error): no turn is in
    ///   flight, so the scratch is [`TurnScratch::None`].
    ///
    /// # Limitations under 落点 2
    ///
    /// The active [`ToolPhase`](tools) detail is not reconstructed (`tools:
    /// None`; see [`InFlight::rebuild_from_pending`]), so a rebuilt in-tool park
    /// is a faithful phase marker rather than a resumable batch. Two parks
    /// additionally depend on host-supplied inputs that are not persisted and so
    /// are left as [`TurnScratch::None`] for the driver to re-establish: a
    /// `StreamingStep` on its very first LLM step (no frozen assistant to anchor,
    /// and the outstanding assistant's id is not yet minted) and a start-of-turn
    /// reconfiguration whose queued [`AgentUserInput`] is not persisted (its
    /// cursor carries no committing `step_id`). The fully round-tripping restore
    /// is therefore the during-turn `AwaitingReconfig` boundary.
    fn rebuild_scratch_from_state(&mut self) -> Result<(), StepError> {
        self.scratch = match self.state.loop_cursor() {
            LoopCursor::StreamingStep(_) => self.rebuild_in_flight_scratch(true),
            LoopCursor::AwaitingTool(_) | LoopCursor::AwaitingApproval(_) => {
                self.rebuild_in_flight_scratch(false)
            }
            LoopCursor::AwaitingReconfig(cursor) => {
                let step_id = cursor.step_id();
                self.rebuild_reconfig_scratch(step_id)?
            }
            LoopCursor::Idle
            | LoopCursor::CancelRecovery(_)
            | LoopCursor::Done(_)
            | LoopCursor::Error(_) => TurnScratch::None,
        };
        Ok(())
    }

    /// Rebuilds [`TurnScratch::InTurn`] from the pending turn, or
    /// [`TurnScratch::None`] when no frozen assistant anchors it (see
    /// [`InFlight::rebuild_from_pending`] for the anchor and `tools: None`
    /// limitations).
    fn rebuild_in_flight_scratch(&self, awaiting_unfrozen_assistant: bool) -> TurnScratch {
        match self.state.conversation().pending() {
            Some(pending) => InFlight::rebuild_from_pending(pending, awaiting_unfrozen_assistant)
                .map_or(TurnScratch::None, TurnScratch::InTurn),
            None => TurnScratch::None,
        }
    }

    /// Rebuilds a deferred [`PendingReconfig`] from the persisted reconfiguration
    /// queue, or [`TurnScratch::None`] when the queue is empty or the park is a
    /// non-reconstructable start-of-turn reconfiguration (see the
    /// [`rebuild_scratch_from_state`](Self::rebuild_scratch_from_state) doc).
    fn rebuild_reconfig_scratch(&self, step_id: Option<StepId>) -> Result<TurnScratch, StepError> {
        let Some(application) = self.state.queued_reconfig_application()? else {
            return Ok(TurnScratch::None);
        };
        // A during-turn commit records its committing step on the cursor; a
        // start-of-turn `BeginTurn` does not, and its queued user input is not
        // persisted, so only the during-turn `Commit` is reconstructable.
        let Some(step_id) = step_id else {
            return Ok(TurnScratch::None);
        };
        let records = reconfig_boundary_records(application.requests());
        Ok(TurnScratch::Reconfig(PendingReconfig::Commit {
            step_id,
            application,
            records,
        }))
    }

    /// Opens a fresh user turn and blocks on one `NeedLlm` requirement.
    fn begin_user_turn(&mut self, user: AgentUserInput) -> Result<StepOutcome, StepError> {
        // A fresh user message is only feedable at a rest boundary (`Idle`, the
        // terminal `Done`/`Error` a previous turn settled on, or the transient
        // `CancelRecovery` a persisted snapshot may have captured between the
        // never-resume closure and the settle back to `Idle` — the scratch
        // rebuild already maps it to `None`, so it is a rest boundary too,
        // M4-5). Mid-turn —
        // while the machine is parked on an LLM step, a tool batch, an approval,
        // or a reconfig — a second user message would collide with the live
        // pending turn (`Conversation::begin_turn` rejects a second open
        // pending), so it is soft-rejected instead: the turn in progress keeps
        // its state and the driver may retry once the turn settles (or inject a
        // pivot at a legal boundary). Checked before any state is touched.
        if !matches!(
            self.state.loop_cursor(),
            LoopCursor::Idle
                | LoopCursor::Done(_)
                | LoopCursor::Error(_)
                | LoopCursor::CancelRecovery(_)
        ) {
            let kind = self.state.loop_cursor().kind();
            return Err(StepError::Rejected(StepRejectReason::TurnInProgress(
                format!(
                    "a user message arrived while a turn is in progress (cursor `{kind:?}`); \
                     feed it once the turn settles, or inject it as a pivot at a legal boundary"
                ),
            )));
        }

        // Re-derive the mid-turn scratch from the persisted state at this turn
        // boundary so it is aligned to the cursor phase before the next turn
        // opens. This is the explicit stand-in for the former implicit
        // assumption that the scratch was already `None` here (effect-refine doc
        // §3.4): a machine reused across turns rests at `Idle` / `Done` / `Error`
        // (all `TurnScratch::None`), and a machine handed a persisted state
        // rebuilds whatever its cursor implies.
        self.rebuild_scratch_from_state()?;
        debug_assert!(
            self.scratch.matches_cursor(self.state.loop_cursor()),
            "begin_user_turn: turn scratch phase must match the loop cursor phase"
        );

        // A completed or errored turn settles the cursor at a terminal rest state
        // (`Done` / `Error`), and a restored snapshot may have captured the
        // transient `CancelRecovery` marker mid-settle (M4-5). The same machine
        // is reused across turns, so a new
        // user message supersedes that finished turn: reset the cursor to the
        // feedable `Idle` before opening the next one. (A fresh machine already
        // starts at `Idle`, so this is a no-op for the first turn.)
        if matches!(
            self.state.loop_cursor(),
            LoopCursor::Done(_) | LoopCursor::Error(_) | LoopCursor::CancelRecovery(_)
        ) {
            self.state
                .transition_cursor(LoopCursor::Idle)
                .map_err(StepError::CursorTransition)?;
        }

        // A never-resume abandon of a tool batch leaves a *coherent* pending turn
        // (its dangling tool_use closed by synthesized `Cancelled` results) with
        // the cursor settled at `Idle`. This is the same consistency definition
        // `rebuild_scratch_from_state` encodes: an `Idle` cursor carries
        // `TurnScratch::None` (no in-flight assistant), so any leftover pending
        // is a superseded transaction. Discard it before `begin_turn` opens the
        // next one (which rejects a second open pending).
        if matches!(self.state.loop_cursor(), LoopCursor::Idle)
            && self.state.conversation().pending().is_some()
        {
            self.state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn)?;
        }

        // Apply (or defer) any queued reconfiguration at the turn boundary before
        // the turn opens. A start-of-turn application writes no step-boundary
        // metadata (mirroring the legacy `apply_queued_reconfigs_before_turn`).
        // A tool-set change parks on a registry effect; the turn opens on resume.
        let application = self.state.queued_reconfig_application()?;
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
    fn open_user_turn(&mut self, user: AgentUserInput) -> Result<StepOutcome, StepError> {
        self.state.conversation_mut().begin_turn(
            user.turn_id(),
            user.message_id(),
            user.message().clone(),
        )?;

        self.scratch = TurnScratch::InTurn(InFlight::new(user.assistant_message_id()));
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
    fn inject_pivot(&mut self, pivot: PivotMessage) -> Result<StepOutcome, StepError> {
        let LoopCursor::StreamingStep(cursor) = self.state.loop_cursor() else {
            let kind = self.state.loop_cursor().kind();
            return Err(StepError::Rejected(StepRejectReason::IllegalPivotBoundary(
                format!(
                    "pivot injection requires a streaming step boundary, but cursor is `{kind:?}`"
                ),
            )));
        };
        let Some(requirement_id) = cursor.requirement_id() else {
            return Err(StepError::Rejected(StepRejectReason::IllegalPivotBoundary(
                "streaming step has no outstanding LLM requirement to re-render for a pivot"
                    .to_string(),
            )));
        };

        // A `StreamingStep` cursor is isomorphic to the `TurnScratch::InTurn`
        // phase, so the in-flight scratch is guaranteed present here; the pivot
        // path needs no separate "is a turn actually in flight?" guard beyond
        // the cursor match above (effect-refine doc §3.1).
        debug_assert!(
            self.in_flight().is_some(),
            "pivot on a streaming step requires an in-flight turn scratch"
        );

        // Reject non-user pivot payloads up front (mirrors the queued-pivot role
        // check); the injection entry re-validates the role as a second guard.
        // A payload the driver should never have built is a soft rejection: the
        // machine's state is provably untouched.
        pivot.validate().map_err(|error| {
            StepError::Rejected(StepRejectReason::IllegalPivotBoundary(format!(
                "pivot payload rejected: {error}"
            )))
        })?;

        let boundary = self.state.conversation().head();
        // The Conversation rejects an illegal injection boundary (no closed
        // tool-result step yet, an open tool call, or a duplicate message id).
        // That is a caller boundary violation, not an internal failure: reject
        // softly, leaving the in-flight turn exactly as it was.
        self.state
            .conversation_mut()
            .inject_user_message(
                boundary,
                pivot.message_id(),
                pivot.message().clone(),
                pivot.message_meta(),
            )
            .map_err(|error| {
                StepError::Rejected(StepRejectReason::IllegalPivotBoundary(format!(
                    "pivot injection boundary rejected: {error}"
                )))
            })?;

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
        Ok(StepOutcome::new(Vec::new(), vec![requirement], true))
    }

    /// Builds the next generation request and parks on one `NeedLlm` requirement.
    ///
    /// Shared by the opening user turn and by the post-tool continuation: both
    /// allocate an LLM requirement, render the [`ChatRequest`](crate::client::ChatRequest)
    /// from current state, and transition to [`LoopCursor::StreamingStep`]. The
    /// caller supplies any notifications produced earlier in the same step (for
    /// example a tool step boundary) to emit alongside the requirement.
    fn block_on_llm(
        &mut self,
        step_id: StepId,
        notifications: Vec<Notification>,
    ) -> Result<StepOutcome, StepError> {
        let requirement_id = self
            .requirement_ids
            .next_requirement_id(RequirementKindTag::Llm)?;

        let tools = self.state.current_tool_set().tools().to_vec();
        let request = build_chat_request(&self.state, tools, self.mode.request_stream_flag());

        let cursor =
            LoopCursor::streaming_step(step_id, Some(CursorRequirement::root(requirement_id)));
        self.state
            .transition_cursor(cursor)
            .map_err(StepError::CursorTransition)?;

        let requirement = Requirement::at_root(
            requirement_id,
            RequirementKind::NeedLlm {
                request,
                mode: self.mode,
            },
        );
        Ok(StepOutcome::new(notifications, vec![requirement], true))
    }

    /// Feeds a fulfilled requirement result back into the in-flight turn.
    ///
    /// The cursor selects the return path: an outstanding LLM step folds a
    /// [`Response`], while a tool batch or a pending approval route into the tool
    /// phase (see [`tools`]).
    fn resume(&mut self, resolution: RequirementResolution) -> Result<StepOutcome, StepError> {
        // With the single `TurnScratch`, matching the cursor phase *is* reaching
        // the scratch: the two can no longer drift, so the former "re-match the
        // cursor, then separately re-check the scratch" double guard collapses
        // into this one invariant (effect-refine doc §3.2).
        debug_assert!(
            self.scratch.matches_cursor(self.state.loop_cursor()),
            "resume: turn scratch phase must match the loop cursor phase"
        );
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
                Err(StepError::Rejected(StepRejectReason::UnknownRequirement(
                    format!(
                        "resume received while cursor is `{kind:?}`, no outstanding requirement"
                    ),
                )))
            }
        }
    }

    /// Feeds a fulfilled `NeedLlm` result back into the in-flight LLM step.
    fn resume_llm(
        &mut self,
        step_id: StepId,
        expected_id: Option<RequirementId>,
        resolution: RequirementResolution,
    ) -> Result<StepOutcome, StepError> {
        if let Some(expected) = expected_id
            && resolution.id != expected
        {
            return Err(StepError::Rejected(StepRejectReason::UnknownRequirement(
                format!(
                    "resume targets requirement {}, but the machine awaits {expected}",
                    resolution.id
                ),
            )));
        }

        match resolution.result {
            RequirementResult::Llm(Ok(response)) => self.fold_llm_response(step_id, response),
            RequirementResult::Llm(Err(error)) => Err(StepError::Protocol(format!(
                "client operation failed: {error}"
            ))),
            other => Err(StepError::Protocol(format!(
                "NeedLlm requirement cannot accept a `{}` result",
                other.tag()
            ))),
        }
    }

    /// Folds a complete assistant response into the pending turn.
    ///
    /// A tool-free response commits the turn; a tool-use response opens the tool
    /// phase (M2-4) rather than being rejected.
    fn fold_llm_response(
        &mut self,
        step_id: StepId,
        response: Response,
    ) -> Result<StepOutcome, StepError> {
        let Some(assistant_message_id) = self.in_flight().map(InFlight::assistant_message_id)
        else {
            return Err(StepError::Protocol(
                "missing in-flight assistant message id for the LLM response".to_string(),
            ));
        };

        self.state
            .conversation_mut()
            .start_assistant_response(response)?;

        let finish = self
            .state
            .conversation_mut()
            .finish_assistant(assistant_message_id)?;

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
    fn commit_text_turn(&mut self, step_id: StepId) -> Result<StepOutcome, StepError> {
        let application = self.state.queued_reconfig_application()?;
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
    ) -> Result<StepOutcome, StepError> {
        self.state
            .conversation_mut()
            .commit_pending(TurnMeta::default())?;

        let boundary = self.state.conversation().head();
        let metadata = match &reconfig {
            Some((_, records)) => reconfig_boundary_metadata(records.clone()),
            None => Map::new(),
        };
        if let Some((application, _)) = reconfig {
            self.state.apply_reconfig_application(application);
        }

        self.state
            .transition_cursor(LoopCursor::done(LoopDoneReason::Completed))
            .map_err(StepError::CursorTransition)?;

        self.scratch = TurnScratch::None;

        let notification = Notification::StepBoundary(StepBoundary::with_metadata(
            step_id, boundary, None, metadata,
        ));
        Ok(StepOutcome::new(vec![notification], Vec::new(), true))
    }

    /// Emits a `NeedReconfigRegistry` requirement and parks on
    /// [`LoopCursor::AwaitingReconfig`], stashing the deferred application.
    ///
    /// The requested tool set is taken from the planned application; the driver
    /// resolves it to a live registry, validates its declarations, swaps it in,
    /// and confirms with a [`RequirementResult::Reconfig`]. The pending
    /// application is folded into state on resume.
    fn emit_reconfig_effect(&mut self, pending: PendingReconfig) -> Result<StepOutcome, StepError> {
        let requirement_id = self
            .requirement_ids
            .next_requirement_id(RequirementKindTag::Reconfig)?;

        let tool_set = pending.application().current_tool_set().clone();
        let step_id = pending.step_id();
        self.scratch = TurnScratch::Reconfig(pending);

        let cursor =
            LoopCursor::awaiting_reconfig(step_id, Some(CursorRequirement::root(requirement_id)));
        self.state
            .transition_cursor(cursor)
            .map_err(StepError::CursorTransition)?;

        let requirement = Requirement::at_root(
            requirement_id,
            RequirementKind::NeedReconfigRegistry { tool_set },
        );
        Ok(StepOutcome::new(Vec::new(), vec![requirement], true))
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
    ) -> Result<StepOutcome, StepError> {
        if let Some(expected) = expected_id
            && resolution.id != expected
        {
            return Err(StepError::Rejected(StepRejectReason::UnknownRequirement(
                format!(
                    "resume targets requirement {}, but the machine awaits {expected}",
                    resolution.id
                ),
            )));
        }

        match resolution.result {
            RequirementResult::Reconfig(Ok(())) => {}
            RequirementResult::Reconfig(Err(error)) => {
                return Err(StepError::ToolRuntime(error));
            }
            other => {
                return Err(StepError::Protocol(format!(
                    "NeedReconfigRegistry requirement cannot accept a `{}` result",
                    other.tag()
                )));
            }
        }

        let Some(pending) = self.take_pending_reconfig() else {
            return Err(StepError::Protocol(
                "reconfig resume with no deferred reconfiguration in flight".to_string(),
            ));
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
    fn abandon(&mut self, id: RequirementId) -> Result<StepOutcome, StepError> {
        debug_assert!(
            self.scratch.matches_cursor(self.state.loop_cursor()),
            "abandon: turn scratch phase must match the loop cursor phase"
        );
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
            return Err(StepError::Rejected(StepRejectReason::UnknownRequirement(
                format!(
                    "abandon received while cursor is `{cursor_kind:?}`, no outstanding requirement"
                ),
            )));
        };

        if !outstanding.contains(&id) {
            return Err(StepError::Rejected(StepRejectReason::UnknownRequirement(
                format!("abandon targets requirement {id}, which is not outstanding this step"),
            )));
        }

        match kind {
            AbandonKind::Llm => self.abandon_llm_step(step_id),
            AbandonKind::Tool => self.abandon_tool_phase(step_id),
            AbandonKind::Reconfig => self.abandon_reconfig(step_id),
        }
    }

    /// Never-resume close for an outstanding LLM step: discard the pending turn
    /// wholesale, since no tool_use has been committed for it yet.
    fn abandon_llm_step(&mut self, step_id: Option<StepId>) -> Result<StepOutcome, StepError> {
        if self.state.conversation().pending().is_some() {
            self.state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn)?;
        }
        self.finish_cancel(step_id, CancelRecoveryReason::LlmInterrupted)
    }

    /// Never-resume close for a deferred reconfiguration parked on a registry
    /// requirement: preserve the folded-but-uncommitted text turn (if any) and
    /// drop the deferred reconfiguration scratch, then settle back to a feedable
    /// `Idle`.
    ///
    /// A start-of-turn reconfiguration has not opened a turn yet (no pending).
    /// A during-turn reconfiguration parks *after* the final assistant response
    /// froze (`ReadyToCommit`), where [`CancelDisposition::ResumeTurn`] is not a
    /// legal closure (the conversation layer only closes open tool calls there,
    /// never commits), so committing the pending turn is the only closure that
    /// preserves the text — the same "keep the caller's work" alignment the
    /// tool-abandon path gets from `ResumeTurn` (M4-4). The abandoned
    /// reconfiguration stays queued and is retried at the next turn boundary.
    fn abandon_reconfig(&mut self, step_id: Option<StepId>) -> Result<StepOutcome, StepError> {
        if self.state.conversation().pending().is_some() {
            self.state
                .conversation_mut()
                .commit_pending(TurnMeta::default())?;
        }
        self.scratch = TurnScratch::None;
        self.finish_cancel(step_id, CancelRecoveryReason::Cancelled)
    }

    /// Shared cancel wrap-up: drop the in-flight scratch and step the cursor
    /// through the transient [`LoopCursor::CancelRecovery`] to a feedable
    /// [`LoopCursor::Idle`] rest state, returning a quiescent outcome.
    fn finish_cancel(
        &mut self,
        step_id: Option<StepId>,
        reason: CancelRecoveryReason,
    ) -> Result<StepOutcome, StepError> {
        self.scratch = TurnScratch::None;
        self.state
            .transition_cursor(LoopCursor::cancel_recovery(step_id, reason))
            .map_err(StepError::CursorTransition)?;
        self.state
            .transition_cursor(LoopCursor::Idle)
            .map_err(StepError::CursorTransition)?;
        Ok(StepOutcome::new(Vec::new(), Vec::new(), true))
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
    ///
    /// Teardown failures are never swallowed (M4-4): a failed pending-turn
    /// discard is folded into the parked message, and the error-cursor park
    /// itself is total — the transition table accepts `Error` from every cursor
    /// kind and the message is guaranteed non-empty — so the diagnostic can
    /// never be lost the way the former `let _ =` pair allowed.
    fn fail_with_notifications(
        &mut self,
        notifications: Vec<Notification>,
        message: impl Into<String>,
    ) -> StepOutcome {
        let mut message = message.into();
        if self.state.conversation().pending().is_some()
            && let Err(error) = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn)
        {
            // The discard failing means the pending turn survives; say so in
            // the parked diagnostic instead of dropping the failure.
            message =
                format!("{message}; additionally failed to discard the pending turn: {error}");
        }
        if message.is_empty() {
            message = "agent step failed without a diagnostic".to_owned();
        }
        self.scratch = TurnScratch::None;
        let cursor =
            LoopCursor::error(message).expect("the error message is normalized to be non-empty");
        if let Err(error) = self.state.transition_cursor(cursor) {
            // The transition table accepts `Error` from every cursor kind
            // (M4-4 added the `(Done | Error) -> Error` edge), so this branch
            // is structurally unreachable; keep it loud instead of silent.
            debug_assert!(false, "parking on the error cursor is total: {error}");
        }
        StepOutcome::new(notifications, Vec::new(), true)
    }

    /// Folds an internal [`StepError`] into a quiescent
    /// [`LoopCursor::Error`], reusing [`fail`](Self::fail)'s teardown.
    ///
    /// This is the single conversion from the machine's internal `Result`
    /// layer back to `step`'s infallible [`StepOutcome`] contract (刀 (C),
    /// migration doc §2.2). The rendered text comes from
    /// [`StepError::message`], so the resulting error cursor is byte-for-byte
    /// identical to the legacy `self.fail(format!(..))` call sites.
    fn fail_from(&mut self, error: StepError) -> StepOutcome {
        self.fail(error.message())
    }
}

impl AgentMachine for DefaultAgentMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        let result = match input {
            StepInput::External(AgentInput::UserMessage(user)) => self.begin_user_turn(user),
            StepInput::External(AgentInput::Pivot(pivot)) => self.inject_pivot(pivot),
            StepInput::Resume(resolution) => self.resume(resolution),
            StepInput::Abandon(id) => self.abandon(id),
        };
        match result {
            Ok(outcome) => outcome,
            // A caller protocol violation rejects the input without touching
            // machine state (M4-4); only genuine runtime failures park on the
            // error cursor.
            Err(StepError::Rejected(reason)) => StepOutcome::rejected(reason),
            Err(error) => self.fail_from(error),
        }
    }

    fn cursor(&self) -> &LoopCursor {
        self.state.loop_cursor()
    }
}

#[cfg(test)]
mod tests;
