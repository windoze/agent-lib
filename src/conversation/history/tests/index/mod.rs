//! Derived-index lifecycle, lookup, rebuild, and cancellation tests.

use super::{begin, message_id, register_batch, response, text, tool_response};
use crate::{
    conversation::{AssistantFinish, Conversation, TurnMeta},
    model::normalized::StopReason,
};

mod cancellation;
mod lifecycle;
mod provider;

/// Commits one single-call turn while keeping all identities deterministic.
fn commit_one_call_turn(
    conversation: &mut Conversation,
    turn_seed: u128,
    message_seed: u128,
    provider_call_id: &str,
    call_seed: u128,
) {
    begin(conversation, turn_seed, message_seed);
    register_batch(
        conversation,
        &[(provider_call_id, call_seed)],
        message_seed + 1,
    );
    conversation
        .append_tool_response(
            message_id(message_seed + 2),
            tool_response(provider_call_id),
        )
        .expect("close the single call");
    assert_eq!(
        super::freeze(
            conversation,
            response(vec![text("final")], StopReason::EndTurn),
            message_seed + 3,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit the single-call turn");
}
