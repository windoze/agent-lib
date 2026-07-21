//! Runtime tool-call pauses: NeedTool bridging, batch collection,
//! out-of-order/partial results, and tool-resume protocol violations.

use super::*;

#[test]
fn external_tool_pause_emits_need_tool_batch() {
    let mut machine = machine_with_tool_ids();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let calls = vec![
        external_tool_call("call-a", "apply_patch"),
        external_tool_call("call-b", "run_tests"),
    ];
    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", calls.clone()),
    )));

    assert!(paused.is_quiescent());
    assert!(paused.notifications.is_empty());

    // The pause reifies exactly one NeedTool per runtime call, in call order, and
    // no external-session requirement.
    assert_eq!(paused.requirements.len(), calls.len());
    let mut requirement_ids = Vec::new();
    for (requirement, call) in paused.requirements.iter().zip(&calls) {
        assert_eq!(requirement.tag(), RequirementKindTag::Tool);
        match &requirement.kind {
            RequirementKind::NeedTool {
                call: tool_call, ..
            } => {
                // Each bridged NeedTool carries the runtime's provider_call_id as
                // the provider-neutral ToolCall::id so the answer lines back up.
                assert_eq!(tool_call.id, call.provider_call_id);
                assert_eq!(tool_call.name, call.name);
            }
            other => panic!("expected a NeedTool requirement, got {other:?}"),
        }
        requirement_ids.push(requirement.id);
    }

    // The driver-facing cursor parks on AwaitingTool with every tool requirement
    // id outstanding, so a driver can rebuild its pending registry from it.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    let pending = machine.cursor().pending_requirement_ids();
    assert_eq!(pending.len(), requirement_ids.len());
    for id in &requirement_ids {
        assert!(
            pending.contains(id),
            "pending requirement ids must include {id}"
        );
    }

    // The serializable cursor records the batch id and the full tool addressing.
    match machine.state().cursor() {
        ExternalAgentCursor::AwaitingTool {
            batch_id,
            requirements,
        } => {
            assert_eq!(batch_id.as_str(), "batch-7");
            assert_eq!(requirements.ids().len(), calls.len());
        }
        other => panic!("expected an AwaitingTool cursor, got {other:?}"),
    }

    // The resumable session facts reported at the pause are recorded.
    assert_eq!(machine.state().session(), Some(&session_ref()));

    // The in-flight turn stays open across the pause and is not committed; tool
    // results are relayed to the runtime, never written into host history.
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_tool_pause_without_tool_ids_fails() {
    // The default machine has no ToolExecutionIds source (NoToolExecutionIds), so
    // it cannot mint a host tool-call id for a runtime tool pause.
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;
    assert!(machine.state().conversation().pending().is_some());

    let outcome = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", vec![external_tool_call("call-a", "apply_patch")]),
    )));

    // A tool pause without an id source settles on a classified error cursor and
    // emits no requirement.
    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);

    // The dangling pending turn is discarded so no half-open turn lingers.
    assert!(machine.state().conversation().pending().is_none());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_tool_results_resume_back_to_session_when_batch_complete() {
    let (mut machine, requirement_ids) = pause_on_two_tools();

    // The first result is collected but the batch is not yet complete: the
    // machine stays parked on the tool batch and emits nothing.
    let first = machine.step(StepInput::resume(tool_resolution(
        requirement_ids[0],
        "call-a",
        "patch applied",
    )));
    assert!(first.is_quiescent());
    assert!(first.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);

    // The final result completes the batch and relays every result back to the
    // runtime under the paused batch id, in the original call order.
    let done = machine.step(StepInput::resume(tool_resolution(
        requirement_ids[1],
        "call-b",
        "tests pass",
    )));
    assert!(done.is_quiescent());
    assert_responds_with_batch(&done, &["call-a", "call-b"]);

    // The successful host status/content ride back on the runtime-facing result.
    match &done.requirements[0].kind {
        RequirementKind::NeedExternalSession { request } => match &request.input {
            ExternalSessionInput::RespondToolResults { results, .. } => {
                assert_eq!(results[0].status, ToolStatus::Ok);
                assert_eq!(
                    results[0].content,
                    tool_response("call-a", "patch applied").content
                );
                assert!(results[0].error.is_none());
            }
            other => panic!("expected a RespondToolResults input, got {other:?}"),
        },
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }

    // The batch completion reparks on an outstanding external session (rendered as
    // a streaming step in the driver-facing view), keeping the turn open.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_tool_batch_accepts_out_of_order_results() {
    let (mut machine, requirement_ids) = pause_on_two_tools();

    // Resolve the second call first: an out-of-order result is collected without
    // completing the batch.
    let first = machine.step(StepInput::resume(tool_resolution(
        requirement_ids[1],
        "call-b",
        "tests pass",
    )));
    assert!(first.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);

    // Resolving the first call completes the batch; the results are still emitted
    // in the runtime's original call order, not completion order.
    let done = machine.step(StepInput::resume(tool_resolution(
        requirement_ids[0],
        "call-a",
        "patch applied",
    )));
    assert_responds_with_batch(&done, &["call-a", "call-b"]);
}

#[test]
fn external_tool_partial_result_keeps_waiting() {
    let (mut machine, requirement_ids) = pause_on_two_tools();

    let outcome = machine.step(StepInput::resume(tool_resolution(
        requirement_ids[0],
        "call-a",
        "patch applied",
    )));

    // A partial batch stays quiescent: no requirement is emitted, the cursor
    // stays parked on the tool batch, and no external-session step is started.
    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    match machine.state().cursor() {
        ExternalAgentCursor::AwaitingTool { batch_id, .. } => {
            assert_eq!(batch_id.as_str(), "batch-7");
        }
        other => panic!("expected an AwaitingTool cursor, got {other:?}"),
    }

    // The turn stays open across the incomplete batch.
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_tool_batch_returns_runtime_errors_to_the_runtime() {
    let (mut machine, requirement_ids) = pause_on_two_tools();

    machine.step(StepInput::resume(tool_resolution(
        requirement_ids[0],
        "call-a",
        "patch applied",
    )));
    // A tool that failed to execute is returned to the runtime as a failed result
    // (return-error-to-runtime policy), never stopping the turn.
    let done = machine.step(StepInput::resume(tool_error_resolution(
        requirement_ids[1],
        ToolRuntimeError::ExecutionFailed {
            tool_name: "run_tests".to_owned(),
            message: "boom".to_owned(),
        },
    )));

    assert_responds_with_batch(&done, &["call-a", "call-b"]);
    match &done.requirements[0].kind {
        RequirementKind::NeedExternalSession { request } => match &request.input {
            ExternalSessionInput::RespondToolResults { results, .. } => {
                assert_eq!(results[1].status, ToolStatus::Error);
                assert!(
                    results[1]
                        .error
                        .as_deref()
                        .is_some_and(|error| error.contains("run_tests")),
                    "runtime error text should ride back on the result: {:?}",
                    results[1].error
                );
            }
            other => panic!("expected a RespondToolResults input, got {other:?}"),
        },
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }
}

#[test]
fn external_tool_resume_wrong_requirement_fails() {
    let (mut machine, _requirement_ids) = pause_on_two_tools();

    // A requirement id outside the pending batch is a protocol violation.
    let stranger: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890aa"
        .parse()
        .expect("requirement id");
    let outcome = machine.step(StepInput::resume(tool_resolution(
        stranger, "call-z", "stray",
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn external_tool_resume_wrong_family_fails() {
    let (mut machine, requirement_ids) = pause_on_two_tools();

    // A NeedTool requirement cannot accept a non-tool result family.
    let outcome = machine.step(StepInput::resume(interaction_resolution(
        requirement_ids[0],
        "approved",
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("NeedTool requirement cannot accept"),
                "unexpected error text: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    assert!(machine.state().conversation().pending().is_none());
}
