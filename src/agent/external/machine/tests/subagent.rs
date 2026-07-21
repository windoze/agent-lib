//! Subagent phase (M3-1): PausedForSubagent bridging, child-result
//! relay, and subagent-resume protocol violations.

use super::*;

// ----- subagent phase (M3-1) -----------------------------------------------

/// Drives a machine to a subagent pause under `request_id` and returns it
/// alongside the emitted `NeedSubagent` requirement id, so a resume can target
/// the child result.
fn pause_on_subagent(request_id: &str) -> (ExternalAgentMachine, RequirementId) {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_subagent(subagent_request(request_id)),
    )));
    assert_eq!(paused.requirements.len(), 1);
    let requirement_id = paused.requirements[0].id;
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    (machine, requirement_id)
}

#[test]
fn external_subagent_pause_emits_need_subagent() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let request = subagent_request("spawn-3");
    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_subagent(request.clone()),
    )));

    assert!(paused.is_quiescent());
    assert!(paused.notifications.is_empty());

    // The pause reifies exactly one NeedSubagent, reusing the request's spec_ref,
    // brief, and result_schema unchanged, and no external-session requirement.
    assert_eq!(paused.requirements.len(), 1);
    let requirement = &paused.requirements[0];
    assert_eq!(requirement.tag(), RequirementKindTag::Subagent);
    match &requirement.kind {
        RequirementKind::NeedSubagent {
            spec_ref,
            brief,
            result_schema,
        } => {
            assert_eq!(spec_ref, &request.spec_ref);
            assert_eq!(brief, &request.brief);
            assert_eq!(result_schema, &request.result_schema);
        }
        other => panic!("expected a NeedSubagent requirement, got {other:?}"),
    }

    // The driver-facing cursor parks on the subagent requirement so a driver can
    // rebuild its pending registry from it.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![requirement.id]
    );

    // The serializable cursor records the outstanding requirement and the runtime
    // request id echoed back through RespondSubagent.
    match machine.state().cursor() {
        ExternalAgentCursor::AwaitingSubagent {
            requirement: cursor_requirement,
            request_id,
        } => {
            assert_eq!(cursor_requirement.id(), requirement.id);
            assert_eq!(request_id.as_str(), "spawn-3");
        }
        other => panic!("expected an AwaitingSubagent cursor, got {other:?}"),
    }

    // The resumable session facts reported at the pause are recorded.
    assert_eq!(machine.state().session(), Some(&session_ref()));

    // The in-flight turn stays open across the pause and is not committed; the
    // child result is relayed to the runtime, never written into host history.
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_subagent_result_responds_to_session() {
    let (mut machine, requirement_id) = pause_on_subagent("spawn-3");

    // The driven child's output feeds a RespondSubagent back to the runtime that
    // echoes the paused request id and reuses the established session facts.
    let responded = machine.step(StepInput::resume(subagent_resolution(
        requirement_id,
        "found the race in the scheduler",
    )));

    assert!(responded.is_quiescent());
    assert!(responded.notifications.is_empty());
    assert_eq!(responded.requirements.len(), 1);

    let requirement = &responded.requirements[0];
    match &requirement.kind {
        RequirementKind::NeedExternalSession { request } => {
            assert_eq!(request.session.as_ref(), Some(&session_ref()));
            match &request.input {
                ExternalSessionInput::RespondSubagent { request_id, output } => {
                    assert_eq!(request_id, &ExternalSubagentRequestId::new("spawn-3"));
                    assert_eq!(
                        output,
                        &ExternalSubagentOutput {
                            summary: "found the race in the scheduler".to_owned(),
                            raw: None,
                        }
                    );
                }
                other => panic!("resume must feed a RespondSubagent, got {other:?}"),
            }
        }
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }

    // The response reparks on an outstanding external session (rendered as a
    // streaming step), keeping the turn open.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![requirement.id]
    );
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_subagent_wrong_family_fails() {
    let (mut machine, requirement_id) = pause_on_subagent("spawn-3");

    // A NeedSubagent requirement cannot accept a non-subagent result family.
    let outcome = machine.step(StepInput::resume(interaction_resolution(
        requirement_id,
        "approved",
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("NeedSubagent requirement cannot accept"),
                "unexpected error text: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn external_subagent_resume_wrong_requirement_fails() {
    let (mut machine, _requirement_id) = pause_on_subagent("spawn-3");

    // A requirement id other than the outstanding subagent one is a protocol
    // violation.
    let stranger: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890ab"
        .parse()
        .expect("requirement id");
    let outcome = machine.step(StepInput::resume(subagent_resolution(stranger, "stray")));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn external_subagent_error_settles_error_cursor() {
    let (mut machine, requirement_id) = pause_on_subagent("spawn-3");

    // A subagent-drive failure settles the host turn on a classified error cursor
    // (this first version defers a runtime-visible child error payload).
    let outcome = machine.step(StepInput::resume(subagent_error_resolution(
        requirement_id,
        AgentError::SubagentDepthExceeded { limit: 4, depth: 5 },
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("external subagent failed"),
                "unexpected error text: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    assert!(machine.state().conversation().pending().is_none());
}
