//! Core Rust suite: external-agent session-advance basics (milestone 3, M3-2).
//!
//! Fast, offline regressions over the basic advance path of an
//! [`ExternalAgentMachine`](agent_lib::agent::ExternalAgentMachine), driven to
//! the end of one turn through the testkit
//! [`DrainHarness`](agent_testkit::prelude::DrainHarness) with the scripted
//! [`ScriptedExternalSessionHandler`](agent_testkit::prelude::ScriptedExternalSessionHandler)
//! standing in for a real runtime. Each `#[tokio::test]` proves one invariant:
//!
//! - start → completed — a first user message reifies one `NeedExternalSession`
//!   carrying a `Start`, the scripted `Completed` result records the resumable
//!   session, folds the runtime's terminal output into a committed turn, and the
//!   machine settles on the `Done` cursor.
//! - start → failed — a scripted `Failed` result records the retained session
//!   facts, discards the pending turn, and settles the machine on the `Error`
//!   cursor.
//! - continue — a second user message on an established session advances it with
//!   a `Continue` rather than a fresh `Start`, committing a second turn.
//!
//! Run in isolation with `cargo test --test agent_external_basic`, or filter the
//! two start-path regressions the milestone calls out with
//! `cargo test external_agent_start`.

use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::LoopCursorKind;

/// A first user message drives one `Start` session step whose `Completed` result
/// records the session, commits the turn, and settles the machine on `Done`.
#[tokio::test]
async fn external_agent_start_to_completed() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let handler = ScriptedExternalSessionHandler::from_steps([ExternalSessionStep::result(
        fixture.completed(),
    )]);
    let log = Arc::clone(handler.log());
    let scope = TestScope::builder().external(Arc::new(handler)).build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the start→completed advance drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);
    assert_external_calls(&log)
        .count(1)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start])
        .result_kinds(&[ExternalResultKind::Completed]);

    let machine = harness.into_machine();
    assert!(
        machine.state().session().is_some(),
        "a completed advance records the resumable session facts"
    );
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// A scripted `Failed` result retains the session facts, discards the pending
/// turn, and settles the machine on the `Error` cursor.
#[tokio::test]
async fn external_agent_start_to_failed() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let handler =
        ScriptedExternalSessionHandler::from_steps([ExternalSessionStep::result(fixture.failed())]);
    let log = Arc::clone(handler.log());
    let scope = TestScope::builder().external(Arc::new(handler)).build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("a failed advance still drains to a terminal error cursor");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Error);
    assert_external_calls(&log)
        .count(1)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start])
        .result_kinds(&[ExternalResultKind::Failed]);

    let machine = harness.into_machine();
    assert!(
        machine.state().session().is_some(),
        "a failed advance retains the session facts reported before the failure"
    );
    assert_conversation(machine.state().conversation())
        .committed_turns(0)
        .pending_none();
}

/// A second user message on an established session advances it with a `Continue`
/// rather than starting fresh, committing a second turn.
#[tokio::test]
async fn external_agent_continue_advances_established_session() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let handler = ScriptedExternalSessionHandler::from_steps([
        ExternalSessionStep::result(fixture.completed()),
        ExternalSessionStep::result(fixture.completed()),
    ]);
    let log = Arc::clone(handler.log());
    let scope = TestScope::builder().external(Arc::new(handler)).build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    harness
        .run_user("refactor the parser")
        .await
        .expect("the first turn starts and completes a session");
    let second = harness
        .run_user("now add tests")
        .await
        .expect("the second turn continues the established session");

    assert_eq!(second.final_cursor().kind(), LoopCursorKind::Done);
    assert_external_calls(&log)
        .count(2)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start, ExternalInputKind::Continue])
        .result_kinds(&[ExternalResultKind::Completed, ExternalResultKind::Completed]);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(2)
        .pending_none();
}
