//! OpenAI Chat/Completions SSE transport and normalized event translation.

use super::OpenAiChatAdapter;
use crate::{
    adapter::common,
    client::{ChatRequest, ClientError},
    stream::StreamEvent,
};
use futures::stream::BoxStream;

mod decoder;
mod normalizer;
mod wire;

use decoder::normalize_sse;

impl OpenAiChatAdapter {
    /// Starts one native OpenAI Chat/Completions SSE request.
    ///
    /// The returned stream owns the HTTP response and translates provider chunks
    /// lazily. Callers must set [`ChatRequest::stream`] to `true` so a complete
    /// JSON response cannot accidentally enter the SSE decoder, and so the
    /// request body carries `stream_options.include_usage` (injected by
    /// [`OpenAiChatAdapter::build_request`]) that makes the terminal usage
    /// chunk arrive.
    ///
    /// Only the connect + response-headers phase is bounded (10 minutes); the
    /// SSE body itself has no total timeout because long-lived streams are the
    /// normal case. The `data: [DONE]` sentinel ends the stream normally; an
    /// EOF without it surfaces as a protocol error.
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

/// Adds Chat/Completions stream context to protocol conversion failures.
fn invalid_stream(message: String) -> ClientError {
    ClientError::Protocol(format!("invalid OpenAI Chat/Completions stream: {message}"))
}

#[cfg(test)]
mod tests;
