//! Convenient re-exports of the most common facade types.
//!
//! `use agent_lib::prelude::*;` brings the everyday facade entry points into
//! scope. It intentionally re-exports only simple, path-friendly types and
//! never the lower-layer machinery (`AgentMachine`, `Requirement`, `Boundary`,
//! adapter internals); code needing those should import them explicitly from
//! their owning module (see `docs/facade-api.md` §3).
//!
//! Milestone 1 exposes the configuration wrappers, the shared result/event
//! types, and the one-shot `Chat` plus stateful `ChatSession` (including its
//! incremental `RunStream`) entry points. Milestone 2 adds the base Agent
//! facade — the tool-using [`Agent`], the typed [`Tool`] surface with its
//! [`ToolContext`], and the [`Approval`] / [`ApprovalPolicy`] tiers. Milestone 3
//! adds the [`Delegation`] routing type, and Milestone 4 adds the
//! [`ManagedExternalAgent`] entry point for managed external-agent delegates.
//! Subsequent milestones extend this prelude with the remaining delegation
//! types as they land.

pub use crate::facade::{
    Agent, Approval, ApprovalPolicy, Chat, ChatSession, Delegation, ManagedExternalAgent,
    ModelConfig, ProviderConfig, Reply, RunEvent, RunOutput, RunStream, Tool, ToolContext,
};
