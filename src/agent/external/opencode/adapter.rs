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
//! - `OpenCodeSession` (private) is one live session ([`ExternalRuntimeSession`]).
//!   It owns a per-turn CLI process, feeds each stdout line to the decoder,
//!   mirrors observations to the live sink, and
//!   [`advance`](ExternalRuntimeSession::advance)s to the next
//!   [`RuntimeDecisionPoint`].
//!
//! # One process per turn
//!
//! Like `codex exec`, `opencode run` is **one-shot per turn**: the prompt is a
//! CLI positional argument (not a stdin frame), and the process exits when the
//! turn settles. A follow-up turn is a brand-new `opencode run --session
//! <session_id> <message>` process. `OpenCodeSession` therefore spawns a fresh
//! process for the first turn (in `begin`) and another for every follow-up turn
//! (in `advance`), threading a single decoder — whose `seq` spans the whole
//! session (design §5.5) — across all of them.
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
//! [`OpenCodeLauncher`] / [`OpenCodeTurnStream`] traits, not a
//! `tokio::process::Child` directly. Production uses [`SystemOpenCodeLauncher`],
//! which spawns the real CLI; the unit tests inject a fake launcher that replays
//! canned JSON lines and captures the [`OpenCodeTurnSpec`] of every turn, so the
//! whole start/advance/resume/shutdown state machine is exercised with no
//! OpenCode binary and no network. The real end-to-end coverage lives behind an
//! `#[ignore]` in `tests/external_opencode.rs`.

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
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::{Instant, timeout, timeout_at};

use crate::agent::{RunContext, TraceNodeId};

use crate::agent::external::process_group;
use crate::agent::external::{
    ExternalAgentError, ExternalCapability, ExternalEventSink, ExternalObservedEvent,
    ExternalRuntimeAdapter, ExternalRuntimeCapabilities, ExternalRuntimeKind,
    ExternalRuntimeSession, ExternalSessionInput, ExternalSessionRef, ExternalSessionRequest,
    ExternalSessionShutdown, RuntimeDecisionPoint,
};

use super::{OpenCodeConfig, OpenCodeDecision, OpenCodeDecodeContext, OpenCodeStreamDecoder};

/// The launch shape of one `opencode run` turn.
///
/// Each turn is a fresh CLI process: a [`Fresh`](Self::Fresh) turn starts a brand
/// new session (`opencode run … <prompt>`), while a [`Resume`](Self::Resume) turn
/// continues an existing session (`opencode run … --session <id> <message>`).
/// [`args`](Self::args) turns the spec into the full argument list by appending
/// the per-turn text to the config's frozen base arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OpenCodeTurnSpec {
    /// A brand-new session turn carrying the initial prompt.
    Fresh {
        /// The initial user prompt, appended as the `opencode run` positional arg.
        prompt: String,
    },
    /// A follow-up turn resuming `session_id` with a new user message.
    Resume {
        /// The runtime-assigned session id to resume.
        session_id: String,
        /// The follow-up user message, appended after the resume flags and a
        /// `--` separator.
        message: String,
    },
}

impl OpenCodeTurnSpec {
    /// Builds the full CLI argument list (after the binary) for this turn.
    ///
    /// A fresh turn reuses [`base_run_args`](OpenCodeConfig::base_run_args) and
    /// appends the prompt; a resume turn reuses
    /// [`base_resume_args`](OpenCodeConfig::base_resume_args) and appends the
    /// follow-up message after the `--session <id>` flag.
    ///
    /// The user-controlled text is always preceded by a `--` separator so a
    /// prompt that starts with `-` (for example `--model`) is parsed as the
    /// positional message instead of a flag. OpenCode's yargs entry point sets
    /// `parserConfiguration({ "populate--": true })` (sst/opencode
    /// `src/index.ts`), so post-`--` words populate the `message` positional
    /// array (confirmed against the installed CLI source, M2-4 / M-EXT-4).
    fn args(&self, config: &OpenCodeConfig) -> Vec<String> {
        match self {
            OpenCodeTurnSpec::Fresh { prompt } => {
                let mut args = config.base_run_args();
                args.push("--".to_owned());
                args.push(prompt.clone());
                args
            }
            OpenCodeTurnSpec::Resume {
                session_id,
                message,
            } => {
                let mut args = config.base_resume_args(session_id);
                args.push("--".to_owned());
                args.push(message.clone());
                args
            }
        }
    }
}

/// The stdout stream of one live `opencode run` turn process.
///
/// Splitting the raw IO behind this trait lets the session state machine
/// (`OpenCodeSession`) be unit-tested offline with a fake transport while
/// production drives a real CLI child through [`OpenCodeProcessTurn`]. Every frame
/// is one newline-delimited `opencode run --format json` object.
#[async_trait]
trait OpenCodeTurnStream: Send {
    /// Reads the next stdout frame line, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the read fails or times out.
    async fn read_frame(&mut self) -> io::Result<Option<String>>;

    /// Closes the turn's process and classifies how the close went.
    async fn close(&mut self) -> ExternalSessionShutdown;
}

/// Spawns one `opencode run` turn process.
///
/// Splitting the spawn behind this trait lets the session be exercised offline
/// with a fake launcher that captures each [`OpenCodeTurnSpec`]; production uses
/// [`SystemOpenCodeLauncher`], which spawns the real CLI.
#[async_trait]
trait OpenCodeLauncher: Send + Sync {
    /// Spawns a turn process for `spec` and returns its live stdout stream.
    ///
    /// # Errors
    ///
    /// Returns the raw [`io::Error`] from spawning (missing binary, permission
    /// denied); the caller classifies it into
    /// [`Launch`](ExternalAgentError::Launch) /
    /// [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable) /
    /// [`SessionLost`](ExternalAgentError::SessionLost) depending on the turn.
    async fn launch(&self, spec: &OpenCodeTurnSpec) -> io::Result<Box<dyn OpenCodeTurnStream>>;
}

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
        command
            .args(&args)
            // OpenCode reads a prompt from its positional argument; a piped or
            // inherited stdin would make `run` block reading a message from
            // stdin, so it is closed. stderr is discarded so no raw runtime text
            // (which could echo prompt or tool output) can leak into a
            // diagnostic — only the structured `--format json` stdout is consumed.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
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
        // The child leads its own process group on unix so a force-close can
        // signal the whole group, grandchildren included (H-EXT-2).
        process_group::configure_managed_command(&mut command);

        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "opencode run stdout was not captured",
            )
        })?;
        Ok(Box::new(OpenCodeProcessTurn {
            child,
            stdout: BufReader::new(stdout).lines(),
            read_timeout: self.config.read_idle_timeout(),
            shutdown_grace: self.config.shutdown_grace(),
        }))
    }
}

/// Production [`OpenCodeTurnStream`] backed by a real `tokio::process` child.
///
/// It pipes the CLI's stdout, kills the child on drop, bounds each read with
/// the configured read-idle timeout, and — on
/// [`close`](OpenCodeTurnStream::close) — waits for the one-shot process to
/// exit within the shutdown grace (a settled turn has already exited),
/// classifies the exit by status (zero → graceful, non-zero → failed), and on
/// overrun force-kills the child's whole process group (unix; the direct child
/// only on Windows) so CLI-spawned grandchildren cannot outlive the turn
/// (H-EXT-2).
struct OpenCodeProcessTurn {
    child: Child,
    stdout: Lines<BufReader<ChildStdout>>,
    read_timeout: Duration,
    shutdown_grace: Duration,
}

#[async_trait]
impl OpenCodeTurnStream for OpenCodeProcessTurn {
    async fn read_frame(&mut self) -> io::Result<Option<String>> {
        match timeout(self.read_timeout, self.stdout.next_line()).await {
            Ok(result) => result,
            Err(_elapsed) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "opencode run read timed out",
            )),
        }
    }

    async fn close(&mut self) -> ExternalSessionShutdown {
        // A one-shot turn process exits on its own once the turn settles, so a
        // graceful close usually just reaps it; a still-running turn is waited on
        // within the grace window and force-killed on overrun.
        match timeout(self.shutdown_grace, self.child.wait()).await {
            Ok(Ok(status)) if status.success() => ExternalSessionShutdown::Graceful,
            // A non-zero exit means the CLI turn failed, so its partial side
            // effects cannot be trusted as clean (H-EXT-3).
            Ok(Ok(_status)) => ExternalSessionShutdown::Failed,
            Ok(Err(_error)) => ExternalSessionShutdown::Failed,
            Err(_elapsed) => match process_group::force_kill(&mut self.child).await {
                Ok(()) => ExternalSessionShutdown::ForcedKill,
                Err(_error) => ExternalSessionShutdown::Failed,
            },
        }
    }
}

/// How the first turn of a session is launched, controlling how a spawn failure
/// and a missing session id are classified.
enum FirstLaunch {
    /// A fresh `start`: a spawn failure is a [`Launch`](ExternalAgentError::Launch).
    Fresh,
    /// A cross-process `resume`: a spawn failure is a
    /// [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable) naming the
    /// session being revived.
    Resume(ExternalSessionRef),
}

/// One live OpenCode session wrapping the private stream decoder.
///
/// The session owns a per-turn CLI process and a single [`OpenCodeStreamDecoder`]
/// whose `seq` line spans the whole session (design §5.5). The first turn is
/// launched by [`begin`](Self::begin) (which reads until the decoder captures the
/// runtime session id from the first `sessionID`-bearing frame); each follow-up
/// [`advance`](ExternalRuntimeSession::advance) launches a fresh `opencode run
/// --session` process, feeds its stdout to the decoder, mirrors observations to
/// the live sink, and returns at the decision the turn settles on.
struct OpenCodeSession<L: OpenCodeLauncher> {
    launcher: L,
    decoder: OpenCodeStreamDecoder,
    session_id: String,
    last_event_seq: Option<u64>,
    sink: Option<Arc<dyn ExternalEventSink>>,
    capabilities: ExternalRuntimeCapabilities,
    /// The stdout stream of the currently-open turn process, if any.
    current: Option<Box<dyn OpenCodeTurnStream>>,
    /// Observations buffered by the startup prelude, prepended to the first turn.
    carried: Vec<ExternalObservedEvent>,
    /// A decision reached during the prelude (defensive; the prelude normally
    /// only reads up to the first `sessionID`-bearing frame, which precedes any
    /// turn boundary, but a session that errors immediately settles here).
    carried_decision: Option<OpenCodeDecision>,
    /// Set when `begin` already launched the first turn's process (OpenCode takes
    /// the prompt as a launch argument), so the first `advance` — which carries
    /// the same input — continues that in-flight turn instead of spawning another.
    first_turn_pending: bool,
    /// The most severe disposition seen closing a mid-session turn process,
    /// folded into [`shutdown`](ExternalRuntimeSession::shutdown) so a
    /// force-killed turn still marks the session as leaving residual side
    /// effects (review M-EXT-5).
    worst_close: Option<ExternalSessionShutdown>,
    /// Per-session counter for minting trace node ids of mid-session closes.
    close_trace_seq: u64,
}

impl<L: OpenCodeLauncher> OpenCodeSession<L> {
    /// Builds a session over `launcher`, binding the decode context and the
    /// capability set the adapter reports.
    fn new(
        launcher: L,
        context: OpenCodeDecodeContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
        capabilities: ExternalRuntimeCapabilities,
    ) -> Self {
        Self {
            launcher,
            decoder: OpenCodeStreamDecoder::new(context),
            session_id: String::new(),
            last_event_seq: None,
            sink,
            capabilities,
            current: None,
            carried: Vec::new(),
            carried_decision: None,
            first_turn_pending: false,
            worst_close: None,
            close_trace_seq: 0,
        }
    }

    /// Seeds the session from the persisted high-water mark of a resumed
    /// session.
    ///
    /// Continues the decoder's `seq` line past `high_water` and restores the
    /// session's own water mark so [`session_ref`](ExternalRuntimeSession::session_ref)
    /// never reports a regressed `last_event_seq`. See
    /// [`OpenCodeStreamDecoder::with_next_seq`] for why a resume must not
    /// restart the seq line at 0.
    #[must_use]
    fn with_resume_high_water(mut self, high_water: Option<u64>) -> Self {
        if let Some(high_water) = high_water {
            self.decoder = self.decoder.with_next_seq(high_water.saturating_add(1));
            self.last_event_seq = Some(high_water);
        }
        self
    }

    /// Launches the first turn's process and reads its prelude until the decoder
    /// captures the runtime session id.
    ///
    /// OpenCode has no dedicated init frame: the session id rides on the
    /// `sessionID` field of every mirrored event, so the prelude reads frames
    /// until the decoder captures it (which also emits the single
    /// `SessionStarted` observation) before the rest of the turn — text,
    /// completion — is deferred to the first
    /// [`advance`](ExternalRuntimeSession::advance). A resume already knows its id
    /// from the persisted [`ExternalSessionRef`], so that id is pre-seeded and the
    /// prelude only refreshes it.
    ///
    /// The prelude is bounded twice (review M-EXT-6): the whole loop must finish
    /// within `prelude_timeout` (the config's launch timeout — the per-line
    /// read-idle timeout resets every line, so a CLI babbling non-init frames
    /// would otherwise loop forever), and every iteration honours
    /// `ctx.is_cancelled()` like the `advance` loop does.
    ///
    /// # Errors
    ///
    /// Returns [`Launch`](ExternalAgentError::Launch) /
    /// [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable) when the turn
    /// process cannot be spawned or the prelude misses its launch deadline,
    /// [`Protocol`](ExternalAgentError::Protocol) for a corrupt prelude frame,
    /// [`SessionLost`](ExternalAgentError::SessionLost) on a read failure or a
    /// cancellation, or `Launch` when a fresh session never reports a session
    /// id.
    async fn begin(
        &mut self,
        spec: &OpenCodeTurnSpec,
        first: FirstLaunch,
        ctx: &RunContext,
        prelude_timeout: Duration,
    ) -> Result<(), ExternalAgentError> {
        let stream = self
            .launcher
            .launch(spec)
            .await
            .map_err(|error| match &first {
                FirstLaunch::Fresh => ExternalAgentError::Launch {
                    runtime: ExternalRuntimeKind::OpenCode,
                    detail: format!("spawning opencode run failed: {:?}", error.kind()),
                },
                FirstLaunch::Resume(session) => ExternalAgentError::ResumeUnavailable {
                    session: session.clone(),
                    detail: format!("failed spawning opencode run --session: {:?}", error.kind()),
                },
            })?;
        self.current = Some(stream);
        self.first_turn_pending = true;

        // A resume already knows its id, so pre-seed it: the session must expose a
        // non-empty id to be registered even if the resumed session never
        // re-reports its `sessionID` before settling.
        if let FirstLaunch::Resume(session) = &first
            && let Some(id) = &session.session_id
        {
            self.session_id = id.clone();
        }

        let deadline = Instant::now() + prelude_timeout;
        // Fresh vs resume classification axis, shared by both deadline guards.
        let deadline_error = || match &first {
            FirstLaunch::Fresh => ExternalAgentError::Launch {
                runtime: ExternalRuntimeKind::OpenCode,
                detail: "opencode run did not report a session id within the launch timeout"
                    .to_owned(),
            },
            FirstLaunch::Resume(session) => ExternalAgentError::ResumeUnavailable {
                session: session.clone(),
                detail:
                    "resumed opencode run did not report a session id within the launch timeout"
                        .to_owned(),
            },
        };
        while self.decoder.session_id().is_none() {
            if ctx.is_cancelled() {
                return Err(ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: "opencode session begin was cancelled".to_owned(),
                });
            }
            // The `timeout_at` below only fires while the runtime is polled; a
            // transport whose reads resolve instantly would starve the timer, so
            // the deadline is also enforced by this explicit wall-clock check.
            if Instant::now() >= deadline {
                return Err(deadline_error());
            }
            let line = match timeout_at(deadline, self.read_line()).await {
                Ok(result) => result?,
                Err(_elapsed) => return Err(deadline_error()),
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

        if let Some(id) = self.decoder.session_id() {
            self.session_id = id.to_owned();
        }
        if self.session_id.is_empty() {
            // Only reachable for a fresh start; a resume pre-seeds its id above.
            return Err(ExternalAgentError::Launch {
                runtime: ExternalRuntimeKind::OpenCode,
                detail: "opencode run did not report a session id".to_owned(),
            });
        }
        Ok(())
    }

    /// Reads one stdout frame from the current turn, classifying a read failure
    /// as [`SessionLost`](ExternalAgentError::SessionLost).
    ///
    /// A read with no open turn process is itself a lost session.
    async fn read_line(&mut self) -> Result<Option<String>, ExternalAgentError> {
        let Some(stream) = self.current.as_mut() else {
            return Err(ExternalAgentError::SessionLost {
                session: self.maybe_session_ref(),
                detail: "opencode session has no open turn to read".to_owned(),
            });
        };
        stream
            .read_frame()
            .await
            .map_err(|error| ExternalAgentError::SessionLost {
                session: self.maybe_session_ref(),
                detail: format!("failed reading opencode run stream: {:?}", error.kind()),
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

    /// Records the disposition of a mid-session turn-process close.
    ///
    /// The close is written to the trace (best effort — a trace hiccup never
    /// masks the advance) and folded into [`worst_close`](Self::worst_close), so
    /// the session's final [`shutdown`](ExternalRuntimeSession::shutdown) still
    /// reports residual side effects when an earlier turn had to be force-killed
    /// (review M-EXT-5, design §6.4). The trace node id is minted from the run id
    /// plus a per-session counter (the crate mints no other ids), keeping it
    /// deterministic and collision-free.
    fn note_close(&mut self, ctx: &RunContext, disposition: ExternalSessionShutdown) {
        let seq = self.close_trace_seq;
        self.close_trace_seq += 1;
        let id = TraceNodeId::new(format!("external-shutdown/{}/{seq}", ctx.run_id()));
        let _ = ctx.trace().record_external_shutdown(id, disposition);
        self.worst_close = Some(match self.worst_close {
            Some(worst) => worst.merge(disposition),
            None => disposition,
        });
    }

    /// Spawns the follow-up turn process for `input`, or refuses an unsupported
    /// one.
    ///
    /// The previous (already-exited) turn process is closed before the new one is
    /// spawned so no zombie lingers between turns. That close's disposition is
    /// **not** dropped (review M-EXT-5): it is recorded to the trace and folded
    /// into [`worst_close`](Self::worst_close), so a force-killed turn process
    /// still marks the session as leaving residual side effects at
    /// [`shutdown`](ExternalRuntimeSession::shutdown).
    ///
    /// # Errors
    ///
    /// Returns [`UnsupportedCapability`](ExternalAgentError::UnsupportedCapability)
    /// for a tool/subagent/interaction response this adapter cannot bridge,
    /// [`Protocol`](ExternalAgentError::Protocol) for a shutdown misrouted through
    /// `advance`, or [`SessionLost`](ExternalAgentError::SessionLost) when the new
    /// turn process cannot be spawned.
    async fn spawn_follow_up_turn(
        &mut self,
        input: &ExternalSessionInput,
        ctx: &RunContext,
    ) -> Result<(), ExternalAgentError> {
        let message = turn_message(&self.capabilities, input)?;

        if let Some(mut old) = self.current.take() {
            let disposition = old.close().await;
            self.note_close(ctx, disposition);
        }

        let spec = OpenCodeTurnSpec::Resume {
            session_id: self.session_id.clone(),
            message,
        };
        let stream =
            self.launcher
                .launch(&spec)
                .await
                .map_err(|error| ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: format!("failed spawning opencode run --session: {:?}", error.kind()),
                })?;
        self.current = Some(stream);
        Ok(())
    }

    /// Folds a settled [`OpenCodeDecision`] into a [`RuntimeDecisionPoint`].
    ///
    /// `opencode run --format json` never pauses for the host, so a turn only
    /// ever completes or fails (M8-2).
    fn finish(
        &self,
        decision: OpenCodeDecision,
        observations: Vec<ExternalObservedEvent>,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        match decision {
            OpenCodeDecision::Completed { output } => Ok(RuntimeDecisionPoint::Completed {
                session: self.session_ref(),
                output,
                observations,
            }),
            OpenCodeDecision::Failed { error } => Err(error),
        }
    }

    /// Returns the session facts, or `None` before a session id has been assigned.
    fn maybe_session_ref(&self) -> Option<ExternalSessionRef> {
        if self.session_id.is_empty() {
            None
        } else {
            Some(self.session_ref())
        }
    }
}

#[async_trait]
impl<L: OpenCodeLauncher> ExternalRuntimeSession for OpenCodeSession<L> {
    fn session_ref(&self) -> ExternalSessionRef {
        let session_id = (!self.session_id.is_empty()).then(|| self.session_id.clone());
        ExternalSessionRef {
            runtime: ExternalRuntimeKind::OpenCode,
            session_id: session_id.clone(),
            transcript_ref: None,
            // OpenCode resumes by session id (`run --session <id>`), so it doubles
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

        // A fresh `start` / `resume` already launched and began this turn, so the
        // first advance (carrying the same input) continues the in-flight turn
        // instead of spawning another process for it.
        let already_spawned = std::mem::take(&mut self.first_turn_pending);

        // A decision reached during the startup prelude settles the turn without
        // further IO (defensive: a session that errored before producing content).
        if let Some(decision) = self.carried_decision.take() {
            return self.finish(decision, collected);
        }

        if !already_spawned {
            self.spawn_follow_up_turn(input, ctx).await?;
        }

        loop {
            if ctx.is_cancelled() {
                return Err(ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: "opencode session advance was cancelled".to_owned(),
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
                        detail: "opencode session closed before reaching a decision point"
                            .to_owned(),
                    });
                }
            }
        }
    }

    async fn shutdown(&mut self) -> ExternalSessionShutdown {
        let current = match self.current.as_mut() {
            Some(stream) => stream.close().await,
            // Between turns the session holds no live process, so there is nothing
            // to close.
            None => ExternalSessionShutdown::Graceful,
        };
        // A mid-session turn process that had to be force-killed (or failed to
        // close) marks the whole session as leaving residual side effects, even
        // when the final close itself was clean (review M-EXT-5).
        match self.worst_close {
            Some(worst) => worst.merge(current),
            None => current,
        }
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
            capabilities: intersect_capabilities(&implemented_capabilities(), probed),
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
        if !request.tools.is_empty() && !self.capabilities.host_tools {
            return Err(self.capabilities.unsupported(
                ExternalCapability::HostTools,
                "opencode adapter cannot inject host tools; opencode run executes autonomously",
            ));
        }
        Ok(())
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
    match input {
        ExternalSessionInput::Start { prompt } => Ok(prompt.clone()),
        ExternalSessionInput::Continue { message } => Ok(message.clone()),
        ExternalSessionInput::RespondInteraction { .. } => Err(capabilities.unsupported(
            ExternalCapability::PermissionBridge,
            "opencode run executes autonomously; there is no host-answerable interaction to resolve",
        )),
        ExternalSessionInput::RespondToolResults { .. } => Err(capabilities.unsupported(
            ExternalCapability::HostTools,
            "opencode adapter does not bridge host tool results",
        )),
        ExternalSessionInput::RespondSubagent { .. } => Err(capabilities.unsupported(
            ExternalCapability::HostSubagents,
            "opencode adapter does not bridge host subagents",
        )),
        ExternalSessionInput::Shutdown => Err(ExternalAgentError::Protocol {
            detail: "opencode session shutdown must go through shutdown(), not advance".to_owned(),
        }),
    }
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

#[cfg(test)]
mod tests {
    use super::{
        FirstLaunch, OpenCodeAdapter, OpenCodeLauncher, OpenCodeSession, OpenCodeTurnSpec,
        OpenCodeTurnStream, implemented_capabilities, intersect_capabilities, turn_message,
    };
    use crate::agent::external::OpenCodeConfig;
    use crate::agent::external::{
        ExternalAgentError, ExternalCapability, ExternalEventSink, ExternalObservedEvent,
        ExternalPermissionMode, ExternalRuntimeAdapter, ExternalRuntimeCapabilities,
        ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionInput, ExternalSessionPolicy,
        ExternalSessionRef, ExternalSessionRequest, ExternalSessionShutdown, ExternalStreamPolicy,
        ExternalToolBatchId, RuntimeDecisionPoint, WorktreeIsolation,
    };
    use crate::agent::spec::WorktreeRef;
    use crate::agent::{AgentId, BudgetLimits, RunContext, RunId, TraceNodeId, TraceNodeKind};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    const SESSION_ID: &str = "ses_8b1f7a2c9d3e4f50";
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
            TraceNodeId::new("opencode-adapter-test"),
        )
    }

    fn policy() -> ExternalSessionPolicy {
        ExternalSessionPolicy {
            permission_mode: ExternalPermissionMode::AcceptEdits,
            isolation: WorktreeIsolation::EphemeralGitWorktree,
            max_turns: Some(8),
            stream_events: ExternalStreamPolicy::Streaming,
        }
    }

    fn start_request(tools: Vec<crate::model::tool::Tool>) -> ExternalSessionRequest {
        ExternalSessionRequest {
            agent_id: agent_id(),
            runtime: ExternalRuntimeKind::OpenCode,
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

    fn resume_ref() -> ExternalSessionRef {
        ExternalSessionRef {
            runtime: ExternalRuntimeKind::OpenCode,
            session_id: Some(SESSION_ID.to_owned()),
            transcript_ref: None,
            resume_token: Some(SESSION_ID.to_owned()),
            last_event_seq: Some(3),
        }
    }

    /// A `step_start` boundary frame carrying the runtime session id — the first
    /// frame a real `opencode run` process emits, from which the decoder captures
    /// the id and announces the session.
    fn step_start(session_id: &str) -> String {
        format!(
            r#"{{"type":"step_start","sessionID":"{session_id}","part":{{"type":"step-start","sessionID":"{session_id}"}}}}"#
        )
    }

    fn text(session_id: &str, body: &str) -> String {
        format!(
            r#"{{"type":"text","sessionID":"{session_id}","part":{{"type":"text","text":"{body}","time":{{"end":1}}}}}}"#
        )
    }

    fn step_finish_stop(session_id: &str) -> String {
        format!(
            r#"{{"type":"step_finish","sessionID":"{session_id}","part":{{"type":"step-finish","reason":"stop","cost":0.001,"tokens":{{"input":10,"output":5,"reasoning":0,"cache":{{"read":0,"write":0}}}}}}}}"#
        )
    }

    fn error_frame(session_id: &str) -> String {
        format!(
            r#"{{"type":"error","sessionID":"{session_id}","error":{{"name":"ProviderError","data":{{"message":"boom"}}}}}}"#
        )
    }

    /// A fake turn stream replaying canned stdout lines.
    struct FakeTurn {
        lines: VecDeque<String>,
        close_disposition: ExternalSessionShutdown,
        /// Line replayed forever once `lines` drains (prelude-deadline tests).
        repeat: Option<String>,
    }

    #[async_trait]
    impl OpenCodeTurnStream for FakeTurn {
        async fn read_frame(&mut self) -> std::io::Result<Option<String>> {
            match self.lines.pop_front() {
                Some(line) => Ok(Some(line)),
                None => Ok(self.repeat.clone()),
            }
        }

        async fn close(&mut self) -> ExternalSessionShutdown {
            self.close_disposition
        }
    }

    /// Shared recorder of the specs a [`FakeLauncher`] was asked to launch.
    type RecordedSpecs = Arc<Mutex<Vec<OpenCodeTurnSpec>>>;

    /// A fake launcher popping one canned turn per launch and recording specs.
    struct FakeLauncher {
        turns: Mutex<VecDeque<Vec<String>>>,
        specs: RecordedSpecs,
        /// Close disposition for turns without a queued per-turn entry.
        default_close: ExternalSessionShutdown,
        /// Per-turn close dispositions, popped one per launch.
        close_sequence: Mutex<VecDeque<ExternalSessionShutdown>>,
        /// Line replayed forever by every spawned turn (prelude-deadline tests).
        repeat: Option<String>,
        fail_kind: Option<std::io::ErrorKind>,
    }

    impl FakeLauncher {
        fn new(turns: Vec<Vec<String>>) -> Self {
            Self {
                turns: Mutex::new(turns.into_iter().collect()),
                specs: Arc::new(Mutex::new(Vec::new())),
                default_close: ExternalSessionShutdown::Graceful,
                close_sequence: Mutex::new(VecDeque::new()),
                repeat: None,
                fail_kind: None,
            }
        }

        fn recorded_specs(&self) -> RecordedSpecs {
            Arc::clone(&self.specs)
        }

        /// Closes every spawned turn with `disposition`.
        fn with_close(mut self, disposition: ExternalSessionShutdown) -> Self {
            self.default_close = disposition;
            self
        }

        /// Closes the Nth spawned turn with the Nth entry (later turns fall back
        /// to the default).
        fn with_close_sequence(self, dispositions: &[ExternalSessionShutdown]) -> Self {
            *self.close_sequence.lock().unwrap() = dispositions.iter().copied().collect();
            self
        }

        /// Every spawned turn replays `line` forever once its canned lines drain.
        fn repeating(mut self, line: String) -> Self {
            self.repeat = Some(line);
            self
        }

        fn failing(kind: std::io::ErrorKind) -> Self {
            let mut launcher = Self::new(Vec::new());
            launcher.fail_kind = Some(kind);
            launcher
        }
    }

    #[async_trait]
    impl OpenCodeLauncher for FakeLauncher {
        async fn launch(
            &self,
            spec: &OpenCodeTurnSpec,
        ) -> std::io::Result<Box<dyn OpenCodeTurnStream>> {
            self.specs.lock().unwrap().push(spec.clone());
            if let Some(kind) = self.fail_kind {
                return Err(std::io::Error::new(kind, "fake launch failure"));
            }
            let lines = self.turns.lock().unwrap().pop_front().unwrap_or_default();
            let close_disposition = self
                .close_sequence
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(self.default_close);
            Ok(Box::new(FakeTurn {
                lines: lines.into_iter().collect(),
                close_disposition,
                repeat: self.repeat.clone(),
            }))
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
        launcher: FakeLauncher,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> OpenCodeSession<FakeLauncher> {
        let context =
            OpenCodeAdapter::decode_context(&OpenCodeConfig::new(), &start_request(Vec::new()));
        OpenCodeSession::new(launcher, context, sink, implemented_capabilities())
    }

    fn fresh_spec(prompt: &str) -> OpenCodeTurnSpec {
        OpenCodeTurnSpec::Fresh {
            prompt: prompt.to_owned(),
        }
    }

    #[tokio::test]
    async fn opencode_adapter_advance_drives_text_and_completion() {
        let sink = Arc::new(RecordingSink::default());
        let launcher = FakeLauncher::new(vec![vec![
            step_start(SESSION_ID),
            text(SESSION_ID, "looking into it"),
            step_finish_stop(SESSION_ID),
        ]]);
        let specs = launcher.recorded_specs();
        let mut session = session_over(
            launcher,
            Some(Arc::clone(&sink) as Arc<dyn ExternalEventSink>),
        );

        session
            .begin(
                &fresh_spec("investigate the failing test"),
                FirstLaunch::Fresh,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("begin launches the first turn and captures the session id");
        assert_eq!(
            session.session_ref().session_id.as_deref(),
            Some(SESSION_ID)
        );

        let ctx = run_context();
        let decision = session
            .advance(&start_request(Vec::new()).input, &ctx)
            .await
            .expect("first advance settles the turn");
        match decision {
            RuntimeDecisionPoint::Completed {
                output,
                observations,
                session,
            } => {
                // The carried SessionStarted plus the TextDelta plus the
                // SessionCompleted all ride the first decision point.
                assert!(observations.len() >= 3, "prelude + turn observations");
                assert_eq!(output.summary, "looking into it");
                assert!(output.usage.is_some());
                assert_eq!(session.session_id.as_deref(), Some(SESSION_ID));
            }
            other => panic!("expected completion, got {other:?}"),
        }

        // Exactly one process was launched, a Fresh turn carrying the prompt.
        let recorded = specs.lock().unwrap().clone();
        assert_eq!(recorded, vec![fresh_spec("investigate the failing test")]);

        // The sink saw the same sequenced observations, monotonically.
        let seqs: Vec<u64> = sink.events.lock().unwrap().iter().map(|e| e.seq).collect();
        assert!(seqs.len() >= 3, "streamed at least three observations");
        assert!(
            seqs.windows(2).all(|w| w[0] < w[1]),
            "seq is monotonic: {seqs:?}"
        );
    }

    #[tokio::test]
    async fn opencode_adapter_follow_up_turn_resumes_with_session_id() {
        let launcher = FakeLauncher::new(vec![
            vec![
                step_start(SESSION_ID),
                text(SESSION_ID, "first"),
                step_finish_stop(SESSION_ID),
            ],
            vec![
                step_start(SESSION_ID),
                text(SESSION_ID, "second"),
                step_finish_stop(SESSION_ID),
            ],
        ]);
        let specs = launcher.recorded_specs();
        let mut session = session_over(launcher, None);
        session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("begin");

        let ctx = run_context();
        let first = session
            .advance(&start_request(Vec::new()).input, &ctx)
            .await
            .expect("first completion");
        assert!(matches!(first, RuntimeDecisionPoint::Completed { .. }));

        // A follow-up turn spawns a fresh `run --session` process for the session.
        let follow_up = ExternalSessionInput::Continue {
            message: "keep going".to_owned(),
        };
        let second = session
            .advance(&follow_up, &ctx)
            .await
            .expect("second completion");
        match second {
            RuntimeDecisionPoint::Completed { output, .. } => {
                assert_eq!(output.summary, "second");
            }
            other => panic!("expected completion, got {other:?}"),
        }

        let recorded = specs.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                fresh_spec("start"),
                OpenCodeTurnSpec::Resume {
                    session_id: SESSION_ID.to_owned(),
                    message: "keep going".to_owned(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn opencode_adapter_advance_reports_session_lost_on_early_eof() {
        let launcher = FakeLauncher::new(vec![vec![step_start(SESSION_ID)]]);
        let mut session = session_over(launcher, None);
        session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("begin");

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
    async fn opencode_adapter_advance_propagates_protocol_error_on_malformed_frame() {
        let launcher =
            FakeLauncher::new(vec![vec![step_start(SESSION_ID), "{ not json".to_owned()]]);
        let mut session = session_over(launcher, None);
        session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("begin");

        let ctx = run_context();
        let error = session
            .advance(&start_request(Vec::new()).input, &ctx)
            .await
            .expect_err("a corrupt frame is a protocol error");
        assert!(matches!(error, ExternalAgentError::Protocol { .. }));
    }

    #[tokio::test]
    async fn opencode_adapter_advance_propagates_turn_failed() {
        let launcher =
            FakeLauncher::new(vec![vec![step_start(SESSION_ID), error_frame(SESSION_ID)]]);
        let mut session = session_over(launcher, None);
        session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("begin");

        let ctx = run_context();
        let error = session
            .advance(&start_request(Vec::new()).input, &ctx)
            .await
            .expect_err("an error frame fails the turn");
        assert!(matches!(error, ExternalAgentError::Runtime { .. }));
    }

    #[tokio::test]
    async fn opencode_adapter_shutdown_classifies_the_close() {
        let launcher = FakeLauncher::new(vec![vec![step_start(SESSION_ID)]])
            .with_close(ExternalSessionShutdown::ForcedKill);
        let mut session = session_over(launcher, None);
        session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("begin");
        assert_eq!(
            session.shutdown().await,
            ExternalSessionShutdown::ForcedKill
        );
    }

    #[tokio::test]
    async fn opencode_adapter_begin_times_out_when_session_id_never_arrives() {
        // A CLI babbling tolerated frames that carry no `sessionID` would loop
        // the prelude forever on the per-line read timeout alone (each line
        // resets it); the launch deadline caps the whole prelude (review
        // M-EXT-6).
        let launcher = FakeLauncher::new(Vec::new()).repeating(r#"{"type":"ping"}"#.to_owned());
        let mut session = session_over(launcher, None);
        let started = std::time::Instant::now();
        let error = session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
                &run_context(),
                std::time::Duration::from_millis(50),
            )
            .await
            .expect_err("a prelude that never reports a session id hits the launch deadline");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the prelude deadline fires promptly"
        );
        match error {
            ExternalAgentError::Launch { runtime, detail } => {
                assert_eq!(runtime, ExternalRuntimeKind::OpenCode);
                assert!(detail.contains("launch timeout"), "detail: {detail}");
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn opencode_adapter_begin_resume_times_out_when_session_id_never_arrives() {
        // The same prelude deadline on the resume path is classified as
        // `ResumeUnavailable`, matching the spawn-failure classification axis.
        let launcher = FakeLauncher::new(Vec::new()).repeating(r#"{"type":"ping"}"#.to_owned());
        let mut session = session_over(launcher, None);
        let spec = OpenCodeTurnSpec::Resume {
            session_id: SESSION_ID.to_owned(),
            message: "continue".to_owned(),
        };
        let error = session
            .begin(
                &spec,
                FirstLaunch::Resume(resume_ref()),
                &run_context(),
                std::time::Duration::from_millis(50),
            )
            .await
            .expect_err("a resumed prelude that never re-reports its id hits the deadline");
        match error {
            ExternalAgentError::ResumeUnavailable { session, detail } => {
                assert_eq!(session.session_id.as_deref(), Some(SESSION_ID));
                assert!(detail.contains("launch timeout"), "detail: {detail}");
            }
            other => panic!("expected ResumeUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn opencode_adapter_begin_honours_cancellation() {
        // The prelude checks `ctx.is_cancelled()` per iteration, just like the
        // advance loop (review M-EXT-6).
        let launcher = FakeLauncher::new(vec![vec![step_start(SESSION_ID)]]);
        let mut session = session_over(launcher, None);
        let ctx = run_context();
        ctx.cancellation().cancel();
        let error = session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
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
    async fn opencode_adapter_mid_turn_close_is_traced_and_marks_the_session_dirty() {
        // Turn 1's process has to be force-killed when turn 2 spawns; that
        // disposition must reach the trace and the session's final shutdown
        // report instead of being dropped (review M-EXT-5).
        let launcher = FakeLauncher::new(vec![
            vec![
                step_start(SESSION_ID),
                text(SESSION_ID, "first"),
                step_finish_stop(SESSION_ID),
            ],
            vec![
                step_start(SESSION_ID),
                text(SESSION_ID, "second"),
                step_finish_stop(SESSION_ID),
            ],
        ])
        .with_close_sequence(&[
            ExternalSessionShutdown::ForcedKill,
            ExternalSessionShutdown::Graceful,
        ]);
        let mut session = session_over(launcher, None);
        session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("begin");

        let ctx = run_context();
        session
            .advance(&start_request(Vec::new()).input, &ctx)
            .await
            .expect("first completion");
        let follow_up = ExternalSessionInput::Continue {
            message: "keep going".to_owned(),
        };
        session
            .advance(&follow_up, &ctx)
            .await
            .expect("second completion");

        // The mid-turn close disposition was recorded to the trace.
        let shutdowns: Vec<TraceNodeKind> = ctx
            .trace()
            .records()
            .into_iter()
            .map(|record| record.kind())
            .filter(|kind| matches!(kind, TraceNodeKind::ExternalShutdown { .. }))
            .collect();
        assert_eq!(
            shutdowns,
            vec![TraceNodeKind::ExternalShutdown {
                disposition: ExternalSessionShutdown::ForcedKill,
            }],
            "the force-killed turn process is traced"
        );

        // ...and folded into the final shutdown even though turn 2's own close
        // was graceful, so the worktree is judged as potentially dirty.
        assert_eq!(
            session.shutdown().await,
            ExternalSessionShutdown::ForcedKill
        );
    }

    #[tokio::test]
    async fn opencode_adapter_resume_defers_and_records_session_id() {
        let launcher = FakeLauncher::new(vec![vec![
            step_start(SESSION_ID),
            text(SESSION_ID, "resumed"),
            step_finish_stop(SESSION_ID),
        ]]);
        let specs = launcher.recorded_specs();
        let mut session = session_over(launcher, None);

        let spec = OpenCodeTurnSpec::Resume {
            session_id: SESSION_ID.to_owned(),
            message: "continue".to_owned(),
        };
        session
            .begin(
                &spec,
                FirstLaunch::Resume(resume_ref()),
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("resume begin");
        assert_eq!(
            session.session_ref().session_id.as_deref(),
            Some(SESSION_ID)
        );

        let ctx = run_context();
        let follow_up = ExternalSessionInput::Continue {
            message: "continue".to_owned(),
        };
        let decision = session.advance(&follow_up, &ctx).await.expect("completion");
        assert!(matches!(decision, RuntimeDecisionPoint::Completed { .. }));

        // The one recorded spec is the resume turn carrying the session id.
        let recorded = specs.lock().unwrap().clone();
        assert_eq!(recorded, vec![spec]);
    }

    #[tokio::test]
    async fn opencode_adapter_resume_continues_the_seq_line_past_the_high_water() {
        // A resume must continue the decoder's seq line past the persisted
        // `last_event_seq`: restarting at 0 would let the machine's replay dedup
        // silently drop every post-resume observation (design §5.5, review
        // M-EXT-1).
        let launcher = FakeLauncher::new(vec![vec![
            step_start(SESSION_ID),
            text(SESSION_ID, "resumed"),
            step_finish_stop(SESSION_ID),
        ]]);
        let mut session = session_over(launcher, None).with_resume_high_water(Some(50));

        let spec = OpenCodeTurnSpec::Resume {
            session_id: SESSION_ID.to_owned(),
            message: "continue".to_owned(),
        };
        session
            .begin(
                &spec,
                FirstLaunch::Resume(resume_ref()),
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("resume begin");
        // The prelude already emitted its first observations past the mark.
        assert!(
            session.session_ref().last_event_seq >= Some(50),
            "the reported water mark never regresses below the persisted one"
        );

        let ctx = run_context();
        let follow_up = ExternalSessionInput::Continue {
            message: "continue".to_owned(),
        };
        let decision = session.advance(&follow_up, &ctx).await.expect("completion");
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

    #[tokio::test]
    async fn opencode_adapter_resume_survives_a_session_that_never_re_reports_its_id() {
        // A resumed process whose only frame settles the turn without re-reporting
        // a `sessionID` still exposes the pre-seeded id and completes.
        let launcher = FakeLauncher::new(vec![vec![
            r#"{"type":"step_finish","part":{"type":"step-finish","reason":"stop"}}"#.to_owned(),
        ]]);
        let mut session = session_over(launcher, None);

        let spec = OpenCodeTurnSpec::Resume {
            session_id: SESSION_ID.to_owned(),
            message: "continue".to_owned(),
        };
        session
            .begin(
                &spec,
                FirstLaunch::Resume(resume_ref()),
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("resume begin pre-seeds the id");
        assert_eq!(
            session.session_ref().session_id.as_deref(),
            Some(SESSION_ID)
        );

        let ctx = run_context();
        let decision = session
            .advance(
                &ExternalSessionInput::Continue {
                    message: "continue".to_owned(),
                },
                &ctx,
            )
            .await
            .expect("completion");
        assert!(matches!(decision, RuntimeDecisionPoint::Completed { .. }));
    }

    #[tokio::test]
    async fn opencode_adapter_follow_up_respond_tool_results_is_unsupported() {
        let launcher = FakeLauncher::new(vec![vec![
            step_start(SESSION_ID),
            text(SESSION_ID, "done"),
            step_finish_stop(SESSION_ID),
        ]]);
        let mut session = session_over(launcher, None);
        session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect("begin");

        let ctx = run_context();
        let first = session
            .advance(&start_request(Vec::new()).input, &ctx)
            .await
            .expect("first completion");
        assert!(matches!(first, RuntimeDecisionPoint::Completed { .. }));

        let input = ExternalSessionInput::RespondToolResults {
            batch_id: ExternalToolBatchId::new("batch-1"),
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
    async fn opencode_adapter_begin_reports_launch_failure() {
        let launcher = FakeLauncher::failing(std::io::ErrorKind::NotFound);
        let mut session = session_over(launcher, None);
        let error = session
            .begin(
                &fresh_spec("start"),
                FirstLaunch::Fresh,
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect_err("a spawn failure is a launch error");
        assert!(matches!(
            error,
            ExternalAgentError::Launch {
                runtime: ExternalRuntimeKind::OpenCode,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn opencode_adapter_begin_reports_resume_failure() {
        let launcher = FakeLauncher::failing(std::io::ErrorKind::NotFound);
        let mut session = session_over(launcher, None);
        let spec = OpenCodeTurnSpec::Resume {
            session_id: SESSION_ID.to_owned(),
            message: "continue".to_owned(),
        };
        let error = session
            .begin(
                &spec,
                FirstLaunch::Resume(resume_ref()),
                &run_context(),
                PRELUDE_TIMEOUT,
            )
            .await
            .expect_err("a resume spawn failure is unavailable");
        assert!(matches!(
            error,
            ExternalAgentError::ResumeUnavailable { .. }
        ));
    }

    #[test]
    fn opencode_adapter_turn_message_maps_inputs_and_refusals() {
        let caps = implemented_capabilities();
        assert_eq!(
            turn_message(
                &caps,
                &ExternalSessionInput::Start {
                    prompt: "p".to_owned()
                }
            )
            .expect("start text"),
            "p"
        );
        assert_eq!(
            turn_message(
                &caps,
                &ExternalSessionInput::Continue {
                    message: "m".to_owned()
                }
            )
            .expect("continue text"),
            "m"
        );
        assert!(matches!(
            turn_message(
                &caps,
                &ExternalSessionInput::RespondInteraction {
                    action_id: "a".to_owned(),
                    response: crate::agent::interaction::InteractionResponse::Answer(
                        "yes".to_owned()
                    ),
                }
            ),
            Err(ExternalAgentError::UnsupportedCapability {
                capability: ExternalCapability::PermissionBridge,
                ..
            })
        ));
        assert!(matches!(
            turn_message(
                &caps,
                &ExternalSessionInput::RespondSubagent {
                    request_id: crate::agent::external::ExternalSubagentRequestId::new("req-1"),
                    output: crate::agent::external::ExternalSubagentOutput {
                        summary: "done".to_owned(),
                        raw: None,
                    },
                }
            ),
            Err(ExternalAgentError::UnsupportedCapability {
                capability: ExternalCapability::HostSubagents,
                ..
            })
        ));
        assert!(matches!(
            turn_message(&caps, &ExternalSessionInput::Shutdown),
            Err(ExternalAgentError::Protocol { .. })
        ));
    }

    #[test]
    fn opencode_turn_spec_appends_prompt_and_message_to_base_args() {
        let config =
            OpenCodeConfig::new().with_permission_mode(ExternalPermissionMode::AcceptEdits);

        let fresh = OpenCodeTurnSpec::Fresh {
            prompt: "do it".to_owned(),
        }
        .args(&config);
        assert_eq!(fresh.last().map(String::as_str), Some("do it"));
        assert!(fresh.iter().any(|a| a == "run"));
        assert!(!fresh.iter().any(|a| a == "--session"));

        let resume = OpenCodeTurnSpec::Resume {
            session_id: "ses_9".to_owned(),
            message: "again".to_owned(),
        }
        .args(&config);
        assert_eq!(resume.last().map(String::as_str), Some("again"));
        assert!(resume.iter().any(|a| a == "--session"));
        // The session id flag is followed by a `--` separator and then the
        // message.
        let id_pos = resume
            .iter()
            .position(|a| a == "ses_9")
            .expect("session id present");
        assert_eq!(
            id_pos,
            resume.len() - 3,
            "id precedes the `--` separator and the appended message"
        );
        assert_eq!(resume.get(id_pos + 1).map(String::as_str), Some("--"));
        assert_eq!(resume.get(id_pos + 2).map(String::as_str), Some("again"));

        // A configured working directory rides along as `--dir <path>` for both a
        // fresh and a resumed turn, so OpenCode confines its file operations to
        // the intended worktree rather than the launching checkout.
        let scoped = OpenCodeConfig::new().with_working_dir("/tmp/wt");
        let scoped_fresh = OpenCodeTurnSpec::Fresh {
            prompt: "go".to_owned(),
        }
        .args(&scoped);
        assert!(scoped_fresh.windows(2).any(|w| w == ["--dir", "/tmp/wt"]));
        let scoped_resume = OpenCodeTurnSpec::Resume {
            session_id: "ses_x".to_owned(),
            message: "more".to_owned(),
        }
        .args(&scoped);
        assert!(scoped_resume.windows(2).any(|w| w == ["--dir", "/tmp/wt"]));
    }

    #[test]
    fn opencode_turn_spec_separates_dash_prefixed_prompt_with_double_dash() {
        // M2-4 / M-EXT-4: a message that starts with `-` must not be parsed
        // as a flag; a `--` separator keeps it positional (OpenCode's yargs
        // sets `populate--: true`).
        let config = OpenCodeConfig::new();

        let fresh = OpenCodeTurnSpec::Fresh {
            prompt: "--model openai/gpt-5".to_owned(),
        }
        .args(&config);
        assert_eq!(
            fresh.last().map(String::as_str),
            Some("--model openai/gpt-5")
        );
        assert_eq!(
            fresh.get(fresh.len() - 2).map(String::as_str),
            Some("--"),
            "prompt follows a `--` separator"
        );

        let resume = OpenCodeTurnSpec::Resume {
            session_id: "ses_9".to_owned(),
            message: "--session other".to_owned(),
        }
        .args(&config);
        assert_eq!(resume.last().map(String::as_str), Some("--session other"));
        assert_eq!(
            resume.get(resume.len() - 2).map(String::as_str),
            Some("--"),
            "message follows a `--` separator"
        );
    }

    #[test]
    fn opencode_adapter_implemented_capabilities_disable_host_bridges() {
        let caps = implemented_capabilities();
        assert!(caps.streaming);
        assert!(caps.resume);
        assert!(caps.artifacts);
        assert!(caps.usage);
        assert!(caps.graceful_shutdown);
        assert!(
            !caps.permission_bridge,
            "opencode run never pauses for approval"
        );
        assert!(!caps.host_tools, "no host-tool bridge");
        assert!(!caps.host_subagents, "no subagent bridge");
    }

    #[test]
    fn opencode_adapter_probed_capabilities_intersect_with_implemented() {
        let mut probed = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::OpenCode);
        // A CLI that advertises streaming but not resume, and claims host tools.
        probed.streaming = true;
        probed.resume = false;
        probed.host_tools = true;
        probed.artifacts = true;
        probed.usage = true;
        probed.graceful_shutdown = true;

        let adapter = OpenCodeAdapter::with_probed_capabilities(OpenCodeConfig::new(), &probed);
        let caps = adapter.capabilities();
        assert!(caps.streaming, "streaming is implemented and probed");
        assert!(!caps.resume, "resume is off because the probe lacked it");
        assert!(
            !caps.host_tools,
            "host tools stay off even though the probe claimed them"
        );
        assert_eq!(adapter.kind(), ExternalRuntimeKind::OpenCode);
    }

    #[test]
    fn opencode_adapter_intersect_keeps_left_runtime_and_ands_flags() {
        let left = implemented_capabilities();
        let right = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::OpenCode);
        let both = intersect_capabilities(&left, &right);
        assert_eq!(both.runtime, ExternalRuntimeKind::OpenCode);
        for capability in ExternalCapability::ALL {
            assert!(!both.supports(capability));
        }
    }

    #[tokio::test]
    async fn opencode_adapter_start_rejects_declared_tools() {
        let tool = crate::model::tool::Tool {
            name: "search".to_owned(),
            description: "search the repo".to_owned(),
            input_schema: serde_json::json!({ "type": "object" }),
        };
        let adapter = OpenCodeAdapter::new(OpenCodeConfig::new());
        let ctx = run_context();
        let outcome = adapter.start(&start_request(vec![tool]), &ctx, None).await;
        match outcome {
            Err(ExternalAgentError::UnsupportedCapability {
                capability,
                runtime,
                ..
            }) => {
                assert_eq!(capability, ExternalCapability::HostTools);
                assert_eq!(runtime, ExternalRuntimeKind::OpenCode);
            }
            Err(other) => panic!("expected UnsupportedCapability, got {other:?}"),
            Ok(_) => panic!("declared host tools must be refused before spawning"),
        }
    }

    /// H-EXT-3: `close` classifies the child exit by status code, so a crashed
    /// turn process is never mistaken for a clean close (which would mark a
    /// dirty worktree as reusable). These tests spawn a real short-lived `sh`
    /// child wired exactly like the production turn stream.
    mod close_classification {
        use super::super::{OpenCodeProcessTurn, OpenCodeTurnStream};
        use crate::agent::external::{ExternalSessionShutdown, process_group};
        use std::time::Duration;
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;

        /// Spawns a real `sh -c <script>` child with piped stdout.
        fn spawn_sh(script: &str) -> OpenCodeProcessTurn {
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg(script)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            // Mirror the production spawn: the child leads its own process group.
            process_group::configure_managed_command(&mut command);
            let mut child = command.spawn().expect("spawn sh");
            let stdout = child.stdout.take().expect("stdout is piped");
            OpenCodeProcessTurn {
                child,
                stdout: BufReader::new(stdout).lines(),
                read_timeout: Duration::from_secs(1),
                shutdown_grace: Duration::from_millis(250),
            }
        }

        /// A zero exit status closes `Graceful`.
        #[tokio::test]
        async fn zero_exit_is_graceful() {
            let mut turn = spawn_sh("exit 0");
            assert_eq!(turn.close().await, ExternalSessionShutdown::Graceful);
        }

        /// A non-zero exit status closes `Failed`, not `Graceful`.
        #[tokio::test]
        async fn nonzero_exit_is_failed() {
            let mut turn = spawn_sh("exit 1");
            assert_eq!(turn.close().await, ExternalSessionShutdown::Failed);
        }

        /// A child still running past the grace window is force-killed.
        #[tokio::test]
        async fn grace_overrun_is_forced_kill() {
            let mut turn = spawn_sh("sleep 30");
            assert_eq!(turn.close().await, ExternalSessionShutdown::ForcedKill);
        }

        /// H-EXT-2: a force-close kills the whole process group, so
        /// grandchildren the CLI spawned (builds, dev servers, ...) cannot
        /// outlive the turn.
        #[cfg(unix)]
        #[tokio::test]
        async fn force_close_kills_the_whole_process_group() {
            let mut turn = spawn_sh("sleep 300 & sleep 300");
            let pgid = turn.child.id().expect("child id") as i32;
            assert_eq!(turn.close().await, ExternalSessionShutdown::ForcedKill);
            process_group::assert_process_group_reaped(pgid).await;
        }
    }

    #[test]
    fn session_config_applies_request_level_policy_overrides() {
        // M2-7: the request's policy overrides the construction-time config;
        // the prepared session dir flows into the `--dir` flag OpenCode
        // actually resolves file operations from.
        let adapter = OpenCodeAdapter::new(
            OpenCodeConfig::new()
                .with_permission_mode(ExternalPermissionMode::Prompt)
                .with_working_dir("/config/dir"),
        );

        let mut request = start_request(Vec::new());
        request.policy.permission_mode = ExternalPermissionMode::BypassPermissions;
        request.session_dir = Some(WorktreeRef::new("/prepared/session-0"));

        let effective = adapter.session_config(&request);
        assert_eq!(
            effective.permission_mode(),
            ExternalPermissionMode::BypassPermissions,
        );
        assert!(effective.auto_approve());
        let spec = OpenCodeTurnSpec::Fresh {
            prompt: "do the thing".to_owned(),
        };
        let args = spec.args(&effective);
        assert!(args.iter().any(|arg| arg == "--auto"));
        let dir = args
            .iter()
            .position(|arg| arg == "--dir")
            .expect("--dir flag present");
        assert_eq!(args[dir + 1], "/prepared/session-0");

        let fallback = adapter.session_config(&start_request(Vec::new()));
        assert!(
            !fallback.auto_approve(),
            "the fixture policy is AcceptEdits"
        );
        assert_eq!(
            fallback.working_dir(),
            Some(std::path::Path::new("/config/dir")),
            "without a prepared session dir the config working dir stays"
        );
    }
}
