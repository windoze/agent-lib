//! Agent-layer data models, runtime state, run context, and loop contracts.
//!
//! This module contains the static, serde-friendly Agent data and the first
//! runtime boundary, [`RunContext`]. [`AgentState`] adds the single active
//! Conversation and data-only loop cursor boundary. [`AgentLoop`] defines the
//! guarded feed-to-event-stream contract used by future runtime drivers;
//! concrete LLM drivers, tool registries, approval responders, and orchestration
//! remain future layers. Live handles stay out of serde data shapes.

pub mod context;
pub mod event;
pub mod id;
pub mod loop_driver;
pub mod spec;
pub mod state;

pub use context::{
    BudgetCharge, BudgetDimension, BudgetError, BudgetHandle, BudgetLimits, BudgetSnapshot,
    BudgetUsage, CancellationToken, RunContext, RunContextError, TraceError, TraceHandle,
    TraceNodeId, TraceNodeKind, TraceRecord,
};
pub use event::{
    AgentError, AgentErrorKind, AgentEvent, AgentFailure, AgentInput, AgentOutcome,
    AgentOutcomeKind, AgentUserInput, ApprovalRequest, BudgetExhaustedOutcome,
    ExternalRecoveryKind, ExternalRecoveryOutcome, PivotMessage, ResumeInput, StepBoundary,
    ToolCallFinished, ToolCallStarted,
};
pub use id::{AgentId, BlackboardId, PlanId, RunId, SkillId, StepId, ToolSetId};
pub use loop_driver::{
    AgentEventStream, AgentFeedGuard, AgentFeedPermit, AgentLoop, BoxAgentEventStream,
    BoxAgentLoop, DefaultAgentLoop, LlmStepMode,
};
pub use spec::{AgentSpec, LoopPolicy, ModelRef, ToolFailurePolicy, ToolSetRef, WorktreeRef};
pub use state::{
    AgentRuntimeHandles, AgentState, AgentStateError, ApprovalCursor, CancelRecoveryCursor,
    CancelRecoveryReason, DoneCursor, ErrorCursor, LoopCursor, LoopCursorKind, LoopDoneReason,
    PivotSource, QueuedPivot, QueuedReconfig, StepCursor, ToolWaitCursor,
};
