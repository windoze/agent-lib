//! Transaction-local message state that has not entered closed history.
//!
//! A [`PendingMessage`] owns the only mutable Client accumulator for one LLM
//! response. It exposes no partial [`Message`](crate::model::message::Message):
//! only a successful, caller-identified freeze produces a [`FrozenMessage`].
//! [`PendingTurn`] keeps those frozen messages and tool correlations outside
//! committed history until a final assistant response passes the shared
//! closed-turn validator.

mod cancel;
mod message;
mod turn;

pub use cancel::{
    CANCELLED_TOOL_RESULT_TEXT, CancelDisposition, CancelOutcome, CancelledToolResult,
};
pub use message::{FrozenMessage, PendingMessage};
pub use turn::{AssistantFinish, PendingToolCall, PendingTurn, PendingTurnPhase, ToolCallMapping};
