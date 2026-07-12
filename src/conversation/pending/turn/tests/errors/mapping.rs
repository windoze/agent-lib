use super::*;

#[test]
fn tool_call_mapping_errors_are_atomic_and_retryable() {
    let mut conversation = conversation();
    begin(&mut conversation, 20, 200);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("call-a"), tool_use("call-b")],
            5,
            2,
            StopReason::ToolUse,
            "req-map",
        ),
        201,
    );
    let before = pending_view(&conversation);

    for (mappings, expected) in [
        (
            vec![mapping("call-a", 500)],
            PendingTurnError::MissingToolCallMapping {
                provider_call_id: "call-b".to_owned(),
            },
        ),
        (
            vec![
                mapping("call-a", 500),
                mapping("call-b", 501),
                mapping("unknown", 502),
            ],
            PendingTurnError::UnknownToolCallMapping {
                provider_call_id: "unknown".to_owned(),
            },
        ),
        (
            vec![mapping("call-a", 500), mapping("call-a", 501)],
            PendingTurnError::DuplicateToolCallMapping {
                provider_call_id: "call-a".to_owned(),
            },
        ),
        (
            vec![mapping("call-a", 500), mapping("call-b", 500)],
            PendingTurnError::DuplicateToolCallId {
                call_id: call_id(500),
            },
        ),
    ] {
        let error = conversation
            .register_tool_calls(mappings)
            .expect_err("bad mapping must fail");
        assert_eq!(error, ConversationError::PendingTurn(expected));
        assert_eq!(pending_view(&conversation), before);
        assert!(conversation.turns().is_empty());
    }

    conversation
        .register_tool_calls(vec![mapping("call-b", 501), mapping("call-a", 500)])
        .expect("corrected mappings succeed");
    assert_eq!(
        conversation.pending().expect("pending results").phase(),
        PendingTurnPhase::AwaitingToolResults
    );
}

#[test]
fn duplicate_provider_calls_are_rejected_before_open_call_registration() {
    let mut conversation = conversation();
    begin(&mut conversation, 21, 210);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("same-call"), tool_use("same-call")],
            1,
            1,
            StopReason::ToolUse,
            "req-duplicate-provider",
        ),
        211,
    );
    let before = pending_view(&conversation);

    let error = conversation
        .register_tool_calls(vec![mapping("same-call", 510)])
        .expect_err("duplicate provider ids cannot be mapped safely");

    assert_eq!(
        error,
        ConversationError::PendingTurn(PendingTurnError::DuplicateProviderCallId {
            provider_call_id: "same-call".to_owned(),
        })
    );
    assert_eq!(pending_view(&conversation), before);
}
