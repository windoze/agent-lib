//! Agent-layer data models.
//!
//! This module contains only the static, serde-friendly Agent data introduced
//! before the runtime loop exists. It deliberately keeps live conversations,
//! LLM clients, tool registries, task handles, and other runtime resources out
//! of [`AgentSpec`].

pub mod id;
pub mod spec;

pub use id::{AgentId, BlackboardId, PlanId, RunId, SkillId, StepId, ToolSetId};
pub use spec::{AgentSpec, LoopPolicy, ModelRef, ToolFailurePolicy, ToolSetRef, WorktreeRef};
