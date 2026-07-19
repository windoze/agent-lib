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
//! - `CodexSession` (private) is one live session ([`ExternalRuntimeSession`]). It
//!   owns a per-turn CLI process, feeds each stdout line to the decoder, mirrors
//!   observations to the live sink, and
//!   [`advance`](ExternalRuntimeSession::advance)s to the next
//!   [`RuntimeDecisionPoint`].
//!
//! # One process per turn
//!
//! Unlike Claude Code's single long-lived `stream-json` process, `codex exec` is
//! **one-shot per turn**: the prompt is a CLI positional argument (not a stdin
//! frame), and the process exits when the turn settles. A follow-up turn is a
//! brand-new `codex exec resume <thread_id> <message>` process. `CodexSession`
//! therefore spawns a fresh process for the first turn (in `begin`) and another
//! for every follow-up turn (in `advance`), threading a single decoder — whose
//! `seq` spans the whole session (design §5.5) — across all of them.
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
//! [`CodexTurnStream`] traits, not a `tokio::process::Child` directly. Production
//! uses [`SystemCodexLauncher`], which spawns the real CLI; the unit tests inject
//! a fake launcher that replays canned JSONL lines and captures the
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
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;

use crate::agent::RunContext;

use crate::agent::external::process::{self, ChildStdinMode, ManagedChild, PreludeDeadline};
use crate::agent::external::{
    ExternalAgentError, ExternalEventSink, ExternalObservedEvent, ExternalRuntimeAdapter,
    ExternalRuntimeCapabilities, ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionInput,
    ExternalSessionRef, ExternalSessionRequest, ExternalSessionShutdown, RuntimeDecisionPoint,
};

use super::{CodexConfig, CodexDecision, CodexDecodeContext, CodexStreamDecoder};

/// The launch shape of one `codex exec` turn.
///
/// Each turn is a fresh CLI process: a [`Fresh`](Self::Fresh) turn starts a brand
/// new session (`codex … exec … <prompt>`), while a [`Resume`](Self::Resume) turn
/// continues an existing thread (`codex … exec resume … <thread_id> <message>`).
/// [`args`](Self::args) turns the spec into the full argument list by appending
/// the per-turn text to the config's frozen base arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CodexTurnSpec {
    /// A brand-new session turn carrying the initial prompt.
    Fresh {
        /// The initial user prompt, appended as the `codex exec` positional arg.
        prompt: String,
    },
    /// A follow-up turn resuming `session_id` with a new user message.
    Resume {
        /// The runtime-assigned thread id to resume.
        session_id: String,
        /// The follow-up user message, appended after the thread id and a
        /// `--` separator.
        message: String,
    },
}

impl CodexTurnSpec {
    /// Builds the full CLI argument list (after the binary) for this turn.
    ///
    /// A fresh turn reuses [`base_exec_args`](CodexConfig::base_exec_args) and
    /// appends the prompt; a resume turn reuses
    /// [`base_resume_args`](CodexConfig::base_resume_args) and appends the
    /// follow-up message after the thread id.
    ///
    /// The user-controlled text is always preceded by a `--` separator so a
    /// prompt that starts with `-` (for example `--model`) is parsed as the
    /// positional prompt instead of a clap flag. `codex exec` is clap-based
    /// and honors the separator (confirmed against `codex exec --help` and
    /// `codex exec resume --help`, M2-4 / M-EXT-4).
    fn args(&self, config: &CodexConfig) -> Vec<String> {
        match self {
            CodexTurnSpec::Fresh { prompt } => {
                let mut args = config.base_exec_args();
                args.push("--".to_owned());
                args.push(prompt.clone());
                args
            }
            CodexTurnSpec::Resume {
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

/// The stdout stream of one live `codex exec` turn process.
///
/// Splitting the raw IO behind this trait lets the session state machine
/// (`CodexSession`) be unit-tested offline with a fake transport while production
/// drives a real CLI child through [`CodexProcessTurn`]. Every frame is one
/// newline-delimited `codex exec --json` object.
#[async_trait]
trait CodexTurnStream: Send {
    /// Reads the next stdout frame line, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the read fails or times out.
    async fn read_frame(&mut self) -> io::Result<Option<String>>;

    /// Closes the turn's process and classifies how the close went.
    async fn close(&mut self) -> ExternalSessionShutdown;
}

/// Spawns one `codex exec` / `codex exec resume` turn process.
///
/// Splitting the spawn behind this trait lets the session be exercised offline
/// with a fake launcher that captures each [`CodexTurnSpec`]; production uses
/// [`SystemCodexLauncher`], which spawns the real CLI.
#[async_trait]
trait CodexLauncher: Send + Sync {
    /// Spawns a turn process for `spec` and returns its live stdout stream.
    ///
    /// # Errors
    ///
    /// Returns the raw [`io::Error`] from spawning (missing binary, permission
    /// denied); the caller classifies it into
    /// [`Launch`](ExternalAgentError::Launch) /
    /// [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable) /
    /// [`SessionLost`](ExternalAgentError::SessionLost) depending on the turn.
    async fn launch(&self, spec: &CodexTurnSpec) -> io::Result<Box<dyn CodexTurnStream>>;
}

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

#[async_trait]
impl CodexTurnStream for ManagedChild {
    async fn read_frame(&mut self) -> io::Result<Option<String>> {
        self.read_line().await
    }

    async fn close(&mut self) -> ExternalSessionShutdown {
        ManagedChild::close(self).await
    }
}

/// How the first turn of a session is launched, controlling how a spawn failure
/// and a missing thread id are classified.
enum FirstLaunch {
    /// A fresh `start`: a spawn failure is a [`Launch`](ExternalAgentError::Launch).
    Fresh,
    /// A cross-process `resume`: a spawn failure is a
    /// [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable) naming the
    /// session being revived.
    Resume(ExternalSessionRef),
}

/// One live Codex session wrapping the private stream decoder.
///
/// The session owns a per-turn CLI process and a single [`CodexStreamDecoder`]
/// whose `seq` line spans the whole session (design §5.5). The first turn is
/// launched by [`begin`](Self::begin) (which reads up to the `thread.started`
/// frame to learn the runtime thread id); each follow-up
/// [`advance`](ExternalRuntimeSession::advance) launches a fresh `codex exec
/// resume` process, feeds its stdout to the decoder, mirrors observations to the
/// live sink, and returns at the decision the turn settles on.
struct CodexSession<L: CodexLauncher> {
    launcher: L,
    decoder: CodexStreamDecoder,
    session_id: String,
    last_event_seq: Option<u64>,
    sink: Option<Arc<dyn ExternalEventSink>>,
    capabilities: ExternalRuntimeCapabilities,
    /// The stdout stream of the currently-open turn process, if any.
    current: Option<Box<dyn CodexTurnStream>>,
    /// Observations buffered by the startup prelude, prepended to the first turn.
    carried: Vec<ExternalObservedEvent>,
    /// A decision reached during the prelude (defensive; the prelude only reads
    /// up to `thread.started`, which precedes any turn boundary).
    carried_decision: Option<CodexDecision>,
    /// Set when `begin` already launched the first turn's process (Codex takes the
    /// prompt as a launch argument), so the first `advance` — which carries the
    /// same input — continues that in-flight turn instead of spawning another.
    first_turn_pending: bool,
    /// The most severe disposition seen closing a mid-session turn process,
    /// folded into [`shutdown`](ExternalRuntimeSession::shutdown) so a
    /// force-killed turn still marks the session as leaving residual side
    /// effects (review M-EXT-5).
    worst_close: Option<ExternalSessionShutdown>,
    /// Per-session counter for minting trace node ids of mid-session closes.
    close_trace_seq: u64,
}

impl<L: CodexLauncher> CodexSession<L> {
    /// Builds a session over `launcher`, binding the decode context and the
    /// capability set the adapter reports.
    fn new(
        launcher: L,
        context: CodexDecodeContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
        capabilities: ExternalRuntimeCapabilities,
    ) -> Self {
        Self {
            launcher,
            decoder: CodexStreamDecoder::new(context),
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
    /// [`CodexStreamDecoder::with_next_seq`] for why a resume must not restart
    /// the seq line at 0.
    #[must_use]
    fn with_resume_high_water(mut self, high_water: Option<u64>) -> Self {
        if let Some(high_water) = high_water {
            self.decoder = self.decoder.with_next_seq(high_water.saturating_add(1));
            self.last_event_seq = Some(high_water);
        }
        self
    }

    /// Launches the first turn's process and reads its prelude up to the
    /// `thread.started` frame that carries the runtime thread id.
    ///
    /// Codex emits a `thread.started` frame first on every `exec` process, so the
    /// prelude read captures the id (needed to register and later resume the
    /// session) before the rest of the turn — text, completion — is deferred to
    /// the first [`advance`](ExternalRuntimeSession::advance). A resume already
    /// knows its id from the persisted [`ExternalSessionRef`], so that id is
    /// pre-seeded and the prelude only refreshes it.
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
    /// cancellation, or `Launch` when a fresh session never reports a thread id.
    async fn begin(
        &mut self,
        spec: &CodexTurnSpec,
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
                    runtime: ExternalRuntimeKind::Codex,
                    detail: format!("spawning codex exec failed: {:?}", error.kind()),
                },
                FirstLaunch::Resume(session) => ExternalAgentError::ResumeUnavailable {
                    session: session.clone(),
                    detail: format!("failed spawning codex exec resume: {:?}", error.kind()),
                },
            })?;
        self.current = Some(stream);
        self.first_turn_pending = true;

        // A resume already knows its id, so pre-seed it: the session must expose a
        // non-empty id to be registered even if the resumed thread never re-emits
        // `thread.started`.
        if let FirstLaunch::Resume(session) = &first
            && let Some(id) = &session.session_id
        {
            self.session_id = id.clone();
        }

        let prelude = PreludeDeadline::new(prelude_timeout);
        // Fresh vs resume classification axis, shared by both deadline guards.
        let deadline_error = |first: &FirstLaunch| match first {
            FirstLaunch::Fresh => ExternalAgentError::Launch {
                runtime: ExternalRuntimeKind::Codex,
                detail: "codex exec did not report a thread id within the launch timeout"
                    .to_owned(),
            },
            FirstLaunch::Resume(session) => ExternalAgentError::ResumeUnavailable {
                session: session.clone(),
                detail: "resumed codex exec did not report a thread id within the launch timeout"
                    .to_owned(),
            },
        };
        while self.decoder.session_id().is_none() {
            prelude.check_active(
                ctx,
                self.maybe_session_ref(),
                "codex session begin was cancelled",
                || deadline_error(&first),
            )?;
            let line = prelude
                .await_until(self.read_line(), || deadline_error(&first))
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

        if let Some(id) = self.decoder.session_id() {
            self.session_id = id.to_owned();
        }
        if self.session_id.is_empty() {
            // Only reachable for a fresh start; a resume pre-seeds its id above.
            return Err(ExternalAgentError::Launch {
                runtime: ExternalRuntimeKind::Codex,
                detail: "codex exec did not report a thread id".to_owned(),
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
                detail: "codex session has no open turn to read".to_owned(),
            });
        };
        stream
            .read_frame()
            .await
            .map_err(|error| ExternalAgentError::SessionLost {
                session: self.maybe_session_ref(),
                detail: format!("failed reading codex exec stream: {:?}", error.kind()),
            })
    }

    /// Drains the decoder's buffered observations, mirroring each to the live
    /// sink and advancing the high-water `seq`.
    fn drain_and_emit(&mut self) -> Vec<ExternalObservedEvent> {
        let observed = self.decoder.take_observations();
        process::emit_observations(&observed, self.sink.as_ref(), &mut self.last_event_seq);
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
        process::record_mid_session_close(
            ctx,
            &mut self.close_trace_seq,
            &mut self.worst_close,
            disposition,
        );
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

        let spec = CodexTurnSpec::Resume {
            session_id: self.session_id.clone(),
            message,
        };
        let stream =
            self.launcher
                .launch(&spec)
                .await
                .map_err(|error| ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: format!("failed spawning codex exec resume: {:?}", error.kind()),
                })?;
        self.current = Some(stream);
        Ok(())
    }

    /// Folds a settled [`CodexDecision`] into a [`RuntimeDecisionPoint`].
    ///
    /// `codex exec --json` never pauses for the host, so a turn only ever
    /// completes or fails (M7-2).
    fn finish(
        &self,
        decision: CodexDecision,
        observations: Vec<ExternalObservedEvent>,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        match decision {
            CodexDecision::Completed { output } => Ok(RuntimeDecisionPoint::Completed {
                session: self.session_ref(),
                output,
                observations,
            }),
            CodexDecision::Failed { error } => Err(error),
        }
    }

    /// Returns the session facts, or `None` before a thread id has been assigned.
    fn maybe_session_ref(&self) -> Option<ExternalSessionRef> {
        process::maybe_session_ref_for_id(
            ExternalRuntimeKind::Codex,
            &self.session_id,
            self.last_event_seq,
        )
    }
}

#[async_trait]
impl<L: CodexLauncher> ExternalRuntimeSession for CodexSession<L> {
    fn session_ref(&self) -> ExternalSessionRef {
        process::session_ref_for_id(
            ExternalRuntimeKind::Codex,
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

        // A fresh `start` / `resume` already launched and began this turn, so the
        // first advance (carrying the same input) continues the in-flight turn
        // instead of spawning another process for it.
        let already_spawned = std::mem::take(&mut self.first_turn_pending);

        // A decision reached during the startup prelude settles the turn without
        // further IO (defensive: the prelude only reads up to `thread.started`).
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
                    detail: "codex session advance was cancelled".to_owned(),
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
                        detail: "codex session closed before reaching a decision point".to_owned(),
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
    process::autonomous_turn_message(
        capabilities,
        input,
        process::AutonomousTurnMessages {
            interaction: "codex exec runs autonomously; there is no host-answerable interaction to resolve",
            tool_results: "codex adapter does not bridge host tool results",
            subagent: "codex adapter does not bridge host subagents",
            shutdown: "codex session shutdown must go through shutdown(), not advance",
        },
    )
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
mod tests {
    use super::{
        CodexAdapter, CodexLauncher, CodexSession, CodexTurnSpec, CodexTurnStream, FirstLaunch,
        implemented_capabilities, turn_message,
    };
    use crate::agent::external::CodexConfig;
    use crate::agent::external::process;
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

    const THREAD_ID: &str = "codex-thread-1";
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
            TraceNodeId::new("codex-adapter-test"),
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
            runtime: ExternalRuntimeKind::Codex,
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
            runtime: ExternalRuntimeKind::Codex,
            session_id: Some(THREAD_ID.to_owned()),
            transcript_ref: None,
            resume_token: Some(THREAD_ID.to_owned()),
            last_event_seq: Some(3),
        }
    }

    fn thread_started(thread_id: &str) -> String {
        format!(r#"{{"type":"thread.started","thread_id":"{thread_id}"}}"#)
    }

    fn turn_started() -> String {
        r#"{"type":"turn.started"}"#.to_owned()
    }

    fn agent_message(text: &str) -> String {
        format!(r#"{{"type":"item.completed","item":{{"type":"agent_message","text":"{text}"}}}}"#)
    }

    fn turn_completed() -> String {
        r#"{"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":4,"output_tokens":6,"reasoning_output_tokens":2}}"#.to_owned()
    }

    fn turn_failed() -> String {
        r#"{"type":"turn.failed","error":{"message":"boom"}}"#.to_owned()
    }

    /// A fake turn stream replaying canned stdout lines.
    struct FakeTurn {
        lines: VecDeque<String>,
        close_disposition: ExternalSessionShutdown,
        /// Line replayed forever once `lines` drains (prelude-deadline tests).
        repeat: Option<String>,
    }

    #[async_trait]
    impl CodexTurnStream for FakeTurn {
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
    type RecordedSpecs = Arc<Mutex<Vec<CodexTurnSpec>>>;

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
    impl CodexLauncher for FakeLauncher {
        async fn launch(&self, spec: &CodexTurnSpec) -> std::io::Result<Box<dyn CodexTurnStream>> {
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
    ) -> CodexSession<FakeLauncher> {
        let context = CodexAdapter::decode_context(&CodexConfig::new(), &start_request(Vec::new()));
        CodexSession::new(launcher, context, sink, implemented_capabilities())
    }

    fn fresh_spec(prompt: &str) -> CodexTurnSpec {
        CodexTurnSpec::Fresh {
            prompt: prompt.to_owned(),
        }
    }

    #[tokio::test]
    async fn codex_adapter_advance_drives_text_and_completion() {
        let sink = Arc::new(RecordingSink::default());
        let launcher = FakeLauncher::new(vec![vec![
            thread_started(THREAD_ID),
            turn_started(),
            agent_message("looking into it"),
            turn_completed(),
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
            .expect("begin launches the first turn and reads thread.started");
        assert_eq!(session.session_ref().session_id.as_deref(), Some(THREAD_ID));

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
                assert_eq!(session.session_id.as_deref(), Some(THREAD_ID));
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
    async fn codex_adapter_follow_up_turn_resumes_with_thread_id() {
        let launcher = FakeLauncher::new(vec![
            vec![
                thread_started(THREAD_ID),
                agent_message("first"),
                turn_completed(),
            ],
            vec![
                thread_started(THREAD_ID),
                agent_message("second"),
                turn_completed(),
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

        // A follow-up turn spawns a fresh `exec resume` process for the thread.
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
                CodexTurnSpec::Resume {
                    session_id: THREAD_ID.to_owned(),
                    message: "keep going".to_owned(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn codex_adapter_advance_reports_session_lost_on_early_eof() {
        let launcher = FakeLauncher::new(vec![vec![thread_started(THREAD_ID)]]);
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
                    Some(THREAD_ID)
                );
                assert!(detail.contains("decision point"));
            }
            other => panic!("expected SessionLost, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn codex_adapter_advance_propagates_protocol_error_on_malformed_frame() {
        let mut frames = vec![thread_started(THREAD_ID)];
        frames.extend((0..=8).map(|_| "{ not json".to_owned()));
        let launcher = FakeLauncher::new(vec![frames]);
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
            .expect_err("too much non-json noise is a protocol error");
        assert!(matches!(error, ExternalAgentError::Protocol { .. }));
    }

    #[tokio::test]
    async fn codex_adapter_advance_propagates_turn_failed() {
        let launcher = FakeLauncher::new(vec![vec![thread_started(THREAD_ID), turn_failed()]]);
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
            .expect_err("a turn.failed frame fails the turn");
        assert!(matches!(error, ExternalAgentError::Runtime { .. }));
    }

    #[tokio::test]
    async fn codex_adapter_shutdown_classifies_the_close() {
        let launcher = FakeLauncher::new(vec![vec![thread_started(THREAD_ID)]])
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
    async fn codex_adapter_begin_times_out_when_thread_id_never_arrives() {
        // A CLI babbling tolerated non-init frames would loop the prelude forever
        // on the per-line read timeout alone (each line resets it); the launch
        // deadline caps the whole prelude (review M-EXT-6).
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
            .expect_err("a prelude that never reports a thread id hits the launch deadline");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the prelude deadline fires promptly"
        );
        match error {
            ExternalAgentError::Launch { runtime, detail } => {
                assert_eq!(runtime, ExternalRuntimeKind::Codex);
                assert!(detail.contains("launch timeout"), "detail: {detail}");
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn codex_adapter_begin_resume_times_out_when_thread_id_never_arrives() {
        // The same prelude deadline on the resume path is classified as
        // `ResumeUnavailable`, matching the spawn-failure classification axis.
        let launcher = FakeLauncher::new(Vec::new()).repeating(r#"{"type":"ping"}"#.to_owned());
        let mut session = session_over(launcher, None);
        let spec = CodexTurnSpec::Resume {
            session_id: THREAD_ID.to_owned(),
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
                assert_eq!(session.session_id.as_deref(), Some(THREAD_ID));
                assert!(detail.contains("launch timeout"), "detail: {detail}");
            }
            other => panic!("expected ResumeUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn codex_adapter_begin_honours_cancellation() {
        // The prelude checks `ctx.is_cancelled()` per iteration, just like the
        // advance loop (review M-EXT-6).
        let launcher = FakeLauncher::new(vec![vec![thread_started(THREAD_ID)]]);
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
    async fn codex_adapter_mid_turn_close_is_traced_and_marks_the_session_dirty() {
        // Turn 1's process has to be force-killed when turn 2 spawns; that
        // disposition must reach the trace and the session's final shutdown
        // report instead of being dropped (review M-EXT-5).
        let launcher = FakeLauncher::new(vec![
            vec![
                thread_started(THREAD_ID),
                agent_message("first"),
                turn_completed(),
            ],
            vec![
                thread_started(THREAD_ID),
                agent_message("second"),
                turn_completed(),
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
    async fn codex_adapter_resume_defers_and_records_thread_id() {
        let launcher = FakeLauncher::new(vec![vec![
            thread_started(THREAD_ID),
            agent_message("resumed"),
            turn_completed(),
        ]]);
        let specs = launcher.recorded_specs();
        let mut session = session_over(launcher, None);

        let spec = CodexTurnSpec::Resume {
            session_id: THREAD_ID.to_owned(),
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
        assert_eq!(session.session_ref().session_id.as_deref(), Some(THREAD_ID));

        let ctx = run_context();
        let follow_up = ExternalSessionInput::Continue {
            message: "continue".to_owned(),
        };
        let decision = session.advance(&follow_up, &ctx).await.expect("completion");
        assert!(matches!(decision, RuntimeDecisionPoint::Completed { .. }));

        // The one recorded spec is the resume turn carrying the thread id.
        let recorded = specs.lock().unwrap().clone();
        assert_eq!(recorded, vec![spec]);
    }

    #[tokio::test]
    async fn codex_adapter_resume_continues_the_seq_line_past_the_high_water() {
        // A resume must continue the decoder's seq line past the persisted
        // `last_event_seq`: restarting at 0 would let the machine's replay dedup
        // silently drop every post-resume observation (design §5.5, review
        // M-EXT-1).
        let launcher = FakeLauncher::new(vec![vec![
            thread_started(THREAD_ID),
            agent_message("resumed"),
            turn_completed(),
        ]]);
        let mut session = session_over(launcher, None).with_resume_high_water(Some(50));

        let spec = CodexTurnSpec::Resume {
            session_id: THREAD_ID.to_owned(),
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
    async fn codex_adapter_follow_up_respond_tool_results_is_unsupported() {
        let launcher = FakeLauncher::new(vec![vec![
            thread_started(THREAD_ID),
            agent_message("done"),
            turn_completed(),
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
    async fn codex_adapter_begin_reports_launch_failure() {
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
                runtime: ExternalRuntimeKind::Codex,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn codex_adapter_begin_reports_resume_failure() {
        let launcher = FakeLauncher::failing(std::io::ErrorKind::NotFound);
        let mut session = session_over(launcher, None);
        let spec = CodexTurnSpec::Resume {
            session_id: THREAD_ID.to_owned(),
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
    fn codex_adapter_turn_message_maps_inputs_and_refusals() {
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
    fn codex_turn_spec_appends_prompt_and_message_to_base_args() {
        let config = CodexConfig::new().with_permission_mode(ExternalPermissionMode::AcceptEdits);

        let fresh = CodexTurnSpec::Fresh {
            prompt: "do it".to_owned(),
        }
        .args(&config);
        assert_eq!(fresh.last().map(String::as_str), Some("do it"));
        assert!(fresh.iter().any(|a| a == "exec"));
        assert!(!fresh.iter().any(|a| a == "resume"));

        let resume = CodexTurnSpec::Resume {
            session_id: "thread-9".to_owned(),
            message: "again".to_owned(),
        }
        .args(&config);
        assert_eq!(resume.last().map(String::as_str), Some("again"));
        assert!(resume.iter().any(|a| a == "resume"));
        // The session id is followed by a `--` separator and then the message.
        let id_pos = resume
            .iter()
            .position(|a| a == "thread-9")
            .expect("session id present");
        assert_eq!(
            id_pos,
            resume.len() - 3,
            "id precedes the `--` separator and the appended message"
        );
        assert_eq!(resume.get(id_pos + 1).map(String::as_str), Some("--"));
        assert_eq!(resume.get(id_pos + 2).map(String::as_str), Some("again"));
    }

    #[test]
    fn codex_turn_spec_separates_dash_prefixed_prompt_with_double_dash() {
        // M2-4 / M-EXT-4: a prompt that starts with `-` must not be parsed as
        // a clap flag; a `--` separator keeps it positional.
        let config = CodexConfig::new();

        let fresh = CodexTurnSpec::Fresh {
            prompt: "--model gpt-5".to_owned(),
        }
        .args(&config);
        assert_eq!(fresh.last().map(String::as_str), Some("--model gpt-5"));
        assert_eq!(
            fresh.get(fresh.len() - 2).map(String::as_str),
            Some("--"),
            "prompt follows a `--` separator"
        );

        let resume = CodexTurnSpec::Resume {
            session_id: "thread-9".to_owned(),
            message: "--sandbox read-only".to_owned(),
        }
        .args(&config);
        assert_eq!(
            resume.last().map(String::as_str),
            Some("--sandbox read-only")
        );
        assert_eq!(
            resume.get(resume.len() - 2).map(String::as_str),
            Some("--"),
            "message follows a `--` separator"
        );
    }

    #[test]
    fn codex_adapter_implemented_capabilities_disable_host_bridges() {
        let caps = implemented_capabilities();
        assert!(caps.streaming);
        assert!(caps.resume);
        assert!(caps.artifacts);
        assert!(caps.usage);
        assert!(caps.graceful_shutdown);
        assert!(
            !caps.permission_bridge,
            "codex exec never pauses for approval"
        );
        assert!(!caps.host_tools, "no host-tool bridge");
        assert!(!caps.host_subagents, "no subagent bridge");
    }

    #[test]
    fn codex_adapter_probed_capabilities_intersect_with_implemented() {
        let mut probed = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::Codex);
        // A CLI that advertises streaming but not resume, and claims host tools.
        probed.streaming = true;
        probed.resume = false;
        probed.host_tools = true;
        probed.artifacts = true;
        probed.usage = true;
        probed.graceful_shutdown = true;

        let adapter = CodexAdapter::with_probed_capabilities(CodexConfig::new(), &probed);
        let caps = adapter.capabilities();
        assert!(caps.streaming, "streaming is implemented and probed");
        assert!(!caps.resume, "resume is off because the probe lacked it");
        assert!(
            !caps.host_tools,
            "host tools stay off even though the probe claimed them"
        );
        assert_eq!(adapter.kind(), ExternalRuntimeKind::Codex);
    }

    #[test]
    fn codex_adapter_intersect_keeps_left_runtime_and_ands_flags() {
        let left = implemented_capabilities();
        let right = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::Codex);
        let both = process::intersect_capabilities(&left, &right);
        assert_eq!(both.runtime, ExternalRuntimeKind::Codex);
        for capability in ExternalCapability::ALL {
            assert!(!both.supports(capability));
        }
    }

    #[tokio::test]
    async fn codex_adapter_start_rejects_declared_tools() {
        let tool = crate::model::tool::Tool {
            name: "search".to_owned(),
            description: "search the repo".to_owned(),
            input_schema: serde_json::json!({ "type": "object" }),
        };
        let adapter = CodexAdapter::new(CodexConfig::new());
        let ctx = run_context();
        let outcome = adapter.start(&start_request(vec![tool]), &ctx, None).await;
        match outcome {
            Err(ExternalAgentError::UnsupportedCapability {
                capability,
                runtime,
                ..
            }) => {
                assert_eq!(capability, ExternalCapability::HostTools);
                assert_eq!(runtime, ExternalRuntimeKind::Codex);
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
        use crate::agent::external::ExternalSessionShutdown;
        use crate::agent::external::process::{self, ChildStdinMode, ManagedChild};
        use std::time::Duration;
        use tokio::process::Command;

        /// Spawns a real `sh -c <script>` child with piped stdout.
        fn spawn_sh(script: &str) -> ManagedChild {
            let mut command = Command::new("sh");
            command.arg("-c").arg(script);
            ManagedChild::spawn(
                command,
                ChildStdinMode::Null,
                Duration::from_secs(1),
                Duration::from_millis(250),
                "stdout is piped",
                "test read timed out",
            )
            .expect("spawn sh")
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
            let pgid = turn.child_id().expect("child id") as i32;
            assert_eq!(turn.close().await, ExternalSessionShutdown::ForcedKill);
            process::assert_process_group_reaped(pgid).await;
        }
    }

    #[test]
    fn session_config_applies_request_level_policy_overrides() {
        // M2-7: the request's policy overrides the construction-time config,
        // flowing into the approval/sandbox flags every turn spawns with.
        let adapter = CodexAdapter::new(
            CodexConfig::new()
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
        assert_eq!(
            effective.working_dir(),
            Some(std::path::Path::new("/prepared/session-0")),
        );
        let spec = CodexTurnSpec::Fresh {
            prompt: "do the thing".to_owned(),
        };
        let args = spec.args(&effective);
        let approval = args
            .iter()
            .position(|arg| arg == "-a")
            .expect("approval flag present");
        assert_eq!(args[approval + 1], "never");
        let sandbox = args
            .iter()
            .position(|arg| arg == "-s")
            .expect("sandbox flag present");
        assert_eq!(args[sandbox + 1], "danger-full-access");

        let fallback = adapter.session_config(&start_request(Vec::new()));
        assert_eq!(
            fallback.working_dir(),
            Some(std::path::Path::new("/config/dir")),
            "without a prepared session dir the config working dir stays"
        );
    }
}
