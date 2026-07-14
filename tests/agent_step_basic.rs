//! Core Rust suite: single-machine step protocol basics (milestone 6, M6-3).
//!
//! These are fast, offline, single-invariant regressions over the synchronous
//! step contract of a [`DefaultAgentMachine`], driven entirely through the
//! testkit [`StepHarness`]. Where the testkit's own unit tests prove the harness
//! mechanics, this suite asserts on the *agent-observable* outcome of each move:
//! the emitted requirement batch, the cursor transition, and the committed
//! conversation. It covers, one `#[test]` each:
//!
//! - `NeedLlm` emit — a fresh user turn parks on exactly one `NeedLlm`.
//! - resume text — resuming that `NeedLlm` with an assistant text commits the
//!   turn and rests on `Done`.
//! - wrong id — a resume addressing a stray id is rejected before the machine is
//!   stepped, and the real requirement still commits afterward.
//! - wrong kind — a tool result offered for the `NeedLlm` is rejected before the
//!   machine is stepped.
//! - abandon — abandoning the opening `NeedLlm` discards the turn and settles the
//!   machine back to a feedable `Idle`.
//!
//! Run in isolation with `cargo test --test agent_step_basic`.

use agent_testkit::prelude::*;

use agent_lib::agent::{LoopCursorKind, RequirementId, RequirementResult};
use agent_lib::model::message::Role;

/// A fresh user turn parks the machine on exactly one `NeedLlm`, on the
/// `StreamingStep` cursor, and that requirement is the sole outstanding one.
#[test]
fn user_message_opens_on_a_single_need_llm() {
    let ids = SeqIds::new();
    let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
    let mut harness = StepHarness::with_ids(machine, ids);

    let opened = harness.user("hello");

    assert_eq!(opened.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_requirements(opened.requirements())
        .count(1)
        .single_llm();
    let llm_id = opened
        .single_llm()
        .expect("a text turn opens on NeedLlm")
        .id;
    assert_eq!(harness.outstanding_ids(), vec![llm_id]);
}

/// Resuming the opening `NeedLlm` with an assistant text response commits the
/// turn: the machine rests on `Done`, nothing stays outstanding, and the
/// conversation carries the user message followed by the assistant answer.
#[test]
fn resume_text_commits_the_turn() {
    let ids = SeqIds::new();
    let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
    let mut harness = StepHarness::with_ids(machine, ids);

    let llm_id = harness.user("hello").single_llm().expect("NeedLlm").id;
    let committed = harness.resume(
        llm_id,
        RequirementResult::Llm(Ok(assistant_text("hi there", usage(3, 2)))),
    );

    assert!(committed.is_quiescent());
    assert!(committed.requirements().is_empty());
    assert_eq!(committed.cursor().kind(), LoopCursorKind::Done);
    assert!(harness.outstanding_ids().is_empty());

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .message_role(0, 0, Role::User)
        .message_text(0, 0, "hello")
        .last_assistant_text("hi there");
}

/// A resume addressing a stray id is rejected before the machine is stepped: the
/// diagnostic names the live cursor and the real outstanding id, the machine
/// stays put, and the real requirement still commits afterward.
#[test]
fn wrong_id_resume_is_rejected_before_stepping() {
    let ids = SeqIds::new();
    let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
    let mut harness = StepHarness::with_ids(machine, ids);

    let real_id = harness.user("hello").single_llm().expect("NeedLlm").id;
    let stray =
        RequirementId::parse_str("018f0d9c-7b6a-7c12-8f31-0000feedbeef").expect("valid stray id");

    let error = harness
        .try_resume(
            stray,
            RequirementResult::Llm(Ok(assistant_text("hi", usage(1, 1)))),
        )
        .expect_err("a stray id cannot be resumed");
    assert_eq!(error.cursor(), LoopCursorKind::StreamingStep);
    assert_eq!(error.outstanding(), [real_id].as_slice());

    // The machine was never stepped: the real requirement is still open and can
    // still be resumed to commit the turn.
    assert_eq!(harness.outstanding_ids(), vec![real_id]);
    assert_eq!(harness.cursor().kind(), LoopCursorKind::StreamingStep);
    let committed = harness.resume(
        real_id,
        RequirementResult::Llm(Ok(assistant_text("hi", usage(1, 1)))),
    );
    assert_eq!(committed.cursor().kind(), LoopCursorKind::Done);
}

/// A tool result offered for the outstanding `NeedLlm` is the wrong family, so
/// the harness rejects it before stepping the machine.
#[test]
fn wrong_kind_resume_is_rejected_before_stepping() {
    let ids = SeqIds::new();
    let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
    let mut harness = StepHarness::with_ids(machine, ids);

    let llm_id = harness.user("hello").single_llm().expect("NeedLlm").id;

    let error = harness
        .try_resume(
            llm_id,
            RequirementResult::Tool(Ok(tool_ok("call-x", "nope"))),
        )
        .expect_err("a tool result cannot fulfil a NeedLlm");
    assert!(error.message().contains("rejected"), "{error}");

    // Unchanged: the LLM requirement is still outstanding on the same cursor.
    assert_eq!(harness.outstanding_ids(), vec![llm_id]);
    assert_eq!(harness.cursor().kind(), LoopCursorKind::StreamingStep);
}

/// Abandoning the opening `NeedLlm` discards the in-flight turn (never-resume)
/// and settles the machine back to a feedable `Idle`, with nothing committed.
#[test]
fn abandon_discards_the_turn_and_settles_idle() {
    let ids = SeqIds::new();
    let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
    let mut harness = StepHarness::with_ids(machine, ids);

    let llm_id = harness.user("hello").single_llm().expect("NeedLlm").id;
    let abandoned = harness.abandon(llm_id);

    assert!(abandoned.requirements().is_empty());
    assert!(harness.outstanding_ids().is_empty());
    assert_eq!(harness.cursor().kind(), LoopCursorKind::Idle);

    // The discarded turn left the conversation empty: no committed turn, no
    // pending turn.
    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(0)
        .pending_none();
}
