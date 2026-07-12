//! Normalized streaming event types.

use crate::model::{
    message::Role,
    normalized::{Normalized, StopReason},
    usage::Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

pub mod accumulator;

/// Stable identifier used to correlate a streaming block's start, deltas, and
/// stop event.
///
/// Provider adapters are responsible for mapping positional identifiers such
/// as Anthropic content block indices into stable block identifiers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlockId(String);

impl BlockId {
    /// Creates a block identifier from its stable string representation.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrows the stable string representation of this identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the owned stable string representation.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Kind and start metadata of a normalized streaming content block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// A block containing assistant-visible text.
    Text,
    /// A block containing model reasoning or thinking text.
    Reasoning,
    /// A block containing the streamed JSON input for a tool invocation.
    ToolInput {
        /// Name of the tool selected by the model.
        tool_name: String,
        /// Provider-assigned identifier used to correlate the tool response.
        tool_call_id: String,
    },
}

/// Incremental payload emitted for a streaming content block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delta {
    /// Assistant-visible text appended to a text block.
    Text(String),
    /// Raw JSON text appended to a tool-input block.
    ///
    /// A JSON delta can contain an incomplete token or document and must never
    /// be parsed as it arrives. Accumulate every fragment for the block first,
    /// then parse the completed string at `ToolInputAvailable` or block stop.
    Json(String),
    /// Model reasoning text appended to a reasoning block.
    Reasoning(String),
}

/// A provider-neutral event emitted while an LLM response is streaming.
///
/// This taxonomy follows Vercel AI SDK v5 stream parts while remaining an
/// internal Client-layer model rather than adopting Vercel's SSE transport
/// encoding. Agent-layer events such as approval, abort, and pivot are
/// intentionally excluded because they do not originate from the LLM wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEvent {
    /// Starts an assistant message.
    ///
    /// Corresponds to Vercel v5's `start` message-control part, with the
    /// provider role retained in normalized form.
    MessageStart {
        /// Role reported for the message being streamed.
        role: Role,
    },
    /// Starts a text, reasoning, or tool-input block identified by `id`.
    ///
    /// Corresponds to Vercel v5's `text-start`, `reasoning-start`, or
    /// `tool-input-start` part according to `kind`.
    BlockStart {
        /// Stable identifier shared by all events for this block.
        id: BlockId,
        /// Block category and its start metadata.
        kind: BlockKind,
    },
    /// Appends one raw delta to the block identified by `id`.
    ///
    /// Corresponds to Vercel v5's `text-delta`, `reasoning-delta`, or
    /// `tool-input-delta` part according to `delta`.
    BlockDelta {
        /// Stable identifier of the block receiving this delta.
        id: BlockId,
        /// Incremental text, reasoning, or raw JSON payload.
        delta: Delta,
    },
    /// Closes the block identified by `id`.
    ///
    /// Corresponds to Vercel v5's `text-end` and `reasoning-end` parts, and
    /// supplies the equivalent terminal boundary for provider tool-input
    /// blocks.
    BlockStop {
        /// Stable identifier of the completed block.
        id: BlockId,
    },
    /// Publishes parsed tool input after all raw JSON deltas are accumulated.
    ///
    /// Corresponds directly to Vercel v5's `tool-input-available` part.
    ToolInputAvailable {
        /// Stable identifier of the tool-input block.
        id: BlockId,
        /// Complete parsed JSON input for the tool invocation.
        input: Value,
    },
    /// Reports an intermediate or final token-usage update.
    ///
    /// Corresponds to usage carried by Vercel v5's `finish-step` and `finish`
    /// message-control parts.
    Usage(Usage),
    /// Ends the streamed assistant message with its normalized stop reason.
    ///
    /// Corresponds to Vercel v5's `finish` message-control part.
    MessageStop {
        /// Provider-neutral reason for ending, with the raw value retained.
        stop_reason: Normalized<StopReason>,
    },
    /// Reports a provider or protocol error observed in the stream.
    ///
    /// Corresponds directly to Vercel v5's `error` part. The string payload is
    /// temporary until M3-1 introduces the classified `ClientError` type.
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::{BlockId, BlockKind, Delta, StreamEvent};
    use crate::model::{
        message::Role,
        normalized::{Normalized, StopReason},
        usage::Usage,
    };
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::json;
    use std::fmt::Debug;

    fn assert_json_round_trip<T>(value: T)
    where
        T: Debug + PartialEq + Serialize + DeserializeOwned,
    {
        let encoded = serde_json::to_value(&value).expect("serialize streaming type");
        let decoded: T = serde_json::from_value(encoded).expect("deserialize streaming type");

        assert_eq!(decoded, value);
    }

    #[test]
    fn block_id_round_trips_as_a_transparent_string() {
        let id = BlockId::new("anthropic-block-3");
        let encoded = serde_json::to_value(&id).expect("serialize block id");

        assert_eq!(encoded, json!("anthropic-block-3"));
        assert_eq!(id.as_str(), "anthropic-block-3");
        assert_json_round_trip(id);
    }

    #[test]
    fn every_block_kind_round_trips() {
        for kind in [
            BlockKind::Text,
            BlockKind::Reasoning,
            BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "call_weather_1".to_owned(),
            },
        ] {
            assert_json_round_trip(kind);
        }
    }

    #[test]
    fn block_kind_uses_stable_snake_case_wire_names() {
        assert_eq!(
            serde_json::to_value(BlockKind::Text).unwrap(),
            json!("text")
        );
        assert_eq!(
            serde_json::to_value(BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "call_weather_1".to_owned(),
            })
            .unwrap(),
            json!({
                "tool_input": {
                    "tool_name": "get_weather",
                    "tool_call_id": "call_weather_1"
                }
            })
        );
    }

    #[test]
    fn every_delta_round_trips() {
        for delta in [
            Delta::Text("hello".to_owned()),
            Delta::Json("{\"city\":\"Shang".to_owned()),
            Delta::Reasoning("considering weather data".to_owned()),
        ] {
            assert_json_round_trip(delta);
        }
    }

    #[test]
    fn delta_uses_stable_snake_case_wire_names() {
        assert_eq!(
            serde_json::to_value(Delta::Json("{\"city\":\"Shang".to_owned())).unwrap(),
            json!({ "json": "{\"city\":\"Shang" })
        );
    }

    #[test]
    fn every_stream_event_round_trips() {
        let text_id = BlockId::new("text-1");
        let tool_id = BlockId::new("tool-1");
        let events = [
            StreamEvent::MessageStart {
                role: Role::Assistant,
            },
            StreamEvent::BlockStart {
                id: text_id.clone(),
                kind: BlockKind::Text,
            },
            StreamEvent::BlockDelta {
                id: text_id.clone(),
                delta: Delta::Text("hello".to_owned()),
            },
            StreamEvent::BlockStop { id: text_id },
            StreamEvent::ToolInputAvailable {
                id: tool_id,
                input: json!({ "city": "Shanghai" }),
            },
            StreamEvent::Usage(Usage {
                input: 12,
                output: 5,
                ..Usage::default()
            }),
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::EndTurn, "end_turn"),
            },
            StreamEvent::Error("provider stream disconnected".to_owned()),
        ];

        for event in events {
            assert_json_round_trip(event);
        }
    }

    #[test]
    fn stream_event_uses_stable_snake_case_wire_names() {
        assert_eq!(
            serde_json::to_value(StreamEvent::BlockStop {
                id: BlockId::new("text-1"),
            })
            .unwrap(),
            json!({ "block_stop": { "id": "text-1" } })
        );
        assert_eq!(
            serde_json::to_value(StreamEvent::Error("bad event".to_owned())).unwrap(),
            json!({ "error": "bad event" })
        );
    }
}
