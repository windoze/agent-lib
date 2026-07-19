//! Conversion from complete Responses output items to normalized blocks.

use super::invalid_response;
use crate::{
    client::ClientError,
    model::{
        content::ContentBlock,
        normalized::{Normalized, StopReason},
    },
};
use serde_json::{Map, Value};

use crate::adapter::openai_resp::RESPONSE_EXTRA_KEY;

/// Accumulates normalized blocks and evidence while visiting output items.
#[derive(Default)]
pub(super) struct ConvertedOutput {
    pub(super) content: Vec<ContentBlock>,
    pub(super) has_tool_call: bool,
    pub(super) refusal_raw: Option<String>,
    pub(super) unmodeled: Vec<Value>,
}

/// Converts all output items without relying on array position as identity.
pub(super) fn convert_output(output: Vec<Value>) -> Result<ConvertedOutput, ClientError> {
    let mut converted = ConvertedOutput::default();

    for (index, item) in output.into_iter().enumerate() {
        convert_output_item(index, item, &mut converted)?;
    }

    Ok(converted)
}

/// Dispatches one typed output item to its normalized representation.
fn convert_output_item(
    index: usize,
    item: Value,
    converted: &mut ConvertedOutput,
) -> Result<(), ClientError> {
    let original = item.clone();
    let Value::Object(mut fields) = item else {
        return Err(invalid_response(format!(
            "output item {index} must be an object"
        )));
    };
    let item_type = take_required_string(&mut fields, "type", &format!("output item {index}"))?;

    match item_type.as_str() {
        "message" => convert_message_item(index, fields, original, converted),
        "reasoning" => convert_reasoning_item(index, fields, converted),
        "function_call" => convert_function_call_item(index, fields, converted),
        _ => {
            converted.unmodeled.push(original);
            Ok(())
        }
    }
}

/// Converts an assistant output-message item and all recognized content parts.
fn convert_message_item(
    index: usize,
    mut fields: Map<String, Value>,
    original: Value,
    converted: &mut ConvertedOutput,
) -> Result<(), ClientError> {
    let role = take_required_string(&mut fields, "role", &format!("message item {index}"))?;
    if role != "assistant" {
        return Err(invalid_response(format!(
            "message item {index} role must be `assistant`, got `{role}`"
        )));
    }
    let content = take_required_array(&mut fields, "content", &format!("message item {index}"))?;
    let content_was_empty = content.is_empty();
    fields.insert("type".to_owned(), Value::String("message".to_owned()));
    fields.insert("role".to_owned(), Value::String(role));

    for (content_index, part) in content.into_iter().enumerate() {
        let part_original = part.clone();
        let Value::Object(mut part_fields) = part else {
            return Err(invalid_response(format!(
                "message item {index} content {content_index} must be an object"
            )));
        };
        let part_type = take_required_string(
            &mut part_fields,
            "type",
            &format!("message item {index} content {content_index}"),
        )?;

        match part_type.as_str() {
            "output_text" => {
                let text = take_required_string(
                    &mut part_fields,
                    "text",
                    &format!("message item {index} content {content_index}"),
                )?;
                part_fields.insert("type".to_owned(), Value::String(part_type));
                converted.content.push(ContentBlock::Text {
                    text,
                    extra: response_block_extra(&fields, Some(part_fields)),
                });
            }
            "refusal" => {
                let refusal = take_required_string(
                    &mut part_fields,
                    "refusal",
                    &format!("message item {index} content {content_index}"),
                )?;
                part_fields.insert("type".to_owned(), Value::String(part_type));
                converted.content.push(ContentBlock::Text {
                    text: refusal,
                    extra: response_block_extra(&fields, Some(part_fields)),
                });
                converted.refusal_raw = Some("refusal".to_owned());
            }
            _ => converted.content.push(ContentBlock::Unknown {
                type_name: Some(part_type),
                raw: part_original,
            }),
        }
    }

    if content_was_empty {
        converted.unmodeled.push(original);
    }

    Ok(())
}

/// Converts a reasoning item while retaining its original structured arrays.
fn convert_reasoning_item(
    index: usize,
    mut fields: Map<String, Value>,
    converted: &mut ConvertedOutput,
) -> Result<(), ClientError> {
    let content_text = reasoning_text(&fields, "content", index)?;
    let summary_text = reasoning_text(&fields, "summary", index)?;
    let text = if content_text.is_empty() {
        summary_text
    } else {
        content_text
    };
    let signature = optional_string_field(&fields, "encrypted_content", index)?;
    fields.insert("type".to_owned(), Value::String("reasoning".to_owned()));

    converted.content.push(ContentBlock::Thinking {
        text,
        signature,
        extra: response_block_extra(&fields, None),
    });

    Ok(())
}

/// Converts a complete function call and parses its JSON arguments once.
fn convert_function_call_item(
    index: usize,
    mut fields: Map<String, Value>,
    converted: &mut ConvertedOutput,
) -> Result<(), ClientError> {
    let call_id = take_required_string(
        &mut fields,
        "call_id",
        &format!("function_call item {index}"),
    )?;
    let name = take_required_string(&mut fields, "name", &format!("function_call item {index}"))?;
    let arguments = take_required_string(
        &mut fields,
        "arguments",
        &format!("function_call item {index}"),
    )?;
    let input_json = if arguments.is_empty() {
        "{}"
    } else {
        arguments.as_str()
    };
    let input = serde_json::from_str(input_json).map_err(|error| {
        invalid_response(format!(
            "function_call item {index} has invalid JSON arguments: {error}"
        ))
    })?;
    fields.insert("type".to_owned(), Value::String("function_call".to_owned()));

    converted.content.push(ContentBlock::ToolUse {
        id: call_id,
        name,
        input,
        extra: response_block_extra(&fields, None),
    });
    converted.has_tool_call = true;

    Ok(())
}

/// Collects reasoning text from a typed `content` or `summary` array.
fn reasoning_text(
    fields: &Map<String, Value>,
    key: &str,
    item_index: usize,
) -> Result<String, ClientError> {
    let Some(value) = fields.get(key) else {
        return Ok(String::new());
    };
    if value.is_null() {
        return Ok(String::new());
    }
    let Value::Array(parts) = value else {
        return Err(invalid_response(format!(
            "reasoning item {item_index} field `{key}` must be an array"
        )));
    };
    let mut text = Vec::new();

    for (part_index, part) in parts.iter().enumerate() {
        let Value::Object(part) = part else {
            return Err(invalid_response(format!(
                "reasoning item {item_index} field `{key}` entry {part_index} must be an object"
            )));
        };
        let Some(value) = part.get("text") else {
            continue;
        };
        let Value::String(value) = value else {
            return Err(invalid_response(format!(
                "reasoning item {item_index} field `{key}` entry {part_index} text must be a string"
            )));
        };
        text.push(value.as_str());
    }

    Ok(text.join("\n"))
}

/// Reads an optional string field without consuming the retained wire map.
fn optional_string_field(
    fields: &Map<String, Value>,
    key: &str,
    item_index: usize,
) -> Result<Option<String>, ClientError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_response(format!(
            "reasoning item {item_index} field `{key}` must be a string or null"
        ))),
    }
}

/// Places item- and content-level wire evidence in one collision-free block
/// namespace so normalized field names remain authoritative.
fn response_block_extra(
    item: &Map<String, Value>,
    content: Option<Map<String, Value>>,
) -> Map<String, Value> {
    let mut provider = Map::new();
    provider.insert("item".to_owned(), Value::Object(item.clone()));
    if let Some(content) = content {
        provider.insert("content".to_owned(), Value::Object(content));
    }

    Map::from_iter([(RESPONSE_EXTRA_KEY.to_owned(), Value::Object(provider))])
}

/// Reads `incomplete_details.reason` while leaving the details in `extra`.
pub(super) fn response_incomplete_reason(
    wire: &Map<String, Value>,
) -> Result<Option<String>, ClientError> {
    match wire.get("incomplete_details") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(details)) => match details.get("reason") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(reason)) => Ok(Some(reason.clone())),
            Some(_) => Err(invalid_response(
                "incomplete_details.reason must be a string or null".to_owned(),
            )),
        },
        Some(_) => Err(invalid_response(
            "incomplete_details must be an object or null".to_owned(),
        )),
    }
}

/// Detects Azure/Foundry filter evidence without treating safe category
/// results as a refusal.
pub(super) fn response_is_filtered(wire: &Map<String, Value>) -> Result<bool, ClientError> {
    let Some(value) = wire.get("content_filters") else {
        return Ok(false);
    };
    let Value::Array(filters) = value else {
        return Err(invalid_response(
            "content_filters must be an array".to_owned(),
        ));
    };

    Ok(filters.iter().any(|filter| {
        filter
            .as_object()
            .and_then(|filter| filter.get("blocked"))
            .and_then(Value::as_bool)
            == Some(true)
    }))
}

/// Maps Responses completion status and output evidence into stable reasons.
pub(super) fn normalize_stop_reason(
    status: &str,
    incomplete_reason: Option<&str>,
    has_tool_call: bool,
    refusal_raw: Option<&str>,
) -> Normalized<StopReason> {
    if let Some(reason) = incomplete_reason {
        return match reason {
            "max_output_tokens" | "max_tokens" => {
                Normalized::from_mapped(StopReason::MaxTokens, reason)
            }
            "content_filter" | "content_filtered" => {
                Normalized::from_mapped(StopReason::Refusal, reason)
            }
            _ => Normalized::unknown(reason),
        };
    }
    if let Some(raw) = refusal_raw {
        return Normalized::from_mapped(StopReason::Refusal, raw);
    }
    if status == "completed" && has_tool_call {
        return Normalized::from_mapped(StopReason::ToolUse, status);
    }
    if status == "completed" {
        return Normalized::from_mapped(StopReason::EndTurn, status);
    }

    Normalized::unknown(status)
}

/// Removes a required string field and reports its wire location on failure.
pub(super) fn take_required_string(
    fields: &mut Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<String, ClientError> {
    match fields.remove(key) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(invalid_response(format!(
            "{context} field `{key}` must be a string"
        ))),
        None => Err(invalid_response(format!(
            "{context} field `{key}` is required"
        ))),
    }
}

/// Removes a required array field and reports its wire location on failure.
pub(super) fn take_required_array(
    fields: &mut Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Vec<Value>, ClientError> {
    match fields.remove(key) {
        Some(Value::Array(value)) => Ok(value),
        Some(_) => Err(invalid_response(format!(
            "{context} field `{key}` must be an array"
        ))),
        None => Err(invalid_response(format!(
            "{context} field `{key}` is required"
        ))),
    }
}
