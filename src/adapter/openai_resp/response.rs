//! OpenAI Responses complete-response parsing and non-streaming transport.

use super::OpenAiRespAdapter;
use crate::{
    client::{ChatRequest, ClientError, Response},
    model::{
        message::{Message, Role},
        usage::Usage,
    },
};
use reqwest::header::RETRY_AFTER;
use serde_json::{Map, Value};

mod convert;

use convert::{
    convert_output, normalize_stop_reason, response_incomplete_reason, response_is_filtered,
    take_required_array, take_required_string,
};

/// Top-level escape-hatch key for output item kinds the normalized model does
/// not yet represent.
const UNMODELED_OUTPUT_KEY: &str = "openai_unmodeled_output_items";

impl OpenAiRespAdapter {
    /// Parses one complete OpenAI Responses JSON body.
    ///
    /// Message, reasoning, and function-call items become ordered normalized
    /// content blocks. Provider metadata is retained at the closest available
    /// `extra` level, including Azure `content_filters` and unknown output item
    /// variants.
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

    /// Executes one native non-streaming OpenAI Responses request.
    ///
    /// Callers must set [`ChatRequest::stream`] to `false`; SSE responses are
    /// handled by [`OpenAiRespAdapter::chat_stream`].
    pub async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        if request.stream {
            return Err(invalid_response(
                "non-streaming chat requires ChatRequest.stream to be false".to_owned(),
            ));
        }

        let request = self.build_request(&request)?;
        let response = self
            .http_client
            .execute(request)
            .await
            .map_err(map_transport_error)?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.bytes().await.map_err(map_transport_error)?;

        if !status.is_success() {
            return Err(ClientError::from_http_response(
                status.as_u16(),
                String::from_utf8_lossy(&body),
                retry_after.as_deref(),
            ));
        }

        Self::parse_response(&body)
    }
}

/// Converts one already-deserialized complete response object.
///
/// Streaming terminal events embed the same response shape as the native
/// non-streaming endpoint. Keeping their conversion here guarantees identical
/// usage, stop-reason, and escape-hatch behavior across both paths.
pub(super) fn parse_response_value(value: Value) -> Result<Response, ClientError> {
    let Value::Object(mut wire) = value else {
        return Err(invalid_response(
            "response JSON must be an object".to_owned(),
        ));
    };

    match wire.get("object") {
        Some(Value::String(object)) if object == "response" => {}
        Some(Value::String(object)) => {
            return Err(invalid_response(format!(
                "response object must be `response`, got `{object}`"
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

    let status = take_required_string(&mut wire, "status", "response")?;
    let output = take_required_array(&mut wire, "output", "response")?;
    let usage = take_usage(&mut wire)?;
    let incomplete_reason = response_incomplete_reason(&wire)?;
    let mut converted = convert_output(output)?;
    if response_is_filtered(&wire)? {
        converted.refusal_raw = Some("content_filter".to_owned());
    }
    if !converted.unmodeled.is_empty() {
        insert_preserving_collision(
            &mut wire,
            UNMODELED_OUTPUT_KEY,
            Value::Array(converted.unmodeled),
        );
    }

    Ok(Response {
        message: Message {
            role: Role::Assistant,
            content: converted.content,
        },
        usage,
        stop_reason: normalize_stop_reason(
            &status,
            incomplete_reason.as_deref(),
            converted.has_tool_call,
            converted.refusal_raw.as_deref(),
        ),
        extra: wire,
    })
}

/// Deserializes the optional Responses usage object through the shared model.
fn take_usage(wire: &mut Map<String, Value>) -> Result<Usage, ClientError> {
    match wire.remove("usage") {
        None | Some(Value::Null) => Ok(Usage::default()),
        Some(value) => serde_json::from_value(value)
            .map_err(|error| invalid_response(format!("invalid usage object: {error}"))),
    }
}

/// Inserts adapter-owned evidence without discarding a colliding provider key.
fn insert_preserving_collision(fields: &mut Map<String, Value>, key: &str, value: Value) {
    if let Some(existing) = fields.remove(key) {
        fields.insert(key.to_owned(), Value::Array(vec![existing, value]));
    } else {
        fields.insert(key.to_owned(), value);
    }
}

/// Maps reqwest failures into retry-relevant client error classes.
fn map_transport_error(error: reqwest::Error) -> ClientError {
    if error.is_timeout() {
        ClientError::Timeout
    } else {
        ClientError::Network(error.to_string())
    }
}

/// Adds Responses context to protocol conversion failures.
pub(super) fn invalid_response(message: String) -> ClientError {
    ClientError::Protocol(format!("invalid OpenAI Responses response: {message}"))
}

#[cfg(test)]
mod tests;
