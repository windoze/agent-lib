use super::{FrozenMessage, PendingMessage};
use crate::{
    client::Response,
    conversation::MessageId,
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::{Normalized, StopReason},
        usage::Usage,
    },
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use serde_json::{Map, json};

mod errors;
mod success;

const MESSAGE_ID: &str = "018f0d9c-7b6a-7c12-8f31-123456789201";

fn message_id() -> MessageId {
    MESSAGE_ID.parse().expect("valid message id")
}

fn stop_reason(reason: StopReason) -> Normalized<StopReason> {
    let raw = match reason {
        StopReason::ToolUse => "tool_use",
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::Refusal => "refusal",
        StopReason::Other => "provider_specific",
    };
    Normalized::from_mapped(reason, raw)
}

fn message_start(role: Role) -> StreamEvent {
    StreamEvent::MessageStart { role }
}

fn message_stop(reason: StopReason) -> StreamEvent {
    StreamEvent::MessageStop {
        stop_reason: stop_reason(reason),
    }
}

fn complete_response() -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Weather checked.".to_owned(),
                    extra: Map::new(),
                },
                ContentBlock::Thinking {
                    text: "Need a lookup.".to_owned(),
                    signature: Some("opaque-signature".to_owned()),
                    extra: Map::new(),
                },
                ContentBlock::ToolUse {
                    id: "call-weather-1".to_owned(),
                    name: "get_weather".to_owned(),
                    input: json!({ "city": "Shanghai" }),
                    extra: Map::new(),
                },
            ],
        },
        usage: Usage {
            input: 10,
            output: 4,
            reasoning: 1,
            ..Usage::default()
        },
        stop_reason: stop_reason(StopReason::ToolUse),
        extra: Map::from_iter([
            ("request_id".to_owned(), json!("req-pending-1")),
            ("provider_latency_ms".to_owned(), json!(42)),
        ]),
    }
}

fn complete_stream_events() -> Vec<StreamEvent> {
    let text_id = BlockId::new("text-1");
    let reasoning_id = BlockId::new("reasoning-1");
    let tool_id = BlockId::new("tool-1");

    vec![
        message_start(Role::Assistant),
        StreamEvent::BlockStart {
            id: text_id.clone(),
            kind: BlockKind::Text,
        },
        StreamEvent::BlockStart {
            id: reasoning_id.clone(),
            kind: BlockKind::Reasoning,
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
            delta: Delta::Json("{\"city\":\"Shang".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: reasoning_id.clone(),
            delta: Delta::Reasoning("Need a lookup.".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: reasoning_id.clone(),
            delta: Delta::ReasoningSignature("opaque-".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: text_id.clone(),
            delta: Delta::Text("checked.".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: tool_id.clone(),
            delta: Delta::Json("hai\"}".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: reasoning_id.clone(),
            delta: Delta::ReasoningSignature("signature".to_owned()),
        },
        StreamEvent::BlockStop {
            id: tool_id.clone(),
        },
        StreamEvent::BlockStop {
            id: text_id.clone(),
        },
        StreamEvent::BlockStop { id: reasoning_id },
        StreamEvent::Usage(Usage {
            input: 10,
            ..Usage::default()
        }),
        StreamEvent::Usage(Usage {
            output: 4,
            reasoning: 1,
            ..Usage::default()
        }),
        StreamEvent::ResponseMetadata {
            extra: Map::from_iter([
                ("request_id".to_owned(), json!("req-pending-1")),
                ("provider_latency_ms".to_owned(), json!(42)),
            ]),
        },
        message_stop(StopReason::ToolUse),
    ]
}

fn finish_empty_assistant(pending: &mut PendingMessage) -> FrozenMessage {
    pending
        .push(message_start(Role::Assistant))
        .expect("start assistant message");
    pending
        .push(message_stop(StopReason::EndTurn))
        .expect("stop assistant message");
    pending.finish(message_id()).expect("freeze message")
}
