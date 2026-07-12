use super::{
    PendingMessage, complete_response, complete_stream_events, finish_empty_assistant, message_id,
};
use crate::model::{message::Role, normalized::StopReason};

#[test]
fn interleaved_stream_and_complete_response_freeze_identically() {
    let expected_response = complete_response();
    let mut streaming = PendingMessage::new();
    for event in complete_stream_events() {
        streaming.push(event).expect("push complete stream event");
    }

    let streamed = streaming
        .finish(message_id())
        .expect("freeze streamed response");
    let mut non_streaming = PendingMessage::from_response(expected_response.clone());
    let completed = non_streaming
        .finish(message_id())
        .expect("freeze complete response");

    assert_eq!(streamed, completed);
    assert_eq!(streamed.message().id(), message_id());
    assert_eq!(streamed.message().payload(), &expected_response.message);
    assert_eq!(streamed.message().payload().role, Role::Assistant);
    assert_eq!(streamed.usage(), &expected_response.usage);
    assert_eq!(streamed.stop_reason(), &expected_response.stop_reason);
    assert_eq!(streamed.stop_reason().value, StopReason::ToolUse);
    assert_eq!(streamed.extra(), &expected_response.extra);
}

#[test]
fn frozen_metadata_can_be_split_without_changing_the_message() {
    let response = complete_response();
    let mut pending = PendingMessage::from_response(response.clone());
    let frozen = pending.finish(message_id()).expect("freeze response");

    let (message, usage, stop_reason, extra) = frozen.into_parts();

    assert_eq!(message.id(), message_id());
    assert_eq!(message.payload(), &response.message);
    assert_eq!(usage, response.usage);
    assert_eq!(stop_reason, response.stop_reason);
    assert_eq!(extra, response.extra);
}

#[test]
fn pending_debug_output_does_not_expose_partial_content() {
    let mut pending = PendingMessage::new();
    let debug_before = format!("{pending:?}");
    assert!(debug_before.contains("streaming"));

    let frozen = finish_empty_assistant(&mut pending);
    let debug_after = format!("{pending:?}");

    assert!(debug_after.contains("frozen"));
    assert!(!debug_after.contains("Accumulator"));
    assert_eq!(frozen.message().id(), message_id());
}
