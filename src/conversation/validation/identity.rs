//! Completion, parent, and conversation-wide identity validation.

use crate::conversation::{
    CommitError, MessageId, ToolCallId, Turn, TurnId,
    turn::{TurnCompletion, TurnData},
};
use std::collections::HashSet;

/// Rejects a draft marker before parsed values can be treated as complete.
pub(super) fn validate_completion(data: &TurnData) -> Result<(), CommitError> {
    if data.completion == TurnCompletion::PendingContent {
        return Err(CommitError::IncompleteContent {
            message_id: None,
            detail: "a pending message or content block has not reached its terminal boundary",
        });
    }
    Ok(())
}

/// Enforces conversation-wide uniqueness for the candidate turn id.
pub(super) fn validate_turn_identity(
    data: &TurnData,
    retained: &[&Turn],
) -> Result<(), CommitError> {
    if retained.iter().any(|turn| turn.id() == data.id) {
        return Err(CommitError::DuplicateTurnId { turn_id: data.id });
    }
    Ok(())
}

/// Ensures each new turn extends the current committed head exactly.
pub(super) fn validate_parent(
    data: &TurnData,
    expected_parent: Option<TurnId>,
) -> Result<(), CommitError> {
    if data.parent != expected_parent {
        return Err(CommitError::ParentMismatch {
            expected: expected_parent,
            actual: data.parent,
        });
    }
    Ok(())
}

/// Collects message ids from every retained raw history node.
pub(super) fn retained_message_ids(retained: &[&Turn]) -> HashSet<MessageId> {
    retained
        .iter()
        .flat_map(|turn| turn.messages())
        .map(super::super::ConversationMessage::id)
        .collect()
}

/// Enforces message-id uniqueness both within the draft and across history.
pub(super) fn validate_message_ids(
    data: &TurnData,
    retained: &HashSet<MessageId>,
) -> Result<HashSet<MessageId>, CommitError> {
    let mut current = HashSet::with_capacity(data.messages.len());
    for message in &data.messages {
        let id = message.id();
        if retained.contains(&id) || !current.insert(id) {
            return Err(CommitError::DuplicateMessageId { message_id: id });
        }
    }
    Ok(current)
}

/// Enforces framework tool-call identity uniqueness for the whole conversation.
pub(super) fn validate_tool_call_ids(
    data: &TurnData,
    retained: &[&Turn],
) -> Result<(), CommitError> {
    let retained_ids = retained
        .iter()
        .flat_map(|turn| turn.pairings())
        .map(super::super::ToolPairing::call_id)
        .collect::<HashSet<_>>();
    let mut current = HashSet::<ToolCallId>::with_capacity(data.pairings.len());

    for pairing in &data.pairings {
        if retained_ids.contains(&pairing.call_id) || !current.insert(pairing.call_id) {
            return Err(CommitError::DuplicateToolCallId {
                call_id: pairing.call_id,
            });
        }
    }
    Ok(())
}
