//! OpenAI Responses API transport and wire-format adapter.

use crate::client::EndpointConfig;

mod request;
mod response;

/// Namespace used inside content-block extras for replayable Responses wire
/// metadata that has no provider-neutral field.
const RESPONSE_EXTRA_KEY: &str = "openai_response";

/// Client resources and endpoint configuration for OpenAI Responses.
///
/// The adapter owns a reusable HTTP client while keeping endpoint transport
/// details separate from serializable provider-neutral requests.
#[derive(Clone, Debug)]
pub struct OpenAiRespAdapter {
    http_client: reqwest::Client,
    endpoint: EndpointConfig,
}

impl OpenAiRespAdapter {
    /// Creates an adapter with reqwest's default reusable HTTP client.
    pub fn new(endpoint: EndpointConfig) -> Self {
        Self::with_http_client(endpoint, reqwest::Client::new())
    }

    /// Creates an adapter with a caller-configured reusable HTTP client.
    ///
    /// Applications can use the supplied client to configure timeouts,
    /// proxies, and connection pooling without putting runtime resources in
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
