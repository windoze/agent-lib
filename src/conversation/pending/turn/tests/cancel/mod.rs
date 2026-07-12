use super::{
    assistant_response, begin, call_id, committed_view, conversation, freeze_response, mapping,
    message_id, pending_view, text, tool_response, tool_use, turn_id,
};
use crate::{
    conversation::{
        AssistantFinish, CANCELLED_TOOL_RESULT_TEXT, CancelDisposition, CancelError, CancelOutcome,
        CancelledToolResult, CommitError, ContentBlockKind, Conversation, ConversationError,
        PendingMessageError, PendingTurnPhase, TurnMeta,
    },
    model::{
        content::{ContentBlock, ImageSource},
        message::Role,
        normalized::StopReason,
        tool::ToolStatus,
    },
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use serde_json::Map;

mod errors;
mod success;

/// Builds caller-owned identities for one synthetic cancelled result.
fn cancelled_result(
    provider_call_id: &str,
    call_seed: u128,
    message_seed: u128,
) -> CancelledToolResult {
    CancelledToolResult::new(
        provider_call_id,
        call_id(call_seed),
        message_id(message_seed),
    )
}

/// Completes one ordinary text turn to prove post-cancel usability.
fn commit_text_turn(
    conversation: &mut Conversation,
    turn_seed: u128,
    user_seed: u128,
    assistant_seed: u128,
) {
    begin(conversation, turn_seed, user_seed);
    assert_eq!(
        freeze_response(
            conversation,
            assistant_response(
                vec![text("post-cancel answer")],
                1,
                1,
                StopReason::EndTurn,
                "req-post-cancel",
            ),
            assistant_seed,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("post-cancel text turn commits");
}

/// Checks the normalized cancellation status and explicit interruption text.
fn assert_cancelled_message(message: &crate::conversation::ConversationMessage, provider_id: &str) {
    assert_eq!(message.payload().role, Role::Tool);
    let [
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            status,
            ..
        },
    ] = message.payload().content.as_slice()
    else {
        panic!("expected exactly one synthetic tool result");
    };
    assert_eq!(tool_use_id, provider_id);
    assert_eq!(*status, ToolStatus::Cancelled);
    assert_eq!(
        content,
        &[ContentBlock::Text {
            text: CANCELLED_TOOL_RESULT_TEXT.to_owned(),
            extra: Map::new(),
        }]
    );
}
