//! Canonical role sequencing and complete content-block validation.

use crate::{
    conversation::{CommitError, ContentBlockKind, ConversationMessage, MessageId, turn::TurnData},
    model::{content::ContentBlock, message::Role},
};
use std::collections::HashMap;

/// Content locations collected while running the role state machine.
#[derive(Default)]
pub(super) struct BlockFacts {
    pub(super) calls: HashMap<String, MessageId>,
    pub(super) call_order: Vec<String>,
    pub(super) results: HashMap<String, MessageId>,
}

/// Provider calls or results found in one message.
#[derive(Default)]
struct MessageFacts {
    calls: Vec<String>,
    results: Vec<String>,
}

/// The only legal next role while walking a candidate turn.
enum RoleState {
    ExpectAssistant,
    AwaitToolResults(HashMap<String, MessageId>),
    Closed,
}

/// Runs the canonical user→assistant→tool*→assistant state machine.
pub(super) fn validate_role_sequence(data: &TurnData) -> Result<BlockFacts, CommitError> {
    if let Some(system) = data
        .messages
        .iter()
        .find(|message| message.payload().role == Role::System)
    {
        return Err(CommitError::SystemRole {
            message_id: system.id(),
        });
    }

    let Some(first) = data.messages.first() else {
        return Err(CommitError::InvalidStartState { first_role: None });
    };
    if first.payload().role != Role::User {
        return Err(CommitError::InvalidStartState {
            first_role: Some(first.payload().role),
        });
    }

    let mut facts = BlockFacts::default();
    inspect_message(first, &mut facts)?;
    let mut state = RoleState::ExpectAssistant;

    for message in data.messages.iter().skip(1) {
        let message_facts = inspect_message(message, &mut facts)?;
        state = match state {
            RoleState::ExpectAssistant if message.payload().role == Role::Assistant => {
                if message_facts.calls.is_empty() {
                    RoleState::Closed
                } else {
                    let open = message_facts
                        .calls
                        .into_iter()
                        .map(|provider_call_id| (provider_call_id, message.id()))
                        .collect();
                    RoleState::AwaitToolResults(open)
                }
            }
            RoleState::AwaitToolResults(mut open) if message.payload().role == Role::Tool => {
                if message_facts.results.is_empty() {
                    return Err(CommitError::EmptyToolMessage {
                        message_id: message.id(),
                    });
                }
                for provider_call_id in message_facts.results {
                    if open.remove(&provider_call_id).is_none() {
                        return Err(CommitError::OrphanToolResult {
                            provider_call_id,
                            result_msg: message.id(),
                        });
                    }
                }
                if open.is_empty() {
                    RoleState::ExpectAssistant
                } else {
                    RoleState::AwaitToolResults(open)
                }
            }
            RoleState::ExpectAssistant => {
                if let Some(provider_call_id) = message_facts.results.first() {
                    return Err(CommitError::OrphanToolResult {
                        provider_call_id: provider_call_id.clone(),
                        result_msg: message.id(),
                    });
                }
                return Err(CommitError::UnexpectedRole {
                    message_id: message.id(),
                    actual: message.payload().role,
                    expected: "assistant after the user or all tool results",
                });
            }
            RoleState::AwaitToolResults(_) => {
                return Err(CommitError::UnexpectedRole {
                    message_id: message.id(),
                    actual: message.payload().role,
                    expected: "one or more tool messages until every parallel call is answered",
                });
            }
            RoleState::Closed => {
                if let Some(provider_call_id) = message_facts.results.first() {
                    return Err(CommitError::OrphanToolResult {
                        provider_call_id: provider_call_id.clone(),
                        result_msg: message.id(),
                    });
                }
                return Err(CommitError::UnexpectedRole {
                    message_id: message.id(),
                    actual: message.payload().role,
                    expected: "the end of the turn after the final assistant message",
                });
            }
        };
    }

    match state {
        RoleState::Closed => Ok(facts),
        RoleState::ExpectAssistant => Err(CommitError::InvalidEndState {
            last_role: data.messages.last().map(|message| message.payload().role),
            has_open_calls: false,
        }),
        RoleState::AwaitToolResults(open) => {
            let last_role = data.messages.last().map(|message| message.payload().role);
            if last_role == Some(Role::Assistant) {
                return Err(CommitError::InvalidEndState {
                    last_role,
                    has_open_calls: true,
                });
            }
            let provider_call_id = facts
                .call_order
                .iter()
                .find(|provider_call_id| open.contains_key(*provider_call_id))
                .expect("open calls originate from collected call facts")
                .clone();
            Err(CommitError::DanglingProviderCall {
                call_msg: open[&provider_call_id],
                provider_call_id,
            })
        }
    }
}

/// Validates one role's block set and records tool-correlation facts.
fn inspect_message(
    message: &ConversationMessage,
    facts: &mut BlockFacts,
) -> Result<MessageFacts, CommitError> {
    let mut message_facts = MessageFacts::default();
    let role = message.payload().role;

    for block in &message.payload().content {
        let allowed = matches!(
            (role, block),
            (
                Role::User,
                ContentBlock::Text { .. } | ContentBlock::Image { .. }
            ) | (
                Role::Assistant,
                ContentBlock::Text { .. }
                    | ContentBlock::ToolUse { .. }
                    | ContentBlock::Thinking { .. }
            ) | (Role::Tool, ContentBlock::ToolResult { .. })
        );
        if !allowed {
            return Err(CommitError::InvalidRoleBlock {
                message_id: message.id(),
                role,
                block: content_kind(block),
            });
        }

        match block {
            ContentBlock::ToolUse { id, name, .. } => {
                validate_complete_tool_use(message.id(), id, name)?;
                if facts.calls.insert(id.clone(), message.id()).is_some() {
                    return Err(CommitError::DuplicateProviderCallId {
                        provider_call_id: id.clone(),
                    });
                }
                facts.call_order.push(id.clone());
                message_facts.calls.push(id.clone());
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                validate_complete_tool_result(message.id(), tool_use_id, content)?;
                if let Some(first_result_msg) =
                    facts.results.insert(tool_use_id.clone(), message.id())
                {
                    return Err(CommitError::DuplicateToolResult {
                        provider_call_id: tool_use_id.clone(),
                        first_result_msg,
                        duplicate_result_msg: message.id(),
                    });
                }
                message_facts.results.push(tool_use_id.clone());
            }
            ContentBlock::Text { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::Thinking { .. } => {}
        }
    }

    Ok(message_facts)
}

/// Rejects tool-use fields that cannot represent a complete invocation.
fn validate_complete_tool_use(
    message_id: MessageId,
    provider_call_id: &str,
    name: &str,
) -> Result<(), CommitError> {
    if provider_call_id.is_empty() {
        return Err(CommitError::IncompleteContent {
            message_id: Some(message_id),
            detail: "a tool-use block has no provider call id",
        });
    }
    if name.is_empty() {
        return Err(CommitError::IncompleteContent {
            message_id: Some(message_id),
            detail: "a tool-use block has no tool name",
        });
    }
    Ok(())
}

/// Rejects incomplete result ids and nested blocks outside the shared adapter model.
fn validate_complete_tool_result(
    message_id: MessageId,
    provider_call_id: &str,
    content: &[ContentBlock],
) -> Result<(), CommitError> {
    if provider_call_id.is_empty() {
        return Err(CommitError::IncompleteContent {
            message_id: Some(message_id),
            detail: "a tool-result block has no provider call id",
        });
    }

    for block in content {
        if !matches!(
            block,
            ContentBlock::Text { .. } | ContentBlock::Image { .. }
        ) {
            return Err(CommitError::InvalidToolResultContent {
                message_id,
                provider_call_id: provider_call_id.to_owned(),
                block: content_kind(block),
            });
        }
    }
    Ok(())
}

/// Maps complete content variants to stable diagnostic categories.
fn content_kind(block: &ContentBlock) -> ContentBlockKind {
    match block {
        ContentBlock::Text { .. } => ContentBlockKind::Text,
        ContentBlock::Image { .. } => ContentBlockKind::Image,
        ContentBlock::ToolUse { .. } => ContentBlockKind::ToolUse,
        ContentBlock::ToolResult { .. } => ContentBlockKind::ToolResult,
        ContentBlock::Thinking { .. } => ContentBlockKind::Thinking,
    }
}
