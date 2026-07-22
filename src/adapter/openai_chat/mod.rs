//! Placeholder for the OpenAI Chat/Completions adapter (classic
//! `POST /v1/chat/completions`, including SSE streaming).
//!
//! M1-1 registers the [`crate::model::extras::ProviderId`] variant, the
//! [`crate::client::OPENAI_CHAT_DEFAULT_CAPABILITY`] entry, and this module so the
//! crate compiles with the new provider enumerated. M1-2 builds the real adapter
//! skeleton: both constructors, the stream-flag mutual-exclusion validation, and
//! the request/response/stream wiring (one adapter covering OpenAI-compatible
//! baselines, DeepSeek, and vLLM; dialect strategy in `docs/openai-chat-api.md`
//! §5 — replay `reasoning_content` plus an extras escape hatch, no quirk types).

use crate::{
    client::{Capability, ChatRequest, ClientError, EndpointConfig, LlmClient, Response},
    stream::StreamEvent,
};
use async_trait::async_trait;
use futures::stream::BoxStream;

/// Placeholder adapter shell for the OpenAI Chat/Completions protocol.
///
/// Only enough is implemented here for the adapter to register as a
/// [`LlmClient`] so the facade can match [`crate::model::extras::ProviderId`]
/// exhaustively. The constructors, stream-flag validation, and HTTP/SSE wiring
/// are filled in by M1-2. `capability()` already returns the final protocol-level
/// table entry.
#[derive(Clone, Debug)]
#[allow(dead_code)] // http_client and endpoint are wired in M1-2.
pub struct OpenAiChatAdapter {
    http_client: reqwest::Client,
    endpoint: EndpointConfig,
}

impl OpenAiChatAdapter {
    /// Creates a placeholder adapter carrying the endpoint with the default HTTP
    /// client. M1-2 keeps this constructor and adds `with_http_client`.
    pub fn new(endpoint: EndpointConfig) -> Self {
        Self {
            http_client: super::common::default_http_client(),
            endpoint,
        }
    }
}

#[async_trait]
impl LlmClient for OpenAiChatAdapter {
    /// Returns the protocol-level OpenAI Chat/Completions capability table entry.
    fn capability(&self) -> &Capability {
        &crate::client::OPENAI_CHAT_DEFAULT_CAPABILITY
    }

    /// Not yet wired; M1-2 implements the non-streaming path.
    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
        Err(ClientError::Other(
            "openai_chat adapter chat() is implemented in M1-2".to_owned(),
        ))
    }

    /// Not yet wired; M1-2 implements the SSE streaming path.
    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        Err(ClientError::Other(
            "openai_chat adapter chat_stream() is implemented in M1-2".to_owned(),
        ))
    }
}
