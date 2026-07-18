//! Codex `codex exec --json` *decoder* cassette suite (M7-2, feature
//! `external-codex`).
//!
//! A committed cassette
//! ([`ExternalRuntimeCassette`](agent_testkit::prelude::ExternalRuntimeCassette))
//! freezes what the adapter-private
//! [`CodexStreamDecoder`](agent_lib::agent::external::CodexStreamDecoder) must
//! produce: for each turn it stores the raw `codex exec --json` frames the decoder
//! consumes, the sequenced
//! [`ExternalObservedEvent`](agent_lib::agent::external::ExternalObservedEvent)
//! stream it emits, and the [`CodexDecision`](agent_lib::agent::external::CodexDecision)
//! it settles on. This suite proves the decoder end to end, entirely offline and
//! with no real Codex binary:
//!
//! - **decode** — one decoder spans the whole session; replaying every recorded
//!   frame reproduces the frozen observations and per-turn decision, covering
//!   text, a shell command, a file patch, an MCP tool call, a policy-declined
//!   (permission) command, completion, and a failed turn;
//! - **tolerance / errors** — blank, `turn.started`, top-level `error`,
//!   `item.updated`, and unknown frames are tolerated while malformed frames
//!   classify as [`Protocol`](agent_lib::agent::external::ExternalAgentError::Protocol),
//!   and a `turn.failed` decodes to a classified failure;
//! - **redaction** — the committed fixture is free of credential-shaped text.
//!
//! The fixture under `tests/fixtures/external/codex/` is the committed source of
//! truth; [`codex_cassette_regenerate_fixture`] rewrites it from the in-code
//! builder only when `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` is set, so a normal
//! run never overwrites a committed file.
//!
//! Run with `cargo test --features external-codex --test agent_codex_cassette`,
//! or filter with `cargo test --features external-codex codex_cassette`.

#![cfg(feature = "external-codex")]

use std::path::PathBuf;

use agent_testkit::prelude::*;

use agent_lib::agent::external::{
    CodexDecision, CodexDecodeContext, CodexStreamDecoder, ExternalAgentError, ExternalAgentEvent,
    ExternalAgentOutput, ExternalObservedEvent, ExternalRuntimeKind,
};
use agent_lib::model::tool::ToolStatus;
use agent_lib::model::usage::Usage;
use serde_json::{Value, json};

/// Environment opt-in that lets [`codex_cassette_regenerate_fixture`] rewrite the
/// committed fixture. Unset on a normal/CI run.
const UPDATE_ENV_VAR: &str = "AGENT_LIB_UPDATE_EXTERNAL_CASSETTES";

/// Runtime-assigned thread id the fixture fixes.
const SESSION_ID: &str = "019f6e33-c533-7680-9bb7-7d3b5a45e780";

/// Working directory the decoder stamps onto command observations.
const CWD: &str = "/repo/agent-lib";

/// The `git commit` command the approval policy declines.
const COMMIT_COMMAND: &str = "git commit -am refactor";

/// The decode context the fixture and the decoder share.
fn decode_context() -> CodexDecodeContext {
    CodexDecodeContext::new().with_cwd(CWD)
}

/// Absolute path to the committed decoder cassette.
fn full_session_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/external/codex/full_session.json")
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

/// The token usage the completion turn decodes from the `turn.completed` frame.
fn completion_usage() -> Usage {
    Usage {
        input: 1200,
        output: 340,
        cache_read: 100,
        cache_write: 50,
        reasoning: 20,
        total: None,
        extra: serde_json::Map::new(),
    }
}

/// Turn 1: `Start` → the thread starts, streams text, runs a shell command,
/// applies a patch, calls an MCP tool, has a command declined by policy, then
/// completes with usage.
fn turn_one() -> CassetteTurn {
    CassetteTurn::new(CassetteDecision::Completed {
        output: ExternalAgentOutput {
            summary: "I'll refactor the parser.".to_owned(),
            artifacts: Vec::new(),
            usage: Some(completion_usage()),
            cost_micros: None,
        },
    })
    .expecting(CassetteInputKind::Start)
    .with_frames([
        frame(json!({ "type": "thread.started", "thread_id": SESSION_ID })),
        frame(json!({ "type": "turn.started" })),
        frame(json!({
            "type": "item.completed",
            "item": {
                "id": "item_0",
                "type": "agent_message",
                "text": "I'll refactor the parser.",
            },
        })),
        frame(json!({
            "type": "item.started",
            "item": {
                "id": "item_1",
                "type": "command_execution",
                "command": "cargo fmt",
                "aggregated_output": "",
                "exit_code": Value::Null,
                "status": "in_progress",
            },
        })),
        frame(json!({
            "type": "item.completed",
            "item": {
                "id": "item_1",
                "type": "command_execution",
                "command": "cargo fmt",
                "aggregated_output": "formatted 3 files",
                "exit_code": 0,
                "status": "completed",
            },
        })),
        frame(json!({
            "type": "item.completed",
            "item": {
                "id": "item_2",
                "type": "file_change",
                "status": "completed",
                "changes": [{ "path": "src/parser.rs", "kind": "update" }],
            },
        })),
        frame(json!({
            "type": "item.started",
            "item": {
                "id": "item_3",
                "type": "mcp_tool_call",
                "server": "docs",
                "tool": "search",
                "arguments": { "query": "parser" },
                "result": Value::Null,
                "error": Value::Null,
                "status": "in_progress",
            },
        })),
        frame(json!({
            "type": "item.completed",
            "item": {
                "id": "item_3",
                "type": "mcp_tool_call",
                "server": "docs",
                "tool": "search",
                "arguments": { "query": "parser" },
                "result": { "content": [] },
                "error": Value::Null,
                "status": "completed",
            },
        })),
        frame(json!({
            "type": "item.completed",
            "item": {
                "id": "item_4",
                "type": "command_execution",
                "command": COMMIT_COMMAND,
                "aggregated_output": "",
                "exit_code": Value::Null,
                "status": "declined",
            },
        })),
        frame(json!({
            "type": "turn.completed",
            "usage": {
                "input_tokens": 1200,
                "cached_input_tokens": 100,
                "cache_write_input_tokens": 50,
                "output_tokens": 340,
                "reasoning_output_tokens": 20,
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
                cwd: CWD.to_owned(),
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
                summary: "update src/parser.rs".to_owned(),
                diff_ref: None,
            },
        ),
        ExternalObservedEvent::new(
            5,
            ExternalAgentEvent::ToolStarted {
                name: "docs/search".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            6,
            ExternalAgentEvent::ToolFinished {
                name: "docs/search".to_owned(),
                status: ToolStatus::Ok,
            },
        ),
        ExternalObservedEvent::new(
            7,
            ExternalAgentEvent::PermissionRequested {
                action_id: "item_4".to_owned(),
                summary: format!("run `{COMMIT_COMMAND}` (declined by approval policy)"),
            },
        ),
        ExternalObservedEvent::new(8, ExternalAgentEvent::SessionCompleted),
    ])
}

/// Turn 2: `Continue` (resume) → the thread streams one line, weathers a
/// transient top-level `error`, then fails the turn.
fn turn_two() -> CassetteTurn {
    CassetteTurn::new(CassetteDecision::Failed {
        error: ExternalAgentError::Runtime {
            code: None,
            message: "codex turn failed".to_owned(),
            runtime_output: Some("turn aborted: sandbox denied network access".to_owned()),
        },
    })
    .expecting(CassetteInputKind::Continue)
    .with_frames([
        frame(json!({ "type": "turn.started" })),
        frame(json!({
            "type": "item.completed",
            "item": {
                "id": "item_5",
                "type": "agent_message",
                "text": "I cannot reach the sandbox.",
            },
        })),
        frame(json!({ "type": "error", "message": "stream disconnected, retrying" })),
        frame(json!({
            "type": "turn.failed",
            "error": { "message": "turn aborted: sandbox denied network access" },
        })),
    ])
    .emitting([ExternalObservedEvent::new(
        9,
        ExternalAgentEvent::TextDelta {
            text: "I cannot reach the sandbox.".to_owned(),
        },
    )])
}

/// The in-code source of truth for `full_session.json`.
fn full_session_cassette() -> ExternalRuntimeCassette {
    ExternalRuntimeCassette::new(
        CassetteRuntimeInfo::new(ExternalRuntimeKind::Codex)
            .with_version("0.144.1-codex-fixture")
            .with_probe("codex-cli 0.144.1 (offline decoder fixture)")
            .with_session_id(SESSION_ID),
    )
    .with_redaction(
        RedactionMetadata::applied("<redacted>")
            .with_notes("synthetic codex exec --json; no real prompt or credential content"),
    )
    .with_turn(turn_one())
    .with_turn(turn_two())
}

/// Records `full_session.json` from [`full_session_cassette`] — but only when
/// `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1`, so a default run never overwrites the
/// committed fixture.
#[test]
fn codex_cassette_regenerate_fixture() {
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
fn codex_cassette_matches_in_code_builder() {
    let loaded = ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads");

    assert_eq!(
        loaded,
        full_session_cassette(),
        "the committed fixture drifted from the in-code builder; \
         rerun with {UPDATE_ENV_VAR}=1 to regenerate it",
    );
    assert_eq!(loaded.schema_version, EXTERNAL_CASSETTE_SCHEMA_VERSION);
    assert_eq!(loaded.runtime.kind, ExternalRuntimeKind::Codex);
    assert_eq!(loaded.runtime.session_id.as_deref(), Some(SESSION_ID));
    assert_eq!(loaded.turns.len(), 2);
}

/// Every committed frame, observation, and summary in the fixture is free of
/// credential-shaped text.
#[test]
fn codex_cassette_is_secret_free() {
    ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads")
        .assert_no_secrets();
}

/// Replaying the whole committed session through one decoder reproduces every
/// frozen observation stream and per-turn decision.
#[test]
fn codex_cassette_decodes_full_session() {
    let cassette = ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads");

    let mut decoder = CodexStreamDecoder::new(decode_context());
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

/// Asserts a decoder [`CodexDecision`] carries the same payload as the recorded
/// cassette [`CassetteDecision`].
fn assert_decision_matches(actual: &CodexDecision, expected: &CassetteDecision, turn: usize) {
    match (actual, expected) {
        (
            CodexDecision::Completed { output },
            CassetteDecision::Completed {
                output: expected_output,
            },
        ) => assert_eq!(output, expected_output, "turn {turn} completed output"),
        (
            CodexDecision::Failed { error },
            CassetteDecision::Failed {
                error: expected_error,
            },
        ) => assert_eq!(error, expected_error, "turn {turn} failure"),
        _ => {
            panic!("turn {turn}: decoder decision {actual:?} does not match cassette {expected:?}")
        }
    }
}

/// Blank, `turn.started`, top-level `error`, `item.updated`, and unknown frames
/// are tolerated: they produce neither observations nor a decision.
#[test]
fn codex_cassette_tolerates_unknown_and_blank_frames() {
    let mut decoder = CodexStreamDecoder::new(decode_context());

    for line in [
        "",
        "   ",
        &json!({ "type": "turn.started" }).to_string(),
        &json!({ "type": "error", "message": "Reconnecting... 1/5" }).to_string(),
        &json!({
            "type": "item.updated",
            "item": { "id": "item_0", "type": "command_execution", "status": "in_progress" },
        })
        .to_string(),
        &json!({
            "type": "item.completed",
            "item": { "id": "item_1", "type": "reasoning", "text": "thinking" },
        })
        .to_string(),
        &json!({ "type": "totally_unknown_future_frame", "payload": 7 }).to_string(),
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
fn codex_cassette_rejects_malformed_frames() {
    let malformed = [
        "this is not json",
        &json!([1, 2, 3]).to_string(),
        &json!({ "no_type": true }).to_string(),
        &json!({ "type": 7 }).to_string(),
        &json!({ "type": "thread.started" }).to_string(),
        &json!({ "type": "item.completed" }).to_string(),
        &json!({ "type": "item.started", "item": 7 }).to_string(),
    ];

    for line in malformed {
        let mut decoder = CodexStreamDecoder::new(decode_context());
        match decoder.push_line(line) {
            Err(ExternalAgentError::Protocol { .. }) => {}
            other => panic!("expected a Protocol error for {line:?}, got {other:?}"),
        }
    }
}

/// A `turn.failed` decodes to a classified failure rather than a completion, and
/// emits no `SessionCompleted` observation.
#[test]
fn codex_cassette_decodes_turn_failed_as_failed() {
    let mut decoder = CodexStreamDecoder::new(decode_context());
    let failed = json!({
        "type": "turn.failed",
        "error": { "message": "usage limit reached" },
    })
    .to_string();
    match decoder.push_line(&failed) {
        Ok(Some(CodexDecision::Failed {
            error:
                ExternalAgentError::Runtime {
                    code,
                    message,
                    runtime_output,
                },
        })) => {
            assert_eq!(code, None);
            assert_eq!(message, "codex turn failed");
            assert_eq!(runtime_output.as_deref(), Some("usage limit reached"));
        }
        other => panic!("expected a Runtime failure, got {other:?}"),
    }
    assert!(
        decoder.take_observations().is_empty(),
        "a failed turn must not emit a SessionCompleted observation",
    );
}

/// The reported message of a `turn.failed` frame is model-influenced output; it
/// is preserved in `runtime_output` but never folded into the `Display`
/// rendering (M-EXT-3).
#[test]
fn codex_cassette_turn_failed_keeps_runtime_text_out_of_display() {
    let secret = "API_KEY=sk-secret-123";
    let mut decoder = CodexStreamDecoder::new(decode_context());
    let failed = json!({
        "type": "turn.failed",
        "error": { "message": format!("request aborted after reading .env: {secret}") },
    })
    .to_string();
    match decoder.push_line(&failed) {
        Ok(Some(CodexDecision::Failed { error })) => {
            let rendered = error.to_string();
            assert!(
                !rendered.contains(secret),
                "Display must not leak runtime output: {rendered}"
            );
            let ExternalAgentError::Runtime {
                message,
                runtime_output,
                ..
            } = error
            else {
                unreachable!("matched Failed above");
            };
            assert_eq!(message, "codex turn failed");
            assert!(
                runtime_output
                    .as_deref()
                    .is_some_and(|text| text.contains(secret)),
                "raw runtime text is preserved separately"
            );
        }
        other => panic!("expected a Runtime failure, got {other:?}"),
    }
}
