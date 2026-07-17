//! ACP `session/update` *decoder* cassette suite (M10-2, feature `external-acp`).
//!
//! A committed cassette
//! ([`ExternalRuntimeCassette`](agent_testkit::prelude::ExternalRuntimeCassette))
//! freezes what the connection-private
//! [`AcpStreamDecoder`](agent_lib::agent::external::AcpStreamDecoder) must
//! produce: for each prompt turn it stores the raw JSON-RPC frames the decoder
//! consumes (agent→client `session/update` notifications, `session/new` and
//! `session/prompt` responses, and a `session/request_permission` request), the
//! sequenced
//! [`ExternalObservedEvent`](agent_lib::agent::external::ExternalObservedEvent)
//! stream it emits, and the
//! [`AcpDecision`](agent_lib::agent::external::AcpDecision) it settles on. This
//! suite proves the decoder end to end, entirely offline and with no real ACP
//! agent binary or official-crate type on the wire:
//!
//! - **decode** — one decoder spans the whole session; replaying every recorded
//!   frame reproduces the frozen observations and per-turn decision, covering
//!   assistant text, a plan/todo update, a (non-command) tool call, a file diff,
//!   a cached permission request, and end-of-turn completion;
//! - **client requests** — the `session/request_permission` frame is surfaced as
//!   a cached [`PendingClientRequest`](agent_lib::agent::external::PendingClientRequest)
//!   the live adapter (M10-3) will service;
//! - **redaction** — the committed fixture is free of credential-shaped text.
//!
//! The fixture under `tests/fixtures/external/acp/` is the committed source of
//! truth; [`acp_cassette_regenerate_fixture`] rewrites it from the in-code
//! builder only when `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` is set, so a normal
//! run never overwrites a committed file.
//!
//! Run with `cargo test --features external-acp --test agent_acp_cassette`, or
//! filter with `cargo test --features external-acp acp_cassette`.

#![cfg(feature = "external-acp")]

use std::path::PathBuf;

use agent_testkit::prelude::*;

use agent_lib::agent::external::{
    AcpDecision, AcpStreamDecoder, ExternalAgentEvent, ExternalAgentOutput, ExternalObservedEvent,
    PendingClientRequest, acp_runtime_kind,
};
use agent_lib::model::tool::ToolStatus;
use serde_json::{Value, json};

/// Environment opt-in that lets [`acp_cassette_regenerate_fixture`] rewrite the
/// committed fixture. Unset on a normal/CI run.
const UPDATE_ENV_VAR: &str = "AGENT_LIB_UPDATE_EXTERNAL_CASSETTES";

/// Runtime-assigned ACP session id the fixture fixes.
const SESSION_ID: &str = "sess-019f6e33-c533-7680-9bb7-7d3b5a45e780";

/// Absolute path to the committed decoder cassette.
fn full_session_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external/acp/full_session.json")
}

/// Wraps a JSON value as a verbatim stdout JSON-RPC frame line.
fn frame(value: Value) -> CassetteFrame {
    CassetteFrame::stdout(value.to_string())
}

/// A `session/update` notification carrying `update` for [`SESSION_ID`].
fn update_frame(update: Value) -> CassetteFrame {
    frame(json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": { "sessionId": SESSION_ID, "update": update },
    }))
}

/// Turn 1: `Start` → the session is created, streams text, publishes a plan,
/// runs a (non-command) search tool call, then completes.
fn turn_one() -> CassetteTurn {
    CassetteTurn::new(CassetteDecision::Completed {
        output: ExternalAgentOutput {
            summary: "I'll refactor the parser.".to_owned(),
            artifacts: Vec::new(),
            usage: None,
            cost_micros: None,
        },
    })
    .expecting(CassetteInputKind::Start)
    .with_frames([
        frame(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "sessionId": SESSION_ID },
        })),
        update_frame(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "I'll refactor the parser." },
        })),
        update_frame(json!({
            "sessionUpdate": "plan",
            "entries": [
                { "content": "outline the parser", "priority": "high", "status": "in_progress" },
                { "content": "add tests", "priority": "medium", "status": "pending" },
            ],
        })),
        update_frame(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-search",
            "title": "search the workspace docs",
            "kind": "search",
            "status": "pending",
        })),
        update_frame(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-search",
            "status": "completed",
        })),
        frame(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": { "stopReason": "end_turn" },
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
            ExternalAgentEvent::TaskUpdated {
                task_id: "0".to_owned(),
                status: "in_progress".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            3,
            ExternalAgentEvent::TaskUpdated {
                task_id: "1".to_owned(),
                status: "pending".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            4,
            ExternalAgentEvent::ToolStarted {
                name: "search the workspace docs".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            5,
            ExternalAgentEvent::ToolFinished {
                name: "search the workspace docs".to_owned(),
                status: ToolStatus::Ok,
            },
        ),
        ExternalObservedEvent::new(6, ExternalAgentEvent::SessionCompleted),
    ])
}

/// Turn 2: `Continue` (resume) → the agent streams a line, opens a file-edit
/// tool call, asks for permission (cached, not answered by the decoder), reports
/// the edit as a diff, then completes.
fn turn_two() -> CassetteTurn {
    CassetteTurn::new(CassetteDecision::Completed {
        output: ExternalAgentOutput {
            summary: "Applying the patch to src/lib.rs.".to_owned(),
            artifacts: Vec::new(),
            usage: None,
            cost_micros: None,
        },
    })
    .expecting(CassetteInputKind::Continue)
    .with_frames([
        update_frame(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "Applying the patch to src/lib.rs." },
        })),
        update_frame(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-edit",
            "title": "edit src/lib.rs",
            "kind": "edit",
            "status": "pending",
        })),
        frame(json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "session/request_permission",
            "params": {
                "sessionId": SESSION_ID,
                "toolCall": { "toolCallId": "call-edit", "title": "write src/lib.rs" },
                "options": [],
            },
        })),
        update_frame(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-edit",
            "status": "completed",
            "content": [
                { "type": "diff", "path": "/repo/src/lib.rs", "newText": "pub fn added() {}\n" },
            ],
        })),
        frame(json!({
            "jsonrpc": "2.0",
            "id": 11,
            "result": { "stopReason": "end_turn" },
        })),
    ])
    .emitting([
        ExternalObservedEvent::new(
            7,
            ExternalAgentEvent::TextDelta {
                text: "Applying the patch to src/lib.rs.".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            8,
            ExternalAgentEvent::ToolStarted {
                name: "edit src/lib.rs".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            9,
            ExternalAgentEvent::PermissionRequested {
                action_id: "10".to_owned(),
                summary: "write src/lib.rs".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            10,
            ExternalAgentEvent::FilePatch {
                path: "/repo/src/lib.rs".to_owned(),
                summary: "edit /repo/src/lib.rs".to_owned(),
                diff_ref: None,
            },
        ),
        ExternalObservedEvent::new(
            11,
            ExternalAgentEvent::ToolFinished {
                name: "edit src/lib.rs".to_owned(),
                status: ToolStatus::Ok,
            },
        ),
        ExternalObservedEvent::new(12, ExternalAgentEvent::SessionCompleted),
    ])
}

/// The permission request the fixture expects the decoder to cache for M10-3.
fn expected_client_request() -> PendingClientRequest {
    PendingClientRequest::Permission {
        action_id: "10".to_owned(),
        summary: "write src/lib.rs".to_owned(),
    }
}

/// The in-code source of truth for `full_session.json`.
fn full_session_cassette() -> ExternalRuntimeCassette {
    ExternalRuntimeCassette::new(
        CassetteRuntimeInfo::new(acp_runtime_kind())
            .with_version("acp-wire-1-fixture")
            .with_probe("agent-client-protocol 1.2.0 (offline decoder fixture)")
            .with_session_id(SESSION_ID),
    )
    .with_redaction(
        RedactionMetadata::applied("<redacted>").with_notes(
            "synthetic ACP session/update stream; no real prompt or credential content",
        ),
    )
    .with_turn(turn_one())
    .with_turn(turn_two())
}

/// Records `full_session.json` from [`full_session_cassette`] — but only when
/// `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1`, so a default run never overwrites the
/// committed fixture.
#[test]
fn acp_cassette_regenerate_fixture() {
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
fn acp_cassette_matches_in_code_builder() {
    let loaded = ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads");

    assert_eq!(
        loaded,
        full_session_cassette(),
        "the committed fixture drifted from the in-code builder; \
         rerun with {UPDATE_ENV_VAR}=1 to regenerate it",
    );
    assert_eq!(loaded.schema_version, EXTERNAL_CASSETTE_SCHEMA_VERSION);
    assert_eq!(loaded.runtime.kind, acp_runtime_kind());
    assert_eq!(loaded.runtime.session_id.as_deref(), Some(SESSION_ID));
    assert_eq!(loaded.turns.len(), 2);
}

/// Every committed frame, observation, and summary in the fixture is free of
/// credential-shaped text.
#[test]
fn acp_cassette_is_secret_free() {
    ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads")
        .assert_no_secrets();
}

/// Replaying the whole committed session through one decoder reproduces every
/// frozen observation stream, per-turn decision, and the cached permission
/// request.
#[test]
fn acp_cassette_decodes_full_session() {
    let cassette = ExternalRuntimeCassette::load(full_session_path())
        .expect("the committed decoder cassette loads");

    let mut decoder = AcpStreamDecoder::new();
    let mut client_requests = Vec::new();
    for (index, turn) in cassette.turns.iter().enumerate() {
        let mut decision = None;
        for cassette_frame in &turn.input_frames {
            if let Some(reached) = decoder
                .push_jsonrpc_line(&cassette_frame.payload)
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
        client_requests.extend(decoder.take_client_requests());

        let decision = decision.unwrap_or_else(|| panic!("turn {index} settled on no decision"));
        assert_decision_matches(&decision, &turn.decision, index);
    }

    assert_eq!(decoder.session_id(), Some(SESSION_ID));
    assert_eq!(client_requests, vec![expected_client_request()]);
}

/// Asserts a decoder [`AcpDecision`] carries the same payload as the recorded
/// cassette [`CassetteDecision`].
fn assert_decision_matches(actual: &AcpDecision, expected: &CassetteDecision, turn: usize) {
    match (actual, expected) {
        (
            AcpDecision::Completed { output },
            CassetteDecision::Completed {
                output: expected_output,
            },
        ) => assert_eq!(output, expected_output, "turn {turn} completed output"),
        (
            AcpDecision::Failed { error },
            CassetteDecision::Failed {
                error: expected_error,
            },
        ) => assert_eq!(error, expected_error, "turn {turn} failure"),
        _ => {
            panic!("turn {turn}: decoder decision {actual:?} does not match cassette {expected:?}")
        }
    }
}
