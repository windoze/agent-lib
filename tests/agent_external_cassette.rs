//! Core Rust suite: external *runtime cassette* schema, loader, redaction, and
//! offline replay (M5-3).
//!
//! A runtime cassette
//! ([`ExternalRuntimeCassette`](agent_testkit::prelude::ExternalRuntimeCassette))
//! freezes what a concrete runtime adapter's parser (Claude Code / Codex /
//! OpenCode, milestones 6–8) must produce: the raw CLI output frames it consumes,
//! the sequenced [`ExternalObservedEvent`](agent_lib::agent::external::ExternalObservedEvent)
//! stream it emits, and the decision point it settles on. This suite proves the
//! layer end to end, entirely offline:
//!
//! - **loader** — a committed synthetic fixture round-trips, an unknown schema
//!   version is rejected, and unrecognised fields are preserved raw;
//! - **redaction** — every committed fixture is free of credential-shaped text;
//! - **replay** — a loaded cassette drives the whole managed loop (start, tool
//!   batch, interaction, subagent) through a real
//!   [`ExternalSessionRegistry`](agent_lib::agent::external::ExternalSessionRegistry)
//!   and the reference [`DrainHarness`](agent_testkit::prelude::DrainHarness),
//!   replaying each recorded observation on its frozen `seq` line.
//!
//! The synthetic fixtures under `tests/fixtures/external/synthetic/` are the
//! committed source of truth; [`external_cassette_regenerate_fixtures`] rewrites
//! `full_stream.json` from the in-code builder only when
//! `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` is set, so a normal run never
//! overwrites a committed file.
//!
//! Run in isolation with `cargo test --test agent_external_cassette`, or filter
//! with `cargo test external_cassette`.

use std::path::PathBuf;
use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::external::{ExternalAgentEvent, ExternalObservedEvent, ExternalRuntimeKind};
use agent_lib::agent::{LoopCursorKind, RequirementKindTag};
use agent_lib::model::tool::ToolStatus;

/// Environment opt-in that lets [`external_cassette_regenerate_fixtures`] rewrite
/// the committed `full_stream.json`. Unset on a normal/CI run, so the fixture is
/// never overwritten implicitly.
const UPDATE_ENV_VAR: &str = "AGENT_LIB_UPDATE_EXTERNAL_CASSETTES";

/// Runtime-assigned session id the synthetic cassettes fix.
const SESSION_ID: &str = "cassette-sess-1";

/// Absolute path to the committed synthetic fixtures directory.
fn synthetic_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external/synthetic")
}

/// Path to the rich single-turn synthetic cassette.
fn full_stream_path() -> PathBuf {
    synthetic_dir().join("full_stream.json")
}

/// Path to the forward-compatibility (unknown-field) synthetic cassette.
fn forward_compat_path() -> PathBuf {
    synthetic_dir().join("forward_compat.json")
}

/// The in-code source of truth for `full_stream.json`.
///
/// One `Start → Completed` turn whose observations cover every synthetic event
/// category the task requires — text delta, command start/finish, permission
/// request, tool call, and completion — on one monotonic `seq` line, so replaying
/// it drives the machine to `Done` in a single advance with no host tool or
/// interaction handler.
fn full_stream_cassette() -> ExternalRuntimeCassette {
    ExternalRuntimeCassette::new(
        CassetteRuntimeInfo::new(ExternalRuntimeKind::ClaudeCode)
            .with_version("1.0.0-synthetic")
            .with_probe("claude-code 1.0.0-synthetic (offline fixture)")
            .with_session_id(SESSION_ID),
    )
    .with_redaction(
        RedactionMetadata::applied("<redacted>")
            .with_notes("synthetic fixture; no real prompt or credential content"),
    )
    .with_turn(
        CassetteTurn::new(CassetteDecision::Completed {
            output: output_summary("refactored the parser module"),
        })
        .expecting(CassetteInputKind::Start)
        .with_frames([
            CassetteFrame::stdout("{\"type\":\"session\",\"event\":\"started\"}"),
            CassetteFrame::stdout("{\"type\":\"assistant\",\"text\":\"Refactoring the parser.\"}"),
            CassetteFrame::stdout(
                "{\"type\":\"command\",\"phase\":\"start\",\"cmd\":\"cargo fmt\"}",
            ),
            CassetteFrame::stdout("{\"type\":\"command\",\"phase\":\"end\",\"exit\":0}"),
            CassetteFrame::stdout("{\"type\":\"permission\",\"id\":\"act-1\"}"),
            CassetteFrame::stdout(
                "{\"type\":\"tool\",\"phase\":\"start\",\"name\":\"apply_patch\"}",
            ),
            CassetteFrame::stdout("{\"type\":\"tool\",\"phase\":\"end\",\"name\":\"apply_patch\"}"),
            CassetteFrame::stdout("{\"type\":\"session\",\"event\":\"completed\"}"),
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
                    text: "Refactoring the parser module.".to_owned(),
                },
            ),
            ExternalObservedEvent::new(
                2,
                ExternalAgentEvent::CommandStarted {
                    command: "cargo fmt".to_owned(),
                    cwd: "/workspace".to_owned(),
                },
            ),
            ExternalObservedEvent::new(
                3,
                ExternalAgentEvent::CommandFinished {
                    exit_code: Some(0),
                    stdout_tail: "ok".to_owned(),
                    stderr_tail: String::new(),
                },
            ),
            ExternalObservedEvent::new(
                4,
                ExternalAgentEvent::PermissionRequested {
                    action_id: "act-1".to_owned(),
                    summary: "run `cargo test`".to_owned(),
                },
            ),
            ExternalObservedEvent::new(
                5,
                ExternalAgentEvent::ToolStarted {
                    name: "apply_patch".to_owned(),
                },
            ),
            ExternalObservedEvent::new(
                6,
                ExternalAgentEvent::ToolFinished {
                    name: "apply_patch".to_owned(),
                    status: ToolStatus::Ok,
                },
            ),
            ExternalObservedEvent::new(7, ExternalAgentEvent::SessionCompleted),
        ]),
    )
}

/// Builds a terminal output carrying just a `summary` (no artifacts/usage/cost).
fn output_summary(summary: &str) -> agent_lib::agent::external::ExternalAgentOutput {
    agent_lib::agent::external::ExternalAgentOutput {
        summary: summary.to_owned(),
        artifacts: Vec::new(),
        usage: None,
        cost_micros: None,
    }
}

/// Records `full_stream.json` from [`full_stream_cassette`] — but only when
/// `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1`.
///
/// This keeps the committed fixture in lock-step with the in-code builder while
/// leaving a default run a no-op, so the committed file is never overwritten
/// implicitly (mirroring the `regenerate_*_cassette` convention used by the
/// milestone-3 replay suites).
#[test]
fn external_cassette_regenerate_fixtures() {
    if std::env::var(UPDATE_ENV_VAR).as_deref() != Ok("1") {
        return;
    }
    let json = full_stream_cassette()
        .to_json_string_pretty()
        .expect("the synthetic full-stream cassette serializes");
    std::fs::write(full_stream_path(), format!("{json}\n"))
        .expect("the synthetic full-stream cassette is written to disk");
}

/// The committed `full_stream.json` loads and matches the in-code builder exactly,
/// carrying the recorded runtime metadata and redaction.
#[test]
fn external_cassette_loads_synthetic_fixture() {
    let loaded = ExternalRuntimeCassette::load(full_stream_path())
        .expect("the committed full-stream cassette loads");

    assert_eq!(
        loaded,
        full_stream_cassette(),
        "the committed fixture drifted from the in-code builder; \
         rerun with {UPDATE_ENV_VAR}=1 to regenerate it",
    );
    assert_eq!(loaded.schema_version, EXTERNAL_CASSETTE_SCHEMA_VERSION);
    assert_eq!(loaded.runtime.kind, ExternalRuntimeKind::ClaudeCode);
    assert_eq!(loaded.runtime.session_id.as_deref(), Some(SESSION_ID));
    assert!(loaded.redaction.applied);
    assert_eq!(loaded.turns.len(), 1);
    assert_eq!(loaded.turns[0].expected_events.len(), 8);
    // No unknown fields in a clean, current-schema fixture.
    assert!(loaded.extra.is_empty());
}

/// A single `Start → Completed` cassette drains the machine to `Done` in one
/// advance, mirroring every recorded observation to the live sink on its frozen
/// `seq` line.
#[tokio::test]
async fn external_cassette_replay_drains_to_done() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let cassette = ExternalRuntimeCassette::load(full_stream_path())
        .expect("the committed full-stream cassette loads");
    let handler = CassetteRuntimeExternalSessionHandler::from_cassette(&cassette);
    let external_log = Arc::clone(handler.log());
    let sink = Arc::clone(handler.sink());
    let start_log = handler.start_log().clone();
    let scope = TestScope::builder().external(Arc::new(handler)).build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the cassette start→completed replay drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    // One fresh session started; one advance, keyed Start → Completed.
    assert_eq!(start_log.len(), 1);
    assert_external_calls(&external_log)
        .count(1)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start])
        .result_kinds(&[ExternalResultKind::Completed]);

    // The live sink replayed all eight observations on the recorded seq line.
    assert_eq!(sink.seqs(), vec![0, 1, 2, 3, 4, 5, 6, 7]);

    let machine = harness.into_machine();
    assert!(
        machine.state().session().is_some(),
        "a completed replay records the resumable session facts"
    );
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactored the parser module");
}

/// A cassette built in code survives a JSON round-trip and then replays a
/// `PausedForToolCalls → RespondToolResults → Completed` managed loop, reattaching
/// the same live session (no restart) across the tool batch.
#[tokio::test]
async fn external_cassette_replay_tool_batch_round_trip() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine_with_tool_ids();

    let cassette = ExternalRuntimeCassette::new(
        CassetteRuntimeInfo::new(ExternalRuntimeKind::ClaudeCode).with_session_id(SESSION_ID),
    )
    .with_turn(
        CassetteTurn::new(CassetteDecision::PausedForToolCalls {
            batch_id: fixture.tool_batch_id(),
            calls: vec![
                fixture.tool_call("call-a", "apply_patch"),
                fixture.tool_call("call-b", "run_tests"),
            ],
        })
        .expecting(CassetteInputKind::Start),
    )
    .with_turn(
        CassetteTurn::new(CassetteDecision::Completed {
            output: output_summary("refactor complete"),
        })
        .expecting(CassetteInputKind::RespondToolResults),
    );

    let cassette = round_trip(&cassette);
    let handler = CassetteRuntimeExternalSessionHandler::from_cassette(&cassette);
    let external_log = Arc::clone(handler.log());
    let start_log = handler.start_log().clone();

    let tool = ScriptedToolHandler::from_steps([
        ToolStep::ok("call-a", "patch applied"),
        ToolStep::ok("call-b", "1 passed"),
    ]);
    let tool_log = Arc::clone(tool.log());

    let scope = TestScope::builder()
        .external(Arc::new(handler))
        .tool(Arc::new(tool))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the cassette tool-batch round-trip drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    // A single fresh session serviced both advances: the reattach never restarts.
    assert_eq!(start_log.len(), 1);
    assert_external_calls(&external_log)
        .count(2)
        .all_completed()
        .input_kinds(&[
            ExternalInputKind::Start,
            ExternalInputKind::RespondToolResults,
        ])
        .result_kinds(&[
            ExternalResultKind::PausedForToolCalls,
            ExternalResultKind::Completed,
        ]);

    assert_eq!(tool_log.records().len(), 2);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// A round-tripped cassette replays a `PausedForInteraction → RespondInteraction
/// → Completed` managed loop.
#[tokio::test]
async fn external_cassette_replay_interaction_round_trip() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let cassette = ExternalRuntimeCassette::new(
        CassetteRuntimeInfo::new(ExternalRuntimeKind::ClaudeCode).with_session_id(SESSION_ID),
    )
    .with_turn(
        CassetteTurn::new(CassetteDecision::PausedForInteraction {
            action_id: "act-1".to_owned(),
            request: Interaction::permission(ids.step_id(), fixture.permission_request()),
        })
        .expecting(CassetteInputKind::Start)
        .emitting([ExternalObservedEvent::new(
            0,
            ExternalAgentEvent::PermissionRequested {
                action_id: "act-1".to_owned(),
                summary: "run `cargo test`".to_owned(),
            },
        )]),
    )
    .with_turn(
        CassetteTurn::new(CassetteDecision::Completed {
            output: output_summary("refactor complete"),
        })
        .expecting(CassetteInputKind::RespondInteraction),
    );

    let cassette = round_trip(&cassette);
    let handler = CassetteRuntimeExternalSessionHandler::from_cassette(&cassette);
    let external_log = Arc::clone(handler.log());
    let start_log = handler.start_log().clone();

    let interaction = ScriptedInteractionHandler::sequence([InteractionDecision::Approve]);
    let interaction_log = Arc::clone(interaction.log());

    let scope = TestScope::builder()
        .external(Arc::new(handler))
        .interaction(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the cassette interaction round-trip drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    assert_eq!(start_log.len(), 1);
    assert_external_calls(&external_log)
        .count(2)
        .all_completed()
        .input_kinds(&[
            ExternalInputKind::Start,
            ExternalInputKind::RespondInteraction,
        ])
        .result_kinds(&[
            ExternalResultKind::PausedForInteraction,
            ExternalResultKind::Completed,
        ]);

    assert_eq!(interaction_log.records().len(), 1);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// A round-tripped cassette replays a `PausedForSubagent → RespondSubagent →
/// Completed` managed loop, driving a real host subagent.
#[tokio::test]
async fn external_cassette_replay_subagent_round_trip() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    // Child: emits one NeedInteraction its own attended scope answers in place,
    // so it never pops to the parent and runs to completion.
    let child_machine = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            ids.requirement_id(),
            RequirementKind::NeedInteraction {
                request: Interaction::question(ids.step_id(), "child needs a human".to_owned()),
            },
        ))
        .done_after_all_resumed()
        .label("child")
        .build();
    let child_log = Arc::clone(child_machine.log());
    let child_interaction = Arc::new(ScriptedInteractionHandler::fixed(
        InteractionDecision::Answer("done".to_owned()),
    ));
    let child = SpawnedChildBuilder::new()
        .machine(child_machine)
        .scope(attended_child_scope(child_interaction).build())
        .opening(user_input(&ids, "open child"))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("child summary")
            .build(),
    );
    let subagent = Arc::clone(&spawner).into_handler(4);

    let cassette = ExternalRuntimeCassette::new(
        CassetteRuntimeInfo::new(ExternalRuntimeKind::ClaudeCode).with_session_id(SESSION_ID),
    )
    .with_turn(
        CassetteTurn::new(CassetteDecision::PausedForSubagent {
            request: fixture.subagent_request("spawn-1"),
        })
        .expecting(CassetteInputKind::Start),
    )
    .with_turn(
        CassetteTurn::new(CassetteDecision::Completed {
            output: output_summary("refactor complete"),
        })
        .expecting(CassetteInputKind::RespondSubagent),
    );

    let cassette = round_trip(&cassette);
    let handler = CassetteRuntimeExternalSessionHandler::from_cassette(&cassette);
    let external_log = Arc::clone(handler.log());
    let start_log = handler.start_log().clone();

    let scope = TestScope::builder()
        .external(Arc::new(handler))
        .subagent(Arc::new(subagent))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("investigate the flaky test")
        .await
        .expect("the cassette subagent round-trip drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    assert_eq!(start_log.len(), 1);
    assert_external_calls(&external_log)
        .count(2)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start, ExternalInputKind::RespondSubagent])
        .result_kinds(&[
            ExternalResultKind::PausedForSubagent,
            ExternalResultKind::Completed,
        ]);

    assert_eq!(
        child_log.resume_tags(),
        vec![RequirementKindTag::Interaction]
    );
    assert_eq!(spawner.spawn_calls(), 1);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// The loader rejects a cassette that names an unsupported schema version,
/// classifying it rather than failing with a vague shape error.
#[test]
fn external_cassette_rejects_unknown_schema_version() {
    let json = r#"{ "schema_version": 999, "runtime": { "kind": "codex" } }"#;
    match ExternalRuntimeCassette::from_json_str(json) {
        Err(ExternalCassetteError::UnsupportedSchemaVersion { found, supported }) => {
            assert_eq!(found, Some(999));
            assert_eq!(supported, EXTERNAL_CASSETTE_SCHEMA_VERSION);
        }
        other => panic!("expected UnsupportedSchemaVersion, got {other:?}"),
    }
}

/// The loader preserves unrecognised fields (top-level, runtime, turn, and frame)
/// raw rather than dropping them or failing, and they survive a re-serialize.
#[test]
fn external_cassette_preserves_unknown_fields() {
    let loaded = ExternalRuntimeCassette::load(forward_compat_path())
        .expect("the forward-compat cassette loads");

    assert!(loaded.extra.contains_key("future_top_level"));
    assert!(loaded.runtime.extra.contains_key("future_runtime_field"));

    let turn = &loaded.turns[0];
    assert!(turn.extra.contains_key("future_turn_field"));
    assert!(
        turn.input_frames[0]
            .extra
            .contains_key("future_frame_field")
    );

    // Preserved fields survive a round-trip back through the loader.
    let round = loaded
        .to_json_string()
        .expect("the forward-compat cassette re-serializes");
    let reloaded =
        ExternalRuntimeCassette::from_json_str(&round).expect("the re-serialized cassette reloads");
    assert_eq!(reloaded, loaded);
}

/// Every committed synthetic fixture is free of credential-shaped text, scanned
/// both as raw bytes on disk and through the cassette's own redaction guard.
#[test]
fn external_cassette_fixtures_are_redacted() {
    let fixtures = [full_stream_path(), forward_compat_path()];
    for path in fixtures {
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("fixture {} is readable: {error}", path.display()));
        let hits = scan_secrets(&raw);
        assert!(
            hits.is_empty(),
            "fixture {} carries credential-shaped text: {hits:?}",
            path.display(),
        );
        // The cassette's own guard agrees after a load + re-serialize.
        ExternalRuntimeCassette::load(&path)
            .unwrap_or_else(|error| panic!("fixture {} loads: {error}", path.display()))
            .assert_no_secrets();
    }
}

/// Serializes a cassette and reloads it through the loader, proving the built
/// shape survives the on-disk JSON boundary before it is replayed.
fn round_trip(cassette: &ExternalRuntimeCassette) -> ExternalRuntimeCassette {
    let json = cassette
        .to_json_string_pretty()
        .expect("the cassette serializes");
    ExternalRuntimeCassette::from_json_str(&json).expect("the serialized cassette reloads")
}
