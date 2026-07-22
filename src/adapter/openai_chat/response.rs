//! OpenAI Chat/Completions complete-response parsing and non-streaming transport.
//!
//! This module owns the `chat()` entry point and the complete-response wire
//! parser. M1-2 nails down the stream-flag mutual exclusion guard shared with
//! the streaming path; request building arrives through `build_request` (M1-3)
//! and the HTTP transport is driven by [`crate::adapter::common::execute_json_response`].

use super::OpenAiChatAdapter;
use crate::{
    adapter::common,
    client::{ChatRequest, ClientError, Response},
    model::{
        message::{Message, Role},
        usage::Usage,
    },
};
use serde_json::{Map, Value};

// `pub(crate)` so the streaming terminal path (§4.4.4) can reuse the same
// `normalize_finish_reason` mapping instead of duplicating it; the module's
// other helpers stay `pub(super)`/private and remain response-only.
pub(crate) mod convert;

use convert::{convert_message, normalize_finish_reason};

impl OpenAiChatAdapter {
    /// Parses one complete chat/completions JSON body.
    ///
    /// `choices[0].message` becomes ordered normalized content blocks (reasoning,
    /// text, then tool calls), `finish_reason` is mapped through the design
    /// doc §4.3 table, and remaining top-level wire evidence (including
    /// `choices` with its `logprobs`) is retained in [`Response::extra`].
    pub fn parse_response(body: &[u8]) -> Result<Response, ClientError> {
        let value: Value = serde_json::from_slice(body).map_err(|error| {
            invalid_response(format!(
                "failed to deserialize response JSON at line {}, column {}: {error}",
                error.line(),
                error.column()
            ))
        })?;

        parse_response_value(value)
    }

    /// Executes one native non-streaming OpenAI Chat/Completions request.
    ///
    /// Callers must set [`ChatRequest::stream`] to `false`; SSE responses are
    /// handled by [`OpenAiChatAdapter::chat_stream`].
    ///
    /// The whole request is bounded by a 10-minute total timeout; the connect
    /// phase and non-2xx error bodies have their own tighter limits (see
    /// [`OpenAiChatAdapter::new`]).
    pub async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        if request.stream {
            return Err(invalid_response(
                "non-streaming chat requires ChatRequest.stream to be false".to_owned(),
            ));
        }

        let request = self.build_request(&request)?;
        common::execute_json_response(&self.http_client, request, Self::parse_response).await
    }
}

/// Converts one already-deserialized complete chat/completions response object.
///
/// Kept separate from the byte-oriented [`OpenAiChatAdapter::parse_response`] so
/// the streaming terminal path (which embeds the same response shape) can reuse
/// identical message, stop-reason, and usage handling in M3.
pub(super) fn parse_response_value(value: Value) -> Result<Response, ClientError> {
    let Value::Object(mut wire) = value else {
        return Err(invalid_response(
            "response JSON must be an object".to_owned(),
        ));
    };

    match wire.get("object") {
        Some(Value::String(object)) if object == "chat.completion" => {}
        Some(Value::String(object)) => {
            return Err(invalid_response(format!(
                "response object must be `chat.completion`, got `{object}`"
            )));
        }
        Some(_) => {
            return Err(invalid_response(
                "response field `object` must be a string".to_owned(),
            ));
        }
        None => {
            return Err(invalid_response(
                "response field `object` is required".to_owned(),
            ));
        }
    }

    let usage = take_usage(&mut wire)?;
    let (message, finish_reason) = read_choice(&wire)?;
    let content = convert_message(&message)?;
    let stop_reason = normalize_finish_reason(finish_reason.as_deref());

    Ok(Response {
        message: Message {
            role: Role::Assistant,
            content,
        },
        usage,
        stop_reason,
        extra: wire,
    })
}

/// Deserializes the optional chat/completions usage object through the shared
/// model, leaving its unmodeled fields in [`Usage::extra`].
fn take_usage(wire: &mut Map<String, Value>) -> Result<Usage, ClientError> {
    match wire.remove("usage") {
        None | Some(Value::Null) => Ok(Usage::default()),
        Some(value) => serde_json::from_value(value)
            .map_err(|error| invalid_response(format!("invalid usage object: {error}"))),
    }
}

/// Reads `choices[0]` (message plus `finish_reason`) without consuming `choices`
/// so the choice-level wire evidence — including `logprobs` (design doc §2.2) —
/// survives in [`Response::extra`].
fn read_choice(wire: &Map<String, Value>) -> Result<(Value, Option<String>), ClientError> {
    let Some(choices) = wire.get("choices") else {
        return Err(invalid_response(
            "response field `choices` is required".to_owned(),
        ));
    };
    let Value::Array(choices) = choices else {
        return Err(invalid_response(
            "response field `choices` must be an array".to_owned(),
        ));
    };
    if choices.is_empty() {
        return Err(invalid_response(
            "response field `choices` must contain at least one choice".to_owned(),
        ));
    }
    let Some(Value::Object(choice)) = choices.first() else {
        return Err(invalid_response("choices[0] must be an object".to_owned()));
    };
    let Some(message) = choice.get("message") else {
        return Err(invalid_response(
            "choices[0].message is required".to_owned(),
        ));
    };
    let finish_reason = match choice.get("finish_reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            return Err(invalid_response(
                "choices[0].finish_reason must be a string or null".to_owned(),
            ));
        }
    };
    Ok((message.clone(), finish_reason))
}

/// Adds Chat/Completions response context to protocol conversion failures.
///
/// Visible to the parent module so the convert sub-module reuses the same
/// classification for wire-to-normalized block conversion.
pub(super) fn invalid_response(message: String) -> ClientError {
    ClientError::Protocol(format!(
        "invalid OpenAI Chat/Completions response: {message}"
    ))
}

#[cfg(test)]
mod tests;
