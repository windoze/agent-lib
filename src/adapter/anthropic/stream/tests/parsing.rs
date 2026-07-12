//! Successful normalization and shared-accumulator folding tests.

use super::*;
use crate::model::{content::ContentBlock, normalized::StopReason};
use serde_json::{Map, Value, json};

#[tokio::test]
async fn real_text_sse_maps_events_and_matches_complete_response_shape() {
    let events = decode_fixture(REAL_TEXT_STREAM)
        .await
        .expect("decode recorded text stream");
    let id = BlockId::new("anthropic-block-0");

    assert_eq!(
        events,
        vec![
            StreamEvent::MessageStart {
                role: Role::Assistant,
            },
            StreamEvent::Usage(Usage {
                input: 22,
                output: 8,
                extra: Map::from_iter([(
                    "cache_creation".to_owned(),
                    json!({
                        "ephemeral_5m_input_tokens": 0,
                        "ephemeral_1h_input_tokens": 0
                    }),
                )]),
                ..Usage::default()
            }),
            StreamEvent::ResponseMetadata {
                extra: Map::from_iter([
                    ("model".to_owned(), json!("claude-haiku-4-5-20251001"),),
                    ("id".to_owned(), json!("msg_bdrk_recorded_text_stream")),
                    ("type".to_owned(), json!("message")),
                    ("stop_sequence".to_owned(), Value::Null),
                ]),
            },
            StreamEvent::BlockStart {
                id: id.clone(),
                kind: BlockKind::Text,
            },
            StreamEvent::BlockDelta {
                id: id.clone(),
                delta: Delta::Text("1\n2\n3\n4".to_owned()),
            },
            StreamEvent::BlockDelta {
                id: id.clone(),
                delta: Delta::Text("\n5".to_owned()),
            },
            StreamEvent::BlockStop { id },
            StreamEvent::Usage(Usage {
                output: 5,
                ..Usage::default()
            }),
            StreamEvent::ResponseMetadata {
                extra: Map::from_iter([("stop_sequence".to_owned(), Value::Null)]),
            },
            StreamEvent::ResponseMetadata {
                extra: Map::from_iter([(
                    "amazon-bedrock-invocationMetrics".to_owned(),
                    json!({
                        "inputTokenCount": 22,
                        "outputTokenCount": 13,
                        "invocationLatency": 691,
                        "firstByteLatency": 599
                    }),
                )]),
            },
            StreamEvent::MessageStop {
                stop_reason: StopReason::normalize("end_turn"),
            },
        ]
    );

    let folded = fold_events(&events).expect("fold recorded text events");
    let complete = AnthropicAdapter::parse_response(
        serde_json::to_string(&json!({
            "model": "claude-haiku-4-5-20251001",
            "id": "msg_bdrk_recorded_text_stream",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "1\n2\n3\n4\n5" }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "amazon-bedrock-invocationMetrics": {
                "inputTokenCount": 22,
                "outputTokenCount": 13,
                "invocationLatency": 691,
                "firstByteLatency": 599
            },
            "usage": {
                "input_tokens": 22,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 0,
                    "ephemeral_1h_input_tokens": 0
                },
                "output_tokens": 13
            }
        }))
        .expect("serialize complete text fixture")
        .as_bytes(),
    )
    .expect("parse equivalent complete text response");

    assert_eq!(folded, complete);
}

#[tokio::test]
async fn real_tool_sse_keeps_raw_fragments_and_publishes_complete_input_at_stop() {
    let events = decode_fixture(REAL_TOOL_STREAM)
        .await
        .expect("decode recorded tool stream");
    let id = BlockId::new("anthropic-block-0");

    assert_eq!(
        events[3],
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "toolu_bdrk_recorded_weather".to_owned(),
            },
        }
    );
    let fragments = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::BlockDelta {
                id: event_id,
                delta: Delta::Json(fragment),
            } if event_id == &id => Some(fragment.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(fragments, r#"{"city": "Tokyo"}"#);
    assert_eq!(
        events[9],
        StreamEvent::ToolInputAvailable {
            id: id.clone(),
            input: json!({ "city": "Tokyo" }),
        }
    );
    assert_eq!(events[10], StreamEvent::BlockStop { id });
    assert_eq!(
        events[11],
        StreamEvent::Usage(Usage {
            output: 28,
            ..Usage::default()
        })
    );

    let folded = fold_events(&events).expect("fold recorded tool events");
    let complete = AnthropicAdapter::parse_response(
        serde_json::to_string(&json!({
            "model": "claude-haiku-4-5-20251001",
            "id": "msg_bdrk_recorded_tool_stream",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_bdrk_recorded_weather",
                "name": "get_weather",
                "input": { "city": "Tokyo" }
            }],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "amazon-bedrock-invocationMetrics": {
                "inputTokenCount": 571,
                "outputTokenCount": 54,
                "invocationLatency": 918,
                "firstByteLatency": 741
            },
            "usage": {
                "input_tokens": 571,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 0,
                    "ephemeral_1h_input_tokens": 0
                },
                "output_tokens": 54
            }
        }))
        .expect("serialize complete tool fixture")
        .as_bytes(),
    )
    .expect("parse equivalent complete tool response");

    assert_eq!(folded, complete);
}

#[tokio::test]
async fn thinking_signature_deltas_survive_normalization_and_folding() {
    let events = decode_fixture(THINKING_STREAM)
        .await
        .expect("decode thinking stream");
    let id = BlockId::new("anthropic-block-3");

    assert!(events.contains(&StreamEvent::BlockStart {
        id: id.clone(),
        kind: BlockKind::Reasoning,
    }));
    assert!(events.contains(&StreamEvent::BlockDelta {
        id: id.clone(),
        delta: Delta::ReasoningSignature("opaque-".to_owned()),
    }));
    assert!(events.contains(&StreamEvent::BlockDelta {
        id,
        delta: Delta::ReasoningSignature("signature".to_owned()),
    }));

    let response = fold_events(&events).expect("fold thinking events");
    assert_eq!(
        response.message.content,
        vec![ContentBlock::Thinking {
            text: "Need a careful answer.".to_owned(),
            signature: Some("opaque-signature".to_owned()),
            extra: Map::new(),
        }]
    );
    assert_eq!(response.usage.input, 10);
    assert_eq!(response.usage.output, 7);
}

#[tokio::test]
async fn interleaved_provider_indices_keep_stable_ids_and_start_order() {
    let fixture = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":7,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_7\",\"name\":\"lookup\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"A\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":7,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"x\\\":\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"B\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":7,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"1}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":7}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"input_tokens\":3,\"output_tokens\":4}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    );
    let events = decode_fixture(fixture)
        .await
        .expect("decode interleaved blocks");

    assert!(events.contains(&StreamEvent::BlockDelta {
        id: BlockId::new("anthropic-block-2"),
        delta: Delta::Text("A".to_owned()),
    }));
    assert!(events.contains(&StreamEvent::BlockDelta {
        id: BlockId::new("anthropic-block-7"),
        delta: Delta::Json("{\"x\":".to_owned()),
    }));

    let response = fold_events(&events).expect("fold interleaved blocks");
    assert_eq!(
        response.message.content,
        vec![
            ContentBlock::Text {
                text: "AB".to_owned(),
                extra: Map::new(),
            },
            ContentBlock::ToolUse {
                id: "toolu_7".to_owned(),
                name: "lookup".to_owned(),
                input: json!({ "x": 1 }),
                extra: Map::new(),
            },
        ]
    );
}
