//! Subagent hierarchy over a shared plan/blackboard (milestone 3, M3-1).
//!
//! `docs/complex-tests.md` §4.2 pins the behaviour where a *headless* child does
//! real work against a plan dependency graph and an append-only blackboard the
//! parent also touches, while its human-in-the-loop approval **pops out to the
//! attended parent** because the child's own scope carries no interaction
//! backend. This is the combined face of three mechanisms at once:
//!
//! - **Scope pop.** The child is a real [`DefaultAgentMachine`] whose
//!   dangerous-tool call is gated by an approval policy. Its headless drain layer
//!   cannot answer the resulting `NeedInteraction`, so it pops one scope out to
//!   the parent, which grants it — only then does the guarded tool run in the
//!   child.
//! - **Shared plan/blackboard.** The parent seeds a `design -> review ->
//!   implement` dependency chain (with `design` already completed) into one
//!   [`MockPlanBlackboardStore`] and hands the *same* `Arc` to the child's tool
//!   handler. `plan_claim_first_available` skips the completed `design` and the
//!   dependency-blocked `implement` and atomically claims `review`; the child
//!   then completes it. Both parent and child append to the shared board under
//!   distinguishable senders, in order, with no duplicated side effect.
//! - **Budget / derivation.** The child context is derived from the parent's, so
//!   the child's model token charges aggregate onto the parent's shared budget
//!   ledger.
//!
//! The parent is a [`ScriptMachine`] that emits a single `NeedSubagent`, which is
//! the lowest-boilerplate way to exercise the reference
//! [`DrivingSubagentHandler`] end to end: the handler derives the child, drives
//! it under a nested drain whose pop target is the parent scope, and summarizes
//! it once it completes. Every observation is read back from a scripted handler's
//! call log, the shared store, or the run-context trace — never a bespoke
//! counter.
//!
//! Run in isolation with `cargo test --test agent_complex_subagent`.

#[path = "complex_support/mod.rs"]
mod complex_support;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use agent_lib::agent::{
    LlmHandler, LoopCursorKind, PlanId, RequirementId, RequirementKindTag, RequirementResult,
    RunContext, ToolHandler, TraceNodeKind, TraceRecord, drain,
};
use agent_lib::client::ChatRequest;

use agent_testkit::prelude::*;

use complex_support::assertions::{
    assert_board_messages, assert_no_task_owner, assert_task_depends_on, assert_task_owner,
    assert_task_status, assert_tool_executions,
};
use complex_support::plan_blackboard::{MockPlanBlackboardStore, TaskStatus};
use complex_support::tools::{
    BLACKBOARD_POST, DANGEROUS_WRITE, PLAN_CLAIM_FIRST_AVAILABLE, PLAN_UPDATE,
    complex_agent_machine, complex_tool_handler,
};

/// Fixed plan id so store construction stays deterministic and offline.
fn plan_id() -> PlanId {
    PlanId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890c3").expect("valid plan id")
}

/// Total child model usage across the four scripted LLM steps:
/// `(5 + 3) + (4 + 2) + (6 + 3) + (3 + 2)`.
const CHILD_TOKENS: u64 = 28;

/// An [`LlmHandler`] that delegates to an inner scripted handler and charges the
/// response usage against the run context.
///
/// Charging usage is a host responsibility — neither the scripted testkit
/// handler nor the reference client handler touches the budget — so this thin
/// wrapper is what proves the child's consumption lands on the parent's shared
/// ledger through the derived child context.
struct ChargingLlm {
    inner: Arc<dyn LlmHandler>,
}

#[async_trait]
impl LlmHandler for ChargingLlm {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        let result = self.inner.fulfill(request, mode, ctx).await;
        if let RequirementResult::Llm(Ok(response)) = &result {
            let tokens = u64::from(response.usage.input) + u64::from(response.usage.output);
            ctx.charge_tokens(tokens)
                .expect("charge child usage on the shared parent ledger");
        }
        result
    }
}

/// Seeds `store` with a `design -> review -> implement` dependency chain in which
/// `design` is already completed, and returns the plan version after seeding.
///
/// The seeding runs directly against the store (not through the model), so the
/// child's `expected_version` arguments can be scripted against a known version:
/// create (v0), add three tasks (v1..v3), claim + complete `design` (v4, v5).
fn seed_plan(store: &MockPlanBlackboardStore) -> u64 {
    store.create_plan();
    store
        .add_task("design", Vec::<&str>::new())
        .expect("add design");
    store.add_task("review", ["design"]).expect("add review");
    store
        .add_task("implement", ["review"])
        .expect("add implement");
    store.claim("design", "seed", 3).expect("claim design");
    store
        .update_status("design", "seed", TaskStatus::Completed, 4)
        .expect("complete design");
    store.version()
}

/// Returns the id of the first settled interaction-family requirement trace node.
///
/// The child's popped approval is the only interaction in the run, so this
/// resolves it uniquely; its `resolved_at_scope` then proves the pop crossed
/// exactly one layer to the parent.
fn interaction_requirement_id(records: &[TraceRecord]) -> RequirementId {
    records
        .iter()
        .find_map(|record| match record.kind() {
            TraceNodeKind::Requirement {
                kind_tag: RequirementKindTag::Interaction,
                ..
            } => Some(
                record
                    .id()
                    .as_str()
                    .parse()
                    .expect("interaction requirement node id parses"),
            ),
            _ => None,
        })
        .expect("a settled interaction requirement trace node exists")
}

/// A headless child updates the shared plan/blackboard and pops its approval to
/// the attended parent.
///
/// One parent turn threads: a seeded plan dependency graph, a first-available
/// claim that skips the completed and dependency-blocked tasks, a dangerous
/// write whose approval pops out to the parent, ordered shared-board side
/// effects under distinguishable senders, a plan completion, and the child's
/// token charges aggregating onto the parent budget.
#[tokio::test]
async fn complex_subagent_updates_shared_plan_and_pops_approval_to_parent() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);

    // Shared store: parent seeds the dependency chain, then records its own board
    // side effect under the `parent` sender before delegating.
    let store = Arc::new(MockPlanBlackboardStore::new(plan_id()));
    let seeded_version = seed_plan(&store);
    assert_eq!(
        seeded_version, 5,
        "seeding leaves the plan at a known version"
    );
    store.post("parent", "delegating review to the review subagent");

    // The child: a real machine that claims `review`, runs a gated dangerous
    // write, and completes `review` — driven by scripted offline effects.
    let child_llm_inner = ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![
            tool_call(
                "child-claim",
                PLAN_CLAIM_FIRST_AVAILABLE,
                json!({ "owner": "worker", "expected_version": 5 }),
            ),
            tool_call(
                "child-post-start",
                BLACKBOARD_POST,
                json!({ "sender": "child", "text": "review started" }),
            ),
        ])
        .with_usage(usage(5, 3)),
        LlmStep::tool_use(vec![tool_call(
            "child-danger",
            DANGEROUS_WRITE,
            json!({ "text": "apply review fix" }),
        )])
        .with_usage(usage(4, 2)),
        LlmStep::tool_use(vec![
            tool_call(
                "child-update",
                PLAN_UPDATE,
                json!({
                    "task": "review",
                    "owner": "worker",
                    "status": "completed",
                    "expected_version": 6
                }),
            ),
            tool_call(
                "child-post-done",
                BLACKBOARD_POST,
                json!({ "sender": "child", "text": "review done" }),
            ),
        ])
        .with_usage(usage(6, 3)),
        LlmStep::response(assistant_text("review complete", usage(3, 2))),
    ]);
    let child_llm_log = Arc::clone(child_llm_inner.log());
    let charging = ChargingLlm {
        inner: Arc::new(child_llm_inner),
    };

    let tool_handler = complex_tool_handler(Arc::clone(&store));
    let child_tool: Arc<dyn ToolHandler> = tool_handler.clone();

    // Headless child scope: LLM + tool, *no* interaction backend, so the
    // dangerous-write approval pops out to the attended parent.
    let child = SpawnedChildBuilder::new()
        .machine(complex_agent_machine(&ids))
        .scope(
            headless_child_scope()
                .llm(Arc::new(charging))
                .tool(child_tool)
                .build(),
        )
        .opening(user_input(&ids, "review the design"))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("review subagent completed")
            .build(),
    );
    let subagent_handler = Arc::clone(&spawner).into_handler(4);

    // The attended parent serves the popped approval and derives the subagent.
    let parent_interaction = ScriptedInteractionHandler::approve_all();
    let parent_interaction_log = Arc::clone(parent_interaction.log());
    let parent_scope = parent_scope_with_subagent(subagent_handler)
        .attended(Arc::new(parent_interaction))
        .build();

    // The parent emits a single NeedSubagent and completes once it resumes.
    let subagent_req_id = ids.requirement_id();
    let spec_ref = AgentSpecRef(ids.agent_id());
    let brief = Interaction::question(ids.step_id(), "review the design".to_owned());
    let mut parent = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            subagent_req_id,
            RequirementKind::NeedSubagent {
                spec_ref,
                brief,
                result_schema: None,
            },
        ))
        .done_after_all_resumed()
        .label("parent")
        .build();
    let parent_log = Arc::clone(parent.log());

    let done = drain(
        &mut parent,
        user_input(&ids, "delegate the review"),
        &parent_scope,
        None,
        &ctx,
    )
    .await
    .expect("parent turn drains to completion");
    assert_eq!(
        done.cursor().kind(),
        LoopCursorKind::Done,
        "the whole turn closes on the parent"
    );

    // ----- subagent lifecycle ----------------------------------------------

    // The handler derived, spawned, and summarized exactly one child.
    assert_eq!(spawner.ids_calls(), 1);
    assert_eq!(spawner.spawn_calls(), 1);
    assert_eq!(spawner.summarize_calls(), 1);
    // The parent was resumed with the driven subagent's output.
    assert_eq!(parent_log.resume_tags(), vec![RequirementKindTag::Subagent]);
    // The child ran its full four-step script.
    assert_eq!(child_llm_log.len(), 4);

    // ----- scope pop -------------------------------------------------------

    // The child's approval popped to the attended parent, which answered it once;
    // only because it was granted did the guarded dangerous write run in the
    // child (a denial would leave zero executions).
    assert_eq!(parent_interaction_log.len(), 1);
    assert_tool_executions(&tool_handler, DANGEROUS_WRITE, 1);

    // ----- shared plan -----------------------------------------------------

    // `plan_claim_first_available` skipped completed `design` and dependency-
    // blocked `implement`, claimed `review`, and the child then completed it.
    assert_task_status(&store, "design", TaskStatus::Completed);
    assert_task_status(&store, "review", TaskStatus::Completed);
    assert_task_owner(&store, "review", "worker");
    // `implement` was never claimed: only one first-available claim ran.
    assert_task_status(&store, "implement", TaskStatus::Todo);
    assert_no_task_owner(&store, "implement");
    // The dependency graph is intact.
    assert_task_depends_on(&store, "review", &["design"]);
    assert_task_depends_on(&store, "implement", &["review"]);

    // ----- shared blackboard ----------------------------------------------

    // Monotonic, non-duplicated side effects across parent and child, in order.
    assert_board_messages(
        &store,
        &[
            "delegating review",
            "review started",
            "apply review fix",
            "review done",
        ],
    );
    // Senders distinguish who authored each message: the parent, the child, and
    // the child's dangerous-write tool.
    let board = store.board_snapshot();
    assert_eq!(board[0].sender, "parent");
    assert_eq!(board[1].sender, "child");
    assert_eq!(board[2].sender, DANGEROUS_WRITE);
    assert_eq!(board[3].sender, "child");

    // ----- budget aggregation ----------------------------------------------

    // The child's token charges land on the parent's shared ledger via the
    // derived child context.
    assert_eq!(ctx.budget().snapshot().used().tokens(), CHILD_TOKENS);

    // ----- trace: pop + subagent resumed -----------------------------------

    let records = ctx.trace().records();
    // Exactly one subagent node was recorded (the single derivation).
    assert_trace(&ctx).subagent_count(1);
    // The parent's NeedSubagent settled in place and resumed.
    assert_trace(&ctx)
        .requirement(subagent_req_id)
        .tag(RequirementKindTag::Subagent)
        .resolved_at_scope(0)
        .resumed();
    // The child's interaction popped one layer out to the parent scope, and the
    // child was resumed with the granted approval.
    let interaction_req_id = interaction_requirement_id(&records);
    assert_trace(&ctx)
        .requirement(interaction_req_id)
        .tag(RequirementKindTag::Interaction)
        .resolved_at_scope(1)
        .resumed();
}
