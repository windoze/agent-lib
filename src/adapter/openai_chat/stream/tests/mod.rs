//! Chat/Completions SSE normalization tests.
//!
//! This module holds the shared byte-chunking and folding helpers used by the
//! [`parsing`], [`errors`], and [`transport`] submodules, plus the inline
//! state-machine tests. Each state-machine test feeds an inline SSE byte string
//! through the full `normalize_sse` pipeline (split on an uneven chunk pattern)
//! and asserts the exact [`StreamEvent`] sequence the M3-2 state machine
//! produces, covering the six scenarios from design doc §4.4: text, reasoning,
//! single tool call, parallel interleaved tool calls, terminal usage, and the
//! `finish_reason` stop-reason table, plus the `[DONE]` / EOF error paths.

use super::*;
use crate::{
    client::Response,
    model::{
        message::Role,
        normalized::{Normalized, StopReason},
        usage::Usage,
    },
    stream::{
        BlockId, BlockKind, Delta,
        accumulator::{Accumulator, AccumulatorError},
    },
};
use futures::{TryStreamExt, stream};
use std::convert::Infallible;

/// Decodes an SSE fixture after splitting its bytes across framing boundaries.
///
/// The repeating uneven chunk sizes exercise UTF-8 and SSE-line splits so tests
/// do not depend on HTTP chunk boundaries.
async fn decode_fixture(fixture: impl AsRef<str>) -> Result<Vec<StreamEvent>, ClientError> {
    let chunks = irregular_chunks(fixture.as_ref().as_bytes());
    let source = stream::iter(chunks.into_iter().map(Ok::<_, Infallible>));
    normalize_sse(source, |never| match never {})
        .try_collect()
        .await
}

/// Splits `bytes` on a repeating uneven pattern.
fn irregular_chunks(bytes: &[u8]) -> Vec<Vec<u8>> {
    const SIZES: &[usize] = &[1, 2, 7, 3, 19, 5, 11];
    let mut chunks = Vec::new();
    let mut offset = 0;
    let mut next_size = 0;
    while offset < bytes.len() {
        let end = (offset + SIZES[next_size % SIZES.len()]).min(bytes.len());
        chunks.push(bytes[offset..end].to_vec());
        offset = end;
        next_size += 1;
    }
    chunks
}

/// Builds an SSE body from chunk JSON payloads terminated by `[DONE]`.
fn sse(chunks: &[&str]) -> String {
    let mut fixture = String::new();
    for chunk in chunks {
        fixture.push_str("data: ");
        fixture.push_str(chunk);
        fixture.push_str("\n\n");
    }
    fixture.push_str("data: [DONE]\n\n");
    fixture
}

// Sanitized demo recordings (no real keys/accounts) of chat/completions streams
// exercising each scenario from design doc §4.4. Each `.sse` has a paired
// `.json` representing the same semantic response as a non-streaming body, used
// to prove the streaming fold equals the non-streaming parse.
const REAL_TEXT_STREAM: &str = include_str!("fixtures/text_stream.sse");
const REAL_TOOL_STREAM: &str = include_str!("fixtures/tool_stream.sse");
const REAL_REASONING_STREAM: &str = include_str!("fixtures/reasoning_stream.sse");
const REAL_USAGE_TERMINAL_STREAM: &str = include_str!("fixtures/usage_terminal.sse");

/// Folds a decoded event list through the shared accumulator into a response.
fn fold_events(events: &[StreamEvent]) -> Result<Response, AccumulatorError> {
    let mut accumulator = Accumulator::new();
    for event in events {
        accumulator.push(event.clone())?;
    }
    accumulator.finish()
}

/// Reduces a response to the modeled members for streaming/non-streaming
/// comparison.
///
/// Streaming events carry no response-level metadata, so a folded response has
/// an empty `extra`; a non-streaming `parse_response` retains top-level wire
/// fields (including `choices` with `logprobs`) in `extra`. Clearing `extra`
/// on both sides leaves the message content, usage, and stop reason — the
/// fields a stream and its complete response must agree on.
fn comparable(mut response: Response) -> Response {
    response.extra.clear();
    response
}

mod errors;
mod parsing;
mod transport;

/// A plain text stream opens one text block, appends deltas, stops it, and ends
/// with a message stop carrying the `finish_reason` (design doc §4.4.3).
#[tokio::test]
async fn text_stream_emits_text_block_then_message_stop() {
    let fixture = sse(&[
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":" world"},"finish_reason":"stop"}]}"#,
    ]);
    let events = decode_fixture(fixture).await.expect("text stream decodes");
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
            StreamEvent::BlockStop { id: text },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::EndTurn, "stop"),
            },
        ]
    );
}

/// `reasoning_content` opens a reasoning block (no signature) that precedes the
/// text block, matching the wire field order (design doc §4.4.3).
#[tokio::test]
async fn reasoning_stream_emits_reasoning_block_before_text() {
    let fixture = sse(&[
        r#"{"choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"reasoning_content":"th"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"reasoning_content":"ink"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":"stop"}]}"#,
    ]);
    let events = decode_fixture(fixture)
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
                delta: Delta::Reasoning("th".to_owned()),
            },
            StreamEvent::BlockDelta {
                id: reasoning.clone(),
                delta: Delta::Reasoning("ink".to_owned()),
            },
            StreamEvent::BlockStart {
                id: text.clone(),
                kind: BlockKind::Text,
            },
            StreamEvent::BlockDelta {
                id: text.clone(),
                delta: Delta::Text("answer".to_owned()),
            },
            StreamEvent::BlockStop {
                id: reasoning.clone(),
            },
            StreamEvent::BlockStop { id: text },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::EndTurn, "stop"),
            },
        ]
    );
}

/// A single tool call opens its block on the first fragment (carrying `id` and
/// `function.name`) and appends raw argument fragments; the JSON is never parsed
/// mid-stream — no `ToolInputAvailable` event is emitted (design doc §4.4.2).
#[tokio::test]
async fn single_tool_call_streams_argument_fragments_without_parsing() {
    let fixture = sse(&[
        r#"{"choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"SF\"}"}}]},"finish_reason":"tool_calls"}]}"#,
    ]);
    let events = decode_fixture(fixture).await.expect("tool stream decodes");
    let tool = BlockId::new("tool-call-0");
    assert_eq!(
        events,
        vec![
            StreamEvent::MessageStart {
                role: Role::Assistant,
            },
            StreamEvent::BlockStart {
                id: tool.clone(),
                kind: BlockKind::ToolInput {
                    tool_name: "get_weather".to_owned(),
                    tool_call_id: "call_1".to_owned(),
                },
            },
            StreamEvent::BlockDelta {
                id: tool.clone(),
                delta: Delta::Json(r#"{"city":"#.to_owned()),
            },
            StreamEvent::BlockDelta {
                id: tool.clone(),
                delta: Delta::Json(r#""SF"}"#.to_owned()),
            },
            StreamEvent::BlockStop { id: tool },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::ToolUse, "tool_calls"),
            },
        ]
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::ToolInputAvailable { .. })),
        "arguments must not be parsed mid-stream"
    );
}

/// Two interleaved `index` values keep independent tool-input blocks keyed by
/// their wire `index`, so parallel tool calls accumulate correctly (§4.4.2).
#[tokio::test]
async fn parallel_tool_calls_interleave_by_index() {
    let fixture = sse(&[
        r#"{"choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c0","type":"function","function":{"name":"f0","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"c1","type":"function","function":{"name":"f1","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"b\":"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"2}"}}]},"finish_reason":"tool_calls"}]}"#,
    ]);
    let events = decode_fixture(fixture)
        .await
        .expect("parallel tool stream decodes");
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
                    tool_name: "f0".to_owned(),
                    tool_call_id: "c0".to_owned(),
                },
            },
            StreamEvent::BlockStart {
                id: tool1.clone(),
                kind: BlockKind::ToolInput {
                    tool_name: "f1".to_owned(),
                    tool_call_id: "c1".to_owned(),
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
            StreamEvent::BlockStop { id: tool0 },
            StreamEvent::BlockStop { id: tool1 },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::ToolUse, "tool_calls"),
            },
        ]
    );
}

/// The terminal usage chunk (empty `choices`) emits one additive usage segment
/// ahead of the deferred message stop (design doc §4.4.4).
#[tokio::test]
async fn terminal_usage_chunk_emits_additive_usage_before_stop() {
    let fixture = sse(&[
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":12,"total_tokens":21}}"#,
    ]);
    let events = decode_fixture(fixture).await.expect("usage stream decodes");
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
                delta: Delta::Text("hi".to_owned()),
            },
            StreamEvent::Usage(Usage {
                input: 9,
                output: 12,
                total: Some(21),
                ..Usage::default()
            }),
            StreamEvent::BlockStop { id: text },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::EndTurn, "stop"),
            },
        ]
    );
}

/// Every `finish_reason` value maps through the shared §4.3 table, including an
/// unknown value and an absent one.
#[tokio::test]
async fn finish_reason_maps_each_value_to_stop_reason() {
    let cases: [(&str, Normalized<StopReason>); 5] = [
        ("stop", Normalized::from_mapped(StopReason::EndTurn, "stop")),
        (
            "length",
            Normalized::from_mapped(StopReason::MaxTokens, "length"),
        ),
        (
            "tool_calls",
            Normalized::from_mapped(StopReason::ToolUse, "tool_calls"),
        ),
        (
            "content_filter",
            Normalized::from_mapped(StopReason::Refusal, "content_filter"),
        ),
        ("weird", Normalized::unknown("weird")),
    ];

    for (finish_reason, expected) in cases {
        let chunk = format!(
            r#"{{"choices":[{{"index":0,"delta":{{"content":"x"}},"finish_reason":"{finish_reason}"}}]}}"#
        );
        let fixture = sse(&[&chunk]);
        let events = decode_fixture(fixture)
            .await
            .expect("finish_reason stream decodes");
        let stop_reason = events.iter().find_map(|event| match event {
            StreamEvent::MessageStop { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        });
        assert_eq!(
            stop_reason,
            Some(expected),
            "finish_reason `{finish_reason}`"
        );
    }

    // A missing finish_reason maps to `Other` with no retained raw value.
    let fixture =
        sse(&[r#"{"choices":[{"index":0,"delta":{"content":"x"},"finish_reason":null}]}"#]);
    let events = decode_fixture(fixture)
        .await
        .expect("null finish_reason stream decodes");
    let stop_reason = events.iter().find_map(|event| match event {
        StreamEvent::MessageStop { stop_reason } => Some(stop_reason.clone()),
        _ => None,
    });
    assert_eq!(
        stop_reason,
        Some(Normalized::without_raw(StopReason::Other))
    );
}

/// The `data: [DONE]` sentinel closes every open block and emits the cached
/// message stop, without surfacing the non-JSON sentinel as a parse error.
#[tokio::test]
async fn done_sentinel_closes_open_blocks_and_emits_message_stop() {
    let fixture = sse(&[
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#,
    ]);
    let events = decode_fixture(fixture)
        .await
        .expect("[DONE] must terminate the stream cleanly");
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
                delta: Delta::Text("hi".to_owned()),
            },
            StreamEvent::BlockStop { id: text },
            StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::EndTurn, "stop"),
            },
        ]
    );
}

/// The SSE `event:` field is accepted without an event/type consistency check;
/// chat/completions always emits `message` and carries no `type` discriminator.
#[tokio::test]
async fn message_event_field_is_not_checked_for_consistency() {
    let fixture = concat!(
        "event: message\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n",
        "event: message\n",
        "data: [DONE]\n\n",
    );
    let events = decode_fixture(fixture)
        .await
        .expect("event: message must not be treated as a mismatch");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::MessageStop { .. })),
        "event: message must still produce a message stop"
    );
}

/// A stream with only the sentinel still emits a message start and stop.
#[tokio::test]
async fn empty_stream_emits_message_start_and_stop() {
    let fixture = "data: [DONE]\n\n";
    let events = decode_fixture(fixture).await.expect("empty stream decodes");
    assert_eq!(
        events,
        vec![
            StreamEvent::MessageStart {
                role: Role::Assistant,
            },
            StreamEvent::MessageStop {
                stop_reason: Normalized::without_raw(StopReason::Other),
            },
        ]
    );
}

/// A stream that ends without a `[DONE]` sentinel surfaces as a protocol error.
#[tokio::test]
async fn premature_eof_without_done_sentinel_is_a_protocol_error() {
    let fixture = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n";
    let error = decode_fixture(fixture)
        .await
        .expect_err("a stream without [DONE] must not end cleanly");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("[DONE]"));
}

/// A non-`assistant` role is rejected as a protocol error.
#[tokio::test]
async fn non_assistant_role_is_a_protocol_error() {
    let fixture = sse(&[
        r#"{"choices":[{"index":0,"delta":{"role":"system","content":"x"},"finish_reason":"stop"}]}"#,
    ]);
    let error = decode_fixture(fixture)
        .await
        .expect_err("a non-assistant role must be rejected");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("assistant"));
}

/// The chunk wire view decodes every delta shape the state machine consumes:
/// assistant text, reasoning content, indexed tool-call fragments, and the
/// terminal usage-only chunk.
#[test]
fn wire_decodes_each_delta_shape() {
    let text = super::wire::decode(
        r#"{"id":"x","object":"chat.completion.chunk","created":0,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"hi"},"finish_reason":null}]}"#,
    )
    .expect("text chunk decodes");
    let text_delta = &text.choices[0].delta;
    assert_eq!(text_delta.role.as_deref(), Some("assistant"));
    assert_eq!(text_delta.content.as_deref(), Some("hi"));
    assert_eq!(text.choices[0].finish_reason, None);

    let reasoning = super::wire::decode(
        r#"{"choices":[{"index":0,"delta":{"reasoning_content":"thinking"},"finish_reason":null}]}"#,
    )
    .expect("reasoning chunk decodes");
    assert_eq!(
        reasoning.choices[0].delta.reasoning_content.as_deref(),
        Some("thinking")
    );

    let tool = super::wire::decode(
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":"}}]},"finish_reason":null}]}"#,
    )
    .expect("tool chunk decodes");
    let tool_call = &tool.choices[0].delta.tool_calls.as_deref().unwrap()[0];
    assert_eq!(tool_call.index, 0);
    assert_eq!(tool_call.id.as_deref(), Some("call_1"));
    let function = tool_call.function.as_ref().unwrap();
    assert_eq!(function.name.as_deref(), Some("get_weather"));
    assert_eq!(function.arguments.as_deref(), Some("{\"city\":"));

    let usage = super::wire::decode(
        r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":12,"total_tokens":21}}"#,
    )
    .expect("usage-only chunk decodes");
    assert!(usage.choices.is_empty());
    assert!(usage.usage.is_some());
}
