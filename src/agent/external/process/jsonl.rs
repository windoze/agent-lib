//! Shared JSONL-decoder core for the managed CLI runtimes.
//!
//! The three managed CLI adapters (Claude Code, Codex, OpenCode) each decode a
//! newline-delimited JSON event stream under the same envelope policy: trim the
//! line, tolerate a bounded run of non-JSON noise, require a JSON object with a
//! string `type` tag, mint monotonically-sequenced observations, and buffer them
//! for the session to drain. [`JsonlDecoderCore`] single-sources that envelope
//! so each runtime's decoder keeps only its frame dispatch and per-frame
//! handling.

use serde_json::{Map, Value};

use crate::agent::external::{ExternalAgentError, ExternalAgentEvent, ExternalObservedEvent};

/// Maximum tolerated consecutive non-JSON lines before the stream is declared
/// corrupt (decoders surface [`ExternalAgentError::Protocol`] past this bound).
pub(crate) const MAX_CONSECUTIVE_NON_JSON_LINES: usize = 8;

/// Result of classifying one raw JSONL line: the frame's `type` tag and its
/// object when the line carried a valid frame, `None` for a blank or tolerated
/// noise line.
type ParseFrameResult = Result<Option<(String, Map<String, Value>)>, ExternalAgentError>;

/// Sequencing, session-id, observation-buffer, and noise-tolerance state shared
/// by the managed CLI JSONL decoders.
///
/// One core is embedded per runtime decoder: the decoder owns its frame
/// dispatch and per-frame handling and delegates sequencing, draining, and
/// line classification here, so the `seq` high-water-mark contract (design
/// §5.5) and the fixed-diagnostic tolerance policy stay single-sourced.
#[derive(Debug)]
pub(crate) struct JsonlDecoderCore {
    next_seq: u64,
    session_id: Option<String>,
    pending: Vec<ExternalObservedEvent>,
    consecutive_non_json_lines: usize,
}

impl JsonlDecoderCore {
    /// Creates a core for a fresh session: the `seq` line starts at 0, no
    /// session id is known, and no observations are buffered.
    pub(crate) fn new() -> Self {
        Self {
            next_seq: 0,
            session_id: None,
            pending: Vec::new(),
            consecutive_non_json_lines: 0,
        }
    }

    /// Seeds the `seq` line at `next_seq`, for a session resumed across
    /// processes.
    ///
    /// The machine's replay dedup keeps only observations with `seq` greater
    /// than the persisted [`ExternalSessionRef::last_event_seq`] high-water
    /// mark, so a resumed session must continue the seq line where the previous
    /// process left off instead of restarting at 0 — otherwise every
    /// post-resume observation would be silently dropped as a false duplicate
    /// (design §5.5).
    ///
    /// [`ExternalSessionRef::last_event_seq`]: crate::agent::external::ExternalSessionRef::last_event_seq
    pub(crate) fn with_next_seq(mut self, next_seq: u64) -> Self {
        self.next_seq = next_seq;
        self
    }

    /// Returns the sequence number the next emitted observation will carry.
    pub(crate) fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Returns the runtime-assigned session id, once a frame has reported one.
    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Records the runtime-assigned session id reported by a frame.
    pub(crate) fn set_session_id(&mut self, session_id: String) {
        self.session_id = Some(session_id);
    }

    /// Drains the observations buffered since the last drain, transferring
    /// ownership to the caller and leaving the running `seq` untouched.
    pub(crate) fn take_observations(&mut self) -> Vec<ExternalObservedEvent> {
        std::mem::take(&mut self.pending)
    }

    /// Buffers `event` under the next monotonic sequence number.
    pub(crate) fn emit(&mut self, event: ExternalAgentEvent) {
        self.pending
            .push(ExternalObservedEvent::new(self.next_seq, event));
        self.next_seq += 1;
    }

    /// Classifies one raw JSONL line, returning the frame's `type` tag and its
    /// object for the runtime decoder to dispatch on.
    ///
    /// Returns `Ok(None)` for a blank line or a tolerated non-JSON noise line
    /// (bounded by [`MAX_CONSECUTIVE_NON_JSON_LINES`]); a valid JSON line
    /// resets the noise counter. `frame_name` labels the fixed diagnostics
    /// (e.g. `"claude stream-json"`).
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Protocol`] after too many consecutive
    /// non-JSON lines, for a JSON line that is not an object, or for one
    /// missing a string `type`.
    pub(crate) fn parse_frame(&mut self, line: &str, frame_name: &'static str) -> ParseFrameResult {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => {
                self.consecutive_non_json_lines = 0;
                value
            }
            Err(_) => return self.tolerate_non_json_line(frame_name),
        };
        let Value::Object(frame) = value else {
            return Err(protocol(format!("{frame_name} frame is not a JSON object")));
        };
        let Some(frame_type) = frame.get("type").and_then(Value::as_str).map(str::to_owned) else {
            return Err(protocol(format!(
                "{frame_name} frame is missing a string `type`"
            )));
        };
        Ok(Some((frame_type, frame)))
    }

    /// Counts one non-JSON line, tolerating it up to the bounded noise budget.
    fn tolerate_non_json_line(&mut self, frame_name: &'static str) -> ParseFrameResult {
        self.consecutive_non_json_lines = self.consecutive_non_json_lines.saturating_add(1);
        if self.consecutive_non_json_lines <= MAX_CONSECUTIVE_NON_JSON_LINES {
            return Ok(None);
        }
        Err(protocol(format!(
            "too many consecutive non-json {frame_name} lines ({}/{})",
            self.consecutive_non_json_lines, MAX_CONSECUTIVE_NON_JSON_LINES
        )))
    }
}

/// Builds an [`ExternalAgentError::Protocol`] from a fixed diagnostic.
fn protocol(detail: impl Into<String>) -> ExternalAgentError {
    ExternalAgentError::Protocol {
        detail: detail.into(),
    }
}
