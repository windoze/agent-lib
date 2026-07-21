//! Shared one-shot-per-turn CLI session state machine (features
//! `external-codex` and `external-opencode`).
//!
//! `codex exec` and `opencode run` are both **one-shot per turn**: the prompt
//! is a CLI positional argument (not a stdin frame), the process exits when the
//! turn settles, and a follow-up turn is a brand-new process resuming the
//! runtime-assigned session id. Their live sessions were near-clones differing
//! only in the wire decoder, the runtime kind stamped onto errors, and the
//! wording of error messages, so the state machine is single-sourced here and
//! each adapter binds it to its own runtime:
//!
//! - [`TurnSpec`] is the launch shape of one turn; [`TurnArgs`] is the
//!   per-runtime hook supplying the config's frozen base arguments.
//! - [`TurnStream`] / [`Launcher`] are the transport traits the session drives:
//!   production spawns a [`ManagedChild`] per turn, while the unit tests inject
//!   fakes replaying canned stdout lines.
//! - [`Session`] is the [`ExternalRuntimeSession`] state machine
//!   (begin/advance/shutdown), parameterized over a [`Flavor`] — the
//!   per-runtime [`Decoder`], the runtime kind, and the [`Labels`] message
//!   wording.
//!
//! Each CLI adapter re-exports these pieces under its runtime's own names
//! (`CodexTurnSpec`, `OpenCodeSession`, ...), so call sites and tests keep
//! their per-runtime vocabulary. Claude Code stays separate: its single
//! long-lived `stream-json` process takes turns as stdin frames, a genuinely
//! different shape.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::agent::RunContext;

use super::{
    AutonomousTurnMessages, ManagedChild, PreludeDeadline, emit_observations,
    maybe_session_ref_for_id, record_mid_session_close, session_ref_for_id,
};
use crate::agent::external::{
    ExternalAgentError, ExternalAgentOutput, ExternalEventSink, ExternalObservedEvent,
    ExternalRuntimeCapabilities, ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionInput,
    ExternalSessionRef, ExternalSessionShutdown, RuntimeDecisionPoint,
};

/// The launch shape of one turn of a one-shot CLI session.
///
/// Each turn is a fresh CLI process: a [`Fresh`](Self::Fresh) turn starts a
/// brand new session, while a [`Resume`](Self::Resume) turn continues an
/// existing one from its runtime-assigned session id. [`args`](Self::args)
/// turns the spec into the full argument list by appending the per-turn text to
/// the config's frozen base arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TurnSpec {
    /// A brand-new session turn carrying the initial prompt.
    Fresh {
        /// The initial user prompt, appended as the CLI's positional arg.
        prompt: String,
    },
    /// A follow-up turn resuming `session_id` with a new user message.
    Resume {
        /// The runtime-assigned session id to resume.
        session_id: String,
        /// The follow-up user message, appended after the resume arguments and
        /// a `--` separator.
        message: String,
    },
}

impl TurnSpec {
    /// Builds the full CLI argument list (after the binary) for this turn.
    ///
    /// A fresh turn reuses the config's base turn arguments and appends the
    /// prompt; a resume turn reuses the config's base resume arguments and
    /// appends the follow-up message after the session id.
    ///
    /// The user-controlled text is always preceded by a `--` separator so a
    /// prompt that starts with `-` (for example `--model`) is parsed as the
    /// positional prompt instead of a flag; both one-shot CLIs honor the
    /// separator (confirmed against `codex exec --help` / `codex exec resume
    /// --help` and the installed OpenCode source, M2-4 / M-EXT-4).
    pub(crate) fn args<C: TurnArgs>(&self, config: &C) -> Vec<String> {
        match self {
            TurnSpec::Fresh { prompt } => {
                let mut args = config.base_turn_args();
                args.push("--".to_owned());
                args.push(prompt.clone());
                args
            }
            TurnSpec::Resume {
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

/// Supplies a runtime config's frozen base arguments to [`TurnSpec::args`].
///
/// Implemented by each one-shot runtime's config in its adapter module,
/// delegating to the config's own frozen `base_*_args` methods.
pub(crate) trait TurnArgs {
    /// The base argument list of a fresh turn (`codex exec …` / `opencode run
    /// …`).
    fn base_turn_args(&self) -> Vec<String>;

    /// The base argument list of a turn resuming `session_id` (`codex exec
    /// resume … <id>` / `opencode run … --session <id>`).
    fn base_resume_args(&self, session_id: &str) -> Vec<String>;
}

/// The stdout stream of one live turn process.
///
/// Splitting the raw IO behind this trait lets the session state machine
/// ([`Session`]) be unit-tested offline with a fake transport while production
/// drives a real CLI child through [`ManagedChild`]. Every frame is one
/// newline-delimited JSON object.
#[async_trait]
pub(crate) trait TurnStream: Send {
    /// Reads the next stdout frame line, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the read fails or times out.
    async fn read_frame(&mut self) -> io::Result<Option<String>>;

    /// Closes the turn's process and classifies how the close went.
    async fn close(&mut self) -> ExternalSessionShutdown;
}

#[async_trait]
impl TurnStream for ManagedChild {
    async fn read_frame(&mut self) -> io::Result<Option<String>> {
        self.read_line().await
    }

    async fn close(&mut self) -> ExternalSessionShutdown {
        ManagedChild::close(self).await
    }
}

/// Spawns one turn process of a one-shot CLI session.
///
/// Splitting the spawn behind this trait lets the session be exercised offline
/// with a fake launcher that captures each [`TurnSpec`]; production uses the
/// per-runtime system launcher, which spawns the real CLI.
#[async_trait]
pub(crate) trait Launcher: Send + Sync {
    /// Spawns a turn process for `spec` and returns its live stdout stream.
    ///
    /// # Errors
    ///
    /// Returns the raw [`io::Error`] from spawning (missing binary, permission
    /// denied); the caller classifies it into
    /// [`Launch`](ExternalAgentError::Launch) /
    /// [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable) /
    /// [`SessionLost`](ExternalAgentError::SessionLost) depending on the turn.
    async fn launch(&self, spec: &TurnSpec) -> io::Result<Box<dyn TurnStream>>;
}

/// How the first turn of a session is launched, controlling how a spawn failure
/// and a missing session id are classified.
pub(crate) enum FirstLaunch {
    /// A fresh `start`: a spawn failure is a [`Launch`](ExternalAgentError::Launch).
    Fresh,
    /// A cross-process `resume`: a spawn failure is a
    /// [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable) naming the
    /// session being revived.
    Resume(ExternalSessionRef),
}

/// The per-runtime wording of the messages the one-shot state machine produces.
///
/// Every string is a complete message except the `*_failed` prefixes, which are
/// followed by `: {:?}` of the underlying [`io::ErrorKind`] at the call site.
/// Each runtime binds its own set through [`Flavor::labels`], so the
/// deduplicated state machine's errors read exactly as the per-runtime code
/// worded them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Labels {
    /// Wording for the turn-text extraction refusals (unsupported host-bridge
    /// inputs and a misrouted shutdown).
    pub(crate) turn: AutonomousTurnMessages,
    /// Spawn failure of the first turn, e.g. `spawning codex exec failed`.
    pub(crate) spawn_failed: &'static str,
    /// Spawn failure of a resumed turn, e.g. `failed spawning codex exec
    /// resume`.
    pub(crate) resume_spawn_failed: &'static str,
    /// The prelude missed its launch deadline, e.g. `codex exec did not report
    /// a thread id within the launch timeout`.
    pub(crate) id_timeout: &'static str,
    /// The resumed prelude missed its launch deadline.
    pub(crate) resume_id_timeout: &'static str,
    /// The prelude observed a host cancellation.
    pub(crate) begin_cancelled: &'static str,
    /// A fresh start never reported a session id.
    pub(crate) id_missing: &'static str,
    /// A read was attempted with no open turn process.
    pub(crate) no_open_turn: &'static str,
    /// A turn stream read failed, e.g. `failed reading codex exec stream`.
    pub(crate) read_failed: &'static str,
    /// An advance observed a host cancellation.
    pub(crate) advance_cancelled: &'static str,
    /// The stream hit EOF before settling the turn.
    pub(crate) closed_before_decision: &'static str,
}

/// The per-runtime frame decoder a [`Session`] threads across its turn
/// processes.
///
/// This mirrors the inherent API both one-shot decoders already expose; the
/// per-runtime adapter delegates to it, so the state machine here stays
/// decoder-agnostic while the wire formats remain per-runtime.
pub(crate) trait Decoder: Send {
    /// Host-supplied context the decoder needs (for example the working
    /// directory stamped onto command observations).
    type Context;
    /// The control-flow transfer a decoded turn settles on.
    type Decision: Send;

    /// Creates a decoder for a fresh session, binding the host decode context.
    fn new(context: Self::Context) -> Self;

    /// Seeds the `seq` line at `next_seq`, for a session resumed across
    /// processes.
    #[must_use]
    fn with_next_seq(self, next_seq: u64) -> Self;

    /// Returns the runtime-assigned session id, once a frame has reported one.
    fn session_id(&self) -> Option<&str>;

    /// Decodes one raw frame line, returning the settling decision if any.
    ///
    /// # Errors
    ///
    /// Returns the per-runtime protocol error for a corrupt frame.
    fn push_line(&mut self, line: &str) -> Result<Option<Self::Decision>, ExternalAgentError>;

    /// Drains the observations buffered since the last drain, leaving the
    /// running `seq` untouched.
    fn take_observations(&mut self) -> Vec<ExternalObservedEvent>;

    /// Folds a settled turn decision into the terminal output or the failure.
    ///
    /// A one-shot CLI never pauses for the host, so a turn only ever completes
    /// or fails.
    fn decision_result(decision: Self::Decision)
    -> Result<ExternalAgentOutput, ExternalAgentError>;
}

/// The per-runtime binding of a [`Session`]: decoder, runtime kind, and message
/// wording.
pub(crate) trait Flavor {
    /// The per-runtime frame decoder.
    type Decoder: Decoder;

    /// The runtime kind stamped onto errors and session refs.
    fn runtime() -> ExternalRuntimeKind;

    /// The runtime's wording of the state machine's messages.
    fn labels() -> &'static Labels;
}

/// One live one-shot CLI session wrapping the per-runtime stream decoder.
///
/// The session owns a per-turn CLI process and a single decoder whose `seq`
/// line spans the whole session (design §5.5). The first turn is launched by
/// [`begin`](Self::begin) (which reads until the decoder captures the runtime
/// session id); each follow-up [`advance`](ExternalRuntimeSession::advance)
/// launches a fresh resume process, feeds its stdout to the decoder, mirrors
/// observations to the live sink, and returns at the decision the turn settles
/// on.
pub(crate) struct Session<F: Flavor, L: Launcher> {
    launcher: L,
    decoder: F::Decoder,
    session_id: String,
    last_event_seq: Option<u64>,
    sink: Option<Arc<dyn ExternalEventSink>>,
    capabilities: ExternalRuntimeCapabilities,
    /// The stdout stream of the currently-open turn process, if any.
    current: Option<Box<dyn TurnStream>>,
    /// Observations buffered by the startup prelude, prepended to the first turn.
    carried: Vec<ExternalObservedEvent>,
    /// A decision reached during the prelude (defensive; the prelude normally
    /// only reads up to the frame that first reports the session id, which
    /// precedes any turn boundary, but a session that errors immediately
    /// settles here).
    carried_decision: Option<<F::Decoder as Decoder>::Decision>,
    /// Set when `begin` already launched the first turn's process (the runtime
    /// takes the prompt as a launch argument), so the first `advance` — which
    /// carries the same input — continues that in-flight turn instead of
    /// spawning another.
    first_turn_pending: bool,
    /// The most severe disposition seen closing a mid-session turn process,
    /// folded into [`shutdown`](ExternalRuntimeSession::shutdown) so a
    /// force-killed turn still marks the session as leaving residual side
    /// effects (review M-EXT-5).
    worst_close: Option<ExternalSessionShutdown>,
    /// Per-session counter for minting trace node ids of mid-session closes.
    close_trace_seq: u64,
}

impl<F: Flavor, L: Launcher> Session<F, L> {
    /// Builds a session over `launcher`, binding the decode context and the
    /// capability set the adapter reports.
    pub(crate) fn new(
        launcher: L,
        context: <F::Decoder as Decoder>::Context,
        sink: Option<Arc<dyn ExternalEventSink>>,
        capabilities: ExternalRuntimeCapabilities,
    ) -> Self {
        Self {
            launcher,
            decoder: F::Decoder::new(context),
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
    /// [`Decoder::with_next_seq`] for why a resume must not restart the seq
    /// line at 0.
    #[must_use]
    pub(crate) fn with_resume_high_water(mut self, high_water: Option<u64>) -> Self {
        if let Some(high_water) = high_water {
            self.decoder = self.decoder.with_next_seq(high_water.saturating_add(1));
            self.last_event_seq = Some(high_water);
        }
        self
    }

    /// Launches the first turn's process and reads its prelude until the
    /// decoder captures the runtime session id.
    ///
    /// The prelude captures the id (needed to register and later resume the
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
    /// cancellation, or `Launch` when a fresh session never reports a session
    /// id.
    pub(crate) async fn begin(
        &mut self,
        spec: &TurnSpec,
        first: FirstLaunch,
        ctx: &RunContext,
        prelude_timeout: Duration,
    ) -> Result<(), ExternalAgentError> {
        let labels = F::labels();
        let stream = self
            .launcher
            .launch(spec)
            .await
            .map_err(|error| match &first {
                FirstLaunch::Fresh => ExternalAgentError::Launch {
                    runtime: F::runtime(),
                    detail: format!("{}: {:?}", labels.spawn_failed, error.kind()),
                },
                FirstLaunch::Resume(session) => ExternalAgentError::ResumeUnavailable {
                    session: session.clone(),
                    detail: format!("{}: {:?}", labels.resume_spawn_failed, error.kind()),
                },
            })?;
        self.current = Some(stream);
        self.first_turn_pending = true;

        // A resume already knows its id, so pre-seed it: the session must expose a
        // non-empty id to be registered even if the resumed session never
        // re-reports its id before settling.
        if let FirstLaunch::Resume(session) = &first
            && let Some(id) = &session.session_id
        {
            self.session_id = id.clone();
        }

        let prelude = PreludeDeadline::new(prelude_timeout);
        // Fresh vs resume classification axis, shared by both deadline guards.
        let deadline_error = |first: &FirstLaunch| match first {
            FirstLaunch::Fresh => ExternalAgentError::Launch {
                runtime: F::runtime(),
                detail: labels.id_timeout.to_owned(),
            },
            FirstLaunch::Resume(session) => ExternalAgentError::ResumeUnavailable {
                session: session.clone(),
                detail: labels.resume_id_timeout.to_owned(),
            },
        };
        while self.decoder.session_id().is_none() {
            prelude.check_active(
                ctx,
                self.maybe_session_ref(),
                labels.begin_cancelled,
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
                runtime: F::runtime(),
                detail: labels.id_missing.to_owned(),
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
                detail: F::labels().no_open_turn.to_owned(),
            });
        };
        stream
            .read_frame()
            .await
            .map_err(|error| ExternalAgentError::SessionLost {
                session: self.maybe_session_ref(),
                detail: format!("{}: {:?}", F::labels().read_failed, error.kind()),
            })
    }

    /// Drains the decoder's buffered observations, mirroring each to the live
    /// sink and advancing the high-water `seq`.
    fn drain_and_emit(&mut self) -> Vec<ExternalObservedEvent> {
        let observed = self.decoder.take_observations();
        emit_observations(&observed, self.sink.as_ref(), &mut self.last_event_seq);
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
        record_mid_session_close(
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
        let message = super::autonomous_turn_message(&self.capabilities, input, F::labels().turn)?;

        if let Some(mut old) = self.current.take() {
            let disposition = old.close().await;
            self.note_close(ctx, disposition);
        }

        let spec = TurnSpec::Resume {
            session_id: self.session_id.clone(),
            message,
        };
        let stream =
            self.launcher
                .launch(&spec)
                .await
                .map_err(|error| ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: format!("{}: {:?}", F::labels().resume_spawn_failed, error.kind()),
                })?;
        self.current = Some(stream);
        Ok(())
    }

    /// Folds a settled turn decision into a [`RuntimeDecisionPoint`].
    ///
    /// A one-shot CLI never pauses for the host, so a turn only ever completes
    /// or fails.
    fn finish(
        &self,
        decision: <F::Decoder as Decoder>::Decision,
        observations: Vec<ExternalObservedEvent>,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        match F::Decoder::decision_result(decision) {
            Ok(output) => Ok(RuntimeDecisionPoint::Completed {
                session: self.session_ref(),
                output,
                observations,
            }),
            Err(error) => Err(error),
        }
    }

    /// Returns the session facts, or `None` before a session id has been assigned.
    fn maybe_session_ref(&self) -> Option<ExternalSessionRef> {
        maybe_session_ref_for_id(F::runtime(), &self.session_id, self.last_event_seq)
    }
}

#[async_trait]
impl<F: Flavor, L: Launcher> ExternalRuntimeSession for Session<F, L> {
    fn session_ref(&self) -> ExternalSessionRef {
        session_ref_for_id(F::runtime(), &self.session_id, self.last_event_seq)
    }

    async fn advance(
        &mut self,
        input: &ExternalSessionInput,
        ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        let labels = F::labels();
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
                    detail: labels.advance_cancelled.to_owned(),
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
                        detail: labels.closed_before_decision.to_owned(),
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
