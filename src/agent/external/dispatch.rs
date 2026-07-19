//! Cost-aware / capability-aware task dispatcher for the mixed-agent scheduler.
//!
//! Design §9 splits scheduling into two layers so the common case stays cheap:
//!
//! - A deterministic **rule router** ([`RuleRouter`]) handles obvious tasks with
//!   no model call — a clear, low-risk, single-file search goes straight to the
//!   cheapest capable worker, an architectural change goes to the strongest.
//! - A pluggable **evaluator** ([`TaskEvaluator`]) is consulted only for the
//!   ambiguous / high-uncertainty middle the rules deliberately decline. The
//!   real product plugs an LLM behind this trait; tests use
//!   [`ScriptedTaskEvaluator`].
//!
//! The [`Dispatcher`] wires the two together and layers budget awareness on top
//! (design §9 "budget 接近上限 -> 降级"): when the shared [`RunContext`] budget is
//! near exhaustion it downgrades to the cheapest capable worker instead of the
//! router's or evaluator's pricier pick, and it charges the run for the
//! (potentially model-backed) evaluator call.
//!
//! Crucially the dispatcher does **not** introduce a new orchestration runtime.
//! Its output is a [`WorkerChoice`], and [`WorkerChoice::into_subagent`] turns
//! that choice into a [`RequirementKind::NeedSubagent`] so the chosen worker is
//! derived and driven through the *existing* subagent path
//! ([`SubagentHandler`](crate::agent::SubagentHandler)).

use crate::agent::{
    context::{BudgetDimension, BudgetError, RunContext, RunContextError},
    external::profile::{Capability, WorkerProfile, WorkerProfileRef, WorkerProfileRegistry},
    interaction::Interaction,
    permission::PermissionRisk,
    requirement::{AgentSpecRef, RequirementKind},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use thiserror::Error;

/// How far a task's edits reach (design §9 "影响范围").
///
/// Ordered smallest-to-largest so a router can compare blast radius: a
/// single-file tweak is cheap to hand to a small worker, an architectural change
/// warrants a strong one.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ImpactScope {
    /// A change contained to a single file.
    #[default]
    SingleFile,
    /// A change spanning several files.
    MultiFile,
    /// A change crossing module boundaries.
    CrossModule,
    /// A change to the architecture / public contracts.
    Architectural,
}

/// How well-specified a task is (design §9 "不确定性").
///
/// Ordered clearest-to-murkiest. [`Ambiguous`](Self::Ambiguous) tasks are never
/// rule-routed; the router defers them to the [`TaskEvaluator`].
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Uncertainty {
    /// The requirement is explicit with a known execution path.
    #[default]
    Clear,
    /// Some exploration is needed but the goal is understood.
    Exploratory,
    /// The requirement is unclear, or the reproduction path is unknown.
    Ambiguous,
}

/// The scheduling bias a caller expresses for a task (design §9 "用户偏好").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostPreference {
    /// No explicit bias; rules decide purely on task shape.
    #[default]
    Balanced,
    /// Prefer the cheapest acceptable worker.
    CostFirst,
    /// Prefer whichever worker turns the task around fastest.
    SpeedFirst,
    /// Prefer the strongest worker for quality-sensitive work.
    QualityFirst,
}

/// Provider-neutral description of a task to be dispatched (design §9 table).
///
/// A `TaskDescriptor` captures the scheduling-relevant dimensions of a unit of
/// work — its [`Capability`] class, [`ImpactScope`], risk (reusing
/// [`PermissionRisk`] as the blast-radius scale), [`Uncertainty`], and the
/// caller's [`CostPreference`]. The live budget is *not* part of the descriptor:
/// the [`Dispatcher`] reads it from the [`RunContext`] at dispatch time so a
/// single descriptor can be re-dispatched as budget changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDescriptor {
    task_type: Capability,
    #[serde(default)]
    impact: ImpactScope,
    risk: PermissionRisk,
    #[serde(default)]
    uncertainty: Uncertainty,
    #[serde(default)]
    preference: CostPreference,
}

impl TaskDescriptor {
    /// Creates a task descriptor from its scheduling dimensions.
    ///
    /// The [`CostPreference`] defaults to [`CostPreference::Balanced`]; use
    /// [`with_preference`](Self::with_preference) to override it.
    #[must_use]
    pub fn new(
        task_type: Capability,
        impact: ImpactScope,
        risk: PermissionRisk,
        uncertainty: Uncertainty,
    ) -> Self {
        Self {
            task_type,
            impact,
            risk,
            uncertainty,
            preference: CostPreference::Balanced,
        }
    }

    /// Returns this descriptor with `preference` applied.
    #[must_use]
    pub fn with_preference(mut self, preference: CostPreference) -> Self {
        self.preference = preference;
        self
    }

    /// Returns the capability class the task requires.
    #[must_use]
    pub fn task_type(&self) -> &Capability {
        &self.task_type
    }

    /// Returns how far the task's edits reach.
    #[must_use]
    pub const fn impact(&self) -> ImpactScope {
        self.impact
    }

    /// Returns the task's risk (blast radius) level.
    #[must_use]
    pub const fn risk(&self) -> PermissionRisk {
        self.risk
    }

    /// Returns how well-specified the task is.
    #[must_use]
    pub const fn uncertainty(&self) -> Uncertainty {
        self.uncertainty
    }

    /// Returns the caller's scheduling bias for the task.
    #[must_use]
    pub const fn preference(&self) -> CostPreference {
        self.preference
    }

    /// Returns `true` when the task is high-risk or complex enough to warrant an
    /// independent verifier pass after a worker runs (design §9 "验证/升级").
    ///
    /// The escalation engine ([`Escalator`](super::escalation::Escalator)) only
    /// consults its [`Verifier`](super::escalation::Verifier) for tasks matching
    /// this predicate — high risk (`>= High`), a cross-module-or-wider blast
    /// radius, or an ambiguous requirement — so cheap, obviously-safe work is not
    /// gated behind a review pass. A worker's own reported triggers still drive
    /// escalation regardless of this predicate.
    #[must_use]
    pub fn warrants_verification(&self) -> bool {
        self.risk >= PermissionRisk::High
            || self.impact >= ImpactScope::CrossModule
            || self.uncertainty == Uncertainty::Ambiguous
    }
}

/// One dispatchable worker: its scheduling [`WorkerProfileRef`] paired with the
/// [`AgentSpecRef`] to derive when the worker is chosen.
///
/// The profile ref resolves (through a [`WorkerProfileRegistry`]) to capability
/// and cost data used for routing; the spec ref is what a
/// [`RequirementKind::NeedSubagent`] derives so the worker runs on the existing
/// subagent path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    profile: WorkerProfileRef,
    spec: AgentSpecRef,
}

impl Worker {
    /// Binds a worker profile reference to the subagent spec it derives.
    #[must_use]
    pub const fn new(profile: WorkerProfileRef, spec: AgentSpecRef) -> Self {
        Self { profile, spec }
    }

    /// Returns the worker's scheduling profile reference.
    #[must_use]
    pub const fn profile(&self) -> &WorkerProfileRef {
        &self.profile
    }

    /// Returns the subagent spec reference this worker derives.
    #[must_use]
    pub const fn spec(&self) -> &AgentSpecRef {
        &self.spec
    }
}

/// The set of workers a [`Dispatcher`] may choose between.
///
/// A roster owns a [`WorkerProfileRegistry`] (the capability / cost data) plus
/// the [`Worker`] bindings that map each profile to a concrete subagent spec.
/// Selection helpers ([`cheapest_capable`](Self::cheapest_capable) /
/// [`strongest_capable`](Self::strongest_capable)) resolve a task's required
/// [`Capability`] to a worker, breaking cost-tier ties by profile id so the
/// choice is deterministic.
#[derive(Clone, Debug, Default)]
pub struct WorkerRoster {
    registry: WorkerProfileRegistry,
    workers: Vec<Worker>,
}

impl WorkerRoster {
    /// Creates an empty roster.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `profile` and binds it to `spec`, returning the profile ref.
    ///
    /// If a worker with the same profile id is already registered its profile
    /// and spec binding are both replaced, so the returned ref always resolves
    /// to the newly registered worker.
    pub fn register(&mut self, profile: WorkerProfile, spec: AgentSpecRef) -> WorkerProfileRef {
        let reference = self.registry.register(profile);
        self.workers.retain(|worker| worker.profile != reference);
        self.workers.push(Worker::new(reference.clone(), spec));
        reference
    }

    /// Returns the backing worker-profile registry.
    #[must_use]
    pub const fn registry(&self) -> &WorkerProfileRegistry {
        &self.registry
    }

    /// Returns the registered worker bindings.
    #[must_use]
    pub fn workers(&self) -> &[Worker] {
        &self.workers
    }

    /// Resolves a worker binding by its profile reference.
    #[must_use]
    pub fn resolve_worker(&self, reference: &WorkerProfileRef) -> Option<&Worker> {
        self.workers
            .iter()
            .find(|worker| &worker.profile == reference)
    }

    /// Resolves the full [`WorkerProfile`] behind a reference.
    #[must_use]
    pub fn profile(&self, reference: &WorkerProfileRef) -> Option<&WorkerProfile> {
        self.registry.resolve(reference)
    }

    /// Returns the cheapest worker that advertises `capability`, or `None` when
    /// no registered worker is capable.
    ///
    /// Ties on [`CostTier`](crate::agent::CostTier) are broken by ascending
    /// profile id for a deterministic result.
    #[must_use]
    pub fn cheapest_capable(&self, capability: &Capability) -> Option<WorkerProfileRef> {
        self.capable(capability)
            .min_by(|(a_worker, a_profile), (b_worker, b_profile)| {
                a_profile
                    .cost_tier()
                    .cmp(&b_profile.cost_tier())
                    .then_with(|| a_worker.profile.id().cmp(b_worker.profile.id()))
            })
            .map(|(worker, _)| worker.profile.clone())
    }

    /// Returns the strongest (most expensive) worker that advertises
    /// `capability`, or `None` when no registered worker is capable.
    ///
    /// Ties on [`CostTier`](crate::agent::CostTier) are broken by ascending
    /// profile id for a deterministic result.
    #[must_use]
    pub fn strongest_capable(&self, capability: &Capability) -> Option<WorkerProfileRef> {
        self.capable(capability)
            .max_by(|(a_worker, a_profile), (b_worker, b_profile)| {
                a_profile
                    .cost_tier()
                    .cmp(&b_profile.cost_tier())
                    .then_with(|| b_worker.profile.id().cmp(a_worker.profile.id()))
            })
            .map(|(worker, _)| worker.profile.clone())
    }

    /// Iterates workers advertising `capability`, paired with their profiles.
    fn capable<'a>(
        &'a self,
        capability: &'a Capability,
    ) -> impl Iterator<Item = (&'a Worker, &'a WorkerProfile)> {
        self.workers.iter().filter_map(move |worker| {
            let profile = self.registry.resolve(&worker.profile)?;
            profile
                .has_capability(capability)
                .then_some((worker, profile))
        })
    }
}

/// Why the [`Dispatcher`] settled on a particular [`WorkerChoice`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchReason {
    /// The deterministic [`RuleRouter`] matched the task.
    RuleRoute,
    /// The [`TaskEvaluator`] decided after the rules declined.
    Evaluator,
    /// Budget was near exhaustion, forcing a downgrade to the cheapest worker.
    BudgetDowngrade,
    /// A worker failed / self-reported low confidence / was review-rejected,
    /// forcing an escalation to a stronger worker (design §9, Milestone 6-4).
    Escalation,
}

/// The dispatcher's decision: which worker to run and why.
///
/// A `WorkerChoice` is deliberately inert. It names the chosen worker profile
/// and the [`AgentSpecRef`] to derive; [`into_subagent`](Self::into_subagent)
/// converts it into a [`RequirementKind::NeedSubagent`] so the caller derives the
/// worker through the existing subagent path rather than any new runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerChoice {
    worker: WorkerProfileRef,
    spec: AgentSpecRef,
    reason: DispatchReason,
}

impl WorkerChoice {
    /// Creates a worker choice from the chosen worker, its spec, and the reason.
    #[must_use]
    pub const fn new(worker: WorkerProfileRef, spec: AgentSpecRef, reason: DispatchReason) -> Self {
        Self {
            worker,
            spec,
            reason,
        }
    }

    /// Returns the chosen worker's profile reference.
    #[must_use]
    pub const fn worker(&self) -> &WorkerProfileRef {
        &self.worker
    }

    /// Returns the subagent spec reference to derive for the chosen worker.
    #[must_use]
    pub const fn spec(&self) -> &AgentSpecRef {
        &self.spec
    }

    /// Returns why this choice was made.
    #[must_use]
    pub const fn reason(&self) -> DispatchReason {
        self.reason
    }

    /// Converts this choice into a [`RequirementKind::NeedSubagent`] that derives
    /// the chosen worker through the existing subagent path.
    ///
    /// `brief` is presented to the child worker as its opening interaction and
    /// `result_schema` optionally constrains the worker's structured result.
    #[must_use]
    pub fn into_subagent(
        self,
        brief: Interaction,
        result_schema: Option<Value>,
    ) -> RequirementKind {
        RequirementKind::NeedSubagent {
            spec_ref: self.spec,
            brief,
            result_schema,
        }
    }
}

/// Deterministic, model-free first layer of the dispatcher (design §9 规则路由).
///
/// The router matches a [`TaskDescriptor`] against a small ordered rule set and
/// returns the chosen worker's [`WorkerProfileRef`], or `None` when the task is
/// ambiguous / moderate enough that it should be left to the [`TaskEvaluator`].
/// It is intentionally cheap and side-effect free.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuleRouter {
    _private: (),
}

impl RuleRouter {
    /// Creates the default rule router.
    #[must_use]
    pub const fn new() -> Self {
        Self { _private: () }
    }

    /// Routes `task` against `roster`, returning the chosen worker or `None` when
    /// the decision should defer to a [`TaskEvaluator`].
    ///
    /// Rules are applied in order, first match wins:
    ///
    /// 1. Ambiguous tasks are never rule-routed.
    /// 2. Architectural, high-risk, or quality-first cross-module work goes to
    ///    the strongest capable worker.
    /// 3. Clear, low-risk, contained work — or an explicit cost-first preference
    ///    on non-high-risk work — goes to the cheapest capable worker.
    /// 4. Everything else defers to the evaluator.
    #[must_use]
    pub fn route(&self, task: &TaskDescriptor, roster: &WorkerRoster) -> Option<WorkerProfileRef> {
        if task.uncertainty == Uncertainty::Ambiguous {
            return None;
        }

        let heavy = task.impact == ImpactScope::Architectural
            || task.risk >= PermissionRisk::High
            || (task.preference == CostPreference::QualityFirst
                && task.impact >= ImpactScope::CrossModule);
        if heavy {
            return roster.strongest_capable(&task.task_type);
        }

        let clearly_light = task.uncertainty == Uncertainty::Clear
            && task.risk <= PermissionRisk::Low
            && task.impact <= ImpactScope::MultiFile;
        let cost_first =
            task.preference == CostPreference::CostFirst && task.risk <= PermissionRisk::Medium;
        if clearly_light || cost_first {
            return roster.cheapest_capable(&task.task_type);
        }

        None
    }
}

/// Pluggable second layer consulted for tasks the [`RuleRouter`] declines
/// (design §9 LLM evaluator).
///
/// The production evaluator prompts a model to weigh the [`TaskDescriptor`]
/// against the [`WorkerRoster`]; that implementation lives behind this trait so
/// the [`Dispatcher`] stays independent of any provider. Tests supply a
/// [`ScriptedTaskEvaluator`]. An implementation returns the chosen worker's
/// [`WorkerProfileRef`], or `None` to decline (yielding
/// [`DispatchError::NoWorker`]).
pub trait TaskEvaluator {
    /// Evaluates `task` against `roster`, returning the chosen worker or `None`.
    fn evaluate(&self, task: &TaskDescriptor, roster: &WorkerRoster) -> Option<WorkerProfileRef>;
}

/// Boxed decision closure backing a [`ScriptedTaskEvaluator`].
type EvaluatorFn =
    Box<dyn Fn(&TaskDescriptor, &WorkerRoster) -> Option<WorkerProfileRef> + Send + Sync>;

/// A [`TaskEvaluator`] backed by a caller-supplied closure.
///
/// Useful both in tests (to script deterministic decisions) and for hosts that
/// want a fixed policy without wiring up a model. A real LLM evaluator would
/// implement [`TaskEvaluator`] directly instead of using this shim.
pub struct ScriptedTaskEvaluator {
    decide: EvaluatorFn,
}

impl ScriptedTaskEvaluator {
    /// Creates a scripted evaluator from a decision closure.
    #[must_use]
    pub fn new(
        decide: impl Fn(&TaskDescriptor, &WorkerRoster) -> Option<WorkerProfileRef>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            decide: Box::new(decide),
        }
    }

    /// Creates a scripted evaluator that always chooses `worker`.
    #[must_use]
    pub fn always(worker: WorkerProfileRef) -> Self {
        Self::new(move |_, _| Some(worker.clone()))
    }
}

impl fmt::Debug for ScriptedTaskEvaluator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ScriptedTaskEvaluator")
    }
}

impl TaskEvaluator for ScriptedTaskEvaluator {
    fn evaluate(&self, task: &TaskDescriptor, roster: &WorkerRoster) -> Option<WorkerProfileRef> {
        (self.decide)(task, roster)
    }
}

/// Default budget headroom, in percent, below which the dispatcher downgrades.
const DEFAULT_MIN_BUDGET_HEADROOM_PERCENT: u8 = 20;

/// Two-layer, budget-aware task dispatcher (design §9).
///
/// A `Dispatcher` runs the [`RuleRouter`] first and falls back to a
/// [`TaskEvaluator`] for the ambiguous middle, all while respecting the shared
/// [`RunContext`] budget: when budget headroom drops below
/// [`min_budget_headroom_percent`](Self::with_budget_headroom) it downgrades to
/// the cheapest capable worker, and it charges the run one step for each
/// (potentially model-backed) evaluator consultation.
#[derive(Debug)]
pub struct Dispatcher<E: TaskEvaluator> {
    router: RuleRouter,
    evaluator: E,
    min_budget_headroom_percent: u8,
}

impl<E: TaskEvaluator> Dispatcher<E> {
    /// Creates a dispatcher with the default [`RuleRouter`] and budget headroom.
    #[must_use]
    pub fn new(evaluator: E) -> Self {
        Self::with_router(RuleRouter::new(), evaluator)
    }

    /// Creates a dispatcher with an explicit router and default budget headroom.
    #[must_use]
    pub fn with_router(router: RuleRouter, evaluator: E) -> Self {
        Self {
            router,
            evaluator,
            min_budget_headroom_percent: DEFAULT_MIN_BUDGET_HEADROOM_PERCENT,
        }
    }

    /// Sets the budget headroom, in percent, below which the dispatcher
    /// downgrades to the cheapest capable worker.
    ///
    /// A value of `0` disables budget-based downgrade entirely.
    #[must_use]
    pub fn with_budget_headroom(mut self, min_percent: u8) -> Self {
        self.min_budget_headroom_percent = min_percent;
        self
    }

    /// Dispatches `task` to a worker drawn from `roster`, honoring the `ctx`
    /// budget.
    ///
    /// The decision order is: cancellation check, exhausted-budget hard stop,
    /// budget downgrade, rule route, then evaluator fallback (charged against the budget). See
    /// [`DispatchReason`] for how the result is classified.
    ///
    /// # Errors
    ///
    /// Returns [`DispatchError::Context`] when the run is cancelled;
    /// [`DispatchError::BudgetExhausted`] when the run budget has no headroom;
    /// [`DispatchError::NoCapableWorker`] when a downgrade finds no capable
    /// worker; [`DispatchError::UnknownWorker`] when the evaluator names a worker
    /// absent from the roster; and [`DispatchError::NoWorker`] when neither the
    /// rules nor the evaluator select a worker.
    pub fn dispatch(
        &self,
        task: &TaskDescriptor,
        roster: &WorkerRoster,
        ctx: &RunContext,
    ) -> Result<WorkerChoice, DispatchError> {
        ctx.check_cancelled()?;

        if let Some(dimension) = ctx.budget_exhausted() {
            return Err(DispatchError::BudgetExhausted { dimension });
        }

        if budget_is_low(ctx, self.min_budget_headroom_percent) {
            return self.downgrade(task, roster);
        }

        if let Some(worker) = self.router.route(task, roster) {
            return self.finish(worker, roster, DispatchReason::RuleRoute);
        }

        // The evaluator is the expensive path (an LLM call in production); charge
        // the run for it. If that charge itself exhausts the budget, stop before
        // dispatching any worker.
        match ctx.charge_step() {
            Ok(_) => {}
            Err(RunContextError::Budget(error)) => {
                return Err(DispatchError::BudgetExhausted {
                    dimension: budget_error_dimension(&error),
                });
            }
            Err(other) => return Err(DispatchError::Context(other)),
        }

        match self.evaluator.evaluate(task, roster) {
            Some(worker) => self.finish(worker, roster, DispatchReason::Evaluator),
            None => Err(DispatchError::NoWorker),
        }
    }

    /// Selects the cheapest capable worker as a budget downgrade.
    fn downgrade(
        &self,
        task: &TaskDescriptor,
        roster: &WorkerRoster,
    ) -> Result<WorkerChoice, DispatchError> {
        let worker = roster.cheapest_capable(task.task_type()).ok_or_else(|| {
            DispatchError::NoCapableWorker {
                capability: task.task_type().clone(),
            }
        })?;
        self.finish(worker, roster, DispatchReason::BudgetDowngrade)
    }

    /// Resolves the chosen worker's spec and packages the final [`WorkerChoice`].
    fn finish(
        &self,
        worker: WorkerProfileRef,
        roster: &WorkerRoster,
        reason: DispatchReason,
    ) -> Result<WorkerChoice, DispatchError> {
        let entry = roster
            .resolve_worker(&worker)
            .ok_or_else(|| DispatchError::UnknownWorker {
                worker: worker.clone(),
            })?;
        Ok(WorkerChoice::new(worker, *entry.spec(), reason))
    }
}

fn budget_error_dimension(error: &BudgetError) -> BudgetDimension {
    match error {
        BudgetError::Exceeded { dimension, .. } | BudgetError::CounterOverflow { dimension } => {
            *dimension
        }
        BudgetError::WallClockExceeded { .. } => BudgetDimension::WallClock,
    }
}

/// Returns `true` when any configured budget dimension has less than
/// `min_headroom_percent` of its limit remaining.
///
/// Unbounded dimensions never count as low, so a run with no limits is never
/// downgraded. A `min_headroom_percent` of `0` disables the check. Shared with
/// the escalation engine ([`Escalator`](super::escalation::Escalator)) so budget
/// pressure is judged identically at dispatch and re-dispatch time.
pub(super) fn budget_is_low(ctx: &RunContext, min_headroom_percent: u8) -> bool {
    if min_headroom_percent == 0 {
        return false;
    }

    let snapshot = ctx.budget().snapshot();
    let limits = snapshot.limits();
    let used = snapshot.used();
    let headroom = u128::from(min_headroom_percent);

    let dimension_low = |limit: Option<u64>, used: u64| -> bool {
        match limit {
            Some(limit) => {
                let remaining = u128::from(limit.saturating_sub(used));
                // remaining < headroom% of limit  <=>  remaining*100 < limit*headroom
                remaining.saturating_mul(100) < u128::from(limit).saturating_mul(headroom)
            }
            None => false,
        }
    };

    dimension_low(limits.max_steps(), used.steps())
        || dimension_low(limits.max_tokens(), used.tokens())
        || dimension_low(limits.max_cost_micros(), used.cost_micros())
}

/// Why a dispatch could not produce a [`WorkerChoice`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum DispatchError {
    /// No registered worker advertises the task's required capability.
    #[error("no registered worker advertises capability {capability:?}")]
    NoCapableWorker {
        /// The capability that could not be satisfied.
        capability: Capability,
    },
    /// Neither the rule router nor the evaluator selected a worker.
    #[error("no worker selected by rules or evaluator")]
    NoWorker,
    /// The evaluator named a worker that is not registered in the roster.
    #[error("evaluator selected unregistered worker {worker:?}")]
    UnknownWorker {
        /// The unresolved worker reference.
        worker: WorkerProfileRef,
    },
    /// The run budget has no remaining headroom, so no worker is dispatched.
    #[error("{dimension:?} budget exhausted; no worker dispatched")]
    BudgetExhausted {
        /// Dimension whose headroom was exhausted.
        dimension: BudgetDimension,
    },
    /// A run-context operation (cancellation or budget) failed.
    #[error(transparent)]
    Context(#[from] RunContextError),
}

#[cfg(test)]
mod tests;
