//! Complete provider-neutral LLM response data.

use crate::model::{
    message::Message,
    normalized::{Normalized, StopReason},
    usage::Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A complete provider-neutral response returned by an LLM client.
///
/// Provider-specific response fields that have not been modeled are retained
/// in `extra` so non-streaming adapters can preserve their wire evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// Complete assistant message produced by the model.
    pub message: Message,
    /// Token usage reported for the response.
    pub usage: Usage,
    /// Normalized reason why generation stopped, including the raw wire value.
    pub stop_reason: Normalized<StopReason>,
    /// Provider-specific response fields this crate does not model yet.
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::Response;
    use crate::model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::{Normalized, StopReason},
        usage::Usage,
    };
    use serde_json::{Map, json};

    #[test]
    fn response_round_trips_and_preserves_unknown_fields() {
        let response: Response = serde_json::from_value(json!({
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "hello" }]
            },
            "usage": { "input": 4, "output": 1 },
            "stop_reason": { "value": "end_turn", "raw": "end_turn" },
            "provider_trace_id": "trace-1"
        }))
        .expect("deserialize response");

        assert_eq!(
            response,
            Response {
                message: Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "hello".to_owned(),
                        extra: Map::new(),
                    }],
                },
                usage: Usage {
                    input: 4,
                    output: 1,
                    ..Usage::default()
                },
                stop_reason: Normalized::from_mapped(StopReason::EndTurn, "end_turn"),
                extra: [("provider_trace_id".to_owned(), json!("trace-1"))]
                    .into_iter()
                    .collect(),
            }
        );

        let encoded = serde_json::to_value(&response).expect("serialize response");
        let decoded: Response = serde_json::from_value(encoded).expect("round-trip response");
        assert_eq!(decoded, response);
    }
}
