//! Core Rust suite: tool-phase basics (milestone 6, M6-3).
//!
//! Fast, offline regressions over the tool phase of a [`DefaultAgentMachine`],
//! driven to completion through the testkit [`DrainHarness`] with scripted
//! effect handlers. Each `#[tokio::test]` proves one invariant:
//!
//! - single tool — a `tool_use` opens a tool round-trip that runs the tool and
//!   folds its result back into a final assistant answer.
//! - parallel tool — a two-call batch is served and both results are committed.
//! - tool error — a failed tool result returns to the model (the fixture's
//!   `ReturnErrorToModel` policy) and the turn still commits.
//! - step limit — a one-step loop policy parks on the error cursor rather than
//!   starting a second model step.
//! - provider call mismatch — a tool result referencing an unregistered provider
//!   call id fails the append and discards the pending turn.
//!
//! Peak-concurrency of a parallel batch is proven end to end in
//! `agent_effect_e2e`, so this suite asserts the batch *ran* without repeating
//! that concurrency measurement.
//!
//! Run in isolation with `cargo test --test agent_tool_basic`.

use std::num::NonZeroU32;
use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::{
    AgentSpec, LoopCursorKind, LoopPolicy, ModelRef, ToolFailurePolicy, ToolSetRef, WorktreeRef,
};
use agent_lib::model::tool::{Tool, ToolStatus};
use serde_json::json;

/// Builds an [`AgentSpec`] like the `agent_spec_with_tools` fixture but with a
/// caller-chosen `max_steps` loop limit, so a test can force the step-limit path.
fn spec_with_step_limit(ids: &SeqIds, max_steps: u32, tools: Vec<Tool>) -> AgentSpec {
    AgentSpec::new(
        ids.agent_id(),
        WorktreeRef::new("/repo/agent-lib"),
        Some("Test agent system.".to_owned()),
        ToolSetRef::new(ids.tool_set_id(), tools),
        ModelRef::new(
            "gpt-5.5",
            NonZeroU32::new(512).expect("non-zero max tokens"),
            Some(0.1),
            None,
        ),
        LoopPolicy::new(
            NonZeroU32::new(max_steps).expect("non-zero step limit"),
            NonZeroU32::new(4).expect("non-zero parallel tools"),
            ToolFailurePolicy::ReturnErrorToModel,
        ),
    )
}

/// A single `tool_use` opens a tool round-trip: the tool runs once, its result is
/// committed, and the model's follow-up text closes the turn.
#[tokio::test]
async fn single_tool_round_trip_commits() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
    let machine = default_machine(&ids, agent_state(&ids, spec));

    let llm = ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "call-weather",
            "get_weather",
            json!({ "city": "SH" }),
        )]),
        LlmStep::text("sunny in SH"),
    ]);
    let tool = ScriptedToolHandler::from_steps([ToolStep::ok("call-weather", "sunny")]);
    let tool_log = Arc::clone(tool.log());
    let scope = TestScope::builder()
        .llm(Arc::new(llm))
        .tool(Arc::new(tool))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("weather?")
        .await
        .expect("the single-tool round-trip drains to completion");

    assert_done(observed.turn_done());
    assert_eq!(tool_log.len(), 1, "the guarded tool ran exactly once");

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .pairing_count(0, 1)
        .tool_result_status("call-weather", ToolStatus::Ok)
        .last_assistant_text("sunny in SH");
}

/// A single step's two-call `tool_use` batch is served: both tools run and both
/// results are paired into the committed turn before the final answer.
#[tokio::test]
async fn parallel_tool_batch_runs_both_calls() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
    let machine = default_machine(&ids, agent_state(&ids, spec));

    let llm = ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![
            tool_call("call-a", "get_weather", json!({ "city": "Shanghai" })),
            tool_call("call-b", "get_weather", json!({ "city": "Osaka" })),
        ]),
        LlmStep::text("both looked up"),
    ]);
    let tool = ScriptedToolHandler::from_steps([
        ToolStep::ok("call-a", "Sunny"),
        ToolStep::ok("call-b", "Cloudy"),
    ]);
    let tool_log = Arc::clone(tool.log());
    let scope = TestScope::builder()
        .llm(Arc::new(llm))
        .tool(Arc::new(tool))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("weather for both?")
        .await
        .expect("the parallel batch drains to completion");

    assert_done(observed.turn_done());
    assert_eq!(tool_log.len(), 2, "both batched tools ran");
    assert_notifications(observed.notifications())
        .tool_started_count(2)
        .tool_finished_count(2);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .pairing_count(0, 2)
        .last_assistant_text("both looked up");
}

/// A failed tool result returns to the model under the `ReturnErrorToModel`
/// policy: the error status is committed and the model's recovery text still
/// closes the turn.
#[tokio::test]
async fn tool_error_returns_to_model_and_commits() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
    let machine = default_machine(&ids, agent_state(&ids, spec));

    let llm = ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "call-weather",
            "get_weather",
            json!({ "city": "SH" }),
        )]),
        LlmStep::text("sorry, the lookup failed"),
    ]);
    let tool = ScriptedToolHandler::from_steps([ToolStep::error("call-weather", "boom")]);
    let tool_log = Arc::clone(tool.log());
    let scope = TestScope::builder()
        .llm(Arc::new(llm))
        .tool(Arc::new(tool))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("weather?")
        .await
        .expect("a tool error returns to the model and the turn drains");

    assert_done(observed.turn_done());
    assert_eq!(tool_log.len(), 1);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .tool_result_status("call-weather", ToolStatus::Error)
        .last_assistant_text("sorry, the lookup failed");
}

/// A one-step loop policy stops before the second model step: after the tool
/// runs, the machine parks on the error cursor instead of generating again, and
/// the pending turn is discarded.
#[tokio::test]
async fn step_limit_parks_on_error_before_second_model_step() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = spec_with_step_limit(&ids, 1, vec![weather_tool()]);
    let machine = default_machine(&ids, agent_state(&ids, spec));

    // Only the first model step is scripted: the limit prevents a second one.
    let llm = ScriptedLlmHandler::from_steps([LlmStep::tool_use(vec![tool_call(
        "call-weather",
        "get_weather",
        json!({ "city": "SH" }),
    )])]);
    let tool = ScriptedToolHandler::from_steps([ToolStep::ok("call-weather", "sunny")]);
    let llm_log = Arc::clone(llm.log());
    let tool_log = Arc::clone(tool.log());
    let scope = TestScope::builder()
        .llm(Arc::new(llm))
        .tool(Arc::new(tool))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("weather?")
        .await
        .expect("the step limit is a terminal error cursor, not a drive failure");

    assert_eq!(
        observed.final_cursor().kind(),
        LoopCursorKind::Error,
        "the one-step limit parks on the error cursor"
    );
    assert_eq!(llm_log.len(), 1, "only the first model step ran");
    assert_eq!(
        tool_log.len(),
        1,
        "the first tool ran before the limit tripped"
    );

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation()).pending_none();
}

/// A tool result referencing a provider call id the turn never registered fails
/// the conversation append, parking on the error cursor and discarding the
/// pending turn without committing anything.
#[tokio::test]
async fn provider_call_mismatch_discards_the_pending_turn() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
    let machine = default_machine(&ids, agent_state(&ids, spec));

    let llm = ScriptedLlmHandler::from_steps([LlmStep::tool_use(vec![tool_call(
        "call-weather",
        "get_weather",
        json!({ "city": "SH" }),
    )])]);
    // The scripted tool answers a *different* provider call id than the one in
    // flight, so appending its result cannot pair and the turn fails.
    let tool = ScriptedToolHandler::from_steps([ToolStep::ok("unknown-call", "wrong result")]);
    let tool_log = Arc::clone(tool.log());
    let scope = TestScope::builder()
        .llm(Arc::new(llm))
        .tool(Arc::new(tool))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("weather?")
        .await
        .expect("a mismatched provider call id is a terminal error cursor");

    assert_eq!(
        observed.final_cursor().kind(),
        LoopCursorKind::Error,
        "the mismatched provider call parks on the error cursor"
    );
    assert_eq!(
        tool_log.len(),
        1,
        "the tool ran; its result could not be paired"
    );

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(0)
        .pending_none();
}
