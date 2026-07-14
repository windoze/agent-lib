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
//! Milestone 4 (M4-2) adds the sibling distinction `docs/complex-tests.md` §5.2
//! P1-2 pins: an approval `Cancel` is **not** a context cancel. This file's
//! second test threads both halves of that contrast over one machine each:
//!
//! - **Approval `Cancel` only cancels the one guarded call.** A gated
//!   `dangerous_write` is resolved with
//!   [`InteractionDecision::Cancel`](agent_testkit::handlers::InteractionDecision::Cancel):
//!   the machine folds it into a synthesized [`ToolStatus::Cancelled`] result,
//!   the dangerous tool never runs, and the LLM loop keeps going — a following
//!   `safe_read` executes and a final answer commits the turn. Crucially the
//!   run context stays alive (`is_cancelled() == false`), and the trace records
//!   no never-resume.
//! - **A driver cancel abandons the outstanding requirement.** A
//!   [`CancelOnCall`] wrapper cancels the run context right after the model
//!   emits its `safe_read` tool call, so the reference driver abandons that
//!   still-open `NeedTool` on the never-resume path: the tool handler never
//!   runs, `is_cancelled() == true`, and the trace records the tool requirement
//!   as `NeverResumed`.
//!
//! Run in isolation with `cargo test --test agent_complex_cancel`.

#[path = "complex_support/mod.rs"]
mod complex_support;

use std::sync::Arc;

use serde_json::json;

use agent_lib::agent::{
    InteractionHandler, LlmHandler, LoopCursorKind, PlanId, Requirement, RequirementDisposition,
    RequirementId, RequirementKind, RequirementKindTag, RequirementResult, RunContext, ScopePop,
    SubagentHandler, ToolHandler, TraceNodeKind, drain,
};
use agent_lib::model::tool::ToolStatus;

use agent_testkit::prelude::*;

use complex_support::assertions::{
    assert_board_messages, assert_interaction_decisions, assert_task_owner, assert_task_status,
    assert_tool_executions,
};
use complex_support::plan_blackboard::{MockPlanBlackboardStore, TaskStatus};
use complex_support::tools::{
    BLACKBOARD_POST, DANGEROUS_WRITE, PLAN_UPDATE, SAFE_READ, complex_agent_machine, complex_scope,
    complex_tool_handler,
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

// ----- M4-2: approval cancel vs context cancel -----------------------------

/// Fixed plan id for the approval-cancel half, kept distinct from the
/// context-cancel half so the two stores never share identity.
fn approval_cancel_plan_id() -> PlanId {
    PlanId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890e1").expect("valid plan id")
}

/// Fixed plan id for the context-cancel half.
fn context_cancel_plan_id() -> PlanId {
    PlanId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890e2").expect("valid plan id")
}

/// Returns the ids of every requirement the trace recorded as `NeverResumed`.
///
/// The reference driver keys a requirement's trace node by the requirement's own
/// id, so a `NeverResumed` node's id parses straight back to a
/// [`RequirementId`]. A test uses this to tell the two cancels apart: an approval
/// `Cancel` leaves this empty (every requirement resumed), while a context cancel
/// records exactly the abandoned requirement.
fn never_resumed_requirement_ids(ctx: &RunContext) -> Vec<RequirementId> {
    ctx.trace()
        .records()
        .iter()
        .filter_map(|record| match record.kind() {
            TraceNodeKind::Requirement {
                disposition: RequirementDisposition::NeverResumed,
                ..
            } => record.id().as_str().parse::<RequirementId>().ok(),
            _ => None,
        })
        .collect()
}

/// An approval `Cancel` cancels only the one guarded tool call; a driver cancel
/// of the run context is what abandons an outstanding requirement.
///
/// Phase A resolves a gated `dangerous_write` with
/// [`InteractionDecision::Cancel`] and proves the run keeps going: the dangerous
/// tool never runs, a following `safe_read` does, the turn commits with a final
/// answer, and the context is still live with no never-resume in the trace. Phase
/// B — a fresh machine over its own store — cancels the run context right after
/// the model emits a `safe_read` call, so the reference driver abandons that
/// outstanding `NeedTool`: the tool handler never runs, the context reads
/// cancelled, and the trace records the tool requirement as `NeverResumed`.
#[tokio::test]
async fn complex_approval_cancel_does_not_cancel_context_unless_driver_cancels() {
    // ----- phase A: approval Cancel does not cancel the context ------------

    let ids_a = SeqIds::new();
    let ctx_a = root_context(&ids_a);

    let store_a = Arc::new(MockPlanBlackboardStore::new(approval_cancel_plan_id()));
    let handler_a = complex_tool_handler(Arc::clone(&store_a));

    // The single approval the turn raises (for `dangerous_write`) is cancelled.
    let interaction = ScriptedInteractionHandler::sequence([InteractionDecision::Cancel(Some(
        "not now".to_owned(),
    ))]);
    let interaction_log = Arc::clone(interaction.log());

    // The model asks for the gated write, then — once its call is cancelled —
    // keeps going with a benign read and a closing answer.
    let llm_a = ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "a-danger",
            DANGEROUS_WRITE,
            json!({ "text": "apply the risky change to file A" }),
        )]),
        LlmStep::tool_use(vec![tool_call("a-safe", SAFE_READ, json!({}))]),
        LlmStep::text("continued after the approval cancel"),
    ]);

    let scope_a = complex_scope(
        Arc::new(llm_a),
        Arc::clone(&handler_a) as Arc<dyn ToolHandler>,
        Some(Arc::new(interaction) as Arc<dyn InteractionHandler>),
    );

    let machine_a = complex_agent_machine(&ids_a);
    let mut harness_a = DrainHarness::with_ids(machine_a, &scope_a, None, &ctx_a, ids_a);
    let observed_a = harness_a
        .run_user("实现功能 A")
        .await
        .expect("the approval-cancel turn drains to completion");
    assert_done(observed_a.turn_done());

    // The approval `Cancel` did not touch the run context: it is still live.
    assert!(
        !ctx_a.is_cancelled(),
        "an approval Cancel must not cancel the run context"
    );

    // Exactly one approval was rendered (the gated write), and it was a Cancel:
    // the folded `Cancelled` tool result below is the observable proof of that
    // decision (a Deny would fold to `Denied`).
    assert_interaction_decisions(&interaction_log, 1);

    // The cancelled dangerous write never executed, but the loop continued and
    // the following safe read did — so the cancel scoped to the single call.
    assert_tool_executions(&handler_a, DANGEROUS_WRITE, 0);
    assert_tool_executions(&handler_a, SAFE_READ, 1);

    // The cancelled write left no side effect on the shared board.
    assert_board_messages(&store_a, &[]);

    // No requirement was abandoned: an approval Cancel is a normal in-turn
    // resolution, not a never-resume.
    assert!(
        never_resumed_requirement_ids(&ctx_a).is_empty(),
        "an approval Cancel records no never-resumed requirement, found {:?}",
        never_resumed_requirement_ids(&ctx_a)
    );

    let machine_a = harness_a.into_machine();
    assert_conversation(machine_a.state().conversation())
        .committed_turns(1)
        .pending_none()
        .tool_result_status("a-danger", ToolStatus::Cancelled)
        .tool_result_status("a-safe", ToolStatus::Ok)
        .last_assistant_text("continued after the approval cancel");

    // ----- phase B: a driver cancel abandons the outstanding requirement ---

    // A fresh machine over its own store and context, so phase A's committed
    // state cannot leak into the never-resume observation.
    let ids_b = SeqIds::new();
    let ctx_b = root_context(&ids_b);

    let store_b = Arc::new(MockPlanBlackboardStore::new(context_cancel_plan_id()));
    let handler_b = complex_tool_handler(Arc::clone(&store_b));

    // The model emits a benign tool call; the `CancelOnCall::after` wrapper then
    // cancels the run context as that LLM step resolves, modelling a driver stop
    // that lands with a tool call outstanding. The scripted step never needs a
    // second call: the outstanding tool is abandoned before the model resumes.
    let cancel_llm = Arc::new(CancelOnCall::after(ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call("b-safe", SAFE_READ, json!({}))]),
    ])));
    let cancel_log = Arc::clone(cancel_llm.log());

    let scope_b = complex_scope(
        Arc::clone(&cancel_llm) as Arc<dyn LlmHandler>,
        Arc::clone(&handler_b) as Arc<dyn ToolHandler>,
        None,
    );

    let machine_b = complex_agent_machine(&ids_b);
    let mut harness_b = DrainHarness::with_ids(machine_b, &scope_b, None, &ctx_b, ids_b);
    harness_b
        .run_user("实现功能 B")
        .await
        .expect("a cancelled drain closes the turn without erroring");

    // The driver cancel did cancel the run context — the opposite of phase A.
    assert!(
        ctx_b.is_cancelled(),
        "an explicit context cancel must mark the run cancelled"
    );

    // The cancel fired once, as the first (and only) LLM step resolved.
    assert!(cancel_llm.cancelled(), "the wrapper fired its cancel");
    assert_eq!(
        cancel_log.cancelled_at(),
        Some(0),
        "the cancel fired on the first LLM dispatch"
    );
    assert_eq!(
        cancel_llm.dispatched(),
        1,
        "the model was not asked to resume after the abandon"
    );

    // The outstanding tool was abandoned on the never-resume path, so its handler
    // never ran — the store-mutating `safe_read` never touched the store.
    assert_tool_executions(&handler_b, SAFE_READ, 0);
    assert_board_messages(&store_b, &[]);

    // The trace tells the two cancels apart: phase B abandons exactly one
    // requirement, and it is the outstanding tool, settled at the performing
    // layer.
    let abandoned = never_resumed_requirement_ids(&ctx_b);
    assert_eq!(
        abandoned.len(),
        1,
        "a context cancel abandons exactly the outstanding requirement, found {abandoned:?}"
    );
    assert_trace(&ctx_b)
        .requirement(abandoned[0])
        .tag(RequirementKindTag::Tool)
        .resolved_at_scope(0)
        .never_resumed();

    // The abandoned tool phase leaves a *coherent* but uncommitted turn: the
    // never-resume synthesizes a `Cancelled` result for the outstanding call (so
    // there is no dangling tool_use and no open call), yet the turn never closes
    // on a final answer, so it stays pending rather than committing.
    let machine_b = harness_b.into_machine();
    assert_conversation(machine_b.state().conversation())
        .committed_turns(0)
        .pending_present()
        .open_call_count(0)
        .tool_result_status("b-safe", ToolStatus::Cancelled);
}
