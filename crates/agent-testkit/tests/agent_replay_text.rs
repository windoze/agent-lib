//! Recorded replay suite (`agent_replay_text`, milestone 6, M6-4).
//!
//! The simplest offline replay: a single `user -> LLM text -> commit` turn with
//! no tools, interactions, or reconfigurations. It proves that a plain text turn
//! can run in CI with **no network, credentials, or live provider** off a
//! committed cassette ([`agent_text_turn.json`](../tests/cassettes)).
//!
//! Like the other replay suites, the committed fixture is *synthetic* recorded
//! data written in the exact shape [`CassetteRecorder`] produces:
//! [`regenerate_text_cassette`] records the same scenario through the recorder
//! and, only when
//! [`AGENT_TESTKIT_UPDATE_CASSETTES=1`](agent_testkit::cassette::UPDATE_ENV_VAR)
//! is set, rewrites the committed file. Recording the request the machine
//! actually issues keeps the entry fingerprint in lock-step with what the
//! machine reproduces on replay. A normal CI run leaves that test a no-op
//! (`RecorderReport::Skipped`), so the committed fixture is never overwritten.
//!
//! Run in isolation with `cargo test --test agent_replay_text`.

use std::path::PathBuf;
use std::sync::Arc;

use agent_lib::agent::drain;
use agent_lib::model::message::Role;
use agent_testkit::prelude::*;

/// The assistant's recorded, model-visible answer.
const ANSWER: &str = "Hello from the recorded model.";

/// Absolute path to the committed cassette fixture.
fn cassette_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/agent_text_turn.json")
}

/// The scripted LLM handler for the plain text turn: one non-zero-usage text
/// generation that closes the turn.
fn scripted_llm() -> ScriptedLlmHandler {
    ScriptedLlmHandler::from_steps([LlmStep::text(ANSWER).with_usage(usage(4, 3))])
}

/// Records the text turn through [`CassetteRecorder`] and rewrites the committed
/// fixture — but only when `AGENT_TESTKIT_UPDATE_CASSETTES=1`.
///
/// This is the fixture's source of truth: it wraps the scripted handler so the
/// recorder captures the *exact* request a real [`DefaultAgentMachine`] issues,
/// guaranteeing the entry fingerprint matches what the replay test reproduces. A
/// default CI run leaves it a no-op and never touches the committed file.
#[tokio::test]
async fn regenerate_text_cassette() {
    let recorder = CassetteRecorder::update(cassette_path()).with_metadata(
        CassetteMetadata::new("agent_text_turn")
            .with_description("user -> LLM text -> commit; no tools, network, or credentials"),
    );

    // A default CI run never opts in, so the fixture is left untouched.
    if let Some(reason) = recorder.skip_reason() {
        assert!(reason.contains("AGENT_TESTKIT_UPDATE_CASSETTES"));
        return;
    }

    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = agent_spec(&ids);
    let mut machine = default_machine(&ids, agent_state(&ids, spec));

    let scope = TestScope::builder()
        .llm(Arc::new(recorder.wrap_llm(scripted_llm())))
        .build();

    drain(&mut machine, user_input(&ids, "hello?"), &scope, None, &ctx)
        .await
        .expect("the recorded text turn drains to completion");

    let report = recorder.finish().expect("cassette write succeeds");
    match report {
        RecorderReport::Wrote { entry_count, .. } => assert_eq!(entry_count, 1),
        other => panic!("update mode must write the cassette, got {other:?}"),
    }
}

/// Replays the committed cassette through a real machine with no live backend.
///
/// The turn runs entirely off recorded data: [`CassetteLlmHandler`] serves the
/// single generation, so there is no network, credentials, or real provider. The
/// assertions cover the three things the task calls out — the committed
/// conversation, the handler call log, and the final cursor.
#[tokio::test]
async fn offline_replay_runs_a_text_turn() {
    let json = std::fs::read_to_string(cassette_path()).expect(
        "committed cassette fixture is present; regenerate with AGENT_TESTKIT_UPDATE_CASSETTES=1",
    );
    let cassette = Cassette::from_json_str(&json).expect("fixture parses at the current schema");
    assert_eq!(cassette.entries.len(), 1, "a single text generation");

    let player = CassettePlayer::new(cassette, "tests/cassettes/agent_text_turn.json");
    let llm = Arc::new(player.llm_handler());
    let llm_log = Arc::clone(llm.log());

    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = agent_spec(&ids);
    let mut machine = default_machine(&ids, agent_state(&ids, spec));

    // Only the LLM family is wired: the scope is headless, so any stray tool or
    // interaction would surface as an `UnhandledRequirement` rather than being
    // silently served.
    let scope = TestScope::builder().llm(llm.clone()).build();

    let done = drain(&mut machine, user_input(&ids, "hello?"), &scope, None, &ctx)
        .await
        .expect("the offline text turn drains to completion");

    // Final cursor: `assert_done` bakes in the clean `Done`-cursor check.
    assert_done(&done);

    // Handler call log: exactly one completed LLM generation and no other traffic.
    assert_calls(&llm_log).count(1).all_completed();

    // Committed conversation: one closed turn of user then the assistant's
    // recorded answer, with nothing left pending.
    assert_conversation(machine.state().conversation())
        .pending_none()
        .committed_turns(1)
        .open_call_count(0)
        .message_role(0, 0, Role::User)
        .message_role(0, 1, Role::Assistant)
        .last_assistant_text(ANSWER);

    // The assertion module intentionally stays out of block-level detail, so the
    // exact message count is checked directly against the committed conversation.
    let messages = machine.state().conversation().turns()[0].messages();
    assert_eq!(messages.len(), 2, "user then the assistant's answer");
}
