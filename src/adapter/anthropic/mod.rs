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
    /// Use [`AnthropicAdapter::with_http_client`] to supply stricter
    /// client-level timeouts, proxies, or connection-pool settings.
    pub fn new(endpoint: EndpointConfig) -> Self {
        Self::with_http_client(endpoint, super::common::default_http_client())
    }

    /// Creates an adapter with a caller-configured reusable HTTP client.
    ///
    /// Supplying the client lets applications configure timeouts, proxies, or
    /// connection-pool behavior without adding those runtime concerns to
    /// [`EndpointConfig`]. The per-request phase limits documented on
    /// [`AnthropicAdapter::new`] still apply on top of the supplied client.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::AuthScheme;

    #[test]
    fn adapter_debug_redacts_endpoint_credentials() {
        let adapter = AnthropicAdapter::new(EndpointConfig {
            base_url: "https://anthropic.example.test".to_owned(),
            auth: AuthScheme::Bearer("sk-ant-secret".to_owned()),
            query_params: Vec::new(),
            extra_headers: vec![("x-api-key".to_owned(), "sk-ant-secret".to_owned())],
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
}
