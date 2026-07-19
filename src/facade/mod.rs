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
//! - [`Reply`], [`RunOutput`], [`UsageSummary`], [`RunEvent`], its serializable
//!   projection [`WireRunEvent`], and [`IntoUserMessage`] — the shared result,
//!   usage, event, and input types returned and accepted by every run entry
//!   point ([`run`]).
//! - [`Chat`] and [`ChatBuilder`] — the stateless one-shot Chat facade, plus
//!   [`ChatSession`] / [`ChatSessionBuilder`] for stateful multi-turn chat with
//!   snapshot/restore and an incremental [`RunStream`] ([`chat`]).
//!
//! Later milestones add the `Agent`/`AgentSession`, subagent,
//! managed-external-agent, dispatcher, and collaboration facades on top of these
//! foundations. Milestone 2 begins with the typed function tool surface
//! ([`Tool`], [`ToolContext`], [`ToolResult`], [`IntoToolResult`]) in [`tool`],
//! then the approval surface ([`Approval`], [`ApprovalPolicy`],
//! [`ApprovalDecision`]) in [`approval`]. Milestone 3 adds the local subagent
//! surface ([`Agent::worker`], [`LocalSubagent`], [`Delegation`]) in
//! [`delegate`]. Milestone 4 begins the managed-external-agent surface
//! ([`ManagedExternalAgent`], [`ExternalRunMode`], [`ExternalAgentCapabilities`])
//! in [`external`].

pub mod agent;
pub mod approval;
pub mod chat;
pub mod collab;
pub mod config;
pub mod delegate;
pub mod error;
pub mod external;
pub mod ids;
pub mod run;
pub mod tool;

pub use agent::{
    Agent, AgentBuilder, AgentParts, AgentRestoreBuilder, AgentRunStream, AgentSnapshot,
    AgentStateSnapshot, BlackboardSnapshot, CancelHandle, DelegateSnapshot, DelegationSnapshot,
    ExternalDelegateSnapshot, MailboxSnapshot,
};
pub use approval::{Approval, ApprovalDecision, ApprovalPolicy, FacadeApproval};
pub use chat::{Chat, ChatBuilder, ChatSession, ChatSessionBuilder, RunStream};
pub use collab::Collaboration;
pub use config::{ModelConfig, ProviderConfig, ProviderConfigBuilder};
pub use delegate::{AgentWorkerBuilder, Delegation, LocalSubagent};
pub use error::FacadeError;
pub use external::{
    CapabilitySource, ExternalAgentCapabilities, ExternalDelegateStatus, ExternalRunMode,
    ManagedExternalAgent, ManagedExternalAgentBuilder, ManagedExternalDelegate,
    RegistryExternalSessionHandler, RestoreExternal, RetainedExternalSession,
    default_external_session_handler, default_external_session_handler_with_capabilities,
};
pub use ids::FacadeIds;
pub use run::{
    ApprovalRequest, ArtifactRef, DelegationMessage, DelegationProgress, DelegationStatus,
    DelegationTrace, EscalationTrace, IntoUserMessage, RawEventKind, Reply, RunEvent, RunOutput,
    ToolTrace, UsageSummary, WireRunEvent, WireRunOutput,
};
pub use tool::{
    FacadeToolRegistry, IntoToolResult, Tool, ToolContext, ToolContextParts, ToolResult,
};
