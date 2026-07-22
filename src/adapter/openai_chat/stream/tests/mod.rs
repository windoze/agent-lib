//! Focused tests for Chat/Completions SSE normalization.
//!
//! M3-1 covers the terminal skeleton: the `[DONE]` sentinel ends the stream
//! normally without a JSON parse error, and the `event:` field is not checked
//! against any payload discriminator. Full per-field event production
//! (text / reasoning / tool deltas, usage, terminal stop reason) and fixture
//! end-to-end comparison arrive in M3-3.

use super::*;
use crate::{client::ClientError, stream::StreamEvent};
use futures::{TryStreamExt, stream};
use std::convert::Infallible;

/// Decodes an SSE fixture after splitting its bytes across framing boundaries.
///
/// The repeating uneven chunk sizes exercise UTF-8 and SSE-line splits so tests
/// do not depend on HTTP chunk boundaries.
async fn decode_fixture(fixture: &str) -> Result<Vec<StreamEvent>, ClientError> {
    let chunks = irregular_chunks(fixture.as_bytes());
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

/// A `data: [DONE]` sentinel ends the stream normally, and the non-JSON
/// sentinel itself never surfaces as a parse error (design doc §4.4.1).
#[tokio::test]
async fn done_sentinel_terminates_stream_without_json_error() {
    let fixture = concat!(
        "data: {\"id\":\"chatcmpl-done\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );
    let events = decode_fixture(fixture)
        .await
        .expect("the [DONE] sentinel must terminate the stream cleanly");
    // M3-1 skeleton produces no normalized events yet (M3-2 fills these in);
    // the assertion here is that the stream ends without error.
    assert!(events.is_empty());
}

/// The SSE `event:` field is accepted without an event/type consistency check;
/// chat/completions always emits `message` and carries no `type` discriminator.
#[tokio::test]
async fn message_event_field_is_not_checked_for_consistency() {
    let fixture = concat!(
        "event: message\n",
        "data: {\"id\":\"chatcmpl-event\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n",
        "event: message\n",
        "data: [DONE]\n\n",
    );
    let events = decode_fixture(fixture)
        .await
        .expect("event: message must not be treated as a mismatch");
    assert!(events.is_empty());
}

/// A stream that ends without a `[DONE]` sentinel surfaces as a protocol error.
#[tokio::test]
async fn premature_eof_without_done_sentinel_is_a_protocol_error() {
    let fixture = "data: {\"id\":\"chatcmpl-eof\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n";
    let error = decode_fixture(fixture)
        .await
        .expect_err("a stream without [DONE] must not end cleanly");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("[DONE]"));
}

/// The chunk wire view decodes every delta shape the state machine will need in
/// M3-2: assistant text, reasoning content, indexed tool-call fragments, and the
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
