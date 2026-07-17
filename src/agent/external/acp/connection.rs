//! ACP client **connection layer** (M10-2, feature `external-acp`).
//!
//! The decoder ([`AcpStreamDecoder`](super::AcpStreamDecoder)) turns agent→client
//! wire lines into neutral observations; this module owns the other half — the
//! process/transport plumbing that produces those lines. It mirrors the IO
//! discipline of the three CLI adapters: a child ACP **agent** is spawned with
//! stdin/stdout piped, stderr discarded (so a chatty agent cannot leak a
//! credential into our logs), `kill_on_drop` armed, and every read bounded by a
//! timeout so a hung agent surfaces as a classified
//! [`SessionLost`](ExternalAgentError::SessionLost) rather than blocking forever.
//!
//! Spawning goes through the injectable [`AcpLauncher`] trait: production uses
//! [`TokioProcessLauncher`] (real `tokio::process`), while offline tests inject a
//! fake launcher that hands back a [`SpawnedAcpAgent`] wrapping in-memory
//! streams. The live adapter that drives `initialize` / `session/new` /
//! `session/prompt` over this transport and folds decoder decisions into
//! [`RuntimeDecisionPoint`](crate::agent::external::RuntimeDecisionPoint)s lands
//! in M10-3; this task freezes only the launch + framed IO.

use std::fmt;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::agent::external::ExternalAgentError;

use super::{AcpConfig, acp_runtime_kind};

/// Spawns an ACP agent subprocess and attaches a framed JSON-RPC transport.
///
/// The trait exists so the live adapter's IO can be injected: production wires
/// [`TokioProcessLauncher`], while tests supply a fake that returns a
/// [`SpawnedAcpAgent`] built from in-memory streams, keeping the whole
/// initialize/prompt/permission loop drivable offline with no real agent binary.
#[async_trait]
pub trait AcpLauncher: Send + Sync {
    /// Launches the agent described by `config` and returns its transport.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Launch`] when the agent cannot be spawned or
    /// its stdio pipes are unavailable.
    async fn launch(&self, config: &AcpConfig) -> Result<SpawnedAcpAgent, ExternalAgentError>;
}

/// A spawned ACP agent's line-framed JSON-RPC transport.
///
/// Reads and writes newline-delimited JSON-RPC messages. Reads are bounded by the
/// configured timeout; a timed-out or dropped connection is classified as
/// [`SessionLost`](ExternalAgentError::SessionLost). When built from a real child
/// the process handle is retained so `kill_on_drop` reaps it on drop.
pub struct SpawnedAcpAgent {
    writer: Box<dyn AsyncWrite + Send + Unpin>,
    reader: BufReader<Box<dyn AsyncRead + Send + Unpin>>,
    read_timeout: Duration,
    // Retained purely so the spawned child is killed when the transport drops.
    child: Option<Child>,
}

impl fmt::Debug for SpawnedAcpAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpawnedAcpAgent")
            .field("read_timeout", &self.read_timeout)
            .field("has_child", &self.child.is_some())
            .finish_non_exhaustive()
    }
}

impl SpawnedAcpAgent {
    /// Builds a transport over arbitrary async streams (for injected fakes).
    #[must_use]
    pub fn new<W, R>(writer: W, reader: R, read_timeout: Duration) -> Self
    where
        W: AsyncWrite + Send + Unpin + 'static,
        R: AsyncRead + Send + Unpin + 'static,
    {
        let reader: Box<dyn AsyncRead + Send + Unpin> = Box::new(reader);
        Self {
            writer: Box::new(writer),
            reader: BufReader::new(reader),
            read_timeout,
            child: None,
        }
    }

    /// Builds a transport over a spawned child's piped stdio, retaining the child
    /// so `kill_on_drop` reaps it when the transport is dropped.
    fn from_child(
        child: Child,
        stdin: ChildStdin,
        stdout: ChildStdout,
        read_timeout: Duration,
    ) -> Self {
        let reader: Box<dyn AsyncRead + Send + Unpin> = Box::new(stdout);
        Self {
            writer: Box::new(stdin),
            reader: BufReader::new(reader),
            read_timeout,
            child: Some(child),
        }
    }

    /// Writes one JSON-RPC message as a newline-terminated line and flushes it.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::SessionLost`] when the write or flush fails
    /// (the agent's stdin closed).
    pub async fn write_line(&mut self, line: &str) -> Result<(), ExternalAgentError> {
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(|error| session_lost(format!("acp transport write failed: {error}")))?;
        self.writer
            .write_all(b"\n")
            .await
            .map_err(|error| session_lost(format!("acp transport write failed: {error}")))?;
        self.writer
            .flush()
            .await
            .map_err(|error| session_lost(format!("acp transport flush failed: {error}")))
    }

    /// Reads the next JSON-RPC line, bounded by the read timeout.
    ///
    /// Returns `Ok(None)` at end of stream (the agent closed stdout) and the line
    /// (without its trailing newline) otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::SessionLost`] when the read fails or exceeds
    /// the configured timeout.
    pub async fn read_line(&mut self) -> Result<Option<String>, ExternalAgentError> {
        let mut line = String::new();
        let read = tokio::time::timeout(self.read_timeout, self.reader.read_line(&mut line))
            .await
            .map_err(|_| session_lost("acp transport read timed out"))?
            .map_err(|error| session_lost(format!("acp transport read failed: {error}")))?;
        if read == 0 {
            return Ok(None);
        }
        while line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        Ok(Some(line))
    }
}

/// Production [`AcpLauncher`] that spawns a real ACP agent via `tokio::process`.
///
/// The child inherits the resolved environment ([`AcpConfig::resolved_env`]) so a
/// logged-in CLI "just works", runs in the configured working dir (the worktree),
/// pipes stdin/stdout, discards stderr, and is armed with `kill_on_drop`.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioProcessLauncher;

#[async_trait]
impl AcpLauncher for TokioProcessLauncher {
    async fn launch(&self, config: &AcpConfig) -> Result<SpawnedAcpAgent, ExternalAgentError> {
        let mut command = Command::new(config.binary());
        command.args(config.args());
        command.env_clear();
        for (key, value) in config.resolved_env(std::env::vars()) {
            command.env(key, value);
        }
        if let Some(dir) = config.working_dir() {
            command.current_dir(dir);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|error| launch_error(format!("failed to spawn acp agent: {error}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| launch_error("acp agent stdin was not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| launch_error("acp agent stdout was not piped"))?;
        Ok(SpawnedAcpAgent::from_child(
            child,
            stdin,
            stdout,
            config.timeout(),
        ))
    }
}

/// Builds an [`ExternalAgentError::Launch`] tagged with the ACP runtime kind.
fn launch_error(detail: impl Into<String>) -> ExternalAgentError {
    ExternalAgentError::Launch {
        runtime: acp_runtime_kind(),
        detail: detail.into(),
    }
}

/// Builds an [`ExternalAgentError::SessionLost`] from a fixed diagnostic.
fn session_lost(detail: impl Into<String>) -> ExternalAgentError {
    ExternalAgentError::SessionLost {
        session: None,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::{AcpLauncher, SpawnedAcpAgent};
    use crate::agent::external::acp::{AcpConfig, AcpStreamDecoder};
    use crate::agent::external::{ExternalAgentError, ExternalAgentEvent};

    /// A fake launcher that hands back a transport reading canned agent lines and
    /// discarding writes — proving [`AcpLauncher`] is injectable for offline tests.
    struct FakeLauncher {
        lines: String,
    }

    #[async_trait]
    impl AcpLauncher for FakeLauncher {
        async fn launch(&self, _config: &AcpConfig) -> Result<SpawnedAcpAgent, ExternalAgentError> {
            let reader = Cursor::new(self.lines.clone().into_bytes());
            Ok(SpawnedAcpAgent::new(
                tokio::io::sink(),
                reader,
                Duration::from_secs(5),
            ))
        }
    }

    /// The injected transport streams every canned line into the decoder and the
    /// writer accepts a JSON-RPC line.
    #[tokio::test]
    async fn fake_launcher_transport_feeds_decoder() {
        let lines = [
            r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"s1"}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn"}}"#,
        ]
        .join("\n");
        let launcher = FakeLauncher { lines };

        let mut agent = launcher
            .launch(&AcpConfig::opencode_acp())
            .await
            .expect("fake launch succeeds");

        // Writing a request goes to the sink without error.
        agent
            .write_line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)
            .await
            .expect("write succeeds");

        let mut decoder = AcpStreamDecoder::new();
        let mut settled = false;
        while let Some(line) = agent.read_line().await.expect("read succeeds") {
            if decoder
                .push_jsonrpc_line(&line)
                .expect("line decodes")
                .is_some()
            {
                settled = true;
            }
        }
        assert!(settled, "the prompt result settles the turn");

        let observations = decoder.take_observations();
        assert_eq!(observations.len(), 3);
        assert_eq!(
            observations[0].event,
            ExternalAgentEvent::SessionStarted {
                session_id: Some("s1".to_owned()),
            },
        );
        assert_eq!(observations[2].event, ExternalAgentEvent::SessionCompleted);
        assert_eq!(decoder.session_id(), Some("s1"));
    }

    /// A read that never yields is bounded by the timeout and classified as
    /// `SessionLost` rather than hanging.
    #[tokio::test]
    async fn read_line_times_out_into_session_lost() {
        // Hold the far end of a duplex open but never write, so the read blocks.
        let (mine, _theirs) = tokio::io::duplex(64);
        let (reader, writer) = tokio::io::split(mine);
        let mut agent = SpawnedAcpAgent::new(writer, reader, Duration::from_millis(50));

        match agent.read_line().await {
            Err(ExternalAgentError::SessionLost { detail, .. }) => {
                assert!(detail.contains("timed out"));
            }
            other => panic!("expected a SessionLost timeout, got {other:?}"),
        }
    }

    /// End of stream is reported as `Ok(None)`.
    #[tokio::test]
    async fn read_line_reports_eof() {
        let mut agent = SpawnedAcpAgent::new(
            tokio::io::sink(),
            tokio::io::empty(),
            Duration::from_secs(1),
        );
        assert_eq!(agent.read_line().await.expect("eof read"), None);
    }
}
