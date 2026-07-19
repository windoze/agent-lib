//! Message and role types shared by provider adapters.

use crate::model::content::ContentBlock;
use serde::{Deserialize, Serialize};

/// A complete provider-neutral chat message.
///
/// Message identity intentionally belongs to the Conversation layer. The
/// Client layer keeps only the role and complete content needed for provider
/// wire conversion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// The participant that authored the message.
    pub role: Role,
    /// Ordered complete-state content blocks carried by the message.
    pub content: Vec<ContentBlock>,
}

/// Provider-neutral chat participant roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A message authored by the end user.
    User,
    /// A message authored by the assistant model.
    Assistant,
    /// System-level instructions for the model.
    System,
    /// Tool output or tool-related content.
    Tool,
}

#[cfg(test)]
mod tests {
    use super::{Message, Role};
    use crate::model::{
        content::ContentBlock,
        tool::{ToolCall, ToolResponse, ToolStatus},
    };
    use serde_json::{Map, Value, json};

    fn empty_extra() -> Map<String, Value> {
        Map::new()
    }

    #[test]
    fn every_role_round_trips_through_its_lowercase_wire_name() {
        for (role, wire_name) in [
            (Role::User, "user"),
            (Role::Assistant, "assistant"),
            (Role::System, "system"),
            (Role::Tool, "tool"),
        ] {
            let json = serde_json::to_string(&role).expect("serialize role");
            assert_eq!(json, format!("\"{wire_name}\""));

            let decoded: Role = serde_json::from_str(&json).expect("deserialize role");
            assert_eq!(decoded, role);
        }
    }

    #[test]
    fn tool_message_sequence_preserves_call_and_response_structure() {
        let call = ToolCall {
            id: "toolu_weather_1".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
            extra: empty_extra(),
        };
        let response = ToolResponse {
            tool_call_id: call.id.clone(),
            content: vec![ContentBlock::Text {
                text: "Sunny, 28 C".to_owned(),
                extra: empty_extra(),
            }],
            status: ToolStatus::Ok,
            extra: empty_extra(),
        };
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                    extra: empty_extra(),
                }],
            },
            Message {
                role: Role::Tool,
                content: vec![response.clone().into()],
            },
        ];

        let json = serde_json::to_string(&messages).expect("serialize message sequence");
        let decoded: Vec<Message> =
            serde_json::from_str(&json).expect("deserialize message sequence");

        assert_eq!(decoded, messages);
        assert_eq!(decoded[0].role, Role::Assistant);
        assert_eq!(decoded[1].role, Role::Tool);
        let ContentBlock::ToolUse {
            id, name, input, ..
        } = &decoded[0].content[0]
        else {
            panic!("expected assistant tool-use content");
        };
        assert_eq!(id, &response.tool_call_id);
        assert_eq!(name, "get_weather");
        assert_eq!(input, &json!({ "city": "Shanghai" }));
        let ContentBlock::ToolResult { tool_use_id, .. } = &decoded[1].content[0] else {
            panic!("expected tool-result content");
        };
        assert_eq!(tool_use_id, id);
    }
}
