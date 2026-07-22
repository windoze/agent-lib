//! Conversion from one chat/completions assistant message to normalized blocks.
//!
//! The chat/completions message model hangs `content`, `reasoning_content`, and
//! `tool_calls` off a single message object rather than spreading them across
//! many. This module flattens one such message into ordered normalized content
//! blocks: reasoning first, then text, then tool calls. The reasoning-before-
//! text order matches the anthropic convention referenced in design doc §4.3,
//! and tool calls follow text to mirror the wire message field order.

use super::invalid_response;
use crate::{
    adapter::openai_chat::RESPONSE_EXTRA_KEY,
    client::ClientError,
    model::{
        content::ContentBlock,
        normalized::{Normalized, StopReason},
    },
};
use serde_json::{Map, Value};

/// Converts one assistant message object into ordered normalized blocks.
///
/// `reasoning_content` becomes a `Thinking` block first, then `content` becomes
/// a `Text` block, then each `tool_calls` entry becomes a `ToolUse` block. The
/// message is read by reference so the caller retains the original wire evidence
/// (including `logprobs` and any unmodeled fields) in `Response.extra` per
/// design doc §2.2/§4.3.
pub(super) fn convert_message(message: &Value) -> Result<Vec<ContentBlock>, ClientError> {
    let Value::Object(fields) = message else {
        return Err(invalid_response(
            "choices[0].message must be an object".to_owned(),
        ));
    };

    let role = required_string(fields, "role")?;
    if role != "assistant" {
        return Err(invalid_response(format!(
            "choices[0].message role must be `assistant`, got `{role}`"
        )));
    }

    let mut content = Vec::new();

    if let Some(reasoning) = optional_string_field(fields, "reasoning_content")?
        && !reasoning.is_empty()
    {
        content.push(ContentBlock::Thinking {
            text: reasoning,
            signature: None,
            extra: Map::new(),
        });
    }

    if let Some(text) = convert_content(fields.get("content"))? {
        content.push(ContentBlock::Text {
            text,
            extra: Map::new(),
        });
    }

    if let Some(tool_calls) = fields.get("tool_calls") {
        let Value::Array(tool_calls) = tool_calls else {
            return Err(invalid_response(
                "message field `tool_calls` must be an array".to_owned(),
            ));
        };
        for tool_call in tool_calls {
            content.push(convert_tool_call(tool_call)?);
        }
    }

    Ok(content)
}

/// Maps the chat/completions `finish_reason` into a stable stop reason.
///
/// Design doc §4.3 mapping table: `stop`→`EndTurn`, `length`→`MaxTokens`,
/// `tool_calls`→`ToolUse`, `content_filter`→`Refusal`, anything else (or
/// absence)→`Other`. The raw wire value is retained for diagnostics except when
/// `finish_reason` is missing.
pub(super) fn normalize_finish_reason(finish_reason: Option<&str>) -> Normalized<StopReason> {
    match finish_reason {
        Some("stop") => Normalized::from_mapped(StopReason::EndTurn, "stop"),
        Some("length") => Normalized::from_mapped(StopReason::MaxTokens, "length"),
        Some("tool_calls") => Normalized::from_mapped(StopReason::ToolUse, "tool_calls"),
        Some("content_filter") => Normalized::from_mapped(StopReason::Refusal, "content_filter"),
        Some(other) => Normalized::unknown(other),
        None => Normalized::without_raw(StopReason::Other),
    }
}

/// Reads a message `content` value into a single text string.
///
/// Phase-one assistant content is a plain string (or `null` when only tool
/// calls are present). The multimodal array form is supported defensively by
/// joining its `text` parts; non-text parts are dropped (lossy). Empty content
/// yields no text block.
fn convert_content(content: Option<&Value>) -> Result<Option<String>, ClientError> {
    let Some(content) = content else {
        return Ok(None);
    };
    match content {
        Value::Null => Ok(None),
        Value::String(text) => {
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text.clone()))
            }
        }
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                let Some(part) = part.as_object() else {
                    continue;
                };
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                if let Some(value) = part.get("text").and_then(Value::as_str) {
                    text.push_str(value);
                }
            }
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text))
            }
        }
        _ => Err(invalid_response(
            "message field `content` must be a string, null, or array".to_owned(),
        )),
    }
}

/// Converts one `tool_calls` entry into a tool-use block.
///
/// `function.arguments` is parsed once into JSON. Empty arguments become an
/// empty object; invalid JSON keeps the raw text in the block extra (design doc
/// §4.3) with the parsed input set to `null` so nothing is silently fabricated.
fn convert_tool_call(tool_call: &Value) -> Result<ContentBlock, ClientError> {
    let Value::Object(fields) = tool_call else {
        return Err(invalid_response(
            "tool_calls entry must be an object".to_owned(),
        ));
    };
    let id = required_string(fields, "id")?.to_owned();

    let function = match fields.get("function") {
        Some(Value::Object(function)) => function,
        Some(_) => {
            return Err(invalid_response(
                "tool_calls entry field `function` must be an object".to_owned(),
            ));
        }
        None => {
            return Err(invalid_response(
                "tool_calls entry field `function` is required".to_owned(),
            ));
        }
    };
    let name = required_string(function, "name")?.to_owned();
    let arguments = required_string(function, "arguments")?.to_owned();

    let (input, extra) = parse_arguments(&arguments);
    Ok(ContentBlock::ToolUse {
        id,
        name,
        input,
        extra,
    })
}

/// Parses tool-call arguments once, retaining raw text on failure (design §4.3).
fn parse_arguments(arguments: &str) -> (Value, Map<String, Value>) {
    if arguments.is_empty() {
        return (Value::Object(Map::new()), Map::new());
    }
    match serde_json::from_str::<Value>(arguments) {
        Ok(input) => (input, Map::new()),
        Err(_) => {
            let mut provider = Map::new();
            provider.insert(
                "raw_arguments".to_owned(),
                Value::String(arguments.to_owned()),
            );
            let mut extra = Map::new();
            extra.insert(RESPONSE_EXTRA_KEY.to_owned(), Value::Object(provider));
            (Value::Null, extra)
        }
    }
}

/// Reads a required string field from a borrowed map, reporting its wire name.
fn required_string<'a>(fields: &'a Map<String, Value>, key: &str) -> Result<&'a str, ClientError> {
    match fields.get(key) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(invalid_response(format!("field `{key}` must be a string"))),
        None => Err(invalid_response(format!("field `{key}` is required"))),
    }
}

/// Reads an optional string-or-null field from a borrowed map.
fn optional_string_field(
    fields: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, ClientError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_response(format!(
            "message field `{key}` must be a string or null"
        ))),
    }
}
