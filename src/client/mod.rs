//! Client abstractions, endpoint configuration, capabilities, and errors.

pub mod capability;
pub mod error;
pub mod response;

pub use capability::{
    ANTHROPIC_DEFAULT_CAPABILITY, Capability, Modality, OPENAI_RESP_DEFAULT_CAPABILITY,
};
pub use error::ClientError;
pub use response::Response;
