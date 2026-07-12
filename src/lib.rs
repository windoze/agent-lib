//! Client-layer building blocks for normalized LLM API access.
//!
//! This crate implements the Client layer from the project architecture. It
//! owns provider-neutral request/response models, streaming events, adapters,
//! and client abstractions for LLM wire protocols.
//!
//! Higher-level Conversation and Agent layers are intentionally out of scope
//! for this crate. They will consume the normalized types exported here instead
//! of depending on provider-specific API shapes.

pub mod adapter;
pub mod client;
pub mod model;
pub mod stream;
