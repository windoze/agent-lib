//! Cross-event index validation and diagnostic event naming.

use super::{invalid_stream, item::PartKind};
use crate::client::ClientError;
use serde_json::Value;

use super::super::wire::{DeltaEvent, TextDoneEvent};

/// Reads the id needed to route an output-item done event.
pub(super) fn item_id(item: &Value, event: &str) -> Result<String, ClientError> {
    let fields = item
        .as_object()
        .ok_or_else(|| invalid_stream(format!("{event} item must be an object")))?;
    match fields.get("id") {
        Some(Value::String(id)) => Ok(id.clone()),
        Some(_) => Err(invalid_stream(format!(
            "{event} item field `id` must be a string"
        ))),
        None => Err(invalid_stream(format!(
            "{event} item field `id` is required"
        ))),
    }
}

/// Requires one content or summary index on a typed event.
pub(super) fn required_index(
    index: Option<u64>,
    field: &str,
    event: &str,
) -> Result<u64, ClientError> {
    index.ok_or_else(|| invalid_stream(format!("{event} field `{field}` is required")))
}

/// Rejects a summary index on message-content delta events.
pub(super) fn reject_summary_index(event: &DeltaEvent, name: &str) -> Result<(), ClientError> {
    if event.summary_index.is_some() {
        Err(invalid_stream(format!(
            "{name} must not contain summary_index"
        )))
    } else {
        Ok(())
    }
}

/// Rejects a summary index on message-content done events.
pub(super) fn reject_text_summary_index(
    event: &TextDoneEvent,
    name: &str,
) -> Result<(), ClientError> {
    if event.summary_index.is_some() {
        Err(invalid_stream(format!(
            "{name} must not contain summary_index"
        )))
    } else {
        Ok(())
    }
}

/// Function-call argument events are item-level and must not claim a nested
/// reasoning or message part.
pub(super) fn reject_all_part_indices(event: &DeltaEvent, name: &str) -> Result<(), ClientError> {
    if event.content_index.is_some() || event.summary_index.is_some() {
        Err(invalid_stream(format!(
            "{name} must not contain content_index or summary_index"
        )))
    } else {
        Ok(())
    }
}

/// Names text/refusal delta or done events for shared diagnostics.
pub(super) fn kind_event_name(kind: PartKind, done: bool) -> &'static str {
    match (kind, done) {
        (PartKind::OutputText, false) => "response.output_text.delta",
        (PartKind::OutputText, true) => "response.output_text.done",
        (PartKind::Refusal, false) => "response.refusal.delta",
        (PartKind::Refusal, true) => "response.refusal.done",
    }
}

/// Names raw/summary reasoning delta or done events for shared diagnostics.
pub(super) fn reasoning_event_name(summary: bool, done: bool) -> &'static str {
    match (summary, done) {
        (false, false) => "response.reasoning_text.delta",
        (false, true) => "response.reasoning_text.done",
        (true, false) => "response.reasoning_summary_text.delta",
        (true, true) => "response.reasoning_summary_text.done",
    }
}
