use std::sync::{Arc, Mutex};

use crate::agent::external::{
    ExternalAgentMachine, ExternalAgentSpec, ExternalAgentState, ExternalArtifactRef,
    ExternalSessionPolicy, ExternalSessionRef, ExternalStreamPolicy, WorktreeIsolation,
};
use crate::agent::{
    AgentError, AgentId, AgentMachine, AgentSpecRef, BudgetLimits, CancellationToken,
    DrivingSubagentHandler, ExternalRuntimeKind, ExternalSessionHandler, HandlerScope, Interaction,
    InteractionHandler, LoopCursor, RequirementIds, RequirementKindTag, RequirementResult,
    RunContext, RunId, ScopePop, SpawnedChild, StepInput, StepOutcome, SubagentHandler,
    SubagentOutput, SubagentSpawner, ToolSetRef, TraceHandle, TraceNodeId, TurnDone, WorktreeRef,
};
use crate::conversation::{Conversation, ConversationConfig};
use crate::facade::agent::final_turn_summary;
use crate::facade::collab::CollabBridge;
use crate::facade::delegate::{
    DEFAULT_MAX_DELEGATION_DEPTH, DelegationInteractionRouter, delegation_child_ids,
    delegation_opening_input, summarize_delegation_slot,
};
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::ArtifactRef;
use crate::model::usage::Usage;

use super::{ManagedExternalAgent, runtime_label};

///
/// This pairs the registration `name` (which mints the `ask_<name>` delegation
/// tool, §13.1) with the data-first [`ManagedExternalAgent`] recipe that is
/// driven when the supervising model routes work to it (M4-2). It mirrors
/// [`LocalSubagent`](crate::facade::LocalSubagent) for the external side: the
/// live runtime is assembled only when a delegation is fulfilled.
#[derive(Clone, Debug)]
pub struct ManagedExternalDelegate {
    name: String,
    agent: ManagedExternalAgent,
}

impl ManagedExternalDelegate {
    /// Stamps `name` onto `agent`, forming a registered external delegate.
    #[must_use]
    pub(crate) fn new(name: impl Into<String>, agent: ManagedExternalAgent) -> Self {
        Self {
            name: name.into(),
            agent,
        }
    }

    /// Returns the delegate's registration name (the `ask_<name>` stem).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns a terse description advertised on the delegation tool.
    ///
    /// A managed external agent carries no free-form description, so this is a
    /// generated one naming the backing runtime and run mode.
    #[must_use]
    pub fn description(&self) -> String {
        format!(
            "Delegate a task to the `{}` managed external agent ({} runtime, {} mode).",
            self.name,
            runtime_label(self.agent.runtime()),
            self.agent.mode().as_str()
        )
    }

    /// Returns the data-first managed external agent recipe.
    #[must_use]
    pub const fn agent(&self) -> &ManagedExternalAgent {
        &self.agent
    }
}

/// The policy for reconciling a managed external delegate's previously-live
/// session when an [`Agent`](crate::facade::Agent) is restored from an
/// [`AgentSnapshot`](crate::facade::AgentSnapshot) (`docs/facade-api.md` §15.3,
/// `PLAN.md` R6).
///
/// An [`AgentSnapshot`](crate::facade::AgentSnapshot) captures only *data* about
/// a managed external delegate's last-known session (its runtime kind, worktree,
/// session id, last status, artifact and transcript refs) — never the live
/// process, SDK client, or credentials (§15.2). When such a snapshot is
/// restored, the previously-live external runtime is gone, so the caller must
/// declare how to reconcile it:
///
/// - [`MarkInterrupted`](Self::MarkInterrupted) (the default) records the
///   delegate as interrupted and does **not** touch the external runtime, so the
///   caller can inspect [`RunOutput`](crate::facade::RunOutput) or the snapshot
///   and decide to continue, cancel, manually repair, or restart. This is the
///   safe default because a coding agent may already have changed the worktree,
///   so a blind restart is risky (R6).
/// - [`AttachOrFail`](Self::AttachOrFail) re-attaches to the recorded session and
///   fails fast if it cannot (no re-registered runtime handler, no resumable
///   session, or a runtime that does not support resume). Reserved for read-only
///   / resumable external agents where re-attaching is safe (R6).
/// - [`RestartFromBrief`](Self::RestartFromBrief) discards the recorded session
///   and lets the next run start the delegate afresh from its task brief.
///
/// ```
/// use agent_lib::facade::RestoreExternal;
///
/// // The safe default leaves the external runtime untouched.
/// assert_eq!(RestoreExternal::default(), RestoreExternal::MarkInterrupted);
/// assert_eq!(RestoreExternal::AttachOrFail.as_str(), "attach_or_fail");
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreExternal {
    /// Re-attach to the recorded session; fail fast if it cannot be attached.
    AttachOrFail,
    /// Mark the delegate interrupted without touching the external runtime
    /// (the safe default).
    #[default]
    MarkInterrupted,
    /// Discard the recorded session and start the delegate afresh next run.
    RestartFromBrief,
}

impl RestoreExternal {
    /// Returns the stable snake_case label of this policy.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AttachOrFail => "attach_or_fail",
            Self::MarkInterrupted => "mark_interrupted",
            Self::RestartFromBrief => "restart_from_brief",
        }
    }
}

impl std::fmt::Display for RestoreExternal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The last-known lifecycle status of a managed external delegate's session, as
/// captured in an [`AgentSnapshot`](crate::facade::AgentSnapshot) (data-only,
/// §15.2).
///
/// This is a coarse, serializable status a host can inspect after a restore to
/// decide how to proceed; it carries no runtime handle or credential.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalDelegateStatus {
    /// No session has been driven for this delegate yet.
    #[default]
    Pending,
    /// The last driven session completed cleanly.
    Completed,
    /// The last driven session failed or was cancel-abandoned.
    Failed,
    /// The session was marked interrupted by an
    /// [`AgentSnapshot`](crate::facade::AgentSnapshot) restore under
    /// [`RestoreExternal::MarkInterrupted`].
    Interrupted,
}

impl ExternalDelegateStatus {
    /// Returns the stable snake_case label of this status.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

impl std::fmt::Display for ExternalDelegateStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The retained last-known session facts for one managed external delegate.
///
/// The [`Agent`](crate::facade::Agent) updates this after a `run_full` drive so a
/// later [`snapshot`](crate::facade::Agent::snapshot) can persist the delegate's
/// data-only session state (status, resumable [`ExternalSessionRef`], and any
/// reported [`ArtifactRef`]s). It never holds a process handle, SDK client, or
/// credential.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RetainedExternalSession {
    /// The delegate's last-known coarse status.
    pub status: ExternalDelegateStatus,
    /// The resumable session facts reported by the last drive, if any.
    pub session: Option<ExternalSessionRef>,
    /// Artifacts reported by the last completed drive, in order.
    pub artifacts: Vec<ArtifactRef>,
}

/// The facts captured from a driven external delegation.
///
/// The drive's recording wrapper snapshots these off the
/// [`ExternalAgentState`] after every step, so the last write reflects the
/// final state whether the session ran to completion or was abandoned on
/// cancel. Hosts receive this from [`run_external_once`]; the crate-internal
/// delegation drive folds it back as the delegation tool result.
#[derive(Clone, Debug, Default)]
pub struct ExternalDriveOutcome {
    /// The session's final summary text, folded back as the tool result.
    pub summary: String,
    /// Token usage reported by the runtime for the delegated turn.
    pub usage: Usage,
    /// Artifacts (patches/diffs/test results/files) the session reported.
    pub artifacts: Vec<ArtifactRef>,
    /// Whether the machine reached its terminal `Done` cursor.
    pub completed: bool,
    /// Whether the abandoned session left a live runtime for the handle layer to
    /// sweep (the cancel cleanup marker, design §6.4).
    pub cleanup_required: bool,
    /// The resumable session facts the runtime reported, if any. Captured so a
    /// later [`Agent`](crate::facade::Agent) snapshot can persist the delegate's
    /// data-only session id / transcript / resume token (§15.2).
    pub session: Option<ExternalSessionRef>,
}

/// A shared, single-slot capture of an [`ExternalDriveOutcome`].
type ExternalOutcomeSlot = Arc<Mutex<Option<ExternalDriveOutcome>>>;

/// Wraps an [`ExternalAgentMachine`] to capture its terminal facts and bridge
/// its collab observations.
///
/// The [`SubagentSpawner`] only observes the drained [`TurnDone`], never the
/// child machine state, so this wrapper snapshots the current
/// [`ExternalAgentState`] into a shared slot after every step. On a
/// `Completed` step it captures the committed turn's summary/usage plus the
/// recorded artifacts; on a cancel `Abandon` step it captures the
/// [`cleanup_required`](ExternalAgentState::cleanup_required) marker. The
/// [`drive_external`] caller then reads the slot to fold the result back and
/// record the delegation trace, artifacts, and usage.
///
/// Every step's notifications are also handed to the [`CollabBridge`], which
/// reflects the delegate's `send_message` / `plan_update` / `blackboard_post`
/// observations into the facade's provisioned collab substrate (§14 末段). A
/// machine replays each observation exactly once (design §5.5), so the bridge
/// absorbs each collab event a single time.
struct RecordingExternalMachine {
    inner: ExternalAgentMachine,
    slot: ExternalOutcomeSlot,
    /// The delegate's name, attributed as the sender of bridged collab writes.
    from: String,
    /// Bridge into the facade's provisioned collab substrate.
    bridge: CollabBridge,
}

impl AgentMachine for RecordingExternalMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        let outcome = self.inner.step(input);
        let state = self.inner.state();
        let completed = matches!(self.inner.cursor(), LoopCursor::Done(_));
        let (summary, usage, _stop) = final_turn_summary(state.conversation());
        let artifacts = state.artifacts().iter().map(map_artifact).collect();
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = Some(ExternalDriveOutcome {
            summary,
            usage,
            artifacts,
            completed,
            cleanup_required: state.cleanup_required(),
            session: state.session().cloned(),
        });
        self.bridge
            .absorb_notifications(&self.from, &outcome.notifications);
        outcome
    }

    fn cursor(&self) -> &LoopCursor {
        self.inner.cursor()
    }
}

/// The child external session's own drain layer: it serves only the
/// `NeedExternalSession` family through the injected handler.
///
/// Other requirements the external machine could emit (a bridged
/// `NeedInteraction`, `NeedTool`, or `NeedSubagent`) pop to the outer layer. The
/// facade installs [`ExternalInteractionScope`] outside this child layer so
/// external permission prompts can be answered by the supervisor-injected
/// interaction handler while unsupported families still surface as unhandled
/// requirements instead of being silently dropped.
struct ExternalChildScope {
    external: Arc<dyn ExternalSessionHandler>,
}

impl HandlerScope for ExternalChildScope {
    fn external(&self) -> Option<&dyn ExternalSessionHandler> {
        Some(self.external.as_ref())
    }
}

/// The outer layer for an external child drive.
///
/// When the supervisor supplied an async interaction handler, this scope answers
/// external runtime permission prompts through it with delegate attribution. When
/// no handler is present the scope deliberately stays headless for interactions;
/// `drive_external` turns the resulting `UnhandledRequirement` into a clearer
/// facade error.
struct ExternalInteractionScope {
    interaction: Option<DelegationInteractionRouter>,
}

impl ExternalInteractionScope {
    /// Builds the optional interaction route for one external delegate.
    fn new(delegate: String, parent: Option<Arc<dyn InteractionHandler>>) -> Self {
        Self {
            interaction: parent.map(|parent| DelegationInteractionRouter { delegate, parent }),
        }
    }
}

impl HandlerScope for ExternalInteractionScope {
    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        self.interaction
            .as_ref()
            .map(|router| router as &dyn InteractionHandler)
    }
}

/// Turns one external delegation into a drivable [`ExternalAgentMachine`], its
/// scope, and its opening input.
///
/// Built fresh per delegation call so its capture `slot` is call-local. The
/// external [`ExternalAgentSpec`] is rebuilt from the delegate's data-first
/// [`ManagedExternalAgent`]: its runtime kind, worktree, permission mode, and an
/// empty tool set (host tools are an M4-3+ capability). The scope serves the
/// machine's `NeedExternalSession` requirements through the delegate's injected
/// [`ExternalSessionHandler`].
struct FacadeExternalSpawner {
    name: String,
    agent_id: AgentId,
    runtime: ExternalRuntimeKind,
    worktree: WorktreeRef,
    policy: ExternalSessionPolicy,
    handler: Arc<dyn ExternalSessionHandler>,
    ids: FacadeIds,
    task: String,
    slot: ExternalOutcomeSlot,
    /// Bridge the child machine's collab observations flow into (§14 末段).
    bridge: CollabBridge,
}

impl SubagentSpawner for FacadeExternalSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        Ok(delegation_child_ids(&self.ids, "external", &self.name))
    }

    fn spawn(
        &self,
        _spec_ref: &AgentSpecRef,
        _brief: &Interaction,
        _result_schema: Option<&serde_json::Value>,
    ) -> Result<SpawnedChild, AgentError> {
        let spec = ExternalAgentSpec::new(
            self.agent_id,
            self.runtime.clone(),
            self.worktree.clone(),
            None,
            ToolSetRef::new(self.ids.tool_set_id(), Vec::new()),
            self.policy,
        );
        let state = ExternalAgentState::new(
            spec,
            Conversation::new(self.ids.conversation_id(), ConversationConfig::new(None)),
        );
        let requirement_ids: Arc<dyn RequirementIds> = Arc::new(self.ids.clone());
        let machine = ExternalAgentMachine::new(state, requirement_ids);
        let recording = RecordingExternalMachine {
            inner: machine,
            slot: self.slot.clone(),
            from: self.name.clone(),
            bridge: self.bridge.clone(),
        };

        let scope = ExternalChildScope {
            external: self.handler.clone(),
        };

        let opening = delegation_opening_input(&self.ids, &self.task)?;

        Ok(SpawnedChild {
            machine: Box::new(recording),
            scope: Box::new(scope),
            opening,
        })
    }

    fn summarize(&self, _done: &TurnDone) -> SubagentOutput {
        summarize_delegation_slot(&self.slot, |captured| captured.summary.clone())
    }
}

/// Drives one managed external delegation to its next terminal state, returning
/// the captured [`ExternalDriveOutcome`].
///
/// The external agent is driven the same way a local subagent is (M3-2): through
/// the reference [`DrivingSubagentHandler`], so it shares the host's scope
/// derivation, cancel propagation, budget ledger, and trace node. The child
/// machine is an [`ExternalAgentMachine`] whose `NeedExternalSession`
/// requirements are served by the delegate's injected
/// [`ExternalSessionHandler`] (design §11.2). External `NeedInteraction`
/// requirements pop to an outer route that uses the supervisor-injected
/// [`InteractionHandler`] when present, adding delegate/depth attribution before
/// the answer is fed back to the runtime. A cancelled `ctx` makes the drive
/// abandon the outstanding session step, so the returned outcome carries the
/// runtime cleanup marker.
///
/// # Automatic session cleanup (M3-2)
///
/// A drive that ends without a committed session — cancel-abandoned
/// ([`cleanup_required`](ExternalDriveOutcome::cleanup_required)) or failed
/// before reaching its terminal cursor — may have left a live runtime in the
/// handler's registry, so this helper force-closes it:
/// [`ExternalSessionHandler::cleanup_agent`] is called with the drive's
/// freshly minted agent id, which scopes the sweep to exactly this drive's
/// sessions. The shipped registry-backed handler forwards that to
/// [`ExternalSessionRegistry::cleanup_agent`](crate::agent::external::ExternalSessionRegistry::cleanup_agent), running the adapter's shutdown
/// (a best-effort `session/cancel` plus transport close, process-group
/// termination for a real child) and feeding each session's disposition into
/// the registry's worktree policy; the dispositions are also recorded into the
/// run trace (best effort). A host that does nothing extra therefore leaks no
/// subprocess. A *committed* drive keeps its live session untouched — the
/// clean-teardown / dirty-retention worktree policy is unchanged.
///
/// The sweep is spawned as a detached background task before this helper
/// returns (M3-R): a slow-to-die session can take far longer to close than the
/// outer run's cancel-unwind grace, and an inline await could be dropped
/// mid-sweep with it. Classified teardown therefore continues in the
/// background beyond the outer run's cancellation; its trace audit lands in
/// the shared run trace whenever it finishes.
///
/// # Errors
///
/// Returns [`FacadeError::ExternalAgent`] when the delegate has no session
/// handler attached, or when the drive fails before reaching a terminal cursor.
pub(crate) async fn drive_external(
    name: &str,
    agent: &ManagedExternalAgent,
    ids: &FacadeIds,
    task: String,
    collab: &CollabBridge,
    parent_interaction: Option<Arc<dyn InteractionHandler>>,
    ctx: &RunContext,
) -> Result<ExternalDriveOutcome, FacadeError> {
    drive_external_with_agent_id(name, agent, ids, task, collab, parent_interaction, ctx)
        .await
        .map(|(_agent_id, outcome)| outcome)
}

/// The worker behind [`drive_external`] and [`run_external_once`]: it drives
/// the delegation with a freshly minted `agent_id` and reports it back to the
/// caller, so the caller can scope an additional terminal cleanup sweep to
/// exactly this drive's sessions. See [`drive_external`] for the full
/// contract.
async fn drive_external_with_agent_id(
    name: &str,
    agent: &ManagedExternalAgent,
    ids: &FacadeIds,
    task: String,
    collab: &CollabBridge,
    parent_interaction: Option<Arc<dyn InteractionHandler>>,
    ctx: &RunContext,
) -> Result<(AgentId, ExternalDriveOutcome), FacadeError> {
    let Some(session_handler) = agent.session_handler() else {
        return Err(FacadeError::ExternalAgent {
            name: name.to_owned(),
            message: "no runtime session handler is attached; call \
                      ManagedExternalAgentBuilder::session_handler(..) to drive it"
                .to_owned(),
        });
    };

    let worktree = agent
        .worktree()
        .cloned()
        .unwrap_or_else(|| WorktreeRef::new("."));
    let policy = ExternalSessionPolicy {
        permission_mode: agent.permission_mode(),
        isolation: WorktreeIsolation::EphemeralGitWorktree,
        max_turns: None,
        stream_events: ExternalStreamPolicy::Buffered,
    };

    let slot: ExternalOutcomeSlot = Arc::new(Mutex::new(None));
    let agent_id = ids.agent_id();
    let spawner = Arc::new(FacadeExternalSpawner {
        name: name.to_owned(),
        agent_id,
        runtime: agent.runtime().clone(),
        worktree,
        policy,
        handler: session_handler.clone(),
        ids: ids.clone(),
        task: task.clone(),
        slot: slot.clone(),
        bridge: collab.clone(),
    });
    let handler = DrivingSubagentHandler::new(spawner, DEFAULT_MAX_DELEGATION_DEPTH);

    let spec_ref = AgentSpecRef(agent_id);
    let brief = Interaction::question(ids.step_id(), task);
    let interaction_scope = ExternalInteractionScope::new(name.to_owned(), parent_interaction);
    let mut outer = ScopePop::new(&interaction_scope, None);

    let result = handler
        .fulfill(&spec_ref, &brief, None, &mut outer, ctx)
        .await;

    let captured = slot
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
        .unwrap_or_default();

    // M3-2: a drive that did not commit its session — cancel-abandoned
    // (`cleanup_required`) or failed before reaching a terminal cursor — may
    // have left a live runtime in the handler's registry; sweep it so a host
    // that does nothing extra leaks no subprocess. The drive's `agent_id`
    // scopes the sweep to exactly this drive's sessions, and a committed
    // drive is left untouched (worktree teardown/retention policy unchanged).
    if !captured.completed {
        spawn_external_cleanup_sweep(
            Arc::clone(session_handler),
            ctx.trace().clone(),
            ctx.run_id(),
            agent_id,
        );
    }

    match result {
        RequirementResult::Subagent(Ok(_output)) => Ok((agent_id, captured)),
        RequirementResult::Subagent(Err(error)) => Err(FacadeError::ExternalAgent {
            name: name.to_owned(),
            message: external_drive_error_message(&error),
        }),
        other => Err(FacadeError::ExternalAgent {
            name: name.to_owned(),
            message: format!(
                "external drive returned an unexpected `{}` result",
                other.tag()
            ),
        }),
    }
}

/// Spawns the detached `'static` cleanup sweep for one external drive.
///
/// Shared by [`drive_external_with_agent_id`] (uncommitted outcomes) and
/// [`run_external_once`] (additionally the completed one): the drive's
/// `agent_id` scopes the sweep to exactly that drive's sessions, and each
/// swept session's disposition is recorded into the run trace, best effort,
/// mirroring the adapter mid-session close audit.
///
/// The sweep is spawned rather than awaited inline (M3-R, F1). A drive future
/// can run inside an outer run's batch under `fulfill_batch_cancellable`'s
/// `CANCEL_UNWIND_GRACE` (2s), while a slow-to-die session can take far longer
/// to close (the adapter's `shutdown_grace` defaults to ~30s: a child may
/// linger after stdin EOF). An inline await could be dropped mid-sweep when
/// the grace expires — skipping the process-group SIGTERM→SIGKILL (leaking
/// grandchildren), the ephemeral worktree sweep, and the trace audit. The
/// handler is an `Arc`, the ids are owned, and the trace handle is shared, so
/// the spawned sweep borrows nothing: the drive settles promptly and
/// classified teardown completes in the background regardless of the outer
/// run's grace.
///
/// The trace node id carries the drive's `agent_id` as well as the outer
/// `run_id` (M3-R, F2), so two drives swept under one outer run never mint
/// colliding node ids (the second sweep's audit would be silently swallowed
/// by the trace's duplicate-id rejection).
fn spawn_external_cleanup_sweep(
    session_handler: Arc<dyn ExternalSessionHandler>,
    trace: TraceHandle,
    run_id: RunId,
    agent_id: AgentId,
) {
    let _sweep = tokio::spawn(async move {
        let dispositions = session_handler.cleanup_agent(agent_id).await;
        for (seq, disposition) in dispositions.into_iter().enumerate() {
            let id = TraceNodeId::new(format!("external-cleanup-sweep/{run_id}/{agent_id}/{seq}"));
            let _ = trace.record_external_shutdown(id, disposition);
        }
    });
}

/// Drives one managed external agent through a single self-contained task and
/// returns its captured [`ExternalDriveOutcome`] — a "launch the runtime → run
/// one task → collect the final text → reclaim the process" one-shot.
///
/// Unlike the crate-internal delegation drive (which the stateful
/// [`Agent`](crate::facade::Agent) reuses across runs, deliberately keeping a
/// *committed* session live for reuse), this entry point owns the whole run:
/// it mints a fresh root [`RunContext`] from `budget` and `cancel` — a
/// one-shot call has no parent chain — and drives with collaboration
/// reflection disabled (the default `CollabBridge` is an inactive no-op).
/// `parent_interaction`, when present, answers the external runtime's
/// permission prompts with delegate attribution, exactly as the delegation
/// drive routes them.
///
/// # Reclamation guarantee
///
/// The runtime's session/process is reclaimed at **every** terminal state of
/// the call, so a host that does nothing extra leaks no subprocess:
///
/// - **failed / cancelled** — swept by the drive itself (its unchanged
///   semantics: `cleanup_required` or a pre-terminal failure schedules the
///   detached cleanup);
/// - **completed** — the drive leaves its live session registered for reuse,
///   so this wrapper schedules the same detached sweep for it; a one-shot
///   caller has no later run that could reuse the session and no
///   [`Agent`](crate::facade::Agent) drop to catch it.
///
/// The sweep runs as a detached background task: the call returns as soon as
/// the outcome lands, while classified teardown (transport close,
/// process-group termination, worktree sweep, trace audit) completes in the
/// background.
///
/// # Errors
///
/// Returns [`FacadeError::ExternalAgent`] when `agent` has no runtime session
/// handler attached — attach one with
/// [`ManagedExternalAgentBuilder::session_handler`](crate::facade::ManagedExternalAgentBuilder::session_handler),
/// or build with
/// [`build_with_default_session_handler`](crate::facade::ManagedExternalAgentBuilder::build_with_default_session_handler)
/// — or when the drive fails before reaching a terminal cursor.
pub async fn run_external_once(
    name: &str,
    agent: &ManagedExternalAgent,
    ids: &FacadeIds,
    task: String,
    parent_interaction: Option<Arc<dyn InteractionHandler>>,
    budget: BudgetLimits,
    cancel: CancellationToken,
) -> Result<ExternalDriveOutcome, FacadeError> {
    let ctx = RunContext::new_root_with_cancellation(
        ids.run_id(),
        budget,
        ids.trace_root("external-once"),
        cancel,
    );
    let (agent_id, outcome) = drive_external_with_agent_id(
        name,
        agent,
        ids,
        task,
        &CollabBridge::default(),
        parent_interaction,
        &ctx,
    )
    .await?;

    // One-shot reclamation (see the doc above): the drive sweeps only
    // *uncommitted* outcomes, so the wrapper schedules the same detached sweep
    // for a completed one — the one-shot caller has no later run that could
    // reuse the session and no `Agent` drop to catch it. The handler is
    // necessarily attached: the drive could not have completed without one.
    if outcome.completed
        && let Some(session_handler) = agent.session_handler()
    {
        spawn_external_cleanup_sweep(
            Arc::clone(session_handler),
            ctx.trace().clone(),
            ctx.run_id(),
            agent_id,
        );
    }

    Ok(outcome)
}

/// Renders external drive failures with a targeted interaction-routing message.
fn external_drive_error_message(error: &AgentError) -> String {
    if matches!(
        error,
        AgentError::UnhandledRequirement {
            kind: RequirementKindTag::Interaction,
            ..
        }
    ) {
        return "external agent requested permission but no interaction handler is available to answer it"
            .to_owned();
    }

    error.to_string()
}

/// Projects an agent-layer [`ExternalArtifactRef`] into the facade
/// [`ArtifactRef`] surface.
///
/// The facade artifact reference carries only a locating `path`; the agent-layer
/// reference may leave `path` unset (for example a bare test result), so this
/// falls back to the opaque stored `reference` and finally the untrusted
/// `summary` so an artifact is never advertised without a locator. Only
/// references are copied — never inline diffs — keeping the mapping
/// redaction-safe (design §11).
fn map_artifact(artifact: &ExternalArtifactRef) -> ArtifactRef {
    let path = artifact
        .path
        .clone()
        .or_else(|| artifact.reference.clone())
        .unwrap_or_else(|| artifact.summary.clone());
    ArtifactRef { path }
}
