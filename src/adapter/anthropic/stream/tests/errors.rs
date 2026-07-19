//! Protocol-error and boundary behavior for Anthropic SSE normalization.

use super::super::normalizer::StreamNormalizer;
use super::*;
use eventsource_stream::Event;

/// Builds one already-framed SSE event for state-machine tests.
fn event(name: &str, data: &str) -> Event {
    Event {
        event: name.to_owned(),
        data: data.to_owned(),
        ..Event::default()
    }
}

#[test]
fn partial_tool_json_is_not_parsed_until_block_stop() {
    let mut normalizer = StreamNormalizer::default();
    normalizer
        .translate(event(
            "message_start",
            r#"{"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":1,"output_tokens":0}}}"#,
        ))
        .expect("start message");
    normalizer
        .translate(event(
            "content_block_start",
            r#"{"type":"content_block_start","index":4,"content_block":{"type":"tool_use","id":"toolu_4","name":"lookup","input":{}}}"#,
        ))
        .expect("start tool block");

    let delta = normalizer
        .translate(event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":4,"delta":{"type":"input_json_delta","partial_json":"{\"city\""}}"#,
        ))
        .expect("an incomplete fragment must be accepted");
    assert_eq!(
        delta,
        vec![StreamEvent::BlockDelta {
            id: BlockId::new("anthropic-block-4"),
            delta: Delta::Json("{\"city\"".to_owned()),
        }]
    );

    let error = normalizer
        .translate(event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":4}"#,
        ))
        .expect_err("incomplete JSON must fail at the complete boundary");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("invalid JSON"));
    assert!(error.to_string().contains("anthropic-block-4"));
}

#[tokio::test]
async fn mismatched_event_name_and_unknown_payload_type_are_rejected() {
    let mismatch = concat!(
        "event: content_block_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"usage\":{}}}\n\n"
    );
    let error = decode_fixture(mismatch)
        .await
        .expect_err("event and payload type mismatch must fail");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("disagrees"));

    let unknown = "event: future_event\ndata: {\"type\":\"future_event\"}\n\n";
    let error = decode_fixture(unknown)
        .await
        .expect_err("unknown payload type must fail observably");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("future_event"));
}

#[tokio::test]
async fn decreasing_cumulative_usage_is_rejected_instead_of_underflowing() {
    let fixture = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"usage\":{\"output_tokens\":8}}}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n"
    );

    let error = decode_fixture(fixture)
        .await
        .expect_err("decreasing cumulative usage must fail");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("decreased from 8 to 7"));
}

#[tokio::test]
async fn premature_eof_and_invalid_utf8_are_protocol_errors() {
    let truncated = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"usage\":{}}}\n\n"
    );
    let error = decode_fixture(truncated)
        .await
        .expect_err("stream without message_stop must fail");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("before message_stop"));

    let source = stream::iter([Ok::<_, Infallible>(vec![0xff])]);
    let error = normalize_sse(source, |never| match never {})
        .try_collect::<Vec<_>>()
        .await
        .expect_err("invalid UTF-8 must fail");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("valid UTF-8"));
}

#[tokio::test]
async fn provider_error_event_is_classified_and_terminates_without_message_start() {
    let overloaded = concat!(
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n"
    );
    let events = decode_fixture(overloaded)
        .await
        .expect("provider error is a normalized terminal event");

    assert_eq!(events.len(), 1);
    let StreamEvent::Error(ClientError::Api { status, body }) = &events[0] else {
        panic!("expected a classified provider error event");
    };
    assert_eq!(*status, 529);
    assert!(body.contains("overloaded_error"));
}

#[tokio::test]
async fn block_lifecycle_and_message_stop_requirements_are_enforced() {
    let unknown_index = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"usage\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":9,\"delta\":{\"type\":\"text_delta\",\"text\":\"orphan\"}}\n\n"
    );
    let error = decode_fixture(unknown_index)
        .await
        .expect_err("unknown block index must fail");
    assert!(error.to_string().contains("unknown index 9"));

    let missing_reason = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"usage\":{}}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    );
    let events = decode_fixture(missing_reason)
        .await
        .expect("message stop without a reason falls back to Other");
    assert!(events.contains(&StreamEvent::MessageStop {
        stop_reason: crate::model::normalized::Normalized {
            value: crate::model::normalized::StopReason::Other,
            raw: None,
        },
    }));
}
