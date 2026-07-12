//! Client abstractions, endpoint configuration, capabilities, and errors.

use crate::stream::StreamEvent;
use async_trait::async_trait;
use futures::stream::BoxStream;

pub mod capability;
pub mod config;
pub mod error;
pub mod request;
pub mod response;

pub use capability::{
    ANTHROPIC_DEFAULT_CAPABILITY, Capability, Modality, OPENAI_RESP_DEFAULT_CAPABILITY,
};
pub use config::{AuthScheme, EndpointConfig};
pub use error::ClientError;
pub use request::ChatRequest;
pub use response::Response;

/// Provider-neutral asynchronous interface for one LLM endpoint.
///
/// Implementations own provider wire translation and transport resources while
/// callers can select an implementation at runtime through `dyn LlmClient`.
/// Complete and incremental response paths remain separate so an adapter can
/// use its provider's native non-streaming endpoint without first constructing
/// a normalized event stream.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Returns the structured capabilities advertised by this client.
    fn capability(&self) -> &Capability;

    /// Executes one request and returns its complete normalized response.
    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError>;

    /// Starts one request and returns its normalized incremental event stream.
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError>;
}

#[cfg(test)]
mod tests;
