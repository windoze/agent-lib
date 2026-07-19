//! Anthropic Messages SSE transport and normalized event translation.

use super::AnthropicAdapter;
use crate::{
    adapter::common,
    client::{ChatRequest, ClientError},
    stream::StreamEvent,
};
use futures::stream::BoxStream;

mod decoder;
mod normalizer;
mod usage;
mod wire;

use decoder::normalize_sse;

impl AnthropicAdapter {
    /// Starts one native Anthropic Messages SSE request.
    ///
    /// The returned stream owns the HTTP response and normalizes each provider
    /// event lazily. Callers must set [`ChatRequest::stream`] to `true` so an
    /// accidental non-streaming JSON response cannot enter the SSE decoder.
    ///
    /// Only the connect + response-headers phase is bounded (10 minutes);
    /// the SSE body itself has no total timeout because long-lived streams are
    /// the normal case.
    pub async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        if !request.stream {
            return Err(invalid_stream(
                "streaming chat requires ChatRequest.stream to be true".to_owned(),
            ));
        }

        let request = self.build_request(&request)?;
        let response =
            common::execute_sse_response(&self.http_client, request, invalid_stream).await?;
        Ok(normalize_sse(
            response.bytes_stream(),
            common::map_transport_error,
        ))
    }
}

/// Adds Anthropic stream context to protocol conversion failures.
fn invalid_stream(message: String) -> ClientError {
    ClientError::Protocol(format!("invalid Anthropic Messages stream: {message}"))
}

#[cfg(test)]
mod tests;
