//! End-to-end mixed-agent scheduler scenario (Milestone 6-5 review).
//!
//! This integration test pins the milestone-6 story described in
//! `docs/external-agent.md` §8/§9: a coordinator drives a heterogeneous worker
//! set — a cheap internal agent and a premium external coding agent — through the
//! milestone-6 primitives, all reusing the existing subagent path rather than any
//! new orchestration runtime. It exercises the four collaboration surfaces
//! together:
//!
//! - **Dispatch** ([`Dispatcher`]): a clear, low-risk task rule-routes to the
//!   cheap worker; a complex, high-risk task rule-routes to the strong external
//!   worker.
//! - **Plan / Blackboard** ([`Plan`], [`Blackboard`]): the coordinator tracks the
//!   work as a dependency-ordered task board and the workers coordinate through an
//!   append-only message log, with dependency gating enforced.
//! - **Escalation** ([`Escalator`]): a cost-first attempt on a complex task is run
//!   cheaply, fails, and the escalation engine re-dispatches it to the strong
//!   external worker.
//! - **Artifact aggregation** ([`RecordingArtifactSink`]): the references each
//!   worker produces are collected for the host in report order.
//!
//! Every worker is derived through [`WorkerChoice::into_subagent`] →
//! [`RequirementKind::NeedSubagent`] → [`DrivingSubagentHandler`], so the test
//! observes the real dispatch → requirement → subagent-handler wiring. The child
//! machines are deliberately minimal doubles (they complete on their opening
//! input) so the scenario measures the composition, not any concrete machine's
//! internals. Every test name contains `mixed_scheduler` so the milestone
//! selector `cargo test mixed_scheduler` runs the suite.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use serde_json::{Map, Value};

use agent_lib::agent::collab::{ArtifactSink, Blackboard, Plan, RecordingArtifactSink, TaskStatus};
use agent_lib::agent::id::{AgentId, BlackboardId, PlanId, StepId};
use agent_lib::agent::requirement::AgentSpecRef;
use agent_lib::agent::{
    AgentError, AgentInput, AgentMachine, BudgetLimits, Capability, CostPreference, CostTier,
    DispatchReason, Dispatcher, EscalationRules, EscalationTrigger, Escalator,
    ExternalArtifactKind, ExternalArtifactRef, HandlerScope, HumanGate, ImpactScope, Interaction,
    LoopCursor, LoopDoneReason, PermissionRisk, RequirementKind, RequirementResult, RunContext,
    RunId, ScopePop, ScriptedTaskEvaluator, ScriptedVerifier, StepInput, StepOutcome,
    TaskDescriptor, TraceNodeId, Uncertainty, WorkerProfile, WorkerProfileRef, WorkerReport,
    WorkerRoster,
};
use agent_lib::agent::{
    DrivingSubagentHandler, SpawnedChild, SubagentHandler, SubagentSpawner, TurnDone,
};
use agent_lib::agent::{EscalationOutcome, SubagentOutput};
use agent_lib::conversation::{MessageId, TurnId};
use agent_lib::model::content::ContentBlock;
use agent_lib::model::message::{Message, Role};
use uuid::Uuid;

// ----- deterministic id helpers --------------------------------------------

fn run_id() -> RunId {
    RunId::new(Uuid::from_u128(0x6005_A001))
}

fn child_run_id(seq: u64) -> RunId {
    RunId::new(Uuid::from_u128(0x6005_A002_0000_0000 + u128::from(seq)))
}

fn step_id() -> StepId {
    StepId::new(Uuid::from_u128(0x6005_A200))
}

fn actor_id() -> AgentId {
    AgentId::new(Uuid::from_u128(0x6005_A300))
}

fn cheap_spec() -> AgentSpecRef {
    AgentSpecRef(AgentId::new(Uuid::from_u128(0x6005_C100)))
}

fn strong_spec() -> AgentSpecRef {
    AgentSpecRef(AgentId::new(Uuid::from_u128(0x6005_C200)))
}

fn root_context() -> RunContext {
    RunContext::new_root(
        run_id(),
        BudgetLimits::unbounded(),
        TraceNodeId::new("root"),
    )
}

/// A trivial opening input for a derived child worker turn.
fn child_opening() -> AgentInput {
    let message = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "start".to_owned(),
            extra: Map::new(),
        }],
    };
    AgentInput::user_message(
        TurnId::new(Uuid::from_u128(0x6005_A400)),
        MessageId::new(Uuid::from_u128(0x6005_A401)),
        message,
        MessageId::new(Uuid::from_u128(0x6005_A402)),
        step_id(),
    )
    .expect("valid child opening input")
}

// ----- roster --------------------------------------------------------------

/// Builds the mixed worker roster: an `internal-cheap` worker (cheap tier,
/// search / shell / debug) and a `cc-agent` external worker (premium tier,
/// feature / debug / refactor). The cheap worker escalates test failures up to
/// the external worker, falling back to a human gate when nothing stronger is
/// available.
fn mixed_roster() -> (WorkerRoster, WorkerProfileRef, WorkerProfileRef) {
    let mut roster = WorkerRoster::new();
    let cheap = roster.register(
        WorkerProfile::new(
            "internal-cheap",
            [Capability::Search, Capability::Shell, Capability::Debug],
            CostTier::Cheap,
            EscalationRules::new(
                [
                    EscalationTrigger::TestFailure,
                    EscalationTrigger::Timeout,
                    EscalationTrigger::LowConfidence,
                ],
                Some(WorkerProfileRef::new("cc-agent")),
                true,
            ),
        ),
        cheap_spec(),
    );
    let strong = roster.register(
        WorkerProfile::new(
            "cc-agent",
            [Capability::Feature, Capability::Debug, Capability::Refactor],
            CostTier::Premium,
            EscalationRules::none(),
        ),
        strong_spec(),
    );
    (roster, cheap, strong)
}

// ----- minimal child worker machine + scope --------------------------------

/// A derived worker double that completes on its opening input, emitting no
/// requirements. It records that it was driven so the scenario can confirm the
/// subagent handler actually ran it.
struct ImmediateWorker {
    cursor: LoopCursor,
    steps: Arc<AtomicUsize>,
}

impl ImmediateWorker {
    fn new(steps: Arc<AtomicUsize>) -> Self {
        Self {
            cursor: LoopCursor::default(),
            steps,
        }
    }
}

impl AgentMachine for ImmediateWorker {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        self.steps.fetch_add(1, Ordering::SeqCst);
        if let StepInput::External(_) = input {
            self.cursor = LoopCursor::done(LoopDoneReason::Completed);
        }
        StepOutcome::new(Vec::new(), Vec::new(), true)
    }

    fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }
}

/// An empty handler scope: the derived worker needs no effect handlers because
/// it emits no requirements.
#[derive(Default)]
struct EmptyScope;

impl HandlerScope for EmptyScope {}

/// A [`SubagentSpawner`] that returns an [`ImmediateWorker`] and reports a fixed
/// summary — standing in for whatever concrete worker machine the chosen spec
/// would derive in production.
struct WorkerSpawner {
    steps: Arc<AtomicUsize>,
    summary: String,
}

/// Monotonic counter so every derived child gets a distinct run id / trace node,
/// letting one root context derive several sibling workers.
static NEXT_CHILD: AtomicU64 = AtomicU64::new(1);

impl SubagentSpawner for WorkerSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        let seq = NEXT_CHILD.fetch_add(1, Ordering::SeqCst);
        Ok((child_run_id(seq), TraceNodeId::new(format!("worker-{seq}"))))
    }

    fn spawn(
        &self,
        _spec_ref: &AgentSpecRef,
        _brief: &Interaction,
        _result_schema: Option<&Value>,
    ) -> Result<SpawnedChild, AgentError> {
        Ok(SpawnedChild {
            machine: Box::new(ImmediateWorker::new(self.steps.clone())),
            scope: Box::new(EmptyScope),
            opening: child_opening(),
        })
    }

    fn summarize(&self, _done: &TurnDone) -> SubagentOutput {
        SubagentOutput {
            summary: self.summary.clone(),
        }
    }
}

/// Derives `choice` through the existing subagent path and returns the worker's
/// summary, asserting the child machine was actually driven at least once.
async fn derive_worker(
    choice: agent_lib::agent::WorkerChoice,
    brief: &str,
    summary: &str,
    ctx: &RunContext,
) -> String {
    let RequirementKind::NeedSubagent {
        spec_ref,
        brief,
        result_schema,
    } = choice.into_subagent(Interaction::question(step_id(), brief.to_owned()), None)
    else {
        panic!("into_subagent must produce NeedSubagent");
    };

    let steps = Arc::new(AtomicUsize::new(0));
    let spawner = Arc::new(WorkerSpawner {
        steps: steps.clone(),
        summary: summary.to_owned(),
    });
    let handler = DrivingSubagentHandler::new(spawner, 4);

    let scope = EmptyScope;
    let mut outer = ScopePop::new(&scope, None);
    let result = handler
        .fulfill(&spec_ref, &brief, result_schema.as_ref(), &mut outer, ctx)
        .await;

    assert!(
        steps.load(Ordering::SeqCst) >= 1,
        "the derived worker should have been driven at least once"
    );
    match result {
        RequirementResult::Subagent(Ok(output)) => output.summary,
        other => panic!("expected Subagent(Ok(..)), got {other:?}"),
    }
}

// ----- tests ---------------------------------------------------------------

/// The dispatcher rule-routes a clear, low-risk task to the cheap worker and a
/// complex, high-risk task to the strong external worker — the "明确任务派给
/// cheap、复杂任务派给 external" split from design §8/§9.
#[tokio::test]
async fn mixed_scheduler_dispatch_routes_clear_to_cheap_and_complex_to_external() {
    let (roster, cheap, strong) = mixed_roster();
    let evaluator = ScriptedTaskEvaluator::new(|_, _| None);
    let dispatcher = Dispatcher::new(evaluator);
    let ctx = root_context();

    // Clear, contained, low-risk search -> cheapest capable worker.
    let locate = TaskDescriptor::new(
        Capability::Search,
        ImpactScope::MultiFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    );
    let choice = dispatcher
        .dispatch(&locate, &roster, &ctx)
        .expect("clear task dispatches");
    assert_eq!(choice.worker(), &cheap);
    assert_eq!(choice.spec(), &cheap_spec());
    assert_eq!(choice.reason(), DispatchReason::RuleRoute);

    // Complex, high-risk feature -> strongest capable worker (the external agent).
    let implement = TaskDescriptor::new(
        Capability::Feature,
        ImpactScope::CrossModule,
        PermissionRisk::High,
        Uncertainty::Exploratory,
    );
    let choice = dispatcher
        .dispatch(&implement, &roster, &ctx)
        .expect("complex task dispatches");
    assert_eq!(choice.worker(), &strong);
    assert_eq!(choice.spec(), &strong_spec());
    assert_eq!(choice.reason(), DispatchReason::RuleRoute);
}

/// The plan enforces dependency gating: a task cannot be claimed until its
/// prerequisite completes, and the blackboard preserves an append-only,
/// monotonic order across the coordinating agents.
#[tokio::test]
async fn mixed_scheduler_plan_and_blackboard_gate_and_coordinate() {
    let plan = Plan::new(PlanId::new(Uuid::from_u128(0x6005_B100)));
    let board = Blackboard::new(BlackboardId::new(Uuid::from_u128(0x6005_B200)));

    // Coordinator lays out a dependency chain: implement depends on locate.
    plan.add_task("locate", Vec::<String>::new())
        .expect("add locate");
    let mut version = plan
        .add_task("implement", ["locate"])
        .expect("add implement");

    // implement cannot be claimed while locate is unfinished.
    let blocked = plan.claim("implement", "cc-agent", version);
    assert!(
        matches!(
            blocked,
            Err(agent_lib::agent::collab::PlanError::DependencyBlocked { .. })
        ),
        "implement must be gated on locate, got {blocked:?}"
    );

    // The cheap worker claims and completes locate, posting its finding.
    version = plan
        .claim("locate", "internal-cheap", version)
        .expect("claim locate");
    let off_kickoff = board.post_default("coordinator", "kickoff: locate then implement");
    let off_found = board.post_default("internal-cheap", "located bug in parser.rs");
    version = plan
        .update_status("locate", "internal-cheap", TaskStatus::Completed, version)
        .expect("complete locate");

    // Now implement is claimable by the external worker.
    version = plan
        .claim("implement", "cc-agent", version)
        .expect("claim implement");
    let off_patch = board.post_default("cc-agent", "applied patch to parser.rs");
    plan.update_status("implement", "cc-agent", TaskStatus::Completed, version)
        .expect("complete implement");

    // Blackboard offsets are strictly monotonic and the log is append-only.
    assert!(off_kickoff < off_found && off_found < off_patch);
    let history = board.read_default_from(0);
    let senders: Vec<&str> = history.iter().map(|m| m.sender.as_str()).collect();
    assert_eq!(senders, ["coordinator", "internal-cheap", "cc-agent"]);

    // Both tasks reached Completed under their respective owners.
    let snapshot = plan.snapshot();
    for id in ["locate", "implement"] {
        let task = snapshot.tasks.get(id).expect("task present");
        assert_eq!(task.status, TaskStatus::Completed);
    }
}

/// The full mixed-agent flow: the coordinator dispatches a clear task to the
/// cheap worker and a complex task cost-first to the cheap worker; the cheap
/// attempt fails, the escalation engine re-dispatches it to the strong external
/// worker, and both workers' artifacts are aggregated for the host — all while
/// the plan/blackboard track the collaboration.
#[tokio::test]
async fn mixed_scheduler_cheap_failure_escalates_to_external_and_aggregates_artifacts() {
    let (roster, cheap, strong) = mixed_roster();
    let ctx = root_context();

    let plan = Plan::new(PlanId::new(Uuid::from_u128(0x6005_D100)));
    let board = Blackboard::new(BlackboardId::new(Uuid::from_u128(0x6005_D200)));
    let sink = RecordingArtifactSink::new();

    // Coordinator plans the work and kicks off collaboration.
    plan.add_task("locate", Vec::<String>::new())
        .expect("add locate");
    let mut version = plan
        .add_task("implement", ["locate"])
        .expect("add implement");
    board.post_default("coordinator", "kickoff mixed-agent run");

    let dispatcher = Dispatcher::new(ScriptedTaskEvaluator::new(|_, _| None));

    // --- Stage 1: clear task -> cheap worker, completes and yields an artifact.
    let locate_task = TaskDescriptor::new(
        Capability::Search,
        ImpactScope::MultiFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    );
    let locate_choice = dispatcher
        .dispatch(&locate_task, &roster, &ctx)
        .expect("locate dispatches");
    assert_eq!(locate_choice.worker(), &cheap);

    version = plan
        .claim("locate", "internal-cheap", version)
        .expect("cheap claims locate");
    let summary = derive_worker(
        locate_choice,
        "locate the failing test",
        "found bug in parser.rs",
        &ctx,
    )
    .await;
    assert_eq!(summary, "found bug in parser.rs");
    board.post_default("internal-cheap", &summary);
    sink.record(ExternalArtifactRef {
        kind: ExternalArtifactKind::File,
        summary: "search hit: parser.rs".to_owned(),
        path: Some("parser.rs".to_owned()),
        reference: None,
    });
    version = plan
        .update_status("locate", "internal-cheap", TaskStatus::Completed, version)
        .expect("cheap completes locate");

    // --- Stage 2: complex task, cost-first -> cheap worker attempts and fails.
    let implement_task = TaskDescriptor::new(
        Capability::Debug,
        ImpactScope::MultiFile,
        PermissionRisk::Medium,
        Uncertainty::Clear,
    )
    .with_preference(CostPreference::CostFirst);
    let attempt = dispatcher
        .dispatch(&implement_task, &roster, &ctx)
        .expect("cost-first implement dispatches");
    assert_eq!(
        attempt.worker(),
        &cheap,
        "cost-first attempt goes to cheap first"
    );

    // The cheap worker runs but reports a test failure.
    let failed_summary = derive_worker(
        attempt,
        "implement the fix",
        "cheap attempt failed: tests red",
        &ctx,
    )
    .await;
    assert_eq!(failed_summary, "cheap attempt failed: tests red");
    board.post_default("internal-cheap", &failed_summary);

    // --- Stage 3: escalation re-dispatches the failure to the strong worker.
    let report = WorkerReport::failed(cheap.clone(), EscalationTrigger::TestFailure);
    let escalator = Escalator::new(ScriptedVerifier::passing());
    let gate = HumanGate::new(step_id(), actor_id());
    let outcome = escalator
        .assess(&implement_task, &report, &roster, &ctx, &gate)
        .expect("test failure escalates");

    let escalated_choice = match outcome {
        EscalationOutcome::Reassign(choice) => {
            assert_eq!(
                choice.worker(),
                &strong,
                "escalation upgrades to the external worker"
            );
            assert_eq!(choice.spec(), &strong_spec());
            assert_eq!(choice.reason(), DispatchReason::Escalation);
            choice
        }
        other => panic!("expected reassign to strong worker, got {other:?}"),
    };

    // The strong external worker claims implement and completes it.
    version = plan
        .claim("implement", "cc-agent", version)
        .expect("strong claims implement");
    let patched_summary = derive_worker(
        escalated_choice,
        "implement the fix",
        "patch applied, tests green",
        &ctx,
    )
    .await;
    assert_eq!(patched_summary, "patch applied, tests green");
    board.post_default("cc-agent", &patched_summary);
    sink.record(ExternalArtifactRef {
        kind: ExternalArtifactKind::Patch,
        summary: "diff for parser.rs".to_owned(),
        path: Some("parser.rs".to_owned()),
        reference: Some("blob://patch-1".to_owned()),
    });
    plan.update_status("implement", "cc-agent", TaskStatus::Completed, version)
        .expect("strong completes implement");

    // --- Verify the aggregated outcome across all four surfaces. -----------

    // Artifacts from both workers were aggregated in report order.
    let artifacts = sink.artifacts();
    assert_eq!(artifacts.len(), 2, "one artifact per completed worker");
    assert_eq!(artifacts[0].kind, ExternalArtifactKind::File);
    assert_eq!(artifacts[1].kind, ExternalArtifactKind::Patch);
    assert_eq!(artifacts[1].reference.as_deref(), Some("blob://patch-1"));

    // Plan: both tasks completed, owned by the workers that finished them.
    let snapshot = plan.snapshot();
    let locate = snapshot.tasks.get("locate").expect("locate present");
    assert_eq!(locate.status, TaskStatus::Completed);
    assert_eq!(locate.owner.as_deref(), Some("internal-cheap"));
    let implement = snapshot.tasks.get("implement").expect("implement present");
    assert_eq!(implement.status, TaskStatus::Completed);
    assert_eq!(implement.owner.as_deref(), Some("cc-agent"));

    // Blackboard: the whole collaboration is recorded in append-only order.
    let senders: Vec<String> = board
        .read_default_from(0)
        .into_iter()
        .map(|m| m.sender)
        .collect();
    assert_eq!(
        senders,
        [
            "coordinator",
            "internal-cheap",
            "internal-cheap",
            "cc-agent",
        ]
    );
}
