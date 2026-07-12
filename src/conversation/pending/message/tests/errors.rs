use super::{
    PendingMessage, complete_response, finish_empty_assistant, message_id, message_start,
    message_stop,
};
use crate::{
    client::ClientError,
    conversation::{ConversationError, PendingMessageError},
    model::{message::Role, normalized::StopReason},
    stream::{BlockId, BlockKind, Delta, StreamEvent, accumulator::AccumulatorError},
};
use std::{error::Error, sync::Arc};

fn expect_accumulator_error(error: ConversationError) -> Arc<AccumulatorError> {
    let ConversationError::PendingMessage(PendingMessageError::Accumulator(source)) = error else {
        panic!("expected pending accumulator error");
    };
    source
}

#[test]
fn partial_json_unclosed_block_and_missing_message_stop_never_freeze() {
    let tool_id = BlockId::new("partial-tool");
    let mut partial_json = PendingMessage::new();
    partial_json
        .push(message_start(Role::Assistant))
        .expect("start message");
    partial_json
        .push(StreamEvent::BlockStart {
            id: tool_id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "call-partial".to_owned(),
            },
        })
        .expect("start tool block");
    partial_json
        .push(StreamEvent::BlockDelta {
            id: tool_id.clone(),
            delta: Delta::Json("{\"city\":\"Paris\"".to_owned()),
        })
        .expect("push partial JSON");
    partial_json
        .push(message_stop(StopReason::ToolUse))
        .expect("stop message");

    let source = expect_accumulator_error(
        partial_json
            .finish(message_id())
            .expect_err("partial JSON must not freeze"),
    );
    assert!(matches!(
        source.as_ref(),
        AccumulatorError::InvalidToolInput { id, .. } if id == &tool_id
    ));
    assert_eq!(
        partial_json
            .push(message_start(Role::Assistant))
            .expect_err("failed finish must be terminal"),
        ConversationError::PendingMessage(PendingMessageError::Terminal)
    );

    let text_id = BlockId::new("unclosed-text");
    let mut unclosed_block = PendingMessage::new();
    for event in [
        message_start(Role::Assistant),
        StreamEvent::BlockStart {
            id: text_id.clone(),
            kind: BlockKind::Text,
        },
        StreamEvent::BlockDelta {
            id: text_id.clone(),
            delta: Delta::Text("partial".to_owned()),
        },
        message_stop(StopReason::EndTurn),
    ] {
        unclosed_block.push(event).expect("push partial text");
    }
    let source = expect_accumulator_error(
        unclosed_block
            .finish(message_id())
            .expect_err("unclosed block must not freeze"),
    );
    assert!(matches!(
        source.as_ref(),
        AccumulatorError::UnclosedBlock(id) if id == &text_id
    ));

    let mut missing_stop = PendingMessage::new();
    missing_stop
        .push(message_start(Role::Assistant))
        .expect("start message");
    let source = expect_accumulator_error(
        missing_stop
            .finish(message_id())
            .expect_err("missing message stop must not freeze"),
    );
    assert!(matches!(
        source.as_ref(),
        AccumulatorError::MissingMessageStop
    ));
}

#[test]
fn accumulator_push_error_preserves_the_full_source_chain_and_is_terminal() {
    let stream_error = ClientError::Network("provider disconnected".to_owned());
    let mut pending = PendingMessage::new();
    let error = pending
        .push(StreamEvent::Error(stream_error.clone()))
        .expect_err("error event must fail");

    let pending_source = error.source().expect("pending message source");
    assert!(pending_source.is::<PendingMessageError>());
    let accumulator_source = pending_source.source().expect("accumulator source");
    assert!(matches!(
        accumulator_source.downcast_ref::<AccumulatorError>(),
        Some(AccumulatorError::Stream(actual)) if actual == &stream_error
    ));
    assert_eq!(
        accumulator_source
            .source()
            .and_then(|source| source.downcast_ref::<ClientError>()),
        Some(&stream_error)
    );

    assert_eq!(
        pending
            .push(message_start(Role::Assistant))
            .expect_err("terminal state must reject later events"),
        ConversationError::PendingMessage(PendingMessageError::Terminal)
    );
    assert_eq!(
        pending
            .finish(message_id())
            .expect_err("terminal state must not freeze"),
        ConversationError::PendingMessage(PendingMessageError::Terminal)
    );
}

#[test]
fn successful_message_cannot_finish_twice_or_accept_more_events() {
    let mut pending = PendingMessage::new();
    let first = finish_empty_assistant(&mut pending);

    assert_eq!(first.message().id(), message_id());
    assert_eq!(
        pending
            .finish(message_id())
            .expect_err("second finish must fail"),
        ConversationError::PendingMessage(PendingMessageError::AlreadyFrozen)
    );
    assert_eq!(
        pending
            .push(message_start(Role::Assistant))
            .expect_err("frozen message must reject later events"),
        ConversationError::PendingMessage(PendingMessageError::AlreadyFrozen)
    );
    assert_eq!(first.message().id(), message_id());
}

#[test]
fn stream_and_non_stream_paths_share_the_assistant_role_check() {
    let mut response = complete_response();
    response.message.role = Role::User;
    let mut non_streaming = PendingMessage::from_response(response);
    let non_stream_error = non_streaming
        .finish(message_id())
        .expect_err("user response must not freeze");

    let mut streaming = PendingMessage::new();
    streaming
        .push(message_start(Role::User))
        .expect("start response");
    streaming
        .push(message_stop(StopReason::EndTurn))
        .expect("stop response");
    let stream_error = streaming
        .finish(message_id())
        .expect_err("user stream must not freeze");

    let expected = ConversationError::PendingMessage(PendingMessageError::InvalidResponseRole {
        actual: Role::User,
    });
    assert_eq!(non_stream_error, expected);
    assert_eq!(stream_error, expected);
    assert_eq!(
        streaming
            .finish(message_id())
            .expect_err("invalid role makes state terminal"),
        ConversationError::PendingMessage(PendingMessageError::Terminal)
    );
}

#[test]
fn complete_response_rejects_stream_events_without_exposing_its_message() {
    let mut pending = PendingMessage::from_response(complete_response());

    assert_eq!(
        pending
            .push(message_start(Role::Assistant))
            .expect_err("complete response cannot accept events"),
        ConversationError::PendingMessage(PendingMessageError::StreamEventForCompleteResponse)
    );
    assert_eq!(
        pending
            .finish(message_id())
            .expect_err("invalid transition makes state terminal"),
        ConversationError::PendingMessage(PendingMessageError::Terminal)
    );
}

#[test]
fn cancel_and_drop_discard_partial_state_without_finishing() {
    let tool_id = BlockId::new("cancelled-partial-tool");
    let mut cancelled = PendingMessage::new();
    for event in [
        message_start(Role::Assistant),
        StreamEvent::BlockStart {
            id: tool_id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "lookup".to_owned(),
                tool_call_id: "call-cancelled".to_owned(),
            },
        },
        StreamEvent::BlockDelta {
            id: tool_id,
            delta: Delta::Json("{".to_owned()),
        },
    ] {
        cancelled.push(event).expect("build cancellable partial");
    }
    cancelled.cancel();

    {
        let mut dropped = PendingMessage::new();
        dropped
            .push(message_start(Role::Assistant))
            .expect("build droppable partial");
        dropped
            .push(StreamEvent::BlockStart {
                id: BlockId::new("dropped-text"),
                kind: BlockKind::Text,
            })
            .expect("start droppable block");
        // Leaving scope drops the only accumulator without calling `finish`.
    }
}
