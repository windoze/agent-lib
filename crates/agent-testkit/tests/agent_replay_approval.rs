//! Recorded replay suite (`agent_replay_approval`, milestone 6, M6-4).
//!
//! A guarded tool round-trip that runs entirely off a committed cassette
//! ([`agent_tool_approval_roundtrip.json`](../tests/cassettes)) with **no
//! network, credentials, live tool backend, or human**: a require-approval
//! policy forces the tool call through a `NeedInteraction`, the recorded
//! interaction *approves* it, and the tool then runs. The whole
//! `user -> LLM tool_use -> approval -> tool -> LLM final text` flow replays
//! offline.
//!
//! The require-approval *policy* is a spec-level decision, not a mockable effect
//! boundary, so `agent-testkit` deliberately does not ship one; this suite
//! carries a minimal local policy (mirroring `agent_interaction_basic` and
//! `agent_effect_e2e`). Everything else — the LLM, tool, and interaction
//! backends — is served from the cassette.
//!
//! The committed fixture is *synthetic* recorded data written in the exact shape
//! [`CassetteRecorder`] produces: [`regenerate_approval_cassette`] records the
//! same scenario through the recorder and, only when
//! [`AGENT_TESTKIT_UPDATE_CASSETTES=1`](agent_testkit::cassette::UPDATE_ENV_VAR)
//! is set, rewrites the committed file. Recording the requests the machine
//! actually issues keeps every entry fingerprint in lock-step with what the
//! machine reproduces on replay. A normal CI run leaves that test a no-op
//! (`RecorderReport::Skipped`), so the committed fixture is never overwritten.
//!
//! Run in isolation with `cargo test --test agent_replay_approval`.

use std::path::PathBuf;
use std::sync::Arc;

use agent_lib::agent::{ApprovalRequirement, ToolApprovalPolicy, drain};
use agent_lib::conversation::ToolCallId;
use agent_lib::model::message::Role;
use agent_lib::model::tool::{ToolCall, ToolStatus};
use agent_testkit::prelude::*;

/// Provider tool-call id shared by the tool-use request and its result.
const CALL_ID: &str = "call-weather";
/// The tool's recorded, model-visible reply once the approval is granted.
const WEATHER_REPLY: &str = "Sunny, 20C in Shanghai.";
/// The assistant's final answer, synthesized from the tool reply.
const FINAL_ANSWER: &str = "It is sunny in Shanghai.";

/// Approval policy that requires human approval for every tool call, forcing the
/// guarded tool call through a `NeedInteraction`.
#[derive(Debug)]
struct RequireApprovalPolicy;

impl ToolApprovalPolicy for RequireApprovalPolicy {
    fn approval_requirement(&self, _call_id: ToolCallId, _call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::required(Some("human approval required".to_owned()))
    }
}

/// Absolute path to the committed cassette fixture.
fn cassette_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/cassettes/agent_tool_approval_roundtrip.json")
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

/// The scripted LLM handler: first ask for `get_weather`, then answer once the
/// approved tool result returns. Usages are non-zero so the recorded result
/// payloads carry realistic token counts.
fn scripted_llm() -> ScriptedLlmHandler {
    ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call(
            CALL_ID,
            "get_weather",
            serde_json::json!({ "city": "Shanghai" }),
        )])
        .with_usage(usage(5, 2)),
        LlmStep::text(FINAL_ANSWER).with_usage(usage(6, 4)),
    ])
}

/// The scripted tool handler that serves the single `get_weather` call once the
/// approval is granted.
fn scripted_tool() -> ScriptedToolHandler {
    ScriptedToolHandler::from_steps([ToolStep::ok(CALL_ID, WEATHER_REPLY)])
}

/// Records the approved guarded round-trip through [`CassetteRecorder`] and
/// rewrites the committed fixture — but only when
/// `AGENT_TESTKIT_UPDATE_CASSETTES=1`.
///
/// This is the fixture's source of truth: it wraps the scripted LLM, tool, and
/// interaction backends so the recorder captures the *exact* requests a real
/// [`DefaultAgentMachine`] issues, guaranteeing every entry fingerprint matches
/// what the replay test reproduces. A default CI run leaves it a no-op and never
/// touches the committed file.
#[tokio::test]
async fn regenerate_approval_cassette() {
    let recorder = CassetteRecorder::update(cassette_path()).with_metadata(
        CassetteMetadata::new("agent_tool_approval_roundtrip").with_description(
            "user -> LLM tool_use -> approval (approve) -> get_weather -> LLM final text; no network or credentials",
        ),
    );

    // A default CI run never opts in, so the fixture is left untouched.
    if let Some(reason) = recorder.skip_reason() {
        assert!(reason.contains("AGENT_TESTKIT_UPDATE_CASSETTES"));
        return;
    }

    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let mut machine = guarded_machine(&ids);

    let scope = TestScope::builder()
        .llm(Arc::new(recorder.wrap_llm(scripted_llm())))
        .tool(Arc::new(recorder.wrap_tool(scripted_tool())))
        .attended(Arc::new(
            recorder.wrap_interaction(ScriptedInteractionHandler::approve_all()),
        ))
        .build();

    drain(
        &mut machine,
        user_input(&ids, "weather?"),
        &scope,
        None,
        &ctx,
    )
    .await
    .expect("the recorded approved turn drains to completion");

    let report = recorder.finish().expect("cassette write succeeds");
    match report {
        RecorderReport::Wrote { entry_count, .. } => assert_eq!(entry_count, 4),
        other => panic!("update mode must write the cassette, got {other:?}"),
    }
}

/// Replays the committed cassette through a real guarded machine with no live
/// backend.
///
/// Every requirement — the two LLM generations, the approval interaction, and
/// the tool call — is served from the cassette, so there is no network,
/// credentials, real tool, or human. The assertions cover the three things the
/// task calls out — the committed conversation, the handler call logs, and the
/// final cursor — and confirm the approval was replayed (so the guarded tool
/// ran and committed an `Ok` result).
#[tokio::test]
async fn offline_replay_runs_an_approved_turn() {
    let json = std::fs::read_to_string(cassette_path()).expect(
        "committed cassette fixture is present; regenerate with AGENT_TESTKIT_UPDATE_CASSETTES=1",
    );
    let cassette = Cassette::from_json_str(&json).expect("fixture parses at the current schema");
    assert_eq!(
        cassette.entries.len(),
        4,
        "user/approval/tool/final round-trip"
    );

    let player = CassettePlayer::new(
        cassette,
        "tests/cassettes/agent_tool_approval_roundtrip.json",
    );
    let llm = Arc::new(player.llm_handler());
    let tool = Arc::new(player.tool_handler());
    let interaction = Arc::new(player.interaction_handler());
    let llm_log = Arc::clone(llm.log());
    let tool_log = Arc::clone(tool.log());
    let interaction_log = Arc::clone(interaction.log());

    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let mut machine = guarded_machine(&ids);

    let scope = TestScope::builder()
        .llm(llm.clone())
        .tool(tool.clone())
        .attended(interaction.clone())
        .build();

    let done = drain(
        &mut machine,
        user_input(&ids, "weather?"),
        &scope,
        None,
        &ctx,
    )
    .await
    .expect("the offline approved turn drains to completion");

    // Final cursor: `assert_done` bakes in the clean `Done`-cursor check.
    assert_done(&done);

    // Handler call logs: two LLM generations and one tool call, all completed,
    // plus exactly one replayed approval interaction.
    assert_calls(&llm_log).count(2).all_completed();
    assert_calls(&tool_log).count(1).all_completed();
    assert_eq!(interaction_log.len(), 1, "the approval was answered once");

    // Committed conversation: one closed turn of user, assistant tool-use, tool
    // result, and the assistant's final answer, with the single tool call paired
    // and its result recorded as `Ok` (the approval was granted).
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
    // exact message count is checked directly against the committed conversation.
    let messages = machine.state().conversation().turns()[0].messages();
    assert_eq!(
        messages.len(),
        4,
        "user, tool-use, tool result, final answer"
    );
}
