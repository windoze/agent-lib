use super::*;
use crate::{
    client::Response,
    conversation::{CancelDisposition, Conversation},
};

/// Builds an assistant tool-use block with caller-controlled id and name.
fn raw_tool_use(provider_call_id: &str, name: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: provider_call_id.to_owned(),
        name: name.to_owned(),
        input: serde_json::Value::Object(Map::new()),
        extra: Map::new(),
    }
}

/// Starts a complete response, asserts its freeze is rejected, and returns the
/// error. The rejection must leave the observable pending state untouched.
fn freeze_rejected(
    conversation: &mut Conversation,
    response: Response,
    message_seed: u128,
) -> ConversationError {
    conversation
        .start_assistant_response(response)
        .expect("start complete response");
    let before = pending_view(conversation);
    let error = conversation
        .finish_assistant(message_id(message_seed))
        .expect_err("illegal assistant content must fail at the freeze boundary");
    assert_eq!(pending_view(conversation), before);
    error
}

/// A rejected freeze leaves the turn discardable and the conversation feedable.
fn assert_discard_and_continue(
    conversation: &mut Conversation,
    turn_seed: u128,
    user_seed: u128,
    message_seed: u128,
) {
    conversation
        .cancel_pending(CancelDisposition::DiscardTurn)
        .expect("a turn with a rejected freeze can be discarded");
    assert!(conversation.pending().is_none());

    begin(conversation, turn_seed, user_seed);
    freeze_response(
        conversation,
        assistant_response(
            vec![text("recovered")],
            1,
            1,
            StopReason::EndTurn,
            "req-recovered",
        ),
        message_seed,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("the next turn commits after discarding the rejected freeze");
}

#[test]
fn duplicate_provider_call_ids_are_rejected_at_the_freeze_boundary() {
    let mut conversation = conversation();
    begin(&mut conversation, 30, 300);

    let error = freeze_rejected(
        &mut conversation,
        assistant_response(
            vec![tool_use("same-call"), tool_use("same-call")],
            1,
            1,
            StopReason::ToolUse,
            "req-duplicate-provider",
        ),
        301,
    );

    assert_eq!(
        error,
        ConversationError::PendingTurn(PendingTurnError::DuplicateProviderCallId {
            provider_call_id: "same-call".to_owned(),
        })
    );
    assert_discard_and_continue(&mut conversation, 36, 360, 361);
}

#[test]
fn illegal_assistant_blocks_are_rejected_at_the_freeze_boundary() {
    for (block, kind) in [
        (
            ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.test/not-assistant.png".to_owned(),
                    extra: Map::new(),
                },
                extra: Map::new(),
            },
            ContentBlockKind::Image,
        ),
        (
            ContentBlock::ToolResult {
                tool_use_id: "orphan".to_owned(),
                content: vec![text("late result")],
                status: ToolStatus::Ok,
                extra: Map::new(),
            },
            ContentBlockKind::ToolResult,
        ),
    ] {
        let mut conversation = conversation();
        begin(&mut conversation, 31, 310);

        let error = freeze_rejected(
            &mut conversation,
            assistant_response(vec![block], 1, 1, StopReason::EndTurn, "req-illegal-block"),
            311,
        );

        assert_eq!(
            error,
            ConversationError::PendingTurn(PendingTurnError::InvalidAssistantBlock { block: kind })
        );
        assert_discard_and_continue(&mut conversation, 32, 320, 321);
    }
}

#[test]
fn incomplete_tool_uses_are_rejected_at_the_freeze_boundary() {
    for (block, detail) in [
        (
            raw_tool_use("", "lookup"),
            "a tool-use block has no provider call id",
        ),
        (
            raw_tool_use("call-a", ""),
            "a tool-use block has no tool name",
        ),
    ] {
        let mut conversation = conversation();
        begin(&mut conversation, 33, 330);

        let error = freeze_rejected(
            &mut conversation,
            assistant_response(vec![block], 1, 1, StopReason::ToolUse, "req-incomplete"),
            331,
        );

        assert_eq!(
            error,
            ConversationError::PendingTurn(PendingTurnError::IncompleteToolUse { detail })
        );
        assert_discard_and_continue(&mut conversation, 34, 340, 341);
    }
}

#[test]
fn a_provider_call_id_registered_by_an_earlier_step_cannot_be_reused() {
    let mut conversation = conversation();
    begin(&mut conversation, 35, 350);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("call-a")],
            1,
            1,
            StopReason::ToolUse,
            "req-first",
        ),
        351,
    );
    conversation
        .register_tool_calls(vec![mapping("call-a", 900)])
        .expect("map the first call");
    conversation
        .append_tool_response(message_id(352), tool_response("call-a", "first result"))
        .expect("close the first call");

    let error = freeze_rejected(
        &mut conversation,
        assistant_response(
            vec![tool_use("call-a")],
            1,
            1,
            StopReason::ToolUse,
            "req-reuse",
        ),
        353,
    );

    assert_eq!(
        error,
        ConversationError::PendingTurn(PendingTurnError::DuplicateProviderCallId {
            provider_call_id: "call-a".to_owned(),
        })
    );
    // The closed first step survives the rejection of the second response.
    assert_eq!(
        conversation
            .pending()
            .expect("pending turn")
            .messages()
            .len(),
        3
    );
    assert_discard_and_continue(&mut conversation, 37, 370, 371);
}
