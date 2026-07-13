//! Agent-layer data models and run-level context handles.
//!
//! This module contains the static, serde-friendly Agent data and the first
//! runtime boundary, [`RunContext`]. Agent state, loop drivers, tool
//! registries, and orchestration remain future layers; live handles stay out of
//! serde data shapes.

pub mod context;
pub mod id;
pub mod spec;

pub use context::{
    BudgetCharge, BudgetDimension, BudgetError, BudgetHandle, BudgetLimits, BudgetSnapshot,
    BudgetUsage, CancellationToken, RunContext, RunContextError, TraceError, TraceHandle,
    TraceNodeId, TraceNodeKind, TraceRecord,
};
pub use id::{AgentId, BlackboardId, PlanId, RunId, SkillId, StepId, ToolSetId};
pub use spec::{AgentSpec, LoopPolicy, ModelRef, ToolFailurePolicy, ToolSetRef, WorktreeRef};
