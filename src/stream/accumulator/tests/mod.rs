//! Focused tests for response folding, protocol errors, and async collection.

use super::Accumulator;
use crate::{
    model::{
        message::Role,
        normalized::{Normalized, StopReason},
    },
    stream::StreamEvent,
};

mod collect;
mod errors;
mod folding;

/// Pushes a message-start event using the provider's assistant role.
fn start_message(accumulator: &mut Accumulator) {
    accumulator
        .push(StreamEvent::MessageStart {
            role: Role::Assistant,
        })
        .expect("start message");
}

/// Pushes a message-stop event with the requested normalized reason.
fn stop_message(accumulator: &mut Accumulator, reason: StopReason) {
    let raw = match reason {
        StopReason::ToolUse => "tool_use",
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::Refusal => "refusal",
        StopReason::Other => "provider_specific",
    };
    accumulator
        .push(StreamEvent::MessageStop {
            stop_reason: Normalized::from_mapped(reason, raw),
        })
        .expect("stop message");
}
