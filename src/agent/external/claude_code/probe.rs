//! Capability probe for the managed Claude Code runtime adapter.
//!
//! Before a host dispatches managed work to Claude Code it must learn, without
//! assuming anything, whether the local CLI is present and which managed
//! features it can serve. [`probe`] answers that: it invokes the configured
//! binary's `--version` and `--help`, classifies a missing or broken binary as
//! [`ExternalAgentError::Launch`], classifies a binary that cannot emit the
//! structured `stream-json` protocol the managed adapter relies on as
//! [`ExternalAgentError::UnsupportedCapability`], and otherwise reports a
//! conservatively-detected [`ExternalRuntimeCapabilities`] set (design §12.1,
//! §15). It never panics.
//!
//! # Offline testability
//!
//! The probe is written against the [`ClaudeCodeProbeExec`] trait rather than
//! spawning a process directly. Production uses [`SystemClaudeCodeExec`], which
//! runs the real CLI through [`tokio::process`]; tests inject a fake exec that
//! returns canned `--version` / `--help` output, so the whole error-
//! classification surface is exercised with no Claude Code install and no
//! network. The default `cargo test --all --all-targets` build does not compile
//! this module at all, because the whole adapter is gated behind the
//! `external-claude-code` feature.
//!
//! # Secret hygiene
//!
//! Neither the probe nor its errors embed environment values or raw CLI output.
//! Error `detail` strings name only stable, non-secret facts (the binary path, a
//! classified [`io::ErrorKind`], the missing capability), so a probe failure
//! surfaced to a log cannot leak a credential (design constraint "任何可能包含
//! secret … 的日志/错误必须脱敏").

use std::io;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::timeout;

use crate::agent::external::{
    ExternalAgentError, ExternalCapability, ExternalRuntimeCapabilities, ExternalRuntimeKind,
};

use super::ClaudeCodeConfig;

/// Captured result of one probe subcommand invocation.
///
/// This is the provider-neutral shape a [`ClaudeCodeProbeExec`] returns for a
/// single `claude <args>` run: whether the process exited successfully and its
/// captured `stdout` / `stderr` decoded as lossy UTF-8. The probe inspects the
/// text but never echoes it into an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeOutput {
    /// Whether the process exited with a success status.
    pub success: bool,
    /// Captured standard output, decoded lossily.
    pub stdout: String,
    /// Captured standard error, decoded lossily.
    pub stderr: String,
}

impl ProbeOutput {
    /// Returns the combined `stdout` + `stderr` text.
    ///
    /// A CLI may print its `--help` to either stream (or split it across both),
    /// so feature detection scans the union rather than a single stream.
    fn combined(&self) -> String {
        let mut text = String::with_capacity(self.stdout.len() + self.stderr.len() + 1);
        text.push_str(&self.stdout);
        if !self.stdout.is_empty() && !self.stderr.is_empty() {
            text.push('\n');
        }
        text.push_str(&self.stderr);
        text
    }
}

/// Runs Claude Code probe subcommands, abstracting the real process spawn.
///
/// The one method invokes `<binary> <args>` under the config's working
/// directory, environment overrides, and timeout, returning the captured
/// [`ProbeOutput`] or the raw [`io::Error`] from spawning/waiting. Splitting this
/// out lets the probe's classification logic be unit-tested with a fake exec and
/// no real Claude Code binary.
#[async_trait]
pub trait ClaudeCodeProbeExec: Send + Sync {
    /// Runs `<binary> <args>` and captures its output.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the process cannot be spawned
    /// (missing binary, permission denied) or does not complete within the
    /// config's [`timeout`](ClaudeCodeConfig::timeout).
    async fn invoke(&self, config: &ClaudeCodeConfig, args: &[&str]) -> io::Result<ProbeOutput>;
}

/// Production [`ClaudeCodeProbeExec`] that spawns the real CLI via [`tokio::process`].
///
/// It applies the config's working directory and environment overrides, pipes
/// the child's output, kills the child on drop, and bounds the run with the
/// config's timeout. It adds no dependency beyond tokio's process support, which
/// the crate already enables.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClaudeCodeExec;

#[async_trait]
impl ClaudeCodeProbeExec for SystemClaudeCodeExec {
    async fn invoke(&self, config: &ClaudeCodeConfig, args: &[&str]) -> io::Result<ProbeOutput> {
        let mut command = Command::new(config.binary());
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = config.working_dir() {
            command.current_dir(dir);
        }
        for (key, value) in config.env() {
            command.env(key, value);
        }

        let child = command.spawn()?;
        let output = match timeout(config.timeout(), child.wait_with_output()).await {
            Ok(result) => result?,
            Err(_elapsed) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "claude code probe timed out",
                ));
            }
        };

        Ok(ProbeOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Probes the local Claude Code CLI described by `config`.
///
/// Runs the real CLI through [`SystemClaudeCodeExec`]; see
/// [`probe_with_exec`] for the classification contract and
/// [`ExternalRuntimeCapabilities`] shape.
///
/// # Errors
///
/// Returns [`ExternalAgentError::Launch`] when the binary is missing or broken,
/// or [`ExternalAgentError::UnsupportedCapability`] when it cannot emit the
/// `stream-json` protocol the managed adapter requires.
pub async fn probe(
    config: &ClaudeCodeConfig,
) -> Result<ExternalRuntimeCapabilities, ExternalAgentError> {
    probe_with_exec(config, &SystemClaudeCodeExec).await
}

/// Probes Claude Code through an injected `exec`, returning its capabilities.
///
/// The probe runs two subcommands and classifies the outcome:
///
/// 1. `--version` — a spawn/timeout [`io::Error`] or a non-success exit means the
///    binary is missing or broken, classified as
///    [`ExternalAgentError::Launch`].
/// 2. `--help` — a spawn/timeout error, or empty help output, is likewise a
///    [`ExternalAgentError::Launch`]. Otherwise the help text is scanned for the
///    managed features the CLI advertises.
///
/// The managed adapter drives Claude Code through its structured `stream-json`
/// protocol; if the CLI does not advertise `--output-format stream-json` /
/// `--input-format`, the probe fails loudly with
/// [`ExternalAgentError::UnsupportedCapability`] for
/// [`ExternalCapability::Streaming`] rather than pretending the runtime is
/// usable. Every other capability is detected conservatively from the help text
/// and defaults to `false` when not clearly advertised (design §15):
///
/// - `permission_bridge` — `--permission-mode` present.
/// - `resume` — `--resume` or `--continue` present.
/// - `host_tools` — `--mcp-config` present (MCP tool injection).
/// - `usage` / `artifacts` — implied by the structured stream, whose result and
///   file-edit frames carry usage and produced-file facts.
/// - `graceful_shutdown` — always `true` for a CLI whose stdin the adapter can
///   close cleanly.
/// - `host_subagents` — left `false`; the spawn bridge is verified in M6-3, not
///   here.
///
/// # Errors
///
/// Returns [`ExternalAgentError::Launch`] or
/// [`ExternalAgentError::UnsupportedCapability`] as described above. It never
/// panics.
pub async fn probe_with_exec(
    config: &ClaudeCodeConfig,
    exec: &dyn ClaudeCodeProbeExec,
) -> Result<ExternalRuntimeCapabilities, ExternalAgentError> {
    let version = exec
        .invoke(config, &["--version"])
        .await
        .map_err(|error| launch_error(config, "querying --version", &error))?;
    if !version.success {
        return Err(ExternalAgentError::Launch {
            runtime: ExternalRuntimeKind::ClaudeCode,
            detail: format!(
                "claude code binary {} exited unsuccessfully for --version",
                config.binary().display()
            ),
        });
    }

    let help = exec
        .invoke(config, &["--help"])
        .await
        .map_err(|error| launch_error(config, "querying --help", &error))?;
    let help_text = help.combined();
    if help_text.trim().is_empty() {
        return Err(ExternalAgentError::Launch {
            runtime: ExternalRuntimeKind::ClaudeCode,
            detail: format!(
                "claude code binary {} produced no --help output to probe",
                config.binary().display()
            ),
        });
    }

    let capabilities = detect_capabilities(&help_text);
    if !capabilities.streaming {
        return Err(capabilities.unsupported(
            ExternalCapability::Streaming,
            "claude code CLI does not advertise --output-format stream-json / --input-format",
        ));
    }

    Ok(capabilities)
}

/// Builds a classified [`ExternalAgentError::Launch`] from a spawn/timeout error.
///
/// The `detail` names the stage and the classified [`io::ErrorKind`] plus the
/// binary path only; it never embeds the config's environment values or the
/// CLI's raw output, so a launch failure cannot leak a secret.
fn launch_error(config: &ClaudeCodeConfig, stage: &str, error: &io::Error) -> ExternalAgentError {
    ExternalAgentError::Launch {
        runtime: ExternalRuntimeKind::ClaudeCode,
        detail: format!(
            "failed launching claude code binary {} while {stage}: {:?}",
            config.binary().display(),
            error.kind()
        ),
    }
}

/// Conservatively derives the managed capabilities from `--help` text.
///
/// Every flag defaults to `false` and is turned on only when the help output
/// clearly advertises the backing feature, so an unrecognized or older CLI never
/// has capabilities assumed on its behalf.
fn detect_capabilities(help: &str) -> ExternalRuntimeCapabilities {
    let has = |needle: &str| help.contains(needle);
    let structured_stream = has("--output-format") && has("stream-json") && has("--input-format");

    let mut capabilities = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::ClaudeCode);
    capabilities.streaming = structured_stream;
    capabilities.permission_bridge = has("--permission-mode");
    capabilities.resume = has("--resume") || has("--continue");
    capabilities.host_tools = has("--mcp-config");
    // The structured stream's result frames report usage/cost and its file-edit
    // frames report produced artifacts, so both ride on `streaming`.
    capabilities.usage = structured_stream;
    capabilities.artifacts = structured_stream;
    // A spawned CLI can always be closed cleanly by dropping its stdin/process.
    capabilities.graceful_shutdown = true;
    // `host_subagents` stays false until the spawn bridge is verified in M6-3.
    capabilities
}

#[cfg(test)]
mod tests {
    use super::{
        ClaudeCodeProbeExec, ProbeOutput, SystemClaudeCodeExec, detect_capabilities, probe,
        probe_with_exec,
    };
    use crate::agent::external::{
        ClaudeCodeConfig, ExternalAgentError, ExternalCapability, ExternalRuntimeKind,
    };
    use async_trait::async_trait;
    use std::io;
    use std::sync::Mutex;

    /// A canned response for one probe subcommand: either an IO failure or a
    /// captured [`ProbeOutput`].
    #[derive(Clone)]
    enum FakeResponse {
        Io(io::ErrorKind),
        Output(ProbeOutput),
    }

    impl FakeResponse {
        fn ok(stdout: &str) -> Self {
            FakeResponse::Output(ProbeOutput {
                success: true,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            })
        }

        fn into_result(self) -> io::Result<ProbeOutput> {
            match self {
                FakeResponse::Io(kind) => Err(io::Error::new(kind, "fake probe io error")),
                FakeResponse::Output(output) => Ok(output),
            }
        }
    }

    /// Offline [`ClaudeCodeProbeExec`] returning canned per-subcommand responses
    /// and recording the args it was handed.
    struct FakeExec {
        version: FakeResponse,
        help: FakeResponse,
        seen_args: Mutex<Vec<Vec<String>>>,
    }

    impl FakeExec {
        fn new(version: FakeResponse, help: FakeResponse) -> Self {
            Self {
                version,
                help,
                seen_args: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ClaudeCodeProbeExec for FakeExec {
        async fn invoke(
            &self,
            _config: &ClaudeCodeConfig,
            args: &[&str],
        ) -> io::Result<ProbeOutput> {
            self.seen_args
                .lock()
                .expect("record args")
                .push(args.iter().map(|a| (*a).to_owned()).collect());
            match args.first().copied() {
                Some("--version") => self.version.clone().into_result(),
                Some("--help") => self.help.clone().into_result(),
                other => panic!("unexpected probe args: {other:?}"),
            }
        }
    }

    /// A `--help` fixture that advertises the full managed feature set.
    const FULL_HELP: &str = "\
Usage: claude [options]
  --print                 print mode
  --output-format <fmt>   output format (text, json, stream-json)
  --input-format <fmt>    input format (text, stream-json)
  --permission-mode <m>   permission mode (default, acceptEdits, plan, bypassPermissions)
  --resume <id>           resume a session
  --continue              continue the most recent session
  --model <name>          model to use
  --mcp-config <path>     load MCP servers from a config file
";

    #[tokio::test]
    async fn claude_code_probe_detects_full_capabilities() {
        let exec = FakeExec::new(
            FakeResponse::ok("1.2.3 (Claude Code)"),
            FakeResponse::ok(FULL_HELP),
        );
        let config = ClaudeCodeConfig::new();

        let caps = probe_with_exec(&config, &exec)
            .await
            .expect("probe succeeds");
        assert_eq!(caps.runtime, ExternalRuntimeKind::ClaudeCode);
        assert!(caps.streaming);
        assert!(caps.permission_bridge);
        assert!(caps.resume);
        assert!(caps.host_tools);
        assert!(caps.usage);
        assert!(caps.artifacts);
        assert!(caps.graceful_shutdown);
        // The spawn bridge is only verified in M6-3, so it stays conservative.
        assert!(!caps.host_subagents);

        // Both subcommands were probed, version first.
        let seen = exec.seen_args.lock().expect("args");
        assert_eq!(seen.as_slice(), &[vec!["--version"], vec!["--help"]]);
    }

    #[tokio::test]
    async fn claude_code_probe_missing_binary_is_launch_error() {
        let exec = FakeExec::new(
            FakeResponse::Io(io::ErrorKind::NotFound),
            FakeResponse::ok(FULL_HELP),
        );
        let config = ClaudeCodeConfig::new().with_binary("/no/such/claude");

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::Launch { runtime, detail }) => {
                assert_eq!(runtime, ExternalRuntimeKind::ClaudeCode);
                assert!(detail.contains("/no/such/claude"));
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_code_probe_nonzero_version_is_launch_error() {
        let exec = FakeExec::new(
            FakeResponse::Output(ProbeOutput {
                success: false,
                stdout: String::new(),
                stderr: "boom".to_owned(),
            }),
            FakeResponse::ok(FULL_HELP),
        );
        let config = ClaudeCodeConfig::new();

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::Launch { .. }) => {}
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_code_probe_empty_help_is_launch_error() {
        let exec = FakeExec::new(FakeResponse::ok("1.2.3"), FakeResponse::ok("   \n  "));
        let config = ClaudeCodeConfig::new();

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::Launch { .. }) => {}
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_code_probe_without_stream_json_is_unsupported() {
        // A CLI that lacks structured stream support fails loudly rather than
        // being reported as a usable managed runtime.
        let plain_help = "\
Usage: claude [options]
  --print                 print mode
  --output-format <fmt>   output format (text, json)
  --permission-mode <m>   permission mode
";
        let exec = FakeExec::new(FakeResponse::ok("1.2.3"), FakeResponse::ok(plain_help));
        let config = ClaudeCodeConfig::new();

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::UnsupportedCapability {
                runtime,
                capability,
                ..
            }) => {
                assert_eq!(runtime, ExternalRuntimeKind::ClaudeCode);
                assert_eq!(capability, ExternalCapability::Streaming);
            }
            other => panic!("expected UnsupportedCapability, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_code_probe_does_not_leak_env_secret() {
        // Even with an env secret configured, neither a launch failure nor an
        // unsupported-capability failure may embed the secret value.
        let secret = "sk-super-secret-key";

        let launch_exec = FakeExec::new(
            FakeResponse::Io(io::ErrorKind::PermissionDenied),
            FakeResponse::ok(FULL_HELP),
        );
        let config = ClaudeCodeConfig::new().with_env("ANTHROPIC_API_KEY", secret);
        let launch_err = probe_with_exec(&config, &launch_exec)
            .await
            .expect_err("launch failure");
        assert!(!format!("{launch_err}").contains(secret));
        assert!(!format!("{launch_err:?}").contains(secret));

        let unsupported_exec = FakeExec::new(
            FakeResponse::ok("1.2.3"),
            FakeResponse::ok("--output-format text"),
        );
        let unsupported_err = probe_with_exec(&config, &unsupported_exec)
            .await
            .expect_err("unsupported failure");
        assert!(!format!("{unsupported_err}").contains(secret));
        assert!(!format!("{unsupported_err:?}").contains(secret));
    }

    #[tokio::test]
    async fn claude_code_probe_with_missing_binary_via_system_exec_is_launch() {
        // Exercises the real SystemClaudeCodeExec spawn path offline: a binary
        // that cannot exist must classify as Launch and must not panic.
        let _ = SystemClaudeCodeExec;
        let config =
            ClaudeCodeConfig::new().with_binary("claude-code-probe-nonexistent-binary-xyz");
        match probe(&config).await {
            Err(ExternalAgentError::Launch { runtime, .. }) => {
                assert_eq!(runtime, ExternalRuntimeKind::ClaudeCode);
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[test]
    fn detect_capabilities_defaults_unadvertised_features_off() {
        // A help text advertising only the structured stream turns on streaming
        // (plus its implied usage/artifacts and graceful shutdown) and leaves
        // every unadvertised feature off.
        let caps = detect_capabilities("--output-format stream-json --input-format stream-json");
        assert!(caps.streaming);
        assert!(caps.usage);
        assert!(caps.artifacts);
        assert!(caps.graceful_shutdown);
        assert!(!caps.permission_bridge);
        assert!(!caps.resume);
        assert!(!caps.host_tools);
        assert!(!caps.host_subagents);
    }
}
