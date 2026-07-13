//! Data-only preparation for atomic pending-turn cancellation.

use super::{CANCELLED_TOOL_RESULT_TEXT, CancelledToolResult};
use crate::{
    conversation::{
        CancelError, Conversation, ConversationMessage, MessageId, ToolCallId, TurnMeta,
        TurnResponseMeta,
        pending::{FrozenMessage, PendingToolCall, PendingTurn, PendingTurnPhase},
        turn::{ToolPairingData, TurnCompletion, TurnData},
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        tool::ToolStatus,
    },
};
use serde_json::Map;
use std::collections::{HashMap, HashSet};

/// Complete data prepared without touching the live pending transaction.
pub(super) struct PreparedCancellation {
    pub(super) cancelled_messages: Vec<ConversationMessage>,
    pub(super) tool_calls: Vec<PendingToolCall>,
}

/// One open call derived from frozen pending facts in canonical message order.
struct ExpectedOpenCall {
    provider_call_id: String,
    call_id: Option<ToolCallId>,
    call_msg: MessageId,
    tool_call_index: Option<usize>,
}

/// Collects retained-raw identities that synthetic facts cannot reuse.
pub(super) fn retained_id_sets(
    conversation: &Conversation,
) -> (HashSet<MessageId>, HashSet<ToolCallId>) {
    let message_ids = conversation
        .history
        .raw_turns()
        .into_iter()
        .flat_map(crate::conversation::Turn::messages)
        .map(ConversationMessage::id)
        .collect();
    let call_ids = conversation
        .history
        .raw_turns()
        .into_iter()
        .flat_map(crate::conversation::Turn::pairings)
        .map(crate::conversation::ToolPairing::call_id)
        .collect();
    (message_ids, call_ids)
}

/// Validates an exact one-to-one closure and builds immutable cancelled results.
pub(super) fn prepare_cancellation(
    pending: &PendingTurn,
    cancelled_results: &[CancelledToolResult],
    committed_message_ids: &HashSet<MessageId>,
    committed_call_ids: &HashSet<ToolCallId>,
    disposition: &'static str,
) -> Result<PreparedCancellation, CancelError> {
    let expected = expected_open_calls(pending, disposition)?;
    let mut expected_provider_ids = HashSet::with_capacity(expected.len());
    for call in &expected {
        if !expected_provider_ids.insert(call.provider_call_id.as_str()) {
            return Err(CancelError::DuplicateProviderCallId {
                provider_call_id: call.provider_call_id.clone(),
            });
        }
    }

    let mut supplied = HashMap::with_capacity(cancelled_results.len());
    for result in cancelled_results {
        if supplied.insert(result.provider_call_id(), result).is_some() {
            return Err(CancelError::DuplicateCancellationResult {
                provider_call_id: result.provider_call_id.clone(),
            });
        }
        if !expected_provider_ids.contains(result.provider_call_id()) {
            return Err(CancelError::UnknownCancellationResult {
                provider_call_id: result.provider_call_id.clone(),
            });
        }
    }

    let mut used_message_ids = committed_message_ids.clone();
    used_message_ids.extend(pending.messages().iter().map(ConversationMessage::id));
    let mut used_call_ids = committed_call_ids.clone();
    used_call_ids.extend(pending.tool_calls().iter().map(PendingToolCall::call_id));

    let mut tool_calls = pending.tool_calls().to_vec();
    let mut cancelled_messages = Vec::with_capacity(expected.len());
    for call in expected {
        let result = supplied
            .get(call.provider_call_id.as_str())
            .ok_or_else(|| CancelError::MissingCancellationResult {
                provider_call_id: call.provider_call_id.clone(),
            })?;
        if !used_message_ids.insert(result.message_id()) {
            return Err(CancelError::DuplicateMessageId {
                message_id: result.message_id(),
            });
        }

        match call.call_id {
            Some(expected_call_id) if expected_call_id != result.call_id() => {
                return Err(CancelError::ToolCallIdMismatch {
                    provider_call_id: call.provider_call_id,
                    expected: expected_call_id,
                    actual: result.call_id(),
                });
            }
            Some(_) => {
                let index = call.tool_call_index.ok_or(CancelError::InvalidTransition {
                    disposition,
                    actual: pending.phase(),
                })?;
                tool_calls[index].set_result_message_id(result.message_id());
            }
            None => {
                if !used_call_ids.insert(result.call_id()) {
                    return Err(CancelError::DuplicateToolCallId {
                        call_id: result.call_id(),
                    });
                }
                tool_calls.push(PendingToolCall::new(
                    result.call_id(),
                    call.provider_call_id.clone(),
                    call.call_msg,
                    Some(result.message_id()),
                ));
            }
        }

        cancelled_messages.push(cancelled_result_message(
            result.message_id(),
            call.provider_call_id,
        ));
    }

    Ok(PreparedCancellation {
        cancelled_messages,
        tool_calls,
    })
}

/// Derives open calls without reading the active partial message.
fn expected_open_calls(
    pending: &PendingTurn,
    disposition: &'static str,
) -> Result<Vec<ExpectedOpenCall>, CancelError> {
    match pending.phase() {
        PendingTurnPhase::AwaitingAssistant | PendingTurnPhase::AssistantInProgress => {
            Ok(Vec::new())
        }
        PendingTurnPhase::AwaitingToolCallMappings => {
            let call_msg = pending
                .messages()
                .last()
                .map(ConversationMessage::id)
                .ok_or(CancelError::InvalidTransition {
                    disposition,
                    actual: pending.phase(),
                })?;
            Ok(pending
                .unmapped_provider_call_ids()
                .iter()
                .map(|provider_call_id| ExpectedOpenCall {
                    provider_call_id: provider_call_id.clone(),
                    call_id: None,
                    call_msg,
                    tool_call_index: None,
                })
                .collect())
        }
        PendingTurnPhase::AwaitingToolResults => Ok(pending
            .tool_calls()
            .iter()
            .enumerate()
            .filter(|(_, call)| call.result_message_id().is_none())
            .map(|(index, call)| ExpectedOpenCall {
                provider_call_id: call.provider_call_id().to_owned(),
                call_id: Some(call.call_id()),
                call_msg: call.call_message_id(),
                tool_call_index: Some(index),
            })
            .collect()),
        PendingTurnPhase::ReadyToCommit => Err(CancelError::InvalidTransition {
            disposition,
            actual: PendingTurnPhase::ReadyToCommit,
        }),
    }
}

/// Produces one complete tool-role message with an explicit interruption fact.
fn cancelled_result_message(
    message_id: MessageId,
    provider_call_id: String,
) -> ConversationMessage {
    ConversationMessage::new(
        message_id,
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: provider_call_id,
                content: vec![ContentBlock::Text {
                    text: CANCELLED_TOOL_RESULT_TEXT.to_owned(),
                    extra: Map::new(),
                }],
                status: ToolStatus::Cancelled,
                extra: Map::new(),
            }],
        },
    )
}

/// Ensures the final identity cannot collide with frozen or synthetic messages.
pub(super) fn validate_final_message_id(
    pending: &PendingTurn,
    prepared: &PreparedCancellation,
    committed_message_ids: &HashSet<MessageId>,
    final_message_id: MessageId,
) -> Result<(), CancelError> {
    let duplicate = committed_message_ids.contains(&final_message_id)
        || pending
            .messages()
            .iter()
            .any(|message| message.id() == final_message_id)
        || prepared
            .cancelled_messages
            .iter()
            .any(|message| message.id() == final_message_id);
    if duplicate {
        return Err(CancelError::DuplicateMessageId {
            message_id: final_message_id,
        });
    }
    Ok(())
}

/// Combines prepared cancellation facts with one complete final assistant DTO.
pub(super) fn cancelled_turn_data(
    pending: &PendingTurn,
    prepared: PreparedCancellation,
    final_message: FrozenMessage,
    mut meta: TurnMeta,
) -> TurnData {
    let (final_message, final_usage, stop_reason, extra) = final_message.into_parts();
    let final_message_id = final_message.id();
    let mut messages = pending.messages().to_vec();
    messages.extend(prepared.cancelled_messages);
    messages.push(final_message);

    let pairings = prepared
        .tool_calls
        .iter()
        .map(|call| ToolPairingData {
            call_id: call.call_id(),
            provider_call_id: Some(call.provider_call_id().to_owned()),
            call_msg: call.call_message_id(),
            result_msg: call.result_message_id(),
        })
        .collect();
    let mut usage = pending.usage().clone();
    usage.merge(final_usage);
    let mut responses = pending.responses().to_vec();
    responses.push(TurnResponseMeta::new(final_message_id, stop_reason, extra));
    meta.merge_pending(usage, &responses);

    TurnData {
        id: pending.id(),
        messages,
        pairings,
        parent: pending.parent(),
        meta,
        completion: TurnCompletion::Complete,
    }
}
