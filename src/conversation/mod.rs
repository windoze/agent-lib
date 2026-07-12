//! Provider-neutral Conversation identity and immutable committed history.
//!
//! This module layers stable identities and Conversation-level configuration
//! around complete Client [`Message`] values.
//! It does not change the Client wire model: system instructions remain
//! configuration, while message ids remain in [`ConversationMessage`]. Closed
//! turns can enter history only through the crate-private validated commit path
//! used by the pending layer. Cancellation discards active partials and either
//! drops the whole pending turn, restores an assistant boundary with explicit
//! cancelled tool results, or validates a caller-supplied final response before
//! committing.

pub mod config;
pub mod error;
pub mod id;
pub mod message;
pub mod pending;
pub mod turn;
mod validation;

pub use config::ConversationConfig;
pub use error::{
    CancelError, CommitError, ContentBlockKind, ConversationError, PairingMessageKind,
    PendingMessageError, PendingTurnError,
};
pub use id::{ArtifactId, ConversationId, MessageId, ToolCallId, TurnId};
pub use message::ConversationMessage;
pub use pending::{
    AssistantFinish, CANCELLED_TOOL_RESULT_TEXT, CancelDisposition, CancelOutcome,
    CancelledToolResult, FrozenMessage, PendingMessage, PendingToolCall, PendingTurn,
    PendingTurnPhase, ToolCallMapping,
};
pub use turn::{ToolPairing, Turn, TurnMeta, TurnResponseMeta};

use crate::{
    client::Response,
    model::{content::ContentBlock, message::Message, tool::ToolResponse},
    stream::StreamEvent,
};
use std::collections::HashSet;
use turn::TurnData;

/// One provider-neutral conversation with immutable committed turns.
///
/// A new value is empty and receives all identity and configuration from the
/// caller. Public code can inspect committed state but cannot push raw turns;
/// every pending transaction uses the same crate-private atomic commit gate.
///
/// Committed history cannot be extended through its read-only slice:
///
/// ```compile_fail
/// use agent_lib::conversation::{Conversation, ConversationConfig, ConversationId};
///
/// let id: ConversationId =
///     "018f0d9c-7b6a-7c12-8f31-1234567890ab".parse().unwrap();
/// let mut conversation = Conversation::new(id, ConversationConfig::default());
/// conversation.turns().push(todo!());
/// ```
#[derive(Debug)]
pub struct Conversation {
    id: ConversationId,
    config: ConversationConfig,
    turns: Vec<Turn>,
    pending: Option<PendingTurn>,
    version: u64,
}

impl Conversation {
    /// Creates an empty conversation under an externally supplied identity.
    #[must_use]
    pub const fn new(id: ConversationId, config: ConversationConfig) -> Self {
        Self {
            id,
            config,
            turns: Vec::new(),
            pending: None,
            version: 0,
        }
    }

    /// Returns this conversation's externally supplied stable identity.
    #[must_use]
    pub const fn id(&self) -> ConversationId {
        self.id
    }

    /// Returns configuration kept outside immutable message history.
    #[must_use]
    pub const fn config(&self) -> &ConversationConfig {
        &self.config
    }

    /// Returns all closed turns in committed order through a read-only slice.
    #[must_use]
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    /// Returns the unique uncommitted transaction through a read-only view.
    #[must_use]
    pub const fn pending(&self) -> Option<&PendingTurn> {
        self.pending.as_ref()
    }

    /// Returns the monotonic version advanced by each successful commit.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Begins one transaction from a complete external user payload.
    ///
    /// The caller supplies both identities. The user message remains outside
    /// committed history until a final assistant response passes
    /// [`commit_pending`](Self::commit_pending).
    pub fn begin_turn(
        &mut self,
        turn_id: TurnId,
        user_message_id: MessageId,
        user_payload: Message,
    ) -> Result<(), ConversationError> {
        if let Some(pending) = &self.pending {
            return Err(PendingTurnError::AlreadyPending {
                turn_id: pending.id(),
            }
            .into());
        }
        if self.turns.iter().any(|turn| turn.id() == turn_id) {
            return Err(PendingTurnError::DuplicateTurnId { turn_id }.into());
        }
        if self.committed_message_exists(user_message_id) {
            return Err(PendingTurnError::DuplicateMessageId {
                message_id: user_message_id,
            }
            .into());
        }

        let parent = self.turns.last().map(Turn::id);
        let pending = PendingTurn::new(turn_id, parent, user_message_id, user_payload)?;
        self.pending = Some(pending);
        Ok(())
    }

    /// Starts one streaming assistant response at the current step boundary.
    pub fn start_assistant(&mut self) -> Result<(), ConversationError> {
        self.pending_mut()?.start_assistant().map_err(Into::into)
    }

    /// Starts an assistant response from complete non-streaming Client data.
    pub fn start_assistant_response(
        &mut self,
        response: Response,
    ) -> Result<(), ConversationError> {
        self.pending_mut()?
            .start_assistant_response(response)
            .map_err(Into::into)
    }

    /// Folds one normalized event into the active assistant response.
    pub fn push_assistant_event(&mut self, event: StreamEvent) -> Result<(), ConversationError> {
        self.pending_mut()?.push_assistant_event(event)
    }

    /// Freezes the active assistant response under an external message id.
    ///
    /// A tool-free response makes the transaction ready to commit. A response
    /// containing tool uses freezes first, then requires an exact call to
    /// [`register_tool_calls`](Self::register_tool_calls).
    pub fn finish_assistant(
        &mut self,
        message_id: MessageId,
    ) -> Result<AssistantFinish, ConversationError> {
        if self.committed_message_exists(message_id) {
            return Err(PendingTurnError::DuplicateMessageId { message_id }.into());
        }
        self.pending_mut()?.finish_assistant(message_id)
    }

    /// Registers framework identities for every tool use in the last assistant.
    ///
    /// The mapping is exact and atomic: missing, extra, duplicate, or
    /// conversation-wide reused identities leave the pending bookkeeping
    /// unchanged so the caller can retry with a corrected set.
    pub fn register_tool_calls(
        &mut self,
        mappings: Vec<ToolCallMapping>,
    ) -> Result<(), ConversationError> {
        let committed_call_ids = self
            .turns
            .iter()
            .flat_map(Turn::pairings)
            .map(ToolPairing::call_id)
            .collect::<HashSet<_>>();
        self.pending_mut()?
            .register_tool_calls(mappings, &committed_call_ids)
            .map_err(Into::into)
    }

    /// Adds a complete tool response as one immutable tool-role message.
    ///
    /// The provider call id must name a registered open call. A call can be
    /// closed exactly once; all parallel calls must close before another
    /// assistant response can start.
    pub fn append_tool_response(
        &mut self,
        message_id: MessageId,
        response: ToolResponse,
    ) -> Result<ToolCallId, ConversationError> {
        if self.committed_message_exists(message_id) {
            return Err(PendingTurnError::DuplicateMessageId { message_id }.into());
        }
        self.pending_mut()?
            .append_tool_response(message_id, response)
            .map_err(Into::into)
    }

    /// Adds one already-normalized complete tool-result content block.
    ///
    /// Non-result blocks and invalid nested tool content are rejected before
    /// pending messages or call bookkeeping change.
    pub fn append_tool_result(
        &mut self,
        message_id: MessageId,
        block: ContentBlock,
    ) -> Result<ToolCallId, ConversationError> {
        if self.committed_message_exists(message_id) {
            return Err(PendingTurnError::DuplicateMessageId { message_id }.into());
        }
        self.pending_mut()?
            .append_tool_result(message_id, block)
            .map_err(Into::into)
    }

    /// Validates and atomically commits a ready pending turn.
    ///
    /// Response usage and metadata are merged into `meta` only in the draft
    /// sent through the sole M1 validator. Any phase, validation, parent, or
    /// version error leaves committed history and the pending transaction
    /// available for inspection or later cancellation.
    pub fn commit_pending(&mut self, meta: TurnMeta) -> Result<TurnId, ConversationError> {
        let data = self
            .pending
            .as_ref()
            .ok_or(PendingTurnError::NoPending)?
            .turn_data(meta)?;
        let turn_id = self.commit_draft(data)?;
        self.pending = None;
        Ok(turn_id)
    }

    /// Returns the pending transaction mutably only to this module's checked API.
    fn pending_mut(&mut self) -> Result<&mut PendingTurn, ConversationError> {
        self.pending
            .as_mut()
            .ok_or_else(|| PendingTurnError::NoPending.into())
    }

    /// Scans immutable history for a conversation-wide message identity.
    fn committed_message_exists(&self, message_id: MessageId) -> bool {
        self.turns
            .iter()
            .flat_map(Turn::messages)
            .any(|message| message.id() == message_id)
    }

    /// Validates a complete draft and advances history/version as one operation.
    ///
    /// Every fallible precondition is checked before either field is changed.
    /// This remains crate-private so M2 pending transitions, rather than raw
    /// containers, become the public way to build committed history.
    pub(crate) fn commit_draft(&mut self, data: TurnData) -> Result<TurnId, ConversationError> {
        let next_version =
            self.version
                .checked_add(1)
                .ok_or(ConversationError::NonAtomicCommit {
                    current_version: self.version,
                })?;
        let expected_parent = self.turns.last().map(Turn::id);
        let turn = validation::validate_turn_data(data, &self.turns, expected_parent)?;
        let turn_id = turn.id();

        self.turns.push(turn);
        self.version = next_version;
        Ok(turn_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{Conversation, ConversationConfig, ConversationId, ConversationMessage, MessageId};
    use crate::model::message::{Message, Role};

    fn message(id: &str, role: Role) -> ConversationMessage {
        ConversationMessage::new(
            id.parse::<MessageId>().expect("message id"),
            Message {
                role,
                content: Vec::new(),
            },
        )
    }

    #[test]
    fn system_configuration_does_not_enter_the_payload_role_sequence() {
        let config = ConversationConfig::new(Some("Answer safely.".to_owned()));
        let history = [
            message("018f0d9c-7b6a-7c12-8f31-1234567890ad", Role::User),
            message("018f0d9c-7b6a-7c12-8f31-1234567890ae", Role::Assistant),
        ];

        let roles = history
            .iter()
            .map(|message| message.payload().role)
            .collect::<Vec<_>>();

        assert_eq!(config.system(), Some("Answer safely."));
        assert_eq!(roles, vec![Role::User, Role::Assistant]);
        assert!(!roles.contains(&Role::System));
    }

    #[test]
    fn new_conversation_is_empty_and_preserves_external_identity_and_config() {
        let id = "018f0d9c-7b6a-7c12-8f31-1234567890ac"
            .parse::<ConversationId>()
            .expect("conversation id");
        let config = ConversationConfig::new(Some("Answer safely.".to_owned()));
        let conversation = Conversation::new(id, config.clone());

        assert_eq!(conversation.id(), id);
        assert_eq!(conversation.config(), &config);
        assert!(conversation.turns().is_empty());
        assert_eq!(conversation.version(), 0);
    }
}
