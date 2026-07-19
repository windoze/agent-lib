//! Successful Responses normalization and shared-accumulator folding tests.

use super::*;
use crate::model::normalized::{Normalized, StopReason};
use serde_json::{Map, json};

#[tokio::test]
async fn usage_events_are_additive_segments_matching_accumulated_usage() {
    let events = decode_fixture(REAL_TEXT_STREAM)
        .await
        .expect("decode recorded text stream");
    let folded = fold_events(&events).expect("fold recorded text events");
    let usage_event_count = events
        .iter()
        .filter(|event| matches!(event, StreamEvent::Usage(_)))
        .count();

    assert_eq!(
        usage_event_count, 1,
        "OpenAI fixture should exercise terminal usage emitted as one additive segment"
    );
    assert_eq!(aggregate_usage_events(&events), folded.usage);
}

#[tokio::test]
async fn recorded_text_stream_maps_stable_blocks_usage_and_azure_metadata() {
    let events = decode_fixture(REAL_TEXT_STREAM)
        .await
        .expect("decode recorded text stream");
    let reasoning_id = BlockId::new("openai-response-item-rs_recorded_text_stream");
    let text_id = BlockId::new("openai-response-item-msg_recorded_text_stream-content-0");

    assert_eq!(
        events[0],
        StreamEvent::MessageStart {
            role: Role::Assistant,
        }
    );
    assert_eq!(
        events[1],
        StreamEvent::BlockStart {
            id: reasoning_id.clone(),
            kind: BlockKind::Reasoning,
        }
    );
    assert_eq!(events[2], StreamEvent::BlockStop { id: reasoning_id });
    assert_eq!(
        events[3],
        StreamEvent::BlockStart {
            id: text_id.clone(),
            kind: BlockKind::Text,
        }
    );
    assert_eq!(
        events[4],
        StreamEvent::BlockDelta {
            id: text_id.clone(),
            delta: Delta::Text("hi".to_owned()),
        }
    );
    assert_eq!(
        events[5],
        StreamEvent::BlockDelta {
            id: text_id.clone(),
            delta: Delta::Text(" there".to_owned()),
        }
    );
    assert_eq!(events[6], StreamEvent::BlockStop { id: text_id });
    assert_eq!(
        events[7],
        StreamEvent::Usage(Usage {
            input: 12,
            output: 19,
            reasoning: 11,
            total: Some(31),
            ..Usage::default()
        })
    );
    let StreamEvent::ResponseMetadata { extra } = &events[8] else {
        panic!("terminal response metadata should be retained");
    };
    assert_eq!(extra["model"], json!("gpt-5.5"));
    assert_eq!(extra["service_tier"], json!("default"));
    assert_eq!(extra["content_filters"].as_array().unwrap().len(), 2);
    assert_eq!(
        events[9],
        StreamEvent::MessageStop {
            stop_reason: Normalized::from_mapped(StopReason::EndTurn, "completed"),
        }
    );

    let folded = fold_events(&events).expect("fold recorded text events");
    assert_eq!(folded, parse_terminal_response(REAL_TEXT_STREAM));
}

#[tokio::test]
async fn recorded_tool_stream_keeps_fragments_and_parses_only_at_done() {
    let events = decode_fixture(REAL_TOOL_STREAM)
        .await
        .expect("decode recorded tool stream");
    let id = BlockId::new("openai-response-item-fc_recorded_tool_stream");

    assert_eq!(
        events[1],
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "call_recorded_weather_stream".to_owned(),
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
    assert_eq!(fragments, r#"{"city":"Tokyo"}"#);
    assert_eq!(
        events[7],
        StreamEvent::ToolInputAvailable {
            id: id.clone(),
            input: json!({ "city": "Tokyo" }),
        }
    );
    assert_eq!(events[8], StreamEvent::BlockStop { id });

    let folded = fold_events(&events).expect("fold recorded tool events");
    assert_eq!(folded, parse_terminal_response(REAL_TOOL_STREAM));
    assert_eq!(*folded.stop_reason.value(), StopReason::ToolUse);
    assert_eq!(folded.usage.input, 53);
    assert_eq!(folded.usage.output, 18);
    assert!(folded.extra.contains_key("content_filters"));
}

#[tokio::test]
async fn empty_function_arguments_stream_parse_as_empty_object() {
    let fixture = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_empty_args\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_empty\",\"type\":\"function_call\",\"name\":\"ping\",\"call_id\":\"call_empty\",\"arguments\":\"\"},\"sequence_number\":1}\n\n",
        "event: response.function_call_arguments.done\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_empty\",\"output_index\":0,\"arguments\":\"\",\"sequence_number\":2}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"fc_empty\",\"type\":\"function_call\",\"name\":\"ping\",\"call_id\":\"call_empty\",\"arguments\":\"\"},\"sequence_number\":3}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_empty_args\",\"object\":\"response\",\"status\":\"completed\",\"output\":[{\"id\":\"fc_empty\",\"type\":\"function_call\",\"name\":\"ping\",\"call_id\":\"call_empty\",\"arguments\":\"\"}],\"usage\":{\"input_tokens\":1,\"output_tokens\":1}},\"sequence_number\":4}\n\n"
    );
    let events = decode_fixture(fixture)
        .await
        .expect("decode empty function arguments stream");
    let input = events.iter().find_map(|event| match event {
        StreamEvent::ToolInputAvailable { input, .. } => Some(input),
        _ => None,
    });

    assert_eq!(input, Some(&json!({})));
    let response = fold_events(&events).expect("fold empty function arguments stream");
    assert_eq!(
        response.message.content,
        vec![ContentBlock::ToolUse {
            id: "call_empty".to_owned(),
            name: "ping".to_owned(),
            input: json!({}),
            extra: Map::new(),
        }]
    );
}

#[tokio::test]
async fn interleaved_function_items_remain_correlated_by_item_id_and_index() {
    let fixture = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_parallel\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_a\",\"type\":\"function_call\",\"name\":\"first\",\"call_id\":\"call_a\",\"arguments\":\"\"},\"sequence_number\":1}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"id\":\"fc_b\",\"type\":\"function_call\",\"name\":\"second\",\"call_id\":\"call_b\",\"arguments\":\"\"},\"sequence_number\":2}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_a\",\"output_index\":0,\"delta\":\"{\\\"a\\\":\",\"sequence_number\":3}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_b\",\"output_index\":1,\"delta\":\"{\\\"b\\\":2}\",\"sequence_number\":4}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_a\",\"output_index\":0,\"delta\":\"1}\",\"sequence_number\":5}\n\n",
        "event: response.function_call_arguments.done\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_b\",\"output_index\":1,\"arguments\":\"{\\\"b\\\":2}\",\"sequence_number\":6}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"id\":\"fc_b\",\"type\":\"function_call\",\"name\":\"second\",\"call_id\":\"call_b\",\"arguments\":\"{\\\"b\\\":2}\"},\"sequence_number\":7}\n\n",
        "event: response.function_call_arguments.done\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_a\",\"output_index\":0,\"arguments\":\"{\\\"a\\\":1}\",\"sequence_number\":8}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"fc_a\",\"type\":\"function_call\",\"name\":\"first\",\"call_id\":\"call_a\",\"arguments\":\"{\\\"a\\\":1}\"},\"sequence_number\":9}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_parallel\",\"object\":\"response\",\"status\":\"completed\",\"output\":[{\"id\":\"fc_a\",\"type\":\"function_call\",\"name\":\"first\",\"call_id\":\"call_a\",\"arguments\":\"{\\\"a\\\":1}\"},{\"id\":\"fc_b\",\"type\":\"function_call\",\"name\":\"second\",\"call_id\":\"call_b\",\"arguments\":\"{\\\"b\\\":2}\"}],\"usage\":{\"input_tokens\":3,\"output_tokens\":4}},\"sequence_number\":10}\n\n"
    );
    let events = decode_fixture(fixture)
        .await
        .expect("decode interleaved function items");
    let response = fold_events(&events).expect("fold interleaved function items");

    assert_eq!(
        response.message.content,
        vec![
            ContentBlock::ToolUse {
                id: "call_a".to_owned(),
                name: "first".to_owned(),
                input: json!({ "a": 1 }),
                extra: Map::new(),
            },
            ContentBlock::ToolUse {
                id: "call_b".to_owned(),
                name: "second".to_owned(),
                input: json!({ "b": 2 }),
                extra: Map::new(),
            },
        ]
    );
}

#[tokio::test]
async fn reasoning_text_and_encrypted_content_fold_to_thinking_block() {
    let fixture = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_reasoning\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_reasoning\",\"type\":\"reasoning\",\"content\":[],\"summary\":[]},\"sequence_number\":1}\n\n",
        "event: response.reasoning_text.delta\n",
        "data: {\"type\":\"response.reasoning_text.delta\",\"item_id\":\"rs_reasoning\",\"output_index\":0,\"content_index\":0,\"delta\":\"Need a careful answer.\",\"sequence_number\":2}\n\n",
        "event: response.reasoning_text.done\n",
        "data: {\"type\":\"response.reasoning_text.done\",\"item_id\":\"rs_reasoning\",\"output_index\":0,\"content_index\":0,\"text\":\"Need a careful answer.\",\"sequence_number\":3}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_reasoning\",\"type\":\"reasoning\",\"content\":[{\"type\":\"reasoning_text\",\"text\":\"Need a careful answer.\"}],\"summary\":[],\"encrypted_content\":\"opaque-reasoning\"},\"sequence_number\":4}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_reasoning\",\"object\":\"response\",\"status\":\"completed\",\"output\":[{\"id\":\"rs_reasoning\",\"type\":\"reasoning\",\"content\":[{\"type\":\"reasoning_text\",\"text\":\"Need a careful answer.\"}],\"summary\":[],\"encrypted_content\":\"opaque-reasoning\"}],\"usage\":{\"input_tokens\":5,\"output_tokens\":7,\"output_tokens_details\":{\"reasoning_tokens\":7}}},\"sequence_number\":5}\n\n"
    );
    let events = decode_fixture(fixture)
        .await
        .expect("decode reasoning stream");
    let response = fold_events(&events).expect("fold reasoning stream");

    assert_eq!(
        response.message.content,
        vec![ContentBlock::Thinking {
            text: "Need a careful answer.".to_owned(),
            signature: Some("opaque-reasoning".to_owned()),
            extra: Map::new(),
        }]
    );
    assert_eq!(response.usage.reasoning, 7);
}

#[tokio::test]
async fn unknown_message_content_part_stream_folds_to_unknown_variant() {
    let fixture = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_future\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg_future\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]},\"sequence_number\":1}\n\n",
        "event: response.content_part.added\n",
        "data: {\"type\":\"response.content_part.added\",\"item_id\":\"msg_future\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"future_content\",\"payload\":{\"phase\":\"start\"}},\"sequence_number\":2}\n\n",
        "event: response.content_part.done\n",
        "data: {\"type\":\"response.content_part.done\",\"item_id\":\"msg_future\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"future_content\",\"payload\":{\"phase\":\"done\"}},\"sequence_number\":3}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"msg_future\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"future_content\",\"payload\":{\"phase\":\"done\"}}]},\"sequence_number\":4}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_future\",\"object\":\"response\",\"status\":\"completed\",\"output\":[{\"id\":\"msg_future\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"future_content\",\"payload\":{\"phase\":\"done\"}}]}],\"usage\":{\"input_tokens\":2,\"output_tokens\":1}},\"sequence_number\":5}\n\n"
    );
    let events = decode_fixture(fixture)
        .await
        .expect("decode future content stream");
    let id = BlockId::new("openai-response-item-msg_future-content-0");

    assert!(events.contains(&StreamEvent::BlockStart {
        id: id.clone(),
        kind: BlockKind::Unknown {
            type_name: Some("future_content".to_owned()),
            raw: json!({ "type": "future_content", "payload": { "phase": "start" } }),
        },
    }));
    assert!(events.contains(&StreamEvent::BlockDelta {
        id: id.clone(),
        delta: Delta::Unknown(json!({ "type": "future_content", "payload": { "phase": "done" } })),
    }));
    assert!(events.contains(&StreamEvent::BlockStop { id }));

    let response = fold_events(&events).expect("fold future content stream");
    assert_eq!(
        response.message.content,
        vec![ContentBlock::Unknown {
            type_name: Some("future_content".to_owned()),
            raw: json!({ "type": "future_content", "payload": { "phase": "done" } }),
        }]
    );
}
