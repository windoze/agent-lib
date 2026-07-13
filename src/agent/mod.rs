//! Agent-layer data models, runtime state, run context, and loop contracts.
//!
//! This module contains the static, serde-friendly Agent data and the first
//! runtime boundary, [`RunContext`]. [`AgentState`] adds the single active
//! Conversation and data-only loop cursor boundary. [`AgentLoop`] defines the
//! guarded feed-to-event-stream contract; [`DefaultAgentLoop`] provides the
//! current LLM/tool runtime path, pivot queue soft-turning, and turn-boundary
//! reconfiguration queue, and approval responder boundary. Orchestration remains
//! a future layer. Live handles stay out of serde data shapes.

pub mod approval;
pub mod context;
pub mod drive;
pub mod event;
pub mod id;
pub mod interaction;
pub mod loop_driver;
pub mod machine;
mod request;
pub mod requirement;
pub mod spec;
pub mod state;
pub mod tool;

pub use approval::{
    ApprovalDecision, ApprovalError, ApprovalRequirement, ApprovalResponse, NoApprovalPolicy,
    ToolApprovalPolicy,
};
pub use context::{
    BudgetCharge, BudgetDimension, BudgetError, BudgetHandle, BudgetLimits, BudgetSnapshot,
    BudgetUsage, CancellationToken, RunContext, RunContextError, TraceError, TraceHandle,
    TraceNodeId, TraceNodeKind, TraceRecord,
};
pub use drive::{HandlerScope, InteractionHandler, LlmHandler, SubagentHandler, ToolHandler};
pub use event::{
    AgentError, AgentErrorKind, AgentEvent, AgentFailure, AgentInput, AgentOutcome,
    AgentOutcomeKind, AgentUserInput, ApprovalRequest, BudgetExhaustedOutcome,
    ExternalRecoveryKind, ExternalRecoveryOutcome, Notification, PivotMessage,
    QueuedPivotTurnInput, ResumeInput, StepBoundary, ToolCallFinished, ToolCallStarted,
};
pub use id::{AgentId, BlackboardId, PlanId, RunId, SkillId, StepId, ToolSetId};
pub use interaction::{
    Interaction, InteractionError, InteractionKind, InteractionKindTag, InteractionResponse,
};
pub use loop_driver::{
    AgentEventStream, AgentFeedGuard, AgentFeedPermit, AgentLoop, BoxAgentEventStream,
    BoxAgentLoop, DefaultAgentLoop, LlmStepMode,
};
pub use machine::{AgentMachine, DefaultAgentMachine, StepInput, StepOutcome};
pub use requirement::{
    AgentPath, AgentSlot, AgentSpecRef, NoRequirementIds, Requirement, RequirementError,
    RequirementId, RequirementIds, RequirementKind, RequirementKindTag, RequirementResolution,
    RequirementResult, SubagentOutput,
};
pub use spec::{AgentSpec, LoopPolicy, ModelRef, ToolFailurePolicy, ToolSetRef, WorktreeRef};
pub use state::{
    AgentRuntimeHandles, AgentState, AgentStateError, ApprovalCursor, CancelRecoveryCursor,
    CancelRecoveryReason, CursorRequirement, DoneCursor, ErrorCursor, LoopCursor, LoopCursorKind,
    LoopDoneReason, PivotSource, QueuedPivot, QueuedReconfig, ReconfigQueue, ReconfigRequest,
    StepCursor, ToolSetPatch, ToolWaitCursor, ToolWaitRequirements,
};
pub use tool::{
    DeclaredOnlyToolRegistry, DeclaredOnlyToolRegistryResolver, NoToolExecutionIds,
    StaticToolRegistryResolver, ToolExecutionIds, ToolExecutor, ToolRegistry, ToolRegistryResolver,
    ToolRuntimeError,
};
