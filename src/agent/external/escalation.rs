//! Cheap → strong escalation and a verifier hook layered over the dispatcher.
//!
//! Design §9 "升级规则" turns a worker's *result* into the scheduler's next move:
//!
//! - A cheap worker that times out or fails its tests → a stronger worker.
//! - A worker that self-reports low confidence → re-dispatched upward.
//! - A reviewer that finds an architecture / security problem → a stronger
//!   worker or a human gate.
//! - Budget near exhaustion → downgrade to a cheaper worker, or stop and ask the
//!   user when nothing cheaper remains.
//!
//! This module implements that decision on top of the Milestone 6-2
//! [`Dispatcher`](super::Dispatcher) primitives. It deliberately introduces **no
//! new orchestration runtime**: an escalation that re-dispatches yields a
//! [`WorkerChoice`], and [`WorkerChoice::into_subagent`](super::WorkerChoice::into_subagent)
//! turns that into a [`RequirementKind::NeedSubagent`](crate::agent::RequirementKind::NeedSubagent)
//! so the replacement worker runs through the existing subagent path. An
//! escalation that reaches a human yields an [`Interaction`] — a
//! [`Permission`](crate::agent::InteractionKind::Permission) request for a
//! review / security decision, or a [`Question`](crate::agent::InteractionKind::Question)
//! to stop and ask the user.
//!
//! The [`Verifier`] hook is the "验证" half of design §9's "验证/升级" column: a
//! review-agent or test pass consulted **after** a worker runs (gated on
//! [`TaskDescriptor::warrants_verification`](super::TaskDescriptor::warrants_verification))
//! whose verdict feeds the same escalation decision.

use crate::agent::{
    Capability, EscalationTrigger, WorkerProfile, WorkerProfileRef,
    context::{RunContext, RunContextError},
    id::{AgentId, StepId},
    interaction::Interaction,
    permission::{PermissionCategory, PermissionRequest},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt;
use thiserror::Error;

use super::dispatch::{DispatchReason, TaskDescriptor, WorkerChoice, WorkerRoster, budget_is_low};

/// Default budget headroom, in percent, below which escalation downgrades
/// instead of upgrading — kept in step with the [`Dispatcher`](super::Dispatcher)
/// default so budget pressure is judged identically at dispatch and re-dispatch.
const DEFAULT_MIN_BUDGET_HEADROOM_PERCENT: u8 = 20;

/// The observed outcome of running a worker, feeding the escalation decision.
///
/// A report names the [`WorkerProfileRef`] that ran and the
/// [`EscalationTrigger`]s its run raised. A clean run raises none; a failing or
/// low-confidence run raises one or more. The worker's *self-reported* triggers
/// are always honored by [`Escalator::assess`]; an independent [`Verifier`] may
/// add more.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerReport {
    worker: WorkerProfileRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    triggers: Vec<EscalationTrigger>,
}

impl WorkerReport {
    /// Reports a clean run of `worker` that raised no escalation trigger.
    #[must_use]
    pub fn succeeded(worker: WorkerProfileRef) -> Self {
        Self {
            worker,
            triggers: Vec::new(),
        }
    }

    /// Reports a run of `worker` that raised a single `trigger`.
    #[must_use]
    pub fn failed(worker: WorkerProfileRef, trigger: EscalationTrigger) -> Self {
        Self {
            worker,
            triggers: vec![trigger],
        }
    }

    /// Reports a run of `worker` that raised the given `triggers`, de-duplicated
    /// while preserving first-seen order.
    #[must_use]
    pub fn new(
        worker: WorkerProfileRef,
        triggers: impl IntoIterator<Item = EscalationTrigger>,
    ) -> Self {
        let mut report = Self::succeeded(worker);
        for trigger in triggers {
            report = report.with_trigger(trigger);
        }
        report
    }

    /// Returns this report with `trigger` added if not already present.
    #[must_use]
    pub fn with_trigger(mut self, trigger: EscalationTrigger) -> Self {
        if !self.triggers.contains(&trigger) {
            self.triggers.push(trigger);
        }
        self
    }

    /// Returns the worker that produced this report.
    #[must_use]
    pub const fn worker(&self) -> &WorkerProfileRef {
        &self.worker
    }

    /// Returns the escalation triggers this run raised.
    #[must_use]
    pub fn triggers(&self) -> &[EscalationTrigger] {
        &self.triggers
    }

    /// Returns `true` when the run raised no escalation trigger.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.triggers.is_empty()
    }

    /// Returns `true` when the run raised `trigger`.
    #[must_use]
    pub fn raised(&self, trigger: EscalationTrigger) -> bool {
        self.triggers.contains(&trigger)
    }
}

/// A post-run verification hook (design §9 "验证/升级": review-agent / tests).
///
/// A verifier is consulted by [`Escalator::assess`] **after** a worker runs, and
/// only for tasks where
/// [`TaskDescriptor::warrants_verification`](super::TaskDescriptor::warrants_verification)
/// holds. It returns the [`EscalationTrigger`] to raise when verification fails
/// (for example [`TestFailure`](EscalationTrigger::TestFailure) from a test pass
/// or [`ReviewRejected`](EscalationTrigger::ReviewRejected) from a review-agent),
/// or `None` when the worker's output passes.
///
/// The production hook wraps a review sub-agent or a test runner; tests supply a
/// [`ScriptedVerifier`].
pub trait Verifier {
    /// Verifies `report`'s worker output for `task`, returning the trigger to
    /// raise on failure or `None` when the output passes.
    fn verify(&self, task: &TaskDescriptor, report: &WorkerReport) -> Option<EscalationTrigger>;
}

/// Boxed verification closure backing a [`ScriptedVerifier`].
type VerifierFn =
    Box<dyn Fn(&TaskDescriptor, &WorkerReport) -> Option<EscalationTrigger> + Send + Sync>;

/// A [`Verifier`] backed by a caller-supplied closure.
///
/// Useful in tests (to script a deterministic verdict) and for hosts that want a
/// fixed verification policy without wiring up a review sub-agent. A real
/// review-agent / test-runner verifier implements [`Verifier`] directly.
pub struct ScriptedVerifier {
    check: VerifierFn,
}

impl ScriptedVerifier {
    /// Creates a scripted verifier from a verdict closure.
    #[must_use]
    pub fn new(
        check: impl Fn(&TaskDescriptor, &WorkerReport) -> Option<EscalationTrigger>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            check: Box::new(check),
        }
    }

    /// Creates a verifier that always passes (raises no trigger).
    ///
    /// Pass this to [`Escalator::new`] to run escalation purely off a worker's
    /// self-reported [`WorkerReport`] triggers, with no independent verification.
    #[must_use]
    pub fn passing() -> Self {
        Self::new(|_, _| None)
    }

    /// Creates a verifier that always fails with `trigger`.
    #[must_use]
    pub fn rejecting(trigger: EscalationTrigger) -> Self {
        Self::new(move |_, _| Some(trigger))
    }
}

impl fmt::Debug for ScriptedVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ScriptedVerifier")
    }
}

impl Verifier for ScriptedVerifier {
    fn verify(&self, task: &TaskDescriptor, report: &WorkerReport) -> Option<EscalationTrigger> {
        (self.check)(task, report)
    }
}

/// Identity used to build a human-gate [`Interaction`] when escalation reaches a
/// person (design §9 "或 human" / "停机问用户").
///
/// A gate carries the [`StepId`] awaiting the human decision and the
/// [`AgentId`] recorded as the requesting actor on a
/// [`PermissionRequest`]. Both are supplied by the host, never by a model, so an
/// escalation cannot forge who is asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HumanGate {
    step: StepId,
    actor: AgentId,
}

impl HumanGate {
    /// Creates a human gate awaiting a decision at `step`, asked as `actor`.
    #[must_use]
    pub const fn new(step: StepId, actor: AgentId) -> Self {
        Self { step, actor }
    }

    /// Returns the step awaiting the human decision.
    #[must_use]
    pub const fn step(&self) -> StepId {
        self.step
    }

    /// Returns the actor recorded on a raised permission request.
    #[must_use]
    pub const fn actor(&self) -> AgentId {
        self.actor
    }
}

/// The escalation engine's decision after weighing a [`WorkerReport`].
///
/// The outcome is deliberately inert: a [`Reassign`](Self::Reassign) names a
/// replacement [`WorkerChoice`] the caller derives through the existing subagent
/// path, and a [`Human`](Self::Human) names an [`Interaction`] the caller
/// surfaces through the usual interaction machinery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EscalationOutcome {
    /// The worker's result stands; no escalation is warranted.
    Accept,
    /// Re-dispatch to a different worker — a stronger upgrade
    /// ([`Escalation`](DispatchReason::Escalation)) or a budget downgrade
    /// ([`BudgetDowngrade`](DispatchReason::BudgetDowngrade)).
    Reassign(WorkerChoice),
    /// Hand off to a human decision gate: a permission request for a review /
    /// security decision, or a question to stop and ask the user.
    Human(Interaction),
    /// Escalation is warranted but the roster offers no eligible target and the
    /// worker's profile does not opt into a human fallback; the caller must
    /// decide how to handle the terminal failure.
    Exhausted {
        /// The trigger that could not be escalated.
        trigger: EscalationTrigger,
    },
}

/// Cheap → strong escalation engine with an optional verifier hook (design §9).
///
/// An `Escalator` reads a [`WorkerReport`] (and, for warranting tasks, a
/// [`Verifier`] verdict) and decides the scheduler's next move via
/// [`assess`](Self::assess). Budget pressure — a
/// [`BudgetExhausted`](EscalationTrigger::BudgetExhausted) trigger or a
/// [`RunContext`] whose headroom has dropped below
/// [`with_budget_headroom`](Self::with_budget_headroom) — always takes
/// precedence over an upward escalation, so the engine never upgrades to a
/// pricier worker when the budget is nearly spent.
#[derive(Debug)]
pub struct Escalator<V: Verifier> {
    verifier: V,
    min_budget_headroom_percent: u8,
}

impl<V: Verifier> Escalator<V> {
    /// Creates an escalator using `verifier` and the default budget headroom.
    ///
    /// Pass [`ScriptedVerifier::passing`] to escalate purely off a worker's
    /// self-reported [`WorkerReport`] triggers with no independent verification.
    #[must_use]
    pub fn new(verifier: V) -> Self {
        Self {
            verifier,
            min_budget_headroom_percent: DEFAULT_MIN_BUDGET_HEADROOM_PERCENT,
        }
    }

    /// Sets the budget headroom, in percent, below which escalation downgrades
    /// to a cheaper worker instead of upgrading.
    ///
    /// A value of `0` disables budget-pressure downgrade, so escalation is driven
    /// only by an explicit [`BudgetExhausted`](EscalationTrigger::BudgetExhausted)
    /// trigger.
    #[must_use]
    pub fn with_budget_headroom(mut self, min_percent: u8) -> Self {
        self.min_budget_headroom_percent = min_percent;
        self
    }

    /// Decides the scheduler's next move for `report` running `task`.
    ///
    /// The decision order is: cancellation check, gather effective triggers
    /// (report triggers plus, for a warranting task, the [`Verifier`] verdict),
    /// then — if any trigger fired — budget pressure first (downgrade or stop),
    /// otherwise an upward escalation (stronger worker or human gate). A clean
    /// report yields [`EscalationOutcome::Accept`].
    ///
    /// # Errors
    ///
    /// Returns [`EscalationError::Context`] when the run is cancelled, and
    /// [`EscalationError::UnknownWorker`] when the report's worker (or a resolved
    /// escalation target) is absent from `roster`.
    pub fn assess(
        &self,
        task: &TaskDescriptor,
        report: &WorkerReport,
        roster: &WorkerRoster,
        ctx: &RunContext,
        gate: &HumanGate,
    ) -> Result<EscalationOutcome, EscalationError> {
        ctx.check_cancelled()?;

        let triggers = self.effective_triggers(task, report);
        if triggers.is_empty() {
            return Ok(EscalationOutcome::Accept);
        }

        let current =
            roster
                .profile(report.worker())
                .ok_or_else(|| EscalationError::UnknownWorker {
                    worker: report.worker().clone(),
                })?;
        let capability = task.task_type();

        // Budget pressure overrides an upward escalation (design §9 "budget 接近
        // 上限 -> 降级"): never upgrade to a pricier worker when nearly out of
        // budget.
        let budget_pressure = triggers.contains(&EscalationTrigger::BudgetExhausted)
            || budget_is_low(ctx, self.min_budget_headroom_percent);
        if budget_pressure {
            return self.budget_action(current, roster, capability, gate);
        }

        let trigger = primary_upward(&triggers).expect("a non-budget trigger is present");
        self.upward_action(task, current, trigger, roster, capability, gate)
    }

    /// Merges the report's own triggers with the verifier's verdict for a
    /// warranting task, de-duplicated.
    fn effective_triggers(
        &self,
        task: &TaskDescriptor,
        report: &WorkerReport,
    ) -> Vec<EscalationTrigger> {
        let mut triggers = report.triggers().to_vec();
        if task.warrants_verification()
            && let Some(trigger) = self.verifier.verify(task, report)
            && !triggers.contains(&trigger)
        {
            triggers.push(trigger);
        }
        triggers
    }

    /// Handles budget pressure: downgrade to a strictly cheaper worker, or stop
    /// and ask the user when none is cheaper (design §9 "降级 ... 或停机问用户").
    fn budget_action(
        &self,
        current: &WorkerProfile,
        roster: &WorkerRoster,
        capability: &Capability,
        gate: &HumanGate,
    ) -> Result<EscalationOutcome, EscalationError> {
        let cheaper = roster.cheapest_capable(capability).ok_or_else(|| {
            EscalationError::NoCapableWorker {
                capability: capability.clone(),
            }
        })?;
        let cheaper_tier = roster
            .profile(&cheaper)
            .map(WorkerProfile::cost_tier)
            .expect("cheapest_capable resolves within roster");
        if cheaper_tier < current.cost_tier() {
            return self.reassign(cheaper, roster, DispatchReason::BudgetDowngrade);
        }
        // Already the cheapest capable worker: continuing would blow the budget,
        // so stop and ask the user rather than re-run the same tier.
        Ok(EscalationOutcome::Human(Interaction::question(
            gate.step,
            format!(
                "Budget is exhausted after worker {} and no cheaper worker is available. \
                 Stop and ask the user whether to continue?",
                current.id()
            ),
        )))
    }

    /// Handles an upward escalation for a failure / low-confidence / review
    /// rejection: re-dispatch to a stronger worker, else a human fallback.
    fn upward_action(
        &self,
        task: &TaskDescriptor,
        current: &WorkerProfile,
        trigger: EscalationTrigger,
        roster: &WorkerRoster,
        capability: &Capability,
        gate: &HumanGate,
    ) -> Result<EscalationOutcome, EscalationError> {
        if let Some(target) = upgrade_target(current, trigger, roster, capability) {
            return self.reassign(target, roster, DispatchReason::Escalation);
        }
        if current.escalation().human_fallback() {
            return Ok(EscalationOutcome::Human(human_gate_interaction(
                task, current, trigger, gate,
            )));
        }
        Ok(EscalationOutcome::Exhausted { trigger })
    }

    /// Resolves `worker`'s bound spec and packages a [`Reassign`](EscalationOutcome::Reassign).
    fn reassign(
        &self,
        worker: WorkerProfileRef,
        roster: &WorkerRoster,
        reason: DispatchReason,
    ) -> Result<EscalationOutcome, EscalationError> {
        let entry =
            roster
                .resolve_worker(&worker)
                .ok_or_else(|| EscalationError::UnknownWorker {
                    worker: worker.clone(),
                })?;
        Ok(EscalationOutcome::Reassign(WorkerChoice::new(
            worker,
            *entry.spec(),
            reason,
        )))
    }
}

/// Resolves the worker to escalate *up* to, or `None` when nothing stronger is
/// eligible.
///
/// An explicit [`escalate_to`](crate::agent::EscalationRules::escalate_to)
/// configured on the current worker's profile wins when it triggers on
/// `trigger`, is registered in the roster, advertises `capability`, and is
/// strictly stronger than the current worker. Otherwise the strongest capable
/// worker is chosen, provided it is strictly stronger than the current one.
fn upgrade_target(
    current: &WorkerProfile,
    trigger: EscalationTrigger,
    roster: &WorkerRoster,
    capability: &Capability,
) -> Option<WorkerProfileRef> {
    if current.escalation().triggers_on(trigger)
        && let Some(target) = current.escalation().escalate_to()
        && let Some(profile) = roster.profile(target)
        && profile.has_capability(capability)
        && profile.cost_tier() > current.cost_tier()
        && roster.resolve_worker(target).is_some()
    {
        return Some(target.clone());
    }

    let strongest = roster.strongest_capable(capability)?;
    let stronger = roster
        .profile(&strongest)
        .is_some_and(|profile| profile.cost_tier() > current.cost_tier());
    stronger.then_some(strongest)
}

/// Builds the human-gate [`Interaction`] for an upward escalation that found no
/// stronger worker.
///
/// A [`ReviewRejected`](EscalationTrigger::ReviewRejected) — an architecture /
/// security finding — becomes a [`Permission`](crate::agent::InteractionKind::Permission)
/// request carrying the task's risk (design §9 "review 发现架构/安全问题 -> ...
/// human"); any other trigger becomes an open [`Question`](crate::agent::InteractionKind::Question).
fn human_gate_interaction(
    task: &TaskDescriptor,
    current: &WorkerProfile,
    trigger: EscalationTrigger,
    gate: &HumanGate,
) -> Interaction {
    match trigger {
        EscalationTrigger::ReviewRejected => {
            let request = PermissionRequest::new(
                format!("escalation:{}:{}", gate.step, current.id()),
                gate.actor,
                PermissionCategory::Other,
                format!(
                    "Human review required: worker {} was review-rejected and no \
                     stronger worker is available.",
                    current.id()
                ),
                json!({
                    "worker": current.id(),
                    "trigger": trigger,
                    "task_type": task.task_type(),
                }),
                task.risk(),
                Some(
                    "No stronger worker available; escalating the review decision to a human."
                        .to_owned(),
                ),
            );
            Interaction::permission(gate.step, request)
        }
        other => Interaction::question(
            gate.step,
            format!(
                "Worker {} raised {other:?} and no stronger worker is available. \
                 How should we proceed?",
                current.id()
            ),
        ),
    }
}

/// Picks the most severe non-budget trigger to act on, or `None` when the only
/// trigger present is budget-related (handled separately).
fn primary_upward(triggers: &[EscalationTrigger]) -> Option<EscalationTrigger> {
    // Most-severe first: a review rejection outranks a test failure, which
    // outranks a timeout, which outranks a bare low-confidence self-report.
    const ORDER: [EscalationTrigger; 4] = [
        EscalationTrigger::ReviewRejected,
        EscalationTrigger::TestFailure,
        EscalationTrigger::Timeout,
        EscalationTrigger::LowConfidence,
    ];
    ORDER.into_iter().find(|trigger| triggers.contains(trigger))
}

/// Why an escalation could not produce an [`EscalationOutcome`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum EscalationError {
    /// The report's worker (or a resolved escalation target) is not registered
    /// in the roster.
    #[error("worker {worker:?} is not registered in the roster")]
    UnknownWorker {
        /// The unresolved worker reference.
        worker: WorkerProfileRef,
    },
    /// No registered worker advertises the task's required capability.
    #[error("no registered worker advertises capability {capability:?}")]
    NoCapableWorker {
        /// The capability that could not be satisfied.
        capability: Capability,
    },
    /// A run-context operation (cancellation or budget) failed.
    #[error(transparent)]
    Context(#[from] RunContextError),
}

#[cfg(test)]
mod tests;
