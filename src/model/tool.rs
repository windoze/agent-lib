//! Tool definitions, calls, and tool response data models.

use crate::model::content::ContentBlock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A tool exposed to a model, including its JSON input schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// Name used by the model when selecting the tool.
    pub name: String,
    /// Human-readable guidance describing the tool's purpose.
    pub description: String,
    /// JSON Schema describing the accepted input object.
    pub input_schema: Value,
}

/// A complete request from a model to invoke a tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned identifier used to correlate the response.
    pub id: String,
    /// Name of the selected tool.
    pub name: String,
    /// Fully parsed JSON input supplied by the model.
    pub input: Value,
    /// Provider-specific fields this crate does not model yet.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// A complete response to a prior tool call.
///
/// Conversion to and from [`ContentBlock::ToolResult`] preserves the call id,
/// multimodal content, four-state outcome, and extension metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResponse {
    /// Identifier of the tool call this response answers.
    pub tool_call_id: String,
    /// Multimodal content returned by the tool or execution boundary.
    pub content: Vec<ContentBlock>,
    /// Outcome of attempting the tool call.
    pub status: ToolStatus,
    /// Unmodeled result metadata preserved across content-block conversion.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Outcome of attempting a tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// The tool completed successfully.
    Ok,
    /// The tool ran but returned an error.
    Error,
    /// An approval or policy boundary denied the call.
    Denied,
    /// Execution was cancelled before completion.
    Cancelled,
}

/// Error returned when a non-tool-result content block is converted into a
/// [`ToolResponse`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("content block is not a tool result")]
pub struct ToolResponseConversionError {
    block: ContentBlock,
}

impl ToolResponseConversionError {
    /// Returns the unchanged block that could not be converted.
    pub fn block(&self) -> &ContentBlock {
        &self.block
    }

    /// Recovers ownership of the unchanged block that could not be converted.
    pub fn into_block(self) -> ContentBlock {
        self.block
    }
}

impl From<ToolResponse> for ContentBlock {
    /// Converts a complete tool response into its message content form without
    /// dropping status or extension metadata.
    fn from(response: ToolResponse) -> Self {
        Self::ToolResult {
            tool_use_id: response.tool_call_id,
            content: response.content,
            status: response.status,
            extra: response.extra,
        }
    }
}

impl TryFrom<ContentBlock> for ToolResponse {
    type Error = ToolResponseConversionError;

    /// Extracts a complete tool response while preserving every result field.
    fn try_from(block: ContentBlock) -> Result<Self, Self::Error> {
        match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                status,
                extra,
            } => Ok(Self {
                tool_call_id: tool_use_id,
                content,
                status,
                extra,
            }),
            block => Err(ToolResponseConversionError { block }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Tool, ToolCall, ToolResponse, ToolStatus};
    use crate::model::content::{ContentBlock, ImageSource};
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::{Map, Value, json};
    use std::fmt::Debug;

    fn empty_extra() -> Map<String, Value> {
        Map::new()
    }

    fn assert_json_round_trip<T>(value: T)
    where
        T: Debug + PartialEq + Serialize + DeserializeOwned,
    {
        let json = serde_json::to_string(&value).expect("serialize tool model");
        let decoded: T = serde_json::from_str(&json).expect("deserialize tool model");

        assert_eq!(decoded, value);
    }

    #[test]
    fn tool_schema_round_trips() {
        assert_json_round_trip(Tool {
            name: "get_weather".to_owned(),
            description: "Look up current weather for a city.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                },
                "required": ["city"]
            }),
        });
    }

    #[test]
    fn tool_call_round_trips() {
        assert_json_round_trip(ToolCall {
            id: "call_123".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
            extra: Map::new(),
        });
    }

    #[test]
    fn tool_call_preserves_provider_extra_fields() {
        let call = ToolCall {
            id: "call_123".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
            extra: Map::from_iter([("provider_call_type".to_owned(), json!("server_tool"))]),
        };

        assert_json_round_trip(call);
    }

    #[test]
    fn tool_response_round_trips_with_multimodal_content_and_each_status() {
        for status in [
            ToolStatus::Ok,
            ToolStatus::Error,
            ToolStatus::Denied,
            ToolStatus::Cancelled,
        ] {
            assert_json_round_trip(ToolResponse {
                tool_call_id: "call_123".to_owned(),
                content: vec![
                    ContentBlock::Text {
                        text: "Weather lookup result".to_owned(),
                        extra: empty_extra(),
                    },
                    ContentBlock::Image {
                        source: ImageSource::Url {
                            url: "https://example.test/weather.png".to_owned(),
                            extra: empty_extra(),
                        },
                        extra: empty_extra(),
                    },
                ],
                status,
                extra: Map::from_iter([("provider_trace".to_owned(), json!("trace-1"))]),
            });
        }
    }

    #[test]
    fn tool_response_and_result_block_convert_without_losing_any_field() {
        for status in [
            ToolStatus::Ok,
            ToolStatus::Error,
            ToolStatus::Denied,
            ToolStatus::Cancelled,
        ] {
            let response = ToolResponse {
                tool_call_id: "call_123".to_owned(),
                content: vec![ContentBlock::Text {
                    text: "result".to_owned(),
                    extra: empty_extra(),
                }],
                status,
                extra: Map::from_iter([("provider_trace".to_owned(), json!("trace-1"))]),
            };

            let block = ContentBlock::from(response.clone());
            assert_eq!(ToolResponse::try_from(block).unwrap(), response);
        }
    }

    #[test]
    fn failed_tool_response_conversion_returns_the_original_block() {
        let block = ContentBlock::Text {
            text: "not a result".to_owned(),
            extra: Map::from_iter([("provider_trace".to_owned(), json!("trace-1"))]),
        };
        let error = ToolResponse::try_from(block.clone()).expect_err("text is not a tool result");

        assert_eq!(error.block(), &block);
        assert_eq!(error.into_block(), block);
    }

    #[test]
    fn tool_status_uses_stable_wire_names() {
        assert_eq!(serde_json::to_value(ToolStatus::Ok).unwrap(), json!("ok"));
        assert_eq!(
            serde_json::to_value(ToolStatus::Error).unwrap(),
            json!("error")
        );
        assert_eq!(
            serde_json::to_value(ToolStatus::Denied).unwrap(),
            json!("denied")
        );
        assert_eq!(
            serde_json::to_value(ToolStatus::Cancelled).unwrap(),
            json!("cancelled")
        );
    }
}
