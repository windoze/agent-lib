use super::*;

#[test]
fn identity_conflicts_fail_before_mutation_and_can_be_retried() {
    let mut conversation = conversation();
    begin(&mut conversation, 40, 400);
    freeze_response(
        &mut conversation,
        assistant_response(vec![text("first")], 1, 1, StopReason::EndTurn, "req-first"),
        401,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("seed history");
    let committed_before = committed_view(&conversation);

    assert_eq!(
        conversation
            .begin_turn(turn_id(40), message_id(500), user("duplicate turn"))
            .expect_err("turn id is conversation-wide"),
        ConversationError::PendingTurn(PendingTurnError::DuplicateTurnId {
            turn_id: turn_id(40),
        })
    );
    assert_eq!(
        conversation
            .begin_turn(turn_id(41), message_id(400), user("duplicate message"))
            .expect_err("message id is conversation-wide"),
        ConversationError::PendingTurn(PendingTurnError::DuplicateMessageId {
            message_id: message_id(400),
        })
    );
    assert_eq!(committed_view(&conversation), committed_before);
    assert!(conversation.pending().is_none());

    begin(&mut conversation, 41, 500);
    conversation
        .start_assistant_response(assistant_response(
            vec![text("second")],
            1,
            1,
            StopReason::EndTurn,
            "req-second",
        ))
        .expect("start response");
    let active_before = pending_view(&conversation);
    let duplicate = conversation
        .finish_assistant(message_id(500))
        .expect_err("assistant cannot reuse current user id");
    assert_eq!(
        duplicate,
        ConversationError::PendingTurn(PendingTurnError::DuplicateMessageId {
            message_id: message_id(500),
        })
    );
    assert_eq!(pending_view(&conversation), active_before);
    conversation
        .finish_assistant(message_id(501))
        .expect("same complete response can freeze under a corrected id");
    conversation
        .commit_pending(TurnMeta::default())
        .expect("retry commits");
    assert_eq!(conversation.turns()[1].parent(), Some(turn_id(40)));
}

#[test]
fn framework_call_and_result_message_ids_are_unique_across_history() {
    let mut conversation = conversation();
    begin(&mut conversation, 50, 500);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("old-call")],
            1,
            1,
            StopReason::ToolUse,
            "req-old-call",
        ),
        501,
    );
    conversation
        .register_tool_calls(vec![mapping("old-call", 700)])
        .expect("map old call");
    conversation
        .append_tool_response(message_id(502), tool_response("old-call", "old result"))
        .expect("old result");
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![text("done")],
            1,
            1,
            StopReason::EndTurn,
            "req-old-final",
        ),
        503,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("seed tool history");

    begin(&mut conversation, 51, 510);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("new-call")],
            1,
            1,
            StopReason::ToolUse,
            "req-new-call",
        ),
        511,
    );
    let before_mapping = pending_view(&conversation);
    assert_eq!(
        conversation
            .register_tool_calls(vec![ToolCallMapping::new("new-call", call_id(700))])
            .expect_err("framework id is conversation-wide"),
        ConversationError::PendingTurn(PendingTurnError::DuplicateToolCallId {
            call_id: call_id(700),
        })
    );
    assert_eq!(pending_view(&conversation), before_mapping);
    conversation
        .register_tool_calls(vec![mapping("new-call", 701)])
        .expect("corrected call id");

    let before_result = pending_view(&conversation);
    assert_eq!(
        conversation
            .append_tool_response(message_id(502), tool_response("new-call", "new result"))
            .expect_err("result message id is conversation-wide"),
        ConversationError::PendingTurn(PendingTurnError::DuplicateMessageId {
            message_id: message_id(502),
        })
    );
    assert_eq!(pending_view(&conversation), before_result);
}
