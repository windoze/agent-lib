//! Turn-boundary external reconfiguration (M9-3): boundary apply,
//! in-flight queueing, hot rejection, and snapshot survival.

use super::*;

// --- M9-3: turn-boundary external reconfiguration ---------------------------

/// A distinct tool-set id used by the reconfiguration fixtures so a reconfigured
/// set is clearly different from the spec's initial single-tool set.
fn reconfig_tool_set_id() -> ToolSetId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f2"
        .parse()
        .expect("reconfig tool set id")
}

/// A two-tool reconfiguration target, distinct from the spec's initial
/// single-tool `apply_patch` set.
fn reconfig_tools() -> ToolSetRef {
    ToolSetRef::new(
        reconfig_tool_set_id(),
        vec![tool("apply_patch"), tool("run_tests")],
    )
}

#[test]
fn external_reconfig_at_boundary_updates_start_request_tools() {
    // A fresh machine rests at a turn boundary (no turn in flight), so a
    // reconfiguration applies immediately and the next Start request carries the
    // new tool set.
    let mut machine = machine();
    assert_eq!(machine.state().active_tools().tools().len(), 1);

    let outcome = machine
        .reconfigure(reconfig_tools(), ExternalReconfigTiming::NextBoundary)
        .expect("boundary reconfig applies");
    assert_eq!(outcome, ExternalReconfigOutcome::Applied);
    assert_eq!(machine.state().active_tools().id(), reconfig_tool_set_id());
    assert!(machine.state().pending_reconfig().is_none());

    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let request = need_session_request(&opened);
    assert!(matches!(request.input, ExternalSessionInput::Start { .. }));
    let names: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["apply_patch", "run_tests"]);
}

#[test]
fn external_reconfig_at_boundary_after_completed_turn_updates_continue_request() {
    // After a turn completes the cursor rests on Done, which is still a turn
    // boundary: a reconfiguration applies immediately and the next Continue
    // request carries the new tools.
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let first_id = opened.requirements[0].id;
    machine.step(StepInput::resume(external_resolution(
        first_id,
        completed_result(),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

    let outcome = machine
        .reconfigure(reconfig_tools(), ExternalReconfigTiming::NextBoundary)
        .expect("boundary reconfig applies after a completed turn");
    assert_eq!(outcome, ExternalReconfigOutcome::Applied);

    let followup = machine.step(StepInput::external(user_input_seq("now add tests", 1)));
    let request = need_session_request(&followup);
    assert!(matches!(
        request.input,
        ExternalSessionInput::Continue { .. }
    ));
    assert_eq!(request.tools.len(), 2);
}

#[test]
fn external_reconfig_in_flight_next_boundary_queues_without_touching_live_session() {
    // While a turn is in flight the live session's tool set cannot change: a
    // NextBoundary reconfiguration is queued and folded in only when the next
    // turn opens.
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_id = opened.requirements[0].id;
    // The request already dispatched with the original single-tool set.
    assert_eq!(need_session_request(&opened).tools.len(), 1);
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);

    let outcome = machine
        .reconfigure(reconfig_tools(), ExternalReconfigTiming::NextBoundary)
        .expect("in-flight NextBoundary reconfig queues");
    assert_eq!(outcome, ExternalReconfigOutcome::Queued);

    // The live in-flight session is untouched: the active set is unchanged, the
    // new set is only parked, and the machine stays parked on the same session.
    assert_eq!(machine.state().active_tools().tools().len(), 1);
    assert_eq!(
        machine.state().pending_reconfig().map(ToolSetRef::id),
        Some(reconfig_tool_set_id())
    );
    assert!(matches!(
        machine.state().cursor(),
        ExternalAgentCursor::AwaitingSession { .. }
    ));

    // Completing the in-flight turn leaves the queued reconfiguration intact.
    machine.step(StepInput::resume(external_resolution(
        session_id,
        completed_result(),
    )));
    assert_eq!(
        machine.state().pending_reconfig().map(ToolSetRef::id),
        Some(reconfig_tool_set_id())
    );

    // The next turn folds it in, so the fresh Continue carries the new tools.
    let followup = machine.step(StepInput::external(user_input_seq("now add tests", 1)));
    let request = need_session_request(&followup);
    assert!(matches!(
        request.input,
        ExternalSessionInput::Continue { .. }
    ));
    assert_eq!(request.tools.len(), 2);
    assert!(machine.state().pending_reconfig().is_none());
}

#[test]
fn external_reconfig_in_flight_hot_unsupported_is_rejected_without_changing_live_session() {
    // A hot (live, mid-turn) reconfiguration is not supported: it is rejected
    // with a classified UnsupportedCapability and leaves every piece of state
    // exactly as it was, so the live session is never silently changed.
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_id = opened.requirements[0].id;

    let error = machine
        .reconfigure(reconfig_tools(), ExternalReconfigTiming::Hot)
        .expect_err("in-flight hot reconfig is unsupported");
    match error {
        ExternalAgentError::UnsupportedCapability {
            runtime,
            capability,
            ..
        } => {
            assert_eq!(runtime, ExternalRuntimeKind::ClaudeCode);
            assert_eq!(capability, ExternalCapability::Reconfigure);
        }
        other => panic!("expected UnsupportedCapability, got {other:?}"),
    }

    // Nothing changed: active set, queue, and cursor are all untouched.
    assert_eq!(machine.state().active_tools().tools().len(), 1);
    assert!(machine.state().pending_reconfig().is_none());
    assert!(matches!(
        machine.state().cursor(),
        ExternalAgentCursor::AwaitingSession { .. }
    ));

    // The rejected request left no trace: the next turn still carries the
    // original tool set.
    machine.step(StepInput::resume(external_resolution(
        session_id,
        completed_result(),
    )));
    let followup = machine.step(StepInput::external(user_input_seq("now add tests", 1)));
    assert_eq!(need_session_request(&followup).tools.len(), 1);
}

#[test]
fn external_reconfig_hot_at_boundary_applies_immediately() {
    // At a turn boundary there is no live session to protect, so a hot request
    // behaves like a boundary reconfiguration and applies immediately.
    let mut machine = machine();
    let outcome = machine
        .reconfigure(reconfig_tools(), ExternalReconfigTiming::Hot)
        .expect("hot reconfig at a boundary applies");
    assert_eq!(outcome, ExternalReconfigOutcome::Applied);
    assert_eq!(machine.state().active_tools().id(), reconfig_tool_set_id());
}

#[test]
fn external_reconfig_queued_change_survives_state_snapshot_restore() {
    // A reconfiguration queued while a turn was in flight is persisted in the
    // serializable state, so it survives a snapshot/restore and is still folded
    // into the next turn opened on the restored machine.
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_id = opened.requirements[0].id;
    machine
        .reconfigure(reconfig_tools(), ExternalReconfigTiming::NextBoundary)
        .expect("queue reconfig mid-turn");
    machine.step(StepInput::resume(external_resolution(
        session_id,
        completed_result(),
    )));

    // Round-trip the state through serde; the queued set is retained.
    let encoded = serde_json::to_value(machine.state()).expect("serialize state");
    assert_eq!(
        encoded["pending_reconfig"]["tools"]
            .as_array()
            .expect("pending reconfig tools serialized")
            .len(),
        2
    );
    let restored: ExternalAgentState = serde_json::from_value(encoded).expect("deserialize state");
    assert_eq!(
        restored.pending_reconfig().map(ToolSetRef::id),
        Some(reconfig_tool_set_id())
    );

    let mut restored_machine =
        ExternalAgentMachine::new(restored, Arc::new(SeqRequirementIds::default()));
    let followup = restored_machine.step(StepInput::external(user_input_seq("now add tests", 1)));
    let request = need_session_request(&followup);
    assert!(matches!(
        request.input,
        ExternalSessionInput::Continue { .. }
    ));
    assert_eq!(request.tools.len(), 2);
    assert!(restored_machine.state().pending_reconfig().is_none());
}
