//! Protocol-error and terminal boundary coverage for Chat/Completions streams.
//!
//! Chat/Completions has no modelled `type: "error"` event, so provider errors
//! surface either as a malformed data frame (non-JSON, rejected by
//! [`super::super::wire::decode`]) or as a transport failure. These tests pin
//! the `[DONE]` sentinel termination, the EOF-without-sentinel incomplete
//! error, malformed-frame handling, and the state machine's robustness to
//! missing/empty/unknown fields (design doc §4.4).

use super::super::normalizer::StreamNormalizer;
use super::*;
use eventsource_stream::Event;

/// Builds one already-framed SSE event for direct state-machine tests.
fn event(name: &str, data: &str) -> Event {
    Event {
        event: name.to_owned(),
        data: data.to_owned(),
        ..Event::default()
    }
}

/// A chunk arriving after the `[DONE]` sentinel is rejected; the sentinel
/// itself already terminated the stream normally (design doc §4.4.1).
#[test]
fn trailing_chunk_after_done_sentinel_is_rejected() {
    let mut normalizer = StreamNormalizer::default();
    normalizer
        .translate(event("message", "[DONE]"))
        .expect("[DONE] flushes the stream");

    let error = normalizer
        .translate(event(
            "message",
            r#"{"choices":[{"index":0,"delta":{"content":"late"},"finish_reason":null}]}"#,
        ))
        .expect_err("a chunk after [DONE] must be rejected");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("after the [DONE] sentinel"));
}

/// A stream that ends without the `[DONE]` sentinel surfaces as an incomplete
/// stream error rather than a clean finish.
#[tokio::test]
async fn eof_without_done_sentinel_is_incomplete_error() {
    let fixture = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n",
    );
    let error = decode_fixture(fixture)
        .await
        .expect_err("a stream without [DONE] must not end cleanly");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("[DONE]"));
}

/// A data frame whose JSON does not parse is a protocol error (the closest
/// chat/completions analog to a provider "error frame").
#[tokio::test]
async fn malformed_chunk_json_is_protocol_error() {
    let fixture = "data: {not valid json\n\n";
    let error = decode_fixture(fixture)
        .await
        .expect_err("malformed chunk JSON must fail decoding");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(
        error
            .to_string()
            .contains("invalid OpenAI Chat/Completions stream")
    );
}

/// Invalid UTF-8 in the byte stream surfaces as a protocol error.
#[tokio::test]
async fn invalid_utf8_is_protocol_error() {
    let source = stream::iter([Ok::<_, Infallible>(vec![0xff])]);
    let error = normalize_sse(source, |never| match never {})
        .try_collect::<Vec<_>>()
        .await
        .expect_err("invalid UTF-8 must fail");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("valid UTF-8"));
}

/// The first tool-call fragment for an `index` must carry both the call id and
/// the function name (design doc §4.4.2).
#[test]
fn first_tool_call_fragment_without_id_is_rejected() {
    let mut normalizer = StreamNormalizer::default();
    normalizer
        .translate(event(
            "message",
            r#"{"choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
        ))
        .expect("start message");
    let error = normalizer
        .translate(event(
            "message",
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"f","arguments":""}}]},"finish_reason":null}]}"#,
        ))
        .expect_err("first tool_call fragment must carry id");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("must carry `id`"));
}

/// Empty deltas and unmodelled fields never panic: the stream still terminates
/// cleanly with a message start and stop (M3-R robustness checklist).
#[tokio::test]
async fn empty_delta_and_unknown_fields_terminate_cleanly() {
    let fixture = concat!(
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"system_fingerprint\":\"fp_demo\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );
    let events = decode_fixture(fixture)
        .await
        .expect("robustness fixture decodes");
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
