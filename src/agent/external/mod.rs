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
//! [`ExternalAgentEvent`] values.

use crate::{
    agent::{
        AgentId,
        interaction::{Interaction, InteractionResponse},
        spec::WorktreeRef,
    },
    model::{
        tool::{Tool, ToolStatus},
        usage::Usage,
    },
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod machine;
mod profile;
mod runtime;
mod shutdown;
mod sink;
mod spec;
mod state;

pub use machine::ExternalAgentMachine;
pub use profile::{
    Capability, CostTier, EscalationRules, EscalationTrigger, WorkerProfile, WorkerProfileRef,
    WorkerProfileRegistry,
};
pub use runtime::ExternalRuntimeHandles;
pub use shutdown::ExternalSessionShutdown;
pub use sink::{DiscardEventSink, ExternalEventSink};
pub use spec::ExternalAgentSpec;
pub use state::{ExternalAgentCursor, ExternalAgentState};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSessionPolicy {
    /// How permission-gated actions are handled.
    pub permission_mode: ExternalPermissionMode,
    /// Worktree isolation level for the session.
    pub isolation: WorktreeIsolation,
    /// Optional cap on the number of agent turns for the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// How fine-grained events are surfaced.
    pub stream_events: ExternalStreamPolicy,
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
    pub worktree: WorktreeRef,
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
    MessageSent {
        /// Recipient agent identity.
        to: AgentId,
        /// Short human-readable summary of the message (untrusted).
        summary: String,
    },
    /// A tracked task's status changed.
    TaskUpdated {
        /// Identifier of the task whose status changed.
        task_id: String,
        /// New status label reported by the runtime.
        status: String,
    },
    /// The session finished producing output for this step.
    SessionCompleted,
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
        observations: Vec<ExternalAgentEvent>,
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
        observations: Vec<ExternalAgentEvent>,
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
        observations: Vec<ExternalAgentEvent>,
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
    #[error("external runtime error: {message}")]
    Runtime {
        /// Runtime-specific error code, when one was reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// Runtime-reported error message (untrusted).
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput, ExternalArtifactKind,
        ExternalArtifactRef, ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionInput,
        ExternalSessionPolicy, ExternalSessionRef, ExternalSessionRequest, ExternalSessionResult,
        ExternalStreamPolicy, WorktreeIsolation, collect_file_patch_artifacts,
    };
    use crate::{
        agent::{AgentId, StepId, interaction::Interaction, spec::WorktreeRef},
        model::{tool::Tool, tool::ToolStatus, usage::Usage},
    };
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::json;
    use std::fmt::Debug;

    fn agent_id() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890c1"
            .parse()
            .expect("agent id")
    }

    fn step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890c2"
            .parse()
            .expect("step id")
    }

    fn sample_tool() -> Tool {
        Tool {
            name: "apply_patch".to_owned(),
            description: "Apply a unified diff to the worktree.".to_owned(),
            input_schema: json!({ "type": "object" }),
        }
    }

    fn session_ref() -> ExternalSessionRef {
        ExternalSessionRef {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: Some("sess-42".to_owned()),
            transcript_ref: Some("transcript://42".to_owned()),
            resume_token: Some("resume-token".to_owned()),
            last_event_seq: Some(7),
        }
    }

    fn sample_request() -> ExternalSessionRequest {
        ExternalSessionRequest {
            agent_id: agent_id(),
            runtime: ExternalRuntimeKind::Custom("bespoke-cli".to_owned()),
            worktree: WorktreeRef::new("/repo/agent-lib"),
            session: Some(session_ref()),
            input: ExternalSessionInput::Start {
                prompt: "Refactor the parser.".to_owned(),
            },
            tools: vec![sample_tool()],
            policy: ExternalSessionPolicy {
                permission_mode: ExternalPermissionMode::AcceptEdits,
                isolation: WorktreeIsolation::EphemeralGitWorktree,
                max_turns: Some(16),
                stream_events: ExternalStreamPolicy::Buffered,
            },
        }
    }

    fn sample_observations() -> Vec<ExternalAgentEvent> {
        vec![
            ExternalAgentEvent::SessionStarted {
                session_id: Some("sess-42".to_owned()),
            },
            ExternalAgentEvent::TextDelta {
                text: "working".to_owned(),
            },
            ExternalAgentEvent::CommandFinished {
                exit_code: Some(0),
                stdout_tail: "ok".to_owned(),
                stderr_tail: String::new(),
            },
            ExternalAgentEvent::ToolFinished {
                name: "apply_patch".to_owned(),
                status: ToolStatus::Ok,
            },
            ExternalAgentEvent::MessageSent {
                to: agent_id(),
                summary: "handoff".to_owned(),
            },
            ExternalAgentEvent::SessionCompleted,
        ]
    }

    fn assert_json_round_trip<T>(value: &T)
    where
        T: Debug + PartialEq + Serialize + DeserializeOwned,
    {
        let encoded = serde_json::to_value(value).expect("serialize");
        let decoded: T = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(&decoded, value);
    }

    #[test]
    fn external_dto_roundtrips() {
        let request = sample_request();
        assert_json_round_trip(&request);

        let completed = ExternalSessionResult::Completed {
            session: session_ref(),
            output: ExternalAgentOutput {
                summary: "done".to_owned(),
                artifacts: vec![ExternalArtifactRef {
                    kind: ExternalArtifactKind::Patch,
                    summary: "parser refactor".to_owned(),
                    path: Some("src/parser.rs".to_owned()),
                    reference: Some("blob://abc".to_owned()),
                }],
                usage: Some(Usage {
                    input: 100,
                    output: 40,
                    ..Usage::default()
                }),
                cost_micros: Some(1_250),
            },
            observations: sample_observations(),
        };
        assert_json_round_trip(&completed);

        let paused = ExternalSessionResult::PausedForInteraction {
            session: session_ref(),
            action_id: "act-1".to_owned(),
            request: Interaction::question(step_id(), "Delete build/ ?".to_owned()),
            observations: vec![ExternalAgentEvent::PermissionRequested {
                action_id: "act-1".to_owned(),
                summary: "remove build/".to_owned(),
            }],
        };
        assert_json_round_trip(&paused);

        let failed = ExternalSessionResult::Failed {
            session: Some(session_ref()),
            error: ExternalAgentError::ShutdownFailed {
                session: session_ref(),
                detail: "child process would not exit".to_owned(),
            },
            observations: Vec::new(),
        };
        assert_json_round_trip(&failed);
    }

    #[test]
    fn external_session_result_variants_serialize_snake_case() {
        let completed = ExternalSessionResult::Completed {
            session: session_ref(),
            output: ExternalAgentOutput {
                summary: "done".to_owned(),
                artifacts: Vec::new(),
                usage: None,
                cost_micros: None,
            },
            observations: Vec::new(),
        };
        let encoded = serde_json::to_value(&completed).expect("serialize");
        assert!(encoded.get("completed").is_some());

        let launch = ExternalAgentError::Launch {
            runtime: ExternalRuntimeKind::Codex,
            detail: "binary missing".to_owned(),
        };
        let encoded = serde_json::to_value(&launch).expect("serialize error");
        assert!(encoded.get("launch").is_some());
    }

    #[test]
    fn file_patch_event_maps_to_patch_artifact_ref() {
        let event = ExternalAgentEvent::FilePatch {
            path: "src/parser.rs".to_owned(),
            summary: "tighten error recovery".to_owned(),
            diff_ref: Some("blob://diff-1".to_owned()),
        };
        let artifact = ExternalArtifactRef::from_file_patch(&event).expect("FilePatch maps");
        assert_eq!(
            artifact,
            ExternalArtifactRef {
                kind: ExternalArtifactKind::Patch,
                summary: "tighten error recovery".to_owned(),
                path: Some("src/parser.rs".to_owned()),
                reference: Some("blob://diff-1".to_owned()),
            }
        );

        // A FilePatch without a stored diff still maps, leaving `reference` empty.
        let no_ref = ExternalAgentEvent::FilePatch {
            path: "README.md".to_owned(),
            summary: "note".to_owned(),
            diff_ref: None,
        };
        let artifact = ExternalArtifactRef::from_file_patch(&no_ref).expect("FilePatch maps");
        assert_eq!(artifact.reference, None);
        assert_eq!(artifact.path.as_deref(), Some("README.md"));

        // Non-FilePatch events do not map.
        assert!(
            ExternalArtifactRef::from_file_patch(&ExternalAgentEvent::SessionCompleted).is_none()
        );
    }

    #[test]
    fn collect_file_patch_artifacts_keeps_only_patches_in_order() {
        let events = vec![
            ExternalAgentEvent::SessionStarted { session_id: None },
            ExternalAgentEvent::FilePatch {
                path: "a.rs".to_owned(),
                summary: "first".to_owned(),
                diff_ref: Some("blob://a".to_owned()),
            },
            ExternalAgentEvent::TextDelta {
                text: "chatter".to_owned(),
            },
            ExternalAgentEvent::FilePatch {
                path: "b.rs".to_owned(),
                summary: "second".to_owned(),
                diff_ref: None,
            },
            ExternalAgentEvent::SessionCompleted,
        ];
        let artifacts = collect_file_patch_artifacts(&events);
        assert_eq!(
            artifacts,
            vec![
                ExternalArtifactRef {
                    kind: ExternalArtifactKind::Patch,
                    summary: "first".to_owned(),
                    path: Some("a.rs".to_owned()),
                    reference: Some("blob://a".to_owned()),
                },
                ExternalArtifactRef {
                    kind: ExternalArtifactKind::Patch,
                    summary: "second".to_owned(),
                    path: Some("b.rs".to_owned()),
                    reference: None,
                },
            ]
        );

        assert!(collect_file_patch_artifacts(&[]).is_empty());
    }
}
