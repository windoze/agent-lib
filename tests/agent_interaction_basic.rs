//! Core Rust suite: interaction / approval basics (milestone 6, M6-3).
//!
//! Fast, offline regressions over the approval gate of a
//! [`DefaultAgentMachine`], driven to completion through the testkit
//! [`DrainHarness`]. A minimal require-approval policy forces every guarded tool
//! call through a `NeedInteraction`, and a scripted interaction backend supplies
//! each disposition. One `#[tokio::test]` per invariant:
//!
//! - approve — the approval is granted, so the guarded tool runs and its `Ok`
//!   result is committed.
//! - deny — the approval is denied; the tool never runs and a `Denied` result is
//!   synthesized and returned to the model.
//! - timeout — an approval timeout folds into a `Denied` result, tool unrun.
//! - cancel — an approval cancel folds into a `Cancelled` result, tool unrun.
//! - wrong call/step rejection — an approval response addressing a different
//!   step/call is rejected by the driver's return-path check before the machine
//!   is resumed, and the guarded tool never runs.
//!
//! The require-approval *policy* is a spec-level decision, not a mockable effect
//! boundary, so `agent-testkit` deliberately does not ship one; this suite
//! carries a minimal local policy (mirroring `agent_effect_e2e`).
//!
//! Run in isolation with `cargo test --test agent_interaction_basic`.

use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::{
    AgentError, ApprovalRequirement, ApprovalResponse, InteractionResponse, ToolApprovalPolicy,
};
use agent_lib::conversation::ToolCallId;
use agent_lib::model::tool::{ToolCall, ToolStatus};
use serde_json::json;

/// Approval policy that requires human approval for every tool call, forcing the
/// guarded weather call through a `NeedInteraction`.
#[derive(Debug)]
struct RequireApprovalPolicy;

impl ToolApprovalPolicy for RequireApprovalPolicy {
    fn approval_requirement(&self, _call_id: ToolCallId, _call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::required(Some("human approval required".to_owned()))
    }
}

/// Scripts the guarded weather round-trip: request the tool, then answer once its
/// (possibly synthesized) result returns to the model.
fn guarded_weather_llm() -> ScriptedLlmHandler {
    ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "call-weather",
            "get_weather",
            json!({ "city": "SH" }),
        )]),
        LlmStep::text("done"),
    ])
}

/// A tool handler that answers the guarded weather call once (only reached when
/// the approval is granted).
fn weather_tool_handler() -> ScriptedToolHandler {
    ScriptedToolHandler::from_steps([ToolStep::ok("call-weather", "sunny")])
}

/// A guarded machine: the weather tool is declared and every call requires
/// approval.
fn guarded_machine(ids: &SeqIds) -> DefaultAgentMachine {
    default_machine(
        ids,
        agent_state(ids, agent_spec_with_tools(ids, vec![weather_tool()])),
    )
    .with_approval_policy(Arc::new(RequireApprovalPolicy))
}

/// A granted approval lets the guarded tool run: its `Ok` result is committed and
/// the model's follow-up closes the turn.
#[tokio::test]
async fn approve_runs_the_guarded_tool() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let machine = guarded_machine(&ids);

    let tool = weather_tool_handler();
    let tool_log = Arc::clone(tool.log());
    let interaction = ScriptedInteractionHandler::approve_all();
    let interaction_log = Arc::clone(interaction.log());
    let scope = TestScope::builder()
        .llm(Arc::new(guarded_weather_llm()))
        .tool(Arc::new(tool))
        .attended(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("weather?")
        .await
        .expect("the approved turn drains to completion");

    assert_done(observed.turn_done());
    assert_eq!(interaction_log.len(), 1, "the approval was answered once");
    assert_eq!(tool_log.len(), 1, "granting the approval ran the tool");

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .tool_result_status("call-weather", ToolStatus::Ok)
        .last_assistant_text("done");
}

/// A denied approval synthesizes a `Denied` result without running the tool and
/// returns it to the model, which still closes the turn.
#[tokio::test]
async fn deny_synthesizes_a_denied_result() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let machine = guarded_machine(&ids);

    let tool = weather_tool_handler();
    let tool_log = Arc::clone(tool.log());
    let interaction = ScriptedInteractionHandler::deny_all(Some("not allowed".to_owned()));
    let interaction_log = Arc::clone(interaction.log());
    let scope = TestScope::builder()
        .llm(Arc::new(guarded_weather_llm()))
        .tool(Arc::new(tool))
        .attended(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("weather?")
        .await
        .expect("the denied turn drains to completion");

    assert_done(observed.turn_done());
    assert_eq!(interaction_log.len(), 1);
    assert_eq!(tool_log.len(), 0, "a denied approval never runs the tool");

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .tool_result_status("call-weather", ToolStatus::Denied)
        .last_assistant_text("done");
}

/// An approval timeout folds into a `Denied` tool result — exactly as an attended
/// backend that never answered in time — without running the tool.
#[tokio::test]
async fn timeout_folds_into_a_denied_result() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let machine = guarded_machine(&ids);

    let tool = weather_tool_handler();
    let tool_log = Arc::clone(tool.log());
    let interaction = ScriptedInteractionHandler::fixed(InteractionDecision::Timeout(None));
    let scope = TestScope::builder()
        .llm(Arc::new(guarded_weather_llm()))
        .tool(Arc::new(tool))
        .attended(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("weather?")
        .await
        .expect("the timed-out turn drains to completion");

    assert_done(observed.turn_done());
    assert_eq!(
        tool_log.len(),
        0,
        "a timed-out approval never runs the tool"
    );

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .tool_result_status("call-weather", ToolStatus::Denied);
}

/// An approval cancel folds into a `Cancelled` tool result without running the
/// tool.
#[tokio::test]
async fn cancel_marks_the_result_cancelled() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let machine = guarded_machine(&ids);

    let tool = weather_tool_handler();
    let tool_log = Arc::clone(tool.log());
    let interaction = ScriptedInteractionHandler::fixed(InteractionDecision::Cancel(None));
    let scope = TestScope::builder()
        .llm(Arc::new(guarded_weather_llm()))
        .tool(Arc::new(tool))
        .attended(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("weather?")
        .await
        .expect("the cancelled-approval turn drains to completion");

    assert_done(observed.turn_done());
    assert_eq!(
        tool_log.len(),
        0,
        "a cancelled approval never runs the tool"
    );

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .tool_result_status("call-weather", ToolStatus::Cancelled);
}

/// An approval response addressing a different step/call than the one in flight
/// is rejected by the driver's return-path family/aim check before the machine
/// is resumed, so the turn fails and the guarded tool never runs.
#[tokio::test]
async fn wrong_call_approval_is_rejected() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let machine = guarded_machine(&ids);

    // A fresh step/call id from the same tree is guaranteed distinct from the
    // one the in-flight approval addresses, so this response is misaimed.
    let wrong =
        InteractionResponse::Approval(ApprovalResponse::approve(ids.step_id(), ids.tool_call_id()));

    let tool = weather_tool_handler();
    let tool_log = Arc::clone(tool.log());
    let interaction = ScriptedInteractionHandler::fixed(InteractionDecision::Response(wrong));
    let scope = TestScope::builder()
        .llm(Arc::new(guarded_weather_llm()))
        .tool(Arc::new(tool))
        .attended(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let error = harness
        .run_user("weather?")
        .await
        .expect_err("a misaimed approval must fail the turn");

    match error {
        AgentError::Other(message) => assert!(
            message.contains("misaligned"),
            "expected a misalignment diagnostic, got: {message}"
        ),
        other => panic!("expected AgentError::Other, got {other:?}"),
    }
    assert_eq!(tool_log.len(), 0, "a rejected approval never runs the tool");
}
