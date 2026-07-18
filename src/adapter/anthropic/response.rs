//! Anthropic Messages response parsing and non-streaming transport.

use super::AnthropicAdapter;
use crate::{
    adapter::http,
    client::{ChatRequest, ClientError, Response},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        usage::Usage,
    },
};
use reqwest::header::RETRY_AFTER;
use serde::Deserialize;
use serde_json::{Map, Value};

impl AnthropicAdapter {
    /// Parses one complete Anthropic Messages JSON response.
    ///
    /// The conversion retains unmodeled top-level, content-block, and usage
    /// fields in their corresponding `extra` maps. Malformed or unsupported
    /// wire data is reported as a protocol error instead of being discarded.
    pub fn parse_response(body: &[u8]) -> Result<Response, ClientError> {
        let wire: AnthropicResponseBody = serde_json::from_slice(body).map_err(|error| {
            invalid_response(format!(
                "failed to deserialize response JSON at line {}, column {}: {error}",
                error.line(),
                error.column()
            ))
        })?;

        Ok(Response {
            message: Message {
                role: Role::Assistant,
                content: wire.content.into_iter().map(ContentBlock::from).collect(),
            },
            usage: wire.usage,
            stop_reason: StopReason::normalize(wire.stop_reason),
            extra: wire.extra,
        })
    }

    /// Executes one native non-streaming Anthropic Messages request.
    ///
    /// Callers must set [`ChatRequest::stream`] to `false`; streaming requests
    /// are handled by the separate streaming path so a successful SSE body can
    /// never be mistaken for complete response JSON.
    ///
    /// The whole request is bounded by a 10-minute total timeout; the connect
    /// phase and non-2xx error bodies have their own tighter limits (see
    /// [`AnthropicAdapter::new`]).
    pub async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        match tokio::time::timeout(http::DEFAULT_REQUEST_TIMEOUT, self.chat_inner(request)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(ClientError::Timeout),
        }
    }

    /// Executes the unbounded body of [`AnthropicAdapter::chat`].
    async fn chat_inner(&self, request: ChatRequest) -> Result<Response, ClientError> {
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
            .map_err(http::map_transport_error)?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        if !status.is_success() {
            let body = http::read_error_body(response).await?;
            return Err(ClientError::from_http_response(
                status.as_u16(),
                body,
                retry_after.as_deref(),
            ));
        }

        let body = response.bytes().await.map_err(http::map_transport_error)?;
        Self::parse_response(&body)
    }
}

/// Fields needed to normalize a complete Anthropic response.
#[derive(Deserialize)]
struct AnthropicResponseBody {
    /// Anthropic complete responses must describe an assistant message.
    #[serde(rename = "role")]
    _role: AnthropicResponseRole,
    /// Complete content blocks returned by the model.
    content: Vec<AnthropicContentBlock>,
    /// Raw provider stop reason retained during normalization.
    stop_reason: String,
    /// Provider token accounting, decoded by the shared alias-aware model.
    usage: Usage,
    /// Provider response metadata such as id, model, type, and stop sequence.
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

/// The only role valid on an Anthropic Messages response.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum AnthropicResponseRole {
    /// Anthropic returns generated content as an assistant message.
    Assistant,
}

/// Anthropic assistant-output blocks supported by the normalized model.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    /// Provider text output plus unmodeled block metadata.
    Text {
        /// Generated text.
        text: String,
        /// Fields such as provider-specific citations or annotations.
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
    /// A complete provider tool invocation.
    ToolUse {
        /// Provider-assigned tool-call identifier.
        id: String,
        /// Selected tool name.
        name: String,
        /// Fully parsed tool input.
        input: Value,
        /// Provider-specific tool-use fields.
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
    /// Extended-thinking output and its replay signature.
    Thinking {
        /// Anthropic names the reasoning payload `thinking` on the wire.
        thinking: String,
        /// Signature required when replaying thinking in later requests.
        #[serde(default)]
        signature: Option<String>,
        /// Provider-specific thinking metadata.
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
}

impl From<AnthropicContentBlock> for ContentBlock {
    /// Converts Anthropic field names into complete provider-neutral blocks.
    fn from(block: AnthropicContentBlock) -> Self {
        match block {
            AnthropicContentBlock::Text { text, extra } => Self::Text { text, extra },
            AnthropicContentBlock::ToolUse {
                id,
                name,
                input,
                extra,
            } => Self::ToolUse {
                id,
                name,
                input,
                extra,
            },
            AnthropicContentBlock::Thinking {
                thinking,
                signature,
                extra,
            } => Self::Thinking {
                text: thinking,
                signature,
                extra,
            },
        }
    }
}

/// Adds Anthropic response context to protocol conversion failures.
fn invalid_response(message: String) -> ClientError {
    ClientError::Protocol(format!("invalid Anthropic Messages response: {message}"))
}

#[cfg(test)]
mod tests;
