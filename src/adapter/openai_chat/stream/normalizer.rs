//! Stateful conversion from Chat/Completions SSE chunks to normalized events.
//!
//! Each chunk's `choices[0].delta` is folded into the normalized stream event
//! taxonomy (design doc §4.4):
//!
//! - `delta.content` appends to one assistant text block;
//! - `delta.reasoning_content` appends to one reasoning block (no signature);
//! - `delta.tool_calls` are keyed by `index`: the first fragment for an index
//!   opens a tool-input block (carrying `id` + `function.name`), subsequent
//!   fragments append raw `function.arguments` string deltas — the JSON is
//!   **never** parsed mid-stream, only by the accumulator at block stop;
//! - the terminal `finish_reason` is cached as the message stop reason;
//! - the standalone usage chunk (empty `choices`, present after `include_usage`)
//!   emits one additive usage segment.
//!
//! The `data: [DONE]` sentinel is recognized before JSON decoding (§4.4.1) so
//! the non-JSON sentinel never surfaces as a parse error. Because the usage
//! chunk arrives *after* the `finish_reason` chunk, the message-stop event is
//! deferred to the sentinel: closing every still-open block and emitting the
//! cached stop reason last keeps additive usage ahead of the stop reason, which
//! the accumulator requires. A stream that ends without the sentinel surfaces as
//! an incomplete-stream error instead.

use super::{
    invalid_stream,
    wire::{Choice, ToolCallDelta, decode},
};
use crate::{
    adapter::openai_chat::response::convert::normalize_finish_reason,
    client::ClientError,
    model::{
        message::Role,
        normalized::{Normalized, StopReason},
        usage::Usage,
    },
    stream::{BlockId, BlockKind, Delta as BlockDelta, StreamEvent},
};
use eventsource_stream::Event;

/// Stateful conversion from chat/completions chunks to normalized events.
#[derive(Default)]
pub(super) struct StreamNormalizer {
    /// Set once the `data: [DONE]` sentinel terminates the stream normally.
    terminal: bool,
    /// Set once the assistant message-start event has been emitted.
    message_started: bool,
    /// Stable id of the currently open text block, if any.
    active_text: Option<BlockId>,
    /// Stable id of the currently open reasoning block, if any.
    active_reasoning: Option<BlockId>,
    /// One entry per tool-call `index` whose block has started but not stopped.
    tool_calls: Vec<ToolCallState>,
    /// Cached terminal stop reason, emitted with the deferred message stop.
    stop_reason: Option<Normalized<StopReason>>,
}

/// Tracks one open tool-input block keyed by its wire `index`.
struct ToolCallState {
    /// Wire `index` correlating fragments for this tool call.
    index: u64,
    /// Stable block identifier shared by all events for this tool call.
    block_id: BlockId,
}

impl StreamNormalizer {
    /// Translates one fully framed SSE event into zero or more normalized events.
    pub(super) fn translate(&mut self, event: Event) -> Result<Vec<StreamEvent>, ClientError> {
        if self.terminal {
            return Err(invalid_stream(
                "received a chunk after the [DONE] sentinel".to_owned(),
            ));
        }

        let mut events = Vec::new();

        // The `data: [DONE]` sentinel is not JSON; terminate the stream before
        // JSON decoding so it never surfaces as a parse error (design §4.4.1).
        // This is the only place a message-stop event is emitted: deferring it
        // here keeps additive usage (which arrives after `finish_reason`) ahead
        // of the stop reason, which the accumulator requires.
        if event.data.trim() == "[DONE]" {
            self.terminal = true;
            self.ensure_message_started(&mut events);
            self.close_open_blocks(&mut events);
            events.push(StreamEvent::MessageStop {
                stop_reason: self
                    .stop_reason
                    .take()
                    .unwrap_or_else(|| Normalized::without_raw(StopReason::Other)),
            });
            return Ok(events);
        }

        let chunk = decode(&event.data).map_err(invalid_stream)?;

        // The standalone usage chunk arrives after `finish_reason` with empty
        // `choices`; emit its usage immediately so it precedes the deferred stop.
        if let Some(usage) = chunk.usage {
            let parsed = serde_json::from_value::<Usage>(usage)
                .map_err(|error| invalid_stream(format!("invalid usage object: {error}")))?;
            events.push(StreamEvent::Usage(parsed));
        }

        if let Some(choice) = chunk.choices.into_iter().next() {
            self.translate_choice(&mut events, choice)?;
        }

        Ok(events)
    }

    /// Reports whether the `[DONE]` sentinel has already ended the stream.
    pub(super) fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Builds the error emitted when the byte stream ends before `[DONE]`.
    pub(super) fn incomplete_error(&self) -> ClientError {
        invalid_stream("SSE body ended before the [DONE] sentinel".to_owned())
    }

    /// Emits a message-start event exactly once, deferring to the first chunk or
    /// the `[DONE]` flush for an empty stream.
    fn ensure_message_started(&mut self, events: &mut Vec<StreamEvent>) {
        if !self.message_started {
            events.push(StreamEvent::MessageStart {
                role: Role::Assistant,
            });
            self.message_started = true;
        }
    }

    /// Folds one choice delta into start/delta events and caches `finish_reason`.
    fn translate_choice(
        &mut self,
        events: &mut Vec<StreamEvent>,
        choice: Choice,
    ) -> Result<(), ClientError> {
        self.ensure_message_started(events);

        let Choice {
            delta,
            finish_reason,
        } = choice;

        // The streamed role is always `assistant`; validate defensively so a
        // stray non-assistant tag surfaces as a protocol error rather than
        // silently folding into an assistant message.
        if let Some(role) = delta.role.as_deref()
            && role != "assistant"
        {
            return Err(invalid_stream(format!(
                "delta role must be `assistant`, got `{role}`"
            )));
        }

        if let Some(content) = delta.content.as_deref()
            && !content.is_empty()
        {
            self.push_text_delta(events, content);
        }

        if let Some(reasoning) = delta.reasoning_content.as_deref()
            && !reasoning.is_empty()
        {
            self.push_reasoning_delta(events, reasoning);
        }

        if let Some(tool_calls) = delta.tool_calls {
            for tool_call in tool_calls {
                self.push_tool_call(events, tool_call)?;
            }
        }

        if let Some(finish_reason) = finish_reason.as_deref() {
            self.stop_reason = Some(normalize_finish_reason(Some(finish_reason)));
        }

        Ok(())
    }

    /// Appends one text fragment, opening the text block lazily on first use.
    fn push_text_delta(&mut self, events: &mut Vec<StreamEvent>, content: &str) {
        let id = self.text_block_id(events);
        events.push(StreamEvent::BlockDelta {
            id,
            delta: BlockDelta::Text(content.to_owned()),
        });
    }

    /// Appends one reasoning fragment, opening the reasoning block lazily.
    fn push_reasoning_delta(&mut self, events: &mut Vec<StreamEvent>, reasoning: &str) {
        let id = self.reasoning_block_id(events);
        events.push(StreamEvent::BlockDelta {
            id,
            delta: BlockDelta::Reasoning(reasoning.to_owned()),
        });
    }

    /// Returns the active text block id, starting a text block if none is open.
    fn text_block_id(&mut self, events: &mut Vec<StreamEvent>) -> BlockId {
        if let Some(id) = &self.active_text {
            return id.clone();
        }
        let id = BlockId::new("text");
        events.push(StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::Text,
        });
        self.active_text = Some(id.clone());
        id
    }

    /// Returns the active reasoning block id, starting one if none is open.
    fn reasoning_block_id(&mut self, events: &mut Vec<StreamEvent>) -> BlockId {
        if let Some(id) = &self.active_reasoning {
            return id.clone();
        }
        let id = BlockId::new("reasoning");
        events.push(StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::Reasoning,
        });
        self.active_reasoning = Some(id.clone());
        id
    }

    /// Folds one indexed tool-call delta, opening a tool-input block on the
    /// first fragment for an `index` and appending raw JSON argument fragments.
    fn push_tool_call(
        &mut self,
        events: &mut Vec<StreamEvent>,
        delta: ToolCallDelta,
    ) -> Result<(), ClientError> {
        let block_id = match self
            .tool_calls
            .iter()
            .find(|state| state.index == delta.index)
            .map(|state| state.block_id.clone())
        {
            Some(block_id) => block_id,
            None => self.start_tool_call(events, &delta)?,
        };

        // Subsequent fragments only ever carry `function.arguments`; append them
        // as raw JSON deltas and never parse mid-stream (design §4.4.2).
        if let Some(function) = delta.function.as_ref()
            && let Some(arguments) = function.arguments.as_deref()
            && !arguments.is_empty()
        {
            events.push(StreamEvent::BlockDelta {
                id: block_id,
                delta: BlockDelta::Json(arguments.to_owned()),
            });
        }
        Ok(())
    }

    /// Opens a tool-input block for a new `index`, requiring the first fragment
    /// to carry both the tool-call id and the function name.
    fn start_tool_call(
        &mut self,
        events: &mut Vec<StreamEvent>,
        delta: &ToolCallDelta,
    ) -> Result<BlockId, ClientError> {
        let tool_call_id = delta.id.as_deref().ok_or_else(|| {
            invalid_stream(format!(
                "first tool_call fragment for index {} must carry `id`",
                delta.index
            ))
        })?;
        let function = delta.function.as_ref().ok_or_else(|| {
            invalid_stream(format!(
                "first tool_call fragment for index {} must carry `function`",
                delta.index
            ))
        })?;
        let tool_name = function.name.as_deref().ok_or_else(|| {
            invalid_stream(format!(
                "first tool_call fragment for index {} must carry `function.name`",
                delta.index
            ))
        })?;

        let block_id = BlockId::new(format!("tool-call-{}", delta.index));
        events.push(StreamEvent::BlockStart {
            id: block_id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: tool_name.to_owned(),
                tool_call_id: tool_call_id.to_owned(),
            },
        });
        self.tool_calls.push(ToolCallState {
            index: delta.index,
            block_id: block_id.clone(),
        });
        Ok(block_id)
    }

    /// Stops every still-open block in a fixed order before the message stops.
    fn close_open_blocks(&mut self, events: &mut Vec<StreamEvent>) {
        if let Some(id) = self.active_reasoning.take() {
            events.push(StreamEvent::BlockStop { id });
        }
        if let Some(id) = self.active_text.take() {
            events.push(StreamEvent::BlockStop { id });
        }
        for state in self.tool_calls.drain(..) {
            events.push(StreamEvent::BlockStop { id: state.block_id });
        }
    }
}
