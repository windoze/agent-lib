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
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::agent::RunContext;
use crate::agent::id::StepId;
use crate::agent::interaction::InteractionResponse;
use crate::agent::permission::PermissionDecision;

use crate::agent::external::process::{self, ChildStdinMode, ManagedChild, PreludeDeadline};
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

/// Spawns the Claude Code CLI in managed structured-stream mode.
///
/// `resume` carries the runtime session id to reattach to (`--resume <id>`) when
/// reviving a prior session; `None` starts a fresh one. The shared
/// [`ManagedChild`] owns stdin/stdout, read timeouts, exit-code classification,
/// and process-group force-kill behavior.
fn spawn_process(config: &ClaudeCodeConfig, resume: Option<&str>) -> io::Result<ManagedChild> {
    let mut args = config.base_session_args();
    args.push(VERBOSE_FLAG.to_owned());
    if let Some(session_id) = resume {
        args.push("--resume".to_owned());
        args.push(session_id.to_owned());
    }

    let mut command = Command::new(config.binary());
    command.args(&args);
    if let Some(dir) = config.working_dir() {
        command.current_dir(dir);
    }
    for (key, value) in config.env() {
        command.env(key, value);
    }
    ManagedChild::spawn(
        command,
        ChildStdinMode::Piped,
        config.read_idle_timeout(),
        config.shutdown_grace(),
        "claude code stdout was not captured",
        "claude code read timed out",
    )
}

#[async_trait]
impl ClaudeSessionIo for ManagedChild {
    async fn write_frame(&mut self, frame: &str) -> io::Result<()> {
        self.write_line(frame, "claude code stdin is closed").await
    }

    async fn read_frame(&mut self) -> io::Result<Option<String>> {
        self.read_line().await
    }

    async fn close(&mut self) -> ExternalSessionShutdown {
        ManagedChild::close(self).await
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

        let prelude = PreludeDeadline::new(prelude_timeout);
        while self.decoder.session_id().is_none() {
            prelude.check_active(
                ctx,
                self.maybe_session_ref(),
                "claude code session begin was cancelled",
                claude_prelude_timeout_error,
            )?;
            let line = prelude
                .await_until(self.read_line(), claude_prelude_timeout_error)
                .await?;
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
        process::emit_observations(&observed, self.sink.as_ref(), &mut self.last_event_seq);
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
        process::maybe_session_ref_for_id(
            ExternalRuntimeKind::ClaudeCode,
            &self.session_id,
            self.last_event_seq,
        )
    }
}

#[async_trait]
impl<Io: ClaudeSessionIo> ExternalRuntimeSession for ClaudeCodeSession<Io> {
    fn session_ref(&self) -> ExternalSessionRef {
        process::session_ref_for_id(
            ExternalRuntimeKind::ClaudeCode,
            &self.session_id,
            self.last_event_seq,
        )
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
            // Race the frame read against cancellation (M3-1): a silent CLI
            // must not hold a cancel hostage until the per-line idle timeout
            // fires; the idle timeout stays armed inside `read_line` as the
            // last-resort error for a genuinely dead CLI. `biased` lets an
            // already-landed cancel win over a simultaneously ready frame.
            let line = tokio::select! {
                biased;
                () = ctx.cancellation().cancelled() => None,
                line = self.read_line() => Some(line?),
            };
            let Some(line) = line else {
                return Err(ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: "claude code session advance was cancelled".to_owned(),
                });
            };
            match line {
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
            capabilities: process::intersect_capabilities(&implemented_capabilities(), probed),
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
        process::reject_unsupported_tools(
            &self.capabilities,
            request,
            "claude code adapter cannot inject host tools without an MCP bridge",
        )
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
        let io = spawn_process(&config, None)
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
        let io = spawn_process(&config, Some(&session_id)).map_err(|error| {
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

/// Builds the classified timeout used by the fresh-start prelude deadline.
fn claude_prelude_timeout_error() -> ExternalAgentError {
    ExternalAgentError::Launch {
        runtime: ExternalRuntimeKind::ClaudeCode,
        detail: "claude code session did not report a session id within the launch timeout"
            .to_owned(),
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
mod tests;
