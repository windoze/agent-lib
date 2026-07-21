//! Provider-neutral data model for the external-agent session effect.
//!
//! An *external agent* (Claude Code, Codex, OpenCode, a bespoke CLI/SDK, ...) is
//! driven through a single blocking effect — the [external session
//! effect](crate::agent::external) — rather than a raw LLM call. This module
//! defines only the **serde-friendly data shapes** for that effect: the request
//! a machine reifies, the result a handler returns, the streamed observations it
//! buffers, and the classified failure it can report. It deliberately contains
//! no runtime handles (CLI process, SDK client, stdout reader, task set); those
//! stay behind the handler and runtime-handle boundary, mirroring the split used
//! by [`AgentSpec`](crate::agent::AgentSpec) versus the tool registry traits.
//!
//! The effect DTOs here are wired into the effect model in later milestones:
//! `RequirementKind::NeedExternalSession` and the `ExternalSessionHandler` trait
//! are added on top of them.
//!
//! Alongside those DTOs this module also carries the external-agent machine's
//! own persistence shapes — [`ExternalAgentSpec`] (static recipe),
//! [`ExternalAgentState`] plus [`ExternalAgentCursor`] (serializable running
//! state), and the non-serde [`ExternalRuntimeHandles`] holder — mirroring the
//! [`AgentSpec`](crate::agent::AgentSpec) /
//! [`AgentState`](crate::agent::AgentState) /
//! [`AgentRuntimeHandles`](crate::agent::AgentRuntimeHandles) split (design §4).
//!
//! # Persistence boundary
//!
//! Every effect DTO in this module derives `Clone, Debug, PartialEq, Eq,
//! Serialize, Deserialize`, so a driver can serialize an outstanding
//! [`ExternalSessionRequest`], restore it in another process, and re-register
//! it, and can persist an [`ExternalSessionResult`] for replay.
//! [`ExternalAgentState`] serializes through the same Conversation snapshot
//! boundary as [`AgentState`](crate::agent::AgentState). Live handles are kept
//! out of these shapes on purpose: they live behind
//! [`ExternalRuntimeHandles`].
//!
//! # Blocking effect versus continuous stream
//!
//! An external session is continuously streaming (text, commands, patches,
//! permission prompts) while the effect model is one blocking request → one
//! result. The reconciliation (design §5.5) is that a handler advances the
//! session only to the **next decision point** —
//! [`Completed`](ExternalSessionResult::Completed),
//! [`PausedForInteraction`](ExternalSessionResult::PausedForInteraction), or
//! [`Failed`](ExternalSessionResult::Failed) — and returns every event observed
//! before that point in `observations`, so the blocking result marks only the
//! control-flow transfer while the non-blocking stream rides along as
//! sequenced [`ExternalObservedEvent`] values.

use crate::{
    agent::{
        AgentId, AgentSpecRef, SubagentOutput,
        interaction::{Interaction, InteractionResponse},
        spec::WorktreeRef,
        tool::ToolRuntimeError,
    },
    model::{
        content::ContentBlock,
        tool::{Tool, ToolCall, ToolResponse, ToolStatus},
        usage::Usage,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

#[cfg(feature = "external-acp")]
mod acp;
mod adapter;
mod budget;
mod capability;
#[cfg(feature = "external-claude-code")]
mod claude_code;
#[cfg(feature = "external-codex")]
mod codex;
mod config;
mod dispatch;
mod escalation;
mod handler;
mod machine;
#[cfg(feature = "external-opencode")]
mod opencode;
#[cfg(any(
    feature = "external-acp",
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
mod process;
mod profile;
mod registry;
mod runtime;
mod shutdown;
mod sink;
mod spec;
mod state;
mod worktree;

#[cfg(feature = "external-acp")]
pub use acp::{
    ACP_RUNTIME_LABEL, ACP_WIRE_VERSION, AcpAdapter, AcpConfig, AcpDecision, AcpLauncher,
    AcpNegotiatedCapabilities, AcpPermissionOption, AcpPermissionOptionKind, AcpStreamDecoder,
    PendingClientRequest, SpawnedAcpAgent, TokioProcessLauncher, acp_runtime_kind,
    capabilities_from_initialize,
};
pub use adapter::{ExternalRuntimeAdapter, ExternalRuntimeSession, RuntimeDecisionPoint};
pub use budget::{
    ExternalSessionSweeper, ExternalUsageCharge, ExternalUsageChargingHandler, NoSweep,
    budget_exhausted,
};
pub use capability::{ExternalCapability, ExternalRuntimeCapabilities};
#[cfg(feature = "external-claude-code")]
pub use claude_code::{
    ClaudeCodeAdapter, ClaudeCodeConfig, ClaudeCodeProbeExec, ClaudeDecision, ClaudeDecodeContext,
    ClaudeStreamDecoder, ProbeOutput, SystemClaudeCodeExec, probe, probe_with_exec,
};
#[cfg(feature = "external-codex")]
pub use codex::{
    CodexAdapter, CodexConfig, CodexDecision, CodexDecodeContext, CodexProbeExec, CodexProbeOutput,
    CodexStreamDecoder, SystemCodexExec, probe as codex_probe,
    probe_with_exec as codex_probe_with_exec,
};
pub use config::{ExternalAgentMachineConfig, ExternalToolFailurePolicy};
pub use dispatch::{
    CostPreference, DispatchError, DispatchReason, Dispatcher, ImpactScope, RuleRouter,
    ScriptedTaskEvaluator, TaskDescriptor, TaskEvaluator, Uncertainty, Worker, WorkerChoice,
    WorkerRoster,
};
pub use escalation::{
    EscalationError, EscalationOutcome, Escalator, HumanGate, ScriptedVerifier, Verifier,
    WorkerReport,
};
pub use handler::RegistryExternalSessionHandler;
pub use machine::{ExternalAgentMachine, ExternalReconfigOutcome, ExternalReconfigTiming};
#[cfg(feature = "external-opencode")]
pub use opencode::{
    OpenCodeAdapter, OpenCodeConfig, OpenCodeDecision, OpenCodeDecodeContext, OpenCodeProbeExec,
    OpenCodeProbeOutput, OpenCodeStreamDecoder, SystemOpenCodeExec, probe as opencode_probe,
    probe_with_exec as opencode_probe_with_exec,
};
pub use profile::{
    Capability, CostTier, EscalationRules, EscalationTrigger, WorkerProfile, WorkerProfileRef,
    WorkerProfileRegistry,
};
pub use registry::{ExternalSessionRegistry, LiveSessionHandle};
pub use runtime::ExternalRuntimeHandles;
pub use shutdown::ExternalSessionShutdown;
pub use sink::{DiscardEventSink, ExternalEventSink};
pub use spec::ExternalAgentSpec;
pub use state::{ExternalAgentCursor, ExternalAgentState};
pub use worktree::{
    GitWorktreeManager, PreparedWorktree, SystemGit, WorktreeCleanupOutcome, WorktreeError,
    WorktreeGitExec, WorktreeManager,
};

/// Which external coding-agent runtime backs a session.
///
/// The concrete runtimes are named so a host can route a request to the right
/// adapter; [`Custom`](Self::Custom) carries a free-form identifier for
/// runtimes this crate does not name explicitly. This is a provider-neutral
/// selector, not a wire protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalRuntimeKind {
    /// Anthropic Claude Code CLI/SDK.
    ClaudeCode,
    /// OpenAI Codex CLI/SDK.
    Codex,
    /// OpenCode runtime.
    OpenCode,
    /// A host-defined runtime identified by a free-form label.
    Custom(String),
}

/// Resumable facts about an in-flight or completed external session.
///
/// This is the persistable slice of session state: it records only what a driver
/// needs to re-align with a runtime across process restarts. The live process,
/// SDK client, stdout reader, and watcher stay behind the runtime-handle
/// boundary and never appear here. [`last_event_seq`](Self::last_event_seq) lets
/// a resume skip events already consumed so observations are not replayed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSessionRef {
    /// Runtime that owns this session.
    pub runtime: ExternalRuntimeKind,
    /// Runtime-assigned session identifier, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Opaque reference to a stored transcript, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_ref: Option<String>,
    /// Opaque token used to resume the session, when the runtime supports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<String>,
    /// Sequence number of the last event consumed, used to align on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_seq: Option<u64>,
}

/// Host-level policy for how a runtime handles permission-gated actions.
///
/// These are provider-neutral hints; a handler maps them onto whatever the
/// concrete runtime exposes. Regardless of the mode, an external runtime's
/// output is always treated as untrusted and cannot itself widen the host's
/// permission boundary (design §10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPermissionMode {
    /// Every gated action must be approved through an interaction (safest).
    Prompt,
    /// Auto-approve edits inside the worktree; still prompt for higher-risk
    /// actions (writing outside the worktree, network, long commands).
    AcceptEdits,
    /// Read-only / planning mode: mutating actions are refused.
    Plan,
    /// Bypass permission prompts; the host accepts full responsibility.
    BypassPermissions,
}

/// Worktree isolation level assigned to an external agent.
///
/// Stronger isolation reduces cross-agent edit conflicts at the cost of setup;
/// a scheduler should prefer an independent worktree for capable workers
/// (design §10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeIsolation {
    /// Multiple agents share one worktree (high-conflict; use sparingly).
    Shared,
    /// Each agent gets its own persistent worktree.
    PerAgentWorktree,
    /// Each session runs in an ephemeral git worktree torn down afterward.
    EphemeralGitWorktree,
}

impl Default for WorktreeIsolation {
    /// Isolates each agent by default (design §10 "默认 worktree 隔离").
    ///
    /// With no scheduling policy in play the safe default gives every agent its
    /// own [`PerAgentWorktree`](Self::PerAgentWorktree) so concurrent edits do
    /// not collide. A cost-aware scheduler refines this per worker via
    /// [`CostTier::recommended_isolation`](crate::agent::external::CostTier::recommended_isolation),
    /// which shares a worktree only for cheap workers and gives strong workers
    /// an independent (ephemeral) worktree.
    fn default() -> Self {
        Self::PerAgentWorktree
    }
}

/// Whether and how a handler surfaces fine-grained session events.
///
/// This governs the non-blocking observation path only; the blocking
/// continuation is always expressed through the [`ExternalSessionResult`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalStreamPolicy {
    /// Buffer events and return them at each decision point (default model).
    Buffered,
    /// Buffer events and also forward them to a live sink as they arrive.
    Streaming,
    /// Do not retain fine-grained events; only decision-point results matter.
    Disabled,
}

/// Static policy knobs applied to one external session.
///
/// Every knob is consumed by a designated layer — none is advisory-only
/// (M2-7 / M-PROM-5):
///
/// - [`permission_mode`](Self::permission_mode) is applied by the runtime
///   adapter at session start/resume, overriding the adapter config's
///   construction-time mode (which remains the fallback for adapter-level
///   operations that carry no request, such as capability probes).
/// - [`isolation`](Self::isolation) is applied by
///   [`ExternalSessionRegistry`]
///   through its [`WorktreeManager`]:
///   the prepared path is handed to the adapter as the session's working
///   directory ([`ExternalSessionRequest::session_dir`]) and cleaned up with
///   the session's shutdown disposition.
/// - [`max_turns`](Self::max_turns) is enforced by the
///   [`ExternalAgentMachine`] as a
///   bound on runtime round-trips (decision loops), uniformly across runtimes;
///   it is not passed as a CLI flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSessionPolicy {
    /// How permission-gated actions are handled.
    pub permission_mode: ExternalPermissionMode,
    /// Worktree isolation level for the session.
    pub isolation: WorktreeIsolation,
    /// Optional cap on the number of agent turns (runtime round-trips) for the
    /// session, machine-enforced with a classified
    /// [`LimitExceeded`](ExternalAgentError::LimitExceeded) failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// How fine-grained events are surfaced.
    pub stream_events: ExternalStreamPolicy,
}

/// Stable identifier correlating a batch of external tool calls with the results
/// fed back for them.
///
/// A runtime pauses on [`PausedForToolCalls`](ExternalSessionResult::PausedForToolCalls)
/// carrying one batch of [`ExternalToolCall`] values plus a batch id; the host
/// executes those calls and returns every result under the same id via
/// [`RespondToolResults`](ExternalSessionInput::RespondToolResults), so the
/// runtime can match the answers to the pause it emitted. The value is an opaque
/// runtime-assigned token — this crate never parses it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExternalToolBatchId(String);

impl ExternalToolBatchId {
    /// Wraps a runtime-assigned batch token.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the opaque batch token as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One tool call a runtime asks the host to execute during a paused session.
///
/// This is the provider-neutral shape a runtime adapter decodes each pending
/// tool call into. [`provider_call_id`](Self::provider_call_id) is the runtime's
/// own correlation id: it becomes the [`ToolCall::id`] the machine bridges into a
/// `NeedTool` requirement (see [`to_tool_call`](Self::to_tool_call)) and the id
/// the matching [`ExternalToolResult`] answers, so the runtime can line the
/// result up with the call. [`raw`](Self::raw) is an escape hatch for unmodeled
/// provider fields and must not carry stable logic (design §5.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalToolCall {
    /// Runtime-assigned identifier used to correlate the result.
    pub provider_call_id: String,
    /// Name of the tool the runtime selected.
    pub name: String,
    /// Fully parsed JSON input supplied by the runtime.
    pub input: Value,
    /// Unmodeled provider fields preserved for forward compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

impl ExternalToolCall {
    /// Bridges this runtime tool call into a provider-neutral [`ToolCall`].
    ///
    /// The [`provider_call_id`](Self::provider_call_id) is preserved as the
    /// [`ToolCall::id`] so the host response can answer the runtime's own
    /// correlation id, while `name` and `input` are copied verbatim. The
    /// [`raw`](Self::raw) escape hatch is intentionally dropped: it holds
    /// unmodeled provider fields that must not leak into the stable
    /// tool-execution path (design §5.3).
    #[must_use]
    pub fn to_tool_call(&self) -> ToolCall {
        ToolCall {
            id: self.provider_call_id.clone(),
            name: self.name.clone(),
            input: self.input.clone(),
            extra: Map::new(),
        }
    }
}

/// One tool result the host feeds back to a runtime for a prior
/// [`ExternalToolCall`].
///
/// [`provider_call_id`](Self::provider_call_id) echoes the call's runtime
/// correlation id so the runtime can pair the answer with the call it paused on.
/// [`status`](Self::status) and [`content`](Self::content) mirror the host's
/// [`ToolResponse`]; [`error`](Self::error) carries a stable diagnostic when the
/// tool could not be executed at all (a [`ToolRuntimeError`], distinct from a
/// tool that ran and returned [`ToolStatus::Error`] content). [`raw`](Self::raw)
/// is an escape hatch for unmodeled provider fields (design §5.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalToolResult {
    /// Runtime correlation id this result answers.
    pub provider_call_id: String,
    /// Provider-neutral outcome of attempting the tool call.
    pub status: ToolStatus,
    /// Multimodal content returned to the runtime.
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// Stable diagnostic text when the tool could not be executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Unmodeled provider fields preserved for forward compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

impl ExternalToolResult {
    /// Builds a result from a host [`ToolResponse`], preserving the runtime
    /// correlation id, four-state status, and multimodal content.
    ///
    /// The response's [`tool_call_id`](ToolResponse::tool_call_id) is the
    /// provider correlation id the runtime paused on (design §5.3), so it is
    /// echoed as [`provider_call_id`](Self::provider_call_id). A `ToolResponse`
    /// is a tool that *ran* — even a [`ToolStatus::Error`] outcome carries its
    /// detail in [`content`](ToolResponse::content) — so [`error`](Self::error)
    /// stays `None`; it is reserved for orchestration failures surfaced through
    /// [`from_tool_runtime_error`](Self::from_tool_runtime_error).
    #[must_use]
    pub fn from_tool_response(response: &ToolResponse) -> Self {
        Self {
            provider_call_id: response.tool_call_id.clone(),
            status: response.status,
            content: response.content.clone(),
            error: None,
            raw: None,
        }
    }

    /// Builds an [`Error`](ToolStatus::Error) result from a
    /// [`ToolRuntimeError`] that prevented the call from executing.
    ///
    /// The framework's stable diagnostic ([`ToolRuntimeError`]'s `Display`) is
    /// retained in both [`error`](Self::error) and as a
    /// [`Text`](ContentBlock::Text) content block, so the runtime receives the
    /// failure as tool output while callers keep a stable typed reason. The text
    /// is a fixed diagnostic that never embeds secrets or tool input (design
    /// §5.3, §12).
    #[must_use]
    pub fn from_tool_runtime_error(
        provider_call_id: impl Into<String>,
        error: &ToolRuntimeError,
    ) -> Self {
        let detail = error.to_string();
        Self {
            provider_call_id: provider_call_id.into(),
            status: ToolStatus::Error,
            content: vec![ContentBlock::Text {
                text: detail.clone(),
                extra: serde_json::Map::new(),
            }],
            error: Some(detail),
            raw: None,
        }
    }
}

/// Stable identifier correlating a runtime's subagent spawn request with the
/// output fed back for it.
///
/// A runtime pauses on
/// [`PausedForSubagent`](ExternalSessionResult::PausedForSubagent) carrying an
/// [`ExternalSubagentRequest`] tagged with this id; the host drives the child
/// agent and returns its [`ExternalSubagentOutput`] under the same id via
/// [`RespondSubagent`](ExternalSessionInput::RespondSubagent), so the runtime can
/// match the answer to the spawn it emitted. The value is an opaque
/// runtime-assigned token — this crate never parses it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExternalSubagentRequestId(String);

impl ExternalSubagentRequestId {
    /// Wraps a runtime-assigned subagent request token.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the opaque request token as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One subagent spawn a runtime asks the host to drive during a paused session.
///
/// This is the provider-neutral shape a runtime adapter decodes a native child
/// task request into. The host bridges it into a standard `NeedSubagent`
/// requirement — reusing [`spec_ref`](Self::spec_ref), [`brief`](Self::brief),
/// and [`result_schema`](Self::result_schema) unchanged — rather than spawning
/// the child outside the host's own subagent machinery (design §4, §5.2). The
/// child runs under the host's [`DrivingSubagentHandler`](crate::agent) with its
/// depth, budget, and cancel accounting; its result is returned to the runtime
/// under [`request_id`](Self::request_id). [`raw`](Self::raw) is an escape hatch
/// for unmodeled provider fields and must not carry stable logic (design §5.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSubagentRequest {
    /// Runtime-assigned identifier used to correlate the output.
    pub request_id: ExternalSubagentRequestId,
    /// Which subagent specification the host should drive.
    pub spec_ref: AgentSpecRef,
    /// The brief handed to the child agent as its opening interaction.
    pub brief: Interaction,
    /// Optional JSON schema the child's structured result must satisfy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<Value>,
    /// Unmodeled provider fields preserved for forward compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

/// One subagent result the host feeds back to a runtime for a prior
/// [`ExternalSubagentRequest`].
///
/// This is the serde-friendly counterpart of the runtime-only
/// [`SubagentOutput`]: the host's subagent result never persists inside a
/// [`RequirementResult`](crate::agent::RequirementResult), so a dedicated
/// persistable DTO carries it across the external session boundary without
/// giving [`SubagentOutput`] serde derives it does not otherwise need. Build one
/// from a host result with the [`From<SubagentOutput>`](Self::from) conversion.
/// [`raw`](Self::raw) is an escape hatch for unmodeled provider fields
/// (design §5.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSubagentOutput {
    /// Summary the child agent produced, echoed to the runtime.
    pub summary: String,
    /// Unmodeled provider fields preserved for forward compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

impl From<SubagentOutput> for ExternalSubagentOutput {
    /// Bridges a host [`SubagentOutput`] into the persistable external DTO,
    /// preserving the summary. The [`raw`](Self::raw) escape hatch starts empty:
    /// it is reserved for runtime-specific fields an adapter attaches when
    /// echoing the result back, never for host state.
    fn from(output: SubagentOutput) -> Self {
        Self {
            summary: output.summary,
            raw: None,
        }
    }
}

/// What a single external session effect is asked to do.
///
/// A machine reifies one of these per [`ExternalSessionRequest`]: begin a new
/// session, continue an existing one with a message, feed a resolved interaction
/// back to the runtime, or shut the session down.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSessionInput {
    /// Start a fresh session with an initial prompt.
    Start {
        /// Prompt handed to the runtime as opaque data.
        prompt: String,
    },
    /// Continue an existing session with a follow-up message.
    Continue {
        /// Message handed to the runtime as opaque data.
        message: String,
    },
    /// Feed a resolved interaction back into a paused session.
    RespondInteraction {
        /// Identifier of the action the runtime paused on.
        action_id: String,
        /// The resolution the host produced for that action.
        response: InteractionResponse,
    },
    /// Feed host tool-execution results back into a session paused on a tool-call
    /// batch.
    ///
    /// The runtime paused with
    /// [`PausedForToolCalls`](ExternalSessionResult::PausedForToolCalls) carrying
    /// a [`batch_id`](Self::RespondToolResults::batch_id); the host executes the
    /// bridged calls and returns every [`ExternalToolResult`] under that same id
    /// so the runtime can correlate the answers with the batch it emitted.
    RespondToolResults {
        /// Batch the results answer, echoed from the pause.
        batch_id: ExternalToolBatchId,
        /// One result per tool call in the batch, keyed by provider call id.
        results: Vec<ExternalToolResult>,
    },
    /// Feed a host subagent result back into a session paused on a subagent
    /// spawn request.
    ///
    /// The runtime paused with
    /// [`PausedForSubagent`](ExternalSessionResult::PausedForSubagent) carrying a
    /// [`request_id`](Self::RespondSubagent::request_id); the host drives the
    /// child agent and returns its [`ExternalSubagentOutput`] under that same id
    /// so the runtime can correlate the result with the spawn it emitted.
    RespondSubagent {
        /// Request the output answers, echoed from the pause.
        request_id: ExternalSubagentRequestId,
        /// The child agent's result bridged back to the runtime.
        output: ExternalSubagentOutput,
    },
    /// Shut the session down and release its runtime handles.
    Shutdown,
}

/// One reified external session effect awaiting fulfillment.
///
/// This is the request half of the effect: a handler advances the session
/// described here to its next decision point and returns an
/// [`ExternalSessionResult`]. The optional [`session`](Self::session) is present
/// when continuing or resuming; it is `None` for a first
/// [`Start`](ExternalSessionInput::Start).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSessionRequest {
    /// Identity of the agent that owns the session.
    pub agent_id: AgentId,
    /// Runtime that should service the request.
    pub runtime: ExternalRuntimeKind,
    /// Filesystem boundary the session runs within.
    ///
    /// This is the *base* worktree the agent was assigned; the session layer
    /// may resolve it into a different concrete directory (see
    /// [`session_dir`](Self::session_dir)) before the runtime is spawned.
    pub worktree: WorktreeRef,
    /// Effective working directory the session runs in, if resolved.
    ///
    /// The machine always mints requests with `None` here. The session layer
    /// ([`ExternalSessionRegistry`])
    /// fills it in with the [`PreparedWorktree`]
    /// path produced by applying [`ExternalSessionPolicy::isolation`] through its
    /// [`WorktreeManager`] before the
    /// adapter starts or resumes the runtime. When `Some`, adapters treat it as
    /// the session's working directory, overriding the adapter config's
    /// construction-time `working_dir` (M2-7 / M-PROM-5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<WorktreeRef>,
    /// Existing session to continue or resume, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ExternalSessionRef>,
    /// What the session should do this step.
    pub input: ExternalSessionInput,
    /// Provider-neutral tool declarations exposed to the runtime.
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// Policy knobs applied to the session.
    pub policy: ExternalSessionPolicy,
}

/// One structured observation emitted while a session advances.
///
/// Events are buffered by a handler and returned in the `observations` of an
/// [`ExternalSessionResult`]; a machine converts them into notifications after
/// resume. The variants are deliberately structured (rather than raw text) so
/// text, commands, patches, and permission prompts stay distinguishable
/// (design §5.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAgentEvent {
    /// The runtime started a session, optionally reporting its id.
    SessionStarted {
        /// Runtime-assigned session identifier, if reported at start.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// A chunk of assistant text was produced.
    TextDelta {
        /// The text increment.
        text: String,
    },
    /// The runtime began executing a shell command.
    CommandStarted {
        /// The command line, as reported by the runtime (untrusted).
        command: String,
        /// Working directory the command runs in.
        cwd: String,
    },
    /// A shell command finished.
    CommandFinished {
        /// Process exit code, when one was captured.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Trailing captured stdout (may be truncated).
        stdout_tail: String,
        /// Trailing captured stderr (may be truncated).
        stderr_tail: String,
    },
    /// The runtime applied or proposed a file patch.
    FilePatch {
        /// Path affected by the patch.
        path: String,
        /// Short human-readable summary of the change (untrusted).
        summary: String,
        /// Opaque reference to the full diff, when stored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_ref: Option<String>,
    },
    /// The runtime requested permission for a gated action.
    PermissionRequested {
        /// Identifier used to answer this request via
        /// [`ExternalSessionInput::RespondInteraction`].
        action_id: String,
        /// Short human-readable summary of the requested action (untrusted).
        summary: String,
    },
    /// A tool invocation started.
    ToolStarted {
        /// Name of the tool being invoked.
        name: String,
    },
    /// A tool invocation finished with a terminal status.
    ToolFinished {
        /// Name of the tool that finished.
        name: String,
        /// Terminal outcome of the invocation.
        status: ToolStatus,
    },
    /// The agent sent a message to another agent (mixed-agent sessions).
    ///
    /// This is the provider-neutral shape of a `send_message` collab event; the
    /// facade collab bridge routes it into the shared
    /// [`Mailbox`](crate::agent::collab::Mailbox) when one is provisioned
    /// (`docs/facade-api.md` §14).
    MessageSent {
        /// Recipient agent identity.
        to: AgentId,
        /// Short human-readable summary of the message (untrusted).
        summary: String,
    },
    /// A tracked task's status changed.
    ///
    /// This is the provider-neutral shape of a `plan_update` collab event; the
    /// facade collab bridge reflects it into the shared
    /// [`Plan`](crate::agent::collab::Plan) when one is provisioned
    /// (`docs/facade-api.md` §14).
    TaskUpdated {
        /// Identifier of the task whose status changed.
        task_id: String,
        /// New status label reported by the runtime.
        status: String,
    },
    /// The agent posted a message to a shared blackboard channel (mixed-agent
    /// sessions).
    ///
    /// This is the provider-neutral shape of a `blackboard_post` collab event.
    /// Like [`MessageSent`](Self::MessageSent) it is a model-complete
    /// observation the facade collab bridge routes into the shared
    /// [`Blackboard`](crate::agent::collab::Blackboard) when one is provisioned
    /// (`docs/facade-api.md` §14); a runtime that speaks its own private
    /// blackboard protocol is normalized into this event rather than bridged
    /// directly, so the same collaboration stays observable and replayable
    /// across runtimes (design §3.5).
    BlackboardPosted {
        /// Channel the message was posted to.
        channel: String,
        /// Short human-readable summary of the message (untrusted).
        summary: String,
    },
    /// The session finished producing output for this step.
    SessionCompleted,
}

/// A buffered [`ExternalAgentEvent`] tagged with its runtime sequence number.
///
/// A handler advances a session to its next decision point and buffers every
/// event it observed before that point. Rather than an unlabelled
/// [`ExternalAgentEvent`] list, each observation carries a monotonically
/// increasing `seq` so the [`ExternalAgentMachine`] can replay observations
/// **exactly once, event by event** across resumes: on resume it emits only the
/// events whose `seq` is greater than the last one it already consumed (design
/// §5.5). This is strictly finer than a batch-level cursor — a decision point
/// whose buffer overlaps the previously consumed prefix replays only its unseen
/// suffix.
///
/// `seq` is assigned by the runtime adapter (or a cassette) as it decodes the
/// stream; it is the sole replay-progress marker and must increase across the
/// observations of a session. The companion
/// [`ExternalSessionRef::last_event_seq`] records the high-water mark a machine
/// has consumed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalObservedEvent {
    /// Monotonic sequence number identifying this event in the session stream.
    pub seq: u64,
    /// The observed event.
    pub event: ExternalAgentEvent,
}

impl ExternalObservedEvent {
    /// Pairs an [`ExternalAgentEvent`] with its runtime `seq`.
    #[must_use]
    pub fn new(seq: u64, event: ExternalAgentEvent) -> Self {
        Self { seq, event }
    }

    /// Wraps a list of events into sequenced observations by assigning each a
    /// synthetic index-based `seq` (`0`, `1`, `2`, …), preserving order.
    ///
    /// This is a convenience for tests and fixtures that carry observations but
    /// do not exercise per-event replay alignment. It **must not** back
    /// production dedup: a real runtime adapter assigns `seq` from the decoded
    /// stream, never from vector position.
    #[must_use]
    pub fn unsequenced_for_tests(events: Vec<ExternalAgentEvent>) -> Vec<Self> {
        events
            .into_iter()
            .enumerate()
            .map(|(index, event)| Self::new(index as u64, event))
            .collect()
    }
}

/// Category of an artifact produced by an external session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalArtifactKind {
    /// An applied or proposed patch.
    Patch,
    /// A raw diff.
    Diff,
    /// A recorded test result.
    TestResult,
    /// A produced or modified file.
    File,
    /// An artifact that does not fit the named categories.
    Other,
}

/// Reference to an artifact an external session produced.
///
/// The artifact content itself (full diff, test log, file blob) is not carried
/// inline; [`reference`](Self::reference) points at wherever the host stored it,
/// keeping large or sensitive payloads out of the effect data (design §11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalArtifactRef {
    /// What kind of artifact this is.
    pub kind: ExternalArtifactKind,
    /// Short human-readable summary of the artifact (untrusted).
    pub summary: String,
    /// Path the artifact relates to, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Opaque reference to the stored artifact content, when stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl ExternalArtifactRef {
    /// Derives a [`Patch`](ExternalArtifactKind::Patch) artifact reference from a
    /// [`FilePatch`](ExternalAgentEvent::FilePatch) observation, or `None` for any
    /// other event.
    ///
    /// A runtime reports each applied/proposed change as a `FilePatch`
    /// observation; this maps one into the artifact-reference shape a completed
    /// session records, carrying the affected `path`, the untrusted `summary`,
    /// and the opaque `diff_ref` (if any) as the stored [`reference`](Self::reference).
    /// Only these references are copied — never the full diff — so the mapping
    /// stays redaction-safe (design §11, §12).
    #[must_use]
    pub fn from_file_patch(event: &ExternalAgentEvent) -> Option<Self> {
        match event {
            ExternalAgentEvent::FilePatch {
                path,
                summary,
                diff_ref,
            } => Some(Self {
                kind: ExternalArtifactKind::Patch,
                summary: summary.clone(),
                path: Some(path.clone()),
                reference: diff_ref.clone(),
            }),
            _ => None,
        }
    }
}

/// Collects the [`FilePatch`](ExternalAgentEvent::FilePatch) observations in
/// `events` into [`Patch`](ExternalArtifactKind::Patch) artifact references,
/// preserving order.
///
/// This is a convenience for a handler that wants to fold the patch events it
/// buffered before a decision point into
/// [`ExternalAgentOutput::artifacts`](ExternalAgentOutput::artifacts); every
/// non-`FilePatch` event is ignored. Only artifact references are produced —
/// never the diffs themselves — keeping the result redaction-safe (design §11,
/// §12). See [`ExternalArtifactRef::from_file_patch`] for the per-event mapping.
#[must_use]
pub fn collect_file_patch_artifacts(events: &[ExternalAgentEvent]) -> Vec<ExternalArtifactRef> {
    events
        .iter()
        .filter_map(ExternalArtifactRef::from_file_patch)
        .collect()
}

/// Collects the [`FilePatch`](ExternalAgentEvent::FilePatch) observations in a
/// sequenced `observations` buffer into [`Patch`](ExternalArtifactKind::Patch)
/// artifact references, preserving order.
///
/// This is the [`ExternalObservedEvent`] counterpart of
/// [`collect_file_patch_artifacts`]: a handler folding the observations it
/// buffered before a decision point into
/// [`ExternalAgentOutput::artifacts`](ExternalAgentOutput::artifacts) can call
/// this directly instead of manually mapping each `ExternalObservedEvent` back
/// to its inner event. The `seq` labels are irrelevant to artifact extraction
/// and are ignored; only patch references (never diffs) are produced, keeping
/// the result redaction-safe (design §11, §12).
#[must_use]
pub fn collect_file_patch_artifacts_from_observed(
    observations: &[ExternalObservedEvent],
) -> Vec<ExternalArtifactRef> {
    observations
        .iter()
        .filter_map(|observed| ExternalArtifactRef::from_file_patch(&observed.event))
        .collect()
}

/// Terminal output of an external session that reached
/// [`Completed`](ExternalSessionResult::Completed).
///
/// `usage` and `cost_micros` are independent optional fields because a black-box
/// runtime may not report real token counts or cost; a handler must leave them
/// `None` rather than fabricate an estimate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalAgentOutput {
    /// Final human-readable summary produced by the session (untrusted).
    pub summary: String,
    /// Artifacts the session produced.
    #[serde(default)]
    pub artifacts: Vec<ExternalArtifactRef>,
    /// Token usage reported by the runtime, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Cost in micro-units reported by the runtime, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
}

/// Result of advancing an external session to its next decision point.
///
/// A handler never runs a session to completion in one blocking call; it returns
/// as soon as it reaches [`Completed`](Self::Completed) (no further input needed
/// this step), [`PausedForInteraction`](Self::PausedForInteraction) (an approval
/// or clarification is needed), or [`Failed`](Self::Failed). Every event observed
/// before the decision point is returned in `observations`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSessionResult {
    /// The session produced output and needs no further input this step.
    Completed {
        /// Updated resumable session facts.
        session: ExternalSessionRef,
        /// Terminal output of the session step.
        output: ExternalAgentOutput,
        /// Events observed before completion.
        #[serde(default)]
        observations: Vec<ExternalObservedEvent>,
    },
    /// The session paused awaiting an interaction (approval or clarification).
    ///
    /// The handler translates the runtime's permission/clarification prompt into
    /// a plain [`Interaction`]; the machine then emits a standard
    /// `NeedInteraction` requirement and feeds the answer back with
    /// [`ExternalSessionInput::RespondInteraction`].
    PausedForInteraction {
        /// Updated resumable session facts.
        session: ExternalSessionRef,
        /// Runtime-assigned handle for the paused action.
        ///
        /// The machine records this as its pending action and echoes it back
        /// verbatim in the
        /// [`RespondInteraction`](ExternalSessionInput::RespondInteraction) it
        /// emits once the interaction resolves, so the runtime can correlate the
        /// answer with the action it paused on. It is carried explicitly here
        /// because the neutral [`Interaction`] request does not yet model a
        /// permission action id; once `InteractionKind::Permission` lands
        /// (milestone 4) this stays the canonical handle the machine feeds back.
        action_id: String,
        /// The interaction the host must resolve.
        request: Interaction,
        /// Events observed before the pause.
        #[serde(default)]
        observations: Vec<ExternalObservedEvent>,
    },
    /// The session paused awaiting host execution of a batch of tool calls.
    ///
    /// The handler surfaces the runtime's pending tool calls as provider-neutral
    /// [`ExternalToolCall`] values under a [`batch_id`](Self::PausedForToolCalls::batch_id).
    /// The machine bridges each into a `NeedTool` requirement (via
    /// [`ExternalToolCall::to_tool_call`]), gathers the host results, and feeds
    /// them back with
    /// [`RespondToolResults`](ExternalSessionInput::RespondToolResults) carrying
    /// the same batch id. Driving this decision point in the machine lands with
    /// milestone 2; the protocol shape is defined here.
    PausedForToolCalls {
        /// Updated resumable session facts.
        session: ExternalSessionRef,
        /// Identifier the matching
        /// [`RespondToolResults`](ExternalSessionInput::RespondToolResults) echoes.
        batch_id: ExternalToolBatchId,
        /// Tool calls the host must execute this step.
        calls: Vec<ExternalToolCall>,
        /// Events observed before the pause.
        #[serde(default)]
        observations: Vec<ExternalObservedEvent>,
    },
    /// The session paused awaiting host execution of a subagent spawn request.
    ///
    /// The handler surfaces the runtime's native child-task request as a
    /// provider-neutral [`ExternalSubagentRequest`]. The machine bridges it into
    /// a standard `NeedSubagent` requirement (reusing its `spec_ref`, `brief`,
    /// and `result_schema`), drives the child under the host's own subagent
    /// machinery, and feeds the result back with
    /// [`RespondSubagent`](ExternalSessionInput::RespondSubagent) carrying the
    /// same request id. Driving this decision point in the machine lands with
    /// milestone 3; the protocol shape is defined here.
    PausedForSubagent {
        /// Updated resumable session facts.
        session: ExternalSessionRef,
        /// The subagent spawn the host must drive this step.
        request: ExternalSubagentRequest,
        /// Events observed before the pause.
        #[serde(default)]
        observations: Vec<ExternalObservedEvent>,
    },
    /// The session failed; the error records whether side effects may remain.
    Failed {
        /// Resumable session facts, when a session existed before the failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<ExternalSessionRef>,
        /// Classified failure reason.
        error: ExternalAgentError,
        /// Events observed before the failure.
        #[serde(default)]
        observations: Vec<ExternalObservedEvent>,
    },
}

/// Classified failure from an external session.
///
/// The variants separate the *diagnosable reason* from the *side-effect risk*:
/// [`SessionLost`](Self::SessionLost) and
/// [`ShutdownFailed`](Self::ShutdownFailed) must be treated as "side effects may
/// remain", so a scheduler should not reuse the worktree as clean by default
/// (design §5.4, §6.4, §10).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAgentError {
    /// The runtime could not be launched (missing binary, SDK init, auth).
    #[error("failed to launch {runtime:?} runtime: {detail}")]
    Launch {
        /// Runtime that failed to start.
        runtime: ExternalRuntimeKind,
        /// Stable diagnostic text.
        detail: String,
    },
    /// The session process or connection dropped or crashed mid-advance.
    #[error("external session lost: {detail}")]
    SessionLost {
        /// Session facts known before the loss, when any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<ExternalSessionRef>,
        /// Stable diagnostic text.
        detail: String,
    },
    /// A stream event or transcript failed to parse (protocol/version drift).
    #[error("external session protocol error: {detail}")]
    Protocol {
        /// Stable diagnostic text.
        detail: String,
    },
    /// A policy limit was exceeded (max turns, wall clock, budget).
    #[error("external session limit exceeded: {limit}")]
    LimitExceeded {
        /// Human-readable description of the exceeded limit.
        limit: String,
    },
    /// Resume failed: the session/transcript/resume token is no longer valid.
    #[error("external session cannot be resumed: {detail}")]
    ResumeUnavailable {
        /// Session facts the resume was attempted against.
        session: ExternalSessionRef,
        /// Stable diagnostic text.
        detail: String,
    },
    /// Shutting the session down failed; processes or side effects may remain.
    #[error("external session shutdown failed: {detail}")]
    ShutdownFailed {
        /// Session facts the shutdown was attempted against.
        session: ExternalSessionRef,
        /// Stable diagnostic text.
        detail: String,
    },
    /// The runtime itself reported an error.
    ///
    /// `message` is a fixed, per-runtime diagnostic; the raw runtime-reported
    /// text, when one was captured, is preserved separately in
    /// `runtime_output` so the `Display` rendering can never fold untrusted
    /// output into cursors or logs.
    #[error("external runtime error: {message}")]
    Runtime {
        /// Runtime-specific error code, when one was reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// Stable, fixed diagnostic text (never folds in runtime output).
        message: String,
        /// Raw runtime-reported error text, when one was captured.
        ///
        /// This may contain arbitrary runtime output — including file contents
        /// the model read or tool output it produced — so it must not be
        /// logged or displayed blindly. It is deliberately excluded from the
        /// `Display` rendering (and therefore from any cursor or log built
        /// via `to_string()`); hosts that surface it must treat it as
        /// untrusted content.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runtime_output: Option<String>,
    },
    /// A managed feature was required that the runtime does not support.
    ///
    /// Raised when the machine reaches a decision point the runtime cannot serve
    /// (host-tool injection, resume, subagent bridge, …) so a scheduler fails
    /// loudly instead of degrading silently (design §15). The `detail` is a
    /// stable diagnostic and deliberately carries no raw prompt or tool input.
    #[error("{runtime:?} runtime does not support {capability}: {detail}")]
    UnsupportedCapability {
        /// Runtime that lacks the capability.
        runtime: ExternalRuntimeKind,
        /// The capability that was required but unavailable.
        capability: ExternalCapability,
        /// Stable diagnostic text (never raw prompt/tool input).
        detail: String,
    },
}

#[cfg(test)]
mod tests;
