//! Protocol-error and terminal boundary coverage for Responses streams.

use super::super::normalizer::StreamNormalizer;
use super::*;
use crate::model::normalized::StopReason;
use eventsource_stream::Event;
use serde_json::json;

/// Builds one already-framed SSE event for direct state-machine tests.
fn event(name: &str, data: &str) -> Event {
    Event {
        event: name.to_owned(),
        data: data.to_owned(),
        ..Event::default()
    }
}

#[test]
fn partial_function_json_is_accepted_until_arguments_done() {
    let mut normalizer = StreamNormalizer::default();
    normalizer
        .translate(event(
            "response.created",
            r#"{"type":"response.created","response":{"id":"resp_partial","object":"response","status":"in_progress","output":[],"usage":null},"sequence_number":0}"#,
        ))
        .expect("start response");
    normalizer
        .translate(event(
            "response.output_item.added",
            r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"fc_partial","type":"function_call","name":"lookup","call_id":"call_partial","arguments":""},"sequence_number":1}"#,
        ))
        .expect("start function item");

    let delta = normalizer
        .translate(event(
            "response.function_call_arguments.delta",
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_partial","output_index":0,"delta":"{\"city\"","sequence_number":2}"#,
        ))
        .expect("partial JSON fragment must not be parsed");
    assert_eq!(
        delta,
        vec![StreamEvent::BlockDelta {
            id: BlockId::new("openai-response-item-fc_partial"),
            delta: Delta::Json("{\"city\"".to_owned()),
        }]
    );

    let error = normalizer
        .translate(event(
            "response.function_call_arguments.done",
            r#"{"type":"response.function_call_arguments.done","item_id":"fc_partial","output_index":0,"arguments":"{\"city\"","sequence_number":3}"#,
        ))
        .expect_err("incomplete JSON must fail at arguments done");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("invalid JSON"));
    assert!(error.to_string().contains("fc_partial"));
}

#[tokio::test]
async fn sequence_event_name_and_item_index_mismatches_are_rejected() {
    let bad_sequence = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bad\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":1}\n\n"
    );
    let error = decode_fixture(bad_sequence)
        .await
        .expect_err("sequence must begin at zero");
    assert!(error.to_string().contains("expected 0"));

    let bad_gap = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bad\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n",
        "event: response.in_progress\n",
        "data: {\"type\":\"response.in_progress\",\"response\":{\"id\":\"resp_bad\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":2}\n\n"
    );
    let error = decode_fixture(bad_gap)
        .await
        .expect_err("numbered events must remain contiguous");
    assert!(error.to_string().contains("expected 1"));

    let bad_name = concat!(
        "event: response.completed\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bad\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n"
    );
    let error = decode_fixture(bad_name)
        .await
        .expect_err("SSE event and payload names must agree");
    assert!(error.to_string().contains("disagrees"));

    let bad_index = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bad\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg_bad\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]},\"sequence_number\":1}\n\n",
        "event: response.content_part.added\n",
        "data: {\"type\":\"response.content_part.added\",\"item_id\":\"msg_bad\",\"output_index\":3,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\"},\"sequence_number\":2}\n\n"
    );
    let error = decode_fixture(bad_index)
        .await
        .expect_err("item id and output index must remain correlated");
    assert!(error.to_string().contains("maps to output index 0, not 3"));
}

#[tokio::test]
async fn missing_sequence_numbers_are_accepted_for_compatible_streams() {
    let fixture = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_compat\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_compat\",\"object\":\"response\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n"
    );

    let events = decode_fixture(fixture)
        .await
        .expect("compatible endpoints may omit sequence_number");
    assert_eq!(
        events[0],
        StreamEvent::MessageStart {
            role: Role::Assistant,
        }
    );
    assert!(events.iter().any(|event| {
        event
            == &StreamEvent::Usage(Usage {
                input: 2,
                output: 3,
                ..Usage::default()
            })
    }));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::MessageStop { .. })
    ));
}

#[tokio::test]
async fn premature_eof_and_invalid_utf8_are_protocol_errors() {
    let truncated = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_truncated\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n"
    );
    let error = decode_fixture(truncated)
        .await
        .expect_err("stream without terminal response must fail");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("terminal response event"));

    let source = stream::iter([Ok::<_, Infallible>(vec![0xff])]);
    let error = normalize_sse(source, |never| match never {})
        .try_collect::<Vec<_>>()
        .await
        .expect_err("invalid UTF-8 must fail");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("valid UTF-8"));
}

#[tokio::test]
async fn provider_error_and_failed_response_are_classified_terminal_events() {
    let rate_limit = concat!(
        "event: error\n",
        "data: {\"type\":\"error\",\"code\":\"rate_limit_exceeded\",\"message\":\"slow down\",\"param\":null,\"sequence_number\":0}\n\n"
    );
    let events = decode_fixture(rate_limit)
        .await
        .expect("provider error should normalize as a terminal event");
    assert_eq!(
        events,
        vec![StreamEvent::Error(ClientError::RateLimited {
            retry_after: None,
        })]
    );

    let filtered = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_failed\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n",
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_failed\",\"object\":\"response\",\"status\":\"failed\",\"output\":[],\"usage\":null,\"error\":{\"code\":\"content_filter\",\"message\":\"blocked by content policy\"}},\"sequence_number\":1}\n\n"
    );
    let events = decode_fixture(filtered)
        .await
        .expect("failed response should normalize as a terminal event");
    assert_eq!(events[1], StreamEvent::Error(ClientError::ContentFiltered));
}

#[tokio::test]
async fn incomplete_response_maps_stop_reason_and_unknown_events_are_retained() {
    let fixture = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_incomplete\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n",
        "event: response.future_extension\n",
        "data: {\"type\":\"response.future_extension\",\"payload\":{\"kept\":true},\"sequence_number\":1}\n\n",
        "event: response.incomplete\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_incomplete\",\"object\":\"response\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"output\":[],\"usage\":{\"input_tokens\":7,\"output_tokens\":4}},\"sequence_number\":2}\n\n"
    );
    let events = decode_fixture(fixture)
        .await
        .expect("decode incomplete response with extension event");
    let response = fold_events(&events).expect("fold incomplete response");

    assert_eq!(*response.stop_reason.value(), StopReason::MaxTokens);
    assert_eq!(response.usage.input, 7);
    assert_eq!(response.usage.output, 4);
    assert_eq!(
        response.extra["openai_unmodeled_stream_events"][0]["payload"],
        json!({ "kept": true })
    );
}
