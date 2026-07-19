use super::*;

#[test]
fn result_errors_and_open_call_gates_leave_pending_repairable() {
    let mut conversation = conversation();
    begin(&mut conversation, 30, 300);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("call-a"), tool_use("call-b")],
            4,
            2,
            StopReason::ToolUse,
            "req-tools",
        ),
        301,
    );
    conversation
        .register_tool_calls(vec![mapping("call-a", 600), mapping("call-b", 601)])
        .expect("register calls");
    let before = pending_view(&conversation);

    let unknown = conversation
        .append_tool_response(message_id(302), tool_response("unknown", "result"))
        .expect_err("unknown result must fail");
    assert_eq!(
        unknown,
        ConversationError::PendingTurn(PendingTurnError::UnknownToolResult {
            provider_call_id: "unknown".to_owned(),
        })
    );
    assert_eq!(pending_view(&conversation), before);

    let wrong_block = conversation
        .append_tool_result(message_id(302), text("not a result"))
        .expect_err("non-result block must fail");
    assert_eq!(
        wrong_block,
        ConversationError::PendingTurn(PendingTurnError::InvalidToolResultBlock {
            actual: ContentBlockKind::Text,
        })
    );
    assert_eq!(pending_view(&conversation), before);

    let invalid_nested = ContentBlock::ToolResult {
        tool_use_id: "call-a".to_owned(),
        content: vec![tool_use("nested")],
        status: ToolStatus::Ok,
        extra: Map::new(),
    };
    let invalid = conversation
        .append_tool_result(message_id(302), invalid_nested)
        .expect_err("nested tool use must fail");
    assert_eq!(
        invalid,
        ConversationError::PendingTurn(PendingTurnError::InvalidToolResultContent {
            provider_call_id: "call-a".to_owned(),
            block: ContentBlockKind::ToolUse,
        })
    );
    assert_eq!(pending_view(&conversation), before);

    conversation
        .append_tool_response(message_id(302), tool_response("call-a", "result a"))
        .expect("first result succeeds");
    let after_first = pending_view(&conversation);
    let duplicate = conversation
        .append_tool_response(message_id(303), tool_response("call-a", "again"))
        .expect_err("a call closes once");
    assert_eq!(
        duplicate,
        ConversationError::PendingTurn(PendingTurnError::DuplicateToolResult {
            provider_call_id: "call-a".to_owned(),
        })
    );
    assert_eq!(pending_view(&conversation), after_first);

    let committed_before = committed_view(&conversation);
    let start_error = conversation
        .start_assistant()
        .expect_err("assistant cannot start with one open call");
    assert!(matches!(
        start_error,
        ConversationError::PendingTurn(PendingTurnError::InvalidTransition {
            actual: PendingTurnPhase::AwaitingToolResults,
            ..
        })
    ));
    let commit_error = conversation
        .commit_pending(TurnMeta::default())
        .expect_err("open calls cannot commit");
    assert!(matches!(
        commit_error,
        ConversationError::PendingTurn(PendingTurnError::InvalidTransition {
            actual: PendingTurnPhase::AwaitingToolResults,
            ..
        })
    ));
    assert_eq!(committed_view(&conversation), committed_before);
    assert_eq!(pending_view(&conversation), after_first);

    conversation
        .append_tool_response(message_id(303), tool_response("call-b", "result b"))
        .expect("remaining result succeeds");
    let duplicate_after_batch = conversation
        .append_tool_response(message_id(304), tool_response("call-b", "again"))
        .expect_err("duplicate remains classified after the step closes");
    assert_eq!(
        duplicate_after_batch,
        ConversationError::PendingTurn(PendingTurnError::DuplicateToolResult {
            provider_call_id: "call-b".to_owned(),
        })
    );

    freeze_response(
        &mut conversation,
        assistant_response(vec![text("final")], 2, 1, StopReason::EndTurn, "req-final"),
        304,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("errors did not corrupt the turn");
    assert_eq!(conversation.turns().len(), 1);
}
