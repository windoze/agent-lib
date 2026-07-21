//! Live OpenCode runtime session and adapter (M8-3, feature `external-opencode`).
//!
//! M8-1 froze the [`OpenCodeConfig`](super::OpenCodeConfig) launch recipe
//! (including [`base_run_args`](super::OpenCodeConfig::base_run_args) /
//! [`base_resume_args`](super::OpenCodeConfig::base_resume_args)) and the
//! capability [`probe`](super::probe); M8-2 froze the private
//! [`OpenCodeStreamDecoder`](super::OpenCodeStreamDecoder) that turns raw
//! `opencode run --format json` frames into sequenced observations and per-turn
//! decisions. This module wires those together into the milestone-5 abstraction
//! (design §11, §12, §14):
//!
//! - [`OpenCodeAdapter`] is the per-runtime factory ([`ExternalRuntimeAdapter`]).
//!   It reports the managed capabilities its sessions can actually fulfill,
//!   [`start`](ExternalRuntimeAdapter::start)s a fresh `opencode run` session, and
//!   [`resume`](ExternalRuntimeAdapter::resume)s a prior one from its
//!   runtime-assigned session id.
//! - `OpenCodeSession` (private) is one live session
//!   ([`ExternalRuntimeSession`](crate::agent::external::ExternalRuntimeSession))
//!   — the shared one-shot state machine of
//!   [`process::oneshot`](crate::agent::external::process::oneshot) bound to the
//!   OpenCode decoder, runtime kind, and message wording. It owns a per-turn CLI
//!   process, feeds each stdout line to the decoder, mirrors observations to the
//!   live sink, and advances to the next
//!   [`RuntimeDecisionPoint`](crate::agent::external::RuntimeDecisionPoint).
//!
//! # One process per turn
//!
//! Like `codex exec`, `opencode run` is **one-shot per turn**: the prompt is a
//! CLI positional argument (not a stdin frame), and the process exits when the
//! turn settles. A follow-up turn is a brand-new `opencode run --session
//! <session_id> <message>` process. The session therefore spawns a fresh process
//! for the first turn (in `begin`) and another for every follow-up turn (in
//! `advance`), threading a single decoder — whose `seq` spans the whole session
//! (design §5.5) — across all of them. Codex's `codex exec` has the same shape,
//! so the state machine is single-sourced in
//! [`process::oneshot`](crate::agent::external::process::oneshot); this module
//! keeps only the OpenCode bindings (argv mapping, decoder delegation, message
//! wording) and the public adapter.
//!
//! Unlike Codex's `thread.started` prelude frame, OpenCode has **no dedicated
//! init frame**: the runtime session id rides on `sessionID` of every mirrored
//! event, so `begin` reads frames until the decoder lazily captures the first one
//! (which also emits the single `SessionStarted` observation), buffering those
//! prelude observations for the first `advance` to replay.
//!
//! # Host tools & approvals
//!
//! `opencode run --format json` runs **autonomously**: it resolves permission
//! prompts against the `--auto` launch flag the host pre-set on the command line
//! and executes its own tools, so the JSON stream carries no host-pausable
//! tool-call or approval frame (M8-2). This adapter honestly reports
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
//! The session drives its per-turn process through the private
//! [`OpenCodeLauncher`] / [`OpenCodeTurnStream`] traits (the shared
//! [`oneshot::Launcher`](crate::agent::external::process::oneshot) transport
//! traits under their OpenCode names), not a `tokio::process::Child` directly.
//! Production uses [`SystemOpenCodeLauncher`], which spawns the real CLI; the
//! unit tests inject a fake launcher that replays canned JSON lines and captures
//! the [`OpenCodeTurnSpec`] of every turn, so the whole
//! start/advance/resume/shutdown state machine is exercised with no OpenCode
//! binary and no network. The real end-to-end coverage lives behind an
//! `#[ignore]` in `tests/external_opencode.rs`.

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
    self, FirstLaunch, Launcher as OpenCodeLauncher, TurnSpec as OpenCodeTurnSpec,
    TurnStream as OpenCodeTurnStream,
};
use crate::agent::external::process::{self, ChildStdinMode, ManagedChild};
use crate::agent::external::{
    ExternalAgentError, ExternalAgentOutput, ExternalEventSink, ExternalObservedEvent,
    ExternalRuntimeAdapter, ExternalRuntimeCapabilities, ExternalRuntimeKind,
    ExternalRuntimeSession, ExternalSessionInput, ExternalSessionRef, ExternalSessionRequest,
};

use super::{OpenCodeConfig, OpenCodeDecision, OpenCodeDecodeContext, OpenCodeStreamDecoder};

/// Maps the shared one-shot [`OpenCodeTurnSpec`] onto [`OpenCodeConfig`]'s frozen
/// base arguments: [`base_run_args`](OpenCodeConfig::base_run_args) for a fresh
/// turn, [`base_resume_args`](OpenCodeConfig::base_resume_args) for a resumed
/// one.
impl oneshot::TurnArgs for OpenCodeConfig {
    fn base_turn_args(&self) -> Vec<String> {
        self.base_run_args()
    }

    fn base_resume_args(&self, session_id: &str) -> Vec<String> {
        self.base_resume_args(session_id)
    }
}

/// Binds the shared one-shot state machine to the OpenCode wire decoder,
/// delegating to [`OpenCodeStreamDecoder`]'s inherent API.
impl oneshot::Decoder for OpenCodeStreamDecoder {
    type Context = OpenCodeDecodeContext;
    type Decision = OpenCodeDecision;

    fn new(context: OpenCodeDecodeContext) -> Self {
        OpenCodeStreamDecoder::new(context)
    }

    fn with_next_seq(self, next_seq: u64) -> Self {
        OpenCodeStreamDecoder::with_next_seq(self, next_seq)
    }

    fn session_id(&self) -> Option<&str> {
        OpenCodeStreamDecoder::session_id(self)
    }

    fn push_line(&mut self, line: &str) -> Result<Option<OpenCodeDecision>, ExternalAgentError> {
        OpenCodeStreamDecoder::push_line(self, line)
    }

    fn take_observations(&mut self) -> Vec<ExternalObservedEvent> {
        OpenCodeStreamDecoder::take_observations(self)
    }

    fn decision_result(
        decision: OpenCodeDecision,
    ) -> Result<ExternalAgentOutput, ExternalAgentError> {
        match decision {
            OpenCodeDecision::Completed { output } => Ok(output),
            OpenCodeDecision::Failed { error } => Err(error),
        }
    }
}

/// The OpenCode wording of the shared one-shot state machine's messages.
static LABELS: oneshot::Labels = oneshot::Labels {
    turn: process::AutonomousTurnMessages {
        interaction: "opencode run executes autonomously; there is no host-answerable interaction to resolve",
        tool_results: "opencode adapter does not bridge host tool results",
        subagent: "opencode adapter does not bridge host subagents",
        shutdown: "opencode session shutdown must go through shutdown(), not advance",
    },
    spawn_failed: "spawning opencode run failed",
    resume_spawn_failed: "failed spawning opencode run --session",
    id_timeout: "opencode run did not report a session id within the launch timeout",
    resume_id_timeout: "resumed opencode run did not report a session id within the launch timeout",
    begin_cancelled: "opencode session begin was cancelled",
    id_missing: "opencode run did not report a session id",
    no_open_turn: "opencode session has no open turn to read",
    read_failed: "failed reading opencode run stream",
    advance_cancelled: "opencode session advance was cancelled",
    closed_before_decision: "opencode session closed before reaching a decision point",
};

/// The OpenCode binding of the shared one-shot session: decoder, runtime kind,
/// and message wording.
struct OpenCodeFlavor;

impl oneshot::Flavor for OpenCodeFlavor {
    type Decoder = OpenCodeStreamDecoder;

    fn runtime() -> ExternalRuntimeKind {
        ExternalRuntimeKind::OpenCode
    }

    fn labels() -> &'static oneshot::Labels {
        &LABELS
    }
}

/// One live OpenCode session: the shared [`oneshot::Session`] state machine
/// bound to the OpenCode decoder, runtime kind, and message wording.
///
/// The session owns a per-turn CLI process and a single
/// [`OpenCodeStreamDecoder`] whose `seq` line spans the whole session (design
/// §5.5). The first turn is launched by `begin` (which reads until the decoder
/// captures the runtime session id from the first `sessionID`-bearing frame);
/// each follow-up [`advance`](ExternalRuntimeSession::advance) launches a fresh
/// `opencode run --session` process, feeds its stdout to the decoder, mirrors
/// observations to the live sink, and returns at the decision the turn settles
/// on.
type OpenCodeSession<L> = oneshot::Session<OpenCodeFlavor, L>;

/// Production [`OpenCodeLauncher`] spawning the real OpenCode CLI per turn.
struct SystemOpenCodeLauncher {
    config: OpenCodeConfig,
}

impl SystemOpenCodeLauncher {
    /// Builds a launcher that spawns `config`'s binary for each turn.
    fn new(config: OpenCodeConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl OpenCodeLauncher for SystemOpenCodeLauncher {
    async fn launch(&self, spec: &OpenCodeTurnSpec) -> io::Result<Box<dyn OpenCodeTurnStream>> {
        let args = spec.args(&self.config);

        let mut command = Command::new(self.config.binary());
        command.args(&args);
        if let Some(dir) = self.config.working_dir() {
            // OpenCode resolves file operations from the `--dir <path>` flag that
            // `base_run_args`/`base_resume_args` already emit (falling back to the
            // inherited `$PWD`), not from the child's OS-level cwd alone. Setting
            // `current_dir` here is a belt-and-suspenders measure so the process's
            // cwd and `--dir` agree; it is not sufficient on its own for worktree
            // isolation, which is why the directory is also passed as `--dir`.
            command.current_dir(dir);
        }
        for (key, value) in self.config.env() {
            command.env(key, value);
        }
        Ok(Box::new(ManagedChild::spawn(
            command,
            // OpenCode reads prompt text from its positional argument; a piped or
            // inherited stdin would make `run` block reading a message from stdin.
            ChildStdinMode::Null,
            self.config.read_idle_timeout(),
            self.config.shutdown_grace(),
            "opencode run stdout was not captured",
            "opencode run read timed out",
        )?))
    }
}

/// Managed adapter that starts and resumes live OpenCode CLI sessions.
///
/// Construct one from an [`OpenCodeConfig`] with [`new`](Self::new) (assuming a
/// fully capable CLI) or
/// [`with_probed_capabilities`](Self::with_probed_capabilities) to intersect the
/// adapter's implemented features with what a [`probe`](super::probe) confirmed on
/// the local binary. Wrap the adapter in an
/// [`ExternalSessionRegistry`](crate::agent::external::ExternalSessionRegistry) to
/// own its live sessions between decision points.
pub struct OpenCodeAdapter {
    config: OpenCodeConfig,
    capabilities: ExternalRuntimeCapabilities,
}

impl OpenCodeAdapter {
    /// Builds an adapter for `config` reporting every managed feature this
    /// adapter implements.
    ///
    /// The reported set is fixed: streaming, resume, artifacts, usage, and
    /// graceful shutdown are on; host-tool, host-subagent, and permission
    /// bridging are off because `opencode run --format json` runs autonomously
    /// and never hands a tool call or an approval back to the host (design §14,
    /// M8-2). Prefer
    /// [`with_probed_capabilities`](Self::with_probed_capabilities) when a probe
    /// has confirmed which features the local binary actually advertises.
    #[must_use]
    pub fn new(config: OpenCodeConfig) -> Self {
        Self {
            config,
            capabilities: implemented_capabilities(),
        }
    }

    /// Builds an adapter whose reported capabilities are the intersection of what
    /// this adapter implements and what a probe found on the local CLI.
    ///
    /// A feature is reported supported only when *both* the adapter implements it
    /// and the probe advertised it, so a binary lacking the resumable `run
    /// --session` shape disables resume while host-tool bridging stays off
    /// regardless of the probe (this adapter never serves it).
    #[must_use]
    pub fn with_probed_capabilities(
        config: OpenCodeConfig,
        probed: &ExternalRuntimeCapabilities,
    ) -> Self {
        Self {
            config,
            capabilities: process::intersect_capabilities(&implemented_capabilities(), probed),
        }
    }

    /// Returns the launch configuration backing this adapter.
    #[must_use]
    pub const fn config(&self) -> &OpenCodeConfig {
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
            "opencode adapter cannot inject host tools; opencode run executes autonomously",
        )
    }

    /// Builds the decode context stamping the worktree onto command observations.
    ///
    /// OpenCode reports a `bash` tool part without the directory it ran in, so the
    /// host threads in the directory it launched `opencode run` under — preferring
    /// the config's explicit working directory, falling back to the request's
    /// worktree. It is never taken from model output.
    fn decode_context(
        config: &OpenCodeConfig,
        request: &ExternalSessionRequest,
    ) -> OpenCodeDecodeContext {
        let cwd = config
            .working_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_else(|| request.worktree.path().to_string_lossy().into_owned());
        OpenCodeDecodeContext::new().with_cwd(cwd)
    }

    /// Resolves the effective session configuration for `request`.
    ///
    /// Request-level policy wins over the construction-time config (M2-7 /
    /// M-PROM-5): [`ExternalSessionPolicy::permission_mode`] overrides
    /// [`with_permission_mode`](OpenCodeConfig::with_permission_mode), and a
    /// prepared [`session_dir`](ExternalSessionRequest::session_dir) overrides
    /// [`with_working_dir`](OpenCodeConfig::with_working_dir) — which is what
    /// [`base_run_args`](OpenCodeConfig::base_run_args) emits as `--dir`, so a
    /// prepared worktree flows through to the flag OpenCode actually resolves
    /// file operations from. The stored config remains the fallback for
    /// request-less operations (the capability probe).
    fn session_config(&self, request: &ExternalSessionRequest) -> OpenCodeConfig {
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
impl ExternalRuntimeAdapter for OpenCodeAdapter {
    fn kind(&self) -> ExternalRuntimeKind {
        ExternalRuntimeKind::OpenCode
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
                detail: "a fresh opencode session must start with a prompt".to_owned(),
            });
        };
        let spec = OpenCodeTurnSpec::Fresh {
            prompt: prompt.clone(),
        };

        let config = self.session_config(request);
        let launcher = SystemOpenCodeLauncher::new(config.clone());
        let mut session = OpenCodeSession::new(
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
                detail: "opencode session has no session id to resume".to_owned(),
            });
        };
        let message = turn_message(&self.capabilities, &request.input)?;
        let spec = OpenCodeTurnSpec::Resume {
            session_id,
            message,
        };

        let config = self.session_config(request);
        let launcher = SystemOpenCodeLauncher::new(config.clone());
        let mut live = OpenCodeSession::new(
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
/// host-bridge responses are refused because `opencode run --format json` runs
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
/// Host-tool, host-subagent, and permission bridging are off because `opencode
/// run --format json` runs autonomously and never hands a tool call or an
/// approval back to the host (M8-2); the rest are on because the structured
/// stream, the `run --session` shape, file-change events, turn usage, and a clean
/// process close back them.
fn implemented_capabilities() -> ExternalRuntimeCapabilities {
    ExternalRuntimeCapabilities {
        runtime: ExternalRuntimeKind::OpenCode,
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
