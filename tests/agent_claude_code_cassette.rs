//! Claude Code `stream-json` *decoder* cassette suite (M6-2, feature
//! `external-claude-code`).
//!
//! A committed cassette
//! ([`ExternalRuntimeCassette`](agent_testkit::prelude::ExternalRuntimeCassette))
//! freezes what the adapter-private
//! [`ClaudeStreamDecoder`](agent_lib::agent::external::ClaudeStreamDecoder) must
//! produce: for each turn it stores the raw Claude Code CLI frames the decoder
//! consumes, the sequenced
//! [`ExternalObservedEvent`](agent_lib::agent::external::ExternalObservedEvent)
//! stream it emits, and the [`ClaudeDecision`](agent_lib::agent::external::ClaudeDecision)
//! it settles on. This suite proves the decoder end to end, entirely offline and
//! with no real Claude Code binary:
//!
//! - **decode** — one decoder spans the whole session; replaying every recorded
//!   frame reproduces the frozen observations and per-turn decision, covering
//!   text, a shell command, a file patch, a host tool-call pause, a permission
//!   pause, and completion;
//! - **tolerance / errors** — blank, `stream_event`, and unknown frames are
//!   tolerated while malformed frames classify as
//!   [`Protocol`](agent_lib::agent::external::ExternalAgentError::Protocol), and
//!   an error `result` decodes to a classified failure;
//! - **redaction** — the committed fixture is free of credential-shaped text.
//!
//! The fixture under `tests/fixtures/external/claude_code/` is the committed
//! source of truth; [`claude_code_cassette_regenerate_fixture`] rewrites it from
//! the in-code builder only when `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` is set,
//! so a normal run never overwrites a committed file.
//!
//! Run with `cargo test --features external-claude-code --test
//! agent_claude_code_cassette`, or filter with `cargo test
//! --features external-claude-code claude_code_cassette`.

#![cfg(feature = "external-claude-code")]

use std::path::PathBuf;

use agent_testkit::prelude::*;

use agent_lib::agent::external::{
    ClaudeDecision, ClaudeDecodeContext, ClaudeStreamDecoder, ExternalAgentError,
    ExternalAgentEvent, ExternalAgentOutput, ExternalObservedEvent, ExternalRuntimeKind,
    ExternalToolBatchId, ExternalToolCall,
};
use agent_lib::agent::interaction::Interaction;
use agent_lib::agent::permission::{PermissionCategory, PermissionRequest, PermissionRisk};
use agent_lib::agent::{AgentId, StepId};
use agent_lib::model::tool::ToolStatus;
use agent_lib::model::usage::Usage;
use serde_json::{Value, json};

/// Environment opt-in that lets [`claude_code_cassette_regenerate_fixture`]
/// rewrite the committed fixture. Unset on a normal/CI run.
const UPDATE_ENV_VAR: &str = "AGENT_LIB_UPDATE_EXTERNAL_CASSETTES";

/// Runtime-assigned session id the fixture fixes.
const SESSION_ID: &str = "claude-sess-42";

/// Fixed host step id the decoder binds permission interactions to.
const STEP_UUID: &str = "11111111-1111-4111-8111-111111111111";

/// Fixed requesting-agent id recorded on decoded permission prompts.
const ACTOR_UUID: &str = "22222222-2222-4222-8222-222222222222";

/// The host step id awaiting the session.
fn step_id() -> StepId {
    StepId::parse_str(STEP_UUID).expect("the fixed step uuid parses")
}

/// The agent recorded as the permission requester.
fn actor_id() -> AgentId {
    AgentId::parse_str(ACTOR_UUID).expect("the fixed actor uuid parses")
}

/// The decode context the fixture and the decoder share.
fn decode_context() -> ClaudeDecodeContext {
    ClaudeDecodeContext::new(step_id(), actor_id())
}

/// Absolute path to the committed decoder cassette.
fn full_session_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/external/claude_code/full_session.json")
}

/// Wraps a JSON value as a verbatim stdout frame line.
///
/// Serializes with object keys sorted recursively so the frozen fixture payload
/// is byte-identical whether or not the build unifies `serde_json/preserve_order`
/// (which flips `serde_json::Value` object maps from sorted `BTreeMap` to
/// insertion-order `IndexMap`, e.g. once `external-acp` is enabled).
fn frame(value: Value) -> CassetteFrame {
    CassetteFrame::stdout_json(&value)
}

/// The `git commit` command the permission turn gates on.
const COMMIT_COMMAND: &str = "git commit -am refactor";

/// The permission request the decoder mints on the fixture's `can_use_tool`
/// control request.
fn commit_permission_request() -> PermissionRequest {
    PermissionRequest::new(
        "perm-1".to_owned(),
        actor_id(),
        PermissionCategory::Shell,
        format!("run `{COMMIT_COMMAND}`"),
        json!({ "command": COMMIT_COMMAND }),
        PermissionRisk::Medium,
        None,
    )
}

/// The token-usage the completion turn decodes from the `result` frame.
fn completion_usage() -> Usage {
    Usage {
        input: 1200,
        output: 340,
        cache_read: 100,
        cache_write: 50,
        reasoning: 0,
        total: None,
        extra: serde_json::Map::new(),
    }
}

/// Turn 1: `Start` → the session starts, streams text, runs a shell command,
/// applies a patch, then pauses on a host-bridged (`mcp__…`) tool call.
fn turn_one() -> CassetteTurn {
    CassetteTurn::new(CassetteDecision::PausedForToolCalls {
        batch_id: ExternalToolBatchId::new("msg_4"),
        calls: vec![ExternalToolCall {
            provider_call_id: "toolu_host1".to_owned(),
            name: "mcp__host__run_tests".to_owned(),
            input: json!({ "suite": "parser" }),
            raw: None,
        }],
    })
    .expecting(CassetteInputKind::Start)
    .with_frames([
        frame(json!({
            "type": "system",
            "subtype": "init",
            "session_id": SESSION_ID,
            "cwd": "/repo/agent-lib",
            "tools": ["Bash", "Edit"],
            "model": "claude-sonnet",
        })),
        frame(json!({
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "role": "assistant",
                "content": [{ "type": "text", "text": "I'll refactor the parser." }],
            },
        })),
        frame(json!({
            "type": "assistant",
            "message": {
                "id": "msg_2",
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_bash1",
                    "name": "Bash",
                    "input": { "command": "cargo fmt" },
                }],
            },
        })),
        frame(json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_bash1",
                    "content": "formatted 3 files",
                    "is_error": false,
                }],
            },
        })),
        frame(json!({
            "type": "assistant",
            "message": {
                "id": "msg_3",
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_edit1",
                    "name": "Edit",
                    "input": {
                        "file_path": "src/parser.rs",
                        "old_string": "loop",
                        "new_string": "for",
                    },
                }],
            },
        })),
        frame(json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_edit1",
                    "content": "applied",
                    "is_error": false,
                }],
            },
        })),
        frame(json!({
            "type": "assistant",
            "message": {
                "id": "msg_4",
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_host1",
                    "name": "mcp__host__run_tests",
                    "input": { "suite": "parser" },
                }],
            },
        })),
    ])
    .emitting([
        ExternalObservedEvent::new(
            0,
            ExternalAgentEvent::SessionStarted {
                session_id: Some(SESSION_ID.to_owned()),
            },
        ),
        ExternalObservedEvent::new(
            1,
            ExternalAgentEvent::TextDelta {
                text: "I'll refactor the parser.".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            2,
            ExternalAgentEvent::CommandStarted {
                command: "cargo fmt".to_owned(),
                cwd: "/repo/agent-lib".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            3,
            ExternalAgentEvent::CommandFinished {
                exit_code: Some(0),
                stdout_tail: "formatted 3 files".to_owned(),
                stderr_tail: String::new(),
            },
        ),
        ExternalObservedEvent::new(
            4,
            ExternalAgentEvent::FilePatch {
                path: "src/parser.rs".to_owned(),
                summary: "Edit src/parser.rs".to_owned(),
                diff_ref: None,
            },
        ),
        ExternalObservedEvent::new(
            5,
            ExternalAgentEvent::ToolFinished {
                name: "Edit".to_owned(),
                status: ToolStatus::Ok,
            },
        ),
    ])
}

/// Turn 2: `RespondToolResults` → the session streams text, then pauses on a
/// permission prompt for a gated shell command.
fn turn_two() -> CassetteTurn {
    CassetteTurn::new(CassetteDecision::PausedForInteraction {
        action_id: "perm-1".to_owned(),
        request: Interaction::permission(step_id(), commit_permission_request()),
    })
    .expecting(CassetteInputKind::RespondToolResults)
    .with_frames([
        frame(json!({
            "type": "assistant",
            "message": {
                "id": "msg_5",
                "role": "assistant",
                "content": [{ "type": "text", "text": "Tests pass. I need to commit." }],
            },
        })),
        frame(json!({
            "type": "control_request",
            "request_id": "perm-1",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": { "command": COMMIT_COMMAND },
            },
        })),
    ])
    .emitting([
        ExternalObservedEvent::new(
            6,
            ExternalAgentEvent::TextDelta {
                text: "Tests pass. I need to commit.".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            7,
            ExternalAgentEvent::PermissionRequested {
                action_id: "perm-1".to_owned(),
                summary: format!("run `{COMMIT_COMMAND}`"),
            },
        ),
    ])
}

/// Turn 3: `RespondInteraction` → the session streams a final line and completes
/// with a summary, usage, and cost.
fn turn_three() -> CassetteTurn {
    CassetteTurn::new(CassetteDecision::Completed {
        output: ExternalAgentOutput {
            summary: "Refactored the parser and committed.".to_owned(),
            artifacts: Vec::new(),
            usage: Some(completion_usage()),
            cost_micros: Some(12_300),
        },
    })
    .expecting(CassetteInputKind::RespondInteraction)
    .with_frames([
        frame(json!({
            "type": "assistant",
            "message": {
                "id": "msg_6",
                "role": "assistant",
                "content": [{ "type": "text", "text": "Done." }],
            },
        })),
        frame(json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "Refactored the parser and committed.",
            "session_id": SESSION_ID,
            "total_cost_usd": 0.0123,
            "usage": {
                "input_tokens": 1200,
                "output_tokens": 340,
                "cache_read_input_tokens": 100,
                "cache_creation_input_tokens": 50,
            },
            "num_turns": 3,
            "duration_ms": 4200,
        })),
    ])
    .emitting([
        ExternalObservedEvent::new(
            8,
            ExternalAgentEvent::TextDelta {
                text: "Done.".to_owned(),
            },
        ),
        ExternalObservedEvent::new(9, ExternalAgentEvent::SessionCompleted),
    ])
}

/// The in-code source of truth for `full_session.json`.
fn full_session_cassette() -> ExternalRuntimeCassette {
    ExternalRuntimeCassette::new(
        CassetteRuntimeInfo::new(ExternalRuntimeKind::ClaudeCode)
            .with_version("1.0.0-cc-fixture")
            .with_probe("claude 1.0.0 (offline decoder fixture)")
            .with_session_id(SESSION_ID),
    )
    .with_redaction(
        RedactionMetadata::applied("<redacted>")
            .with_notes("synthetic Claude Code stream-json; no real prompt or credential content"),
    )
    .with_turn(turn_one())
    .with_turn(turn_two())
    .with_turn(turn_three())
}

/// Records `full_session.json` from [`full_session_cassette`] — but only when
/// `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1`, so a default run never overwrites the
/// committed fixture.
#[test]
fn claude_code_cassette_regenerate_fixture() {
    if std::env::var(UPDATE_ENV_VAR).as_deref() != Ok("1") {
        return;
    }
    let json = full_session_cassette()
        .to_json_string_pretty()
        .expect("the decoder cassette serializes");
    std::fs::write(full_session_path(), format!("{json}\n"))
        .expect("the decoder cassette is written to disk");
}

/// The committed fixture loads and matches the in-code builder exactly.
#[test]
fn claude_code_cassette_matches_in_code_builder() {
    let loaded = ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads");

    assert_eq!(
        loaded,
        full_session_cassette(),
        "the committed fixture drifted from the in-code builder; \
         rerun with {UPDATE_ENV_VAR}=1 to regenerate it",
    );
    assert_eq!(loaded.schema_version, EXTERNAL_CASSETTE_SCHEMA_VERSION);
    assert_eq!(loaded.runtime.kind, ExternalRuntimeKind::ClaudeCode);
    assert_eq!(loaded.runtime.session_id.as_deref(), Some(SESSION_ID));
    assert_eq!(loaded.turns.len(), 3);
}

/// Every committed frame, observation, and summary in the fixture is free of
/// credential-shaped text.
#[test]
fn claude_code_cassette_is_secret_free() {
    ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads")
        .assert_no_secrets();
}

/// Replaying the whole committed session through one decoder reproduces every
/// frozen observation stream and per-turn decision.
#[test]
fn claude_code_cassette_decodes_full_session() {
    let cassette = ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads");

    let mut decoder = ClaudeStreamDecoder::new(decode_context());
    for (index, turn) in cassette.turns.iter().enumerate() {
        let mut decision = None;
        for cassette_frame in &turn.input_frames {
            if let Some(reached) = decoder
                .push_line(&cassette_frame.payload)
                .unwrap_or_else(|error| panic!("turn {index} frame failed to decode: {error}"))
            {
                assert!(
                    decision.is_none(),
                    "turn {index} settled on more than one decision",
                );
                decision = Some(reached);
            }
        }

        let observations = decoder.take_observations();
        assert_eq!(
            observations, turn.expected_events,
            "turn {index} observations diverged from the frozen stream",
        );

        let decision = decision.unwrap_or_else(|| panic!("turn {index} settled on no decision"));
        assert_decision_matches(&decision, &turn.decision, index);
    }

    assert_eq!(decoder.session_id(), Some(SESSION_ID));
}

/// Asserts a decoder [`ClaudeDecision`] carries the same payload as the recorded
/// cassette [`CassetteDecision`].
fn assert_decision_matches(actual: &ClaudeDecision, expected: &CassetteDecision, turn: usize) {
    match (actual, expected) {
        (
            ClaudeDecision::Completed { output },
            CassetteDecision::Completed {
                output: expected_output,
            },
        ) => assert_eq!(output, expected_output, "turn {turn} completed output"),
        (
            ClaudeDecision::PausedForToolCalls { batch_id, calls },
            CassetteDecision::PausedForToolCalls {
                batch_id: expected_batch,
                calls: expected_calls,
            },
        ) => {
            assert_eq!(batch_id, expected_batch, "turn {turn} tool batch id");
            assert_eq!(calls, expected_calls, "turn {turn} tool calls");
        }
        (
            ClaudeDecision::PausedForInteraction { action_id, request },
            CassetteDecision::PausedForInteraction {
                action_id: expected_action,
                request: expected_request,
            },
        ) => {
            assert_eq!(
                action_id, expected_action,
                "turn {turn} permission action id"
            );
            assert_eq!(
                request, expected_request,
                "turn {turn} permission interaction"
            );
        }
        (
            ClaudeDecision::Failed { error },
            CassetteDecision::Failed {
                error: expected_error,
            },
        ) => assert_eq!(error, expected_error, "turn {turn} failure"),
        _ => {
            panic!("turn {turn}: decoder decision {actual:?} does not match cassette {expected:?}")
        }
    }
}

/// Blank, `stream_event`, and unknown frames are tolerated: they produce neither
/// observations nor a decision.
#[test]
fn claude_code_cassette_tolerates_unknown_and_blank_frames() {
    let mut decoder = ClaudeStreamDecoder::new(decode_context());

    for line in [
        "",
        "   ",
        &json!({ "type": "stream_event", "event": { "type": "content_block_delta" } }).to_string(),
        &json!({ "type": "totally_unknown_future_frame", "payload": 7 }).to_string(),
        &json!({ "type": "system", "subtype": "compact_boundary" }).to_string(),
    ] {
        assert_eq!(
            decoder
                .push_line(line)
                .expect("a tolerated frame never errors"),
            None,
        );
    }

    assert!(
        decoder.take_observations().is_empty(),
        "tolerated frames must not buffer observations",
    );
    assert_eq!(decoder.session_id(), None);
}

/// Corrupt frames classify as [`ExternalAgentError::Protocol`], never a panic.
#[test]
fn claude_code_cassette_rejects_malformed_frames() {
    let malformed = [
        "this is not json",
        &json!([1, 2, 3]).to_string(),
        &json!({ "no_type": true }).to_string(),
        &json!({ "type": 7 }).to_string(),
        &json!({ "type": "assistant" }).to_string(),
        &json!({ "type": "user" }).to_string(),
        &json!({ "type": "control_request", "request": { "subtype": "can_use_tool" } }).to_string(),
    ];

    for line in malformed {
        let mut decoder = ClaudeStreamDecoder::new(decode_context());
        match decoder.push_line(line) {
            Err(ExternalAgentError::Protocol { .. }) => {}
            other => panic!("expected a Protocol error for {line:?}, got {other:?}"),
        }
    }
}

/// An error `result` decodes to a classified failure rather than a completion.
#[test]
fn claude_code_cassette_decodes_error_result_as_failed() {
    let mut decoder = ClaudeStreamDecoder::new(decode_context());
    let max_turns = json!({
        "type": "result",
        "subtype": "error_max_turns",
        "is_error": true,
        "session_id": SESSION_ID,
    })
    .to_string();
    match decoder.push_line(&max_turns) {
        Ok(Some(ClaudeDecision::Failed {
            error: ExternalAgentError::LimitExceeded { .. },
        })) => {}
        other => panic!("expected a LimitExceeded failure, got {other:?}"),
    }
    assert!(
        decoder.take_observations().is_empty(),
        "a failed result must not emit a SessionCompleted observation",
    );

    let mut decoder = ClaudeStreamDecoder::new(decode_context());
    let execution_error = json!({
        "type": "result",
        "subtype": "error_during_execution",
        "is_error": true,
        "result": "the sandbox crashed",
        "session_id": SESSION_ID,
    })
    .to_string();
    match decoder.push_line(&execution_error) {
        Ok(Some(ClaudeDecision::Failed {
            error: ExternalAgentError::Runtime { code, message },
        })) => {
            assert_eq!(code.as_deref(), Some("error_during_execution"));
            assert_eq!(message, "the sandbox crashed");
        }
        other => panic!("expected a Runtime failure, got {other:?}"),
    }
}
