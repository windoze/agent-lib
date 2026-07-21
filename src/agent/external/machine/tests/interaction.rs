//! Interaction pauses: permission/question/choice relays and
//! interaction-result rejection paths.

use super::*;

#[test]
fn external_pause_emits_interaction_and_parks_on_awaiting_interaction() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_result("act-42"),
    )));

    assert!(paused.is_quiescent());
    assert!(paused.notifications.is_empty());
    assert_eq!(paused.requirements.len(), 1);

    // The pause reifies exactly one NeedInteraction carrying the runtime's
    // clarification, and no external session requirement.
    let interaction_requirement = &paused.requirements[0];
    assert_eq!(
        interaction_requirement.tag(),
        RequirementKindTag::Interaction
    );
    match &interaction_requirement.kind {
        RequirementKind::NeedInteraction { request } => {
            assert_eq!(request.step_id(), paused_step_id());
        }
        other => panic!("expected a NeedInteraction requirement, got {other:?}"),
    }

    // The resumable session facts reported at the pause are recorded, and the
    // in-flight turn stays open across the pause.
    assert_eq!(machine.state().session(), Some(&session_ref()));
    assert!(machine.state().conversation().pending().is_some());

    // The driver-facing cursor is a non-terminal streaming step stuck on the
    // interaction requirement.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![interaction_requirement.id]
    );
}

#[test]
fn external_interaction_resume_responds_with_the_paused_action_id() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_result("act-42"),
    )));
    let interaction_requirement_id = paused.requirements[0].id;

    let responded = machine.step(StepInput::resume(interaction_resolution(
        interaction_requirement_id,
        "yes, run the tests",
    )));

    assert!(responded.is_quiescent());
    assert!(responded.notifications.is_empty());
    assert_eq!(responded.requirements.len(), 1);

    // The resolved interaction re-enters the session as a RespondInteraction that
    // echoes the exact action id the pause carried and reuses the established
    // session facts.
    let requirement = &responded.requirements[0];
    match &requirement.kind {
        RequirementKind::NeedExternalSession { request } => {
            assert_eq!(request.session.as_ref(), Some(&session_ref()));
            match &request.input {
                ExternalSessionInput::RespondInteraction {
                    action_id,
                    response,
                } => {
                    assert_eq!(action_id, "act-42");
                    assert_eq!(
                        response,
                        &InteractionResponse::answer("yes, run the tests".to_owned())
                    );
                }
                other => panic!("resume must feed a RespondInteraction, got {other:?}"),
            }
        }
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }

    // The machine is back on AwaitingSession, stuck on the fresh external
    // requirement, with the turn still open.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![requirement.id]
    );
    assert!(machine.state().conversation().pending().is_some());
}

#[test]
fn external_pause_then_respond_then_complete_commits_the_turn() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));

    let paused = machine.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        paused_result("act-7"),
    )));
    let responded = machine.step(StepInput::resume(interaction_resolution(
        paused.requirements[0].id,
        "go ahead",
    )));
    let completed = machine.step(StepInput::resume(external_resolution(
        responded.requirements[0].id,
        completed_result(),
    )));

    assert!(completed.is_quiescent());
    assert!(completed.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

    // The whole pause↔respond loop folds into a single committed turn.
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
}

#[test]
fn external_interaction_resume_rejecting_a_non_interaction_result_fails() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let paused = machine.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        paused_result("act-42"),
    )));
    let interaction_requirement_id = paused.requirements[0].id;

    // A wrong-family result for an outstanding NeedInteraction settles on Error.
    let outcome = machine.step(StepInput::resume(external_resolution(
        interaction_requirement_id,
        completed_result(),
    )));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn external_interaction_resume_targeting_the_wrong_requirement_fails() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    machine.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        paused_result("act-42"),
    )));

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890ca"
        .parse()
        .expect("stray requirement id");
    let outcome = machine.step(StepInput::resume(interaction_resolution(stray, "hi")));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

/// Drives a machine to a pause on `paused`, returning the machine and the
/// `NeedInteraction` requirement id the pause reified.
fn paused_on_interaction(paused: ExternalSessionResult) -> (ExternalAgentMachine, RequirementId) {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let pause = machine.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        paused,
    )));
    let requirement_id = pause.requirements[0].id;
    (machine, requirement_id)
}

#[test]
fn external_permission_interaction_relays_approve() {
    // An approved permission is validated against the pending request and reaches
    // the runtime as a permission approve echoing the paused action id.
    let (mut machine, requirement_id) = paused_on_interaction(permission_paused_result("act-1"));

    let responded = machine.step(StepInput::resume(response_resolution(
        requirement_id,
        InteractionResponse::Permission(PermissionResponse::approve("act-1".to_owned())),
    )));

    let (fresh, response) = respond_interaction(&responded, "act-1");
    assert_eq!(
        response,
        InteractionResponse::Permission(PermissionResponse::approve("act-1".to_owned()))
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(machine.cursor().pending_requirement_ids(), vec![fresh]);
    assert!(machine.state().conversation().pending().is_some());
}

#[test]
fn external_permission_interaction_relays_deny() {
    // A denied permission relays a deny carrying its rationale.
    let (mut machine, requirement_id) = paused_on_interaction(permission_paused_result("act-1"));

    let responded = machine.step(StepInput::resume(response_resolution(
        requirement_id,
        InteractionResponse::Permission(PermissionResponse::deny(
            "act-1".to_owned(),
            Some("shell is blocked by policy".to_owned()),
        )),
    )));

    let (_, response) = respond_interaction(&responded, "act-1");
    assert_eq!(
        response,
        InteractionResponse::Permission(PermissionResponse::deny(
            "act-1".to_owned(),
            Some("shell is blocked by policy".to_owned()),
        ))
    );
}

#[test]
fn external_permission_interaction_relays_cancel() {
    // A cancelled permission relays a cancel keyed to the paused action.
    let (mut machine, requirement_id) = paused_on_interaction(permission_paused_result("act-1"));

    let responded = machine.step(StepInput::resume(response_resolution(
        requirement_id,
        InteractionResponse::Permission(PermissionResponse::cancel("act-1".to_owned())),
    )));

    let (_, response) = respond_interaction(&responded, "act-1");
    assert_eq!(
        response,
        InteractionResponse::Permission(PermissionResponse::cancel("act-1".to_owned()))
    );
}

#[test]
fn external_question_interaction_relays_answer() {
    // An open question answer type-aligns and relays verbatim.
    let (mut machine, requirement_id) = paused_on_interaction(paused_result("act-9"));

    let responded = machine.step(StepInput::resume(response_resolution(
        requirement_id,
        InteractionResponse::answer("yes, run the tests".to_owned()),
    )));

    let (_, response) = respond_interaction(&responded, "act-9");
    assert_eq!(
        response,
        InteractionResponse::answer("yes, run the tests".to_owned())
    );
}

#[test]
fn external_choice_interaction_relays_selected_index() {
    // A choice index within the offered options is accepted and relayed.
    let (mut machine, requirement_id) = paused_on_interaction(choice_paused_result(
        "act-3",
        vec!["main".to_owned(), "release".to_owned()],
    ));

    let responded = machine.step(StepInput::resume(response_resolution(
        requirement_id,
        InteractionResponse::Choice(1),
    )));

    let (_, response) = respond_interaction(&responded, "act-3");
    assert_eq!(response, InteractionResponse::Choice(1));
}

#[test]
fn interaction_result_rejected_on_action_mismatch_settles_error() {
    // A permission response addressing a different action than the pending
    // request is rejected into an error cursor and never relayed to the runtime.
    let (mut machine, requirement_id) = paused_on_interaction(permission_paused_result("act-1"));

    let outcome = machine.step(StepInput::resume(response_resolution(
        requirement_id,
        InteractionResponse::Permission(PermissionResponse::approve("act-99".to_owned())),
    )));

    assert!(outcome.is_quiescent());
    assert!(
        outcome.requirements.is_empty(),
        "a rejected interaction response must not relay a RespondInteraction"
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn interaction_result_rejected_on_choice_out_of_range_settles_error() {
    // A choice index past the offered options is rejected into an error cursor
    // and never relayed to the runtime.
    let (mut machine, requirement_id) = paused_on_interaction(choice_paused_result(
        "act-3",
        vec!["main".to_owned(), "release".to_owned()],
    ));

    let outcome = machine.step(StepInput::resume(response_resolution(
        requirement_id,
        InteractionResponse::Choice(5),
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn interaction_result_rejected_on_family_mismatch_settles_error() {
    // A wrong-family response (a free-form answer to a permission prompt) is
    // rejected into an error cursor and never relayed to the runtime.
    let (mut machine, requirement_id) = paused_on_interaction(permission_paused_result("act-1"));

    let outcome = machine.step(StepInput::resume(response_resolution(
        requirement_id,
        InteractionResponse::answer("looks fine".to_owned()),
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn interaction_result_rejected_keeps_the_turn_recoverable_state() {
    // The rejection path also discards the dangling pending turn, mirroring the
    // machine's other `fail` transitions.
    let (mut machine, requirement_id) = paused_on_interaction(permission_paused_result("act-1"));

    machine.step(StepInput::resume(response_resolution(
        requirement_id,
        InteractionResponse::answer("looks fine".to_owned()),
    )));

    assert!(machine.state().conversation().pending().is_none());
}
