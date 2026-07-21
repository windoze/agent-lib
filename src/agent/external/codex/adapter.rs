//! Live Codex runtime session and adapter (M7-3, feature `external-codex`).
//!
//! M7-1 froze the [`CodexConfig`](super::CodexConfig) launch recipe (including
//! [`base_exec_args`](super::CodexConfig::base_exec_args) /
//! [`base_resume_args`](super::CodexConfig::base_resume_args)) and the capability
//! [`probe`](super::probe); M7-2 froze the private
//! [`CodexStreamDecoder`](super::CodexStreamDecoder) that turns raw `codex exec
//! --json` frames into sequenced observations and per-turn decisions. This module
//! wires those together into the milestone-5 abstraction (design §11, §12):
//!
//! - [`CodexAdapter`] is the per-runtime factory ([`ExternalRuntimeAdapter`]). It
//!   reports the managed capabilities its sessions can actually fulfill,
//!   [`start`](ExternalRuntimeAdapter::start)s a fresh `codex exec` session, and
//!   [`resume`](ExternalRuntimeAdapter::resume)s a prior one from its
//!   runtime-assigned thread id.
//! - `CodexSession` (private) is one live session
//!   ([`ExternalRuntimeSession`](crate::agent::external::ExternalRuntimeSession))
//!   — the shared one-shot state machine of
//!   [`process::oneshot`](crate::agent::external::process::oneshot) bound to the
//!   Codex decoder, runtime kind, and message wording. It owns a per-turn CLI
//!   process, feeds each stdout line to the decoder, mirrors observations to the
//!   live sink, and advances to the next
//!   [`RuntimeDecisionPoint`](crate::agent::external::RuntimeDecisionPoint).
//!
//! # One process per turn
//!
//! Unlike Claude Code's single long-lived `stream-json` process, `codex exec` is
//! **one-shot per turn**: the prompt is a CLI positional argument (not a stdin
//! frame), and the process exits when the turn settles. A follow-up turn is a
//! brand-new `codex exec resume <thread_id> <message>` process. The session
//! therefore spawns a fresh process for the first turn (in `begin`) and another
//! for every follow-up turn (in `advance`), threading a single decoder — whose
//! `seq` spans the whole session (design §5.5) — across all of them. OpenCode's
//! `opencode run` has the same shape, so the state machine is single-sourced in
//! [`process::oneshot`](crate::agent::external::process::oneshot); this module
//! keeps only the Codex bindings (argv mapping, decoder delegation, message
//! wording) and the public adapter.
//!
//! # Host tools & approvals
//!
//! `codex exec --json` runs **autonomously**: it resolves approvals against the
//! sandbox/approval policy the host pre-set on the command line and executes its
//! own tools, so the JSONL stream carries no host-pausable tool-call or approval
//! frame (M7-2). This adapter honestly reports
//! [`host_tools`](ExternalRuntimeCapabilities::host_tools),
//! [`host_subagents`](ExternalRuntimeCapabilities::host_subagents), and
//! [`permission_bridge`](ExternalRuntimeCapabilities::permission_bridge) as
//! `false`; it refuses a [`start`](ExternalRuntimeAdapter::start) that declares
//! host tools and rejects a
//! [`RespondToolResults`](ExternalSessionInput::RespondToolResults) /
//! [`RespondSubagent`](ExternalSessionInput::RespondSubagent) /
//! [`RespondInteraction`](ExternalSessionInput::RespondInteraction) follow-up with
//! a classified [`UnsupportedCapability`](ExternalAgentError::UnsupportedCapability)
//! rather than silently ignoring it. Streaming, resume, artifacts, usage, and
//! graceful shutdown are supported.
//!
//! # Offline testability
//!
//! The session drives its per-turn process through the private [`CodexLauncher`] /
//! [`CodexTurnStream`] traits (the shared
//! [`oneshot::Launcher`](crate::agent::external::process::oneshot) transport
//! traits under their Codex names), not a `tokio::process::Child` directly.
//! Production uses [`SystemCodexLauncher`], which spawns the real CLI; the unit
//! tests inject a fake launcher that replays canned JSONL lines and captures the
//! [`CodexTurnSpec`] of every turn, so the whole start/advance/resume/shutdown
//! state machine is exercised with no Codex binary and no network. The real
//! end-to-end coverage lives behind an `#[ignore]` in `tests/external_codex.rs`.

// The session's fallible helpers return the external adapter's canonical
// `ExternalAgentError`, matching the unboxed error contract used across
// `adapter.rs`, `registry.rs`, `probe.rs`, and `decoder.rs`. That enum is
// intentionally not boxed, so `result_large_err` (which only fires because some
// helpers have small `Ok` types) would force a signature style inconsistent with
// the rest of the module.
#![allow(clippy::result_large_err)]

use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::Command;

use crate::agent::RunContext;

use crate::agent::external::process::oneshot::{
    self, FirstLaunch, Launcher as CodexLauncher, TurnSpec as CodexTurnSpec,
    TurnStream as CodexTurnStream,
};
use crate::agent::external::process::{self, ChildStdinMode, ManagedChild};
use crate::agent::external::{
    ExternalAgentError, ExternalAgentOutput, ExternalEventSink, ExternalObservedEvent,
    ExternalRuntimeAdapter, ExternalRuntimeCapabilities, ExternalRuntimeKind,
    ExternalRuntimeSession, ExternalSessionInput, ExternalSessionRef, ExternalSessionRequest,
};

use super::{CodexConfig, CodexDecision, CodexDecodeContext, CodexStreamDecoder};

/// Maps the shared one-shot [`CodexTurnSpec`] onto [`CodexConfig`]'s frozen base
/// arguments: [`base_exec_args`](CodexConfig::base_exec_args) for a fresh turn,
/// [`base_resume_args`](CodexConfig::base_resume_args) for a resumed one.
impl oneshot::TurnArgs for CodexConfig {
    fn base_turn_args(&self) -> Vec<String> {
        self.base_exec_args()
    }

    fn base_resume_args(&self, session_id: &str) -> Vec<String> {
        self.base_resume_args(session_id)
    }
}

/// Binds the shared one-shot state machine to the Codex wire decoder, delegating
/// to [`CodexStreamDecoder`]'s inherent API.
impl oneshot::Decoder for CodexStreamDecoder {
    type Context = CodexDecodeContext;
    type Decision = CodexDecision;

    fn new(context: CodexDecodeContext) -> Self {
        CodexStreamDecoder::new(context)
    }

    fn with_next_seq(self, next_seq: u64) -> Self {
        CodexStreamDecoder::with_next_seq(self, next_seq)
    }

    fn session_id(&self) -> Option<&str> {
        CodexStreamDecoder::session_id(self)
    }

    fn push_line(&mut self, line: &str) -> Result<Option<CodexDecision>, ExternalAgentError> {
        CodexStreamDecoder::push_line(self, line)
    }

    fn take_observations(&mut self) -> Vec<ExternalObservedEvent> {
        CodexStreamDecoder::take_observations(self)
    }

    fn decision_result(decision: CodexDecision) -> Result<ExternalAgentOutput, ExternalAgentError> {
        match decision {
            CodexDecision::Completed { output } => Ok(output),
            CodexDecision::Failed { error } => Err(error),
        }
    }
}

/// The Codex wording of the shared one-shot state machine's messages.
static LABELS: oneshot::Labels = oneshot::Labels {
    turn: process::AutonomousTurnMessages {
        interaction: "codex exec runs autonomously; there is no host-answerable interaction to resolve",
        tool_results: "codex adapter does not bridge host tool results",
        subagent: "codex adapter does not bridge host subagents",
        shutdown: "codex session shutdown must go through shutdown(), not advance",
    },
    spawn_failed: "spawning codex exec failed",
    resume_spawn_failed: "failed spawning codex exec resume",
    id_timeout: "codex exec did not report a thread id within the launch timeout",
    resume_id_timeout: "resumed codex exec did not report a thread id within the launch timeout",
    begin_cancelled: "codex session begin was cancelled",
    id_missing: "codex exec did not report a thread id",
    no_open_turn: "codex session has no open turn to read",
    read_failed: "failed reading codex exec stream",
    advance_cancelled: "codex session advance was cancelled",
    closed_before_decision: "codex session closed before reaching a decision point",
};

/// The Codex binding of the shared one-shot session: decoder, runtime kind, and
/// message wording.
struct CodexFlavor;

impl oneshot::Flavor for CodexFlavor {
    type Decoder = CodexStreamDecoder;

    fn runtime() -> ExternalRuntimeKind {
        ExternalRuntimeKind::Codex
    }

    fn labels() -> &'static oneshot::Labels {
        &LABELS
    }
}

/// One live Codex session: the shared [`oneshot::Session`] state machine bound
/// to the Codex decoder, runtime kind, and message wording.
///
/// The session owns a per-turn CLI process and a single [`CodexStreamDecoder`]
/// whose `seq` line spans the whole session (design §5.5). The first turn is
/// launched by `begin` (which reads up to the `thread.started` frame to learn
/// the runtime thread id); each follow-up
/// [`advance`](ExternalRuntimeSession::advance) launches a fresh `codex exec
/// resume` process, feeds its stdout to the decoder, mirrors observations to the
/// live sink, and returns at the decision the turn settles on.
type CodexSession<L> = oneshot::Session<CodexFlavor, L>;

/// Production [`CodexLauncher`] spawning the real Codex CLI per turn.
struct SystemCodexLauncher {
    config: CodexConfig,
}

impl SystemCodexLauncher {
    /// Builds a launcher that spawns `config`'s binary for each turn.
    fn new(config: CodexConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl CodexLauncher for SystemCodexLauncher {
    async fn launch(&self, spec: &CodexTurnSpec) -> io::Result<Box<dyn CodexTurnStream>> {
        let args = spec.args(&self.config);

        let mut command = Command::new(self.config.binary());
        command.args(&args);
        if let Some(dir) = self.config.working_dir() {
            command.current_dir(dir);
        }
        for (key, value) in self.config.env() {
            command.env(key, value);
        }
        Ok(Box::new(ManagedChild::spawn(
            command,
            // Codex reads prompt text only from its positional argument; a piped
            // or inherited stdin makes it block on additional input.
            ChildStdinMode::Null,
            self.config.read_idle_timeout(),
            self.config.shutdown_grace(),
            "codex exec stdout was not captured",
            "codex exec read timed out",
        )?))
    }
}

/// Managed adapter that starts and resumes live Codex CLI sessions.
///
/// Construct one from a [`CodexConfig`] with [`new`](Self::new) (assuming a fully
/// capable CLI) or [`with_probed_capabilities`](Self::with_probed_capabilities)
/// to intersect the adapter's implemented features with what a
/// [`probe`](super::probe) confirmed on the local binary. Wrap the adapter in an
/// [`ExternalSessionRegistry`](crate::agent::external::ExternalSessionRegistry) to
/// own its live sessions between decision points.
pub struct CodexAdapter {
    config: CodexConfig,
    capabilities: ExternalRuntimeCapabilities,
}

impl CodexAdapter {
    /// Builds an adapter for `config` reporting every managed feature this
    /// adapter implements.
    ///
    /// The reported set is fixed: streaming, resume, artifacts, usage, and
    /// graceful shutdown are on; host-tool, host-subagent, and permission
    /// bridging are off because `codex exec --json` runs autonomously and never
    /// hands a tool call or an approval back to the host (design §12, M7-2).
    /// Prefer [`with_probed_capabilities`](Self::with_probed_capabilities) when a
    /// probe has confirmed which features the local binary actually advertises.
    #[must_use]
    pub fn new(config: CodexConfig) -> Self {
        Self {
            config,
            capabilities: implemented_capabilities(),
        }
    }

    /// Builds an adapter whose reported capabilities are the intersection of what
    /// this adapter implements and what a probe found on the local CLI.
    ///
    /// A feature is reported supported only when *both* the adapter implements it
    /// and the probe advertised it, so a binary lacking the resumable `exec
    /// resume` shape disables resume while host-tool bridging stays off regardless
    /// of the probe (this adapter never serves it).
    #[must_use]
    pub fn with_probed_capabilities(
        config: CodexConfig,
        probed: &ExternalRuntimeCapabilities,
    ) -> Self {
        Self {
            config,
            capabilities: process::intersect_capabilities(&implemented_capabilities(), probed),
        }
    }

    /// Returns the launch configuration backing this adapter.
    #[must_use]
    pub const fn config(&self) -> &CodexConfig {
        &self.config
    }

    /// Refuses a request that declares host tools this adapter cannot inject.
    fn reject_unsupported_tools(
        &self,
        request: &ExternalSessionRequest,
    ) -> Result<(), ExternalAgentError> {
        process::reject_unsupported_tools(
            &self.capabilities,
            request,
            "codex adapter cannot inject host tools; codex exec runs autonomously",
        )
    }

    /// Builds the decode context stamping the worktree onto command observations.
    ///
    /// Codex reports a `command_execution` item without the directory it ran in,
    /// so the host threads in the directory it launched `codex exec` under —
    /// preferring the config's explicit working directory, falling back to the
    /// request's worktree. It is never taken from model output.
    fn decode_context(
        config: &CodexConfig,
        request: &ExternalSessionRequest,
    ) -> CodexDecodeContext {
        let cwd = config
            .working_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_else(|| request.worktree.path().to_string_lossy().into_owned());
        CodexDecodeContext::new().with_cwd(cwd)
    }

    /// Resolves the effective session configuration for `request`.
    ///
    /// Request-level policy wins over the construction-time config (M2-7 /
    /// M-PROM-5): [`ExternalSessionPolicy::permission_mode`] overrides
    /// [`with_permission_mode`](CodexConfig::with_permission_mode), and a
    /// prepared [`session_dir`](ExternalSessionRequest::session_dir) overrides
    /// [`with_working_dir`](CodexConfig::with_working_dir). The stored config
    /// remains the fallback for request-less operations (the capability probe).
    fn session_config(&self, request: &ExternalSessionRequest) -> CodexConfig {
        let mut config = self
            .config
            .clone()
            .with_permission_mode(request.policy.permission_mode);
        if let Some(dir) = &request.session_dir {
            config = config.with_working_dir(dir.path().to_path_buf());
        }
        config
    }
}

#[async_trait]
impl ExternalRuntimeAdapter for CodexAdapter {
    fn kind(&self) -> ExternalRuntimeKind {
        ExternalRuntimeKind::Codex
    }

    fn capabilities(&self) -> ExternalRuntimeCapabilities {
        self.capabilities.clone()
    }

    async fn start(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
        self.reject_unsupported_tools(request)?;

        let ExternalSessionInput::Start { prompt } = &request.input else {
            return Err(ExternalAgentError::Protocol {
                detail: "a fresh codex session must start with a prompt".to_owned(),
            });
        };
        let spec = CodexTurnSpec::Fresh {
            prompt: prompt.clone(),
        };

        let config = self.session_config(request);
        let launcher = SystemCodexLauncher::new(config.clone());
        let mut session = CodexSession::new(
            launcher,
            Self::decode_context(&config, request),
            sink,
            self.capabilities.clone(),
        );
        session
            .begin(&spec, FirstLaunch::Fresh, ctx, config.timeout())
            .await?;
        Ok(Box::new(session))
    }

    async fn resume(
        &self,
        session: &ExternalSessionRef,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
        self.reject_unsupported_tools(request)?;

        let Some(session_id) = session.session_id.clone() else {
            return Err(ExternalAgentError::ResumeUnavailable {
                session: session.clone(),
                detail: "codex session has no thread id to resume".to_owned(),
            });
        };
        let message = turn_message(&self.capabilities, &request.input)?;
        let spec = CodexTurnSpec::Resume {
            session_id,
            message,
        };

        let config = self.session_config(request);
        let launcher = SystemCodexLauncher::new(config.clone());
        let mut live = CodexSession::new(
            launcher,
            Self::decode_context(&config, request),
            sink,
            self.capabilities.clone(),
        )
        .with_resume_high_water(session.last_event_seq);
        live.begin(
            &spec,
            FirstLaunch::Resume(session.clone()),
            ctx,
            config.timeout(),
        )
        .await?;
        Ok(Box::new(live))
    }
}

/// Extracts the user-visible turn text from `input`, or refuses an input this
/// adapter cannot serve.
///
/// A [`Start`](ExternalSessionInput::Start) prompt and a
/// [`Continue`](ExternalSessionInput::Continue) message become turn text; the
/// host-bridge responses are refused because `codex exec --json` runs
/// autonomously and never pauses for them, and a
/// [`Shutdown`](ExternalSessionInput::Shutdown) is a protocol error here because
/// the graceful stop path closes the session through
/// [`shutdown`](ExternalRuntimeSession::shutdown), not `advance`.
///
/// # Errors
///
/// Returns [`UnsupportedCapability`](ExternalAgentError::UnsupportedCapability)
/// for a tool/subagent/interaction response, or
/// [`Protocol`](ExternalAgentError::Protocol) for a misrouted shutdown.
fn turn_message(
    capabilities: &ExternalRuntimeCapabilities,
    input: &ExternalSessionInput,
) -> Result<String, ExternalAgentError> {
    process::autonomous_turn_message(capabilities, input, LABELS.turn)
}

/// Returns the managed features this adapter can actually fulfill.
///
/// Host-tool, host-subagent, and permission bridging are off because `codex exec
/// --json` runs autonomously and never hands a tool call or an approval back to
/// the host (M7-2); the rest are on because the structured stream, the `exec
/// resume` shape, file-change items, turn usage, and a clean process close back
/// them.
fn implemented_capabilities() -> ExternalRuntimeCapabilities {
    ExternalRuntimeCapabilities {
        runtime: ExternalRuntimeKind::Codex,
        streaming: true,
        resume: true,
        permission_bridge: false,
        host_tools: false,
        host_subagents: false,
        artifacts: true,
        usage: true,
        graceful_shutdown: true,
        reconfigure: false,
    }
}

#[cfg(test)]
mod tests;
