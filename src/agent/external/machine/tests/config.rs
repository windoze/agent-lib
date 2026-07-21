//! Machine-local config (M4-3): decision-loop limit, tool-failure
//! policy, and required-capability classification.

use super::*;

// ----- machine-local config: loop limit, tool-failure policy, capability
// requirements (M4-3) ------------------------------------------------------

/// A machine wired with a deterministic [`SeqToolIds`] source and the given
/// machine-local `config`, ready to drive a configured managed loop.
fn machine_with_config(config: ExternalAgentMachineConfig) -> ExternalAgentMachine {
    ExternalAgentMachine::new(
        ExternalAgentState::new(spec(), empty_conversation()),
        Arc::new(SeqRequirementIds::default()),
    )
    .with_tool_execution_ids(Arc::new(SeqToolIds::default()))
    .with_external_config(config)
}

/// Drives one runtime pause/respond loop: a single-tool pause under `batch_id`
/// followed by a successful host result, reparking the machine on the next
/// `NeedExternalSession`. Returns that requirement's id so the caller can feed
/// the next pause.
fn drive_one_tool_round(
    machine: &mut ExternalAgentMachine,
    session_requirement_id: RequirementId,
    batch_id: &str,
    provider_call_id: &str,
) -> RequirementId {
    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools(
            batch_id,
            vec![external_tool_call(provider_call_id, "apply_patch")],
        ),
    )));
    assert_eq!(paused.requirements.len(), 1);
    let tool_requirement_id = paused.requirements[0].id;

    let responded = machine.step(StepInput::resume(tool_resolution(
        tool_requirement_id,
        provider_call_id,
        "patch applied",
    )));
    assert_eq!(responded.requirements.len(), 1);
    responded.requirements[0].id
}

#[test]
fn external_loop_limit_fails_before_unbounded_pause_loop() {
    // Bound the machine to two runtime decision loops. The initial Start counts
    // as the first; each RespondToolResults round-trip counts as another.
    let mut machine =
        machine_with_config(ExternalAgentMachineConfig::default().with_max_decision_loops(Some(2)));

    // Loop 1: the opening Start round-trip.
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;
    assert_eq!(machine.state().decision_loops(), 1);

    // Loop 2: the first pause/respond cycle reparks on a fresh session round-trip.
    let second_session_id =
        drive_one_tool_round(&mut machine, session_requirement_id, "batch-1", "call-a");
    assert_eq!(machine.state().decision_loops(), 2);
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);

    // The runtime pauses again and the host answers again. Relaying this batch
    // would open a third decision loop past the cap, so the machine fails with a
    // classified LimitExceeded instead of minting another NeedExternalSession —
    // stopping an otherwise unbounded pause loop.
    let paused = machine.step(StepInput::resume(external_resolution(
        second_session_id,
        paused_for_tools("batch-2", vec![external_tool_call("call-b", "apply_patch")]),
    )));
    let tool_requirement_id = paused.requirements[0].id;
    let over_limit = machine.step(StepInput::resume(tool_resolution(
        tool_requirement_id,
        "call-b",
        "patch applied",
    )));

    assert!(over_limit.is_quiescent());
    assert!(
        over_limit.requirements.is_empty(),
        "an over-limit loop must not mint another session requirement"
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    assert_eq!(machine.state().decision_loops(), 3);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("max external decision loops")
                    && message.contains("limit exceeded"),
                "unexpected error text: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    // The dangling turn is discarded so no half-open turn lingers.
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn external_session_policy_max_turns_bounds_runtime_round_trips() {
    // M2-7: the spec's policy max_turns is enforced by the machine itself,
    // uniformly across runtimes — one decision loop is one runtime round-trip.
    // With a cap of two, the third round-trip fails with a classified
    // LimitExceeded instead of minting another NeedExternalSession.
    let mut machine = ExternalAgentMachine::new(
        ExternalAgentState::new(spec_with_max_turns(Some(2)), empty_conversation()),
        Arc::new(SeqRequirementIds::default()),
    )
    .with_tool_execution_ids(Arc::new(SeqToolIds::default()));

    // Round-trip 1: the opening Start.
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;
    assert_eq!(machine.state().decision_loops(), 1);

    // Round-trip 2: the first pause/respond cycle.
    let second_session_id =
        drive_one_tool_round(&mut machine, session_requirement_id, "batch-1", "call-a");
    assert_eq!(machine.state().decision_loops(), 2);

    // A second pause/respond cycle would open a third round-trip past the
    // policy cap, so the machine fails instead.
    let paused = machine.step(StepInput::resume(external_resolution(
        second_session_id,
        paused_for_tools("batch-2", vec![external_tool_call("call-b", "apply_patch")]),
    )));
    let tool_requirement_id = paused.requirements[0].id;
    let over_limit = machine.step(StepInput::resume(tool_resolution(
        tool_requirement_id,
        "call-b",
        "patch applied",
    )));

    assert!(over_limit.is_quiescent());
    assert!(
        over_limit.requirements.is_empty(),
        "an over-limit turn must not mint another session requirement"
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("max_turns") && message.contains("limit exceeded"),
                "unexpected error text: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn external_default_config_leaves_decision_loop_unbounded() {
    // Without a configured bound the counter still advances but never fails, so
    // the default machine keeps its pre-M4-3 behavior.
    let mut machine = machine_with_config(ExternalAgentMachineConfig::default());
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;
    assert_eq!(machine.state().decision_loops(), 1);

    let next = drive_one_tool_round(&mut machine, session_requirement_id, "batch-1", "call-a");
    assert_eq!(machine.state().decision_loops(), 2);
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);

    // The reparked session completes normally: no limit ever trips.
    let completed = machine.step(StepInput::resume(external_resolution(
        next,
        completed_result(),
    )));
    assert!(completed.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
}

#[test]
fn external_tool_failure_stop_run_fails_turn() {
    // Under the stop-run policy a failed host tool call stops the turn instead of
    // relaying a failed result to the runtime.
    let mut machine = machine_with_config(
        ExternalAgentMachineConfig::default()
            .with_tool_failure_policy(ExternalToolFailurePolicy::StopRun),
    );
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", vec![external_tool_call("call-a", "apply_patch")]),
    )));
    let tool_requirement_id = paused.requirements[0].id;

    let outcome = machine.step(StepInput::resume(tool_error_resolution(
        tool_requirement_id,
        ToolRuntimeError::ExecutionFailed {
            tool_name: "apply_patch".to_owned(),
            message: "boom".to_owned(),
        },
    )));

    assert!(outcome.is_quiescent());
    assert!(
        outcome.requirements.is_empty(),
        "stop-run must not relay a RespondToolResults"
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("stop-run policy"),
                "unexpected error text: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    // The pending turn is discarded so no half-open turn lingers.
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn external_tool_failure_default_returns_error_to_runtime() {
    // The default policy keeps relaying failed tool results to the runtime even
    // when a required-capabilities set is configured.
    let mut machine =
        machine_with_config(ExternalAgentMachineConfig::default().require_host_tools());
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", vec![external_tool_call("call-a", "apply_patch")]),
    )));
    let tool_requirement_id = paused.requirements[0].id;

    let done = machine.step(StepInput::resume(tool_error_resolution(
        tool_requirement_id,
        ToolRuntimeError::ExecutionFailed {
            tool_name: "apply_patch".to_owned(),
            message: "boom".to_owned(),
        },
    )));

    assert_responds_with_batch(&done, &["call-a"]);
    match &done.requirements[0].kind {
        RequirementKind::NeedExternalSession { request } => match &request.input {
            ExternalSessionInput::RespondToolResults { results, .. } => {
                assert_eq!(results[0].status, ToolStatus::Error);
            }
            other => panic!("expected a RespondToolResults input, got {other:?}"),
        },
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }
}

#[test]
fn external_require_host_tools_reports_unsupported_capability() {
    // A run that requires host tools but has no tool-call id source (default
    // NoToolExecutionIds) fails a tool pause with a classified
    // UnsupportedCapability instead of the generic id-unavailable error.
    let mut machine = ExternalAgentMachine::new(
        ExternalAgentState::new(spec(), empty_conversation()),
        Arc::new(SeqRequirementIds::default()),
    )
    .with_external_config(ExternalAgentMachineConfig::default().require_host_tools());

    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let outcome = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", vec![external_tool_call("call-a", "apply_patch")]),
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("does not support host_tools"),
                "expected a classified capability error, got: {message}"
            );
            assert!(
                !message.contains("tool id unavailable"),
                "a required capability must not fall back to the generic error: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn external_require_subagents_reports_unsupported_capability() {
    // A run that requires host subagents but has no tool-call id source fails a
    // spawn_agent bridge with a classified UnsupportedCapability naming the
    // subagent capability.
    let mut machine = ExternalAgentMachine::new(
        ExternalAgentState::new(spec(), empty_conversation()),
        Arc::new(SeqRequirementIds::default()),
    )
    .with_external_config(ExternalAgentMachineConfig::default().require_subagents());

    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let outcome = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools(
            "batch-7",
            vec![spawn_agent_call("call-spawn", "investigate the flake")],
        ),
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("does not support host_subagents"),
                "expected a classified capability error, got: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn external_require_host_tools_without_source_keeps_generic_error_when_unset() {
    // The capability requirement is what upgrades the diagnostic: without it, a
    // missing id source still fails with the generic id-unavailable message, so
    // the require flag is exercised and not a no-op. `ExternalCapability` is used
    // here to assert the configured requirement set.
    let config = ExternalAgentMachineConfig::default();
    assert!(!config.requires(ExternalCapability::HostTools));
    let mut machine = ExternalAgentMachine::new(
        ExternalAgentState::new(spec(), empty_conversation()),
        Arc::new(SeqRequirementIds::default()),
    )
    .with_external_config(config);

    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;
    let outcome = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", vec![external_tool_call("call-a", "apply_patch")]),
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("tool id unavailable"),
                "unset requirement must keep the generic error: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
}
