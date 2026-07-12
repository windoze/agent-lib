//! Client abstractions, endpoint configuration, capabilities, and errors.

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
