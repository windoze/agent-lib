//! Immutable Conversation envelope around a complete Client message.
//!
//! The envelope exposes its payload only through a shared reference. Mutating
//! a frozen payload in place therefore fails at compile time:
//!
//! ```compile_fail
//! use agent_lib::{
//!     conversation::{ConversationMessage, MessageId},
//!     model::message::{Message, Role},
//! };
//!
//! let id: MessageId = "018f0d9c-7b6a-7c12-8f31-1234567890ad".parse().unwrap();
//! let envelope = ConversationMessage::new(
//!     id,
//!     Message { role: Role::User, content: Vec::new() },
//! );
//! envelope.payload().role = Role::Assistant;
//! ```

use crate::{conversation::id::MessageId, model::message::Message};
use serde::{Deserialize, Serialize};

/// A stable identity paired with one complete, immutable Client message.
///
/// This type does not alter the provider-neutral [`Message`] payload. It adds
/// Conversation identity in a separate layer and intentionally provides no
/// mutable getter, mutable dereference, or in-place replacement operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationMessage {
    id: MessageId,
    payload: Message,
}

impl ConversationMessage {
    /// Freezes a complete Client message under an externally supplied id.
    #[must_use]
    pub const fn new(id: MessageId, payload: Message) -> Self {
        Self { id, payload }
    }

    /// Returns this message's stable Conversation identity.
    #[must_use]
    pub const fn id(&self) -> MessageId {
        self.id
    }

    /// Returns a shared view of the complete Client payload.
    #[must_use]
    pub const fn payload(&self) -> &Message {
        &self.payload
    }

    /// Consumes the envelope and separates its id from its Client payload.
    #[must_use]
    pub fn into_parts(self) -> (MessageId, Message) {
        (self.id, self.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::ConversationMessage;
    use crate::{
        conversation::id::MessageId,
        model::{content::ContentBlock, message::Message, message::Role},
    };
    use serde_json::{Map, Value, json};

    const MESSAGE_ID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890ad";

    fn user_payload() -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_owned(),
                extra: Map::<String, Value>::new(),
            }],
        }
    }

    #[test]
    fn envelope_round_trips_without_changing_the_client_payload() {
        let id = MESSAGE_ID.parse::<MessageId>().expect("message id");
        let payload = user_payload();
        let envelope = ConversationMessage::new(id, payload.clone());

        let encoded = serde_json::to_value(&envelope).expect("serialize envelope");
        assert_eq!(
            encoded,
            json!({
                "id": MESSAGE_ID,
                "payload": {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "Hello"
                    }]
                }
            })
        );

        let decoded: ConversationMessage =
            serde_json::from_value(encoded).expect("deserialize envelope");
        assert_eq!(decoded, envelope);
        assert_eq!(decoded.id(), id);
        assert_eq!(decoded.payload(), &payload);
    }

    #[test]
    fn payload_access_is_shared_and_parts_require_consuming_the_envelope() {
        fn accepts_only_a_shared_payload(_: &Message) {}

        let id = MESSAGE_ID.parse::<MessageId>().expect("message id");
        let envelope = ConversationMessage::new(id, user_payload());
        accepts_only_a_shared_payload(envelope.payload());

        let (split_id, split_payload) = envelope.into_parts();
        assert_eq!(split_id, id);
        assert_eq!(split_payload, user_payload());
    }

    #[test]
    fn client_message_remains_constructible_without_a_conversation_id() {
        let payload = user_payload();
        let encoded = serde_json::to_value(&payload).expect("serialize client message");

        assert_eq!(encoded["role"], json!("user"));
        assert!(encoded.get("content").is_some());
        assert!(encoded.get("id").is_none());
    }
}
