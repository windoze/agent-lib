//! Provider-neutral Conversation identity and immutable committed history.
//!
//! This module layers stable identities and Conversation-level configuration
//! around complete Client [`Message`](crate::model::message::Message) values.
//! It does not change the Client wire model: system instructions remain
//! configuration, while message ids remain in [`ConversationMessage`]. Closed
//! turns can enter history only through the crate-private validated commit path
//! used by the pending layer.

pub mod config;
pub mod error;
pub mod id;
pub mod message;
pub mod turn;
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "M1 establishes the crate-private validator consumed by the M2 pending API"
    )
)]
mod validation;

pub use config::ConversationConfig;
pub use error::{CommitError, ContentBlockKind, ConversationError, PairingMessageKind};
pub use id::{ArtifactId, ConversationId, MessageId, ToolCallId, TurnId};
pub use message::ConversationMessage;
pub use turn::{ToolPairing, Turn, TurnMeta};

use turn::TurnData;

/// One provider-neutral conversation with immutable committed turns.
///
/// A new value is empty and receives all identity and configuration from the
/// caller. Public code can inspect committed state but cannot push raw turns;
/// the future pending API will use the same crate-private atomic commit gate.
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    id: ConversationId,
    config: ConversationConfig,
    turns: Vec<Turn>,
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

    /// Returns the monotonic version advanced by each successful commit.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Validates a complete draft and advances history/version as one operation.
    ///
    /// Every fallible precondition is checked before either field is changed.
    /// This remains crate-private so M2 pending transitions, rather than raw
    /// containers, become the public way to build committed history.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the crate-private M1 commit gate is first called by the M2 pending API"
        )
    )]
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
