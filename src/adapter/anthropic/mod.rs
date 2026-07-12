//! Anthropic Messages API transport and wire-format adapter.

use crate::client::EndpointConfig;

mod request;
mod response;

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
