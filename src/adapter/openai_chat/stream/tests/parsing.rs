//! Fixture-driven folding and streaming/non-streaming consistency tests.
//!
//! Each recorded `.sse` fixture is split across UTF-8 and SSE-line boundaries
//! (the repeating uneven pattern in [`super::irregular_chunks`]) and fed through
//! the full `normalize_sse` pipeline. The decoded [`StreamEvent`] sequence is
//! asserted exactly, then folded through the shared accumulator and compared
//! against the non-streaming `parse_response` of the paired `.json` body that
//! represents the same semantic response (design doc §7.1).

use super::*;
use crate::adapter::openai_chat::OpenAiChatAdapter;
use crate::model::usage::Usage;

/// Sanitized non-streaming bodies paired with the `.sse` fixtures above.
const REAL_TEXT_RESPONSE: &str = include_str!("fixtures/text_stream.json");
const REAL_TOOL_RESPONSE: &str = include_str!("fixtures/tool_stream.json");
const REAL_REASONING_RESPONSE: &str = include_str!("fixtures/reasoning_stream.json");
const REAL_USAGE_TERMINAL_RESPONSE: &str = include_str!("fixtures/usage_terminal.json");

/// Aggregates `Usage` events exactly as a direct stream consumer must.
fn aggregate_usage_events(events: &[StreamEvent]) -> Usage {
    let mut usage = Usage::default();
    for event in events {
        if let StreamEvent::Usage(segment) = event {
            usage.merge(segment.clone());
        }
    }
    usage
}

#[tokio::test]
async fn text_stream_matches_exact_events_and_non_streaming_response() {
    let events = decode_fixture(REAL_TEXT_STREAM)
        .await
        .expect("text stream decodes");
    let text = BlockId::new("text");
    assert_eq!(
        events,
        vec![
            StreamEvent::MessageStart {
                role: Role::Assistant,
            },
            StreamEvent::BlockStart {
                id: text.clone(),
                kind: BlockKind::Text,
            },
            StreamEvent::BlockDelta {
                id: text.clone(),
                delta: Delta::Text("Hello".to_owned()),
            },
            StreamEvent::BlockDelta {
                id: text.clone(),
                delta: Delta::Text(" world".to_owned()),
            },
            StreamEvent::Usage(Usage {
                input: 13,
                output: 26,
                total: Some(39),
                cache_read: 4,
                ..Usage::default()
            }),
            StreamEvent::BlockStop { id: text },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::EndTurn, "stop"),
            },
        ]
    );

    let folded = fold_events(&events).expect("fold text stream");
    let parsed = OpenAiChatAdapter::parse_response(REAL_TEXT_RESPONSE.as_bytes())
        .expect("parse paired text response");
    assert_eq!(comparable(folded), comparable(parsed));
}

#[tokio::test]
async fn tool_stream_matches_exact_events_and_non_streaming_response() {
    let events = decode_fixture(REAL_TOOL_STREAM)
        .await
        .expect("tool stream decodes");
    let tool0 = BlockId::new("tool-call-0");
    let tool1 = BlockId::new("tool-call-1");
    assert_eq!(
        events,
        vec![
            StreamEvent::MessageStart {
                role: Role::Assistant,
            },
            StreamEvent::BlockStart {
                id: tool0.clone(),
                kind: BlockKind::ToolInput {
                    tool_name: "first".to_owned(),
                    tool_call_id: "call_demo_a".to_owned(),
                },
            },
            StreamEvent::BlockStart {
                id: tool1.clone(),
                kind: BlockKind::ToolInput {
                    tool_name: "second".to_owned(),
                    tool_call_id: "call_demo_b".to_owned(),
                },
            },
            StreamEvent::BlockDelta {
                id: tool0.clone(),
                delta: Delta::Json(r#"{"a":"#.to_owned()),
            },
            StreamEvent::BlockDelta {
                id: tool1.clone(),
                delta: Delta::Json(r#"{"b":"#.to_owned()),
            },
            StreamEvent::BlockDelta {
                id: tool0.clone(),
                delta: Delta::Json("1}".to_owned()),
            },
            StreamEvent::BlockDelta {
                id: tool1.clone(),
                delta: Delta::Json("2}".to_owned()),
            },
            StreamEvent::Usage(Usage {
                input: 53,
                output: 18,
                total: Some(71),
                ..Usage::default()
            }),
            StreamEvent::BlockStop { id: tool0 },
            StreamEvent::BlockStop { id: tool1 },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::ToolUse, "tool_calls"),
            },
        ]
    );

    let folded = fold_events(&events).expect("fold tool stream");
    let parsed = OpenAiChatAdapter::parse_response(REAL_TOOL_RESPONSE.as_bytes())
        .expect("parse paired tool response");
    assert_eq!(comparable(folded), comparable(parsed));
}

#[tokio::test]
async fn reasoning_stream_matches_exact_events_and_non_streaming_response() {
    let events = decode_fixture(REAL_REASONING_STREAM)
        .await
        .expect("reasoning stream decodes");
    let reasoning = BlockId::new("reasoning");
    let text = BlockId::new("text");
    assert_eq!(
        events,
        vec![
            StreamEvent::MessageStart {
                role: Role::Assistant,
            },
            StreamEvent::BlockStart {
                id: reasoning.clone(),
                kind: BlockKind::Reasoning,
            },
            StreamEvent::BlockDelta {
                id: reasoning.clone(),
                delta: Delta::Reasoning("Let me think".to_owned()),
            },
            StreamEvent::BlockDelta {
                id: reasoning.clone(),
                delta: Delta::Reasoning(" step by step.".to_owned()),
            },
            StreamEvent::BlockStart {
                id: text.clone(),
                kind: BlockKind::Text,
            },
            StreamEvent::BlockDelta {
                id: text.clone(),
                delta: Delta::Text("The answer is 42.".to_owned()),
            },
            StreamEvent::Usage(Usage {
                input: 30,
                output: 50,
                total: Some(80),
                cache_read: 6,
                reasoning: 35,
                ..Usage::default()
            }),
            StreamEvent::BlockStop { id: reasoning },
            StreamEvent::BlockStop { id: text },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::EndTurn, "stop"),
            },
        ]
    );

    let folded = fold_events(&events).expect("fold reasoning stream");
    let parsed = OpenAiChatAdapter::parse_response(REAL_REASONING_RESPONSE.as_bytes())
        .expect("parse paired reasoning response");
    assert_eq!(comparable(folded), comparable(parsed));
}

#[tokio::test]
async fn usage_terminal_stream_matches_exact_events_and_non_streaming_response() {
    let events = decode_fixture(REAL_USAGE_TERMINAL_STREAM)
        .await
        .expect("usage-terminal stream decodes");
    let text = BlockId::new("text");
    assert_eq!(
        events,
        vec![
            StreamEvent::MessageStart {
                role: Role::Assistant,
            },
            StreamEvent::BlockStart {
                id: text.clone(),
                kind: BlockKind::Text,
            },
            StreamEvent::BlockDelta {
                id: text.clone(),
                delta: Delta::Text("Truncated".to_owned()),
            },
            StreamEvent::BlockDelta {
                id: text.clone(),
                delta: Delta::Text(" answer".to_owned()),
            },
            StreamEvent::Usage(Usage {
                input: 20,
                output: 64,
                total: Some(84),
                cache_read: 2,
                ..Usage::default()
            }),
            StreamEvent::BlockStop { id: text },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::MaxTokens, "length"),
            },
        ]
    );

    let folded = fold_events(&events).expect("fold usage-terminal stream");
    let parsed = OpenAiChatAdapter::parse_response(REAL_USAGE_TERMINAL_RESPONSE.as_bytes())
        .expect("parse paired usage-terminal response");
    assert_eq!(comparable(folded), comparable(parsed));
}

/// Each fixture emits exactly one additive usage segment whose aggregate equals
/// the non-streaming response usage (design doc §4.4.4 / §7.1).
#[tokio::test]
async fn usage_events_are_single_additive_segments_matching_non_streaming_usage() {
    for (stream, json) in [
        (REAL_TEXT_STREAM, REAL_TEXT_RESPONSE),
        (REAL_TOOL_STREAM, REAL_TOOL_RESPONSE),
        (REAL_REASONING_STREAM, REAL_REASONING_RESPONSE),
        (REAL_USAGE_TERMINAL_STREAM, REAL_USAGE_TERMINAL_RESPONSE),
    ] {
        let events = decode_fixture(stream)
            .await
            .expect("fixture stream decodes");
        let usage_segments = events
            .iter()
            .filter(|event| matches!(event, StreamEvent::Usage(_)))
            .count();
        assert_eq!(
            usage_segments, 1,
            "fixture should exercise terminal usage as one additive segment"
        );

        let parsed = OpenAiChatAdapter::parse_response(json.as_bytes())
            .expect("parse paired response for usage comparison");
        assert_eq!(aggregate_usage_events(&events), parsed.usage);
    }
}
