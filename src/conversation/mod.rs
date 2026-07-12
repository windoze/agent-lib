//! Provider-neutral Conversation identity and immutable message foundations.
//!
//! This module layers stable identities and Conversation-level configuration
//! around complete Client [`Message`](crate::model::message::Message) values.
//! It does not change the Client wire model: system instructions remain
//! configuration, while message ids remain in [`ConversationMessage`].

pub mod config;
pub mod id;
pub mod message;
pub mod turn;

pub use config::ConversationConfig;
pub use id::{ArtifactId, ConversationId, MessageId, ToolCallId, TurnId};
pub use message::ConversationMessage;
pub use turn::{ToolPairing, Turn, TurnMeta};

#[cfg(test)]
mod tests {
    use super::{ConversationConfig, ConversationMessage, MessageId};
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
}
