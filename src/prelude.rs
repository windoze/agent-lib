//! Convenient re-exports of the most common facade types.
//!
//! `use agent_lib::prelude::*;` brings the everyday facade entry points into
//! scope. It intentionally re-exports only simple, path-friendly types and
//! never the lower-layer machinery (`AgentMachine`, `Requirement`, `Boundary`,
//! adapter internals); code needing those should import them explicitly from
//! their owning module (see `docs/facade-api.md` §3).
//!
//! Milestone 1 exposes the configuration wrappers. Subsequent milestones extend
//! this prelude with `Chat`, `ChatSession`, `Reply`, `RunOutput`, `RunEvent`,
//! and the Agent-facade types as they land.

pub use crate::facade::{ModelConfig, ProviderConfig};
