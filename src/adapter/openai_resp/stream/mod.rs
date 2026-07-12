//! OpenAI Responses SSE transport and normalized event translation.

use super::OpenAiRespAdapter;
use crate::{
    client::{ChatRequest, ClientError},
    stream::StreamEvent,
};
use futures::stream::BoxStream;
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};

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

        if !status.is_success() {
            let body = response.bytes().await.map_err(map_transport_error)?;
            return Err(ClientError::from_http_response(
                status.as_u16(),
                String::from_utf8_lossy(&body),
                retry_after.as_deref(),
            ));
        }

        validate_event_stream_content_type(response.headers().get(CONTENT_TYPE))?;
        Ok(normalize_sse(response.bytes_stream(), map_transport_error))
    }
}

/// Validates that a successful streaming response is actually SSE.
fn validate_event_stream_content_type(
    content_type: Option<&reqwest::header::HeaderValue>,
) -> Result<(), ClientError> {
    let Some(content_type) = content_type else {
        return Err(invalid_stream(
            "successful response omitted the content-type header".to_owned(),
        ));
    };
    let content_type = content_type
        .to_str()
        .map_err(|error| invalid_stream(format!("invalid content-type header: {error}")))?;
    let media_type = content_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default();

    if !media_type.eq_ignore_ascii_case("text/event-stream") {
        return Err(invalid_stream(format!(
            "successful streaming response used content type `{content_type}`"
        )));
    }

    Ok(())
}

/// Maps reqwest failures into retry-relevant client error classes.
fn map_transport_error(error: reqwest::Error) -> ClientError {
    if error.is_timeout() {
        ClientError::Timeout
    } else {
        ClientError::Network(error.to_string())
    }
}

/// Adds Responses stream context to protocol conversion failures.
fn invalid_stream(message: String) -> ClientError {
    ClientError::Protocol(format!("invalid OpenAI Responses stream: {message}"))
}

#[cfg(test)]
mod tests;
