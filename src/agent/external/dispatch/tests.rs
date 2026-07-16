use super::{
    CostPreference, DispatchError, DispatchReason, Dispatcher, ImpactScope, RuleRouter,
    ScriptedTaskEvaluator, TaskDescriptor, Uncertainty, WorkerChoice, WorkerRoster, budget_is_low,
};
use crate::agent::{
    Capability, CostTier, EscalationRules, Interaction, PermissionRisk, RequirementKind,
    WorkerProfile, WorkerProfileRef,
    context::{BudgetLimits, RunContext, TraceNodeId},
    id::{AgentId, RunId, StepId},
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

fn spec_ref(suffix: &str) -> AgentSpecRef {
    let id: AgentId = format!("018f0d9c-7b6a-7c12-8f31-1234567890{suffix}")
        .parse()
        .expect("valid agent id");
    AgentSpecRef(id)
}

fn context(limits: BudgetLimits) -> RunContext {
    RunContext::new_root(run_id(), limits, TraceNodeId::new("root"))
}

/// A roster with a cheap search/shell worker and a premium feature/debug worker,
/// both advertising [`Capability::Debug`] so capability-based selection is
/// exercised by cost tier alone.
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

#[test]
fn dispatcher_rule_route_hits_cheap_worker_for_clear_read_only_shell() {
    let (roster, cheap, _strong) = roster();
    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::new(|_, _| None));

    let task = TaskDescriptor::new(
        Capability::Shell,
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    );

    let ctx = context(BudgetLimits::unbounded());
    let choice = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect("clear read-only shell routes deterministically");

    assert_eq!(choice.reason(), DispatchReason::RuleRoute);
    assert_eq!(choice.worker(), &cheap);
    assert_eq!(choice.spec(), &spec_ref("aa"));
    // Rule-routed tasks must not consume budget on an evaluator call.
    assert_eq!(ctx.budget().snapshot().used().steps(), 0);
}

#[test]
fn dispatcher_falls_back_to_evaluator_for_ambiguous_task() {
    let (roster, _cheap, strong) = roster();
    let evaluator = ScriptedTaskEvaluator::new(move |task, roster| {
        // A scripted "LLM": escalate ambiguous debug work to the strongest worker.
        assert_eq!(task.uncertainty(), Uncertainty::Ambiguous);
        roster.strongest_capable(task.task_type())
    });
    let dispatcher = Dispatcher::new(evaluator);

    let task = TaskDescriptor::new(
        Capability::Debug,
        ImpactScope::MultiFile,
        PermissionRisk::Medium,
        Uncertainty::Ambiguous,
    );

    let ctx = context(BudgetLimits::new(Some(100), None, None, None));
    let choice = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect("ambiguous task resolved by evaluator");

    assert_eq!(choice.reason(), DispatchReason::Evaluator);
    assert_eq!(choice.worker(), &strong);
    assert_eq!(choice.spec(), &spec_ref("bb"));
    // The evaluator consultation is charged one step against the shared budget.
    assert_eq!(ctx.budget().snapshot().used().steps(), 1);
}

#[test]
fn dispatcher_downgrades_when_budget_near_limit() {
    let (roster, cheap, _strong) = roster();
    // The evaluator would pick the strong worker, but budget pressure must win.
    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::new(|task, roster| {
        roster.strongest_capable(task.task_type())
    }));

    // A heavy task that the rule router would send to the strong worker.
    let task = TaskDescriptor::new(
        Capability::Debug,
        ImpactScope::Architectural,
        PermissionRisk::High,
        Uncertainty::Clear,
    );

    // Pre-consume budget so only 10% headroom remains (below the 20% default).
    let ctx = context(BudgetLimits::new(Some(10), None, None, None));
    for _ in 0..9 {
        ctx.charge_step().expect("charge below limit");
    }

    let choice = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect("near-limit budget downgrades to cheapest worker");

    assert_eq!(choice.reason(), DispatchReason::BudgetDowngrade);
    assert_eq!(choice.worker(), &cheap);
    assert_eq!(choice.spec(), &spec_ref("aa"));
}

#[test]
fn dispatcher_charge_failure_downgrades_to_cheapest() {
    let (roster, cheap, _strong) = roster();
    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::new(|task, roster| {
        roster.strongest_capable(task.task_type())
    }))
    // Disable headroom downgrade so the flow reaches the evaluator charge.
    .with_budget_headroom(0);

    // Moderate + exploratory: the rule router declines, forcing the evaluator
    // path, whose step charge then exhausts the fully-consumed budget.
    let task = TaskDescriptor::new(
        Capability::Debug,
        ImpactScope::MultiFile,
        PermissionRisk::Medium,
        Uncertainty::Exploratory,
    );

    let ctx = context(BudgetLimits::new(Some(1), None, None, None));
    ctx.charge_step().expect("consume the only step");

    let choice = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect("exhausted budget downgrades instead of erroring");

    assert_eq!(choice.reason(), DispatchReason::BudgetDowngrade);
    assert_eq!(choice.worker(), &cheap);
}

#[test]
fn dispatcher_routes_architectural_task_to_strong_worker() {
    let (roster, _cheap, strong) = roster();
    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::new(|_, _| None));

    let task = TaskDescriptor::new(
        Capability::Feature,
        ImpactScope::Architectural,
        PermissionRisk::High,
        Uncertainty::Clear,
    );

    let ctx = context(BudgetLimits::unbounded());
    let choice = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect("architectural work routes to strong worker");

    assert_eq!(choice.reason(), DispatchReason::RuleRoute);
    assert_eq!(choice.worker(), &strong);
}

#[test]
fn dispatcher_cost_first_preference_prefers_cheap_worker() {
    let (roster, cheap, _strong) = roster();
    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::new(|_, _| None));

    // Multi-file debug would otherwise fall to the evaluator; cost-first routes
    // it to the cheapest capable worker deterministically.
    let task = TaskDescriptor::new(
        Capability::Debug,
        ImpactScope::MultiFile,
        PermissionRisk::Medium,
        Uncertainty::Exploratory,
    )
    .with_preference(CostPreference::CostFirst);

    let ctx = context(BudgetLimits::unbounded());
    let choice = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect("cost-first routes to cheap worker");

    assert_eq!(choice.reason(), DispatchReason::RuleRoute);
    assert_eq!(choice.worker(), &cheap);
}

#[test]
fn dispatcher_worker_choice_builds_needsubagent() {
    let choice = WorkerChoice::new(
        WorkerProfileRef::new("cc-agent"),
        spec_ref("bb"),
        DispatchReason::Evaluator,
    );

    let brief = Interaction::question(step_id(), "implement the feature".to_owned());
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

#[test]
fn dispatcher_unknown_worker_from_evaluator_errors() {
    let (roster, _cheap, _strong) = roster();
    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::always(WorkerProfileRef::new(
        "ghost-worker",
    )));

    let task = TaskDescriptor::new(
        Capability::Debug,
        ImpactScope::MultiFile,
        PermissionRisk::Medium,
        Uncertainty::Ambiguous,
    );

    let ctx = context(BudgetLimits::unbounded());
    let error = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect_err("unregistered worker is rejected");

    assert_eq!(
        error,
        DispatchError::UnknownWorker {
            worker: WorkerProfileRef::new("ghost-worker"),
        }
    );
}

#[test]
fn dispatcher_no_capable_worker_downgrade_errors() {
    let (roster, _cheap, _strong) = roster();
    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::new(|_, _| None));

    // Review capability is advertised by neither worker; a budget downgrade then
    // finds nothing capable.
    let task = TaskDescriptor::new(
        Capability::Review,
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    );

    let ctx = context(BudgetLimits::new(Some(10), None, None, None));
    for _ in 0..9 {
        ctx.charge_step().expect("charge below limit");
    }

    let error = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect_err("no capable worker to downgrade to");

    assert_eq!(
        error,
        DispatchError::NoCapableWorker {
            capability: Capability::Review,
        }
    );
}

#[test]
fn dispatcher_evaluator_decline_yields_no_worker() {
    let (roster, _cheap, _strong) = roster();
    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::new(|_, _| None));

    let task = TaskDescriptor::new(
        Capability::Debug,
        ImpactScope::MultiFile,
        PermissionRisk::Medium,
        Uncertainty::Ambiguous,
    );

    let ctx = context(BudgetLimits::unbounded());
    let error = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect_err("evaluator declined and no rule matched");

    assert_eq!(error, DispatchError::NoWorker);
}

#[test]
fn dispatcher_cancelled_context_errors() {
    let (roster, _cheap, _strong) = roster();
    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::new(|_, _| None));
    let task = TaskDescriptor::new(
        Capability::Shell,
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    );

    let ctx = context(BudgetLimits::unbounded());
    ctx.cancellation().cancel();

    let error = dispatcher
        .dispatch(&task, &roster, &ctx)
        .expect_err("cancelled run is not dispatched");
    assert!(matches!(error, DispatchError::Context(_)));
}

#[test]
fn dispatcher_task_descriptor_serde_round_trip() {
    let task = TaskDescriptor::new(
        Capability::Custom("mcp-migrate".to_owned()),
        ImpactScope::CrossModule,
        PermissionRisk::High,
        Uncertainty::Exploratory,
    )
    .with_preference(CostPreference::QualityFirst);

    let json = serde_json::to_string(&task).expect("serialize");
    let restored: TaskDescriptor = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored, task);
}

#[test]
fn dispatcher_budget_is_low_threshold() {
    // 8 of 10 steps used -> 20% remaining, exactly at the threshold (not low).
    let ctx = context(BudgetLimits::new(Some(10), None, None, None));
    for _ in 0..8 {
        ctx.charge_step().expect("charge below limit");
    }
    assert!(!budget_is_low(&ctx, 20));

    // One more step -> 10% remaining, below the threshold (low).
    ctx.charge_step().expect("charge below limit");
    assert!(budget_is_low(&ctx, 20));

    // A zero headroom disables the downgrade check entirely.
    assert!(!budget_is_low(&ctx, 0));

    // Unbounded budgets are never low.
    let unbounded = context(BudgetLimits::unbounded());
    assert!(!budget_is_low(&unbounded, 50));
}

#[test]
fn dispatcher_roster_register_replaces_same_worker() {
    let mut roster = WorkerRoster::new();
    let first = roster.register(
        WorkerProfile::new(
            "worker",
            [Capability::Search],
            CostTier::Cheap,
            EscalationRules::none(),
        ),
        spec_ref("aa"),
    );
    let second = roster.register(
        WorkerProfile::new(
            "worker",
            [Capability::Feature],
            CostTier::Premium,
            EscalationRules::none(),
        ),
        spec_ref("bb"),
    );

    assert_eq!(first, second);
    assert_eq!(roster.workers().len(), 1);
    let worker = roster.resolve_worker(&second).expect("worker resolves");
    assert_eq!(worker.spec(), &spec_ref("bb"));
    assert_eq!(
        roster.profile(&second).map(WorkerProfile::cost_tier),
        Some(CostTier::Premium),
    );
}

#[test]
fn dispatcher_router_defers_ambiguous_tasks() {
    let (roster, _cheap, _strong) = roster();
    let router = RuleRouter::new();
    let task = TaskDescriptor::new(
        Capability::Debug,
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Ambiguous,
    );
    assert!(router.route(&task, &roster).is_none());
}
