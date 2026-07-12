//! Boundary and protocol-error tests for response folding.

use super::{start_message, stop_message};
use crate::{
    client::ClientError,
    model::{normalized::StopReason, usage::Usage},
    stream::{
        BlockId, BlockKind, Delta, StreamEvent,
        accumulator::{Accumulator, AccumulatorError},
    },
};

#[test]
fn finish_reports_partial_tool_json_instead_of_panicking() {
    let mut accumulator = Accumulator::new();
    let id = BlockId::new("tool-partial");
    start_message(&mut accumulator);
    accumulator
        .push(StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "call-partial".to_owned(),
            },
        })
        .unwrap();
    accumulator
        .push(StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Json("{\"city\":\"Paris\"".to_owned()),
        })
        .unwrap();
    stop_message(&mut accumulator, StopReason::ToolUse);

    let error = accumulator.finish().unwrap_err();
    assert!(matches!(
        error,
        AccumulatorError::InvalidToolInput { id: error_id, .. } if error_id == id
    ));
}

#[test]
fn block_stop_reports_invalid_tool_json() {
    let mut accumulator = Accumulator::new();
    let id = BlockId::new("tool-invalid");
    accumulator
        .push(StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "lookup".to_owned(),
                tool_call_id: "call-invalid".to_owned(),
            },
        })
        .unwrap();
    accumulator
        .push(StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Json("{".to_owned()),
        })
        .unwrap();

    let error = accumulator
        .push(StreamEvent::BlockStop { id: id.clone() })
        .unwrap_err();
    assert!(matches!(
        error,
        AccumulatorError::InvalidToolInput { id: error_id, .. } if error_id == id
    ));
}

#[test]
fn empty_and_usage_only_streams_report_missing_message_start() {
    assert!(matches!(
        Accumulator::new().finish(),
        Err(AccumulatorError::MissingMessageStart)
    ));

    let mut usage_only = Accumulator::new();
    usage_only
        .push(StreamEvent::Usage(Usage {
            input: 3,
            ..Usage::default()
        }))
        .unwrap();
    assert!(matches!(
        usage_only.finish(),
        Err(AccumulatorError::MissingMessageStart)
    ));
}

#[test]
fn message_without_stop_reason_is_rejected() {
    let mut accumulator = Accumulator::new();
    start_message(&mut accumulator);

    assert!(matches!(
        accumulator.finish(),
        Err(AccumulatorError::MissingMessageStop)
    ));
}

#[test]
fn unclosed_text_block_is_rejected() {
    let mut accumulator = Accumulator::new();
    let id = BlockId::new("text-open");
    start_message(&mut accumulator);
    accumulator
        .push(StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::Text,
        })
        .unwrap();
    accumulator
        .push(StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Text("partial".to_owned()),
        })
        .unwrap();
    stop_message(&mut accumulator, StopReason::EndTurn);

    assert!(matches!(
        accumulator.finish(),
        Err(AccumulatorError::UnclosedBlock(error_id)) if error_id == id
    ));
}

#[test]
fn explicit_error_event_is_returned_immediately() {
    let mut accumulator = Accumulator::new();
    let stream_error = ClientError::Network("provider disconnected".to_owned());
    let error = accumulator
        .push(StreamEvent::Error(stream_error.clone()))
        .unwrap_err();

    assert!(matches!(
        error,
        AccumulatorError::Stream(error) if error == stream_error
    ));
}

#[test]
fn invalid_block_id_and_delta_kind_are_rejected() {
    let mut accumulator = Accumulator::new();
    let unknown_id = BlockId::new("unknown");
    let error = accumulator
        .push(StreamEvent::BlockDelta {
            id: unknown_id.clone(),
            delta: Delta::Text("orphan".to_owned()),
        })
        .unwrap_err();
    assert!(matches!(
        error,
        AccumulatorError::UnknownBlock(error_id) if error_id == unknown_id
    ));

    let text_id = BlockId::new("text-1");
    accumulator
        .push(StreamEvent::BlockStart {
            id: text_id.clone(),
            kind: BlockKind::Text,
        })
        .unwrap();
    let error = accumulator
        .push(StreamEvent::BlockDelta {
            id: text_id.clone(),
            delta: Delta::Reasoning("wrong category".to_owned()),
        })
        .unwrap_err();
    assert!(matches!(
        error,
        AccumulatorError::MismatchedDelta {
            id: error_id,
            expected: "text",
            actual: "reasoning",
        } if error_id == text_id
    ));
}
