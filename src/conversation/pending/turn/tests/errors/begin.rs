use super::*;

#[test]
fn begin_turn_rejects_invalid_input_and_a_second_transaction() {
    let mut conversation = conversation();
    let committed_before = committed_view(&conversation);

    let wrong_role = conversation
        .begin_turn(
            turn_id(10),
            message_id(100),
            Message {
                role: Role::Assistant,
                content: vec![text("not user")],
            },
        )
        .expect_err("assistant cannot begin a turn");
    assert_eq!(
        wrong_role,
        ConversationError::PendingTurn(PendingTurnError::InvalidUserRole {
            actual: Role::Assistant,
        })
    );
    assert_eq!(committed_view(&conversation), committed_before);
    assert!(conversation.pending().is_none());

    let wrong_block = conversation
        .begin_turn(
            turn_id(10),
            message_id(100),
            Message {
                role: Role::User,
                content: vec![tool_use("not-allowed")],
            },
        )
        .expect_err("tool use cannot appear in user input");
    assert_eq!(
        wrong_block,
        ConversationError::PendingTurn(PendingTurnError::InvalidUserBlock {
            block: ContentBlockKind::ToolUse,
        })
    );
    assert!(conversation.pending().is_none());

    begin(&mut conversation, 10, 100);
    let pending_before = pending_view(&conversation);
    let second = conversation
        .begin_turn(turn_id(11), message_id(200), user("second"))
        .expect_err("only one pending turn is legal");
    assert_eq!(
        second,
        ConversationError::PendingTurn(PendingTurnError::AlreadyPending {
            turn_id: turn_id(10),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);
}
