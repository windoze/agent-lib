//! Provider-neutral complete request parameters.

use crate::model::{extras::ProviderExtras, message::Message, tool::Tool};
use serde::{Deserialize, Serialize};

/// A provider-neutral request for one LLM generation.
///
/// System instructions are separate from conversational messages so adapters
/// can render them using the target protocol's required representation.
/// Provider-specific fields remain bound to their provider through
/// [`ProviderExtras`] and are merged only during final wire serialization.
/// Message content must already be complete-state data: streaming deltas are
/// folded before a prior assistant message is replayed. Set `stream` to match
/// the selected [`crate::client::LlmClient`] method.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Model or deployment identifier understood by the endpoint.
    pub model: String,
    /// Ordered conversation history supplied to the model.
    pub messages: Vec<Message>,
    /// Tools the model may call during this generation.
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// Provider-neutral system instructions, kept outside `messages`.
    pub system: Option<String>,
    /// Maximum number of tokens the model may generate.
    pub max_tokens: u32,
    /// Sampling temperature, or `None` to leave the parameter unspecified.
    pub temperature: Option<f32>,
    /// Whether the endpoint should return an incremental response stream.
    ///
    /// Use `false` with [`crate::client::LlmClient::chat`] and `true` with
    /// [`crate::client::LlmClient::chat_stream`].
    pub stream: bool,
    /// Provider-specific request fields for the selected adapter, if any.
    pub provider_extras: Option<ProviderExtras>,
}

#[cfg(test)]
mod tests {
    use super::ChatRequest;
    use crate::model::{
        content::ContentBlock,
        extras::{ProviderExtras, ProviderId},
        message::{Message, Role},
        tool::Tool,
    };
    use serde_json::{Map, json};

    #[test]
    fn complete_chat_request_round_trips_without_mixing_system_and_messages() {
        let request = ChatRequest {
            model: "databricks-claude-haiku-4-5".to_owned(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "What's the weather in Shanghai?".to_owned(),
                    extra: Map::new(),
                }],
            }],
            tools: vec![Tool {
                name: "get_weather".to_owned(),
                description: "Get current weather for a city.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            }],
            system: Some("Answer concisely.".to_owned()),
            max_tokens: 1_024,
            temperature: Some(0.2),
            stream: true,
            provider_extras: Some(ProviderExtras {
                provider: ProviderId::Anthropic,
                fields: Map::from_iter([("top_k".to_owned(), json!(20))]),
            }),
        };

        let encoded = serde_json::to_value(&request).expect("serialize chat request");
        assert_eq!(encoded["system"], json!("Answer concisely."));
        assert_eq!(encoded["messages"].as_array().map(Vec::len), Some(1));
        assert_eq!(encoded["messages"][0]["role"], json!("user"));
        assert_eq!(encoded["provider_extras"]["provider"], json!("anthropic"));

        let decoded: ChatRequest =
            serde_json::from_value(encoded).expect("deserialize chat request");
        assert_eq!(decoded, request);
    }

    #[test]
    fn optional_request_parameters_can_remain_unspecified() {
        let request = ChatRequest {
            model: "gpt-5.5".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            system: None,
            max_tokens: 512,
            temperature: None,
            stream: false,
            provider_extras: None,
        };

        let encoded = serde_json::to_string(&request).expect("serialize minimal chat request");
        let decoded: ChatRequest =
            serde_json::from_str(&encoded).expect("deserialize minimal chat request");

        assert_eq!(decoded, request);
    }
}
