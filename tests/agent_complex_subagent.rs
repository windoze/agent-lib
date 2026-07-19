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

use serde_json::json;

use agent_lib::agent::{
    InteractionKind, LoopCursorKind, PlanId, Requirement, RequirementId, RequirementKind,
    RequirementKindTag, RequirementResult, RunContext, ScopePop, SubagentHandler, ToolHandler,
    TraceNodeKind, TraceRecord, drain,
};
use agent_lib::model::content::ContentBlock;
use agent_lib::model::message::Message;

use agent_testkit::prelude::*;

use serde_json::Value;

use complex_support::assertions::{
    assert_board_messages, assert_no_task_owner, assert_pivot_after_tool_result,
    assert_task_depends_on, assert_task_owner, assert_task_status, assert_tool_executions,
};
use complex_support::plan_blackboard::{MockPlanBlackboardStore, TaskStatus};
use complex_support::tools::{
    BLACKBOARD_POST, DANGEROUS_WRITE, PLAN_CLAIM_FIRST_AVAILABLE, PLAN_UPDATE, SAFE_READ,
    SPAWN_REVIEWER, complex_agent_machine, complex_tool_handler,
};

/// Fixed plan id so store construction stays deterministic and offline.
fn plan_id() -> PlanId {
    PlanId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890c3").expect("valid plan id")
}

/// Total child model usage across the four scripted LLM steps:
/// `(5 + 3) + (4 + 2) + (6 + 3) + (3 + 2)`.
const CHILD_TOKENS: u64 = 28;

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

    let tool_handler = complex_tool_handler(Arc::clone(&store));
    let child_tool: Arc<dyn ToolHandler> = tool_handler.clone();

    // Headless child scope: LLM + tool, *no* interaction backend, so the
    // dangerous-write approval pops out to the attended parent.
    let child = SpawnedChildBuilder::new()
        .machine(complex_agent_machine(&ids))
        .scope(
            headless_child_scope()
                .llm(Arc::new(child_llm_inner))
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

// ----- M4-3: pivot re-renders the request, and the subagent brief follows -----

/// The user's original goal, which names a *direct* dangerous write.
///
/// It stays in conversation history after the pivot, so the mid-turn redirect is
/// what must reshape the coordinator's next action — not this opening goal.
const COORD_OLD_GOAL: &str = "Implement feature A by directly writing file A with dangerous_write.";

/// The human pivot injected at the post-tool boundary, redirecting the turn to a
/// reviewer subagent that only reviews.
const PIVOT_TEXT: &str = "Switch to a reviewer subagent: review only, do not edit files directly.";

/// The pivot intent substring shared by [`PIVOT_TEXT`], the reviewer brief, and
/// the child's opening request — the thread we follow from the re-rendered
/// request all the way into the child's first LLM call.
const REVIEW_ONLY: &str = "review only, do not edit files directly";

/// The brief the coordinator produces for `spawn_reviewer` *after* seeing the
/// pivot: it carries the pivot intent and drops the old goal's `dangerous_write`.
const REVIEWER_BRIEF: &str =
    "Reviewer subagent brief: review only, do not edit files directly (per the latest pivot).";

/// Fulfils a `NeedTool` requirement through the complex tool handler, returning
/// the tool-family result the harness resumes with.
async fn fulfill_tool(
    handler: &complex_support::tools::ComplexToolHandler,
    ctx: &RunContext,
    requirement: &Requirement,
) -> RequirementResult {
    match &requirement.kind {
        RequirementKind::NeedTool { call_id, call } => handler.fulfill(*call_id, call, ctx).await,
        other => panic!("expected a NeedTool requirement, found {other:?}"),
    }
}

/// Concatenates every [`ContentBlock::Text`] payload of `message`.
fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Returns whether any message in `messages` carries `needle` in its text.
fn any_message_contains(messages: &[Message], needle: &str) -> bool {
    messages
        .iter()
        .any(|message| message_text(message).contains(needle))
}

/// Extracts the outstanding `NeedLlm` request messages, panicking with a family
/// diagnostic when the requirement is not a `NeedLlm`.
fn llm_request_messages(requirement: &Requirement) -> Vec<Message> {
    match &requirement.kind {
        RequirementKind::NeedLlm { request, .. } => request.messages.clone(),
        other => panic!("expected a NeedLlm requirement, found {other:?}"),
    }
}

/// Returns the open-question prompt of `interaction`, panicking for any other
/// interaction family.
fn question_prompt(interaction: &Interaction) -> String {
    match interaction.kind() {
        InteractionKind::Question { prompt } => prompt.clone(),
        other => panic!("expected a question brief, found {other:?}"),
    }
}

/// A mid-turn pivot re-renders the coordinator's outstanding request, and the
/// reviewer subagent it then spawns opens on that re-rendered brief — not the
/// pre-pivot goal.
///
/// This is `docs/complex-tests.md` §4.2 P1-3: a pivot must not merely land in the
/// conversation, it must reshape what happens next. The coordinator is a real
/// [`DefaultAgentMachine`] driven by hand through a [`StepHarness`] so the pivot
/// injects at the legal post-tool-result boundary and re-renders the *same*
/// outstanding `NeedLlm`. Because `DefaultAgentMachine` only ever emits tool
/// requirements, the subagent is triggered as a tool-ified `spawn_reviewer`
/// call: at that `NeedTool` the test drives a reviewer child through the
/// reference [`DrivingSubagentHandler`] + [`ScriptedSubagentSpawner`], and folds
/// the child's summary back as the tool result so the coordinator turn closes on
/// one committed turn.
///
/// The pivot thread is followed end to end:
/// - the re-rendered coordinator request carries the pivot text;
/// - the `spawn_reviewer` brief the coordinator produces carries the pivot intent
///   and has dropped the old goal's `dangerous_write`;
/// - the [`ScriptedSubagentSpawner`] captures exactly that brief as it reaches the
///   child derivation;
/// - the child's *own* first LLM request — rendered from the opening the brief
///   folded into — carries the pivot intent too.
///
/// The regression guard is that the pre-pivot dangerous write never runs on
/// either side: neither the coordinator nor the reviewer executes
/// [`DANGEROUS_WRITE`].
#[tokio::test]
async fn complex_pivot_then_subagent_uses_rerendered_brief() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let reviewer_ids = ids.fork("reviewer");

    // A single shared store: the coordinator posts to it, and the reviewer reads
    // it back, but through *separate* tool handlers so each side's execution
    // counts stay distinguishable.
    let store = Arc::new(MockPlanBlackboardStore::new(plan_id()));
    let coord_handler = complex_tool_handler(Arc::clone(&store));

    let machine = complex_agent_machine(&ids);
    let mut harness = StepHarness::with_ids(machine, ids);

    // 1. Open the turn on the original (direct-write) goal.
    let llm_open = harness
        .user(COORD_OLD_GOAL)
        .single_llm()
        .expect("a fresh user turn opens on NeedLlm")
        .id;

    // 2. First model step: a benign blackboard post announcing the old goal. It
    //    auto-approves, runs, and parks on the next NeedLlm — the legal pivot
    //    boundary right after a tool result.
    let after_open = harness.resume(
        llm_open,
        RequirementResult::Llm(Ok(assistant_tool_use(
            vec![tool_call(
                "c-post-start",
                BLACKBOARD_POST,
                serde_json::json!({
                    "sender": "coordinator",
                    "text": "starting feature A: intend to directly edit file A"
                }),
            )],
            usage(6, 4),
        ))),
    );
    let post_req = after_open
        .single_tool()
        .expect("the auto blackboard post parks on NeedTool")
        .clone();
    let after_post = harness.resume(
        post_req.id,
        fulfill_tool(&coord_handler, &ctx, &post_req).await,
    );
    let pre_pivot_llm = after_post
        .single_llm()
        .expect("the blackboard post drains to the next NeedLlm")
        .id;

    // 3. Inject the human pivot. It re-renders the *same* outstanding NeedLlm, and
    //    the re-rendered request must carry the pivot text.
    let after_pivot = harness.pivot(PIVOT_TEXT);
    let pivot_llm = after_pivot
        .single_llm()
        .expect("the pivot re-renders the outstanding NeedLlm")
        .clone();
    assert_eq!(
        pre_pivot_llm, pivot_llm.id,
        "a pivot re-renders the same LLM step under the same id"
    );
    let rerendered = llm_request_messages(&pivot_llm);
    assert!(
        any_message_contains(&rerendered, PIVOT_TEXT),
        "the re-rendered coordinator request must carry the pivot text, got roles/texts:\n{:?}",
        rerendered
            .iter()
            .map(|message| format!("{:?}: {}", message.role, message_text(message)))
            .collect::<Vec<_>>()
    );

    // 4. Re-rendered model step: instead of the old dangerous write, delegate to a
    //    reviewer subagent with a brief that follows the pivot.
    let after_rerender = harness.resume(
        pivot_llm.id,
        RequirementResult::Llm(Ok(assistant_tool_use(
            vec![tool_call(
                "c-spawn-reviewer",
                SPAWN_REVIEWER,
                serde_json::json!({ "brief": REVIEWER_BRIEF }),
            )],
            usage(5, 3),
        ))),
    );
    let spawn_req = after_rerender
        .single_tool()
        .expect("spawn_reviewer parks on NeedTool")
        .clone();
    let (spawn_req_id, spawn_call) = match &spawn_req.kind {
        RequirementKind::NeedTool { call, .. } => (spawn_req.id, call.clone()),
        other => panic!("expected the spawn_reviewer NeedTool, found {other:?}"),
    };
    let brief_text = spawn_call
        .input
        .get("brief")
        .and_then(Value::as_str)
        .expect("the spawn_reviewer call carries a `brief` string")
        .to_owned();

    // The coordinator's brief follows the pivot: it carries the review-only intent
    // and has dropped the old goal's dangerous write.
    assert!(
        brief_text.contains(REVIEW_ONLY),
        "the spawn_reviewer brief must carry the pivot intent, got {brief_text:?}"
    );
    assert!(
        !brief_text.contains("dangerous_write"),
        "the spawn_reviewer brief must not carry the pre-pivot dangerous write, got {brief_text:?}"
    );

    // The reviewer child: a real machine opened on the brief, doing only a safe
    // read then answering. Its own tool handler shares the store but logs
    // separately so we can prove it never runs the dangerous write.
    let reviewer_llm = ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "r-safe-read",
            SAFE_READ,
            serde_json::json!({ "from": 0 }),
        )]),
        LlmStep::text("reviewed per the brief: recommend plan-only, no direct file edits"),
    ]);
    let reviewer_llm_log = Arc::clone(reviewer_llm.log());
    let reviewer_handler = complex_tool_handler(Arc::clone(&store));
    let reviewer_tool: Arc<dyn ToolHandler> = reviewer_handler.clone();

    let child = SpawnedChildBuilder::new()
        .machine(complex_agent_machine(&reviewer_ids))
        .scope(
            headless_child_scope()
                .llm(Arc::new(reviewer_llm))
                .tool(reviewer_tool)
                .build(),
        )
        .opening(user_input(&reviewer_ids, &brief_text))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(reviewer_ids.clone())
            .child(child)
            .summary("reviewer subagent: recommend plan-only, no direct file edits")
            .build(),
    );
    let subagent_handler = Arc::clone(&spawner).into_handler(2);

    // Drive the reviewer child at the spawn_reviewer boundary through the real
    // subagent handler. The child is self-contained, so the outer pop target is
    // an empty scope it never reaches.
    let spec_ref = AgentSpecRef(reviewer_ids.agent_id());
    let brief = Interaction::question(reviewer_ids.step_id(), brief_text.clone());
    let empty = TestScope::builder().build();
    let mut outer = ScopePop::new(&empty, None);
    let summary = match subagent_handler
        .fulfill(&spec_ref, &brief, None, &mut outer, &ctx)
        .await
    {
        RequirementResult::Subagent(Ok(output)) => output.summary,
        other => panic!("the reviewer subagent must complete, got {other:?}"),
    };

    // Fold the child's summary back as the spawn_reviewer tool result and close
    // the coordinator turn.
    let after_spawn = harness.resume(
        spawn_req_id,
        RequirementResult::Tool(Ok(tool_ok(spawn_call.id.as_str(), &summary))),
    );
    let final_llm = after_spawn
        .single_llm()
        .expect("the reviewer result drains to the coordinator's final NeedLlm")
        .id;
    let done = harness.resume(
        final_llm,
        RequirementResult::Llm(Ok(assistant_text(
            "delegated the review to the reviewer subagent",
            usage(4, 3),
        ))),
    );
    assert_eq!(
        done.cursor().kind(),
        LoopCursorKind::Done,
        "the coordinator turn closes after the reviewer result"
    );

    // ----- assertions -------------------------------------------------------

    let machine = harness.into_machine();
    let conversation = machine.state().conversation();

    // Exactly one committed coordinator turn, nothing left pending.
    assert_eq!(
        conversation.turns().len(),
        1,
        "the whole scenario commits a single coordinator turn"
    );
    assert!(
        conversation.pending().is_none(),
        "the coordinator turn is fully committed with no pending frozen messages"
    );
    // The pivot user message lands after the first tool result, in turn order.
    assert_pivot_after_tool_result(conversation, PIVOT_TEXT);

    // ----- pivot thread: brief -> spawner -> child opening ------------------

    // The spawner captured exactly the brief the coordinator produced after the
    // pivot as it reached the child derivation.
    let captured = spawner.briefs();
    assert_eq!(captured.len(), 1, "exactly one reviewer brief was spawned");
    let captured_prompt = question_prompt(&captured[0]);
    assert!(
        captured_prompt.contains(REVIEW_ONLY),
        "the brief handed to spawn must carry the pivot intent, got {captured_prompt:?}"
    );

    // The child opened on that brief: its first LLM request carries the pivot
    // intent, so the re-rendered goal reached the child's actual work.
    let reviewer_requests = reviewer_llm_log.requests();
    assert!(
        any_message_contains(&reviewer_requests[0].messages, REVIEW_ONLY),
        "the reviewer's opening LLM request must carry the pivot intent"
    );

    // ----- regression: the pre-pivot dangerous write never ran --------------

    assert_tool_executions(&coord_handler, DANGEROUS_WRITE, 0);
    assert_tool_executions(&reviewer_handler, DANGEROUS_WRITE, 0);
    // The reviewer did its one safe read instead.
    assert_tool_executions(&reviewer_handler, SAFE_READ, 1);

    // ----- subagent lifecycle + trace --------------------------------------

    assert_eq!(spawner.ids_calls(), 1);
    assert_eq!(spawner.spawn_calls(), 1);
    assert_eq!(spawner.summarize_calls(), 1);
    // One subagent derivation is recorded on the shared trace.
    assert_trace(&ctx).subagent_count(1);
}
