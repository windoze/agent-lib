//! OpenAI Chat/Completions non-streaming response transport and parsing.
//!
//! This module owns the `chat()` entry point and (filled in by M2-1) the
//! complete-response wire parser. M1-2 nails down the stream-flag mutual
//! exclusion guard shared with the streaming path; the transport, request
//! building, and parsing bodies arrive in M1-3/M2-1.

use super::OpenAiChatAdapter;
use crate::client::{ChatRequest, ClientError, Response};

impl OpenAiChatAdapter {
    /// Executes one native non-streaming OpenAI Chat/Completions request.
    ///
    /// Callers must set [`ChatRequest::stream`] to `false`; SSE responses are
    /// handled by [`OpenAiChatAdapter::chat_stream`]. The request build (M1-3),
    /// HTTP transport, and response parsing (M2-1) are filled in by the
    /// subsequent tasks; the stream-flag guard above is final and matches the
    /// `openai_resp` contract.
    pub async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        if request.stream {
            return Err(invalid_response(
                "non-streaming chat requires ChatRequest.stream to be false".to_owned(),
            ));
        }

        // M1-3 builds the request body; M2-1 wires execute + parse_response.
        Err(ClientError::Other(
            "openai_chat adapter chat() body is implemented in M1-3/M2-1".to_owned(),
        ))
    }
}

/// Adds Chat/Completions response context to protocol conversion failures.
///
/// Visible to the parent module so M2-1's `convert.rs` can reuse the same
/// classification for wire-to-normalized block conversion.
pub(super) fn invalid_response(message: String) -> ClientError {
    ClientError::Protocol(format!(
        "invalid OpenAI Chat/Completions response: {message}"
    ))
}
