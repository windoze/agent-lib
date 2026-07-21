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
//! A [`HandlerScope`] offers up to one handler per
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
//! - [`ReconfigHandler`] fulfills a `NeedReconfigRegistry` by swapping the
//!   active tool registry at a turn boundary.
//! - [`ExternalSessionHandler`] fulfills a `NeedExternalSession` by advancing an
//!   external coding-agent session one decision point.
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
//! [`HandlerScope`] and its handler traits give one drain layer its effect
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
        AgentError, AgentId, AgentInput, AgentMachine, LlmStepMode, LoopCursor, LoopCursorKind,
        Notification, Requirement, RequirementDisposition, RequirementId, RequirementKind,
        RequirementResolution, RequirementResult, RunContext, RunContextError, StepInput,
        ToolSetRef, TraceError, TraceNodeId,
        effect_manifest::{define_effect_fan_out, with_effect_manifest},
        external::{ExternalSessionRequest, ExternalSessionShutdown},
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
mod subagent;

pub use reference::{
    ApprovalInteractionHandler, LlmClientHandler, ReconfigRegistryHandler, ReferenceScope,
    ToolRegistryHandler, drive_turn,
};
pub use subagent::{DrivingSubagentHandler, SpawnedChild, SubagentSpawner};

// `HandlerScope`, `scope_handles`, and `fulfill_with_scope` are generated from
// the single effect manifest in
// [`effect_manifest`](crate::agent::effect_manifest) (the coproduct half is
// generated in [`requirement`](crate::agent::requirement)). Each accessor
// defaults to `None`, so an empty scope handles nothing and every requirement
// pops to the outer scope; the handler traits the accessors borrow are defined
// below. See the module docs for how scopes compose into a drain.
with_effect_manifest!(define_effect_fan_out);

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
///
/// # Cancellation
///
/// A cancelled drive pre-empts the batch wait (M3-3): after a bounded unwind
/// grace, a still-blocked fulfill future is **dropped (detached)** and its
/// requirement is settled as a never-resume. Implementations whose work
/// outlives casual dropping — anything with side effects, held resources, or
/// human-visible prompts — must therefore select on
/// [`RunContext::cancellation`] themselves for long work; facade tools receive
/// the same token as [`ToolContext::cancel`](crate::facade::ToolContext::cancel).
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
///
/// # Cancellation
///
/// Same detach contract as [`ToolHandler`]: a cancelled run pre-empts the
/// batch wait and may drop a still-pending fulfill future after the unwind
/// grace (M3-3), so an attended backend that blocks on a human must select on
/// [`RunContext::cancellation`] to abort its prompt instead of relying on
/// running to completion.
#[async_trait]
pub trait InteractionHandler: Send + Sync {
    /// Presents `request` to the backend and returns the resolved response.
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult;
}

/// Fulfills a `NeedSubagent` requirement by deriving and driving a child agent.
///
/// This is the only scope-deepening handler: fulfilling it opens another drain
/// layer for the child machine. The handler derives a child [`RunContext`]
/// (cancel ↓ / budget ↕ / trace ↓), builds the child machine, and drives it with
/// a nested [`drain`]. The `outer` layer it receives is the pop target for
/// requirements the child's own scope cannot serve (migration doc §7.3): a
/// child `NeedInteraction` that its headless scope omits pops to `outer` — the
/// scope that emitted the `NeedSubagent` plus that scope's parents — so the
/// handler's own layer serves it rather than re-entering the handler. The
/// returned [`RequirementResult`] must be a [`RequirementResult::Subagent`].
#[async_trait]
pub trait SubagentHandler: Send + Sync {
    /// Derives the child agent named by `spec_ref`, drives it against `brief`
    /// (optionally constrained by `result_schema`), and returns its result.
    ///
    /// `outer` is the layer requirements the child cannot serve pop to; it is a
    /// [`ScopePop`] over the scope that emitted this `NeedSubagent` and that
    /// scope's own parents, so a popped requirement never re-enters this handler
    /// (§7.3).
    async fn fulfill(
        &self,
        spec_ref: &AgentSpecRef,
        brief: &Interaction,
        result_schema: Option<&Value>,
        outer: &mut dyn Pop,
        ctx: &RunContext,
    ) -> RequirementResult;
}

/// Fulfills a `NeedReconfigRegistry` requirement by swapping the active tool
/// registry at a turn boundary.
///
/// The machine holds no live registry: when a queued reconfiguration changes the
/// active tool set it reifies the swap as this requirement. The handler resolves
/// `tool_set` to an executable registry, validates its declarations against the
/// requested set, installs it as the registry future tool steps execute
/// against, and confirms with an `Ok` [`RequirementResult::Reconfig`]. A
/// resolution or declaration-mismatch failure is carried inside an `Err`
/// [`RequirementResult::Reconfig`], which fails the parked boundary.
#[async_trait]
pub trait ReconfigHandler: Send + Sync {
    /// Resolves and installs the registry for `tool_set`, returning confirmation.
    async fn fulfill(&self, tool_set: &ToolSetRef, ctx: &RunContext) -> RequirementResult;
}

/// Fulfills a `NeedExternalSession` requirement by advancing an external
/// coding-agent session (Claude Code / Codex / …) one decision point.
///
/// The machine holds no runtime connection, so the whole session lives on the
/// driver side: this handler owns the runtime and advances the session
/// described by `request` until it reaches its next decision point — the runtime
/// finished this step ([`Completed`](crate::agent::ExternalSessionResult::Completed)),
/// paused awaiting an interaction
/// ([`PausedForInteraction`](crate::agent::ExternalSessionResult::PausedForInteraction)),
/// or failed ([`Failed`](crate::agent::ExternalSessionResult::Failed)) — never
/// running it to completion in one blocking call (design §5.5). Every event
/// observed on the way to that decision point is buffered into the result's
/// `observations` so the machine can convert them into notifications after
/// resume.
///
/// The returned [`RequirementResult`] must be a
/// [`RequirementResult::ExternalSession`]; launch or session-loss failures are
/// carried inside its `Failed` variant, not by returning the wrong family.
#[async_trait]
pub trait ExternalSessionHandler: Send + Sync {
    /// Advances the session described by `request` to its next decision point
    /// and returns the observed result.
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> RequirementResult;

    /// Force-closes every live session this handler started for `agent_id`,
    /// returning each session's [`ExternalSessionShutdown`] disposition.
    ///
    /// Cancelling an external agent is never-resume (design §6.4): an
    /// abandoned drive is not stepped again, so it can never emit a graceful
    /// [`Shutdown`](crate::agent::ExternalSessionInput::Shutdown) — the machine
    /// only flags
    /// [`cleanup_required`](crate::agent::ExternalAgentState::cleanup_required)
    /// and the handle layer must close the live runtime. The facade drive path
    /// calls this hook automatically whenever a drive ends without a committed
    /// session (cancel-abandoned *or* failed), so a host whose handler owns
    /// real runtime IO leaks no subprocess without doing anything extra (M3-2).
    ///
    /// The default is a no-op, which is correct for handlers that own no live
    /// runtime state (scripted or cassette stand-ins). A handler that *does*
    /// own live runtime IO **must** override this to force-close the agent's
    /// sessions — the shipped
    /// [`RegistryExternalSessionHandler`](crate::agent::external::RegistryExternalSessionHandler)
    /// forwards it to its registry — or a cancelled drive silently leaks
    /// whatever the runtime holds (a CLI child, an SDK client, a reader task)
    /// until the handler itself is dropped.
    async fn cleanup_agent(&self, agent_id: AgentId) -> Vec<ExternalSessionShutdown> {
        let _ = agent_id;
        Vec::new()
    }
}

/// Outcome of draining one machine to the end of a turn.
///
/// Carries the notifications produced across the whole drain (the driver simply
/// forwards them; see migration doc §12 decision C) and the [`LoopCursor`] the
/// machine came to rest on, plus whether the drain was cut short by
/// cancellation:
///
/// - A *natural* end rests on a terminal cursor ([`LoopCursor::Done`] or
///   [`LoopCursor::Error`]) and reports `cancelled() == false`.
/// - A *cancelled* drain closes the in-flight turn through the machine's
///   never-resume path and stops driving; the machine's own rest state after
///   that closure is [`LoopCursor::Idle`] on the reference machines, and the
///   outcome reports `cancelled() == true`. Callers must consult
///   [`cancelled`](Self::cancelled) rather than the cursor alone to tell a
///   cancelled turn apart from a completed one (M4-5 / M-ERR-2).
#[derive(Clone, Debug)]
pub struct TurnDone {
    notifications: Vec<Notification>,
    cursor: LoopCursor,
    cancelled: bool,
}

impl TurnDone {
    /// Creates a turn result from the drained notifications and final cursor
    /// (a natural, non-cancelled end of the drain).
    #[must_use]
    pub const fn new(notifications: Vec<Notification>, cursor: LoopCursor) -> Self {
        Self {
            notifications,
            cursor,
            cancelled: false,
        }
    }

    /// Marks whether the drain ended through cancellation.
    #[must_use]
    pub fn with_cancelled(mut self, cancelled: bool) -> Self {
        self.cancelled = cancelled;
        self
    }

    /// Returns the notifications produced over the whole drain, in order.
    #[must_use]
    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    /// Returns the cursor the machine came to rest on (terminal on a natural
    /// end; the machine's post-cancel rest state on a cancelled drain).
    #[must_use]
    pub const fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }

    /// Returns whether the drain was cut short by cancellation.
    #[must_use]
    pub const fn cancelled(&self) -> bool {
        self.cancelled
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
///
/// The returned `u32` is the number of additional pop hops taken *from this pop
/// target's own scope* to the scope that actually settled the requirement (`0`
/// when this target's scope settled it in place). Each layer the requirement
/// passes through adds one on the way back up, so the emitting layer learns how
/// many scopes out its requirement was resolved — the `resolved_at_scope` a
/// [`TraceNodeKind::Requirement`](crate::agent::TraceNodeKind) records
/// (migration doc §8).
#[async_trait]
pub trait Pop: Send {
    /// Fulfills `requirement` at this outer layer (or pops it further outward),
    /// returning a type-aligned [`RequirementResult`] and the pop distance to
    /// the resolving scope.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::UnhandledRequirement`] when the requirement reaches
    /// the top layer with no handler, or a propagated handler error.
    async fn pop(
        &mut self,
        requirement: &Requirement,
        ctx: &RunContext,
    ) -> Result<(RequirementResult, u32), AgentError>;
}

/// An outer drain layer viewed as a [`Pop`] target.
///
/// Pairs an outer [`HandlerScope`] with *its own* parent. When a popped
/// requirement arrives, it is fulfilled against this scope if possible, and
/// otherwise popped further outward — so the requirement never re-enters the
/// scope it originally popped from (migration doc §7.3).
///
/// The parent's pointee lifetime `'p` is a distinct parameter from the borrow
/// lifetime `'a`. Keeping them separate lets an outer layer be reborrowed to a
/// short `'a` while preserving the (invariant) `&mut dyn Pop` pointee lifetime —
/// which is what lets a subagent handler build a `ScopePop` over the emitting
/// scope and *its* parent without unifying those two independent lifetimes.
pub struct ScopePop<'a, 'p> {
    scope: &'a dyn HandlerScope,
    parent: Option<&'a mut (dyn Pop + 'p)>,
}

impl<'a, 'p> ScopePop<'a, 'p> {
    /// Wraps `scope` (and its own `parent`) as an outer [`Pop`] target.
    #[must_use]
    pub fn new(scope: &'a dyn HandlerScope, parent: Option<&'a mut (dyn Pop + 'p)>) -> Self {
        Self { scope, parent }
    }
}

#[async_trait]
impl Pop for ScopePop<'_, '_> {
    async fn pop(
        &mut self,
        requirement: &Requirement,
        ctx: &RunContext,
    ) -> Result<(RequirementResult, u32), AgentError> {
        // The hop distance this returns is measured from `self.scope`; the
        // popping layer adds one for the hop it took to reach this target.
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
/// # Cancellation
///
/// Cancellation is observed at three points per loop iteration: before a batch
/// is fulfilled, *during* the batch wait (M3-3), and again right after the
/// batch-fulfil await returns. A cancel that lands while a batch is in flight
/// pre-empts the batch-level wait: cooperative handlers get a bounded unwind
/// grace to finish (so their cleanup tails run), then any still-blocked
/// fulfill future is detached and the drive stops without feeding any
/// resolution back or requesting further work. Either way, *every* outstanding requirement
/// of the interrupted batch is settled as a
/// never-resume: recorded on the trace with a
/// [`NeverResumed`](RequirementDisposition::NeverResumed) disposition and
/// abandoned through the machine's never-resume path. The returned
/// [`TurnDone`] reports `cancelled() == true`; the cursor it carries is the
/// machine's post-cancel rest state (`Idle` on the reference machines), not a
/// terminal `Done`/`Error`.
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
    let mut cancelled = false;

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

        // Cancellation is a downward "should stop" signal (migration doc §7):
        // the token never resumes a requirement, it decides to abandon one. A
        // single `Abandon` closes the whole in-flight turn via the machine's
        // never-resume path (`cancel_pending`), settling the cursor to a
        // feedable rest state, so we stop driving this turn once it lands. A
        // never-resume is a real event that mutates the underlying Conversation
        // (§6.3), so *every* outstanding requirement of the batch is recorded
        // in the trace, settled at the performing layer (`resolved_at_scope ==
        // 0`) with a `NeverResumed` disposition — not just the one the `Abandon`
        // targets (M4-5: a partially traced batch violates the "every
        // requirement settles exactly once" contract).
        if ctx.is_cancelled() {
            settle_cancelled(machine, ctx, pending.iter(), &mut notifications);
            cancelled = true;
            break;
        }

        if budget_precheck_exhausted(ctx, &pending) {
            settle_budget_exhausted(machine, ctx, pending.iter(), &mut notifications);
            break;
        }

        let resolutions =
            match fulfill_batch_cancellable(&pending, scope, parent.as_deref_mut(), ctx).await? {
                BatchOutcome::Completed(resolutions) => resolutions,
                // Cancellation pre-empted the batch wait: the in-flight fulfill
                // futures were detached (dropped) after the unwind grace, so
                // every outstanding requirement of the batch is settled as a
                // never-resume, exactly like the pre-batch cancel path (M3-3).
                BatchOutcome::Preempted => {
                    settle_cancelled(machine, ctx, pending.iter(), &mut notifications);
                    cancelled = true;
                    break;
                }
            };

        // Re-check cancellation between the batch settling and feeding the
        // resolutions back (M4-5): a cancel that landed while the batch was in
        // flight stops the turn here instead of advancing one more batch. The
        // fulfilled results are deliberately never fed back, so every one is a
        // never-resume and is traced as such; the `Abandon` steps then close
        // the turn through the same never-resume path as above.
        if ctx.is_cancelled() {
            settle_cancelled(
                machine,
                ctx,
                resolutions.iter().map(|resolved| &resolved.resolution),
                &mut notifications,
            );
            cancelled = true;
            break;
        }

        pending = Vec::new();
        let mut resolutions = resolutions.into_iter();
        while let Some(resolved) = resolutions.next() {
            if charge_resolution_budget(ctx, &resolved.resolution).is_err() {
                record_requirement_resolution(
                    ctx,
                    &resolved.resolution,
                    0,
                    RequirementDisposition::NeverResumed,
                );
                for remaining in resolutions {
                    record_requirement_resolution(
                        ctx,
                        &remaining.resolution,
                        0,
                        RequirementDisposition::NeverResumed,
                    );
                }
                let mut outcome = machine.interrupt_budget_exhausted();
                notifications.append(&mut outcome.notifications);
                break;
            }
            let Resolved {
                resolution,
                resolved_at_scope,
            } = resolved;
            // Every resolution here was settled by a handler and will be fed
            // back, so it is recorded `Resumed` at the scope distance that
            // fulfilled it (migration doc §8).
            record_requirement_resolution(
                ctx,
                &resolution,
                resolved_at_scope,
                RequirementDisposition::Resumed,
            );
            let mut outcome = machine.step(StepInput::Resume(resolution));
            notifications.append(&mut outcome.notifications);
            pending.extend(outcome.requirements);
        }
    }

    Ok(TurnDone::new(notifications, machine.cursor().clone()).with_cancelled(cancelled))
}

/// A settled requirement reference used on the cancel path: either the
/// outstanding [`Requirement`] itself (cancel observed before fulfilment) or
/// its fulfilled-but-never-resumed [`RequirementResolution`] (cancel observed
/// right after the batch settled).
trait SettledRef {
    /// The settled requirement's id.
    fn id(&self) -> RequirementId;
    /// Records the never-resume in the trace at the performing layer (hop 0).
    fn record_never_resumed(&self, ctx: &RunContext);
}

impl SettledRef for &Requirement {
    fn id(&self) -> RequirementId {
        self.id
    }

    fn record_never_resumed(&self, ctx: &RunContext) {
        record_requirement(ctx, self, 0, RequirementDisposition::NeverResumed);
    }
}

impl SettledRef for &RequirementResolution {
    fn id(&self) -> RequirementId {
        self.id
    }

    fn record_never_resumed(&self, ctx: &RunContext) {
        record_requirement_resolution(ctx, self, 0, RequirementDisposition::NeverResumed);
    }
}

/// Settles every outstanding requirement of a cancelled batch: records each as
/// `NeverResumed` and feeds the machine one `Abandon` per id.
///
/// The first `Abandon` closes the whole in-flight turn through the machine's
/// never-resume path; further `Abandon` steps for ids that closure already
/// settled are soft-rejected no-ops (M4-4), leaving state untouched. Recording
/// still happens for *every* requirement, keeping the "each requirement
/// settles exactly once" trace contract on multi-requirement batches (M4-5).
fn settle_cancelled<M, I>(
    machine: &mut M,
    ctx: &RunContext,
    settled: I,
    notifications: &mut Vec<Notification>,
) where
    M: AgentMachine + ?Sized,
    I: IntoIterator,
    I::Item: SettledRef,
{
    for item in settled {
        item.record_never_resumed(ctx);
        let mut outcome = machine.step(StepInput::Abandon(item.id()));
        notifications.append(&mut outcome.notifications);
    }
}

/// Returns whether this batch would start new budgeted model work after a
/// count-like budget dimension has no headroom left.
pub(crate) fn budget_precheck_exhausted(ctx: &RunContext, requirements: &[Requirement]) -> bool {
    requirements.iter().any(requirement_consumes_budget) && ctx.budget_exhausted().is_some()
}

fn requirement_consumes_budget(requirement: &Requirement) -> bool {
    matches!(&requirement.kind, RequirementKind::NeedLlm { .. })
}

/// Charges the budget dimensions represented by a fulfilled requirement before
/// it is resumed into the machine.
///
/// For LLM completions, the driver charges one logical step first and then the
/// provider-reported token usage. The two charges are intentionally not a
/// reservation: a sibling context can still consume budget between the preflight
/// check and these charges, and a successful step charge remains counted even if
/// the subsequent usage charge trips the token limit.
pub(crate) fn charge_resolution_budget(
    ctx: &RunContext,
    resolution: &RequirementResolution,
) -> Result<(), RunContextError> {
    if let RequirementResult::Llm(Ok(response)) = &resolution.result {
        ctx.charge_step()?;
        ctx.charge_usage(&response.usage)?;
    }
    Ok(())
}

/// Settles a budget-stopped batch without resuming any of its requirements.
fn settle_budget_exhausted<M, I>(
    machine: &mut M,
    ctx: &RunContext,
    settled: I,
    notifications: &mut Vec<Notification>,
) where
    M: AgentMachine + ?Sized,
    I: IntoIterator,
    I::Item: SettledRef,
{
    for item in settled {
        item.record_never_resumed(ctx);
    }
    let mut outcome = machine.interrupt_budget_exhausted();
    notifications.append(&mut outcome.notifications);
}

/// Records a settled requirement (identified by its own `Requirement`) in the
/// trace under the performing layer's parent (migration doc §8).
///
/// The trace node id reuses the host-minted requirement id, keeping the library
/// out of the id-minting business (mirroring every other Agent identity).
///
/// Recording is best-effort: the trace is purely observational, so a recording
/// failure never aborts the drive (H-STATE-4). See [`record_requirement_node`]
/// for how a re-emitted (pivot) requirement id is disambiguated.
pub(crate) fn record_requirement(
    ctx: &RunContext,
    requirement: &Requirement,
    resolved_at_scope: u32,
    disposition: RequirementDisposition,
) {
    record_requirement_node(
        ctx,
        requirement.tag(),
        requirement.id,
        resolved_at_scope,
        disposition,
    );
}

/// Records a settled requirement identified by its [`RequirementResolution`].
pub(crate) fn record_requirement_resolution(
    ctx: &RunContext,
    resolution: &RequirementResolution,
    resolved_at_scope: u32,
    disposition: RequirementDisposition,
) {
    record_requirement_node(
        ctx,
        resolution.tag(),
        resolution.id,
        resolved_at_scope,
        disposition,
    );
}

/// Appends a `Requirement` trace node on a best-effort basis.
///
/// A pivot re-emits the outstanding requirement under the *same* id, so the
/// plain node id is already taken when the re-emission settles: the settle is
/// then recorded under the derived id `<id>#attempt-N` (N counting up from 2),
/// keeping every settle on the trace instead of dropping it. Any other
/// recording failure (for example an [`UnknownParent`](TraceError::UnknownParent)
/// from a structurally broken trace tree) drops the node rather than aborting
/// the drive — observability must never kill the work it observes (H-STATE-4).
fn record_requirement_node(
    ctx: &RunContext,
    kind_tag: RequirementKindTag,
    id: RequirementId,
    resolved_at_scope: u32,
    disposition: RequirementDisposition,
) {
    let base = id.to_string();
    let mut node_id = TraceNodeId::new(base.clone());
    let mut attempt = 2u32;
    loop {
        match ctx.trace().record_requirement(
            node_id.clone(),
            kind_tag,
            resolved_at_scope,
            disposition,
        ) {
            Ok(_) => return,
            Err(TraceError::DuplicateNodeId { .. }) => {
                node_id = TraceNodeId::new(format!("{base}#attempt-{attempt}"));
                attempt += 1;
            }
            Err(TraceError::UnknownParent { .. }) => return,
        }
    }
}

/// Returns whether `cursor` marks the end of a turn.
pub(crate) fn is_terminal(cursor: &LoopCursor) -> bool {
    matches!(cursor.kind(), LoopCursorKind::Done | LoopCursorKind::Error)
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
/// Returns the fulfilled result together with the number of pop hops from
/// `scope` to the scope that settled it (`0` = `scope` settled it in place, each
/// pop outward adds one). This hop count is the `resolved_at_scope` recorded on
/// the requirement's trace node (migration doc §8).
///
/// The top layer (`parent = None`) with no matching handler yields
/// [`AgentError::UnhandledRequirement`].
async fn resolve_requirement(
    requirement: &Requirement,
    scope: &dyn HandlerScope,
    parent: Option<&mut (dyn Pop + '_)>,
    ctx: &RunContext,
) -> Result<(RequirementResult, u32), AgentError> {
    // A subagent deepens the scope chain: its handler drives the child with a
    // nested drain whose pop target is *this* layer (the scope that emitted the
    // requirement, plus that scope's own parents). Build that outer layer as a
    // `ScopePop` and hand it to the handler so the child's unhandled
    // requirements pop outward without re-entering the handler (§7.3). When this
    // scope has no subagent handler, fall through to the normal pop path.
    if let RequirementKind::NeedSubagent {
        spec_ref,
        brief,
        result_schema,
    } = &requirement.kind
    {
        if let Some(handler) = scope.subagent() {
            let mut outer = ScopePop::new(scope, parent);
            let result = handler
                .fulfill(spec_ref, brief, result_schema.as_ref(), &mut outer, ctx)
                .await;
            validate(requirement, &result)?;
            return Ok((result, 0));
        }
    } else if let Some(result) = fulfill_with_scope(&requirement.kind, scope, ctx).await {
        validate(requirement, &result)?;
        return Ok((result, 0));
    }

    match parent {
        // The requirement crosses one scope boundary to reach `parent`, so the
        // hop measured from `parent`'s scope gains one to become the hop from
        // `scope`.
        Some(pop) => {
            let (result, hops) = pop.pop(requirement, ctx).await?;
            Ok((result, hops + 1))
        }
        None => Err(AgentError::UnhandledRequirement {
            kind: requirement.tag(),
            origin: requirement.origin.clone(),
        }),
    }
}

/// A requirement resolution paired with the scope distance that settled it.
///
/// `resolved_at_scope` is the pop hop count from the layer that performed the
/// requirement (`0` = the emitting scope settled it in place); [`drain`] records
/// it on the requirement's trace node (migration doc §8).
pub(crate) struct Resolved {
    pub(crate) resolution: RequirementResolution,
    pub(crate) resolved_at_scope: u32,
}

/// Fulfills a batch of requirements against `scope`, popping the rest.
///
/// Requirements this scope handles are run concurrently and collected in
/// completion order (migration decision B); requirements it cannot handle are
/// resolved one at a time, since a [`Pop`] target is `&mut`. A `NeedSubagent`
/// is always resolved serially — even when this scope handles it — because its
/// handler needs the outer layer (`&mut parent`) as a pop target and so cannot
/// join the concurrent set.
///
/// Each resolution carries the `resolved_at_scope` hop distance: the concurrent
/// local set is always settled in place (`0`), while a serially resolved
/// requirement reports how many scopes out it was popped to.
pub(crate) async fn fulfill_batch(
    requirements: &[Requirement],
    scope: &dyn HandlerScope,
    mut parent: Option<&mut (dyn Pop + '_)>,
    ctx: &RunContext,
) -> Result<Vec<Resolved>, AgentError> {
    let mut local = FuturesUnordered::new();
    let mut serial: Vec<&Requirement> = Vec::new();

    for requirement in requirements {
        let tag = requirement.tag();
        if tag != RequirementKindTag::Subagent && scope_handles(scope, tag) {
            local.push(async move {
                let Some(result) = fulfill_with_scope(&requirement.kind, scope, ctx).await else {
                    debug_assert!(
                        false,
                        "scope_handles confirmed a handler for {tag:?}, but none was available"
                    );
                    return Err(AgentError::Other(format!(
                        "scope advertised a handler for {tag:?} but returned no fulfillment"
                    )));
                };
                validate(requirement, &result)?;
                Ok::<_, AgentError>(Resolved {
                    resolution: RequirementResolution::new(requirement.id, result),
                    resolved_at_scope: 0,
                })
            });
        } else {
            serial.push(requirement);
        }
    }

    let mut resolutions = Vec::with_capacity(requirements.len());
    while let Some(resolution) = local.next().await {
        resolutions.push(resolution?);
    }

    for requirement in serial {
        let (result, resolved_at_scope) =
            resolve_requirement(requirement, scope, parent.as_deref_mut(), ctx).await?;
        resolutions.push(Resolved {
            resolution: RequirementResolution::new(requirement.id, result),
            resolved_at_scope,
        });
    }

    Ok(resolutions)
}

/// The bounded window a cancelled batch gets to unwind cooperatively before
/// its still-running fulfill futures are detached (M3-3).
///
/// Cancellation wakes every cancel-selecting handler at about the same time —
/// the reference LLM handler, the shipped external-session read loops (M3-1),
/// and any host tool/interaction handler that selects on the shared token.
/// Those handlers may need a short moment to run their cleanup tail. (The M3-2
/// external session sweep no longer depends on this window: since M3-R the
/// facade drive spawns it as a detached `'static` task, so classified teardown
/// completes in the background even when the grace expires mid-teardown.) The
/// grace keeps polling the in-flight batch instead of dropping it on the
/// spot; a handler that ignores cancellation — a blocked ask-user tool is the
/// canonical case — is detached only once the grace expires.
pub(crate) const CANCEL_UNWIND_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// The outcome of driving one requirement batch under cancellation (M3-3).
pub(crate) enum BatchOutcome {
    /// The batch ran to completion — either undisturbed, or inside the
    /// post-cancel unwind grace when cancel landed mid-batch but every
    /// in-flight handler unwound cooperatively in time.
    Completed(Vec<Resolved>),
    /// Cancellation pre-empted the batch wait: the unwind grace expired with
    /// fulfill futures still in flight, and those futures were **dropped
    /// (detached)**. No resolution was fed back; the caller settles every
    /// outstanding requirement of the batch as a never-resume through the
    /// usual cancel path.
    Preempted,
}

/// Fulfills a batch like [`fulfill_batch`], but lets cancellation pre-empt the
/// batch-level wait (M3-3).
///
/// The batch future races the run's cancellation token. A cancel that lands
/// mid-batch does not detach in-flight fulfill futures on the spot: the batch
/// keeps being polled for up to [`CANCEL_UNWIND_GRACE`] so cooperative
/// handlers can observe the token and run their cleanup tails (plain drop was
/// evaluated and rejected — a detached background join was not an option
/// because the batch future borrows the scope stack and so is not `'static`;
/// the M3-2 external session sweep needed no such join: since M3-R the facade
/// drive spawns the sweep itself as a detached `'static` task). When the
/// grace expires with futures still in flight, the batch future is dropped,
/// detaching them.
///
/// The detach semantics place a contract on handlers: a fulfill future may be
/// dropped at any await point once the run is cancelled and the grace has
/// expired, so long-running handlers **must** select on
/// [`RunContext::cancellation`] themselves (facade tools see the same token as
/// [`ToolContext::cancel`](crate::facade::ToolContext::cancel)) instead of
/// relying on running to completion.
pub(crate) async fn fulfill_batch_cancellable(
    requirements: &[Requirement],
    scope: &dyn HandlerScope,
    parent: Option<&mut (dyn Pop + '_)>,
    ctx: &RunContext,
) -> Result<BatchOutcome, AgentError> {
    let batch = fulfill_batch(requirements, scope, parent, ctx);
    tokio::pin!(batch);
    tokio::select! {
        biased;
        result = &mut batch => Ok(BatchOutcome::Completed(result?)),
        () = ctx.cancellation().cancelled() => {
            match tokio::time::timeout(CANCEL_UNWIND_GRACE, &mut batch).await {
                // Every in-flight handler unwound inside the grace: the batch
                // completed, and the caller's post-batch cancel re-check
                // settles the resolutions as never-resumed.
                Ok(result) => Ok(BatchOutcome::Completed(result?)),
                // The grace expired with futures still in flight: drop the
                // batch future, detaching them (see the fn docs).
                Err(_elapsed) => Ok(BatchOutcome::Preempted),
            }
        }
    }
}

#[cfg(test)]
mod tests;
