//! Successful folding tests for interleaved blocks and tool input.

use super::{start_message, stop_message};
use crate::{
    model::{content::ContentBlock, message::Role, normalized::StopReason, usage::Usage},
    stream::{BlockId, BlockKind, Delta, StreamEvent, accumulator::Accumulator},
};
use serde_json::{Map, json};

#[test]
fn folds_interleaved_blocks_and_three_tool_json_fragments_in_start_order() {
    let mut accumulator = Accumulator::new();
    let text_id = BlockId::new("text-1");
    let tool_id = BlockId::new("tool-1");
    let reasoning_id = BlockId::new("reasoning-1");
    start_message(&mut accumulator);

    for event in [
        StreamEvent::BlockStart {
            id: text_id.clone(),
            kind: BlockKind::Text,
        },
        StreamEvent::BlockStart {
            id: tool_id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "call-weather-1".to_owned(),
            },
        },
        StreamEvent::BlockDelta {
            id: text_id.clone(),
            delta: Delta::Text("Weather ".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: tool_id.clone(),
            delta: Delta::Json("{\"city\":".to_owned()),
        },
        StreamEvent::BlockStart {
            id: reasoning_id.clone(),
            kind: BlockKind::Reasoning,
        },
        StreamEvent::BlockDelta {
            id: reasoning_id.clone(),
            delta: Delta::Reasoning("Need a lookup.".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: tool_id.clone(),
            delta: Delta::Json("\"Shang".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: text_id.clone(),
            delta: Delta::Text("checked.".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: tool_id.clone(),
            delta: Delta::Json("hai\"}".to_owned()),
        },
        StreamEvent::BlockStop {
            id: reasoning_id.clone(),
        },
        StreamEvent::BlockStop {
            id: tool_id.clone(),
        },
        StreamEvent::BlockStop {
            id: text_id.clone(),
        },
        StreamEvent::Usage(Usage {
            input: 10,
            ..Usage::default()
        }),
        StreamEvent::Usage(Usage {
            output: 4,
            reasoning: 1,
            ..Usage::default()
        }),
    ] {
        accumulator.push(event).expect("fold stream event");
    }
    stop_message(&mut accumulator, StopReason::ToolUse);

    let response = accumulator.finish().expect("finish response");

    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(
        response.message.content,
        vec![
            ContentBlock::Text {
                text: "Weather checked.".to_owned(),
                extra: Map::new(),
            },
            ContentBlock::ToolUse {
                id: "call-weather-1".to_owned(),
                name: "get_weather".to_owned(),
                input: json!({ "city": "Shanghai" }),
                extra: Map::new(),
            },
            ContentBlock::Thinking {
                text: "Need a lookup.".to_owned(),
                signature: None,
                extra: Map::new(),
            },
        ]
    );
    assert_eq!(response.usage.input, 10);
    assert_eq!(response.usage.output, 4);
    assert_eq!(response.usage.reasoning, 1);
    assert_eq!(response.stop_reason.value, StopReason::ToolUse);
    assert!(response.extra.is_empty());
}

#[test]
fn tool_input_available_overrides_a_value_parsed_at_block_stop() {
    let mut accumulator = Accumulator::new();
    let id = BlockId::new("tool-1");
    start_message(&mut accumulator);
    accumulator
        .push(StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "call-1".to_owned(),
            },
        })
        .unwrap();
    accumulator
        .push(StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Json("{\"city\":\"wire-fragments\"}".to_owned()),
        })
        .unwrap();
    accumulator
        .push(StreamEvent::BlockStop { id: id.clone() })
        .unwrap();
    accumulator
        .push(StreamEvent::ToolInputAvailable {
            id,
            input: json!({ "city": "authoritative" }),
        })
        .unwrap();
    stop_message(&mut accumulator, StopReason::ToolUse);

    let response = accumulator.finish().unwrap();
    let ContentBlock::ToolUse { input, .. } = &response.message.content[0] else {
        panic!("expected tool-use content");
    };
    assert_eq!(input, &json!({ "city": "authoritative" }));
}
