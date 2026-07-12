//! Complete-state content block types for text, tools, reasoning, and media.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A complete provider-neutral message content block.
///
/// Streaming adapters should fold deltas into this shape only after a block is
/// complete. Unknown provider fields are retained in each variant's `extra`
/// map for forward compatibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain model- or user-authored text.
    Text {
        /// The text payload.
        text: String,
        /// Provider-specific fields this crate does not model yet.
        #[serde(default, skip_serializing_if = "Map::is_empty", flatten)]
        extra: Map<String, Value>,
    },
    /// An image supplied either by URL or by inline base64 data.
    Image {
        /// The image location or inline bytes.
        source: ImageSource,
        /// Provider-specific fields this crate does not model yet.
        #[serde(default, skip_serializing_if = "Map::is_empty", flatten)]
        extra: Map<String, Value>,
    },
    /// A request from the model to call a tool.
    ToolUse {
        /// Provider-assigned identifier for this tool call.
        id: String,
        /// Tool name selected by the model.
        name: String,
        /// Fully parsed tool input JSON.
        #[serde(default)]
        input: Value,
        /// Provider-specific fields this crate does not model yet.
        #[serde(default, skip_serializing_if = "Map::is_empty", flatten)]
        extra: Map<String, Value>,
    },
    /// The result returned for a prior tool call.
    ToolResult {
        /// Identifier of the tool call this result answers.
        tool_use_id: String,
        /// Multimodal result content.
        #[serde(default)]
        content: Vec<ContentBlock>,
        /// Whether the tool execution failed.
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
        /// Provider-specific fields this crate does not model yet.
        #[serde(default, skip_serializing_if = "Map::is_empty", flatten)]
        extra: Map<String, Value>,
    },
    /// Model thinking or reasoning text.
    Thinking {
        /// Thinking/reasoning text.
        text: String,
        /// Provider signature proving or validating the thinking block.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Provider-specific fields this crate does not model yet.
        #[serde(default, skip_serializing_if = "Map::is_empty", flatten)]
        extra: Map<String, Value>,
    },
}

/// Image data carried by a content block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Image referenced by URL.
    Url {
        /// The image URL.
        url: String,
        /// Provider-specific source fields this crate does not model yet.
        #[serde(default, skip_serializing_if = "Map::is_empty", flatten)]
        extra: Map<String, Value>,
    },
    /// Image embedded as base64-encoded bytes.
    Base64 {
        /// MIME media type for the encoded image data.
        media_type: String,
        /// Base64-encoded image data.
        data: String,
        /// Provider-specific source fields this crate does not model yet.
        #[serde(default, skip_serializing_if = "Map::is_empty", flatten)]
        extra: Map<String, Value>,
    },
}

/// Lets serde omit the default successful tool-result error flag.
fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::{ContentBlock, ImageSource};
    use serde_json::{Map, Value, json};

    fn empty_extra() -> Map<String, Value> {
        Map::new()
    }

    fn assert_content_block_round_trip(block: ContentBlock) {
        let json = serde_json::to_string(&block).expect("serialize content block");
        let decoded: ContentBlock = serde_json::from_str(&json).expect("deserialize content block");

        assert_eq!(decoded, block);
    }

    #[test]
    fn text_block_round_trips() {
        assert_content_block_round_trip(ContentBlock::Text {
            text: "hello".to_owned(),
            extra: empty_extra(),
        });
    }

    #[test]
    fn image_block_with_url_source_round_trips() {
        assert_content_block_round_trip(ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.test/cat.png".to_owned(),
                extra: empty_extra(),
            },
            extra: empty_extra(),
        });
    }

    #[test]
    fn image_block_with_base64_source_round_trips() {
        assert_content_block_round_trip(ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/png".to_owned(),
                data: "iVBORw0KGgo=".to_owned(),
                extra: empty_extra(),
            },
            extra: empty_extra(),
        });
    }

    #[test]
    fn tool_use_block_round_trips() {
        assert_content_block_round_trip(ContentBlock::ToolUse {
            id: "toolu_01".to_owned(),
            name: "lookup_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
            extra: empty_extra(),
        });
    }

    #[test]
    fn tool_result_block_round_trips_with_multimodal_content() {
        assert_content_block_round_trip(ContentBlock::ToolResult {
            tool_use_id: "toolu_01".to_owned(),
            content: vec![
                ContentBlock::Text {
                    text: "sunny".to_owned(),
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
            is_error: false,
            extra: empty_extra(),
        });
    }

    #[test]
    fn thinking_block_round_trips_with_signature() {
        assert_content_block_round_trip(ContentBlock::Thinking {
            text: "Need to call a tool.".to_owned(),
            signature: Some("sig-123".to_owned()),
            extra: empty_extra(),
        });
    }

    #[test]
    fn deserializes_anthropic_text_and_tool_use_content_array() {
        let blocks: Vec<ContentBlock> = serde_json::from_value(json!([
            {
                "type": "text",
                "text": "I'll check that."
            },
            {
                "type": "tool_use",
                "id": "toolu_01ABC",
                "name": "get_weather",
                "input": {
                    "location": "Shanghai"
                }
            }
        ]))
        .expect("deserialize anthropic content array");

        assert_eq!(
            blocks,
            vec![
                ContentBlock::Text {
                    text: "I'll check that.".to_owned(),
                    extra: empty_extra(),
                },
                ContentBlock::ToolUse {
                    id: "toolu_01ABC".to_owned(),
                    name: "get_weather".to_owned(),
                    input: json!({ "location": "Shanghai" }),
                    extra: empty_extra(),
                }
            ]
        );
    }

    #[test]
    fn preserves_unknown_provider_fields_in_extra_maps() {
        let block: ContentBlock = serde_json::from_value(json!({
            "type": "image",
            "source": {
                "type": "url",
                "url": "https://example.test/chart.png",
                "cache_control": { "type": "ephemeral" }
            },
            "provider_tag": "kept"
        }))
        .expect("deserialize image with unknown fields");

        let ContentBlock::Image { source, extra } = block else {
            panic!("expected image block");
        };
        assert_eq!(extra.get("provider_tag"), Some(&json!("kept")));

        let ImageSource::Url { extra, .. } = source else {
            panic!("expected url source");
        };
        assert_eq!(
            extra.get("cache_control"),
            Some(&json!({ "type": "ephemeral" }))
        );
    }
}
