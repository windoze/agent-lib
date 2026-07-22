//! Stateful conversion from Chat/Completions SSE chunks to normalized events.
//!
//! M3-1 ships only the terminal skeleton: the `data: [DONE]` sentinel is
//! recognized before JSON decoding (design doc §4.4.1) so the non-JSON sentinel
//! never surfaces as a parse error, a chunk arriving after the sentinel is
//! rejected, and EOF without a sentinel surfaces as an incomplete-stream error.
//! The per-field state machine that turns `content` / `reasoning_content` /
//! `tool_calls` deltas into normalized [`StreamEvent`]s arrives in M3-2.

use super::{invalid_stream, wire::decode};
use crate::{client::ClientError, stream::StreamEvent};
use eventsource_stream::Event;

/// Stateful conversion from chat/completions chunks to normalized events.
#[derive(Default)]
pub(super) struct StreamNormalizer {
    /// Set once the `data: [DONE]` sentinel terminates the stream normally.
    terminal: bool,
}

impl StreamNormalizer {
    /// Translates one fully framed SSE event into zero or more normalized events.
    pub(super) fn translate(&mut self, event: Event) -> Result<Vec<StreamEvent>, ClientError> {
        if self.terminal {
            return Err(invalid_stream(
                "received a chunk after the [DONE] sentinel".to_owned(),
            ));
        }

        // The `data: [DONE]` sentinel is not JSON; terminate the stream before
        // JSON decoding so it never surfaces as a parse error (design §4.4.1).
        // `is_terminal` then lets the shared decoder stop pulling the body.
        if event.data.trim() == "[DONE]" {
            self.terminal = true;
            return Ok(Vec::new());
        }

        // Validate the chunk decodes; M3-2 turns it into normalized events.
        decode(&event.data).map_err(invalid_stream)?;
        Ok(Vec::new())
    }

    /// Reports whether the `[DONE]` sentinel has already ended the stream.
    pub(super) fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Builds the error emitted when the byte stream ends before `[DONE]`.
    pub(super) fn incomplete_error(&self) -> ClientError {
        invalid_stream("SSE body ended before the [DONE] sentinel".to_owned())
    }
}
