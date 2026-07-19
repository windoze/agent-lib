//! Capability probe for the managed Codex runtime adapter.
//!
//! Before a host dispatches managed work to Codex it must learn, without
//! assuming anything, whether the local CLI is present and which managed
//! features it can serve. [`probe`] answers that: it invokes the configured
//! binary's `--version`, `--help`, and `exec --help`, classifies a missing or
//! broken binary as [`ExternalAgentError::Launch`], classifies a binary that
//! cannot emit the structured `--json` event stream the managed adapter relies
//! on as [`ExternalAgentError::UnsupportedCapability`], and otherwise reports a
//! conservatively-detected [`ExternalRuntimeCapabilities`] set (design §12,
//! §15). It never panics.
//!
//! # Why three subcommands
//!
//! The Codex CLI splits its flags across the top level and the `exec`
//! subcommand: the structured `--json` stream lives on `codex exec`, while the
//! approval policy (`-a/--ask-for-approval`) and MCP-server management (`mcp`)
//! live on the top-level command. The probe therefore reads `--help` *and*
//! `exec --help` and detects capabilities from the union, so nothing is missed
//! just because it sits on the other help page.
//!
//! # Offline testability
//!
//! The probe is written against the [`CodexProbeExec`] trait rather than
//! spawning a process directly. Production uses [`SystemCodexExec`], which runs
//! the real CLI through [`tokio::process`]; tests inject a fake exec that returns
//! canned help output, so the whole error-classification surface is exercised
//! with no Codex install and no network. The default
//! `cargo test --all --all-targets` build does not compile this module at all,
//! because the whole adapter is gated behind the `external-codex` feature.
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

use super::CodexConfig;

/// Captured result of one probe subcommand invocation.
///
/// This is the provider-neutral shape a [`CodexProbeExec`] returns for a single
/// `codex <args>` run: whether the process exited successfully and its captured
/// `stdout` / `stderr` decoded as lossy UTF-8. The probe inspects the text but
/// never echoes it into an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexProbeOutput {
    /// Whether the process exited with a success status.
    pub success: bool,
    /// Captured standard output, decoded lossily.
    pub stdout: String,
    /// Captured standard error, decoded lossily.
    pub stderr: String,
}

impl CodexProbeOutput {
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

/// Runs Codex probe subcommands, abstracting the real process spawn.
///
/// The one method invokes `<binary> <args>` under the config's working
/// directory, environment overrides, and timeout, returning the captured
/// [`CodexProbeOutput`] or the raw [`io::Error`] from spawning/waiting. Splitting
/// this out lets the probe's classification logic be unit-tested with a fake exec
/// and no real Codex binary.
#[async_trait]
pub trait CodexProbeExec: Send + Sync {
    /// Runs `<binary> <args>` and captures its output.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the process cannot be spawned
    /// (missing binary, permission denied) or does not complete within the
    /// config's [`timeout`](CodexConfig::timeout).
    async fn invoke(&self, config: &CodexConfig, args: &[&str]) -> io::Result<CodexProbeOutput>;
}

/// Production [`CodexProbeExec`] that spawns the real CLI via [`tokio::process`].
///
/// It applies the config's working directory and environment overrides, pipes
/// the child's output, kills the child on drop, and bounds the run with the
/// config's timeout. It adds no dependency beyond tokio's process support, which
/// the crate already enables.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCodexExec;

#[async_trait]
impl CodexProbeExec for SystemCodexExec {
    async fn invoke(&self, config: &CodexConfig, args: &[&str]) -> io::Result<CodexProbeOutput> {
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
                    "codex probe timed out",
                ));
            }
        };

        Ok(CodexProbeOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Probes the local Codex CLI described by `config`.
///
/// Runs the real CLI through [`SystemCodexExec`]; see [`probe_with_exec`] for the
/// classification contract and [`ExternalRuntimeCapabilities`] shape.
///
/// # Errors
///
/// Returns [`ExternalAgentError::Launch`] when the binary is missing or broken,
/// or [`ExternalAgentError::UnsupportedCapability`] when it cannot emit the
/// `--json` event stream the managed adapter requires.
pub async fn probe(
    config: &CodexConfig,
) -> Result<ExternalRuntimeCapabilities, ExternalAgentError> {
    probe_with_exec(config, &SystemCodexExec).await
}

/// Probes Codex through an injected `exec`, returning its capabilities.
///
/// The probe runs three subcommands and classifies the outcome:
///
/// 1. `--version` — a spawn/timeout [`io::Error`] or a non-success exit means the
///    binary is missing or broken, classified as
///    [`ExternalAgentError::Launch`].
/// 2. `--help` (top level) and `exec --help` — a spawn/timeout error on either,
///    or empty combined help output, is likewise a
///    [`ExternalAgentError::Launch`]. Otherwise the union of both help texts is
///    scanned for the managed features the CLI advertises.
///
/// The managed adapter drives Codex through its structured `codex exec --json`
/// event stream; if the CLI does not advertise `--json` on `exec`, the probe
/// fails loudly with [`ExternalAgentError::UnsupportedCapability`] for
/// [`ExternalCapability::Streaming`] rather than pretending the runtime is
/// usable. Every other capability is detected conservatively from the help text
/// and defaults to `false` when not clearly advertised (design §15):
///
/// - `permission_bridge` — `--ask-for-approval` or `--sandbox` present.
/// - `resume` — the `resume` subcommand present.
/// - `host_tools` — the `mcp` subcommand present (MCP tool injection).
/// - `usage` / `artifacts` — implied by the structured stream, whose completion
///   and file-change frames carry token usage and produced-file facts.
/// - `graceful_shutdown` — always `true` for a CLI whose stdin the adapter can
///   close cleanly.
/// - `host_subagents` — left `false`; the spawn bridge is verified later, not
///   here.
///
/// # Errors
///
/// Returns [`ExternalAgentError::Launch`] or
/// [`ExternalAgentError::UnsupportedCapability`] as described above. It never
/// panics.
pub async fn probe_with_exec(
    config: &CodexConfig,
    exec: &dyn CodexProbeExec,
) -> Result<ExternalRuntimeCapabilities, ExternalAgentError> {
    let version = exec
        .invoke(config, &["--version"])
        .await
        .map_err(|error| launch_error(config, "querying --version", &error))?;
    if !version.success {
        return Err(ExternalAgentError::Launch {
            runtime: ExternalRuntimeKind::Codex,
            detail: format!(
                "codex binary {} exited unsuccessfully for --version",
                config.binary().display()
            ),
        });
    }

    let top_help = exec
        .invoke(config, &["--help"])
        .await
        .map_err(|error| launch_error(config, "querying --help", &error))?;
    let exec_help = exec
        .invoke(config, &["exec", "--help"])
        .await
        .map_err(|error| launch_error(config, "querying exec --help", &error))?;

    let top_text = top_help.combined();
    let exec_text = exec_help.combined();
    if top_text.trim().is_empty() && exec_text.trim().is_empty() {
        return Err(ExternalAgentError::Launch {
            runtime: ExternalRuntimeKind::Codex,
            detail: format!(
                "codex binary {} produced no --help output to probe",
                config.binary().display()
            ),
        });
    }

    let capabilities = detect_capabilities(&top_text, &exec_text);
    if !capabilities.streaming {
        return Err(capabilities.unsupported(
            ExternalCapability::Streaming,
            "codex CLI does not advertise `codex exec --json` structured event stream",
        ));
    }

    Ok(capabilities)
}

/// Builds a classified [`ExternalAgentError::Launch`] from a spawn/timeout error.
///
/// The `detail` names the stage and the classified [`io::ErrorKind`] plus the
/// binary path only; it never embeds the config's environment values or the
/// CLI's raw output, so a launch failure cannot leak a secret.
fn launch_error(config: &CodexConfig, stage: &str, error: &io::Error) -> ExternalAgentError {
    ExternalAgentError::Launch {
        runtime: ExternalRuntimeKind::Codex,
        detail: format!(
            "failed launching codex binary {} while {stage}: {:?}",
            config.binary().display(),
            error.kind()
        ),
    }
}

/// Conservatively derives the managed capabilities from the top-level and `exec`
/// `--help` text.
///
/// Every flag defaults to `false` and is turned on only when the help output
/// clearly advertises the backing feature, so an unrecognized or older CLI never
/// has capabilities assumed on its behalf. The two help pages are scanned
/// together because Codex splits its flags across them.
fn detect_capabilities(top_help: &str, exec_help: &str) -> ExternalRuntimeCapabilities {
    let in_top = |needle: &str| top_help.contains(needle);
    let in_exec = |needle: &str| exec_help.contains(needle);
    let in_either = |needle: &str| in_top(needle) || in_exec(needle);

    // The structured event stream lives on `codex exec --json`.
    let structured_stream = in_exec("--json");

    let mut capabilities = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::Codex);
    capabilities.streaming = structured_stream;
    capabilities.permission_bridge = in_either("--ask-for-approval") || in_either("--sandbox");
    capabilities.resume = in_either("resume");
    // MCP server management (`codex mcp`) lets the host inject tools the runtime
    // can call.
    capabilities.host_tools = in_top("mcp");
    // The structured stream's completion frames report token usage and its
    // file-change frames report produced artifacts, so both ride on `streaming`.
    capabilities.usage = structured_stream;
    capabilities.artifacts = structured_stream;
    // A spawned CLI can always be closed cleanly by dropping its stdin/process.
    capabilities.graceful_shutdown = true;
    // `host_subagents` stays false until a spawn bridge is verified later.
    capabilities
}

#[cfg(test)]
mod tests {
    use super::{
        CodexProbeExec, CodexProbeOutput, SystemCodexExec, detect_capabilities, probe,
        probe_with_exec,
    };
    use crate::agent::external::{
        CodexConfig, ExternalAgentError, ExternalCapability, ExternalRuntimeKind,
    };
    use async_trait::async_trait;
    use std::io;
    use std::sync::Mutex;

    /// A canned response for one probe subcommand: either an IO failure or a
    /// captured [`CodexProbeOutput`].
    #[derive(Clone)]
    enum FakeResponse {
        Io(io::ErrorKind),
        Output(CodexProbeOutput),
    }

    impl FakeResponse {
        fn ok(stdout: &str) -> Self {
            FakeResponse::Output(CodexProbeOutput {
                success: true,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            })
        }

        fn into_result(self) -> io::Result<CodexProbeOutput> {
            match self {
                FakeResponse::Io(kind) => Err(io::Error::new(kind, "fake probe io error")),
                FakeResponse::Output(output) => Ok(output),
            }
        }
    }

    /// Offline [`CodexProbeExec`] returning canned per-subcommand responses and
    /// recording the args it was handed.
    struct FakeExec {
        version: FakeResponse,
        top_help: FakeResponse,
        exec_help: FakeResponse,
        seen_args: Mutex<Vec<Vec<String>>>,
    }

    impl FakeExec {
        fn new(version: FakeResponse, top_help: FakeResponse, exec_help: FakeResponse) -> Self {
            Self {
                version,
                top_help,
                exec_help,
                seen_args: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl CodexProbeExec for FakeExec {
        async fn invoke(
            &self,
            _config: &CodexConfig,
            args: &[&str],
        ) -> io::Result<CodexProbeOutput> {
            self.seen_args
                .lock()
                .expect("record args")
                .push(args.iter().map(|a| (*a).to_owned()).collect());
            match args {
                ["--version"] => self.version.clone().into_result(),
                ["--help"] => self.top_help.clone().into_result(),
                ["exec", "--help"] => self.exec_help.clone().into_result(),
                other => panic!("unexpected probe args: {other:?}"),
            }
        }
    }

    /// A top-level `--help` fixture advertising the managed feature set.
    const FULL_TOP_HELP: &str = "\
Codex CLI
Usage: codex [OPTIONS] [PROMPT]
Commands:
  exec            Run Codex non-interactively
  resume          Resume a previous interactive session
  mcp             Manage external MCP servers for Codex
Options:
  -a, --ask-for-approval <APPROVAL_POLICY>  untrusted, on-request, never
  -s, --sandbox <SANDBOX_MODE>              read-only, workspace-write, danger-full-access
  -m, --model <MODEL>                       model to use
";

    /// An `exec --help` fixture advertising the structured stream.
    const FULL_EXEC_HELP: &str = "\
Run Codex non-interactively
Usage: codex exec [OPTIONS] [PROMPT]
Commands:
  resume  Resume a previous session by id
Options:
  -s, --sandbox <SANDBOX_MODE>  read-only, workspace-write, danger-full-access
  -m, --model <MODEL>           model to use
  -p, --profile <PROFILE>       config profile
      --skip-git-repo-check     allow running outside a git repo
      --json                    print events to stdout as JSONL
";

    #[tokio::test]
    async fn codex_probe_detects_full_capabilities() {
        let exec = FakeExec::new(
            FakeResponse::ok("codex-cli 0.144.1"),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(FULL_EXEC_HELP),
        );
        let config = CodexConfig::new();

        let caps = probe_with_exec(&config, &exec)
            .await
            .expect("probe succeeds");
        assert_eq!(caps.runtime, ExternalRuntimeKind::Codex);
        assert!(caps.streaming);
        assert!(caps.permission_bridge);
        assert!(caps.resume);
        assert!(caps.host_tools);
        assert!(caps.usage);
        assert!(caps.artifacts);
        assert!(caps.graceful_shutdown);
        // The spawn bridge is only verified later, so it stays conservative.
        assert!(!caps.host_subagents);

        // All three subcommands were probed, version first, then both help pages.
        let seen = exec
            .seen_args
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert_eq!(
            seen.as_slice(),
            &[
                vec!["--version".to_owned()],
                vec!["--help".to_owned()],
                vec!["exec".to_owned(), "--help".to_owned()],
            ]
        );
    }

    #[tokio::test]
    async fn codex_probe_missing_binary_is_launch_error() {
        let exec = FakeExec::new(
            FakeResponse::Io(io::ErrorKind::NotFound),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(FULL_EXEC_HELP),
        );
        let config = CodexConfig::new().with_binary("/no/such/codex");

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::Launch { runtime, detail }) => {
                assert_eq!(runtime, ExternalRuntimeKind::Codex);
                assert!(detail.contains("/no/such/codex"));
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn codex_probe_nonzero_version_is_launch_error() {
        let exec = FakeExec::new(
            FakeResponse::Output(CodexProbeOutput {
                success: false,
                stdout: String::new(),
                stderr: "boom".to_owned(),
            }),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(FULL_EXEC_HELP),
        );
        let config = CodexConfig::new();

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::Launch { .. }) => {}
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn codex_probe_empty_help_is_launch_error() {
        let exec = FakeExec::new(
            FakeResponse::ok("codex-cli 0.144.1"),
            FakeResponse::ok("   \n  "),
            FakeResponse::ok("  \n"),
        );
        let config = CodexConfig::new();

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::Launch { .. }) => {}
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn codex_probe_without_json_stream_is_unsupported() {
        // A CLI whose `exec` lacks `--json` fails loudly rather than being
        // reported as a usable managed runtime.
        let plain_exec_help = "\
Run Codex non-interactively
Usage: codex exec [OPTIONS] [PROMPT]
Options:
  -s, --sandbox <SANDBOX_MODE>  read-only, workspace-write
  -m, --model <MODEL>           model to use
";
        let exec = FakeExec::new(
            FakeResponse::ok("codex-cli 0.144.1"),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(plain_exec_help),
        );
        let config = CodexConfig::new();

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::UnsupportedCapability {
                runtime,
                capability,
                ..
            }) => {
                assert_eq!(runtime, ExternalRuntimeKind::Codex);
                assert_eq!(capability, ExternalCapability::Streaming);
            }
            other => panic!("expected UnsupportedCapability, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn codex_probe_does_not_leak_env_secret() {
        // Even with an env secret configured, neither a launch failure nor an
        // unsupported-capability failure may embed the secret value.
        let secret = "sk-super-secret-key";

        let launch_exec = FakeExec::new(
            FakeResponse::Io(io::ErrorKind::PermissionDenied),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(FULL_EXEC_HELP),
        );
        let config = CodexConfig::new().with_env("OPENAI_API_KEY", secret);
        let launch_err = probe_with_exec(&config, &launch_exec)
            .await
            .expect_err("launch failure");
        assert!(!format!("{launch_err}").contains(secret));
        assert!(!format!("{launch_err:?}").contains(secret));

        let unsupported_exec = FakeExec::new(
            FakeResponse::ok("codex-cli 0.144.1"),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok("Usage: codex exec [OPTIONS] [PROMPT]"),
        );
        let unsupported_err = probe_with_exec(&config, &unsupported_exec)
            .await
            .expect_err("unsupported failure");
        assert!(!format!("{unsupported_err}").contains(secret));
        assert!(!format!("{unsupported_err:?}").contains(secret));
    }

    #[tokio::test]
    async fn codex_probe_with_missing_binary_via_system_exec_is_launch() {
        // Exercises the real SystemCodexExec spawn path offline: a binary that
        // cannot exist must classify as Launch and must not panic.
        let _ = SystemCodexExec;
        let config = CodexConfig::new().with_binary("codex-probe-nonexistent-binary-xyz");
        match probe(&config).await {
            Err(ExternalAgentError::Launch { runtime, .. }) => {
                assert_eq!(runtime, ExternalRuntimeKind::Codex);
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[test]
    fn detect_capabilities_defaults_unadvertised_features_off() {
        // Help text advertising only the structured stream turns on streaming
        // (plus its implied usage/artifacts and graceful shutdown) and leaves
        // every unadvertised feature off.
        let caps = detect_capabilities("", "codex exec --json");
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
