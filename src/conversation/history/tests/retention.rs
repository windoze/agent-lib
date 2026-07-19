//! Raw-retention, active-lineage, and hidden-identity tests.

use super::{
    assert_index_matches_rebuild, begin, call_id, commit_text_turn, conversation, freeze,
    message_id, register_batch, response, text, tool_response, tool_use, turn_id, user,
};
use crate::{
    conversation::{
        AssistantFinish, CancelDisposition, ConversationError, PendingTurnError, ToolCallMapping,
        TurnMeta,
    },
    model::normalized::StopReason,
};

#[test]
fn replacement_lineage_hides_old_suffix_but_retains_raw_and_all_identities() {
    let mut conversation = conversation();
    commit_text_turn(&mut conversation, 10, 1_000);

    begin(&mut conversation, 11, 1_100);
    register_batch(&mut conversation, &[("hidden-call", 5_000)], 1_101);
    conversation
        .append_tool_response(message_id(1_102), tool_response("hidden-call"))
        .expect("close hidden-suffix call");
    assert_eq!(
        freeze(
            &mut conversation,
            response(vec![text("hidden final")], StopReason::EndTurn),
            1_103,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit old suffix");
    let hidden_payload = conversation.turns()[1].messages()[0].payload().clone();
    assert!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(5_000))
            .is_some()
    );

    let first_turn = conversation
        .boundary_after(turn_id(10))
        .expect("first turn has a checked boundary");
    conversation
        .revert_to(first_turn)
        .expect("checked head movement rescopes the index");
    assert_eq!(conversation.turns().len(), 1);
    assert!(conversation.raw_turn(turn_id(11)).is_some());
    assert!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(5_000))
            .is_none()
    );

    let duplicate_turn = conversation
        .begin_turn(turn_id(11), message_id(1_200), user("duplicate turn"))
        .expect_err("hidden raw turn id cannot be reused");
    assert_eq!(
        duplicate_turn,
        ConversationError::PendingTurn(PendingTurnError::DuplicateTurnId {
            turn_id: turn_id(11),
        })
    );
    let duplicate_message = conversation
        .begin_turn(turn_id(12), message_id(1_100), user("duplicate message"))
        .expect_err("hidden raw message id cannot be reused");
    assert_eq!(
        duplicate_message,
        ConversationError::PendingTurn(PendingTurnError::DuplicateMessageId {
            message_id: message_id(1_100),
        })
    );

    commit_text_turn(&mut conversation, 12, 1_200);
    assert_eq!(conversation.turns().len(), 2);
    assert_eq!(conversation.turns()[1].parent(), Some(turn_id(10)));
    let retained = conversation
        .raw_turn(turn_id(11))
        .expect("old suffix remains in raw storage");
    assert_eq!(retained.messages()[0].payload(), &hidden_payload);

    begin(&mut conversation, 13, 1_300);
    assert_eq!(
        freeze(
            &mut conversation,
            response(vec![tool_use("new-provider-call")], StopReason::ToolUse),
            1_301,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    let duplicate_call = conversation
        .register_tool_calls(vec![ToolCallMapping::new(
            "new-provider-call",
            call_id(5_000),
        )])
        .expect_err("hidden raw framework call id cannot be reused");
    assert_eq!(
        duplicate_call,
        ConversationError::PendingTurn(PendingTurnError::DuplicateToolCallId {
            call_id: call_id(5_000),
        })
    );
    assert_index_matches_rebuild(&conversation);

    conversation
        .cancel_pending(CancelDisposition::DiscardTurn)
        .expect("discard rejected replacement transaction");
    assert_index_matches_rebuild(&conversation);
    assert!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(5_000))
            .is_none()
    );
}

#[test]
fn retained_message_id_index_covers_long_histories() {
    let mut conversation = conversation();
    for index in 0..512 {
        commit_text_turn(&mut conversation, 1_000 + index, 10_000 + index * 10);
    }

    let duplicate = conversation
        .begin_turn(
            turn_id(9_000),
            message_id(10_000 + 255 * 10),
            user("duplicate message"),
        )
        .expect_err("message id retained in index");
    assert_eq!(
        duplicate,
        ConversationError::PendingTurn(PendingTurnError::DuplicateMessageId {
            message_id: message_id(10_000 + 255 * 10),
        })
    );
    assert_eq!(conversation.raw_turns().len(), 512);
}
