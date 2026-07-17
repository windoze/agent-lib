//! OpenCode `opencode run --format json` *decoder* cassette suite (M8-2, feature
//! `external-opencode`).
//!
//! A committed cassette
//! ([`ExternalRuntimeCassette`](agent_testkit::prelude::ExternalRuntimeCassette))
//! freezes what the adapter-private
//! [`OpenCodeStreamDecoder`](agent_lib::agent::external::OpenCodeStreamDecoder) must
//! produce: for each turn it stores the raw `opencode run --format json` frames the
//! decoder consumes, the sequenced
//! [`ExternalObservedEvent`](agent_lib::agent::external::ExternalObservedEvent)
//! stream it emits, and the [`OpenCodeDecision`](agent_lib::agent::external::OpenCodeDecision)
//! it settles on. This suite proves the decoder end to end, entirely offline and
//! with no real OpenCode binary:
//!
//! - **decode** — one decoder spans the whole session; replaying every recorded
//!   frame reproduces the frozen observations and per-turn decision, covering
//!   text, a shell command, a file patch, a plain tool call, a `task` subagent, a
//!   permission-rejected tool, a `tool-calls` step that continues the agentic
//!   loop, a terminal `step_finish` completion, and a failed turn;
//! - **tolerance / errors** — blank, `step_start`, `reasoning`, a `tool-calls`
//!   `step_finish`, and unknown frames are tolerated while malformed frames
//!   classify as [`Protocol`](agent_lib::agent::external::ExternalAgentError::Protocol),
//!   and an `error` frame decodes to a classified failure;
//! - **redaction** — the committed fixture is free of credential-shaped text.
//!
//! The fixture under `tests/fixtures/external/opencode/` is the committed source of
//! truth; [`opencode_cassette_regenerate_fixture`] rewrites it from the in-code
//! builder only when `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` is set, so a normal
//! run never overwrites a committed file.
//!
//! Run with `cargo test --features external-opencode --test agent_opencode_cassette`,
//! or filter with `cargo test --features external-opencode opencode_cassette`.

#![cfg(feature = "external-opencode")]

use std::path::PathBuf;

use agent_testkit::prelude::*;

use agent_lib::agent::external::{
    ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput, ExternalObservedEvent,
    ExternalRuntimeKind, OpenCodeDecision, OpenCodeDecodeContext, OpenCodeStreamDecoder,
};
use agent_lib::model::tool::ToolStatus;
use agent_lib::model::usage::Usage;
use serde_json::{Value, json};

/// Environment opt-in that lets [`opencode_cassette_regenerate_fixture`] rewrite
/// the committed fixture. Unset on a normal/CI run.
const UPDATE_ENV_VAR: &str = "AGENT_LIB_UPDATE_EXTERNAL_CASSETTES";

/// Runtime-assigned session id the fixture fixes.
const SESSION_ID: &str = "ses_8b1f7a2c9d3e4f50";

/// Working directory the decoder stamps onto command observations.
const CWD: &str = "/repo/agent-lib";

/// The `git commit` command the permission policy rejects.
const COMMIT_COMMAND: &str = "git commit -am wip";

/// OpenCode's stable `PermissionRejectedError` message text.
const REJECTION_MESSAGE: &str = "The user rejected permission to use this specific tool call.";

/// The decode context the fixture and the decoder share.
fn decode_context() -> OpenCodeDecodeContext {
    OpenCodeDecodeContext::new().with_cwd(CWD)
}

/// Absolute path to the committed decoder cassette.
fn full_session_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/external/opencode/full_session.json")
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

/// The token usage the completion turn decodes by summing its two `step_finish`
/// frames (`800/120/…` on the `tool-calls` step plus `400/90/…` on the terminal
/// step).
fn completion_usage() -> Usage {
    Usage {
        input: 1200,
        output: 210,
        cache_read: 80,
        cache_write: 25,
        reasoning: 10,
        total: None,
        extra: serde_json::Map::new(),
    }
}

/// Turn 1: `Start` → the session starts, streams text, runs a shell command,
/// applies an edit, calls a plain tool, spawns a `task` subagent, has a command
/// rejected by the permission policy, then completes with summed usage.
fn turn_one() -> CassetteTurn {
    CassetteTurn::new(CassetteDecision::Completed {
        output: ExternalAgentOutput {
            summary: "Refactored the parser and ran a review.".to_owned(),
            artifacts: Vec::new(),
            usage: Some(completion_usage()),
            cost_micros: Some(3000),
        },
    })
    .expecting(CassetteInputKind::Start)
    .with_frames([
        frame(json!({
            "type": "step_start",
            "timestamp": 1_700_000_000_000_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_0",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "step-start",
                "snapshot": "snap_0",
            },
        })),
        frame(json!({
            "type": "text",
            "timestamp": 1_700_000_000_001_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_1",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "text",
                "text": "I'll update the parser.",
                "time": { "start": 1_700_000_000_000_i64, "end": 1_700_000_000_001_i64 },
            },
        })),
        frame(json!({
            "type": "tool_use",
            "timestamp": 1_700_000_000_002_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_2",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "tool",
                "callID": "call_1",
                "tool": "bash",
                "state": {
                    "status": "completed",
                    "input": { "command": "cargo fmt" },
                    "output": "formatted 3 files",
                    "title": "cargo fmt",
                    "metadata": { "exit": 0, "output": "formatted 3 files" },
                    "time": { "start": 1_700_000_000_001_i64, "end": 1_700_000_000_002_i64 },
                },
            },
        })),
        frame(json!({
            "type": "tool_use",
            "timestamp": 1_700_000_000_003_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_3",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "tool",
                "callID": "call_2",
                "tool": "edit",
                "state": {
                    "status": "completed",
                    "input": { "filePath": "src/parser.rs" },
                    "output": "",
                    "title": "src/parser.rs",
                    "metadata": { "diff": "@@ parser @@" },
                    "time": { "start": 1_700_000_000_002_i64, "end": 1_700_000_000_003_i64 },
                },
            },
        })),
        frame(json!({
            "type": "tool_use",
            "timestamp": 1_700_000_000_004_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_4",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "tool",
                "callID": "call_3",
                "tool": "grep",
                "state": {
                    "status": "completed",
                    "input": { "pattern": "parser" },
                    "output": "3 matches",
                    "title": "grep",
                    "metadata": {},
                    "time": { "start": 1_700_000_000_003_i64, "end": 1_700_000_000_004_i64 },
                },
            },
        })),
        frame(json!({
            "type": "step_finish",
            "timestamp": 1_700_000_000_005_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_5",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "step-finish",
                "reason": "tool-calls",
                "snapshot": "snap_1",
                "cost": 0.001,
                "tokens": {
                    "input": 800,
                    "output": 120,
                    "reasoning": 0,
                    "cache": { "read": 50, "write": 20 },
                },
            },
        })),
        frame(json!({
            "type": "step_start",
            "timestamp": 1_700_000_000_006_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_6",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "step-start",
                "snapshot": "snap_1",
            },
        })),
        frame(json!({
            "type": "tool_use",
            "timestamp": 1_700_000_000_007_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_7",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "tool",
                "callID": "call_4",
                "tool": "task",
                "state": {
                    "status": "completed",
                    "input": {
                        "description": "review parser",
                        "prompt": "Review the parser changes.",
                        "subagent_type": "reviewer",
                    },
                    "output": "looks good",
                    "title": "review parser",
                    "metadata": { "subagent_type": "reviewer" },
                    "time": { "start": 1_700_000_000_006_i64, "end": 1_700_000_000_007_i64 },
                },
            },
        })),
        frame(json!({
            "type": "tool_use",
            "timestamp": 1_700_000_000_008_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_8",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "tool",
                "callID": "call_5",
                "tool": "bash",
                "state": {
                    "status": "error",
                    "input": { "command": COMMIT_COMMAND },
                    "error": REJECTION_MESSAGE,
                    "metadata": {},
                    "time": { "start": 1_700_000_000_007_i64, "end": 1_700_000_000_008_i64 },
                },
            },
        })),
        frame(json!({
            "type": "text",
            "timestamp": 1_700_000_000_009_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_9",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "text",
                "text": "Refactored the parser and ran a review.",
                "time": { "start": 1_700_000_000_008_i64, "end": 1_700_000_000_009_i64 },
            },
        })),
        frame(json!({
            "type": "step_finish",
            "timestamp": 1_700_000_000_010_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_10",
                "sessionID": SESSION_ID,
                "messageID": "msg_1",
                "type": "step-finish",
                "reason": "stop",
                "snapshot": "snap_2",
                "cost": 0.002,
                "tokens": {
                    "input": 400,
                    "output": 90,
                    "reasoning": 10,
                    "cache": { "read": 30, "write": 5 },
                },
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
                text: "I'll update the parser.".to_owned(),
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
                summary: "edit src/parser.rs".to_owned(),
                diff_ref: None,
            },
        ),
        ExternalObservedEvent::new(
            5,
            ExternalAgentEvent::ToolStarted {
                name: "grep".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            6,
            ExternalAgentEvent::ToolFinished {
                name: "grep".to_owned(),
                status: ToolStatus::Ok,
            },
        ),
        ExternalObservedEvent::new(
            7,
            ExternalAgentEvent::ToolStarted {
                name: "task".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            8,
            ExternalAgentEvent::ToolFinished {
                name: "task".to_owned(),
                status: ToolStatus::Ok,
            },
        ),
        ExternalObservedEvent::new(
            9,
            ExternalAgentEvent::PermissionRequested {
                action_id: "call_5".to_owned(),
                summary: "`bash` (rejected by permission policy)".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            10,
            ExternalAgentEvent::TextDelta {
                text: "Refactored the parser and ran a review.".to_owned(),
            },
        ),
        ExternalObservedEvent::new(11, ExternalAgentEvent::SessionCompleted),
    ])
}

/// Turn 2: `Continue` (resume) → the session streams one line, then fails with a
/// top-level `error` frame.
fn turn_two() -> CassetteTurn {
    CassetteTurn::new(CassetteDecision::Failed {
        error: ExternalAgentError::Runtime {
            code: None,
            message: "model request failed: network unreachable".to_owned(),
        },
    })
    .expecting(CassetteInputKind::Continue)
    .with_frames([
        frame(json!({
            "type": "step_start",
            "timestamp": 1_700_000_000_020_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_20",
                "sessionID": SESSION_ID,
                "messageID": "msg_2",
                "type": "step-start",
                "snapshot": "snap_3",
            },
        })),
        frame(json!({
            "type": "text",
            "timestamp": 1_700_000_000_021_i64,
            "sessionID": SESSION_ID,
            "part": {
                "id": "prt_21",
                "sessionID": SESSION_ID,
                "messageID": "msg_2",
                "type": "text",
                "text": "I cannot reach the network.",
                "time": { "start": 1_700_000_000_020_i64, "end": 1_700_000_000_021_i64 },
            },
        })),
        frame(json!({
            "type": "error",
            "timestamp": 1_700_000_000_022_i64,
            "sessionID": SESSION_ID,
            "error": {
                "name": "ProviderError",
                "data": { "message": "model request failed: network unreachable" },
            },
        })),
    ])
    .emitting([ExternalObservedEvent::new(
        12,
        ExternalAgentEvent::TextDelta {
            text: "I cannot reach the network.".to_owned(),
        },
    )])
}

/// The in-code source of truth for `full_session.json`.
fn full_session_cassette() -> ExternalRuntimeCassette {
    ExternalRuntimeCassette::new(
        CassetteRuntimeInfo::new(ExternalRuntimeKind::OpenCode)
            .with_version("0.4.0-opencode-fixture")
            .with_probe("opencode 0.4.0 (offline decoder fixture)")
            .with_session_id(SESSION_ID),
    )
    .with_redaction(
        RedactionMetadata::applied("<redacted>").with_notes(
            "synthetic opencode run --format json; no real prompt or credential content",
        ),
    )
    .with_turn(turn_one())
    .with_turn(turn_two())
}

/// Records `full_session.json` from [`full_session_cassette`] — but only when
/// `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1`, so a default run never overwrites the
/// committed fixture.
#[test]
fn opencode_cassette_regenerate_fixture() {
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
fn opencode_cassette_matches_in_code_builder() {
    let loaded = ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads");

    assert_eq!(
        loaded,
        full_session_cassette(),
        "the committed fixture drifted from the in-code builder; \
         rerun with {UPDATE_ENV_VAR}=1 to regenerate it",
    );
    assert_eq!(loaded.schema_version, EXTERNAL_CASSETTE_SCHEMA_VERSION);
    assert_eq!(loaded.runtime.kind, ExternalRuntimeKind::OpenCode);
    assert_eq!(loaded.runtime.session_id.as_deref(), Some(SESSION_ID));
    assert_eq!(loaded.turns.len(), 2);
}

/// Every committed frame, observation, and summary in the fixture is free of
/// credential-shaped text.
#[test]
fn opencode_cassette_is_secret_free() {
    ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads")
        .assert_no_secrets();
}

/// Replaying the whole committed session through one decoder reproduces every
/// frozen observation stream and per-turn decision.
#[test]
fn opencode_cassette_decodes_full_session() {
    let cassette = ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads");

    let mut decoder = OpenCodeStreamDecoder::new(decode_context());
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

/// Asserts a decoder [`OpenCodeDecision`] carries the same payload as the recorded
/// cassette [`CassetteDecision`].
fn assert_decision_matches(actual: &OpenCodeDecision, expected: &CassetteDecision, turn: usize) {
    match (actual, expected) {
        (
            OpenCodeDecision::Completed { output },
            CassetteDecision::Completed {
                output: expected_output,
            },
        ) => assert_eq!(output, expected_output, "turn {turn} completed output"),
        (
            OpenCodeDecision::Failed { error },
            CassetteDecision::Failed {
                error: expected_error,
            },
        ) => assert_eq!(error, expected_error, "turn {turn} failure"),
        _ => {
            panic!("turn {turn}: decoder decision {actual:?} does not match cassette {expected:?}")
        }
    }
}

/// Blank, `step_start`, `reasoning`, a `tool-calls` `step_finish`, and unknown
/// frames are tolerated: they produce neither observations nor a decision.
#[test]
fn opencode_cassette_tolerates_unknown_and_blank_frames() {
    let mut decoder = OpenCodeStreamDecoder::new(decode_context());

    for line in [
        "",
        "   ",
        &json!({ "type": "step_start", "part": { "type": "step-start" } }).to_string(),
        &json!({ "type": "reasoning", "part": { "type": "reasoning", "text": "thinking" } })
            .to_string(),
        &json!({
            "type": "step_finish",
            "part": {
                "type": "step-finish",
                "reason": "tool-calls",
                "tokens": { "input": 10, "output": 5, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
            },
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
fn opencode_cassette_rejects_malformed_frames() {
    let malformed = [
        "this is not json",
        &json!([1, 2, 3]).to_string(),
        &json!({ "no_type": true }).to_string(),
        &json!({ "type": 7 }).to_string(),
        &json!({ "type": "text" }).to_string(),
        &json!({ "type": "tool_use" }).to_string(),
        &json!({ "type": "step_finish" }).to_string(),
        &json!({ "type": "text", "part": 7 }).to_string(),
    ];

    for line in malformed {
        let mut decoder = OpenCodeStreamDecoder::new(decode_context());
        match decoder.push_line(line) {
            Err(ExternalAgentError::Protocol { .. }) => {}
            other => panic!("expected a Protocol error for {line:?}, got {other:?}"),
        }
    }
}

/// A top-level `error` frame decodes to a classified failure rather than a
/// completion, and emits no `SessionCompleted` observation.
#[test]
fn opencode_cassette_decodes_error_as_failed() {
    let mut decoder = OpenCodeStreamDecoder::new(decode_context());
    let failed = json!({
        "type": "error",
        "error": { "name": "AuthError", "data": { "message": "usage limit reached" } },
    })
    .to_string();
    match decoder.push_line(&failed) {
        Ok(Some(OpenCodeDecision::Failed {
            error: ExternalAgentError::Runtime { code, message },
        })) => {
            assert_eq!(code, None);
            assert_eq!(message, "usage limit reached");
        }
        other => panic!("expected a Runtime failure, got {other:?}"),
    }
    assert!(
        decoder.take_observations().is_empty(),
        "a failed turn must not emit a SessionCompleted observation",
    );
}
