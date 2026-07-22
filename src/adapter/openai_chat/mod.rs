//! OpenAI Chat/Completions transport and wire-format adapter.
//!
//! Covers the classic `POST /v1/chat/completions` endpoint, including SSE
//! streaming. One adapter serves OpenAI-compatible baselines plus the DeepSeek
//! and vLLM dialects; the only per-target differences are the
//! [`EndpointConfig`] (base URL / auth) and provider-extras escape hatch. The
//! unified dialect strategy — replay `reasoning_content` unconditionally plus
//! an extras escape hatch, no quirk configuration types — is specified in
//! `docs/openai-chat-api.md` §5.

use crate::{
    client::{
        Capability, ChatRequest, ClientError, EndpointConfig, LlmClient,
        OPENAI_CHAT_DEFAULT_CAPABILITY, Response,
    },
    stream::StreamEvent,
};
use async_trait::async_trait;
use futures::stream::BoxStream;

mod request;
mod response;
mod stream;

/// Client resources and endpoint configuration for OpenAI Chat/Completions.
///
/// The adapter owns a reusable HTTP client while keeping endpoint transport
/// details separate from serializable provider-neutral requests. This mirrors
/// [`crate::adapter::openai_resp::OpenAiRespAdapter`].
#[derive(Clone, Debug)]
pub struct OpenAiChatAdapter {
    // Read by `build_request`/transport once M1-3 wires the request body.
    #[allow(dead_code)]
    http_client: reqwest::Client,
    endpoint: EndpointConfig,
}

impl OpenAiChatAdapter {
    /// Creates an adapter with a default reusable HTTP client.
    ///
    /// Default transport limits (regardless of the constructor used):
    ///
    /// - connect timeout: 10s (built into the default client),
    /// - `chat()`: 10min total per request,
    /// - `chat_stream()`: 10min for connect + response headers; the SSE body
    ///   itself has no total timeout so long streams are never killed,
    /// - non-2xx error bodies: 30s read timeout, truncated at 1 MiB.
    ///
    /// Use [`OpenAiChatAdapter::with_http_client`] to supply stricter
    /// client-level timeouts, proxies, or connection-pool settings.
    pub fn new(endpoint: EndpointConfig) -> Self {
        Self::with_http_client(endpoint, super::common::default_http_client())
    }

    /// Creates an adapter with a caller-configured reusable HTTP client.
    ///
    /// Applications can use the supplied client to configure timeouts,
    /// proxies, and connection pooling without putting runtime resources in
    /// [`EndpointConfig`]. The per-request phase limits documented on
    /// [`OpenAiChatAdapter::new`] still apply on top of the supplied client.
    pub fn with_http_client(endpoint: EndpointConfig, http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            endpoint,
        }
    }

    /// Returns the endpoint transport configuration used by this adapter.
    pub fn endpoint(&self) -> &EndpointConfig {
        &self.endpoint
    }
}

#[async_trait]
impl LlmClient for OpenAiChatAdapter {
    /// Returns the protocol-level OpenAI Chat/Completions capability table entry.
    fn capability(&self) -> &Capability {
        &OPENAI_CHAT_DEFAULT_CAPABILITY
    }

    /// Executes the adapter's native complete-response path.
    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        OpenAiChatAdapter::chat(self, request).await
    }

    /// Executes the adapter's native SSE path.
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        OpenAiChatAdapter::chat_stream(self, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::AuthScheme;
    use crate::model::content::ContentBlock;
    use crate::model::message::{Message, Role};
    use serde_json::Map;

    /// Builds a minimal provider-neutral request for transport-guard tests.
    fn minimal_request() -> ChatRequest {
        ChatRequest {
            model: "gpt-5.5".to_owned(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "hi".to_owned(),
                    extra: Map::new(),
                }],
            }],
            tools: Vec::new(),
            system: None,
            max_tokens: 32,
            temperature: None,
            stream: false,
            provider_extras: None,
        }
    }

    /// Endpoint credentials never surface through the adapter `Debug` output.
    #[test]
    fn adapter_debug_redacts_endpoint_credentials() {
        let adapter = OpenAiChatAdapter::new(EndpointConfig {
            base_url: "https://openai.example.test".to_owned(),
            auth: AuthScheme::Bearer("sk-ant-secret".to_owned()),
            query_params: Vec::new(),
            extra_headers: vec![("api-key".to_owned(), "sk-ant-secret".to_owned())],
        });

        let rendered = format!("{adapter:?}");
        assert!(
            !rendered.contains("sk-ant-secret"),
            "secret leaked through adapter Debug: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "missing redaction placeholder: {rendered}"
        );
    }

    /// `chat()` rejects a streaming request before touching the transport layer.
    #[tokio::test]
    async fn chat_rejects_streaming_request_before_transport() {
        let adapter = OpenAiChatAdapter::new(EndpointConfig {
            base_url: "http://127.0.0.1:1".to_owned(),
            auth: AuthScheme::None,
            query_params: Vec::new(),
            extra_headers: Vec::new(),
        });
        let mut request = minimal_request();
        request.stream = true;

        let error = adapter
            .chat(request)
            .await
            .expect_err("streaming request should be rejected");

        assert!(matches!(error, ClientError::Protocol(_)));
        assert!(error.to_string().contains("stream to be false"));
    }

    /// `chat_stream()` rejects a non-streaming request before touching the
    /// transport layer.
    #[tokio::test]
    async fn chat_stream_rejects_non_streaming_request_before_transport() {
        let adapter = OpenAiChatAdapter::new(EndpointConfig {
            base_url: "http://127.0.0.1:1".to_owned(),
            auth: AuthScheme::None,
            query_params: Vec::new(),
            extra_headers: Vec::new(),
        });

        let error = match adapter.chat_stream(minimal_request()).await {
            Err(error) => error,
            Ok(_) => panic!("non-stream request must be rejected"),
        };

        assert!(matches!(error, ClientError::Protocol(_)));
        assert!(error.to_string().contains("stream to be true"));
    }
}
