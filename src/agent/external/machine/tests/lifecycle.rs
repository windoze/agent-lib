//! Session lifecycle: start, continue, completed/failed resume, resume
//! rejection, pivot rejection, and abandon/cleanup flagging.

use super::*;

#[test]
fn external_user_message_blocks_on_start_session() {
    let mut machine = machine();
    let outcome = machine.step(StepInput::external(user_input("refactor the parser")));

    assert!(outcome.is_quiescent());
    assert_eq!(outcome.requirements.len(), 1);
    assert!(outcome.notifications.is_empty());

    let requirement = &outcome.requirements[0];
    match &requirement.kind {
        RequirementKind::NeedExternalSession { request } => {
            assert_eq!(request.agent_id, agent_id());
            assert_eq!(request.runtime, ExternalRuntimeKind::ClaudeCode);
            assert!(request.session.is_none());
            assert_eq!(request.tools.len(), 1);
            match &request.input {
                ExternalSessionInput::Start { prompt } => {
                    assert_eq!(prompt, "refactor the parser");
                }
                other => panic!("first advance must be a Start, got {other:?}"),
            }
        }
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }

    // The driver-facing cursor view is a non-terminal streaming step carrying the
    // outstanding requirement id.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![requirement.id]
    );

    // The Conversation opened a pending turn that is not yet committed.
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_completed_resume_commits_and_settles_done() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let requirement_id = opened.requirements[0].id;

    let resumed = machine.step(StepInput::resume(external_resolution(
        requirement_id,
        completed_result(),
    )));

    assert!(resumed.is_quiescent());
    assert!(resumed.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

    // The session facts are recorded and the terminal output committed as the
    // turn's assistant response.
    assert_eq!(machine.state().session(), Some(&session_ref()));
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
}

#[test]
fn external_continue_reuses_the_established_session() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let first_id = opened.requirements[0].id;
    machine.step(StepInput::resume(external_resolution(
        first_id,
        completed_result(),
    )));

    // A second user message on an established session continues rather than
    // starting fresh, and carries the recorded session facts.
    let followup = machine.step(StepInput::external(user_input_seq("now add tests", 1)));
    let requirement = &followup.requirements[0];
    match &requirement.kind {
        RequirementKind::NeedExternalSession { request } => {
            assert_eq!(request.session.as_ref(), Some(&session_ref()));
            match &request.input {
                ExternalSessionInput::Continue { message } => {
                    assert_eq!(message, "now add tests");
                }
                other => panic!("second advance must be a Continue, got {other:?}"),
            }
        }
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }
}

#[test]
fn external_failed_resume_settles_error() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let requirement_id = opened.requirements[0].id;

    let resumed = machine.step(StepInput::resume(external_resolution(
        requirement_id,
        failed_result(),
    )));

    assert!(resumed.is_quiescent());
    assert!(resumed.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);

    // A failed advance still records the retained session facts but leaves no
    // committed turn — the pending turn is discarded.
    assert_eq!(machine.state().session(), Some(&session_ref()));
    assert!(machine.state().conversation().pending().is_none());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_resume_targeting_the_wrong_requirement_fails() {
    let mut machine = machine();
    machine.step(StepInput::external(user_input("refactor the parser")));

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890c9"
        .parse()
        .expect("stray requirement id");
    let resumed = machine.step(StepInput::resume(external_resolution(
        stray,
        completed_result(),
    )));

    assert!(resumed.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn external_resume_while_idle_is_rejected() {
    let mut machine = machine();
    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890c9"
        .parse()
        .expect("stray requirement id");

    let outcome = machine.step(StepInput::resume(external_resolution(
        stray,
        completed_result(),
    )));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn external_pivot_input_is_rejected() {
    let mut machine = machine();
    let pivot = PivotMessage::new(
        "018f0d9c-7b6a-7c12-8f31-1234567890d1"
            .parse()
            .expect("pivot message id"),
        user_message("pivot"),
        PivotSource::Human,
    )
    .expect("valid pivot");

    let outcome = machine.step(StepInput::external(AgentInput::pivot(pivot)));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn external_agent_abandon_settles_and_flags_cleanup() {
    let mut machine = machine();
    machine.step(StepInput::external(user_input("refactor the parser")));
    assert!(machine.state().conversation().pending().is_some());
    // Opening the turn parked the machine on AwaitingSession, so a live runtime
    // session may exist and abandon must flag it for the handle layer.
    assert!(!machine.state().cleanup_required());

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890c9"
        .parse()
        .expect("stray requirement id");
    let outcome = machine.step(StepInput::abandon(stray));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    // Never-resume abandon settles to a feedable Idle without emitting Shutdown.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(machine.state().conversation().pending().is_none());
    // The orphaned session is flagged for the handle layer to force-close (§6.4).
    assert!(machine.state().cleanup_required());
}

#[test]
fn external_agent_abandon_while_awaiting_interaction_flags_cleanup() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    // Drive to a pause so the cursor parks on AwaitingInteraction with a live
    // session behind it.
    machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_result("act-42"),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(machine.state().conversation().pending().is_some());
    assert!(!machine.state().cleanup_required());

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890ca"
        .parse()
        .expect("stray requirement id");
    let outcome = machine.step(StepInput::abandon(stray));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(machine.state().conversation().pending().is_none());
    assert!(machine.state().cleanup_required());
}

#[test]
fn external_agent_abandon_when_idle_does_not_flag_cleanup() {
    // Abandoning a machine that never opened a session has nothing to sweep.
    let mut machine = machine();
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890cb"
        .parse()
        .expect("stray requirement id");
    let outcome = machine.step(StepInput::abandon(stray));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(!machine.state().cleanup_required());
}
