use super::*;

#[test]
fn framework_ids_are_unique_for_new_mappings_and_exact_for_existing_mappings() {
    let mut unmapped = conversation();
    begin(&mut unmapped, 80, 800);
    freeze_response(
        &mut unmapped,
        assistant_response(
            vec![tool_use("new-a"), tool_use("new-b")],
            1,
            1,
            StopReason::ToolUse,
            "req-unmapped",
        ),
        801,
    );
    let pending_before = pending_view(&unmapped);
    assert_eq!(
        unmapped
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![
                    cancelled_result("new-a", 950, 802),
                    cancelled_result("new-b", 950, 803),
                ],
            })
            .expect_err("unmapped calls need distinct framework ids"),
        ConversationError::Cancel(CancelError::DuplicateToolCallId {
            call_id: call_id(950),
        })
    );
    assert_eq!(pending_view(&unmapped), pending_before);

    let mut mapped = registered_parallel_turn();
    let mapped_before = pending_view(&mapped);
    assert_eq!(
        mapped
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![
                    cancelled_result("parallel-a", 999, 702),
                    cancelled_result("parallel-b", 901, 703),
                ],
            })
            .expect_err("registered mapping cannot be replaced"),
        ConversationError::Cancel(CancelError::ToolCallIdMismatch {
            provider_call_id: "parallel-a".to_owned(),
            expected: call_id(900),
            actual: call_id(999),
        })
    );
    assert_eq!(pending_view(&mapped), mapped_before);
}

#[test]
fn synthetic_call_and_message_ids_cannot_reuse_committed_history() {
    let mut conversation = conversation();
    begin(&mut conversation, 85, 850);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("committed-call")],
            1,
            1,
            StopReason::ToolUse,
            "req-committed-call",
        ),
        851,
    );
    conversation
        .register_tool_calls(vec![mapping("committed-call", 980)])
        .expect("map committed call");
    conversation
        .append_tool_response(
            message_id(852),
            tool_response("committed-call", "committed result"),
        )
        .expect("append committed result");
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![text("committed final")],
            1,
            1,
            StopReason::EndTurn,
            "req-committed-final",
        ),
        853,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("seed committed identities");

    begin(&mut conversation, 86, 860);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("new-call")],
            1,
            1,
            StopReason::ToolUse,
            "req-new-call",
        ),
        861,
    );
    let pending_before = pending_view(&conversation);
    let committed_before = committed_view(&conversation);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![cancelled_result("new-call", 980, 862)],
            })
            .expect_err("committed framework id cannot be reused"),
        ConversationError::Cancel(CancelError::DuplicateToolCallId {
            call_id: call_id(980),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![cancelled_result("new-call", 981, 852)],
            })
            .expect_err("committed message id cannot be reused"),
        ConversationError::Cancel(CancelError::DuplicateMessageId {
            message_id: message_id(852),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);

    conversation
        .cancel_pending(CancelDisposition::ResumeTurn {
            cancelled_results: vec![cancelled_result("new-call", 981, 862)],
        })
        .expect("corrected conversation-wide identities succeed");
}

#[test]
fn final_message_cannot_reuse_a_synthetic_result_identity() {
    let mut conversation = registered_parallel_turn();
    let pending_before = pending_view(&conversation);
    let committed_before = committed_view(&conversation);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::commit_turn(
                vec![
                    cancelled_result("parallel-a", 900, 702),
                    cancelled_result("parallel-b", 901, 703),
                ],
                message_id(702),
                assistant_response(
                    vec![text("duplicate id")],
                    1,
                    1,
                    StopReason::EndTurn,
                    "req-duplicate-id",
                ),
                TurnMeta::default(),
            ))
            .expect_err("final assistant cannot share a result id"),
        ConversationError::Cancel(CancelError::DuplicateMessageId {
            message_id: message_id(702),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);
}
