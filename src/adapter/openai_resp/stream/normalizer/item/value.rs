//! Shared output-item JSON accessors and stable block-id construction.

use crate::{client::ClientError, stream::BlockId};
use serde_json::{Map, Value};

use super::super::invalid_stream;

/// Returns an object map with location-aware protocol errors.
pub(super) fn object<'a>(
    value: &'a Value,
    context: &str,
) -> Result<&'a Map<String, Value>, ClientError> {
    value
        .as_object()
        .ok_or_else(|| invalid_stream(format!("{context} must be an object")))
}

/// Reads one required string without consuming retained provider evidence.
pub(super) fn required_string(
    fields: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<String, ClientError> {
    match fields.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(invalid_stream(format!(
            "{context} field `{key}` must be a string"
        ))),
        None => Err(invalid_stream(format!(
            "{context} field `{key}` is required"
        ))),
    }
}

/// Reads one optional nullable string.
pub(super) fn optional_string(
    fields: &Map<String, Value>,
    key: &str,
    item_id: &str,
) -> Result<Option<String>, ClientError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_stream(format!(
            "reasoning item `{item_id}` field `{key}` must be a string or null"
        ))),
    }
}

/// Reads one required array.
pub(super) fn required_array<'a>(
    fields: &'a Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<&'a [Value], ClientError> {
    match fields.get(key) {
        Some(Value::Array(values)) => Ok(values),
        Some(_) => Err(invalid_stream(format!(
            "{context} field `{key}` must be an array"
        ))),
        None => Err(invalid_stream(format!(
            "{context} field `{key}` is required"
        ))),
    }
}

/// Reads an optional nullable reasoning parts array.
pub(super) fn optional_parts<'a>(
    fields: &'a Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<&'a [Value], ClientError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(values)) => Ok(values),
        Some(_) => Err(invalid_stream(format!(
            "{context} field `{key}` must be an array or null"
        ))),
    }
}

/// Ensures an output-message placeholder does not contain unannounced parts.
pub(super) fn validate_empty_array(
    fields: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<(), ClientError> {
    let values = required_array(fields, key, context)?;
    if values.is_empty() {
        Ok(())
    } else {
        Err(invalid_stream(format!(
            "{context} field `{key}` must be empty at output_item.added"
        )))
    }
}

/// Checks that a done item retains its started wire type.
pub(super) fn require_item_type(
    item_id: &str,
    actual: &str,
    expected: &str,
) -> Result<(), ClientError> {
    if actual == expected {
        Ok(())
    } else {
        Err(invalid_stream(format!(
            "output_item.done type `{actual}` disagrees with started type `{expected}` for item `{item_id}`"
        )))
    }
}

/// Compares one completed function-call string field.
pub(super) fn compare_string(
    fields: &Map<String, Value>,
    key: &str,
    expected: &str,
    item_id: &str,
) -> Result<(), ClientError> {
    let actual = required_string(fields, key, &format!("function item `{item_id}`"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(invalid_stream(format!(
            "completed function item `{item_id}` field `{key}` disagrees with streamed value"
        )))
    }
}

/// Builds a stable normalized id for item-level reasoning and tool blocks.
pub(super) fn item_block_id(item_id: &str) -> BlockId {
    BlockId::new(format!("openai-response-item-{item_id}"))
}

/// Builds a stable normalized id for one message content part.
pub(super) fn content_block_id(item_id: &str, content_index: u64) -> BlockId {
    BlockId::new(format!(
        "openai-response-item-{item_id}-content-{content_index}"
    ))
}
