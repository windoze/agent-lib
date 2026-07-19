//! Stateful Anthropic wire-event validation and normalized event translation.

use super::{
    invalid_stream,
    usage::UsageTracker,
    wire::{ContentBlockDelta, ContentBlockStart, ErrorPayload, WireEvent},
};
use crate::{
    client::ClientError,
    model::{
        message::Role,
        normalized::{Normalized, StopReason},
    },
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use eventsource_stream::Event;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Stateful conversion from Anthropic event payloads to normalized events.
#[derive(Default)]
pub(super) struct StreamNormalizer {
    message_started: bool,
    terminal: bool,
    blocks: HashMap<u64, ActiveBlock>,
    stop_reason: Option<Normalized<StopReason>>,
    usage: UsageTracker,
}

impl StreamNormalizer {
    /// Decodes and validates one fully framed SSE event.
    pub(super) fn translate(&mut self, event: Event) -> Result<Vec<StreamEvent>, ClientError> {
        let wire: WireEvent = serde_json::from_str(&event.data).map_err(|error| {
            invalid_stream(format!(
                "failed to deserialize `{}` event JSON at line {}, column {}: {error}",
                event.event,
                error.line(),
                error.column()
            ))
        })?;
        let expected_event = wire.wire_type();
        if event.event != "message" && event.event != expected_event {
            return Err(invalid_stream(format!(
                "SSE event field `{}` disagrees with payload type `{expected_event}`",
                event.event
            )));
        }

        self.translate_wire(wire, &event.data)
    }

    /// Applies lifecycle checks and expands one wire payload into zero or more events.
    fn translate_wire(
        &mut self,
        wire: WireEvent,
        raw_data: &str,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        if self.terminal {
            return Err(invalid_stream(
                "received an event after message_stop or error".to_owned(),
            ));
        }

        match wire {
            WireEvent::MessageStart { message, extra } => {
                if self.message_started {
                    return Err(invalid_stream(
                        "received more than one message_start event".to_owned(),
                    ));
                }
                if message.role != Role::Assistant {
                    return Err(invalid_stream(format!(
                        "message_start reported role {:?}; expected assistant",
                        message.role
                    )));
                }

                let usage = self.usage.incremental(message.usage)?;
                let mut metadata = message_start_metadata(message.extra)?;
                metadata.extend(extra);
                self.message_started = true;
                let mut events = vec![
                    StreamEvent::MessageStart {
                        role: Role::Assistant,
                    },
                    StreamEvent::Usage(usage),
                ];
                push_metadata(&mut events, metadata);
                Ok(events)
            }
            WireEvent::ContentBlockStart {
                index,
                content_block,
                ..
            } => self.start_block(index, content_block),
            WireEvent::ContentBlockDelta { index, delta, .. } => self.push_delta(index, delta),
            WireEvent::ContentBlockStop { index, .. } => self.stop_block(index),
            WireEvent::MessageDelta {
                delta,
                usage,
                extra,
            } => {
                self.require_message_started("message_delta")?;
                if let Some(raw_reason) = delta.stop_reason {
                    if self.stop_reason.is_some() {
                        return Err(invalid_stream(
                            "received more than one stop reason".to_owned(),
                        ));
                    }
                    self.stop_reason = Some(StopReason::normalize(raw_reason));
                }

                let mut metadata = delta.extra;
                metadata.extend(extra);
                let mut events = Vec::new();
                if let Some(usage) = usage {
                    events.push(StreamEvent::Usage(self.usage.incremental(usage)?));
                }
                push_metadata(&mut events, metadata);
                Ok(events)
            }
            WireEvent::MessageStop { extra } => {
                self.require_message_started("message_stop")?;
                if let Some(index) = self.first_open_block() {
                    return Err(invalid_stream(format!(
                        "message_stop arrived before content block index {index} stopped"
                    )));
                }
                let stop_reason = self.stop_reason.clone().unwrap_or(Normalized {
                    value: StopReason::Other,
                    raw: None,
                });
                self.terminal = true;
                let mut events = Vec::with_capacity(2);
                push_metadata(&mut events, extra);
                events.push(StreamEvent::MessageStop { stop_reason });
                Ok(events)
            }
            WireEvent::Ping { .. } => Ok(Vec::new()),
            WireEvent::Error { error, .. } => {
                self.terminal = true;
                Ok(vec![StreamEvent::Error(classify_provider_error(
                    error, raw_data,
                ))])
            }
        }
    }

    /// Creates stable block state and emits any non-empty start payload.
    fn start_block(
        &mut self,
        index: u64,
        content_block: ContentBlockStart,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_message_started("content_block_start")?;
        if self.blocks.contains_key(&index) {
            return Err(invalid_stream(format!(
                "content block index {index} started more than once"
            )));
        }

        let id = block_id(index);
        let (kind, active_kind, initial_deltas) = match content_block {
            ContentBlockStart::Text { text, .. } => {
                let deltas = nonempty_delta(&id, text, Delta::Text);
                (BlockKind::Text, ActiveBlockKind::Text, deltas)
            }
            ContentBlockStart::Thinking {
                thinking,
                signature,
                ..
            } => {
                let mut deltas = nonempty_delta(&id, thinking, Delta::Reasoning);
                if let Some(signature) = signature {
                    deltas.extend(nonempty_delta(&id, signature, Delta::ReasoningSignature));
                }
                (BlockKind::Reasoning, ActiveBlockKind::Reasoning, deltas)
            }
            ContentBlockStart::ToolUse {
                id: tool_call_id,
                name,
                input,
                ..
            } => (
                BlockKind::ToolInput {
                    tool_name: name,
                    tool_call_id,
                },
                ActiveBlockKind::ToolInput {
                    initial_input: input,
                    json: String::new(),
                    saw_delta: false,
                },
                Vec::new(),
            ),
            ContentBlockStart::Unknown { type_name, raw } => (
                BlockKind::Unknown { type_name, raw },
                ActiveBlockKind::Unknown,
                Vec::new(),
            ),
        };

        self.blocks.insert(
            index,
            ActiveBlock {
                id: id.clone(),
                kind: active_kind,
                stopped: false,
            },
        );
        let mut events = Vec::with_capacity(1 + initial_deltas.len());
        events.push(StreamEvent::BlockStart { id, kind });
        events.extend(initial_deltas);
        Ok(events)
    }

    /// Validates a delta against its indexed block and retains tool fragments.
    fn push_delta(
        &mut self,
        index: u64,
        delta: ContentBlockDelta,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_message_started("content_block_delta")?;
        let delta_name = wire_delta_name(&delta);
        let block = self.blocks.get_mut(&index).ok_or_else(|| {
            invalid_stream(format!(
                "content_block_delta referenced unknown index {index}"
            ))
        })?;
        if block.stopped {
            return Err(invalid_stream(format!(
                "content_block_delta followed stop for index {index}"
            )));
        }

        let normalized = match (&mut block.kind, delta) {
            (ActiveBlockKind::Text, ContentBlockDelta::Text { text }) => Delta::Text(text),
            (ActiveBlockKind::Reasoning, ContentBlockDelta::Thinking { thinking }) => {
                Delta::Reasoning(thinking)
            }
            (ActiveBlockKind::Reasoning, ContentBlockDelta::Signature { signature }) => {
                Delta::ReasoningSignature(signature)
            }
            (
                ActiveBlockKind::ToolInput {
                    initial_input,
                    json,
                    saw_delta,
                },
                ContentBlockDelta::InputJson { partial_json },
            ) => {
                if !*saw_delta && !is_empty_object(initial_input) {
                    return Err(invalid_stream(format!(
                        "tool block index {index} supplied both complete start input and JSON deltas"
                    )));
                }
                *saw_delta = true;
                json.push_str(&partial_json);
                Delta::Json(partial_json)
            }
            (ActiveBlockKind::Unknown, ContentBlockDelta::Unknown { raw, .. }) => {
                Delta::Unknown(raw)
            }
            (kind, _) => {
                return Err(invalid_stream(format!(
                    "content block index {index} expects {} deltas but received {delta_name}",
                    kind.delta_name()
                )));
            }
        };

        Ok(vec![StreamEvent::BlockDelta {
            id: block.id.clone(),
            delta: normalized,
        }])
    }

    /// Finalizes one block and parses tool JSON only at this complete boundary.
    fn stop_block(&mut self, index: u64) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_message_started("content_block_stop")?;
        let block = self.blocks.get_mut(&index).ok_or_else(|| {
            invalid_stream(format!(
                "content_block_stop referenced unknown index {index}"
            ))
        })?;
        if block.stopped {
            return Err(invalid_stream(format!(
                "content block index {index} stopped more than once"
            )));
        }

        let tool_input = match &block.kind {
            ActiveBlockKind::ToolInput {
                initial_input,
                json,
                saw_delta,
            } => Some(if *saw_delta {
                serde_json::from_str(json).map_err(|error| {
                    invalid_stream(format!(
                        "tool input for block `{}` at index {index} is invalid JSON: {error}",
                        block.id
                    ))
                })?
            } else {
                initial_input.clone()
            }),
            ActiveBlockKind::Text | ActiveBlockKind::Reasoning | ActiveBlockKind::Unknown => None,
        };
        block.stopped = true;

        let mut events = Vec::with_capacity(1 + usize::from(tool_input.is_some()));
        if let Some(input) = tool_input {
            events.push(StreamEvent::ToolInputAvailable {
                id: block.id.clone(),
                input,
            });
        }
        events.push(StreamEvent::BlockStop {
            id: block.id.clone(),
        });
        Ok(events)
    }

    /// Rejects content or final events before a message start.
    fn require_message_started(&self, event: &str) -> Result<(), ClientError> {
        if self.message_started {
            Ok(())
        } else {
            Err(invalid_stream(format!(
                "received {event} before message_start"
            )))
        }
    }

    /// Returns the lowest still-open provider block index, if any.
    fn first_open_block(&self) -> Option<u64> {
        self.blocks
            .iter()
            .filter_map(|(index, block)| (!block.stopped).then_some(*index))
            .min()
    }

    /// Reports whether a normal stop or provider error ended the stream.
    pub(super) fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Explains why the byte stream ending at the current state is invalid.
    pub(super) fn incomplete_error(&self) -> ClientError {
        if !self.message_started {
            invalid_stream("SSE body ended before message_start".to_owned())
        } else if let Some(index) = self.first_open_block() {
            invalid_stream(format!(
                "SSE body ended before content block index {index} stopped"
            ))
        } else {
            invalid_stream("SSE body ended before message_stop".to_owned())
        }
    }
}

/// Provider block state retained until `content_block_stop`.
struct ActiveBlock {
    id: BlockId,
    kind: ActiveBlockKind,
    stopped: bool,
}

/// Data needed to validate deltas and finalize one provider block.
enum ActiveBlockKind {
    Text,
    Reasoning,
    ToolInput {
        initial_input: Value,
        json: String,
        saw_delta: bool,
    },
    Unknown,
}

impl ActiveBlockKind {
    /// Names accepted wire deltas for diagnostics.
    fn delta_name(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Reasoning => "thinking or signature",
            Self::ToolInput { .. } => "input_json",
            Self::Unknown => "unknown",
        }
    }
}

/// Removes message-start placeholders already represented by normalized state.
fn message_start_metadata(
    mut extra: Map<String, Value>,
) -> Result<Map<String, Value>, ClientError> {
    if let Some(content) = extra.remove("content")
        && !matches!(content, Value::Array(blocks) if blocks.is_empty())
    {
        return Err(invalid_stream(
            "message_start content must be an empty array".to_owned(),
        ));
    }
    if let Some(stop_reason) = extra.remove("stop_reason")
        && !stop_reason.is_null()
    {
        return Err(invalid_stream(
            "message_start stop_reason must be null".to_owned(),
        ));
    }

    Ok(extra)
}

/// Emits non-empty provider metadata without inventing placeholder events.
fn push_metadata(events: &mut Vec<StreamEvent>, extra: Map<String, Value>) {
    if !extra.is_empty() {
        events.push(StreamEvent::ResponseMetadata { extra });
    }
}

/// Builds the stable per-stream identifier for an Anthropic block index.
fn block_id(index: u64) -> BlockId {
    BlockId::new(format!("anthropic-block-{index}"))
}

/// Emits one start payload as a delta only when it contains real content.
fn nonempty_delta(
    id: &BlockId,
    value: String,
    into_delta: impl FnOnce(String) -> Delta,
) -> Vec<StreamEvent> {
    if value.is_empty() {
        Vec::new()
    } else {
        vec![StreamEvent::BlockDelta {
            id: id.clone(),
            delta: into_delta(value),
        }]
    }
}

/// Recognizes Anthropic's normal empty tool-input start placeholder.
fn is_empty_object(value: &Value) -> bool {
    matches!(value, Value::Object(fields) if fields.is_empty())
}

/// Names a provider delta variant before it is moved into normalization.
fn wire_delta_name(delta: &ContentBlockDelta) -> &'static str {
    match delta {
        ContentBlockDelta::Text { .. } => "text",
        ContentBlockDelta::InputJson { .. } => "input_json",
        ContentBlockDelta::Thinking { .. } => "thinking",
        ContentBlockDelta::Signature { .. } => "signature",
        ContentBlockDelta::Unknown { .. } => "unknown",
    }
}

/// Classifies an Anthropic error event while retaining its raw JSON payload.
fn classify_provider_error(error: ErrorPayload, raw_data: &str) -> ClientError {
    match error.kind.as_str() {
        "rate_limit_error" => ClientError::RateLimited { retry_after: None },
        "authentication_error" | "permission_error" => ClientError::Auth,
        "overloaded_error" => ClientError::Api {
            status: 529,
            body: raw_data.to_owned(),
        },
        _ => ClientError::from_http_response(400, raw_data, None),
    }
}
