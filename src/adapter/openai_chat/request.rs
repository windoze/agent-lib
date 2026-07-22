//! OpenAI Chat/Completions request serialization and HTTP request construction.

use super::OpenAiChatAdapter;
use crate::{
    adapter::common,
    client::{ChatRequest, ClientError},
    model::extras::{ProviderExtrasMergeOutcome, ProviderId},
};
use reqwest::Request;
use serde::Serialize;
use serde_json::{Value, json};

mod input;

use input::{message_to_wire, tool_to_wire};

impl OpenAiChatAdapter {
    /// Builds a `POST /chat/completions` request without sending it.
    ///
    /// Provider-neutral messages are expanded into chat/completions `messages`,
    /// `stream_options.include_usage` is injected for streaming requests, and
    /// matching provider extras are merged at the final JSON boundary. Endpoint
    /// authentication, headers, and query parameters are applied to the buffered
    /// reqwest request.
    pub fn build_request(&self, request: &ChatRequest) -> Result<Request, ClientError> {
        let body = serialize_body(request)?;
        let url = common::endpoint_url(&self.endpoint, &["chat", "completions"], invalid_endpoint)?;
        let headers = common::endpoint_headers(&self.endpoint, invalid_endpoint)?;

        self.http_client
            .post(url)
            .headers(headers)
            .json(&body)
            .build()
            .map_err(|error| invalid_endpoint(format!("failed to build HTTP request: {error}")))
    }
}

/// OpenAI Chat/Completions top-level body before provider extras are merged.
///
/// `max_tokens` (not Responses' `max_output_tokens`) is non-optional in the
/// normalized request and maps directly. Sampling extensions such as `top_p`,
/// `stop`, or `seed` travel through `provider_extras`.
#[derive(Serialize)]
struct OpenAiChatRequestBody<'a> {
    model: &'a str,
    messages: Vec<Value>,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

/// Streaming usage hint injected so SSE chunks carry terminal usage.
#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// Serializes one normalized request and performs the final extras merge.
fn serialize_body(request: &ChatRequest) -> Result<Value, ClientError> {
    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for (index, message) in request.messages.iter().enumerate() {
        for wire in message_to_wire(index, message)? {
            messages.push(wire);
        }
    }
    let tools = request.tools.iter().map(tool_to_wire).collect();
    let wire = OpenAiChatRequestBody {
        model: &request.model,
        messages,
        max_tokens: request.max_tokens,
        stream: request.stream,
        temperature: request.temperature,
        tools,
        stream_options: request.stream.then_some(StreamOptions {
            include_usage: true,
        }),
    };
    let mut body = serde_json::to_value(wire)
        .map_err(|error| invalid_request(format!("failed to serialize request body: {error}")))?;

    if let Some(extras) = &request.provider_extras {
        let outcome = extras
            .merge_into(&mut body, ProviderId::OpenAiChat)
            .map_err(|error| invalid_request(error.to_string()))?;
        if let ProviderExtrasMergeOutcome::IgnoredProviderMismatch {
            extras_provider,
            target,
        } = outcome
        {
            return Err(invalid_request(format!(
                "provider extras for {extras_provider:?} cannot be sent through {target:?}"
            )));
        }
    }

    Ok(body)
}

/// Classifies invalid normalized-to-chat/completions conversion as a protocol error.
pub(super) fn invalid_request(message: String) -> ClientError {
    ClientError::Protocol(format!(
        "invalid OpenAI Chat/Completions request: {message}"
    ))
}

/// Classifies malformed endpoint configuration before any network operation.
fn invalid_endpoint(message: String) -> ClientError {
    ClientError::Other(format!(
        "invalid OpenAI Chat/Completions endpoint configuration: {message}"
    ))
}

#[cfg(test)]
mod tests;
