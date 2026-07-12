//! Per-output-item lifecycle state for Responses stream normalization.

mod part;
mod reasoning;
mod value;

use super::invalid_stream;
use crate::{
    client::ClientError,
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use part::ActivePart;
pub(super) use part::PartKind;
use reasoning::IndexedText;
use serde_json::Value;
use std::collections::HashMap;
use value::{
    compare_string, item_block_id, object, optional_parts, optional_string, require_item_type,
    required_array, required_string, validate_empty_array,
};

/// One output item retained from `added` through `done` and the terminal
/// response snapshot.
pub(super) struct ActiveItem {
    id: String,
    output_index: u64,
    kind: ActiveItemKind,
    done_item: Option<Value>,
}

impl ActiveItem {
    /// Creates an item and emits the normalized block start appropriate for
    /// message, reasoning, or function-call output.
    pub(super) fn start(
        output_index: u64,
        item: Value,
    ) -> Result<(Self, Vec<StreamEvent>), ClientError> {
        let fields = object(&item, &format!("output item {output_index}"))?;
        let id = required_string(fields, "id", &format!("output item {output_index}"))?;
        let item_type = required_string(fields, "type", &format!("output item {output_index}"))?;
        let block_id = item_block_id(&id);

        let (kind, events) = match item_type.as_str() {
            "message" => {
                let role = required_string(fields, "role", &format!("message item `{id}`"))?;
                if role != "assistant" {
                    return Err(invalid_stream(format!(
                        "message item `{id}` role must be `assistant`, got `{role}`"
                    )));
                }
                validate_empty_array(fields, "content", &format!("message item `{id}`"))?;
                (
                    ActiveItemKind::Message {
                        parts: HashMap::new(),
                    },
                    Vec::new(),
                )
            }
            "reasoning" => {
                let raw = IndexedText::from_parts(
                    optional_parts(fields, "content", &format!("reasoning item `{id}`"))?,
                    "content",
                    &id,
                )?;
                let summary = IndexedText::from_parts(
                    optional_parts(fields, "summary", &format!("reasoning item `{id}`"))?,
                    "summary",
                    &id,
                )?;
                let signature = optional_string(fields, "encrypted_content", &id)?;
                let mut events = vec![StreamEvent::BlockStart {
                    id: block_id.clone(),
                    kind: BlockKind::Reasoning,
                }];
                events.extend(raw.replay(&block_id, Delta::Reasoning));
                (
                    ActiveItemKind::Reasoning {
                        block_id,
                        raw,
                        summary,
                        signature,
                    },
                    events,
                )
            }
            "function_call" => {
                let name = required_string(fields, "name", &format!("function_call `{id}`"))?;
                let call_id = required_string(fields, "call_id", &format!("function_call `{id}`"))?;
                let arguments =
                    required_string(fields, "arguments", &format!("function_call `{id}`"))?;
                let mut events = vec![StreamEvent::BlockStart {
                    id: block_id.clone(),
                    kind: BlockKind::ToolInput {
                        tool_name: name.clone(),
                        tool_call_id: call_id.clone(),
                    },
                }];
                if !arguments.is_empty() {
                    events.push(StreamEvent::BlockDelta {
                        id: block_id.clone(),
                        delta: Delta::Json(arguments.clone()),
                    });
                }
                (
                    ActiveItemKind::FunctionCall {
                        block_id,
                        name,
                        call_id,
                        arguments,
                        input_available: false,
                    },
                    events,
                )
            }
            _ => (ActiveItemKind::Unsupported { item_type }, Vec::new()),
        };

        Ok((
            Self {
                id,
                output_index,
                kind,
                done_item: None,
            },
            events,
        ))
    }

    /// Borrows the provider item id used by subsequent delta events.
    pub(super) fn id(&self) -> &str {
        &self.id
    }

    /// Returns the provider output-array index assigned at item start.
    pub(super) fn output_index(&self) -> u64 {
        self.output_index
    }

    /// Returns whether `response.output_item.done` has finalized this item.
    pub(super) fn is_done(&self) -> bool {
        self.done_item.is_some()
    }

    /// Adds a typed content part to an output-message item.
    pub(super) fn add_content_part(
        &mut self,
        content_index: u64,
        part: Value,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_open("response.content_part.added")?;
        let ActiveItemKind::Message { parts } = &mut self.kind else {
            return Err(invalid_stream(format!(
                "content part {content_index} targeted non-message item `{}`",
                self.id
            )));
        };
        if parts.contains_key(&content_index) {
            return Err(invalid_stream(format!(
                "message item `{}` content index {content_index} was added more than once",
                self.id
            )));
        }

        let (part, events) = ActivePart::start(&self.id, content_index, part)?;
        parts.insert(content_index, part);
        Ok(events)
    }

    /// Appends one visible-text or refusal delta to an existing message part.
    pub(super) fn push_message_delta(
        &mut self,
        content_index: u64,
        delta: String,
        expected: PartKind,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_open(expected.delta_event_name())?;
        let item_id = self.id.clone();
        let part = self.message_part_mut(content_index, expected.delta_event_name())?;
        part.push_delta(&item_id, content_index, delta, expected)
    }

    /// Checks an authoritative output-text or refusal done value without
    /// closing the content part before its own lifecycle event arrives.
    pub(super) fn finish_message_text(
        &mut self,
        content_index: u64,
        text: &str,
        expected: PartKind,
    ) -> Result<(), ClientError> {
        self.require_open(expected.done_event_name())?;
        let item_id = self.id.clone();
        let part = self.message_part_mut(content_index, expected.done_event_name())?;
        part.validate_text(&item_id, content_index, text, expected)
    }

    /// Validates and closes one message content part.
    pub(super) fn finish_content_part(
        &mut self,
        content_index: u64,
        part: Value,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_open("response.content_part.done")?;
        let item_id = self.id.clone();
        let active = self.message_part_mut(content_index, "response.content_part.done")?;
        active.finish(&item_id, content_index, part)
    }

    /// Appends raw or summary reasoning text.
    ///
    /// Raw reasoning is emitted immediately. Summary text is retained until
    /// item completion and emitted only when no raw reasoning exists, matching
    /// the complete-response converter's raw-first normalization rule.
    pub(super) fn push_reasoning_delta(
        &mut self,
        part_index: u64,
        delta: String,
        summary: bool,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_open(if summary {
            "response.reasoning_summary_text.delta"
        } else {
            "response.reasoning_text.delta"
        })?;
        let ActiveItemKind::Reasoning {
            block_id,
            raw,
            summary: summary_text,
            ..
        } = &mut self.kind
        else {
            return Err(invalid_stream(format!(
                "reasoning delta targeted non-reasoning item `{}`",
                self.id
            )));
        };

        if summary {
            summary_text.push(part_index, delta)?;
            Ok(Vec::new())
        } else {
            let emitted = raw.push(part_index, delta)?;
            Ok(vec![StreamEvent::BlockDelta {
                id: block_id.clone(),
                delta: Delta::Reasoning(emitted),
            }])
        }
    }

    /// Checks the provider's authoritative complete reasoning part.
    pub(super) fn finish_reasoning_text(
        &mut self,
        part_index: u64,
        text: &str,
        summary: bool,
    ) -> Result<(), ClientError> {
        self.require_open(if summary {
            "response.reasoning_summary_text.done"
        } else {
            "response.reasoning_text.done"
        })?;
        let ActiveItemKind::Reasoning {
            raw,
            summary: summary_text,
            ..
        } = &mut self.kind
        else {
            return Err(invalid_stream(format!(
                "reasoning done event targeted non-reasoning item `{}`",
                self.id
            )));
        };

        if summary {
            summary_text.validate(part_index, text, &self.id, "summary")
        } else {
            raw.validate(part_index, text, &self.id, "content")
        }
    }

    /// Appends one raw function-call arguments fragment without parsing it.
    pub(super) fn push_arguments_delta(
        &mut self,
        delta: String,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_open("response.function_call_arguments.delta")?;
        let ActiveItemKind::FunctionCall {
            block_id,
            arguments,
            input_available,
            ..
        } = &mut self.kind
        else {
            return Err(invalid_stream(format!(
                "function arguments delta targeted non-function item `{}`",
                self.id
            )));
        };
        if *input_available {
            return Err(invalid_stream(format!(
                "function arguments delta followed complete input for item `{}`",
                self.id
            )));
        }
        arguments.push_str(&delta);

        Ok(vec![StreamEvent::BlockDelta {
            id: block_id.clone(),
            delta: Delta::Json(delta),
        }])
    }

    /// Parses function-call arguments only at the provider's complete boundary.
    pub(super) fn finish_arguments(
        &mut self,
        complete: String,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_open("response.function_call_arguments.done")?;
        let ActiveItemKind::FunctionCall {
            block_id,
            arguments,
            input_available,
            ..
        } = &mut self.kind
        else {
            return Err(invalid_stream(format!(
                "function arguments done targeted non-function item `{}`",
                self.id
            )));
        };
        if *input_available {
            return Err(invalid_stream(format!(
                "function arguments for item `{}` completed more than once",
                self.id
            )));
        }

        let mut events = Vec::new();
        if arguments.is_empty() && !complete.is_empty() {
            arguments.push_str(&complete);
            events.push(StreamEvent::BlockDelta {
                id: block_id.clone(),
                delta: Delta::Json(complete.clone()),
            });
        } else if arguments != &complete {
            return Err(invalid_stream(format!(
                "function arguments done for item `{}` disagrees with accumulated deltas",
                self.id
            )));
        }
        let input: Value = serde_json::from_str(&complete).map_err(|error| {
            invalid_stream(format!(
                "function arguments for item `{}` are invalid JSON: {error}",
                self.id
            ))
        })?;
        *input_available = true;
        events.push(StreamEvent::ToolInputAvailable {
            id: block_id.clone(),
            input,
        });
        Ok(events)
    }

    /// Validates `output_item.done`, closes item-level blocks, and retains the
    /// authoritative item for comparison with the terminal response snapshot.
    pub(super) fn finish_item(&mut self, item: Value) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_open("response.output_item.done")?;
        let fields = object(&item, &format!("completed output item `{}`", self.id))?;
        let done_id = required_string(fields, "id", "completed output item")?;
        if done_id != self.id {
            return Err(invalid_stream(format!(
                "output_item.done id `{done_id}` disagrees with started item `{}`",
                self.id
            )));
        }
        let done_type = required_string(fields, "type", &format!("output item `{}`", self.id))?;
        let mut events = Vec::new();

        match &mut self.kind {
            ActiveItemKind::Message { parts } => {
                require_item_type(&self.id, &done_type, "message")?;
                let role = required_string(fields, "role", &format!("message item `{}`", self.id))?;
                if role != "assistant" {
                    return Err(invalid_stream(format!(
                        "completed message item `{}` role must be `assistant`, got `{role}`",
                        self.id
                    )));
                }
                let content =
                    required_array(fields, "content", &format!("message item `{}`", self.id))?;
                if content.len() != parts.len() {
                    return Err(invalid_stream(format!(
                        "completed message item `{}` has {} content parts but {} were streamed",
                        self.id,
                        content.len(),
                        parts.len()
                    )));
                }
                for (index, done_part) in content.iter().enumerate() {
                    let index = u64::try_from(index).map_err(|_| {
                        invalid_stream(format!("message item `{}` has too many parts", self.id))
                    })?;
                    let active = parts.get(&index).ok_or_else(|| {
                        invalid_stream(format!(
                            "completed message item `{}` contains unstarted content index {index}",
                            self.id
                        ))
                    })?;
                    active.validate_done_value(&self.id, index, done_part)?;
                }
            }
            ActiveItemKind::Reasoning {
                block_id,
                raw,
                summary,
                signature,
            } => {
                require_item_type(&self.id, &done_type, "reasoning")?;
                let done_raw = IndexedText::from_parts(
                    optional_parts(fields, "content", &format!("reasoning item `{}`", self.id))?,
                    "content",
                    &self.id,
                )?;
                let done_summary = IndexedText::from_parts(
                    optional_parts(fields, "summary", &format!("reasoning item `{}`", self.id))?,
                    "summary",
                    &self.id,
                )?;
                if raw.joined() != done_raw.joined() {
                    return Err(invalid_stream(format!(
                        "completed reasoning content for item `{}` disagrees with streamed deltas",
                        self.id
                    )));
                }
                if summary.joined() != done_summary.joined() {
                    return Err(invalid_stream(format!(
                        "completed reasoning summary for item `{}` disagrees with streamed deltas",
                        self.id
                    )));
                }
                let done_signature = optional_string(fields, "encrypted_content", &self.id)?;
                if signature.is_some() && done_signature != *signature {
                    return Err(invalid_stream(format!(
                        "completed reasoning signature for item `{}` disagrees with its start value",
                        self.id
                    )));
                }
                if raw.is_empty() {
                    events.extend(summary.replay(block_id, Delta::Reasoning));
                }
                if let Some(done_signature) = done_signature.filter(|value| !value.is_empty()) {
                    events.push(StreamEvent::BlockDelta {
                        id: block_id.clone(),
                        delta: Delta::ReasoningSignature(done_signature),
                    });
                }
                events.push(StreamEvent::BlockStop {
                    id: block_id.clone(),
                });
            }
            ActiveItemKind::FunctionCall {
                block_id,
                name,
                call_id,
                arguments,
                input_available,
            } => {
                require_item_type(&self.id, &done_type, "function_call")?;
                compare_string(fields, "name", name, &self.id)?;
                compare_string(fields, "call_id", call_id, &self.id)?;
                compare_string(fields, "arguments", arguments, &self.id)?;
                if !*input_available {
                    return Err(invalid_stream(format!(
                        "function item `{}` ended before arguments became available",
                        self.id
                    )));
                }
                events.push(StreamEvent::BlockStop {
                    id: block_id.clone(),
                });
            }
            ActiveItemKind::Unsupported { item_type } => {
                require_item_type(&self.id, &done_type, item_type)?;
            }
        }

        self.done_item = Some(item);
        Ok(events)
    }

    /// Ensures the terminal response contains the same authoritative item as
    /// the preceding `output_item.done` event.
    pub(super) fn validate_terminal_item(&self, item: &Value) -> Result<(), ClientError> {
        let done = self.done_item.as_ref().ok_or_else(|| {
            invalid_stream(format!(
                "terminal response arrived before output item `{}` completed",
                self.id
            ))
        })?;
        if done != item {
            return Err(invalid_stream(format!(
                "terminal response output item `{}` disagrees with output_item.done",
                self.id
            )));
        }
        Ok(())
    }

    /// Rejects deltas and duplicate done events after item completion.
    fn require_open(&self, event: &str) -> Result<(), ClientError> {
        if self.is_done() {
            Err(invalid_stream(format!(
                "received {event} after output item `{}` completed",
                self.id
            )))
        } else {
            Ok(())
        }
    }

    /// Returns one mutable message content part with contextual diagnostics.
    fn message_part_mut(
        &mut self,
        content_index: u64,
        event: &str,
    ) -> Result<&mut ActivePart, ClientError> {
        let ActiveItemKind::Message { parts } = &mut self.kind else {
            return Err(invalid_stream(format!(
                "{event} targeted non-message item `{}`",
                self.id
            )));
        };
        parts.get_mut(&content_index).ok_or_else(|| {
            invalid_stream(format!(
                "{event} referenced unstarted content index {content_index} for item `{}`",
                self.id
            ))
        })
    }
}

/// Output-item data needed until item completion.
enum ActiveItemKind {
    Message {
        parts: HashMap<u64, ActivePart>,
    },
    Reasoning {
        block_id: BlockId,
        raw: IndexedText,
        summary: IndexedText,
        signature: Option<String>,
    },
    FunctionCall {
        block_id: BlockId,
        name: String,
        call_id: String,
        arguments: String,
        input_available: bool,
    },
    Unsupported {
        item_type: String,
    },
}
