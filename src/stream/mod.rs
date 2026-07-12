//! Normalized streaming event types.

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::{BlockId, BlockKind, Delta};
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
}
