//! spawn_agent tool bridge (M3-3): NeedSubagent bridging from tool
//! batches, mixed batches, and malformed-call error results.

use super::*;

// ----- spawn_agent tool bridge (M3-3) --------------------------------------

/// Drives a machine to a single-`spawn_agent` tool pause under `batch_id` and
/// returns it alongside the emitted `NeedSubagent` requirement id, so a resume
/// can target the child result.
fn pause_on_spawn_agent(
    batch_id: &str,
    provider_call_id: &str,
    brief: &str,
) -> (ExternalAgentMachine, RequirementId) {
    let mut machine = machine_with_tool_ids();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools(batch_id, vec![spawn_agent_call(provider_call_id, brief)]),
    )));
    assert_eq!(paused.requirements.len(), 1);
    let requirement_id = paused.requirements[0].id;
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    (machine, requirement_id)
}

#[test]
fn external_spawn_agent_tool_call_emits_need_subagent() {
    let mut machine = machine_with_tool_ids();
    let input = user_input("refactor the parser");
    // `user_input` (seq 0) mints this step id deterministically; the bridged
    // NeedSubagent brief is addressed to the step the spawn_agent call rode on.
    let step_id: StepId = "018f0d9c-7b6a-7c12-8f31-b30000000000"
        .parse()
        .expect("step id");
    let opened = machine.step(StepInput::external(input));
    let session_requirement_id = opened.requirements[0].id;

    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools(
            "batch-9",
            vec![spawn_agent_call("call-spawn", "investigate the flake")],
        ),
    )));

    assert!(paused.is_quiescent());
    assert!(paused.notifications.is_empty());

    // A spawn_agent call is a scope-deepening operation: it bridges into a
    // standard NeedSubagent (not a NeedTool), reusing the parsed spec, brief, and
    // result schema, so the host's own subagent machinery drives the child.
    assert_eq!(paused.requirements.len(), 1);
    let requirement = &paused.requirements[0];
    assert_eq!(requirement.tag(), RequirementKindTag::Subagent);
    match &requirement.kind {
        RequirementKind::NeedSubagent {
            spec_ref,
            brief,
            result_schema,
        } => {
            assert_eq!(spec_ref, &AgentSpecRef(agent_id()));
            assert_eq!(
                brief,
                &Interaction::question(step_id, "investigate the flake".to_owned())
            );
            assert_eq!(
                result_schema,
                &Some(serde_json::json!({ "type": "object" }))
            );
        }
        other => panic!("expected a NeedSubagent requirement, got {other:?}"),
    }

    // Even though the requirement is a subagent, the whole batch parks on one
    // AwaitingTool cursor so a mixed batch recovers uniformly; the batch id and a
    // single outstanding requirement are recorded.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![requirement.id]
    );
    match machine.state().cursor() {
        ExternalAgentCursor::AwaitingTool {
            batch_id,
            requirements,
        } => {
            assert_eq!(batch_id.as_str(), "batch-9");
            assert_eq!(requirements.ids().len(), 1);
        }
        other => panic!("expected an AwaitingTool cursor, got {other:?}"),
    }

    // The turn stays open across the pause; the child summary later folds back
    // into this batch, never into host history.
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_spawn_agent_result_bridges_summary_into_respond_tool_results() {
    let (mut machine, requirement_id) =
        pause_on_spawn_agent("batch-7", "call-spawn", "investigate the flake");

    // The driven child's summary folds back into the SAME batch as a successful
    // tool result, so the runtime sees the spawn as a tool call that returned a
    // summary (design §8.3).
    let done = machine.step(StepInput::resume(subagent_resolution(
        requirement_id,
        "found the deadlock in the pool",
    )));

    assert!(done.is_quiescent());
    assert_responds_with_batch(&done, &["call-spawn"]);
    match &done.requirements[0].kind {
        RequirementKind::NeedExternalSession { request } => match &request.input {
            ExternalSessionInput::RespondToolResults { results, .. } => {
                assert_eq!(results[0].status, ToolStatus::Ok);
                assert!(results[0].error.is_none());
                assert_eq!(
                    results[0].content,
                    vec![ContentBlock::Text {
                        text: "found the deadlock in the pool".to_owned(),
                        extra: Map::new(),
                    }]
                );
            }
            other => panic!("expected a RespondToolResults input, got {other:?}"),
        },
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }

    // The batch completion reparks on an outstanding external session, keeping the
    // turn open, and never writes the child result into host history.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_mixed_tool_and_spawn_agent_batch_returns_one_respond_tool_results() {
    let mut machine = machine_with_tool_ids();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    // A mixed batch: an ordinary tool call followed by a spawn_agent.
    let calls = vec![
        external_tool_call("call-a", "apply_patch"),
        spawn_agent_call("call-spawn", "investigate the flake"),
    ];
    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", calls),
    )));

    // Two requirements in call order: a NeedTool then a NeedSubagent, all parked
    // under one AwaitingTool cursor.
    assert_eq!(paused.requirements.len(), 2);
    assert_eq!(paused.requirements[0].tag(), RequirementKindTag::Tool);
    assert_eq!(paused.requirements[1].tag(), RequirementKindTag::Subagent);
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    let tool_requirement_id = paused.requirements[0].id;
    let subagent_requirement_id = paused.requirements[1].id;

    // The ordinary tool result is collected but the batch is not yet complete.
    let first = machine.step(StepInput::resume(tool_resolution(
        tool_requirement_id,
        "call-a",
        "patch applied",
    )));
    assert!(first.is_quiescent());
    assert!(first.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);

    // The subagent result completes the batch: one RespondToolResults carries both
    // results in the runtime's original call order (never completion order).
    let done = machine.step(StepInput::resume(subagent_resolution(
        subagent_requirement_id,
        "found the deadlock in the pool",
    )));
    assert_responds_with_batch(&done, &["call-a", "call-spawn"]);
    match &done.requirements[0].kind {
        RequirementKind::NeedExternalSession { request } => match &request.input {
            ExternalSessionInput::RespondToolResults { results, .. } => {
                assert_eq!(results[0].status, ToolStatus::Ok);
                assert_eq!(
                    results[0].content,
                    tool_response("call-a", "patch applied").content
                );
                assert_eq!(results[1].status, ToolStatus::Ok);
                assert_eq!(
                    results[1].content,
                    vec![ContentBlock::Text {
                        text: "found the deadlock in the pool".to_owned(),
                        extra: Map::new(),
                    }]
                );
            }
            other => panic!("expected a RespondToolResults input, got {other:?}"),
        },
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }

    // The completed mixed batch reparks on an outstanding external session.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_mixed_valid_tool_and_malformed_spawn_agent_returns_one_batch() {
    let mut machine = machine_with_tool_ids();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    // A malformed spawn_agent mints no requirement (its error result is pre-seeded
    // into the batch); only the ordinary tool is outstanding.
    let calls = vec![
        external_tool_call("call-a", "apply_patch"),
        malformed_spawn_agent_call("call-bad"),
    ];
    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", calls),
    )));
    assert_eq!(paused.requirements.len(), 1);
    assert_eq!(paused.requirements[0].tag(), RequirementKindTag::Tool);
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    let tool_requirement_id = paused.requirements[0].id;

    // Answering the single real tool completes the batch; the malformed spawn is
    // returned to the runtime as a failed result, in the original call order.
    let done = machine.step(StepInput::resume(tool_resolution(
        tool_requirement_id,
        "call-a",
        "patch applied",
    )));
    assert_responds_with_batch(&done, &["call-a", "call-bad"]);
    match &done.requirements[0].kind {
        RequirementKind::NeedExternalSession { request } => match &request.input {
            ExternalSessionInput::RespondToolResults { results, .. } => {
                assert_eq!(results[0].status, ToolStatus::Ok);
                assert_eq!(results[1].status, ToolStatus::Error);
                assert!(
                    results[1]
                        .error
                        .as_deref()
                        .is_some_and(|error| error.contains("spawn_agent")),
                    "malformed spawn error should ride back on the result: {:?}",
                    results[1].error
                );
            }
            other => panic!("expected a RespondToolResults input, got {other:?}"),
        },
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }
}

#[test]
fn external_spawn_agent_invalid_input_returns_runtime_error_result() {
    let mut machine = machine_with_tool_ids();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    // A lone malformed spawn_agent mints no requirement, so the batch is already
    // complete: the machine skips the AwaitingTool park and relays the pre-seeded
    // runtime-visible error straight back (return-error-to-runtime, design §8.4).
    let done = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", vec![malformed_spawn_agent_call("call-bad")]),
    )));

    assert!(done.is_quiescent());
    assert_responds_with_batch(&done, &["call-bad"]);
    match &done.requirements[0].kind {
        RequirementKind::NeedExternalSession { request } => match &request.input {
            ExternalSessionInput::RespondToolResults { results, .. } => {
                assert_eq!(results[0].status, ToolStatus::Error);
                assert!(
                    results[0]
                        .error
                        .as_deref()
                        .is_some_and(|error| error.contains("spawn_agent")),
                    "malformed spawn error should ride back on the result: {:?}",
                    results[0].error
                );
            }
            other => panic!("expected a RespondToolResults input, got {other:?}"),
        },
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }

    // The turn survives the malformed call and reparks on an outstanding session,
    // never writing anything into host history.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_spawn_agent_subagent_failure_settles_error() {
    let (mut machine, requirement_id) =
        pause_on_spawn_agent("batch-7", "call-spawn", "investigate the flake");

    // A spawn_agent subagent-drive failure is a host-orchestration failure,
    // symmetric with the native subagent pause: it stops the turn on a classified
    // error cursor rather than fabricating a runtime result.
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
                message.contains("external spawn_agent subagent failed"),
                "unexpected error text: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn external_spawn_agent_bridge_wrong_family_fails() {
    let (mut machine, requirement_id) =
        pause_on_spawn_agent("batch-7", "call-spawn", "investigate the flake");

    // A spawn_agent bridge is a NeedSubagent: a tool result family is a protocol
    // violation and settles on a classified error cursor.
    let outcome = machine.step(StepInput::resume(tool_resolution(
        requirement_id,
        "call-spawn",
        "not a subagent output",
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    match machine.state().cursor() {
        ExternalAgentCursor::Error { message } => {
            assert!(
                message.contains("spawn_agent bridge (NeedSubagent) requirement cannot accept"),
                "unexpected error text: {message}"
            );
        }
        other => panic!("expected an Error cursor, got {other:?}"),
    }
    assert!(machine.state().conversation().pending().is_none());
}
