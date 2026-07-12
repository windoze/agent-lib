//! Mapping from provider-neutral messages and tools to Responses input items.

use super::invalid_request;
use crate::{
    client::ClientError,
    model::{
        content::{ContentBlock, ImageSource},
        message::{Message, Role},
        tool::{Tool, ToolStatus},
    },
};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::adapter::openai_resp::RESPONSE_EXTRA_KEY;

/// Roles accepted by an easy Responses input-message item.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum OpenAiMessageRole {
    User,
    Assistant,
}

/// Text content kinds accepted inside an assistant input-message item.
#[derive(Clone, Copy)]
enum AssistantTextKind {
    OutputText,
    Refusal,
}

/// Expands one normalized message into ordered Responses input items.
pub(super) fn message_to_items(index: usize, message: &Message) -> Result<Vec<Value>, ClientError> {
    let role = match message.role {
        Role::User => OpenAiMessageRole::User,
        Role::Assistant => OpenAiMessageRole::Assistant,
        Role::Tool => OpenAiMessageRole::User,
        Role::System => {
            return Err(invalid_request(format!(
                "message {index} has system role; use ChatRequest.system instead"
            )));
        }
    };
    let mut items = Vec::new();
    let mut message_content = Vec::new();

    for (block_index, block) in message.content.iter().enumerate() {
        match block {
            ContentBlock::Text { .. } | ContentBlock::Image { .. }
                if message.role != Role::Tool =>
            {
                message_content.push(message_content_to_wire(block, role)?);
            }
            ContentBlock::ToolUse { .. } if message.role == Role::Assistant => {
                flush_message_item(&mut items, role, &mut message_content);
                items.push(tool_use_to_wire(block)?);
            }
            ContentBlock::ToolResult { .. } if message.role == Role::Tool => {
                flush_message_item(&mut items, role, &mut message_content);
                items.push(tool_result_to_wire(block)?);
            }
            ContentBlock::Thinking { .. } if message.role == Role::Assistant => {
                flush_message_item(&mut items, role, &mut message_content);
                items.push(reasoning_to_wire(block));
            }
            _ => {
                return Err(invalid_request(format!(
                    "message {index} block {block_index} is not valid for {:?} role",
                    message.role
                )));
            }
        }
    }

    flush_message_item(&mut items, role, &mut message_content);
    if items.is_empty() && message.content.is_empty() {
        if message.role == Role::Tool {
            return Err(invalid_request(format!(
                "message {index} has tool role but contains no tool results"
            )));
        }
        items.push(json!({ "role": role, "content": [] }));
    }

    Ok(items)
}

/// Emits one easy input-message item for a contiguous content run.
fn flush_message_item(items: &mut Vec<Value>, role: OpenAiMessageRole, content: &mut Vec<Value>) {
    if content.is_empty() {
        return;
    }

    items.push(json!({
        "role": role,
        "content": std::mem::take(content),
    }));
}

/// Converts normalized text or image content according to its message role.
fn message_content_to_wire(
    block: &ContentBlock,
    role: OpenAiMessageRole,
) -> Result<Value, ClientError> {
    let fields = match (role, block) {
        (OpenAiMessageRole::User, ContentBlock::Text { text, extra }) => {
            let mut fields = request_fields(extra);
            insert_string(&mut fields, "type", "input_text");
            insert_string(&mut fields, "text", text);
            fields
        }
        (OpenAiMessageRole::Assistant, ContentBlock::Text { text, extra }) => {
            assistant_text_fields(text, extra)?
        }
        (OpenAiMessageRole::User, ContentBlock::Image { source, extra }) => {
            let mut fields = image_source_fields(source);
            fields.extend(request_fields(extra));
            insert_string(&mut fields, "type", "input_image");
            fields
        }
        (OpenAiMessageRole::Assistant, ContentBlock::Image { .. }) => {
            return Err(invalid_request(
                "assistant image blocks cannot be represented: Responses assistant message content supports only output_text or refusal"
                    .to_owned(),
            ));
        }
        _ => unreachable!("only message-compatible content reaches this converter"),
    };

    Ok(Value::Object(fields))
}

/// Builds assistant text using Responses' role-specific content vocabulary.
fn assistant_text_fields(
    text: &str,
    extra: &Map<String, Value>,
) -> Result<Map<String, Value>, ClientError> {
    let kind = assistant_text_kind(extra)?;
    let mut fields = request_fields(extra);
    fields.remove("text");
    fields.remove("refusal");

    match kind {
        AssistantTextKind::OutputText => {
            insert_string(&mut fields, "type", "output_text");
            insert_string(&mut fields, "text", text);
        }
        AssistantTextKind::Refusal => {
            insert_string(&mut fields, "type", "refusal");
            insert_string(&mut fields, "refusal", text);
        }
    }

    Ok(fields)
}

/// Recovers refusal evidence retained by the response parser; normalized
/// assistant text without such evidence is ordinary `output_text`.
fn assistant_text_kind(extra: &Map<String, Value>) -> Result<AssistantTextKind, ClientError> {
    let Some(provider) = extra.get(RESPONSE_EXTRA_KEY) else {
        return Ok(AssistantTextKind::OutputText);
    };
    let Value::Object(provider) = provider else {
        return Err(invalid_request(format!(
            "assistant text `{RESPONSE_EXTRA_KEY}` replay metadata must be an object"
        )));
    };
    let Some(content) = provider.get("content") else {
        return Ok(AssistantTextKind::OutputText);
    };
    let Value::Object(content) = content else {
        return Err(invalid_request(
            "assistant text replay metadata field `content` must be an object".to_owned(),
        ));
    };

    match content.get("type") {
        None => Ok(AssistantTextKind::OutputText),
        Some(Value::String(value)) => match value.as_str() {
            "output_text" => Ok(AssistantTextKind::OutputText),
            "refusal" => Ok(AssistantTextKind::Refusal),
            _ => Err(invalid_request(format!(
                "assistant text replay metadata has unsupported content type `{value}`"
            ))),
        },
        Some(_) => Err(invalid_request(
            "assistant text replay metadata field `content.type` must be a string".to_owned(),
        )),
    }
}

/// Converts a URL or base64 image into Responses' `image_url` field.
fn image_source_fields(source: &ImageSource) -> Map<String, Value> {
    match source {
        ImageSource::Url { url, extra } => {
            let mut fields = request_fields(extra);
            insert_string(&mut fields, "image_url", url);
            fields
        }
        ImageSource::Base64 {
            media_type,
            data,
            extra,
        } => {
            let mut fields = request_fields(extra);
            insert_string(
                &mut fields,
                "image_url",
                &format!("data:{media_type};base64,{data}"),
            );
            fields
        }
    }
}

/// Converts one normalized tool call into a `function_call` input item.
fn tool_use_to_wire(block: &ContentBlock) -> Result<Value, ClientError> {
    let ContentBlock::ToolUse {
        id,
        name,
        input,
        extra,
    } = block
    else {
        unreachable!("tool-use converter received another content variant");
    };
    let arguments = serde_json::to_string(input)
        .map_err(|error| invalid_request(format!("failed to serialize tool input: {error}")))?;
    let mut fields = replayable_item_fields(extra);
    insert_string(&mut fields, "type", "function_call");
    insert_string(&mut fields, "call_id", id);
    insert_string(&mut fields, "name", name);
    insert_string(&mut fields, "arguments", &arguments);

    Ok(Value::Object(fields))
}

/// Converts one normalized tool result into a `function_call_output` item.
fn tool_result_to_wire(block: &ContentBlock) -> Result<Value, ClientError> {
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        status,
        extra,
    } = block
    else {
        unreachable!("tool-result converter received another content variant");
    };
    let output = tool_output_to_wire(content)?;
    let mut fields = replayable_item_fields(extra);
    fields.remove("is_error");
    insert_string(&mut fields, "type", "function_call_output");
    insert_string(&mut fields, "call_id", tool_use_id);
    fields.insert("output".to_owned(), output);
    insert_string(&mut fields, "status", openai_tool_result_status(*status));

    Ok(Value::Object(fields))
}

/// Degrades normalized result outcomes to the two terminal states accepted by
/// a Responses `function_call_output` item.
fn openai_tool_result_status(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Ok => "completed",
        ToolStatus::Error | ToolStatus::Denied | ToolStatus::Cancelled => "incomplete",
    }
}

/// Uses the compact string form for a plain result and the multimodal list
/// form whenever preserving multiple blocks or content metadata requires it.
fn tool_output_to_wire(content: &[ContentBlock]) -> Result<Value, ClientError> {
    if let [ContentBlock::Text { text, extra }] = content
        && request_fields(extra).is_empty()
    {
        return Ok(Value::String(text.clone()));
    }

    content
        .iter()
        .enumerate()
        .map(|(index, block)| match block {
            ContentBlock::Text { .. } | ContentBlock::Image { .. } => {
                message_content_to_wire(block, OpenAiMessageRole::User)
            }
            _ => Err(invalid_request(format!(
                "tool result content block {index} must be text or image"
            ))),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

/// Converts normalized reasoning into a replayable Responses reasoning item.
fn reasoning_to_wire(block: &ContentBlock) -> Value {
    let ContentBlock::Thinking {
        text,
        signature,
        extra,
    } = block
    else {
        unreachable!("reasoning converter received another content variant");
    };
    let mut fields = replayable_item_fields(extra);
    insert_string(&mut fields, "type", "reasoning");
    if !fields.contains_key("content") && !fields.contains_key("summary") {
        fields.insert(
            "summary".to_owned(),
            Value::Array(vec![json!({ "type": "summary_text", "text": text })]),
        );
    }
    if let Some(signature) = signature {
        insert_string(&mut fields, "encrypted_content", signature);
    }

    Value::Object(fields)
}

/// Converts one provider-neutral JSON Schema tool into Responses' flat shape.
pub(super) fn tool_to_wire(tool: &Tool) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
    })
}

/// Copies request-owned extras while withholding response-only replay metadata.
fn request_fields(extra: &Map<String, Value>) -> Map<String, Value> {
    let mut fields = extra.clone();
    fields.remove(RESPONSE_EXTRA_KEY);
    fields
}

/// Restores item metadata produced by this adapter while preserving any
/// caller-supplied request fields. Modeled fields are inserted afterward and
/// therefore remain authoritative.
fn replayable_item_fields(extra: &Map<String, Value>) -> Map<String, Value> {
    let mut fields = request_fields(extra);
    let Some(Value::Object(mut provider)) = extra.get(RESPONSE_EXTRA_KEY).cloned() else {
        return fields;
    };
    if let Some(Value::Object(item)) = provider.remove("item") {
        fields.extend(item);
    }
    fields
}

/// Inserts a normalized string field after extras so modeled data wins.
fn insert_string(fields: &mut Map<String, Value>, key: &str, value: &str) {
    fields.insert(key.to_owned(), Value::String(value.to_owned()));
}
