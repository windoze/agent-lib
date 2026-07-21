//! Shared child-process plumbing for managed external runtimes (M8-2).
//!
//! The concrete CLI adapters still own their argv construction and decoder
//! wiring, but their subprocess lifecycle is intentionally identical: spawn a
//! managed process group, read newline-delimited stdout with an idle timeout,
//! close by waiting for a graceful exit and then force-killing the group, and
//! keep session observations/supported-capability checks consistent. The
//! managed CLI runtimes additionally share their capability-probe protocol
//! ([`probe`]) and the envelope of their newline-delimited JSON decoders
//! ([`jsonl`]); the two one-shot-per-turn runtimes (Codex, OpenCode) further
//! share their whole session state machine ([`oneshot`]).

// These helpers return the external adapter's canonical error enum to match the
// surrounding adapter modules. Boxing only here would make the shared API less
// useful and would not reduce the enum carried by the rest of the external stack.
#![allow(clippy::result_large_err)]

mod group;

#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
pub(crate) mod jsonl;

#[cfg(any(feature = "external-codex", feature = "external-opencode"))]
pub(crate) mod oneshot;

#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
pub(crate) mod probe;

use std::io;
use std::sync::Arc;
use std::time::Duration;

#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
use std::{future::Future, process::Stdio};

use tokio::io::{AsyncBufRead, AsyncBufReadExt};
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Child;
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::time::timeout;
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
use tokio::time::{Instant, timeout_at};

#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
use crate::agent::{RunContext, TraceNodeId};

#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
use super::ExternalSessionInput;
use super::{
    ExternalAgentError, ExternalCapability, ExternalEventSink, ExternalObservedEvent,
    ExternalRuntimeCapabilities, ExternalRuntimeKind, ExternalSessionRef, ExternalSessionRequest,
    ExternalSessionShutdown,
};

pub(crate) use group::{configure_managed_command, force_kill};

/// Asserts that a unix process group has no surviving members.
#[cfg(all(test, unix))]
pub(crate) use group::assert_process_group_reaped;

/// Whether a managed child should expose a writable stdin pipe.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChildStdinMode {
    /// The runtime receives host turns through stdin.
    Piped,
    /// The runtime reads all input from argv and must not inherit host stdin.
    Null,
}

/// A line-oriented managed child process.
///
/// This is the common production transport for the CLI adapters. It owns the
/// child handle, optional stdin, and stdout reader; process-group setup, read
/// timeout, exit-code classification, and force-kill fallback live here so the
/// M1/M2 lifecycle fixes remain single-sourced.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
pub(crate) struct ManagedChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    read_timeout: Duration,
    shutdown_grace: Duration,
    read_timeout_message: &'static str,
}

#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
impl ManagedChild {
    /// Spawns `command` as a managed line-oriented child.
    ///
    /// The caller supplies argv, env, and working directory first; this helper
    /// owns stdio, `kill_on_drop`, process-group setup, pipe extraction, and the
    /// lifecycle timeouts.
    pub(crate) fn spawn(
        mut command: Command,
        stdin_mode: ChildStdinMode,
        read_timeout: Duration,
        shutdown_grace: Duration,
        stdout_missing: &'static str,
        read_timeout_message: &'static str,
    ) -> io::Result<Self> {
        match stdin_mode {
            ChildStdinMode::Piped => {
                command.stdin(Stdio::piped());
            }
            ChildStdinMode::Null => {
                command.stdin(Stdio::null());
            }
        }
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        configure_managed_command(&mut command);

        let mut child = command.spawn()?;
        let stdin = match stdin_mode {
            ChildStdinMode::Piped => child.stdin.take(),
            ChildStdinMode::Null => None,
        };
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, stdout_missing))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            read_timeout,
            shutdown_grace,
            read_timeout_message,
        })
    }

    /// Returns the OS process id of the child, when still available.
    #[cfg(test)]
    pub(crate) fn child_id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Writes one newline-terminated frame to stdin and flushes it.
    pub(crate) async fn write_line(
        &mut self,
        line: &str,
        stdin_closed_message: &'static str,
    ) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, stdin_closed_message))?;
        write_line(stdin, line).await
    }

    /// Reads one stdout line, bounded by this child's read-idle timeout.
    pub(crate) async fn read_line(&mut self) -> io::Result<Option<String>> {
        read_line_with_timeout(
            &mut self.stdout,
            self.read_timeout,
            self.read_timeout_message,
        )
        .await
    }

    /// Closes the child, classifying exit status and force-kill outcome.
    pub(crate) async fn close(&mut self) -> ExternalSessionShutdown {
        self.stdin = None;
        close_child(&mut self.child, self.shutdown_grace).await
    }
}

/// Writes one newline-terminated line to an async writer and flushes it.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
pub(crate) async fn write_line<W>(writer: &mut W, line: &str) -> io::Result<()>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

/// Reads one line from `reader`, returning `None` at EOF and trimming line ends.
pub(crate) async fn read_line_with_timeout<R>(
    reader: &mut R,
    read_timeout: Duration,
    timeout_message: &'static str,
) -> io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin + ?Sized,
{
    let mut line = String::new();
    let read = timeout(read_timeout, reader.read_line(&mut line))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, timeout_message))??;
    if read == 0 {
        return Ok(None);
    }
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    Ok(Some(line))
}

/// Closes a child using the shared graceful-wait / forced-kill classification.
pub(crate) async fn close_child(child: &mut Child, grace: Duration) -> ExternalSessionShutdown {
    match timeout(grace, child.wait()).await {
        Ok(Ok(status)) if status.success() => ExternalSessionShutdown::Graceful,
        Ok(Ok(_)) | Ok(Err(_)) => ExternalSessionShutdown::Failed,
        Err(_elapsed) => match force_kill(child).await {
            Ok(()) => ExternalSessionShutdown::ForcedKill,
            Err(_error) => ExternalSessionShutdown::Failed,
        },
    }
}

/// Deadline guard for startup preludes that must not loop forever.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
pub(crate) struct PreludeDeadline {
    deadline: Instant,
}

#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
impl PreludeDeadline {
    /// Starts a wall-clock deadline lasting `timeout` from now.
    pub(crate) fn new(timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
        }
    }

    /// Checks cancellation and the explicit wall-clock deadline.
    pub(crate) fn check_active(
        &self,
        ctx: &RunContext,
        session: Option<ExternalSessionRef>,
        cancel_detail: &'static str,
        timeout_error: impl FnOnce() -> ExternalAgentError,
    ) -> Result<(), ExternalAgentError> {
        if ctx.is_cancelled() {
            return Err(ExternalAgentError::SessionLost {
                session,
                detail: cancel_detail.to_owned(),
            });
        }
        if Instant::now() >= self.deadline {
            return Err(timeout_error());
        }
        Ok(())
    }

    /// Awaits `future` but caps it at the prelude deadline.
    pub(crate) async fn await_until<T>(
        &self,
        future: impl Future<Output = Result<T, ExternalAgentError>>,
        timeout_error: impl FnOnce() -> ExternalAgentError,
    ) -> Result<T, ExternalAgentError> {
        match timeout_at(self.deadline, future).await {
            Ok(result) => result,
            Err(_elapsed) => Err(timeout_error()),
        }
    }
}

/// Emits observations to the optional live sink and advances the high-water mark.
pub(crate) fn emit_observations(
    observed: &[ExternalObservedEvent],
    sink: Option<&Arc<dyn ExternalEventSink>>,
    last_event_seq: &mut Option<u64>,
) {
    for event in observed {
        if let Some(sink) = sink {
            sink.emit(event);
        }
        *last_event_seq = Some(event.seq);
    }
}

/// Builds the standard session reference for runtimes whose resume token is the id.
pub(crate) fn session_ref_for_id(
    runtime: ExternalRuntimeKind,
    session_id: &str,
    last_event_seq: Option<u64>,
) -> ExternalSessionRef {
    let session_id = (!session_id.is_empty()).then(|| session_id.to_owned());
    ExternalSessionRef {
        runtime,
        session_id: session_id.clone(),
        transcript_ref: None,
        resume_token: session_id,
        last_event_seq,
    }
}

/// Returns the standard session reference only after a runtime id is assigned.
pub(crate) fn maybe_session_ref_for_id(
    runtime: ExternalRuntimeKind,
    session_id: &str,
    last_event_seq: Option<u64>,
) -> Option<ExternalSessionRef> {
    (!session_id.is_empty()).then(|| session_ref_for_id(runtime, session_id, last_event_seq))
}

/// Intersects two capability sets field-by-field, keeping the left runtime.
pub(crate) fn intersect_capabilities(
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

/// Refuses declared host tools when an adapter cannot inject them.
pub(crate) fn reject_unsupported_tools(
    capabilities: &ExternalRuntimeCapabilities,
    request: &ExternalSessionRequest,
    detail: &'static str,
) -> Result<(), ExternalAgentError> {
    if !request.tools.is_empty() && !capabilities.host_tools {
        return Err(capabilities.unsupported(ExternalCapability::HostTools, detail));
    }
    Ok(())
}

/// Diagnostics for runtimes that accept only autonomous text turns.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct AutonomousTurnMessages {
    pub(crate) interaction: &'static str,
    pub(crate) tool_results: &'static str,
    pub(crate) subagent: &'static str,
    pub(crate) shutdown: &'static str,
}

/// Extracts text from a start/continue input for autonomous one-shot CLIs.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
pub(crate) fn autonomous_turn_message(
    capabilities: &ExternalRuntimeCapabilities,
    input: &ExternalSessionInput,
    messages: AutonomousTurnMessages,
) -> Result<String, ExternalAgentError> {
    match input {
        ExternalSessionInput::Start { prompt } => Ok(prompt.clone()),
        ExternalSessionInput::Continue { message } => Ok(message.clone()),
        ExternalSessionInput::RespondInteraction { .. } => {
            Err(capabilities
                .unsupported(ExternalCapability::PermissionBridge, messages.interaction))
        }
        ExternalSessionInput::RespondToolResults { .. } => {
            Err(capabilities.unsupported(ExternalCapability::HostTools, messages.tool_results))
        }
        ExternalSessionInput::RespondSubagent { .. } => {
            Err(capabilities.unsupported(ExternalCapability::HostSubagents, messages.subagent))
        }
        ExternalSessionInput::Shutdown => Err(ExternalAgentError::Protocol {
            detail: messages.shutdown.to_owned(),
        }),
    }
}

/// Records and folds a mid-session child close disposition.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
pub(crate) fn record_mid_session_close(
    ctx: &RunContext,
    close_trace_seq: &mut u64,
    worst_close: &mut Option<ExternalSessionShutdown>,
    disposition: ExternalSessionShutdown,
) {
    let seq = *close_trace_seq;
    *close_trace_seq += 1;
    let id = TraceNodeId::new(format!("external-shutdown/{}/{seq}", ctx.run_id()));
    let _ = ctx.trace().record_external_shutdown(id, disposition);
    *worst_close = Some(match *worst_close {
        Some(worst) => worst.merge(disposition),
        None => disposition,
    });
}
