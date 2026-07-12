//! Transaction-local state for one complete Conversation turn.

use super::PendingMessage;
use crate::{
    client::Response,
    conversation::{
        ContentBlockKind, ConversationError, ConversationMessage, MessageId, PendingTurnError,
        TurnId, TurnMeta, TurnResponseMeta,
        turn::{TurnCompletion, TurnData},
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        usage::Usage,
    },
    stream::StreamEvent,
};

mod tool;

pub use tool::{PendingToolCall, ToolCallMapping};

/// The externally visible phase of one pending turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingTurnPhase {
    /// The user message or a complete tool-result batch awaits an assistant.
    AwaitingAssistant,
    /// Exactly one streaming or complete response is waiting to be frozen.
    AssistantInProgress,
    /// A frozen tool-use message awaits framework [`crate::conversation::ToolCallId`] mappings.
    AwaitingToolCallMappings,
    /// One or more registered tool calls still await complete results.
    AwaitingToolResults,
    /// A final assistant message made the turn eligible for checked commit.
    ReadyToCommit,
}

/// The result of freezing one complete assistant response into a pending turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistantFinish {
    /// The response contained no tool use and the turn can now be committed.
    ReadyToCommit,
    /// Tool uses must receive framework identities before results can be added.
    RequiresToolCallMappings,
}

/// One mutable transaction that has not entered committed history.
///
/// Frozen messages are exposed only through shared references. The one active
/// [`PendingMessage`] remains private, so partial blocks and JSON cannot be
/// observed as complete content.
///
/// ```compile_fail
/// use agent_lib::conversation::{ConversationMessage, PendingTurn};
///
/// fn replace_frozen_message(
///     pending: &mut PendingTurn,
///     replacement: ConversationMessage,
/// ) {
///     pending.messages()[0] = replacement;
/// }
/// ```
#[must_use = "a pending turn must be committed or explicitly cancelled"]
pub struct PendingTurn {
    id: TurnId,
    parent: Option<TurnId>,
    messages: Vec<ConversationMessage>,
    tool_calls: Vec<PendingToolCall>,
    usage: Usage,
    responses: Vec<TurnResponseMeta>,
    state: PendingTurnState,
}

impl PendingTurn {
    /// Creates a draft after validating the initial user payload.
    pub(in crate::conversation) fn new(
        id: TurnId,
        parent: Option<TurnId>,
        user_message_id: MessageId,
        user_payload: Message,
    ) -> Result<Self, PendingTurnError> {
        if user_payload.role != Role::User {
            return Err(PendingTurnError::InvalidUserRole {
                actual: user_payload.role,
            });
        }
        if let Some(block) = user_payload.content.iter().find(|block| {
            !matches!(
                block,
                ContentBlock::Text { .. } | ContentBlock::Image { .. }
            )
        }) {
            return Err(PendingTurnError::InvalidUserBlock {
                block: content_kind(block),
            });
        }

        Ok(Self {
            id,
            parent,
            messages: vec![ConversationMessage::new(user_message_id, user_payload)],
            tool_calls: Vec::new(),
            usage: Usage::default(),
            responses: Vec::new(),
            state: PendingTurnState::AwaitingAssistant,
        })
    }

    /// Returns this pending turn's externally supplied identity.
    #[must_use]
    pub const fn id(&self) -> TurnId {
        self.id
    }

    /// Returns the committed parent captured when the transaction began.
    #[must_use]
    pub const fn parent(&self) -> Option<TurnId> {
        self.parent
    }

    /// Returns every message that has already crossed a complete freeze boundary.
    #[must_use]
    pub fn messages(&self) -> &[ConversationMessage] {
        &self.messages
    }

    /// Returns all framework/provider tool correlations registered so far.
    #[must_use]
    pub fn tool_calls(&self) -> &[PendingToolCall] {
        &self.tool_calls
    }

    /// Iterates over only the calls that still need a result.
    pub fn open_calls(&self) -> impl Iterator<Item = &PendingToolCall> {
        self.tool_calls
            .iter()
            .filter(|tool_call| tool_call.result_msg.is_none())
    }

    /// Returns provider ids awaiting caller-supplied framework mappings.
    #[must_use]
    pub fn unmapped_provider_call_ids(&self) -> &[String] {
        match &self.state {
            PendingTurnState::AwaitingToolCallMappings { provider_call_ids } => provider_call_ids,
            PendingTurnState::AwaitingAssistant
            | PendingTurnState::AssistantInProgress(_)
            | PendingTurnState::AwaitingToolResults
            | PendingTurnState::ReadyToCommit => &[],
        }
    }

    /// Returns token usage accumulated from every frozen assistant response.
    #[must_use]
    pub const fn usage(&self) -> &Usage {
        &self.usage
    }

    /// Returns response stop reasons and provider metadata in message order.
    #[must_use]
    pub fn responses(&self) -> &[TurnResponseMeta] {
        &self.responses
    }

    /// Returns the current transition phase without exposing active partials.
    #[must_use]
    pub const fn phase(&self) -> PendingTurnPhase {
        match self.state {
            PendingTurnState::AwaitingAssistant => PendingTurnPhase::AwaitingAssistant,
            PendingTurnState::AssistantInProgress(_) => PendingTurnPhase::AssistantInProgress,
            PendingTurnState::AwaitingToolCallMappings { .. } => {
                PendingTurnPhase::AwaitingToolCallMappings
            }
            PendingTurnState::AwaitingToolResults => PendingTurnPhase::AwaitingToolResults,
            PendingTurnState::ReadyToCommit => PendingTurnPhase::ReadyToCommit,
        }
    }

    /// Starts the one mutable assistant accumulator allowed in this turn.
    pub(in crate::conversation) fn start_assistant(&mut self) -> Result<(), PendingTurnError> {
        self.start_assistant_from(PendingMessage::new())
    }

    /// Starts an assistant freeze from an already complete Client response.
    pub(in crate::conversation) fn start_assistant_response(
        &mut self,
        response: Response,
    ) -> Result<(), PendingTurnError> {
        self.start_assistant_from(PendingMessage::from_response(response))
    }

    /// Folds one normalized event into the active assistant response.
    pub(in crate::conversation) fn push_assistant_event(
        &mut self,
        event: StreamEvent,
    ) -> Result<(), ConversationError> {
        let actual = self.phase();
        let PendingTurnState::AssistantInProgress(pending) = &mut self.state else {
            return Err(PendingTurnError::InvalidTransition {
                operation: "push an assistant stream event",
                expected: "assistant_in_progress",
                actual,
            }
            .into());
        };
        pending.push(event)
    }

    /// Freezes the active assistant and advances according to its tool uses.
    pub(in crate::conversation) fn finish_assistant(
        &mut self,
        message_id: MessageId,
    ) -> Result<AssistantFinish, ConversationError> {
        if self.contains_message_id(message_id) {
            return Err(PendingTurnError::DuplicateMessageId { message_id }.into());
        }

        let actual = self.phase();
        let PendingTurnState::AssistantInProgress(pending) = &mut self.state else {
            return Err(PendingTurnError::InvalidTransition {
                operation: "finish an assistant response",
                expected: "assistant_in_progress",
                actual,
            }
            .into());
        };
        let frozen = pending.finish(message_id)?;
        let (message, usage, stop_reason, extra) = frozen.into_parts();
        let provider_call_ids = message
            .payload()
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                ContentBlock::Text { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::ToolResult { .. }
                | ContentBlock::Thinking { .. } => None,
            })
            .collect::<Vec<_>>();

        self.usage.merge(usage);
        self.responses
            .push(TurnResponseMeta::new(message_id, stop_reason, extra));
        self.messages.push(message);

        if provider_call_ids.is_empty() {
            self.state = PendingTurnState::ReadyToCommit;
            Ok(AssistantFinish::ReadyToCommit)
        } else {
            self.state = PendingTurnState::AwaitingToolCallMappings { provider_call_ids };
            Ok(AssistantFinish::RequiresToolCallMappings)
        }
    }

    /// Builds a complete data-only draft while retaining this pending value.
    pub(in crate::conversation) fn turn_data(
        &self,
        mut meta: TurnMeta,
    ) -> Result<TurnData, PendingTurnError> {
        if self.phase() != PendingTurnPhase::ReadyToCommit {
            return Err(PendingTurnError::InvalidTransition {
                operation: "commit the pending turn",
                expected: "ready_to_commit",
                actual: self.phase(),
            });
        }

        meta.merge_pending(self.usage.clone(), &self.responses);
        Ok(TurnData {
            id: self.id,
            messages: self.messages.clone(),
            pairings: self.pairing_data(),
            parent: self.parent,
            meta,
            completion: TurnCompletion::Complete,
        })
    }

    /// Applies a fully validated cancellation closure as one infallible step.
    ///
    /// Replacing `state` drops any active [`PendingMessage`] without finishing
    /// or parsing it. The caller prepares every synthetic result and updated
    /// correlation before invoking this method, so no partial closure can be
    /// observed after mutation begins.
    pub(in crate::conversation) fn resume_after_cancel(
        &mut self,
        cancelled_messages: Vec<ConversationMessage>,
        tool_calls: Vec<PendingToolCall>,
    ) {
        self.messages.extend(cancelled_messages);
        self.tool_calls = tool_calls;
        self.state = PendingTurnState::AwaitingAssistant;
    }

    /// Reports whether an identity is already frozen in this transaction.
    pub(super) fn contains_message_id(&self, message_id: MessageId) -> bool {
        self.messages
            .iter()
            .any(|message| message.id() == message_id)
    }

    /// Installs an assistant source only from the legal step boundary.
    fn start_assistant_from(&mut self, pending: PendingMessage) -> Result<(), PendingTurnError> {
        if self.phase() != PendingTurnPhase::AwaitingAssistant {
            return Err(PendingTurnError::InvalidTransition {
                operation: "start an assistant response",
                expected: "awaiting_assistant",
                actual: self.phase(),
            });
        }
        self.state = PendingTurnState::AssistantInProgress(pending);
        Ok(())
    }
}

impl std::fmt::Debug for PendingTurn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingTurn")
            .field("id", &self.id)
            .field("parent", &self.parent)
            .field("messages", &self.messages)
            .field("tool_calls", &self.tool_calls)
            .field("usage", &self.usage)
            .field("responses", &self.responses)
            .field("phase", &self.phase())
            .finish()
    }
}

/// Internal state keeps the unique mutable accumulator out of public views.
enum PendingTurnState {
    AwaitingAssistant,
    AssistantInProgress(PendingMessage),
    AwaitingToolCallMappings { provider_call_ids: Vec<String> },
    AwaitingToolResults,
    ReadyToCommit,
}

/// Maps a content value to the category used in classified errors.
fn content_kind(block: &ContentBlock) -> ContentBlockKind {
    match block {
        ContentBlock::Text { .. } => ContentBlockKind::Text,
        ContentBlock::Image { .. } => ContentBlockKind::Image,
        ContentBlock::ToolUse { .. } => ContentBlockKind::ToolUse,
        ContentBlock::ToolResult { .. } => ContentBlockKind::ToolResult,
        ContentBlock::Thinking { .. } => ContentBlockKind::Thinking,
    }
}

#[cfg(test)]
mod tests;
