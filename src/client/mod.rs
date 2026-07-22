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
    ANTHROPIC_DEFAULT_CAPABILITY, Capability, Modality, OPENAI_CHAT_DEFAULT_CAPABILITY,
    OPENAI_RESP_DEFAULT_CAPABILITY,
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
/// a normalized event stream. A client is safe to share across tasks; adapter
/// clones reuse the underlying HTTP connection pool.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Returns the protocol-level structured capabilities advertised by this client.
    ///
    /// Model- or deployment-specific limits may be narrower than this default
    /// table and should be applied by the caller when known.
    fn capability(&self) -> &Capability;

    /// Executes one request and returns its complete normalized response.
    ///
    /// `request.stream` must be `false` so the provider returns its native JSON
    /// complete-response representation.
    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError>;

    /// Starts one request and returns its normalized incremental event stream.
    ///
    /// `request.stream` must be `true`. Errors encountered before response
    /// headers are returned directly; transport or protocol failures observed
    /// later are yielded by the stream. Callers can fold the events with
    /// [`crate::stream::accumulator::Accumulator`] or
    /// [`crate::stream::accumulator::collect`].
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError>;
}

#[cfg(test)]
mod tests;
