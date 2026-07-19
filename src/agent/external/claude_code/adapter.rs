//! Live Claude Code runtime session and adapter (M6-3, feature
//! `external-claude-code`).
//!
//! M6-1 froze the [`ClaudeCodeConfig`](super::ClaudeCodeConfig) launch recipe and
//! the capability [`probe`](super::probe); M6-2 froze the private
//! [`ClaudeStreamDecoder`](super::ClaudeStreamDecoder) that turns raw
//! `stream-json` frames into sequenced observations and per-turn decisions. This
//! module wires those together into the two live-IO traits the milestone-5
//! abstraction defines (design §11, §12):
//!
//! - [`ClaudeCodeAdapter`] is the per-runtime factory
//!   ([`ExternalRuntimeAdapter`]). It reports the managed capabilities its
//!   sessions can actually fulfill, [`start`](ExternalRuntimeAdapter::start)s a
//!   fresh CLI session, and [`resume`](ExternalRuntimeAdapter::resume)s a prior
//!   one from its runtime-assigned id.
//! - `ClaudeCodeSession` (private) is one live session
//!   ([`ExternalRuntimeSession`]). It owns the CLI child process, writes host
//!   turns to its stdin as `stream-json` frames, feeds each stdout line to the
//!   decoder, mirrors observations to the live sink, and
//!   [`advance`](ExternalRuntimeSession::advance)s to the next
//!   [`RuntimeDecisionPoint`].
//!
//! # Host tools
//!
//! Injecting host tools into Claude Code requires running an MCP server the CLI
//! connects to (design §12.3). This adapter does **not** run one, so it honestly
//! reports [`host_tools`](ExternalRuntimeCapabilities::host_tools) and
//! [`host_subagents`](ExternalRuntimeCapabilities::host_subagents) as `false` and
//! refuses a [`start`](ExternalRuntimeAdapter::start) whose request declares
//! tools with [`ExternalAgentError::UnsupportedCapability`] rather than silently
//! ignoring them. Permission bridging, streaming, resume, artifacts, usage, and
//! graceful shutdown are supported.
//!
//! # Offline testability
//!
//! The session drives its IO through the private [`ClaudeSessionIo`] trait, not a
//! `tokio::process::Child` directly. Production uses [`ClaudeProcessIo`], which
//! spawns the real CLI; the unit tests inject a fake transport that replays canned
//! `stream-json` lines and captures the stdin frames the session writes, so the
//! whole start/advance/resume/shutdown state machine is exercised with no Claude
//! Code binary and no network. The real end-to-end coverage lives behind an
//! `#[ignore]` in `tests/external_claude_code.rs`.

// The session's fallible helpers return the external adapter's canonical
// `ExternalAgentError`, matching the unboxed error contract used across
// `adapter.rs`, `registry.rs`, `probe.rs`, and `decoder.rs`. That enum is
// intentionally not boxed, so `result_large_err` (which only fires because some
// helpers have small `Ok` types) would force a signature style inconsistent with
// the rest of the module.
#![allow(clippy::result_large_err)]

use std::io;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::{Instant, timeout, timeout_at};

use crate::agent::RunContext;
use crate::agent::id::StepId;
use crate::agent::interaction::InteractionResponse;
use crate::agent::permission::PermissionDecision;

use crate::agent::external::process_group;
use crate::agent::external::{
    ExternalAgentError, ExternalCapability, ExternalEventSink, ExternalObservedEvent,
    ExternalRuntimeAdapter, ExternalRuntimeCapabilities, ExternalRuntimeKind,
    ExternalRuntimeSession, ExternalSessionInput, ExternalSessionRef, ExternalSessionRequest,
    ExternalSessionShutdown, RuntimeDecisionPoint,
};

use super::{ClaudeCodeConfig, ClaudeDecision, ClaudeDecodeContext, ClaudeStreamDecoder};

/// Extra CLI flag the structured `--print --output-format stream-json` mode
/// requires so Claude Code emits the full frame stream rather than a summary.
const VERBOSE_FLAG: &str = "--verbose";

/// The transport a live Claude Code session reads frames from and writes turns to.
///
/// Splitting the raw IO behind this trait lets the session state machine
/// (`ClaudeCodeSession`) be unit-tested offline with a fake transport while
/// production drives a real CLI child through [`ClaudeProcessIo`]. Every method
/// is line-oriented: a frame is one newline-delimited `stream-json` object.
#[async_trait]
trait ClaudeSessionIo: Send {
    /// Writes one `stream-json` frame (a newline is appended) to the CLI stdin.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the stdin pipe is closed or the
    /// write fails.
    async fn write_frame(&mut self, frame: &str) -> io::Result<()>;

    /// Reads the next stdout frame line, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the read fails or times out.
    async fn read_frame(&mut self) -> io::Result<Option<String>>;

    /// Closes the transport and classifies how the close went.
    async fn close(&mut self) -> ExternalSessionShutdown;
}

/// Production [`ClaudeSessionIo`] backed by a real `tokio::process` child.
///
/// It pipes the CLI's stdin/stdout, kills the child on drop, bounds each read
/// with the configured read-idle timeout, and — on
/// [`close`](ClaudeSessionIo::close) — drops stdin so the CLI sees EOF, waits
/// for the exit within the shutdown grace (classifying it by status: zero →
/// graceful, non-zero → failed), and on overrun force-kills the child's whole
/// process group (unix; the direct child only on Windows) so CLI-spawned
/// grandchildren cannot outlive the session (H-EXT-2).
/// stderr is discarded so no raw runtime text can leak into a diagnostic.
struct ClaudeProcessIo {
    child: Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
    read_timeout: Duration,
    shutdown_grace: Duration,
}

impl ClaudeProcessIo {
    /// Spawns the Claude Code CLI in managed structured-stream mode.
    ///
    /// `resume` carries the runtime session id to reattach to (`--resume <id>`)
    /// when reviving a prior session; `None` starts a fresh one.
    ///
    /// # Errors
    ///
    /// Returns the raw [`io::Error`] from spawning (missing binary, permission
    /// denied); the caller classifies it into
    /// [`ExternalAgentError::Launch`]/[`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable).
    fn spawn(config: &ClaudeCodeConfig, resume: Option<&str>) -> io::Result<Self> {
        let mut args = config.base_session_args();
        args.push(VERBOSE_FLAG.to_owned());
        if let Some(session_id) = resume {
            args.push("--resume".to_owned());
            args.push(session_id.to_owned());
        }

        let mut command = Command::new(config.binary());
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(dir) = config.working_dir() {
            command.current_dir(dir);
        }
        for (key, value) in config.env() {
            command.env(key, value);
        }
        // The child leads its own process group on unix so a force-close can
        // signal the whole group, grandchildren included (H-EXT-2).
        process_group::configure_managed_command(&mut command);

        let mut child = command.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "claude code stdout was not captured",
            )
        })?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            read_timeout: config.read_idle_timeout(),
            shutdown_grace: config.shutdown_grace(),
        })
    }
}

#[async_trait]
impl ClaudeSessionIo for ClaudeProcessIo {
    async fn write_frame(&mut self, frame: &str) -> io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "claude code stdin is closed")
        })?;
        stdin.write_all(frame.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    }

    async fn read_frame(&mut self) -> io::Result<Option<String>> {
        match timeout(self.read_timeout, self.stdout.next_line()).await {
            Ok(result) => result,
            Err(_elapsed) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "claude code read timed out",
            )),
        }
    }

    async fn close(&mut self) -> ExternalSessionShutdown {
        // Dropping stdin signals EOF so the CLI can exit on its own.
        self.stdin = None;
        match timeout(self.shutdown_grace, self.child.wait()).await {
            Ok(Ok(status)) if status.success() => ExternalSessionShutdown::Graceful,
            // A non-zero exit means the CLI failed mid-session, so its partial
            // side effects cannot be trusted as clean (H-EXT-3).
            Ok(Ok(_status)) => ExternalSessionShutdown::Failed,
            Ok(Err(_error)) => ExternalSessionShutdown::Failed,
            Err(_elapsed) => match process_group::force_kill(&mut self.child).await {
                Ok(()) => ExternalSessionShutdown::ForcedKill,
                Err(_error) => ExternalSessionShutdown::Failed,
            },
        }
    }
}

/// One live Claude Code session wrapping the private stream decoder.
///
/// The session owns its transport and a single [`ClaudeStreamDecoder`] whose
/// `seq` line spans the whole session (design §5.5). Each
/// [`advance`](ExternalRuntimeSession::advance) writes the input turn's frame,
/// reads stdout frames feeding the decoder, mirrors observations to the live
/// sink, and returns at the first decision the decoder settles on.
struct ClaudeCodeSession<Io: ClaudeSessionIo> {
    io: Io,
    decoder: ClaudeStreamDecoder,
    session_id: String,
    last_event_seq: Option<u64>,
    sink: Option<Arc<dyn ExternalEventSink>>,
    capabilities: ExternalRuntimeCapabilities,
    /// Observations buffered by the startup prelude, prepended to the first turn.
    carried: Vec<ExternalObservedEvent>,
    /// A decision reached during the prelude (defensive; init precedes any turn).
    carried_decision: Option<ClaudeDecision>,
    /// Set when a fresh `start` already wrote the first turn's input (Claude Code
    /// emits nothing until it receives that frame), so the first `advance` — which
    /// carries the same input — must not write it a second time.
    first_turn_pending: bool,
}

impl<Io: ClaudeSessionIo> ClaudeCodeSession<Io> {
    /// Builds a session over `io`, binding the decode identities used for
    /// permission interactions and the capability set the adapter reports.
    fn new(
        io: Io,
        context: ClaudeDecodeContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
        capabilities: ExternalRuntimeCapabilities,
    ) -> Self {
        Self {
            io,
            decoder: ClaudeStreamDecoder::new(context),
            session_id: String::new(),
            last_event_seq: None,
            sink,
            capabilities,
            carried: Vec::new(),
            carried_decision: None,
            first_turn_pending: false,
        }
    }

    /// Seeds the session from the persisted high-water mark of a resumed
    /// session.
    ///
    /// Continues the decoder's `seq` line past `high_water` and restores the
    /// session's own water mark so [`session_ref`](ExternalRuntimeSession::session_ref)
    /// never reports a regressed `last_event_seq`. See
    /// [`ClaudeStreamDecoder::with_next_seq`] for why a resume must not restart
    /// the seq line at 0.
    #[must_use]
    fn with_resume_high_water(mut self, high_water: Option<u64>) -> Self {
        if let Some(high_water) = high_water {
            self.decoder = self.decoder.with_next_seq(high_water.saturating_add(1));
            self.last_event_seq = Some(high_water);
        }
        self
    }

    /// Runs the startup prelude for a fresh or resumed session.
    ///
    /// Claude Code emits **no output at all** — not even its `system`/`init`
    /// frame — until it receives the first stdin turn. So the two paths differ:
    ///
    /// - **Fresh start** (`first_input` set, `requested` `None`): the first turn
    ///   is written now so the CLI produces its `init` frame, then stdout is read
    ///   up to that frame to capture the runtime-assigned session id. The rest of
    ///   the turn (text, result) is deferred to the first
    ///   [`advance`](ExternalRuntimeSession::advance), which must therefore not
    ///   re-send the input; [`first_turn_pending`](Self::first_turn_pending)
    ///   records that.
    /// - **Resume** (`requested` set): the session id is already known from the
    ///   persisted [`ExternalSessionRef`], so no prelude read is needed; the first
    ///   `advance` writes its continuation turn and reads the fresh `init` frame
    ///   normally.
    ///
    /// The fresh-start prelude is bounded twice (review M-EXT-6): the whole loop
    /// must finish within `prelude_timeout` (the config's launch timeout — the
    /// per-line read-idle timeout resets every line, so a CLI babbling non-init
    /// frames would otherwise loop forever), and every iteration honours
    /// `ctx.is_cancelled()` like the `advance` loop does.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Protocol`] for a corrupt prelude frame,
    /// [`SessionLost`](ExternalAgentError::SessionLost) on a read failure or a
    /// cancellation, or [`Launch`](ExternalAgentError::Launch) when a fresh
    /// session never reports an id — including when `prelude_timeout` expires
    /// first.
    async fn begin(
        &mut self,
        first_input: Option<&ExternalSessionInput>,
        requested: Option<String>,
        ctx: &RunContext,
        prelude_timeout: Duration,
    ) -> Result<(), ExternalAgentError> {
        if let Some(session_id) = requested {
            // Resume: the id is already known; defer all IO to the first advance.
            self.session_id = session_id;
            return Ok(());
        }

        // Fresh start: the CLI stays silent until it receives the first turn, so
        // send it before reading the init frame that carries the session id.
        let input = first_input.expect("a fresh start must supply the first input");
        self.write_input(input).await?;
        self.first_turn_pending = true;

        let deadline = Instant::now() + prelude_timeout;
        while self.decoder.session_id().is_none() {
            if ctx.is_cancelled() {
                return Err(ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: "claude code session begin was cancelled".to_owned(),
                });
            }
            // The `timeout_at` below only fires while the runtime is polled; a
            // transport whose reads resolve instantly would starve the timer, so
            // the deadline is also enforced by this explicit wall-clock check.
            if Instant::now() >= deadline {
                return Err(ExternalAgentError::Launch {
                    runtime: ExternalRuntimeKind::ClaudeCode,
                    detail:
                        "claude code session did not report a session id within the launch timeout"
                            .to_owned(),
                });
            }
            let line = match timeout_at(deadline, self.read_line()).await {
                Ok(result) => result?,
                Err(_elapsed) => {
                    return Err(ExternalAgentError::Launch {
                        runtime: ExternalRuntimeKind::ClaudeCode,
                        detail: "claude code session did not report a session id within the launch timeout".to_owned(),
                    });
                }
            };
            match line {
                Some(line) => {
                    let decision = self.decoder.push_line(&line)?;
                    let observed = self.drain_and_emit();
                    self.carried.extend(observed);
                    if let Some(decision) = decision {
                        self.carried_decision = Some(decision);
                        break;
                    }
                }
                None => break,
            }
        }

        self.session_id = self
            .decoder
            .session_id()
            .map(str::to_owned)
            .ok_or_else(|| ExternalAgentError::Launch {
                runtime: ExternalRuntimeKind::ClaudeCode,
                detail: "claude code session did not report a session id".to_owned(),
            })?;
        Ok(())
    }

    /// Reads one stdout frame, classifying a read failure as
    /// [`SessionLost`](ExternalAgentError::SessionLost).
    async fn read_line(&mut self) -> Result<Option<String>, ExternalAgentError> {
        self.io
            .read_frame()
            .await
            .map_err(|error| ExternalAgentError::SessionLost {
                session: self.maybe_session_ref(),
                detail: format!("failed reading claude code stream: {:?}", error.kind()),
            })
    }

    /// Drains the decoder's buffered observations, mirroring each to the live
    /// sink and advancing the high-water `seq`.
    fn drain_and_emit(&mut self) -> Vec<ExternalObservedEvent> {
        let observed = self.decoder.take_observations();
        for event in &observed {
            if let Some(sink) = &self.sink {
                sink.emit(event);
            }
            self.last_event_seq = Some(event.seq);
        }
        observed
    }

    /// Writes the `stream-json` frame for `input`, or refuses an unsupported one.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::UnsupportedCapability`] for a tool/subagent
    /// result the adapter cannot bridge, [`Protocol`](ExternalAgentError::Protocol)
    /// for a mismatched interaction response, or
    /// [`SessionLost`](ExternalAgentError::SessionLost) on a write failure.
    async fn write_input(
        &mut self,
        input: &ExternalSessionInput,
    ) -> Result<(), ExternalAgentError> {
        match input {
            ExternalSessionInput::Start { prompt } => {
                self.write_frame(user_text_frame(prompt)).await
            }
            ExternalSessionInput::Continue { message } => {
                self.write_frame(user_text_frame(message)).await
            }
            ExternalSessionInput::RespondInteraction {
                action_id,
                response,
            } => {
                let frame = control_response_frame(action_id, response)?;
                self.write_frame(frame).await
            }
            ExternalSessionInput::RespondToolResults { .. } => Err(self.capabilities.unsupported(
                ExternalCapability::HostTools,
                "claude code adapter does not bridge host tool results",
            )),
            ExternalSessionInput::RespondSubagent { .. } => Err(self.capabilities.unsupported(
                ExternalCapability::HostSubagents,
                "claude code adapter does not bridge host subagents",
            )),
            // The graceful stop path closes the transport via `shutdown`; there is
            // no stdin turn to write for it.
            ExternalSessionInput::Shutdown => Ok(()),
        }
    }

    /// Writes one frame, classifying a write failure as
    /// [`SessionLost`](ExternalAgentError::SessionLost).
    async fn write_frame(&mut self, frame: String) -> Result<(), ExternalAgentError> {
        self.io
            .write_frame(&frame)
            .await
            .map_err(|error| ExternalAgentError::SessionLost {
                session: self.maybe_session_ref(),
                detail: format!("failed writing to claude code session: {:?}", error.kind()),
            })
    }

    /// Folds a settled [`ClaudeDecision`] into a [`RuntimeDecisionPoint`].
    fn finish(
        &self,
        decision: ClaudeDecision,
        observations: Vec<ExternalObservedEvent>,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        let session = self.session_ref();
        match decision {
            ClaudeDecision::Completed { output } => Ok(RuntimeDecisionPoint::Completed {
                session,
                output,
                observations,
            }),
            ClaudeDecision::PausedForToolCalls { batch_id, calls } => {
                Ok(RuntimeDecisionPoint::PausedForToolCalls {
                    session,
                    batch_id,
                    calls,
                    observations,
                })
            }
            ClaudeDecision::PausedForInteraction { action_id, request } => {
                Ok(RuntimeDecisionPoint::PausedForInteraction {
                    session,
                    action_id,
                    request,
                    observations,
                })
            }
            ClaudeDecision::Failed { error } => Err(error),
        }
    }

    /// Returns the session facts, or `None` before an id has been assigned.
    fn maybe_session_ref(&self) -> Option<ExternalSessionRef> {
        if self.session_id.is_empty() {
            None
        } else {
            Some(self.session_ref())
        }
    }
}

#[async_trait]
impl<Io: ClaudeSessionIo> ExternalRuntimeSession for ClaudeCodeSession<Io> {
    fn session_ref(&self) -> ExternalSessionRef {
        let session_id = (!self.session_id.is_empty()).then(|| self.session_id.clone());
        ExternalSessionRef {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: session_id.clone(),
            transcript_ref: None,
            // Claude Code resumes by session id (`--resume <id>`), so it doubles
            // as the opaque resume token.
            resume_token: session_id,
            last_event_seq: self.last_event_seq,
        }
    }

    async fn advance(
        &mut self,
        input: &ExternalSessionInput,
        ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        let mut collected = std::mem::take(&mut self.carried);

        // A fresh `start` already wrote and began this turn, so the first advance
        // (carrying the same input) continues the in-flight turn instead of
        // writing it again.
        let already_written = std::mem::take(&mut self.first_turn_pending);

        // A decision reached during the startup prelude settles the turn without
        // any further IO (defensive: init precedes the first turn in practice).
        if let Some(decision) = self.carried_decision.take() {
            return self.finish(decision, collected);
        }

        if !already_written {
            self.write_input(input).await?;
        }

        loop {
            if ctx.is_cancelled() {
                return Err(ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: "claude code session advance was cancelled".to_owned(),
                });
            }
            match self.read_line().await? {
                Some(line) => {
                    let decision = self.decoder.push_line(&line)?;
                    collected.extend(self.drain_and_emit());
                    if let Some(decision) = decision {
                        return self.finish(decision, collected);
                    }
                }
                None => {
                    return Err(ExternalAgentError::SessionLost {
                        session: self.maybe_session_ref(),
                        detail: "claude code session closed before reaching a decision point"
                            .to_owned(),
                    });
                }
            }
        }
    }

    async fn shutdown(&mut self) -> ExternalSessionShutdown {
        self.io.close().await
    }
}

/// Managed adapter that starts and resumes live Claude Code CLI sessions.
///
/// Construct one from a [`ClaudeCodeConfig`] with [`new`](Self::new) (assuming a
/// fully capable CLI) or [`with_probed_capabilities`](Self::with_probed_capabilities)
/// to intersect the adapter's implemented features with what a
/// [`probe`](super::probe) confirmed on the local binary. Wrap the adapter in an
/// [`ExternalSessionRegistry`](crate::agent::external::ExternalSessionRegistry) to
/// own its live sessions between decision points.
pub struct ClaudeCodeAdapter {
    config: ClaudeCodeConfig,
    capabilities: ExternalRuntimeCapabilities,
}

impl ClaudeCodeAdapter {
    /// Builds an adapter for `config` reporting every managed feature this
    /// adapter implements.
    ///
    /// The reported set is fixed: streaming, resume,
    /// permission bridging, artifacts, usage, and graceful shutdown are on;
    /// host-tool and host-subagent bridging are off because this adapter runs no
    /// MCP server (design §12.3). Prefer
    /// [`with_probed_capabilities`](Self::with_probed_capabilities) when a probe
    /// has confirmed which features the local binary actually advertises.
    #[must_use]
    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self {
            config,
            capabilities: implemented_capabilities(),
        }
    }

    /// Builds an adapter whose reported capabilities are the intersection of what
    /// this adapter implements and what a probe found on the local CLI.
    ///
    /// A feature is reported supported only when *both* the adapter implements it
    /// and the probe advertised it, so a binary lacking `--resume` disables
    /// resume while host-tool bridging stays off regardless of the probe (this
    /// adapter never serves it).
    #[must_use]
    pub fn with_probed_capabilities(
        config: ClaudeCodeConfig,
        probed: &ExternalRuntimeCapabilities,
    ) -> Self {
        Self {
            config,
            capabilities: intersect_capabilities(&implemented_capabilities(), probed),
        }
    }

    /// Returns the launch configuration backing this adapter.
    #[must_use]
    pub const fn config(&self) -> &ClaudeCodeConfig {
        &self.config
    }

    /// Refuses a request that declares host tools this adapter cannot inject.
    fn reject_unsupported_tools(
        &self,
        request: &ExternalSessionRequest,
    ) -> Result<(), ExternalAgentError> {
        if !request.tools.is_empty() && !self.capabilities.host_tools {
            return Err(self.capabilities.unsupported(
                ExternalCapability::HostTools,
                "claude code adapter cannot inject host tools without an MCP bridge",
            ));
        }
        Ok(())
    }

    /// Resolves the effective session configuration for `request`.
    ///
    /// Request-level policy wins over the construction-time config (M2-7 /
    /// M-PROM-5): [`ExternalSessionPolicy::permission_mode`] overrides
    /// [`with_permission_mode`](ClaudeCodeConfig::with_permission_mode), and a
    /// prepared [`session_dir`](ExternalSessionRequest::session_dir) overrides
    /// [`with_working_dir`](ClaudeCodeConfig::with_working_dir). The stored
    /// config remains the fallback for request-less operations (the capability
    /// probe).
    fn session_config(&self, request: &ExternalSessionRequest) -> ClaudeCodeConfig {
        let mut config = self
            .config
            .clone()
            .with_permission_mode(request.policy.permission_mode);
        if let Some(dir) = &request.session_dir {
            config = config.with_working_dir(dir.path().to_path_buf());
        }
        config
    }

    /// Builds the decode context binding permission interactions to the host's
    /// run identity and the requesting agent.
    ///
    /// The permission step id is derived from the caller-supplied
    /// [`run_id`](RunContext::run_id) (the Agent layer mints no ids of its own),
    /// and the actor is the request's own agent — never anything the runtime
    /// reports.
    fn decode_context(ctx: &RunContext, request: &ExternalSessionRequest) -> ClaudeDecodeContext {
        ClaudeDecodeContext::new(StepId::new(*ctx.run_id().as_uuid()), request.agent_id)
    }
}

#[async_trait]
impl ExternalRuntimeAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> ExternalRuntimeKind {
        ExternalRuntimeKind::ClaudeCode
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

        let config = self.session_config(request);
        let io = ClaudeProcessIo::spawn(&config, None)
            .map_err(|error| launch_error(&config, "spawning claude code", &error))?;
        let mut session = ClaudeCodeSession::new(
            io,
            Self::decode_context(ctx, request),
            sink,
            self.capabilities.clone(),
        );
        session
            .begin(Some(&request.input), None, ctx, config.timeout())
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
                detail: "claude code session has no id to resume".to_owned(),
            });
        };

        let config = self.session_config(request);
        let io = ClaudeProcessIo::spawn(&config, Some(&session_id)).map_err(|error| {
            ExternalAgentError::ResumeUnavailable {
                session: session.clone(),
                detail: format!("failed spawning claude code to resume: {:?}", error.kind()),
            }
        })?;
        let mut live = ClaudeCodeSession::new(
            io,
            Self::decode_context(ctx, request),
            sink,
            self.capabilities.clone(),
        )
        .with_resume_high_water(session.last_event_seq);
        live.begin(None, Some(session_id), ctx, config.timeout())
            .await?;
        Ok(Box::new(live))
    }
}

/// Returns the managed features this adapter can actually fulfill.
///
/// Host-tool and host-subagent bridging are off (no MCP server); the rest are on
/// because the structured stream, permission control channel, `--resume` flag,
/// file-edit frames, result usage, and clean stdin close back them.
fn implemented_capabilities() -> ExternalRuntimeCapabilities {
    ExternalRuntimeCapabilities {
        runtime: ExternalRuntimeKind::ClaudeCode,
        streaming: true,
        resume: true,
        permission_bridge: true,
        host_tools: false,
        host_subagents: false,
        artifacts: true,
        usage: true,
        graceful_shutdown: true,
        reconfigure: false,
    }
}

/// Intersects two capability sets field-by-field, keeping the left runtime.
fn intersect_capabilities(
    left: &ExternalRuntimeCapabilities,
    right: &ExternalRuntimeCapabilities,
) -> ExternalRuntimeCapabilities {
    ExternalRuntimeCapabilities {
        runtime: left.runtime.clone(),
        streaming: left.streaming && right.streaming,
        resume: left.resume && right.resume,
        permission_bridge: left.permission_bridge && right.permission_bridge,
        host_tools: left.host_tools && right.host_tools,
        host_subagents: left.host_subagents && right.host_subagents,
        artifacts: left.artifacts && right.artifacts,
        usage: left.usage && right.usage,
        graceful_shutdown: left.graceful_shutdown && right.graceful_shutdown,
        reconfigure: left.reconfigure && right.reconfigure,
    }
}

/// Builds a classified [`ExternalAgentError::Launch`] from a spawn failure.
///
/// The detail names the stage, the binary path, and the classified
/// [`io::ErrorKind`] only — never the config's env values or CLI output — so a
/// launch failure surfaced to a log cannot leak a secret.
fn launch_error(config: &ClaudeCodeConfig, stage: &str, error: &io::Error) -> ExternalAgentError {
    ExternalAgentError::Launch {
        runtime: ExternalRuntimeKind::ClaudeCode,
        detail: format!(
            "{stage} binary {} failed: {:?}",
            config.binary().display(),
            error.kind()
        ),
    }
}

/// Builds a `stream-json` user-turn frame carrying `text`.
fn user_text_frame(text: &str) -> String {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [ { "type": "text", "text": text } ],
        },
    })
    .to_string()
}

/// Builds a `control_response` frame answering a permission `control_request`.
///
/// A permission pause only accepts a
/// [`Permission`](InteractionResponse::Permission) response; an
/// [`Approve`](PermissionDecision::Approve) maps to `allow` and a
/// [`Deny`](PermissionDecision::Deny)/[`Cancel`](PermissionDecision::Cancel) maps
/// to `deny` (with the host's stable reason, when supplied).
///
/// # Errors
///
/// Returns [`ExternalAgentError::Protocol`] when the response is not a permission
/// decision.
fn control_response_frame(
    action_id: &str,
    response: &InteractionResponse,
) -> Result<String, ExternalAgentError> {
    let InteractionResponse::Permission(permission) = response else {
        return Err(ExternalAgentError::Protocol {
            detail: "claude code permission pause requires a permission response".to_owned(),
        });
    };
    let behavior: Value = match permission.decision() {
        PermissionDecision::Approve => json!({ "behavior": "allow" }),
        PermissionDecision::Deny { reason } => match reason {
            Some(message) => json!({ "behavior": "deny", "message": message }),
            None => json!({ "behavior": "deny" }),
        },
        PermissionDecision::Cancel => {
            json!({ "behavior": "deny", "message": "cancelled by host" })
        }
    };
    Ok(json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": action_id,
            "response": behavior,
        },
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        ClaudeCodeAdapter, ClaudeCodeSession, ClaudeSessionIo, control_response_frame,
        implemented_capabilities, intersect_capabilities, user_text_frame,
    };
    use crate::agent::external::ClaudeCodeConfig;
    use crate::agent::external::{
        ExternalAgentError, ExternalCapability, ExternalEventSink, ExternalObservedEvent,
        ExternalPermissionMode, ExternalRuntimeAdapter, ExternalRuntimeCapabilities,
        ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionInput, ExternalSessionPolicy,
        ExternalSessionRequest, ExternalSessionShutdown, ExternalStreamPolicy,
        RuntimeDecisionPoint, WorktreeIsolation,
    };
    use crate::agent::interaction::InteractionResponse;
    use crate::agent::permission::PermissionResponse;
    use crate::agent::spec::WorktreeRef;
    use crate::agent::{AgentId, BudgetLimits, RunContext, RunId, TraceNodeId};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    const SESSION_ID: &str = "claude-sess-1";
    const RUN_UUID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890e0";
    const AGENT_UUID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890f0";
    /// Generous prelude bound for tests that never exercise the deadline.
    const PRELUDE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    fn agent_id() -> AgentId {
        AGENT_UUID.parse().expect("agent id parses")
    }

    fn run_context() -> RunContext {
        let run_id: RunId = RUN_UUID.parse().expect("run id parses");
        RunContext::new_root(
            run_id,
            BudgetLimits::unbounded(),
            TraceNodeId::new("claude-code-adapter-test"),
        )
    }

    fn policy() -> ExternalSessionPolicy {
        ExternalSessionPolicy {
            permission_mode: ExternalPermissionMode::Prompt,
            isolation: WorktreeIsolation::EphemeralGitWorktree,
            max_turns: Some(8),
            stream_events: ExternalStreamPolicy::Streaming,
        }
    }

    fn start_request(tools: Vec<crate::model::tool::Tool>) -> ExternalSessionRequest {
        ExternalSessionRequest {
            agent_id: agent_id(),
            runtime: ExternalRuntimeKind::ClaudeCode,
            worktree: WorktreeRef::new("/repo/agent-lib"),
            session_dir: None,
            session: None,
            input: ExternalSessionInput::Start {
                prompt: "investigate the failing test".to_owned(),
            },
            tools,
            policy: policy(),
        }
    }

    /// A fake transport replaying canned stdout lines and capturing stdin frames.
    struct FakeIo {
        lines: VecDeque<String>,
        written: Arc<Mutex<Vec<String>>>,
        close_disposition: ExternalSessionShutdown,
        closed: Arc<Mutex<Option<ExternalSessionShutdown>>>,
        /// Line replayed forever once `lines` drains (prelude-deadline tests).
        repeat: Option<String>,
    }

    impl FakeIo {
        fn new(lines: Vec<String>) -> (Self, Arc<Mutex<Vec<String>>>) {
            let written = Arc::new(Mutex::new(Vec::new()));
            let io = Self {
                lines: lines.into_iter().collect(),
                written: Arc::clone(&written),
                close_disposition: ExternalSessionShutdown::Graceful,
                closed: Arc::new(Mutex::new(None)),
                repeat: None,
            };
            (io, written)
        }

        fn with_close(mut self, disposition: ExternalSessionShutdown) -> Self {
            self.close_disposition = disposition;
            self
        }

        /// Replays `line` forever once the canned lines drain, so a prelude that
        /// never sees its `init` frame can only end on the launch deadline.
        fn repeating(mut self, line: String) -> Self {
            self.repeat = Some(line);
            self
        }
    }

    #[async_trait]
    impl ClaudeSessionIo for FakeIo {
        async fn write_frame(&mut self, frame: &str) -> std::io::Result<()> {
            self.written.lock().unwrap().push(frame.to_owned());
            Ok(())
        }

        async fn read_frame(&mut self) -> std::io::Result<Option<String>> {
            match self.lines.pop_front() {
                Some(line) => Ok(Some(line)),
                None => Ok(self.repeat.clone()),
            }
        }

        async fn close(&mut self) -> ExternalSessionShutdown {
            *self.closed.lock().unwrap() = Some(self.close_disposition);
            self.close_disposition
        }
    }

    /// Collecting sink recording every live observation.
    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<ExternalObservedEvent>>,
    }

    impl ExternalEventSink for RecordingSink {
        fn emit(&self, event: &ExternalObservedEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    fn session_over(
        lines: Vec<String>,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> (ClaudeCodeSession<FakeIo>, Arc<Mutex<Vec<String>>>) {
        let (io, written) = FakeIo::new(lines);
        let context = ClaudeCodeAdapter::decode_context(&run_context(), &start_request(Vec::new()));
        let session = ClaudeCodeSession::new(io, context, sink, implemented_capabilities());
        (session, written)
    }

    fn init_frame() -> String {
        format!(r#"{{"type":"system","subtype":"init","session_id":"{SESSION_ID}","cwd":"/repo"}}"#)
    }

    fn assistant_text_frame(text: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"id":"msg-1","role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    fn permission_request_frame(request_id: &str) -> String {
        format!(
            r#"{{"type":"control_request","request_id":"{request_id}","request":{{"subtype":"can_use_tool","tool_name":"Bash","input":{{"command":"cargo test"}}}}}}"#
        )
    }

    fn result_frame() -> String {
        r#"{"type":"result","subtype":"success","result":"all good","total_cost_usd":0.01,"usage":{"input_tokens":10,"output_tokens":5}}"#.to_owned()
    }

    #[tokio::test]
    async fn claude_code_adapter_advance_drives_text_permission_completion() {
        let sink = Arc::new(RecordingSink::default());
        let (mut session, written) = session_over(
            vec![
                init_frame(),
                assistant_text_frame("looking into it"),
                permission_request_frame("perm-1"),
                assistant_text_frame("running the test"),
                result_frame(),
            ],
            Some(Arc::clone(&sink) as Arc<dyn ExternalEventSink>),
        );

        session
            .begin(
                Some(&start_request(Vec::new()).input),
                None,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("start writes the first turn and reads the init frame");
        assert_eq!(
            session.session_ref().session_id.as_deref(),
            Some(SESSION_ID)
        );

        let ctx = run_context();
        // Turn 1: the prompt is written, the session streams text then pauses for
        // the permission control request.
        let first = session
            .advance(&start_request(Vec::new()).input, &ctx)
            .await
            .expect("first advance settles on a decision");
        let action_id = match first {
            RuntimeDecisionPoint::PausedForInteraction {
                action_id,
                observations,
                ..
            } => {
                // The init SessionStarted plus the text delta plus the permission
                // observation all ride the first decision point.
                assert!(
                    observations.len() >= 3,
                    "carried prelude + turn observations"
                );
                action_id
            }
            other => panic!("expected a permission pause, got {other:?}"),
        };
        assert_eq!(action_id, "perm-1");
        // The prompt frame was written to stdin.
        assert!(written.lock().unwrap()[0].contains("investigate the failing test"));

        // Turn 2: answering the permission writes a control_response and the
        // session runs to completion.
        let approve =
            InteractionResponse::Permission(PermissionResponse::approve(action_id.clone()));
        let respond = ExternalSessionInput::RespondInteraction {
            action_id,
            response: approve,
        };
        let second = session.advance(&respond, &ctx).await.expect("completion");
        match second {
            RuntimeDecisionPoint::Completed { output, .. } => {
                assert_eq!(output.summary, "all good");
                assert_eq!(output.cost_micros, Some(10_000));
                assert!(output.usage.is_some());
            }
            other => panic!("expected completion, got {other:?}"),
        }
        // The control_response frame echoes the runtime request id and allows.
        let frames = written.lock().unwrap().clone();
        assert!(frames.iter().any(|f| f.contains("control_response")
            && f.contains("perm-1")
            && f.contains("allow")));

        // The sink saw the same sequenced observations, monotonically.
        let seqs: Vec<u64> = sink.events.lock().unwrap().iter().map(|e| e.seq).collect();
        assert!(
            seqs.windows(2).all(|w| w[0] < w[1]),
            "seq is monotonic: {seqs:?}"
        );
    }

    #[tokio::test]
    async fn claude_code_adapter_advance_reports_session_lost_on_early_eof() {
        let (mut session, _written) = session_over(vec![init_frame()], None);
        session
            .begin(
                Some(&start_request(Vec::new()).input),
                None,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("start prelude");
        let ctx = run_context();
        let error = session
            .advance(&start_request(Vec::new()).input, &ctx)
            .await
            .expect_err("eof before a decision is a lost session");
        match error {
            ExternalAgentError::SessionLost { session, detail } => {
                assert_eq!(
                    session.and_then(|s| s.session_id).as_deref(),
                    Some(SESSION_ID)
                );
                assert!(detail.contains("decision point"));
            }
            other => panic!("expected SessionLost, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_code_adapter_advance_propagates_protocol_error_on_malformed_frame() {
        let mut frames = vec![init_frame()];
        frames.extend((0..=8).map(|_| "{ not json".to_owned()));
        let (mut session, _written) = session_over(frames, None);
        session
            .begin(
                Some(&start_request(Vec::new()).input),
                None,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("start prelude");
        let ctx = run_context();
        let error = session
            .advance(&start_request(Vec::new()).input, &ctx)
            .await
            .expect_err("too much non-json noise is a protocol error");
        assert!(matches!(error, ExternalAgentError::Protocol { .. }));
    }

    #[tokio::test]
    async fn claude_code_adapter_shutdown_classifies_the_close() {
        let (io, _written) = FakeIo::new(vec![init_frame()]);
        let io = io.with_close(ExternalSessionShutdown::ForcedKill);
        let context = ClaudeCodeAdapter::decode_context(&run_context(), &start_request(Vec::new()));
        let mut session = ClaudeCodeSession::new(io, context, None, implemented_capabilities());
        session
            .begin(
                Some(&start_request(Vec::new()).input),
                None,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("start prelude");
        assert_eq!(
            session.shutdown().await,
            ExternalSessionShutdown::ForcedKill
        );
    }

    #[tokio::test]
    async fn claude_code_adapter_begin_times_out_when_init_never_arrives() {
        // A CLI babbling tolerated non-init frames would loop the prelude forever
        // on the per-line read timeout alone (each line resets it); the launch
        // deadline caps the whole prelude (review M-EXT-6).
        let (io, _written) = FakeIo::new(Vec::new());
        let io = io.repeating(r#"{"type":"ping"}"#.to_owned());
        let context = ClaudeCodeAdapter::decode_context(&run_context(), &start_request(Vec::new()));
        let mut session = ClaudeCodeSession::new(io, context, None, implemented_capabilities());
        let started = std::time::Instant::now();
        let error = session
            .begin(
                Some(&start_request(Vec::new()).input),
                None,
                &run_context(),
                std::time::Duration::from_millis(50),
            )
            .await
            .expect_err("a prelude that never reports an id hits the launch deadline");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the prelude deadline fires promptly"
        );
        match error {
            ExternalAgentError::Launch { runtime, detail } => {
                assert_eq!(runtime, ExternalRuntimeKind::ClaudeCode);
                assert!(detail.contains("launch timeout"), "detail: {detail}");
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_code_adapter_begin_honours_cancellation() {
        // The prelude checks `ctx.is_cancelled()` per iteration, just like the
        // advance loop (review M-EXT-6).
        let (mut session, _written) = session_over(vec![init_frame()], None);
        let ctx = run_context();
        ctx.cancellation().cancel();
        let error = session
            .begin(
                Some(&start_request(Vec::new()).input),
                None,
                &ctx,
                PRELUDE_TIMEOUT,
            )
            .await
            .expect_err("a cancelled run aborts the prelude");
        match error {
            ExternalAgentError::SessionLost { detail, .. } => {
                assert!(detail.contains("cancelled"), "detail: {detail}");
            }
            other => panic!("expected SessionLost, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_code_adapter_respond_tool_results_is_unsupported() {
        // Resume-style begin: the session id is already known and no first turn is
        // pending, so the advance below reaches the input's capability check.
        let (mut session, _written) = session_over(vec![init_frame()], None);
        session
            .begin(
                None,
                Some(SESSION_ID.to_owned()),
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("resume prelude");
        let ctx = run_context();
        let input = ExternalSessionInput::RespondToolResults {
            batch_id: crate::agent::external::ExternalToolBatchId::new("batch-1"),
            results: Vec::new(),
        };
        let error = session
            .advance(&input, &ctx)
            .await
            .expect_err("host tool results are unsupported");
        match error {
            ExternalAgentError::UnsupportedCapability { capability, .. } => {
                assert_eq!(capability, ExternalCapability::HostTools);
            }
            other => panic!("expected UnsupportedCapability, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_code_adapter_start_writes_prompt_before_reading_init() {
        // Claude Code stays silent until it receives the first turn, so `begin`
        // for a fresh start must write the prompt *before* consuming the init
        // frame that carries the session id.
        let (mut session, written) = session_over(
            vec![init_frame(), assistant_text_frame("hi"), result_frame()],
            None,
        );
        let input = start_request(Vec::new()).input;
        session
            .begin(Some(&input), None, &run_context(), PRELUDE_TIMEOUT)
            .await
            .expect("start prelude");

        // The session id was learned from the init frame, and the prompt was the
        // first (and so far only) frame written to stdin.
        assert_eq!(
            session.session_ref().session_id.as_deref(),
            Some(SESSION_ID)
        );
        let frames = written.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "only the prompt is written during begin");
        assert!(frames[0].contains("investigate the failing test"));

        // The first advance continues the in-flight turn without re-sending the
        // prompt, running it to completion.
        let ctx = run_context();
        let decision = session.advance(&input, &ctx).await.expect("completion");
        assert!(matches!(decision, RuntimeDecisionPoint::Completed { .. }));
        assert_eq!(
            written.lock().unwrap().len(),
            1,
            "the first advance must not write the prompt a second time"
        );
    }

    #[tokio::test]
    async fn claude_code_adapter_resume_defers_first_turn_to_advance() {
        // Resume already knows the session id, so `begin` reads nothing; the first
        // advance writes its continuation turn and reads the fresh init + result.
        let (mut session, written) = session_over(
            vec![
                init_frame(),
                assistant_text_frame("resumed"),
                result_frame(),
            ],
            None,
        );
        session
            .begin(
                None,
                Some(SESSION_ID.to_owned()),
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("resume prelude");
        assert_eq!(
            session.session_ref().session_id.as_deref(),
            Some(SESSION_ID)
        );
        assert!(
            written.lock().unwrap().is_empty(),
            "resume writes nothing until the first advance"
        );

        let ctx = run_context();
        let input = ExternalSessionInput::Continue {
            message: "keep going".to_owned(),
        };
        let decision = session.advance(&input, &ctx).await.expect("completion");
        assert!(matches!(decision, RuntimeDecisionPoint::Completed { .. }));
        let frames = written.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "the continuation turn is written once");
        assert!(frames[0].contains("keep going"));
    }

    #[tokio::test]
    async fn claude_code_adapter_resume_continues_the_seq_line_past_the_high_water() {
        // A resume must continue the decoder's seq line past the persisted
        // `last_event_seq`: restarting at 0 would let the machine's replay dedup
        // silently drop every post-resume observation (design §5.5, review
        // M-EXT-1).
        let (session, _written) = session_over(
            vec![
                init_frame(),
                assistant_text_frame("resumed"),
                result_frame(),
            ],
            None,
        );
        let mut session = session.with_resume_high_water(Some(50));
        session
            .begin(
                None,
                Some(SESSION_ID.to_owned()),
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("resume prelude");
        // The restored water mark is reported even before any fresh event.
        assert_eq!(session.session_ref().last_event_seq, Some(50));

        let ctx = run_context();
        let input = ExternalSessionInput::Continue {
            message: "keep going".to_owned(),
        };
        let decision = session.advance(&input, &ctx).await.expect("completion");
        let RuntimeDecisionPoint::Completed { observations, .. } = decision else {
            panic!("expected completion");
        };
        assert!(!observations.is_empty());
        assert_eq!(
            observations[0].seq, 51,
            "the first post-resume observation continues past the high water"
        );
        assert!(
            observations
                .windows(2)
                .all(|pair| pair[1].seq == pair[0].seq + 1),
            "the seq line stays contiguous"
        );
        assert_eq!(
            session.session_ref().last_event_seq,
            Some(observations.last().expect("non-empty").seq),
            "the reported water mark never regresses below the persisted one"
        );
    }

    #[test]
    fn claude_code_adapter_implemented_capabilities_disable_host_tools_and_subagents() {
        let caps = implemented_capabilities();
        assert!(caps.streaming);
        assert!(caps.resume);
        assert!(caps.permission_bridge);
        assert!(caps.artifacts);
        assert!(caps.usage);
        assert!(caps.graceful_shutdown);
        assert!(!caps.host_tools, "no MCP bridge means no host tools");
        assert!(
            !caps.host_subagents,
            "no spawn bridge means no host subagents"
        );
    }

    #[test]
    fn claude_code_adapter_probed_capabilities_intersect_with_implemented() {
        let mut probed = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::ClaudeCode);
        // A CLI that advertises streaming but not resume, and claims host tools.
        probed.streaming = true;
        probed.resume = false;
        probed.permission_bridge = true;
        probed.host_tools = true;
        probed.artifacts = true;
        probed.usage = true;
        probed.graceful_shutdown = true;

        let adapter = ClaudeCodeAdapter::with_probed_capabilities(ClaudeCodeConfig::new(), &probed);
        let caps = adapter.capabilities();
        assert!(caps.streaming, "streaming is implemented and probed");
        assert!(!caps.resume, "resume is off because the probe lacked it");
        assert!(
            !caps.host_tools,
            "host tools stay off even though the probe claimed them"
        );
        assert_eq!(adapter.kind(), ExternalRuntimeKind::ClaudeCode);
    }

    #[test]
    fn claude_code_adapter_intersect_keeps_left_runtime_and_ands_flags() {
        let left = implemented_capabilities();
        let right = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::ClaudeCode);
        let both = intersect_capabilities(&left, &right);
        assert_eq!(both.runtime, ExternalRuntimeKind::ClaudeCode);
        for capability in ExternalCapability::ALL {
            assert!(!both.supports(capability));
        }
    }

    #[tokio::test]
    async fn claude_code_adapter_start_rejects_declared_tools() {
        let tool = crate::model::tool::Tool {
            name: "search".to_owned(),
            description: "search the repo".to_owned(),
            input_schema: serde_json::json!({ "type": "object" }),
        };
        let adapter = ClaudeCodeAdapter::new(ClaudeCodeConfig::new());
        let ctx = run_context();
        let outcome = adapter.start(&start_request(vec![tool]), &ctx, None).await;
        match outcome {
            Err(ExternalAgentError::UnsupportedCapability {
                capability,
                runtime,
                ..
            }) => {
                assert_eq!(capability, ExternalCapability::HostTools);
                assert_eq!(runtime, ExternalRuntimeKind::ClaudeCode);
            }
            Err(other) => panic!("expected UnsupportedCapability, got {other:?}"),
            Ok(_) => panic!("declared host tools must be refused before spawning"),
        }
    }

    #[test]
    fn claude_code_adapter_user_text_frame_is_a_valid_stream_json_user_turn() {
        let frame = user_text_frame("hello \"world\"");
        let value: serde_json::Value = serde_json::from_str(&frame).expect("valid json");
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["content"][0]["text"], "hello \"world\"");
    }

    #[test]
    fn claude_code_adapter_control_response_frame_maps_allow_and_deny() {
        let allow = control_response_frame(
            "perm-9",
            &InteractionResponse::Permission(PermissionResponse::approve("perm-9".to_owned())),
        )
        .expect("allow frame");
        let allow_value: serde_json::Value = serde_json::from_str(&allow).expect("json");
        assert_eq!(allow_value["response"]["request_id"], "perm-9");
        assert_eq!(allow_value["response"]["response"]["behavior"], "allow");

        let deny = control_response_frame(
            "perm-9",
            &InteractionResponse::Permission(PermissionResponse::deny(
                "perm-9".to_owned(),
                Some("not allowed".to_owned()),
            )),
        )
        .expect("deny frame");
        let deny_value: serde_json::Value = serde_json::from_str(&deny).expect("json");
        assert_eq!(deny_value["response"]["response"]["behavior"], "deny");
        assert_eq!(deny_value["response"]["response"]["message"], "not allowed");
    }

    #[test]
    fn claude_code_adapter_control_response_frame_rejects_non_permission_response() {
        let error =
            control_response_frame("perm-9", &InteractionResponse::Answer("yes".to_owned()))
                .expect_err("only permission responses are valid");
        assert!(matches!(error, ExternalAgentError::Protocol { .. }));
    }

    #[test]
    fn claude_code_adapter_cancel_decision_maps_to_deny() {
        let frame = control_response_frame(
            "perm-9",
            &InteractionResponse::Permission(PermissionResponse::cancel("perm-9".to_owned())),
        )
        .expect("cancel frame");
        let value: serde_json::Value = serde_json::from_str(&frame).expect("json");
        assert_eq!(value["response"]["response"]["behavior"], "deny");
    }

    /// H-EXT-3: `close` classifies the child exit by status code, so a crashed
    /// CLI is never mistaken for a clean close (which would mark a dirty
    /// worktree as reusable). These tests spawn a real short-lived `sh` child
    /// wired exactly like the production transport.
    mod close_classification {
        use super::super::{ClaudeProcessIo, ClaudeSessionIo};
        use crate::agent::external::{ExternalSessionShutdown, process_group};
        use std::time::Duration;
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;

        /// Spawns a real `sh -c <script>` child with piped stdio.
        fn spawn_sh(script: &str) -> ClaudeProcessIo {
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg(script)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            // Mirror the production spawn: the child leads its own process group.
            process_group::configure_managed_command(&mut command);
            let mut child = command.spawn().expect("spawn sh");
            let stdin = child.stdin.take();
            let stdout = child.stdout.take().expect("stdout is piped");
            ClaudeProcessIo {
                child,
                stdin,
                stdout: BufReader::new(stdout).lines(),
                read_timeout: Duration::from_secs(1),
                shutdown_grace: Duration::from_millis(250),
            }
        }

        /// A zero exit status closes `Graceful`.
        #[tokio::test]
        async fn zero_exit_is_graceful() {
            let mut io = spawn_sh("exit 0");
            assert_eq!(io.close().await, ExternalSessionShutdown::Graceful);
        }

        /// A non-zero exit status closes `Failed`, not `Graceful`.
        #[tokio::test]
        async fn nonzero_exit_is_failed() {
            let mut io = spawn_sh("exit 1");
            assert_eq!(io.close().await, ExternalSessionShutdown::Failed);
        }

        /// A child still running past the grace window is force-killed.
        #[tokio::test]
        async fn grace_overrun_is_forced_kill() {
            let mut io = spawn_sh("sleep 30");
            assert_eq!(io.close().await, ExternalSessionShutdown::ForcedKill);
        }

        /// H-EXT-2: a force-close kills the whole process group, so
        /// grandchildren the CLI spawned (builds, dev servers, ...) cannot
        /// outlive the session.
        #[cfg(unix)]
        #[tokio::test]
        async fn force_close_kills_the_whole_process_group() {
            let mut io = spawn_sh("sleep 300 & sleep 300");
            let pgid = io.child.id().expect("child id") as i32;
            assert_eq!(io.close().await, ExternalSessionShutdown::ForcedKill);
            process_group::assert_process_group_reaped(pgid).await;
        }
    }

    #[test]
    fn session_config_applies_request_level_policy_overrides() {
        // M2-7: the request's policy overrides the construction-time config —
        // permission_mode always, session_dir (the registry-prepared worktree)
        // when present; the config's working_dir remains the fallback.
        let adapter = ClaudeCodeAdapter::new(
            ClaudeCodeConfig::new()
                .with_permission_mode(ExternalPermissionMode::Prompt)
                .with_working_dir("/config/dir"),
        );

        let mut request = start_request(Vec::new());
        request.policy.permission_mode = ExternalPermissionMode::Plan;
        request.session_dir = Some(WorktreeRef::new("/prepared/session-0"));

        let effective = adapter.session_config(&request);
        assert_eq!(
            effective.permission_mode(),
            ExternalPermissionMode::Plan,
            "the request policy mode wins over the config mode"
        );
        assert_eq!(
            effective.working_dir(),
            Some(std::path::Path::new("/prepared/session-0")),
            "the prepared session dir wins over the config working dir"
        );
        let args = effective.base_session_args();
        let flag = args
            .iter()
            .position(|arg| arg == "--permission-mode")
            .expect("permission-mode flag present");
        assert_eq!(args[flag + 1], "plan");

        let fallback = adapter.session_config(&start_request(Vec::new()));
        assert_eq!(
            fallback.working_dir(),
            Some(std::path::Path::new("/config/dir")),
            "without a prepared session dir the config working dir stays"
        );
        assert_eq!(
            fallback.permission_mode(),
            ExternalPermissionMode::Prompt,
            "the fixture request policy still overrides (here: same value)"
        );
    }
}
