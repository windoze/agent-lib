use super::{
    EscalationError, EscalationOutcome, Escalator, HumanGate, ScriptedVerifier, Verifier,
    WorkerReport, primary_upward, upgrade_target,
};
use crate::agent::{
    Capability, CostPreference, CostTier, DispatchReason, EscalationRules, EscalationTrigger,
    ImpactScope, InteractionKind, PermissionRisk, RequirementKind, TaskDescriptor, Uncertainty,
    WorkerProfile, WorkerProfileRef, WorkerRoster,
    context::{BudgetLimits, RunContext, TraceNodeId},
    id::{AgentId, RunId, StepId},
    interaction::Interaction,
    requirement::AgentSpecRef,
};

fn run_id() -> RunId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f1"
        .parse()
        .expect("valid run id")
}

fn step_id() -> StepId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f2"
        .parse()
        .expect("valid step id")
}

fn actor_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890a0"
        .parse()
        .expect("valid agent id")
}

fn spec_ref(suffix: &str) -> AgentSpecRef {
    let id: AgentId = format!("018f0d9c-7b6a-7c12-8f31-1234567890{suffix}")
        .parse()
        .expect("valid agent id");
    AgentSpecRef(id)
}

fn context(limits: BudgetLimits) -> RunContext {
    RunContext::new_root(run_id(), limits, TraceNodeId::new("root"))
}

fn gate() -> HumanGate {
    HumanGate::new(step_id(), actor_id())
}

/// A roster with a cheap and a premium worker, both advertising
/// [`Capability::Debug`] so selection is exercised by cost tier alone.
fn roster() -> (WorkerRoster, WorkerProfileRef, WorkerProfileRef) {
    let mut roster = WorkerRoster::new();
    let cheap = roster.register(
        WorkerProfile::new(
            "internal-cheap",
            [Capability::Search, Capability::Shell, Capability::Debug],
            CostTier::Cheap,
            EscalationRules::none(),
        ),
        spec_ref("aa"),
    );
    let strong = roster.register(
        WorkerProfile::new(
            "cc-agent",
            [Capability::Feature, Capability::Debug, Capability::Refactor],
            CostTier::Premium,
            EscalationRules::none(),
        ),
        spec_ref("bb"),
    );
    (roster, cheap, strong)
}

fn debug_task(risk: PermissionRisk, uncertainty: Uncertainty) -> TaskDescriptor {
    TaskDescriptor::new(Capability::Debug, ImpactScope::MultiFile, risk, uncertainty)
}

// ----- required verification: cheap failure -> strong re-dispatch ----------

#[test]
fn escalation_cheap_failure_reassigns_to_strong_worker() {
    let (roster, cheap, strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let report = WorkerReport::failed(cheap, EscalationTrigger::TestFailure);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("failure escalates");

    match outcome {
        EscalationOutcome::Reassign(choice) => {
            assert_eq!(choice.reason(), DispatchReason::Escalation);
            assert_eq!(choice.worker(), &strong);
            assert_eq!(choice.spec(), &spec_ref("bb"));
        }
        other => panic!("expected reassign to strong worker, got {other:?}"),
    }
}

#[test]
fn escalation_timeout_also_reassigns_to_strong_worker() {
    let (roster, cheap, strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let report = WorkerReport::failed(cheap, EscalationTrigger::Timeout);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("timeout escalates");

    assert!(matches!(
        outcome,
        EscalationOutcome::Reassign(choice) if choice.worker() == &strong
    ));
}

#[test]
fn escalation_low_confidence_reassigns_upward() {
    let (roster, cheap, strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let report = WorkerReport::failed(cheap, EscalationTrigger::LowConfidence);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("low confidence re-dispatches");

    assert!(matches!(
        outcome,
        EscalationOutcome::Reassign(choice)
            if choice.worker() == &strong && choice.reason() == DispatchReason::Escalation
    ));
}

// ----- required verification: over-budget -> downgrade / stop --------------

#[test]
fn escalation_budget_exhausted_downgrades_to_cheaper_worker() {
    let (roster, cheap, strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    // The premium worker exhausted the budget; downgrade to the cheap worker.
    let report = WorkerReport::failed(strong, EscalationTrigger::BudgetExhausted);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("budget exhaustion downgrades");

    match outcome {
        EscalationOutcome::Reassign(choice) => {
            assert_eq!(choice.reason(), DispatchReason::BudgetDowngrade);
            assert_eq!(choice.worker(), &cheap);
            assert_eq!(choice.spec(), &spec_ref("aa"));
        }
        other => panic!("expected budget downgrade, got {other:?}"),
    }
}

#[test]
fn escalation_budget_exhausted_without_cheaper_worker_stops_and_asks_user() {
    let (roster, cheap, _strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    // The cheap worker is already the cheapest capable; nothing to downgrade to.
    let report = WorkerReport::failed(cheap, EscalationTrigger::BudgetExhausted);
    let task = TaskDescriptor::new(
        Capability::Shell,
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    );
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("budget exhaustion stops and asks");

    match outcome {
        EscalationOutcome::Human(interaction) => {
            assert_eq!(interaction.step_id(), step_id());
            assert!(matches!(
                interaction.kind(),
                InteractionKind::Question { .. }
            ));
        }
        other => panic!("expected a human question, got {other:?}"),
    }
}

#[test]
fn escalation_low_budget_context_overrides_failure_upgrade() {
    let (roster, cheap, strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    // The premium worker failed its tests, which would normally upgrade, but the
    // shared budget is nearly spent so budget pressure downgrades instead.
    let report = WorkerReport::failed(strong, EscalationTrigger::TestFailure);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);

    // Pre-consume budget so only 10% headroom remains (below the 20% default).
    let ctx = context(BudgetLimits::new(Some(10), None, None, None));
    for _ in 0..9 {
        ctx.charge_step().expect("charge below limit");
    }

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("budget pressure wins over upgrade");

    assert!(matches!(
        outcome,
        EscalationOutcome::Reassign(choice)
            if choice.worker() == &cheap && choice.reason() == DispatchReason::BudgetDowngrade
    ));
}

// ----- required verification: verifier failure -> escalation ---------------

#[test]
fn escalation_verifier_failure_triggers_escalation() {
    let (roster, cheap, strong) = roster();
    // The worker's own run is clean, but the verifier rejects the output.
    let escalator = Escalator::new(ScriptedVerifier::rejecting(
        EscalationTrigger::ReviewRejected,
    ));
    let report = WorkerReport::succeeded(cheap);
    // High risk warrants verification.
    let task = debug_task(PermissionRisk::High, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("verifier rejection escalates");

    match outcome {
        EscalationOutcome::Reassign(choice) => {
            assert_eq!(choice.reason(), DispatchReason::Escalation);
            assert_eq!(choice.worker(), &strong);
        }
        other => panic!("expected escalation after verifier rejection, got {other:?}"),
    }
}

#[test]
fn escalation_verifier_is_skipped_for_non_warranting_task() {
    let (roster, cheap, _strong) = roster();
    // The verifier would reject, but a clear low-risk single-file task does not
    // warrant verification, so the verifier is never consulted.
    let escalator = Escalator::new(ScriptedVerifier::rejecting(
        EscalationTrigger::ReviewRejected,
    ));
    let report = WorkerReport::succeeded(cheap);
    let task = TaskDescriptor::new(
        Capability::Shell,
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    );
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("non-warranting task skips verification");

    assert_eq!(outcome, EscalationOutcome::Accept);
}

// ----- human gates and terminal cases --------------------------------------

#[test]
fn escalation_review_rejection_without_stronger_worker_asks_permission() {
    // A single premium worker with an opt-in human fallback: a review rejection
    // has nowhere stronger to go, so it becomes a permission gate.
    let mut roster = WorkerRoster::new();
    let strong = roster.register(
        WorkerProfile::new(
            "cc-agent",
            [Capability::Feature, Capability::Review],
            CostTier::Premium,
            EscalationRules::new([EscalationTrigger::ReviewRejected], None, true),
        ),
        spec_ref("bb"),
    );
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let report = WorkerReport::failed(strong, EscalationTrigger::ReviewRejected);
    let task = TaskDescriptor::new(
        Capability::Feature,
        ImpactScope::Architectural,
        PermissionRisk::High,
        Uncertainty::Clear,
    );
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("review rejection escalates to human");

    match outcome {
        EscalationOutcome::Human(interaction) => {
            assert_eq!(interaction.step_id(), step_id());
            match interaction.kind() {
                InteractionKind::Permission { request } => {
                    // The permission carries the task's risk and the run actor.
                    assert_eq!(request.risk(), PermissionRisk::High);
                    assert_eq!(request.actor(), actor_id());
                }
                other => panic!("expected a permission gate, got {other:?}"),
            }
        }
        other => panic!("expected a human permission, got {other:?}"),
    }
}

#[test]
fn escalation_terminal_profile_without_fallback_is_exhausted() {
    // A single cheap worker with no stronger peer and no human fallback: a
    // failure has nowhere to escalate.
    let mut roster = WorkerRoster::new();
    let cheap = roster.register(
        WorkerProfile::new(
            "internal-cheap",
            [Capability::Debug],
            CostTier::Cheap,
            EscalationRules::none(),
        ),
        spec_ref("aa"),
    );
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let report = WorkerReport::failed(cheap, EscalationTrigger::TestFailure);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("terminal failure has an outcome");

    assert_eq!(
        outcome,
        EscalationOutcome::Exhausted {
            trigger: EscalationTrigger::TestFailure,
        }
    );
}

#[test]
fn escalation_explicit_escalate_to_target_is_honored() {
    // A cheap worker escalates specifically to a standard worker on test
    // failure, even though a premium worker is also capable.
    let mut roster = WorkerRoster::new();
    let standard_ref = WorkerProfileRef::new("standard-worker");
    let cheap = roster.register(
        WorkerProfile::new(
            "internal-cheap",
            [Capability::Debug],
            CostTier::Cheap,
            EscalationRules::new(
                [EscalationTrigger::TestFailure],
                Some(standard_ref.clone()),
                false,
            ),
        ),
        spec_ref("aa"),
    );
    let standard = roster.register(
        WorkerProfile::new(
            "standard-worker",
            [Capability::Debug],
            CostTier::Standard,
            EscalationRules::none(),
        ),
        spec_ref("cc"),
    );
    roster.register(
        WorkerProfile::new(
            "cc-agent",
            [Capability::Debug],
            CostTier::Premium,
            EscalationRules::none(),
        ),
        spec_ref("bb"),
    );
    assert_eq!(standard, standard_ref);

    let escalator = Escalator::new(ScriptedVerifier::passing());
    let report = WorkerReport::failed(cheap, EscalationTrigger::TestFailure);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("explicit target escalates");

    match outcome {
        EscalationOutcome::Reassign(choice) => {
            assert_eq!(choice.worker(), &standard);
            assert_eq!(choice.spec(), &spec_ref("cc"));
        }
        other => panic!("expected escalation to the explicit standard worker, got {other:?}"),
    }
}

#[test]
fn escalation_reassign_choice_builds_needsubagent() {
    let (roster, cheap, _strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let report = WorkerReport::failed(cheap, EscalationTrigger::TestFailure);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("failure escalates");

    let EscalationOutcome::Reassign(choice) = outcome else {
        panic!("expected a reassign");
    };
    let brief = Interaction::question(step_id(), "retry with a stronger worker".to_owned());
    let requirement = choice.into_subagent(brief.clone(), None);
    match requirement {
        RequirementKind::NeedSubagent {
            spec_ref: derived,
            brief: carried,
            result_schema,
        } => {
            assert_eq!(derived, spec_ref("bb"));
            assert_eq!(carried, brief);
            assert!(result_schema.is_none());
        }
        other => panic!("expected NeedSubagent, got {other:?}"),
    }
}

// ----- clean acceptance and error paths ------------------------------------

#[test]
fn escalation_clean_report_is_accepted() {
    let (roster, cheap, _strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let report = WorkerReport::succeeded(cheap);
    // Even a warranting task with a passing verifier accepts a clean run.
    let task = debug_task(PermissionRisk::High, Uncertainty::Ambiguous);
    let ctx = context(BudgetLimits::unbounded());

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("clean report accepts");
    assert_eq!(outcome, EscalationOutcome::Accept);
}

#[test]
fn escalation_unknown_worker_errors() {
    let (roster, _cheap, _strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let ghost = WorkerProfileRef::new("ghost-worker");
    let report = WorkerReport::failed(ghost.clone(), EscalationTrigger::TestFailure);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());

    let error = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect_err("unknown worker is rejected");
    assert_eq!(error, EscalationError::UnknownWorker { worker: ghost });
}

#[test]
fn escalation_no_capable_worker_on_budget_errors() {
    let (roster, cheap, _strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    // Budget exhaustion on a capability no worker advertises.
    let report = WorkerReport::failed(cheap, EscalationTrigger::BudgetExhausted);
    let task = TaskDescriptor::new(
        Capability::Review,
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    );
    let ctx = context(BudgetLimits::unbounded());

    let error = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect_err("no capable worker to downgrade to");
    assert_eq!(
        error,
        EscalationError::NoCapableWorker {
            capability: Capability::Review,
        }
    );
}

#[test]
fn escalation_cancelled_context_errors() {
    let (roster, cheap, _strong) = roster();
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let report = WorkerReport::failed(cheap, EscalationTrigger::TestFailure);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);
    let ctx = context(BudgetLimits::unbounded());
    ctx.cancellation().cancel();

    let error = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect_err("cancelled run is not assessed");
    assert!(matches!(error, EscalationError::Context(_)));
}

// ----- unit coverage for helpers and data ----------------------------------

#[test]
fn escalation_warrants_verification_predicate() {
    let safe = TaskDescriptor::new(
        Capability::Shell,
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    );
    assert!(!safe.warrants_verification());

    assert!(debug_task(PermissionRisk::High, Uncertainty::Clear).warrants_verification());
    assert!(debug_task(PermissionRisk::Low, Uncertainty::Ambiguous).warrants_verification());
    assert!(
        TaskDescriptor::new(
            Capability::Refactor,
            ImpactScope::CrossModule,
            PermissionRisk::Low,
            Uncertainty::Clear,
        )
        .warrants_verification()
    );
}

#[test]
fn escalation_worker_report_builders() {
    let cheap = WorkerProfileRef::new("internal-cheap");
    assert!(WorkerReport::succeeded(cheap.clone()).is_clean());

    let report = WorkerReport::failed(cheap.clone(), EscalationTrigger::Timeout)
        .with_trigger(EscalationTrigger::Timeout)
        .with_trigger(EscalationTrigger::TestFailure);
    assert_eq!(report.worker(), &cheap);
    assert!(report.raised(EscalationTrigger::TestFailure));
    assert!(!report.is_clean());
    // Duplicate triggers are de-duplicated.
    assert_eq!(report.triggers().len(), 2);

    let multi = WorkerReport::new(
        cheap,
        [
            EscalationTrigger::LowConfidence,
            EscalationTrigger::LowConfidence,
        ],
    );
    assert_eq!(multi.triggers(), &[EscalationTrigger::LowConfidence]);
}

#[test]
fn escalation_worker_report_serde_round_trip() {
    let report = WorkerReport::new(
        WorkerProfileRef::new("cc-agent"),
        [
            EscalationTrigger::TestFailure,
            EscalationTrigger::LowConfidence,
        ],
    );
    let json = serde_json::to_string(&report).expect("serialize");
    let restored: WorkerReport = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored, report);

    // A clean report omits the empty trigger list.
    let clean = WorkerReport::succeeded(WorkerProfileRef::new("cc-agent"));
    let clean_json = serde_json::to_string(&clean).expect("serialize");
    assert!(
        !clean_json.contains("triggers"),
        "clean report: {clean_json}"
    );
}

#[test]
fn escalation_primary_upward_orders_by_severity() {
    let triggers = [
        EscalationTrigger::LowConfidence,
        EscalationTrigger::Timeout,
        EscalationTrigger::ReviewRejected,
        EscalationTrigger::TestFailure,
    ];
    assert_eq!(
        primary_upward(&triggers),
        Some(EscalationTrigger::ReviewRejected)
    );
    assert_eq!(
        primary_upward(&[EscalationTrigger::Timeout, EscalationTrigger::TestFailure]),
        Some(EscalationTrigger::TestFailure)
    );
    // A budget-only trigger set has no upward action.
    assert_eq!(primary_upward(&[EscalationTrigger::BudgetExhausted]), None);
}

#[test]
fn escalation_upgrade_target_requires_strictly_stronger_worker() {
    let (roster, _cheap, _strong) = roster();
    let strong_profile = roster
        .profile(&WorkerProfileRef::new("cc-agent"))
        .expect("premium profile")
        .clone();
    // The strongest worker cannot upgrade past itself.
    assert!(
        upgrade_target(
            &strong_profile,
            EscalationTrigger::TestFailure,
            &roster,
            &Capability::Debug,
        )
        .is_none()
    );

    let cheap_profile = roster
        .profile(&WorkerProfileRef::new("internal-cheap"))
        .expect("cheap profile")
        .clone();
    assert_eq!(
        upgrade_target(
            &cheap_profile,
            EscalationTrigger::TestFailure,
            &roster,
            &Capability::Debug,
        ),
        Some(WorkerProfileRef::new("cc-agent"))
    );
}

#[test]
fn escalation_human_gate_accessors() {
    let gate = HumanGate::new(step_id(), actor_id());
    assert_eq!(gate.step(), step_id());
    assert_eq!(gate.actor(), actor_id());
}

#[test]
fn escalation_scripted_verifier_helpers() {
    let task = debug_task(PermissionRisk::High, Uncertainty::Clear);
    let report = WorkerReport::succeeded(WorkerProfileRef::new("cc-agent"));

    assert_eq!(ScriptedVerifier::passing().verify(&task, &report), None);
    assert_eq!(
        ScriptedVerifier::rejecting(EscalationTrigger::TestFailure).verify(&task, &report),
        Some(EscalationTrigger::TestFailure)
    );

    let dynamic = ScriptedVerifier::new(|task, _| {
        (task.preference() == CostPreference::QualityFirst)
            .then_some(EscalationTrigger::ReviewRejected)
    });
    let quality = task.clone().with_preference(CostPreference::QualityFirst);
    assert_eq!(
        dynamic.verify(&quality, &report),
        Some(EscalationTrigger::ReviewRejected)
    );
    assert_eq!(dynamic.verify(&task, &report), None);
}

#[test]
fn escalation_disabled_budget_headroom_ignores_low_context() {
    let (roster, cheap, strong) = roster();
    // With headroom disabled, a low budget no longer forces a downgrade, so a
    // test failure from the cheap worker still upgrades to the strong one.
    let escalator = Escalator::new(ScriptedVerifier::passing()).with_budget_headroom(0);
    let report = WorkerReport::failed(cheap, EscalationTrigger::TestFailure);
    let task = debug_task(PermissionRisk::Medium, Uncertainty::Clear);

    let ctx = context(BudgetLimits::new(Some(10), None, None, None));
    for _ in 0..9 {
        ctx.charge_step().expect("charge below limit");
    }

    let outcome = escalator
        .assess(&task, &report, &roster, &ctx, &gate())
        .expect("disabled headroom upgrades despite low budget");
    assert!(matches!(
        outcome,
        EscalationOutcome::Reassign(choice)
            if choice.worker() == &strong && choice.reason() == DispatchReason::Escalation
    ));
}
