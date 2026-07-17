//! Ergonomic facade over the Conversation and Agent building blocks.
//!
//! The facade is a thin **assembly** layer: it does not introduce a new effect
//! family or a bespoke state machine. It reuses the existing `conversation`,
//! `client`, and `agent` primitives and packages them behind approachable
//! entry points (see `docs/facade-api.md`). Milestone 1 lands the shared
//! foundations used by every later layer:
//!
//! - [`ProviderConfig`] and [`ModelConfig`] — provider/model configuration
//!   wrappers ([`config`]).
//! - [`FacadeError`] — one reduced error type that still preserves the
//!   lower-layer error `source` ([`error`]).
//! - [`FacadeIds`] — a built-in monotonic identity source, since the library
//!   core never mints ids itself ([`ids`]).
//!
//! Later milestones add the `Chat`/`ChatSession`, `Agent`/`AgentSession`,
//! subagent, managed-external-agent, dispatcher, and collaboration facades on
//! top of these foundations.

pub mod config;
pub mod error;
pub mod ids;

pub use config::{ModelConfig, ProviderConfig, ProviderConfigBuilder};
pub use error::FacadeError;
pub use ids::FacadeIds;
