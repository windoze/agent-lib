//! One-message accumulation and checked freeze boundary.

use crate::{
    client::Response,
    conversation::{ConversationError, ConversationMessage, MessageId, PendingMessageError},
    model::{
        message::Role,
        normalized::{Normalized, StopReason},
        usage::Usage,
    },
    stream::{StreamEvent, accumulator::Accumulator},
};
use serde_json::{Map, Value};
use std::{fmt, mem};

/// A complete immutable message together with response metadata needed by a turn.
///
/// The contained [`ConversationMessage`] is created only after the Client
/// accumulator has accepted every completion boundary. Metadata remains
/// separate from the message payload so a later pending-turn transition can
/// aggregate usage without changing the frozen message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenMessage {
    message: ConversationMessage,
    usage: Usage,
    stop_reason: Normalized<StopReason>,
    extra: Map<String, Value>,
}

impl FrozenMessage {
    /// Returns the immutable Conversation envelope produced by this freeze.
    #[must_use]
    pub const fn message(&self) -> &ConversationMessage {
        &self.message
    }

    /// Returns token usage reported for this one response.
    #[must_use]
    pub const fn usage(&self) -> &Usage {
        &self.usage
    }

    /// Returns the normalized response stop reason and retained raw value.
    #[must_use]
    pub const fn stop_reason(&self) -> &Normalized<StopReason> {
        &self.stop_reason
    }

    /// Returns unmodeled response-level provider metadata.
    #[must_use]
    pub const fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }

    /// Consumes the result and separates the immutable message from metadata.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ConversationMessage,
        Usage,
        Normalized<StopReason>,
        Map<String, Value>,
    ) {
        (self.message, self.usage, self.stop_reason, self.extra)
    }

    /// Converts one complete Client response at the shared freeze boundary.
    fn from_response(id: MessageId, response: Response) -> Result<Self, PendingMessageError> {
        let Response {
            message,
            usage,
            stop_reason,
            extra,
        } = response;

        if message.role != Role::Assistant {
            return Err(PendingMessageError::InvalidResponseRole {
                actual: message.role,
            });
        }

        Ok(Self {
            message: ConversationMessage::new(id, message),
            usage,
            stop_reason,
            extra,
        })
    }
}

/// Mutable state for exactly one not-yet-frozen LLM response.
///
/// Streaming instances own the sole Client [`Accumulator`]. Non-streaming
/// instances hold a complete [`Response`], but both paths use the same final
/// response-to-message conversion. No getter exposes accumulated blocks as a
/// Client message before [`finish`](Self::finish) succeeds.
///
/// Partial state deliberately has no serde representation:
///
/// ```compile_fail
/// use agent_lib::conversation::PendingMessage;
///
/// let pending = PendingMessage::new();
/// serde_json::to_string(&pending).unwrap();
/// ```
///
/// It also cannot be inspected as a complete Client message:
///
/// ```compile_fail
/// use agent_lib::conversation::PendingMessage;
///
/// let pending = PendingMessage::new();
/// let complete = pending.message();
/// ```
#[must_use = "a pending message must be finished or explicitly cancelled"]
pub struct PendingMessage {
    state: PendingMessageState,
}

impl PendingMessage {
    /// Creates an empty streaming message with one fresh Client accumulator.
    pub fn new() -> Self {
        Self {
            state: PendingMessageState::Streaming(Accumulator::new()),
        }
    }

    /// Creates pending state from one complete non-streaming Client response.
    ///
    /// The caller-supplied message id is still deferred until
    /// [`finish`](Self::finish), matching the streaming path.
    pub const fn from_response(response: Response) -> Self {
        Self {
            state: PendingMessageState::Complete(response),
        }
    }

    /// Validates and folds the next normalized event in arrival order.
    ///
    /// Any [`AccumulatorError`](crate::stream::accumulator::AccumulatorError)
    /// is preserved in the returned [`ConversationError`] source chain and
    /// makes this pending message terminal. A terminal message can only be
    /// cancelled or dropped; it cannot later be repaired into a frozen value.
    pub fn push(&mut self, event: StreamEvent) -> Result<(), ConversationError> {
        let result = match &mut self.state {
            PendingMessageState::Streaming(accumulator) => accumulator.push(event),
            PendingMessageState::Complete(_) => {
                self.state = PendingMessageState::Terminal;
                return Err(PendingMessageError::StreamEventForCompleteResponse.into());
            }
            PendingMessageState::Terminal => {
                return Err(PendingMessageError::Terminal.into());
            }
            PendingMessageState::Frozen => {
                return Err(PendingMessageError::AlreadyFrozen.into());
            }
        };

        if let Err(source) = result {
            self.state = PendingMessageState::Terminal;
            return Err(PendingMessageError::from(source).into());
        }

        Ok(())
    }

    /// Freezes a complete response under an externally supplied message id.
    ///
    /// Streaming state must contain a message start, a stop for every started
    /// block, complete tool JSON, and a message stop. The id is attached only
    /// after all those checks and the shared assistant-role check succeed.
    /// Failed completion consumes the partial accumulator and leaves this
    /// instance terminal; successful completion can happen only once.
    pub fn finish(&mut self, id: MessageId) -> Result<FrozenMessage, ConversationError> {
        let previous = mem::replace(&mut self.state, PendingMessageState::Terminal);
        let response = match previous {
            PendingMessageState::Streaming(accumulator) => accumulator
                .finish()
                .map_err(PendingMessageError::from)
                .map_err(ConversationError::from)?,
            PendingMessageState::Complete(response) => response,
            PendingMessageState::Terminal => {
                return Err(PendingMessageError::Terminal.into());
            }
            PendingMessageState::Frozen => {
                self.state = PendingMessageState::Frozen;
                return Err(PendingMessageError::AlreadyFrozen.into());
            }
        };

        let frozen = FrozenMessage::from_response(id, response).map_err(ConversationError::from)?;
        self.state = PendingMessageState::Frozen;
        Ok(frozen)
    }

    /// Consumes and discards all partial or complete-but-unfrozen state.
    ///
    /// Cancellation intentionally does not invoke `Accumulator::finish`, does
    /// not parse a partial tool document, and does not allocate a message id.
    pub fn cancel(self) {}
}

impl Default for PendingMessage {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PendingMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.state {
            PendingMessageState::Streaming(_) => "streaming",
            PendingMessageState::Complete(_) => "complete_response",
            PendingMessageState::Terminal => "terminal",
            PendingMessageState::Frozen => "frozen",
        };
        formatter
            .debug_struct("PendingMessage")
            .field("state", &state)
            .finish_non_exhaustive()
    }
}

/// Internal lifecycle; partial response data is never directly exposed.
enum PendingMessageState {
    Streaming(Accumulator),
    Complete(Response),
    Terminal,
    Frozen,
}

#[cfg(test)]
mod tests;
