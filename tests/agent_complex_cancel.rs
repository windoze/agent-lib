//! Cancel during a subagent/tool wait is never-resume (milestone 3, M3-2).
//!
//! `docs/complex-tests.md` §4.3 pins the never-resume contract: a cancel that
//! lands while a child is mid-flight abandons the child's *outstanding*
//! requirement instead of resuming it, the side effect that requirement would
//! have produced never happens, the side effects that already committed are left
//! untouched, and the agent stays usable for a fresh turn afterwards. This test
//! threads all of that over one shared [`MockPlanBlackboardStore`]:
//!
//! - **Cancel abandons the child.** The parent seeds a `review` task, claims it
//!   under `worker` (so it is `InProgress`), and records the worker's committed
//!   `"review started"` progress on the shared board. A child
//!   [`ScriptMachine`] then stands on a single `NeedTool` — a `plan_update` that
//!   *would* mark `review` completed. The parent cancels the run context and
//!   drives the subagent handler: because the child context is derived from the
//!   parent (cancel ↓), the child drain sees the cancellation at the top of its
//!   loop and abandons that tool call before it ever reaches the tool handler.
//!   The child machine settles through its never-resume path (`abandon_count ==
//!   1`, `resume_count == 0`), the tool never runs, `review` stays `InProgress`,
//!   and the board still holds exactly the one `"review started"` message. The
//!   trace records the abandoned tool requirement as `NeverResumed` at the
//!   performing layer.
//! - **The agent continues after the stop.** A cancel is scoped to the run it
//!   fired on, not to the store or to a machine, so a *fresh* context drives one
//!   more committed turn: a real [`DefaultAgentMachine`] records the cancellation
//!   on the board and marks `review` `Cancelled`. The board grows to exactly
//!   `["review started", "review cancelled"]` — no duplicated `"review started"`,
//!   no completed side effect — and the turn commits cleanly.
//!
//! Cancellation timing is deterministic and offline: the context is cancelled
//! explicitly before the subagent is driven (the shape the reference
//! `parent_cancel_propagates_and_abandons_child` unit test exercises), so there
//! is no real clock, sleep, or race.
//!
//! Run in isolation with `cargo test --test agent_complex_cancel`.

#[path = "complex_support/mod.rs"]
mod complex_support;

use std::sync::Arc;

use serde_json::json;

use agent_lib::agent::{
    LoopCursorKind, PlanId, Requirement, RequirementKind, RequirementKindTag, RequirementResult,
    ScopePop, SubagentHandler, ToolHandler, drain,
};

use agent_testkit::prelude::*;

use complex_support::assertions::{
    assert_board_messages, assert_task_owner, assert_task_status, assert_tool_executions,
};
use complex_support::plan_blackboard::{MockPlanBlackboardStore, TaskStatus};
use complex_support::tools::{
    BLACKBOARD_POST, PLAN_UPDATE, complex_agent_machine, complex_scope, complex_tool_handler,
};

/// Fixed plan id so store construction stays deterministic and offline.
fn plan_id() -> PlanId {
    PlanId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890d2").expect("valid plan id")
}

/// Seeds `store` with a single `review` task claimed by `worker` and records the
/// worker's committed `"review started"` progress on the board, returning the
/// plan version after seeding.
///
/// This models the state the run is in *before* the cancel: the child worker has
/// already claimed the task (so it is `InProgress`) and announced it started, but
/// has not yet completed it. Seeding runs directly against the store so the
/// child's scripted `expected_version` can be pinned: create (v0), add `review`
/// (v1), claim it (v2).
fn seed_started_review(store: &MockPlanBlackboardStore) -> u64 {
    store.create_plan();
    store
        .add_task("review", Vec::<&str>::new())
        .expect("add review");
    store.claim("review", "worker", 1).expect("claim review");
    store.post("worker", "review started");
    store.version()
}

/// A cancel that lands during a subagent's tool wait abandons the child's
/// outstanding call, preserves the committed state, and leaves the agent usable.
///
/// Phase A drives the subagent handler under a cancelled context and asserts the
/// never-resume outcome; phase B drives a fresh committed turn over the same
/// store to prove the cancel wedged neither the store nor a machine.
#[tokio::test]
async fn complex_cancel_abandons_child_and_preserves_committed_state() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);

    // Shared store: a `review` task already claimed by the worker, with the
    // worker's "review started" progress committed to the board.
    let store = Arc::new(MockPlanBlackboardStore::new(plan_id()));
    let seeded_version = seed_started_review(&store);
    assert_eq!(
        seeded_version, 2,
        "seeding leaves the plan at a known version"
    );

    // ----- phase A: cancel abandons the child's outstanding tool -----------

    // The child stands on a single tool call that *would* complete `review`. It
    // is the side effect that must never happen once the run is cancelled.
    let child_tool_req_id = ids.requirement_id();
    let child = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            child_tool_req_id,
            RequirementKind::NeedTool {
                call_id: ids.tool_call_id(),
                call: tool_call(
                    "child-complete",
                    PLAN_UPDATE,
                    json!({
                        "task": "review",
                        "owner": "worker",
                        "status": "completed",
                        "expected_version": 2
                    }),
                ),
            },
        ))
        .idle_on_abandon()
        .label("child")
        .build();
    let child_log = Arc::clone(child.log());

    // The child's tool handler mutates the *shared* store, so "it never ran" is
    // observable both as a zero execution count and as an untouched plan.
    let child_tool = complex_tool_handler(Arc::clone(&store));
    let spawned = SpawnedChildBuilder::new()
        .machine(child)
        .scope(
            headless_child_scope()
                .tool(Arc::clone(&child_tool) as Arc<dyn ToolHandler>)
                .build(),
        )
        .opening(user_input(&ids, "finish the review"))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(spawned)
            .summary("review cancelled before completion")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(4);

    // The outer layer the child pops to; nothing pops here because the child's
    // one requirement is abandoned before it is ever fulfilled.
    let outer_scope = TestScope::builder().build();
    let mut outer = ScopePop::new(&outer_scope, None);

    let spec_ref = AgentSpecRef(ids.agent_id());
    let brief = Interaction::question(ids.step_id(), "finish the review".to_owned());

    // Cancel the run, then drive the subagent: derivation inherits the cancel, so
    // the child drain abandons its first requirement (never-resume).
    ctx.cancellation().cancel();
    let result = handler
        .fulfill(&spec_ref, &brief, None, &mut outer, &ctx)
        .await;

    // The subagent still closed cleanly (drain returned Ok via the never-resume
    // path, so the handler summarized): cancel is an orderly stop, not an error.
    assert!(
        matches!(result, RequirementResult::Subagent(Ok(_))),
        "a cancelled child drains to an orderly summary, not an error: {result:?}"
    );
    assert_eq!(spawner.ids_calls(), 1);
    assert_eq!(spawner.spawn_calls(), 1);
    assert_eq!(spawner.summarize_calls(), 1);

    // The child's outstanding requirement was abandoned, never resumed.
    assert_eq!(
        child_log.abandon_count(),
        1,
        "the child's tool was abandoned"
    );
    assert_eq!(
        child_log.resume_count(),
        0,
        "the abandoned tool was never resumed"
    );

    // The tool that should not run after cancel never ran: its handler log is
    // empty and the shared plan is untouched.
    assert_tool_executions(&child_tool, PLAN_UPDATE, 0);
    assert!(
        child_tool.calls().is_empty(),
        "no child tool executed under a cancelled context"
    );
    // The abandoned completion never landed: `review` is still the worker's
    // in-progress claim, not `Completed`.
    assert_task_status(&store, "review", TaskStatus::InProgress);
    assert_task_owner(&store, "review", "worker");
    // The only committed side effect is the worker's earlier "review started".
    assert_board_messages(&store, &["review started"]);

    // The trace records the abandoned tool as never-resumed at the performing
    // layer, under exactly one derived subagent node.
    assert_trace(&ctx).subagent_count(1);
    assert_trace(&ctx)
        .requirement(child_tool_req_id)
        .tag(RequirementKindTag::Tool)
        .resolved_at_scope(0)
        .never_resumed();

    // ----- phase B: the agent continues after the stop ---------------------

    // A cancel is scoped to its run, so a fresh context drives one more committed
    // turn over the same store: record the cancellation and mark `review`
    // cancelled. `plan_update` from `InProgress` to `Cancelled` is a legal
    // transition and uses the worker's still-held claim.
    let cleanup_ctx = root_context(&ids);
    let cleanup_llm = ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![
            tool_call(
                "cleanup-post",
                BLACKBOARD_POST,
                json!({ "sender": "parent", "text": "review cancelled" }),
            ),
            tool_call(
                "cleanup-update",
                PLAN_UPDATE,
                json!({
                    "task": "review",
                    "owner": "worker",
                    "status": "cancelled",
                    "expected_version": 2
                }),
            ),
        ]),
        LlmStep::text("recorded the cancellation"),
    ]);
    let cleanup_tool = complex_tool_handler(Arc::clone(&store));
    let cleanup_scope = complex_scope(
        Arc::new(cleanup_llm),
        Arc::clone(&cleanup_tool) as Arc<dyn ToolHandler>,
        None,
    );

    let mut cleanup = complex_agent_machine(&ids);
    let done = drain(
        &mut cleanup,
        user_input(&ids, "record that the review was cancelled"),
        &cleanup_scope,
        None,
        &cleanup_ctx,
    )
    .await
    .expect("a fresh turn commits after the cancel");
    assert_eq!(
        done.cursor().kind(),
        LoopCursorKind::Done,
        "the follow-up turn closes cleanly"
    );

    // The follow-up marked `review` cancelled (never completed) and appended the
    // cancellation to the board with no duplicated "review started".
    assert_task_status(&store, "review", TaskStatus::Cancelled);
    assert_board_messages(&store, &["review started", "review cancelled"]);

    // The follow-up committed exactly one turn with nothing left pending, proving
    // a machine stays usable after a cancelled run.
    assert_conversation(cleanup.state().conversation())
        .committed_turns(1)
        .pending_none();
}
