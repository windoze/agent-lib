//! OpenAI Responses SSE transport and normalized event translation.
//!
//! OpenAI-compatible endpoints sometimes omit `sequence_number` on SSE
//! payloads. Missing values are accepted without a continuity check; events
//! that do include a number must still match the zero-based event position.

use super::OpenAiRespAdapter;
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

impl OpenAiRespAdapter {
    /// Starts one native OpenAI Responses SSE request.
    ///
    /// The returned stream owns the HTTP response and translates provider
    /// events lazily. Callers must set [`ChatRequest::stream`] to `true` so a
    /// complete JSON response cannot accidentally enter the SSE decoder.
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

/// Adds Responses stream context to protocol conversion failures.
fn invalid_stream(message: String) -> ClientError {
    ClientError::Protocol(format!("invalid OpenAI Responses stream: {message}"))
}

#[cfg(test)]
mod tests;
