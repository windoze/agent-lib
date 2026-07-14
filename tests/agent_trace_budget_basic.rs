//! Core Rust suite: trace and budget accounting basics (milestone 6, M6-3).
//!
//! Fast, offline regressions over the run-context *observability* the reference
//! [`drain`] driver and [`RunContext`](agent_lib::agent::RunContext) maintain:
//! the per-requirement trace ledger and the shared budget ledger. A
//! [`ScriptMachine`] double over explicit [`TestScope`]s keeps the focus on what
//! the driver records rather than on machine folding. One `#[tokio::test]` per
//! invariant:
//!
//! - resolved-at-scope — a batch with one locally-served tool and one popped
//!   interaction records hop-0 and hop-1 `resolved_at_scope` distances, both
//!   `Resumed`.
//! - never-resumed on cancel — a cancelled turn abandons its pending requirement
//!   without calling the handler and traces it as `NeverResumed`.
//! - shared budget ledger — a derived child shares the parent's budget ledger,
//!   so a child charge is visible on the parent, the child sits one level
//!   deeper, and the parent trace gains a subagent node.
//!
//! Run in isolation with `cargo test --test agent_trace_budget_basic`.

use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::{RequirementDisposition, RequirementKindTag, ScopePop, drain};
use serde_json::json;

/// A batch resolved across two layers records each requirement's pop distance:
/// the locally-served tool at hop 0, the popped interaction at hop 1, both
/// `Resumed`.
#[tokio::test]
async fn resolved_at_scope_spans_local_and_popped_layers() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let tool_req = ids.requirement_id();
    let interaction_req = ids.requirement_id();
    let mut machine = ScriptMachine::builder()
        .requirements([
            Requirement::at_root(
                tool_req,
                RequirementKind::NeedTool {
                    call_id: ids.tool_call_id(),
                    call: tool_call("call-a", "note", json!({ "text": "record" })),
                },
            ),
            Requirement::at_root(
                interaction_req,
                RequirementKind::NeedInteraction {
                    request: Interaction::question(ids.step_id(), "confirm?".to_owned()),
                },
            ),
        ])
        .done_after_all_resumed()
        .label("trace")
        .build();

    // The emitting (inner) scope serves the tool; the interaction pops to outer.
    let inner_tool = ScriptedToolHandler::from_steps([ToolStep::ok("call-a", "noted")]);
    let inner = TestScope::builder().tool(Arc::new(inner_tool)).build();
    let outer = TestScope::builder()
        .attended(Arc::new(ScriptedInteractionHandler::approve_all()))
        .build();
    let mut parent = ScopePop::new(&outer, None);

    let done = drain(
        &mut machine,
        user_input(&ids, "go"),
        &inner,
        Some(&mut parent),
        &ctx,
    )
    .await
    .expect("the two-layer batch drains to completion");
    assert_done(&done);

    assert_trace(&ctx).requirement_count(2).subagent_count(0);
    // The tool was settled in place by the emitting scope: hop 0.
    assert_trace(&ctx)
        .requirement(tool_req)
        .tag(RequirementKindTag::Tool)
        .resolved_at_scope(0)
        .disposition(RequirementDisposition::Resumed);
    // The interaction popped one layer out to the attended parent: hop 1.
    assert_trace(&ctx)
        .requirement(interaction_req)
        .tag(RequirementKindTag::Interaction)
        .resolved_at_scope(1)
        .disposition(RequirementDisposition::Resumed);
}

/// A cancelled turn abandons its pending requirement instead of fulfilling it:
/// the handler is never called and the never-resume is still traced.
#[tokio::test]
async fn cancel_records_never_resumed_without_calling_handler() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let req_id = ids.requirement_id();
    let mut machine = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            req_id,
            RequirementKind::NeedTool {
                call_id: ids.tool_call_id(),
                call: tool_call("call-a", "note", json!({ "text": "record" })),
            },
        ))
        .done_after_all_resumed()
        .label("cancelled")
        .build();

    let tool = ScriptedToolHandler::from_steps([ToolStep::ok("call-a", "noted")]);
    let tool_log = Arc::clone(tool.log());
    let scope = TestScope::builder().tool(Arc::new(tool)).build();

    // Cancelling before the drive abandons the batch's requirement.
    ctx.cancellation().cancel();

    drain(&mut machine, user_input(&ids, "go"), &scope, None, &ctx)
        .await
        .expect("a cancelled drain still closes the turn");

    assert_eq!(tool_log.len(), 0, "a cancelled turn never runs the tool");
    assert_eq!(
        machine.abandon_count(),
        1,
        "the requirement was abandoned once"
    );
    assert_trace(&ctx)
        .requirement(req_id)
        .tag(RequirementKindTag::Tool)
        .resolved_at_scope(0)
        .disposition(RequirementDisposition::NeverResumed);
}

/// A derived child shares the parent's budget ledger: a child charge is visible
/// on the parent snapshot, the child sits one level deeper, and the parent trace
/// records the subagent node.
#[tokio::test]
async fn derived_child_shares_the_budget_ledger() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);

    let child = ctx
        .derive_child(ids.run_id(), ids.trace_node("child"))
        .expect("deriving a child context succeeds");
    child
        .charge_tokens(7)
        .expect("charging the shared ledger succeeds");

    // The charge lands on the ledger both contexts share.
    assert_budget(&ctx).steps(0).tokens(7).cost_micros(0);
    assert_budget(&child).steps(0).tokens(7).cost_micros(0);
    assert_eq!(
        child.depth(),
        ctx.depth() + 1,
        "the child sits one level deeper than its parent"
    );
    // Deriving the child recorded a subagent node on the parent trace.
    assert_trace(&ctx).subagent_count(1).requirement_count(0);
}
