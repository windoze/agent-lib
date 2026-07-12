//! Framework/provider tool-call bookkeeping for a pending turn.

use super::{PendingTurn, PendingTurnPhase, PendingTurnState, content_kind};
use crate::{
    conversation::{MessageId, PendingTurnError, ToolCallId, turn::ToolPairingData},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        tool::ToolResponse,
    },
};
use std::collections::{HashMap, HashSet};

/// Caller-supplied correlation between provider and framework tool identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallMapping {
    provider_call_id: String,
    call_id: ToolCallId,
}

impl ToolCallMapping {
    /// Creates a mapping for one provider tool-use block.
    #[must_use]
    pub fn new(provider_call_id: impl Into<String>, call_id: ToolCallId) -> Self {
        Self {
            provider_call_id: provider_call_id.into(),
            call_id,
        }
    }

    /// Returns the provider identity copied from the tool-use block.
    #[must_use]
    pub fn provider_call_id(&self) -> &str {
        &self.provider_call_id
    }

    /// Returns the framework identity supplied for bookkeeping.
    #[must_use]
    pub const fn call_id(&self) -> ToolCallId {
        self.call_id
    }
}

/// One tool call recorded inside a pending turn.
///
/// Unlike a closed [`ToolPairing`](crate::conversation::ToolPairing), the
/// result message is optional while execution is in flight. All fields are
/// read-only to callers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingToolCall {
    pub(super) call_id: ToolCallId,
    pub(super) provider_call_id: String,
    pub(super) call_msg: MessageId,
    pub(super) result_msg: Option<MessageId>,
}

impl PendingToolCall {
    /// Returns the framework-owned bookkeeping identity.
    #[must_use]
    pub const fn call_id(&self) -> ToolCallId {
        self.call_id
    }

    /// Returns the provider identity used by request serialization.
    #[must_use]
    pub fn provider_call_id(&self) -> &str {
        &self.provider_call_id
    }

    /// Returns the frozen assistant message containing the tool use.
    #[must_use]
    pub const fn call_message_id(&self) -> MessageId {
        self.call_msg
    }

    /// Returns the result message once this pending call has been closed.
    #[must_use]
    pub const fn result_message_id(&self) -> Option<MessageId> {
        self.result_msg
    }
}

impl PendingTurn {
    /// Registers an exact, unique mapping for every tool use in the last assistant.
    pub(in crate::conversation) fn register_tool_calls(
        &mut self,
        mappings: Vec<ToolCallMapping>,
        committed_call_ids: &HashSet<ToolCallId>,
    ) -> Result<(), PendingTurnError> {
        let provider_call_ids = match &self.state {
            PendingTurnState::AwaitingToolCallMappings { provider_call_ids } => {
                provider_call_ids.clone()
            }
            PendingTurnState::AwaitingAssistant
            | PendingTurnState::AssistantInProgress(_)
            | PendingTurnState::AwaitingToolResults
            | PendingTurnState::ReadyToCommit => {
                return Err(PendingTurnError::InvalidTransition {
                    operation: "register tool-call mappings",
                    expected: "awaiting_tool_call_mappings",
                    actual: self.phase(),
                });
            }
        };

        let mut expected = HashSet::with_capacity(provider_call_ids.len());
        for provider_call_id in &provider_call_ids {
            if !expected.insert(provider_call_id.as_str())
                || self
                    .tool_calls
                    .iter()
                    .any(|call| call.provider_call_id.as_str() == provider_call_id.as_str())
            {
                return Err(PendingTurnError::DuplicateProviderCallId {
                    provider_call_id: provider_call_id.clone(),
                });
            }
        }

        let mut by_provider = HashMap::with_capacity(mappings.len());
        let mut new_call_ids = HashSet::with_capacity(mappings.len());
        for mapping in &mappings {
            if !expected.contains(mapping.provider_call_id()) {
                return Err(PendingTurnError::UnknownToolCallMapping {
                    provider_call_id: mapping.provider_call_id.clone(),
                });
            }
            if by_provider
                .insert(mapping.provider_call_id(), mapping.call_id)
                .is_some()
            {
                return Err(PendingTurnError::DuplicateToolCallMapping {
                    provider_call_id: mapping.provider_call_id.clone(),
                });
            }
            if committed_call_ids.contains(&mapping.call_id)
                || self.contains_tool_call_id(mapping.call_id)
                || !new_call_ids.insert(mapping.call_id)
            {
                return Err(PendingTurnError::DuplicateToolCallId {
                    call_id: mapping.call_id,
                });
            }
        }

        for provider_call_id in &provider_call_ids {
            if !by_provider.contains_key(provider_call_id.as_str()) {
                return Err(PendingTurnError::MissingToolCallMapping {
                    provider_call_id: provider_call_id.clone(),
                });
            }
        }

        let call_msg = self
            .messages
            .last()
            .expect("a mapped assistant was frozen into pending messages")
            .id();
        self.tool_calls
            .extend(
                provider_call_ids
                    .into_iter()
                    .map(|provider_call_id| PendingToolCall {
                        call_id: by_provider[provider_call_id.as_str()],
                        provider_call_id,
                        call_msg,
                        result_msg: None,
                    }),
            );
        self.state = PendingTurnState::AwaitingToolResults;
        Ok(())
    }

    /// Appends one complete tool-result block and closes its matching call.
    pub(in crate::conversation) fn append_tool_result(
        &mut self,
        message_id: MessageId,
        block: ContentBlock,
    ) -> Result<ToolCallId, PendingTurnError> {
        if self.contains_message_id(message_id) {
            return Err(PendingTurnError::DuplicateMessageId { message_id });
        }

        let ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } = &block
        else {
            return Err(PendingTurnError::InvalidToolResultBlock {
                actual: content_kind(&block),
            });
        };
        if let Some(nested) = content.iter().find(|nested| {
            !matches!(
                nested,
                ContentBlock::Text { .. } | ContentBlock::Image { .. }
            )
        }) {
            return Err(PendingTurnError::InvalidToolResultContent {
                provider_call_id: tool_use_id.clone(),
                block: content_kind(nested),
            });
        }

        let Some(index) = self
            .tool_calls
            .iter()
            .position(|call| call.provider_call_id == *tool_use_id)
        else {
            return Err(PendingTurnError::UnknownToolResult {
                provider_call_id: tool_use_id.clone(),
            });
        };
        if self.tool_calls[index].result_msg.is_some() {
            return Err(PendingTurnError::DuplicateToolResult {
                provider_call_id: tool_use_id.clone(),
            });
        }
        if self.phase() != PendingTurnPhase::AwaitingToolResults {
            return Err(PendingTurnError::InvalidTransition {
                operation: "append a tool result",
                expected: "awaiting_tool_results",
                actual: self.phase(),
            });
        }

        let call_id = self.tool_calls[index].call_id;
        self.messages
            .push(crate::conversation::ConversationMessage::new(
                message_id,
                Message {
                    role: Role::Tool,
                    content: vec![block],
                },
            ));
        self.tool_calls[index].result_msg = Some(message_id);
        if self.tool_calls.iter().all(|call| call.result_msg.is_some()) {
            self.state = PendingTurnState::AwaitingAssistant;
        }
        Ok(call_id)
    }

    /// Converts a complete tool response through the lossless result-block path.
    pub(in crate::conversation) fn append_tool_response(
        &mut self,
        message_id: MessageId,
        response: ToolResponse,
    ) -> Result<ToolCallId, PendingTurnError> {
        self.append_tool_result(message_id, response.into())
    }

    /// Converts all pending call facts to the validator's data-only shape.
    pub(super) fn pairing_data(&self) -> Vec<ToolPairingData> {
        self.tool_calls
            .iter()
            .map(|call| ToolPairingData {
                call_id: call.call_id,
                provider_call_id: Some(call.provider_call_id.clone()),
                call_msg: call.call_msg,
                result_msg: call.result_msg,
            })
            .collect()
    }

    /// Reports whether this transaction already owns a framework call id.
    fn contains_tool_call_id(&self, call_id: ToolCallId) -> bool {
        self.tool_calls.iter().any(|call| call.call_id == call_id)
    }
}
