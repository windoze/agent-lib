//! Agent-layer data models, runtime state, run context, and effect contracts.
//!
//! This module contains the static, serde-friendly Agent data and the first
//! runtime boundary, [`RunContext`]. [`AgentState`] adds the single active
//! Conversation and data-only loop cursor boundary. [`AgentMachine`] defines the
//! sans-io `step` contract; [`DefaultAgentMachine`] provides the current LLM/tool
//! state machine that reifies each effect as a [`Requirement`] and folds resumed
//! results back into the single active Conversation, including pivot injection
//! and turn-boundary reconfiguration. The [`drive`] reference driver fulfils
//! those requirements. Orchestration remains a future layer. Live handles stay
//! out of serde data shapes.

pub mod approval;
pub mod context;
pub mod drive;
pub mod event;
pub mod external;
pub mod id;
pub mod interaction;
pub mod machine;
pub mod permission;
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
    BudgetUsage, CancellationToken, RequirementDisposition, RunContext, RunContextError,
    TraceError, TraceHandle, TraceNodeId, TraceNodeKind, TraceRecord,
};
pub use drive::{
    ApprovalInteractionHandler, DrivingSubagentHandler, ExternalSessionHandler, HandlerScope,
    InteractionHandler, LlmClientHandler, LlmHandler, Pop, ReconfigHandler,
    ReconfigRegistryHandler, ReferenceScope, ScopePop, SpawnedChild, SubagentHandler,
    SubagentSpawner, ToolHandler, ToolRegistryHandler, TurnDone, drain, drive_turn,
};
pub use event::{
    AgentError, AgentErrorKind, AgentInput, AgentUserInput, Notification, PivotMessage,
    StepBoundary, ToolCallFinished, ToolCallStarted,
};
pub use external::{
    ExternalAgentCursor, ExternalAgentError, ExternalAgentEvent, ExternalAgentMachine,
    ExternalAgentOutput, ExternalAgentSpec, ExternalAgentState, ExternalArtifactKind,
    ExternalArtifactRef, ExternalPermissionMode, ExternalRuntimeHandles, ExternalRuntimeKind,
    ExternalSessionInput, ExternalSessionPolicy, ExternalSessionRef, ExternalSessionRequest,
    ExternalSessionResult, ExternalSessionShutdown, ExternalStreamPolicy, WorkerProfileRef,
    WorktreeIsolation,
};
pub use id::{AgentId, BlackboardId, PlanId, RunId, SkillId, StepId, ToolSetId};
pub use interaction::{
    Interaction, InteractionError, InteractionKind, InteractionKindTag, InteractionResponse,
};
pub use machine::{AgentMachine, DefaultAgentMachine, StepInput, StepOutcome};
pub use machine::{MachineTreeState, NestedMachine, NestedMachineError};
pub use permission::{PermissionCategory, PermissionRequest, PermissionRisk};
pub use requirement::{
    AgentPath, AgentSlot, AgentSpecRef, LlmStepMode, NoRequirementIds, Requirement,
    RequirementError, RequirementId, RequirementIds, RequirementKind, RequirementKindTag,
    RequirementResolution, RequirementResult, SubagentOutput,
};
pub use spec::{AgentSpec, LoopPolicy, ModelRef, ToolFailurePolicy, ToolSetRef, WorktreeRef};
pub use state::{
    AgentRuntimeHandles, AgentState, AgentStateError, ApprovalCursor, CancelRecoveryCursor,
    CancelRecoveryReason, CursorRequirement, DoneCursor, ErrorCursor, LoopCursor, LoopCursorKind,
    LoopDoneReason, PivotSource, QueuedPivot, QueuedReconfig, ReconfigCursor, ReconfigQueue,
    ReconfigRequest, StepCursor, ToolSetPatch, ToolWaitCursor, ToolWaitRequirements,
};
pub use tool::{
    DeclaredOnlyToolRegistry, DeclaredOnlyToolRegistryResolver, NoToolExecutionIds,
    StaticToolRegistryResolver, ToolExecutionIds, ToolExecutor, ToolRegistry, ToolRegistryResolver,
    ToolRuntimeError,
};
