//! Anthropic Messages API transport and wire-format adapter.

use crate::{
    client::{
        ANTHROPIC_DEFAULT_CAPABILITY, Capability, ChatRequest, ClientError, EndpointConfig,
        LlmClient, Response,
    },
    stream::StreamEvent,
};
use async_trait::async_trait;
use futures::stream::BoxStream;

mod request;
mod response;
mod stream;

/// Client resources and endpoint configuration for Anthropic Messages.
///
/// The adapter keeps transport state separate from serializable request data.
/// Clones share reqwest's internal connection pool and retain the same endpoint
/// configuration.
#[derive(Clone, Debug)]
pub struct AnthropicAdapter {
    http_client: reqwest::Client,
    endpoint: EndpointConfig,
}

impl AnthropicAdapter {
    /// Creates an adapter with reqwest's default reusable HTTP client.
    pub fn new(endpoint: EndpointConfig) -> Self {
        Self::with_http_client(endpoint, reqwest::Client::new())
    }

    /// Creates an adapter with a caller-configured reusable HTTP client.
    ///
    /// Supplying the client lets applications configure timeouts, proxies, or
    /// connection-pool behavior without adding those runtime concerns to
    /// [`EndpointConfig`].
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
impl LlmClient for AnthropicAdapter {
    /// Returns the protocol-level Anthropic Messages capability table entry.
    fn capability(&self) -> &Capability {
        &ANTHROPIC_DEFAULT_CAPABILITY
    }

    /// Executes the adapter's native complete-response path.
    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        AnthropicAdapter::chat(self, request).await
    }

    /// Executes the adapter's native SSE path.
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        AnthropicAdapter::chat_stream(self, request).await
    }
}
