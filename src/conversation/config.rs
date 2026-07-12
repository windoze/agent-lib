//! Conversation-level configuration kept outside immutable message history.

use serde::{Deserialize, Serialize};

/// Configuration shared by every turn in one Conversation.
///
/// System instructions live here rather than being synthesized as a
/// [`Role::System`](crate::model::message::Role::System) message in committed
/// history. Provider adapters can later map this value to
/// [`ChatRequest::system`](crate::client::ChatRequest::system).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationConfig {
    system: Option<String>,
}

impl ConversationConfig {
    /// Creates configuration with caller-supplied optional system instructions.
    #[must_use]
    pub const fn new(system: Option<String>) -> Self {
        Self { system }
    }

    /// Returns the system instructions without moving them into message history.
    #[must_use]
    pub fn system(&self) -> Option<&str> {
        self.system.as_deref()
    }

    /// Consumes the configuration and returns its optional system instructions.
    #[must_use]
    pub fn into_system(self) -> Option<String> {
        self.system
    }
}

#[cfg(test)]
mod tests {
    use super::ConversationConfig;
    use serde_json::json;

    #[test]
    fn configuration_round_trips_with_a_fixed_system_field() {
        for config in [
            ConversationConfig::default(),
            ConversationConfig::new(Some("Answer concisely.".to_owned())),
        ] {
            let encoded = serde_json::to_value(&config).expect("serialize config");
            assert_eq!(encoded, json!({ "system": config.system() }));

            let decoded: ConversationConfig =
                serde_json::from_value(encoded).expect("deserialize config");
            assert_eq!(decoded, config);
        }
    }

    #[test]
    fn system_instructions_are_only_exposed_as_configuration() {
        let config = ConversationConfig::new(Some("Keep this out of history.".to_owned()));

        assert_eq!(config.system(), Some("Keep this out of history."));
        assert_eq!(
            config.into_system(),
            Some("Keep this out of history.".to_owned())
        );
    }
}
