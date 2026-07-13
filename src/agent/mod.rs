//! Agent-layer data models, runtime state, and run-level context handles.
//!
//! This module contains the static, serde-friendly Agent data and the first
//! runtime boundary, [`RunContext`]. [`AgentState`] adds the single active
//! Conversation and data-only loop cursor boundary; loop drivers, concrete tool
//! registries, and orchestration remain future layers. Live handles stay out of
//! serde data shapes.

pub mod context;
pub mod id;
pub mod spec;
pub mod state;

pub use context::{
    BudgetCharge, BudgetDimension, BudgetError, BudgetHandle, BudgetLimits, BudgetSnapshot,
    BudgetUsage, CancellationToken, RunContext, RunContextError, TraceError, TraceHandle,
    TraceNodeId, TraceNodeKind, TraceRecord,
};
pub use id::{AgentId, BlackboardId, PlanId, RunId, SkillId, StepId, ToolSetId};
pub use spec::{AgentSpec, LoopPolicy, ModelRef, ToolFailurePolicy, ToolSetRef, WorktreeRef};
pub use state::{
    AgentRuntimeHandles, AgentState, AgentStateError, ApprovalCursor, CancelRecoveryCursor,
    CancelRecoveryReason, DoneCursor, ErrorCursor, LoopCursor, LoopCursorKind, LoopDoneReason,
    PivotSource, QueuedPivot, QueuedReconfig, StepCursor, ToolWaitCursor,
};
