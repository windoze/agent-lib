//! OpenAI Chat/Completions SSE transport and normalized event translation.
//!
//! This module owns the `chat_stream()` entry point and (filled in by M3) the
//! SSE decoder + chunk normalizer. M1-2 nails down the stream-flag mutual
//! exclusion guard shared with the non-streaming path; the request build,
//! transport, decoder, and normalizer arrive in M1-3/M3-1/M3-2.

use super::OpenAiChatAdapter;
use crate::client::{ChatRequest, ClientError};
use crate::stream::StreamEvent;
use futures::stream::BoxStream;

impl OpenAiChatAdapter {
    /// Starts one native OpenAI Chat/Completions SSE request.
    ///
    /// Callers must set [`ChatRequest::stream`] to `true` so a complete JSON
    /// response cannot accidentally enter the SSE decoder. M1-3 builds the
    /// `stream=true` request body (with `include_usage`); M3-1/M3-2 wire the
    /// decoder and normalizer. The stream-flag guard above is final and matches
    /// the `openai_resp` contract.
    pub async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        if !request.stream {
            return Err(invalid_stream(
                "streaming chat requires ChatRequest.stream to be true".to_owned(),
            ));
        }

        // M1-3 builds the request body; M3-1/M3-2 wire the SSE decoder + normalizer.
        Err(ClientError::Other(
            "openai_chat adapter chat_stream() body is implemented in M1-3/M3".to_owned(),
        ))
    }
}

/// Adds Chat/Completions stream context to protocol conversion failures.
fn invalid_stream(message: String) -> ClientError {
    ClientError::Protocol(format!("invalid OpenAI Chat/Completions stream: {message}"))
}
