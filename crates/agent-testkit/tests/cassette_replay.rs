//! M3-4: the first offline recorded-replay integration test.
//!
//! This test proves that a whole agent turn can run in CI with **no network,
//! credentials, or live tool backend**: it loads a committed cassette
//! ([`agent_weather_tool_roundtrip.json`](../tests/cassettes)) and drives a real
//! [`DefaultAgentMachine`](agent_lib::agent::DefaultAgentMachine) through a
//! user -> LLM `tool_use` -> tool result -> LLM final-text round-trip using only
//! the cassette replay handlers ([`CassetteLlmHandler`] and
//! [`CassetteToolHandler`]).
//!
//! The fixture is *synthetic* recorded data, but it is written in the exact
//! shape [`CassetteRecorder`] produces: [`regenerate_weather_cassette`] records
//! the same scenario through the recorder and, when
//! [`AGENT_TESTKIT_UPDATE_CASSETTES=1`](agent_testkit::cassette::UPDATE_ENV_VAR)
//! is set, rewrites the committed file. Recording the request the machine
//! actually issues is what keeps the entry fingerprints in lock-step with what
//! the machine reproduces on replay. A normal CI run leaves that test a no-op
//! (`RecorderReport::Skipped`) so the committed fixture is never overwritten.

use std::path::PathBuf;
use std::sync::Arc;

use agent_lib::agent::drain;
use agent_lib::model::content::ContentBlock;
use agent_lib::model::message::Role;
use agent_lib::model::tool::ToolStatus;
use agent_testkit::prelude::*;

/// City the scripted model looks up.
const CITY: &str = "Shanghai";
/// Provider tool-call id shared by the tool-use request and its result.
const CALL_ID: &str = "call-weather";
/// The tool's recorded, model-visible reply.
const WEATHER_REPLY: &str = "Sunny, 20C in Shanghai.";
/// The assistant's final answer, synthesized from the tool reply.
const FINAL_ANSWER: &str = "It is sunny in Shanghai.";

/// Absolute path to the committed cassette fixture.
fn cassette_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/cassettes/agent_weather_tool_roundtrip.json")
}

/// The scripted LLM handler for the weather round-trip: it first asks for the
/// `get_weather` tool, then answers from the tool result. Usages are non-zero so
/// the recorded result payloads carry realistic token counts.
fn scripted_llm() -> ScriptedLlmHandler {
    ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call(
            CALL_ID,
            "get_weather",
            serde_json::json!({ "city": CITY }),
        )])
        .with_usage(usage(5, 2)),
        LlmStep::text(FINAL_ANSWER).with_usage(usage(6, 4)),
    ])
}

/// The scripted tool handler that serves the single `get_weather` call.
fn scripted_tool() -> ScriptedToolHandler {
    ScriptedToolHandler::from_steps([ToolStep::ok(CALL_ID, WEATHER_REPLY)])
}

/// Records the weather round-trip through [`CassetteRecorder`] and rewrites the
/// committed fixture — but only when `AGENT_TESTKIT_UPDATE_CASSETTES=1`.
///
/// This is the fixture's source of truth: it wraps the scripted handlers so the
/// recorder captures the *exact* requests a real [`DefaultAgentMachine`] issues,
/// guaranteeing the entry fingerprints match what the replay test reproduces. A
/// default CI run leaves it a no-op and never touches the committed file.
#[tokio::test]
async fn regenerate_weather_cassette() {
    let recorder = CassetteRecorder::update(cassette_path()).with_metadata(
        CassetteMetadata::new("agent_weather_tool_roundtrip").with_description(
            "user -> LLM tool_use -> get_weather result -> LLM final text; no network or credentials",
        ),
    );

    // A default CI run never opts in, so the fixture is left untouched.
    if let Some(reason) = recorder.skip_reason() {
        assert!(reason.contains("AGENT_TESTKIT_UPDATE_CASSETTES"));
        return;
    }

    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
    let mut machine = default_machine(&ids, agent_state(&ids, spec));

    let scope = TestScope::builder()
        .llm(Arc::new(recorder.wrap_llm(scripted_llm())))
        .tool(Arc::new(recorder.wrap_tool(scripted_tool())))
        .build();

    drain(
        &mut machine,
        user_input(&ids, "weather?"),
        &scope,
        None,
        &ctx,
    )
    .await
    .expect("the recorded turn drains to completion");

    let report = recorder.finish().expect("cassette write succeeds");
    match report {
        RecorderReport::Wrote { entry_count, .. } => assert_eq!(entry_count, 3),
        other => panic!("update mode must write the cassette, got {other:?}"),
    }
}

/// Replays the committed cassette through a real machine with no live backend.
///
/// The turn runs entirely off recorded data: [`CassetteLlmHandler`] and
/// [`CassetteToolHandler`] serve every requirement, so there is no network, no
/// credentials, and no real tool. The assertions cover the three things the task
/// calls out — the committed conversation, the handler call logs, and the final
/// cursor.
#[tokio::test]
async fn offline_replay_runs_a_full_weather_turn() {
    let json = std::fs::read_to_string(cassette_path()).expect(
        "committed cassette fixture is present; regenerate with AGENT_TESTKIT_UPDATE_CASSETTES=1",
    );
    let cassette = Cassette::from_json_str(&json).expect("fixture parses at the current schema");
    assert_eq!(cassette.entries.len(), 3, "user/tool/final round-trip");

    let player = CassettePlayer::new(
        cassette,
        "tests/cassettes/agent_weather_tool_roundtrip.json",
    );
    let llm = Arc::new(player.llm_handler());
    let tool = Arc::new(player.tool_handler());
    let llm_log = Arc::clone(llm.log());
    let tool_log = Arc::clone(tool.log());

    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
    let mut machine = default_machine(&ids, agent_state(&ids, spec));

    // Only the LLM and tool families are wired: the scope is headless, so any
    // stray interaction would surface as an `UnhandledRequirement` rather than
    // being silently auto-approved.
    let scope = TestScope::builder()
        .llm(llm.clone())
        .tool(tool.clone())
        .build();

    let done = drain(
        &mut machine,
        user_input(&ids, "weather?"),
        &scope,
        None,
        &ctx,
    )
    .await
    .expect("the offline turn drains to completion");

    // Final cursor: `assert_done` bakes in the clean `Done`-cursor check.
    assert_done(&done);

    // Handler call logs: two LLM generations and one tool call, all completed,
    // and no interaction/reconfig/subagent traffic (those handlers do not exist).
    assert_calls(&llm_log).count(2).all_completed();
    assert_calls(&tool_log).count(1).all_completed();

    // Committed conversation: one closed turn of user, assistant tool-use, tool
    // result, and the assistant's final answer, with the single tool call paired
    // and its result recorded as `Ok`.
    assert_conversation(machine.state().conversation())
        .pending_none()
        .committed_turns(1)
        .open_call_count(0)
        .pairing_count(0, 1)
        .message_role(0, 0, Role::User)
        .message_role(0, 1, Role::Assistant)
        .message_role(0, 2, Role::Tool)
        .message_role(0, 3, Role::Assistant)
        .tool_result_status(CALL_ID, ToolStatus::Ok)
        .last_assistant_text(FINAL_ANSWER);

    // The assertion module intentionally stays out of block-level detail, so the
    // last low-level facts — the exact message count and the tool-use *name* — are
    // still checked directly against the committed conversation.
    let messages = machine.state().conversation().turns()[0].messages();
    assert_eq!(
        messages.len(),
        4,
        "user, tool-use, tool result, final answer"
    );
    let ContentBlock::ToolUse { name, .. } = &messages[1].payload().content[0] else {
        panic!("the second message must be the assistant's tool-use request");
    };
    assert_eq!(name, "get_weather");
}
