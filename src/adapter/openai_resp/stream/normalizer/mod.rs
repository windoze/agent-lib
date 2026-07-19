//! Stateful Responses event validation and normalized-event translation.

mod event_fields;
mod item;
mod terminal;

use super::{
    invalid_stream,
    wire::{
        ArgumentsDoneEvent, ContentPartEvent, DecodedEvent, DeltaEvent, OutputItemEvent,
        ResponseEvent, TextDoneEvent, WireEvent, decode,
    },
};
use crate::{client::ClientError, model::message::Role, stream::StreamEvent};
use event_fields::{
    item_id, kind_event_name, reasoning_event_name, reject_all_part_indices, reject_summary_index,
    reject_text_summary_index, required_index,
};
use eventsource_stream::Event;
use item::{ActiveItem, PartKind};
use serde_json::Value;
use std::collections::HashMap;
use terminal::{classify_provider_error, validate_created_placeholders, validate_response_object};

/// Stateful conversion from Responses SSE payloads to normalized events.
#[derive(Default)]
pub(super) struct StreamNormalizer {
    response_id: Option<String>,
    terminal: bool,
    next_sequence: u64,
    items: HashMap<u64, ActiveItem>,
    item_indices: HashMap<String, u64>,
    unmodeled_events: Vec<Value>,
}

impl StreamNormalizer {
    /// Decodes, sequences, validates, and translates one fully framed SSE
    /// event.
    pub(super) fn translate(&mut self, event: Event) -> Result<Vec<StreamEvent>, ClientError> {
        if self.terminal {
            return Err(invalid_stream(
                "received an event after response completion or error".to_owned(),
            ));
        }

        let decoded = decode(&event.data).map_err(invalid_stream)?;
        if event.event != "message" && event.event != decoded.kind {
            return Err(invalid_stream(format!(
                "SSE event field `{}` disagrees with payload type `{}`",
                event.event, decoded.kind
            )));
        }
        self.validate_sequence(decoded.event.sequence_number(), &decoded.kind)?;
        self.translate_wire(decoded)
    }

    /// Dispatches one typed wire event after framing and sequence checks.
    fn translate_wire(&mut self, decoded: DecodedEvent) -> Result<Vec<StreamEvent>, ClientError> {
        match decoded.event {
            WireEvent::ResponseCreated(event) => self.start_response(event),
            WireEvent::ResponseInProgress(event) => {
                self.validate_response_snapshot(&event.response, "in_progress")?;
                Ok(Vec::new())
            }
            WireEvent::OutputItemAdded(event) => self.start_item(event),
            WireEvent::ContentPartAdded(event) => self.start_content(event),
            WireEvent::OutputTextDelta(event) => {
                self.push_message_delta(event, PartKind::OutputText)
            }
            WireEvent::RefusalDelta(event) => self.push_message_delta(event, PartKind::Refusal),
            WireEvent::OutputTextDone(event) => {
                self.finish_message_text(event, PartKind::OutputText)
            }
            WireEvent::RefusalDone(event) => self.finish_refusal(event),
            WireEvent::ContentPartDone(event) => self.finish_content(event),
            WireEvent::ReasoningTextDelta(event) => self.push_reasoning(event, false),
            WireEvent::ReasoningSummaryTextDelta(event) => self.push_reasoning(event, true),
            WireEvent::ReasoningTextDone(event) => self.finish_reasoning(event, false),
            WireEvent::ReasoningSummaryTextDone(event) => self.finish_reasoning(event, true),
            WireEvent::FunctionArgumentsDelta(event) => self.push_arguments(event),
            WireEvent::FunctionArgumentsDone(event) => self.finish_arguments(event),
            WireEvent::OutputItemDone(event) => self.finish_item(event),
            WireEvent::ResponseCompleted(event) => self.finish_response(event, "completed"),
            WireEvent::ResponseIncomplete(event) => self.finish_response(event, "incomplete"),
            WireEvent::ResponseFailed(event) => self.fail_response(event, decoded.raw),
            WireEvent::Error(_) => {
                self.terminal = true;
                Ok(vec![StreamEvent::Error(classify_provider_error(
                    &decoded.raw,
                ))])
            }
            WireEvent::Unknown(_) => {
                self.unmodeled_events.push(decoded.raw);
                Ok(Vec::new())
            }
        }
    }

    /// Starts the normalized assistant message from `response.created`.
    fn start_response(&mut self, event: ResponseEvent) -> Result<Vec<StreamEvent>, ClientError> {
        if self.response_id.is_some() {
            return Err(invalid_stream(
                "received more than one response.created event".to_owned(),
            ));
        }
        let id = validate_response_object(&event.response, Some("in_progress"))?;
        validate_created_placeholders(&event.response)?;
        self.response_id = Some(id);

        Ok(vec![StreamEvent::MessageStart {
            role: Role::Assistant,
        }])
    }

    /// Creates stable item state for one output-array position.
    fn start_item(&mut self, event: OutputItemEvent) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_started("response.output_item.added")?;
        if self.items.contains_key(&event.output_index) {
            return Err(invalid_stream(format!(
                "output index {} was added more than once",
                event.output_index
            )));
        }

        let (item, events) = ActiveItem::start(event.output_index, event.item)?;
        if let Some(previous) = self
            .item_indices
            .insert(item.id().to_owned(), event.output_index)
        {
            return Err(invalid_stream(format!(
                "output item id `{}` was reused at indices {previous} and {}",
                item.id(),
                event.output_index
            )));
        }
        self.items.insert(event.output_index, item);
        Ok(events)
    }

    /// Starts one nested content part under an output-message item.
    fn start_content(&mut self, event: ContentPartEvent) -> Result<Vec<StreamEvent>, ClientError> {
        let item = self.item_mut(
            &event.item_id,
            event.output_index,
            "response.content_part.added",
        )?;
        item.add_content_part(event.content_index, event.part)
    }

    /// Appends one assistant-visible output-text or refusal fragment.
    fn push_message_delta(
        &mut self,
        event: DeltaEvent,
        kind: PartKind,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        reject_summary_index(&event, kind_event_name(kind, false))?;
        let content_index = required_index(
            event.content_index,
            "content_index",
            kind_event_name(kind, false),
        )?;
        let item = self.item_mut(
            &event.item_id,
            event.output_index,
            kind_event_name(kind, false),
        )?;
        item.push_message_delta(content_index, event.delta, kind)
    }

    /// Checks one output-text done event against accumulated fragments.
    fn finish_message_text(
        &mut self,
        event: TextDoneEvent,
        kind: PartKind,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        reject_text_summary_index(&event, kind_event_name(kind, true))?;
        let content_index = required_index(
            event.content_index,
            "content_index",
            kind_event_name(kind, true),
        )?;
        let item = self.item_mut(
            &event.item_id,
            event.output_index,
            kind_event_name(kind, true),
        )?;
        item.finish_message_text(content_index, &event.text, kind)?;
        Ok(Vec::new())
    }

    /// Checks one refusal done event against accumulated fragments.
    fn finish_refusal(
        &mut self,
        event: super::wire::RefusalDoneEvent,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        let item = self.item_mut(&event.item_id, event.output_index, "response.refusal.done")?;
        item.finish_message_text(event.content_index, &event.refusal, PartKind::Refusal)?;
        Ok(Vec::new())
    }

    /// Closes one nested message content part.
    fn finish_content(&mut self, event: ContentPartEvent) -> Result<Vec<StreamEvent>, ClientError> {
        let item = self.item_mut(
            &event.item_id,
            event.output_index,
            "response.content_part.done",
        )?;
        item.finish_content_part(event.content_index, event.part)
    }

    /// Appends raw reasoning or buffers summary reasoning according to the
    /// complete-response raw-first rule.
    fn push_reasoning(
        &mut self,
        event: DeltaEvent,
        summary: bool,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        let event_name = reasoning_event_name(summary, false);
        let part_index = if summary {
            if event.content_index.is_some() {
                return Err(invalid_stream(format!(
                    "{event_name} must not contain content_index"
                )));
            }
            required_index(event.summary_index, "summary_index", event_name)?
        } else {
            if event.summary_index.is_some() {
                return Err(invalid_stream(format!(
                    "{event_name} must not contain summary_index"
                )));
            }
            required_index(event.content_index, "content_index", event_name)?
        };
        let item = self.item_mut(&event.item_id, event.output_index, event_name)?;
        item.push_reasoning_delta(part_index, event.delta, summary)
    }

    /// Checks a reasoning done value against accumulated fragments.
    fn finish_reasoning(
        &mut self,
        event: TextDoneEvent,
        summary: bool,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        let event_name = reasoning_event_name(summary, true);
        let part_index = if summary {
            if event.content_index.is_some() {
                return Err(invalid_stream(format!(
                    "{event_name} must not contain content_index"
                )));
            }
            required_index(event.summary_index, "summary_index", event_name)?
        } else {
            if event.summary_index.is_some() {
                return Err(invalid_stream(format!(
                    "{event_name} must not contain summary_index"
                )));
            }
            required_index(event.content_index, "content_index", event_name)?
        };
        let item = self.item_mut(&event.item_id, event.output_index, event_name)?;
        item.finish_reasoning_text(part_index, &event.text, summary)?;
        Ok(Vec::new())
    }

    /// Appends one raw tool-argument JSON fragment.
    fn push_arguments(&mut self, event: DeltaEvent) -> Result<Vec<StreamEvent>, ClientError> {
        reject_all_part_indices(&event, "response.function_call_arguments.delta")?;
        let item = self.item_mut(
            &event.item_id,
            event.output_index,
            "response.function_call_arguments.delta",
        )?;
        item.push_arguments_delta(event.delta)
    }

    /// Publishes parsed tool input at the function arguments complete boundary.
    fn finish_arguments(
        &mut self,
        event: ArgumentsDoneEvent,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        let item = self.item_mut(
            &event.item_id,
            event.output_index,
            "response.function_call_arguments.done",
        )?;
        item.finish_arguments(event.arguments)
    }

    /// Closes one output item and any item-level normalized block.
    fn finish_item(&mut self, event: OutputItemEvent) -> Result<Vec<StreamEvent>, ClientError> {
        let id = item_id(&event.item, "response.output_item.done")?;
        let item = self.item_mut(&id, event.output_index, "response.output_item.done")?;
        item.finish_item(event.item)
    }

    /// Looks up one item while verifying the redundant provider id/index pair.
    fn item_mut(
        &mut self,
        item_id: &str,
        output_index: u64,
        event: &str,
    ) -> Result<&mut ActiveItem, ClientError> {
        self.require_started(event)?;
        let mapped_index = self.item_indices.get(item_id).ok_or_else(|| {
            invalid_stream(format!("{event} referenced unknown item id `{item_id}`"))
        })?;
        if *mapped_index != output_index {
            return Err(invalid_stream(format!(
                "{event} item id `{item_id}` maps to output index {mapped_index}, not {output_index}"
            )));
        }
        let item = self.items.get_mut(&output_index).ok_or_else(|| {
            invalid_stream(format!(
                "{event} referenced unknown output index {output_index}"
            ))
        })?;
        debug_assert_eq!(item.output_index(), output_index);
        Ok(item)
    }

    /// Requires `response.created` before content or terminal events.
    fn require_started(&self, event: &str) -> Result<(), ClientError> {
        if self.response_id.is_some() {
            Ok(())
        } else {
            Err(invalid_stream(format!(
                "received {event} before response.created"
            )))
        }
    }

    /// Enforces contiguous zero-based sequence numbers when the endpoint
    /// supplies them; compatible endpoints may omit the field entirely.
    fn validate_sequence(&mut self, actual: Option<u64>, kind: &str) -> Result<(), ClientError> {
        if let Some(actual) = actual
            && actual != self.next_sequence
        {
            return Err(invalid_stream(format!(
                "event `{kind}` has sequence number {actual}; expected {}",
                self.next_sequence
            )));
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| invalid_stream("event sequence number overflowed u64".to_owned()))?;
        Ok(())
    }

    /// Reports whether a normal terminal response or provider error ended the
    /// stream.
    pub(super) fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Explains why EOF before a terminal event violates the Responses
    /// lifecycle.
    pub(super) fn incomplete_error(&self) -> ClientError {
        if self.response_id.is_none() {
            invalid_stream("SSE body ended before response.created".to_owned())
        } else if let Some((index, item)) = self
            .items
            .iter()
            .filter(|(_, item)| !item.is_done())
            .min_by_key(|(index, _)| *index)
        {
            invalid_stream(format!(
                "SSE body ended before output item `{}` at index {index} completed",
                item.id()
            ))
        } else {
            invalid_stream("SSE body ended before a terminal response event".to_owned())
        }
    }
}
