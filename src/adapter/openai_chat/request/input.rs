//! Mapping from provider-neutral messages and tools to chat/completions wire.

use super::invalid_request;
use crate::{
    client::ClientError,
    model::{
        content::{ContentBlock, ImageSource},
        message::{Message, Role},
        tool::{Tool, ToolStatus},
    },
};
use serde_json::{Map, Value, json};

/// Expands one normalized message into one or more chat/completions messages.
///
/// A user or assistant message yields a single chat message; a tool message
/// yields one `{"role":"tool"}` message per tool result. System roles are
/// rejected — system instructions travel through `ChatRequest.system`.
pub(super) fn message_to_wire(index: usize, message: &Message) -> Result<Vec<Value>, ClientError> {
    match message.role {
        Role::System => Err(invalid_request(format!(
            "message {index} has system role; use ChatRequest.system instead"
        ))),
        Role::User => Ok(vec![user_message_to_wire(index, message)?]),
        Role::Assistant => Ok(vec![assistant_message_to_wire(index, message)?]),
        Role::Tool => tool_message_to_wire(index, message),
    }
}

/// Builds a `{"role":"user"}` chat message.
///
/// Plain-text messages use the compact string `content` form; messages that
/// carry an image or unknown block use the multimodal array form so vision
/// input survives the round trip.
fn user_message_to_wire(index: usize, message: &Message) -> Result<Value, ClientError> {
    let multimodal = message.content.iter().any(|block| {
        matches!(
            block,
            ContentBlock::Image { .. } | ContentBlock::Unknown { .. }
        )
    });

    let content = if multimodal {
        let mut parts = Vec::with_capacity(message.content.len());
        for (block_index, block) in message.content.iter().enumerate() {
            match block {
                ContentBlock::Text { .. } => parts.push(text_part(block)),
                ContentBlock::Image { .. } => parts.push(image_part(block)?),
                ContentBlock::Unknown { raw, .. } => parts.push(raw.clone()),
                _ => {
                    return Err(invalid_request(format!(
                        "message {index} block {block_index} is not valid for User role"
                    )));
                }
            }
        }
        Value::Array(parts)
    } else {
        let mut text = String::new();
        for (block_index, block) in message.content.iter().enumerate() {
            match block {
                ContentBlock::Text {
                    text: block_text, ..
                } => text.push_str(block_text),
                _ => {
                    return Err(invalid_request(format!(
                        "message {index} block {block_index} is not valid for User role"
                    )));
                }
            }
        }
        Value::String(text)
    };

    Ok(json!({ "role": "user", "content": content }))
}

/// Aggregates one assistant message into a single chat/completions assistant
/// message.
///
/// The chat/completions message model hangs `content`, `reasoning_content`,
/// and `tool_calls` off one message rather than spreading them across many, so
/// text/thinking/tool-use blocks on the same normalized message collapse into
/// one wire message. Reasoning is replayed unconditionally (design doc §5.1).
fn assistant_message_to_wire(index: usize, message: &Message) -> Result<Value, ClientError> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for (block_index, block) in message.content.iter().enumerate() {
        match block {
            ContentBlock::Text {
                text: block_text, ..
            } => content.push_str(block_text),
            ContentBlock::Thinking {
                text: block_text, ..
            } => reasoning.push_str(block_text),
            ContentBlock::ToolUse { .. } => tool_calls.push(tool_call_to_wire(block)?),
            _ => {
                return Err(invalid_request(format!(
                    "message {index} block {block_index} is not valid for Assistant role"
                )));
            }
        }
    }

    let mut fields = Map::new();
    insert_string(&mut fields, "role", "assistant");
    fields.insert(
        "content".to_owned(),
        if content.is_empty() {
            Value::Null
        } else {
            Value::String(content)
        },
    );
    if !reasoning.is_empty() {
        insert_string(&mut fields, "reasoning_content", &reasoning);
    }
    if !tool_calls.is_empty() {
        fields.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    Ok(Value::Object(fields))
}

/// Expands one tool message into one `{"role":"tool"}` chat message per tool
/// result.
///
/// Multimodal result content is flattened to text (images and unknown blocks
/// are dropped — lossy, accepted in the first phase); non-`Ok` outcomes are
/// surfaced to the model by prefixing the text (design doc §4.2).
fn tool_message_to_wire(index: usize, message: &Message) -> Result<Vec<Value>, ClientError> {
    let mut messages = Vec::new();
    for (block_index, block) in message.content.iter().enumerate() {
        let ContentBlock::ToolResult {
            tool_use_id,
            content,
            status,
            ..
        } = block
        else {
            return Err(invalid_request(format!(
                "message {index} block {block_index} is not valid for Tool role"
            )));
        };
        let text = flatten_tool_result_text(content, index, block_index)?;
        let content_value = match tool_result_status_marker(*status) {
            Some(marker) => Value::String(format!("[{marker}] {text}")),
            None => Value::String(text),
        };
        messages.push(json!({
            "role": "tool",
            "tool_call_id": tool_use_id,
            "content": content_value,
        }));
    }

    if messages.is_empty() {
        return Err(invalid_request(format!(
            "message {index} has tool role but contains no tool results"
        )));
    }
    Ok(messages)
}

/// Flattens multimodal tool result content into a single text string.
///
/// Image and unknown blocks are dropped because a chat/completions tool message
/// carries only a `content` string (lossy, accepted in the first phase).
fn flatten_tool_result_text(
    content: &[ContentBlock],
    index: usize,
    block_index: usize,
) -> Result<String, ClientError> {
    let mut parts: Vec<String> = Vec::new();
    for (sub_index, block) in content.iter().enumerate() {
        match block {
            ContentBlock::Text { text, .. } => parts.push(text.clone()),
            ContentBlock::Image { .. } | ContentBlock::Unknown { .. } => {}
            _ => {
                return Err(invalid_request(format!(
                    "message {index} block {block_index} result content {sub_index} must be text or image"
                )));
            }
        }
    }
    Ok(parts.join("\n"))
}

/// Maps a non-`Ok` outcome to a human-readable marker prepended to the tool
/// result text; `Ok` produces no marker (Anthropic `is_error` analogue).
fn tool_result_status_marker(status: ToolStatus) -> Option<&'static str> {
    match status {
        ToolStatus::Ok => None,
        ToolStatus::Error => Some("tool error"),
        ToolStatus::Denied => Some("tool denied"),
        ToolStatus::Cancelled => Some("tool cancelled"),
    }
}

/// Serializes one assistant tool call as a chat/completions `tool_calls` entry.
fn tool_call_to_wire(block: &ContentBlock) -> Result<Value, ClientError> {
    let ContentBlock::ToolUse {
        id, name, input, ..
    } = block
    else {
        unreachable!("tool-call converter received another content variant");
    };
    let arguments = serde_json::to_string(input)
        .map_err(|error| invalid_request(format!("failed to serialize tool input: {error}")))?;
    Ok(json!({
        "id": id,
        "type": "function",
        "function": { "name": name, "arguments": arguments }
    }))
}

/// Serializes one provider-neutral JSON Schema tool into chat/completions'
/// nested `function` shape (one `function` level deeper than Responses).
pub(super) fn tool_to_wire(tool: &Tool) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    })
}

/// Serializes one text block as a chat/completions text content part.
fn text_part(block: &ContentBlock) -> Value {
    let ContentBlock::Text { text, extra } = block else {
        unreachable!("text part converter received another content variant");
    };
    let mut fields = extra.clone();
    insert_string(&mut fields, "type", "text");
    insert_string(&mut fields, "text", text);
    Value::Object(fields)
}

/// Serializes one image block as a chat/completions `image_url` content part.
fn image_part(block: &ContentBlock) -> Result<Value, ClientError> {
    let ContentBlock::Image { source, extra } = block else {
        unreachable!("image part converter received another content variant");
    };
    let mut image_url = image_source_to_object(source);
    image_url.extend(extra.clone());
    let mut fields = Map::new();
    insert_string(&mut fields, "type", "image_url");
    fields.insert("image_url".to_owned(), Value::Object(image_url));
    Ok(Value::Object(fields))
}

/// Converts a URL or base64 image source into the `image_url` object payload.
fn image_source_to_object(source: &ImageSource) -> Map<String, Value> {
    match source {
        ImageSource::Url { url, extra } => {
            let mut fields = extra.clone();
            insert_string(&mut fields, "url", url);
            fields
        }
        ImageSource::Base64 {
            media_type,
            data,
            extra,
        } => {
            let mut fields = extra.clone();
            insert_string(
                &mut fields,
                "url",
                &format!("data:{media_type};base64,{data}"),
            );
            fields
        }
    }
}

/// Inserts a normalized string field after extras so modeled data wins.
fn insert_string(fields: &mut Map<String, Value>, key: &str, value: &str) {
    fields.insert(key.to_owned(), Value::String(value.to_owned()));
}
