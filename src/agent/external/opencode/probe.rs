//! Capability probe for the managed OpenCode runtime adapter.
//!
//! Before a host dispatches managed work to OpenCode it must learn, without
//! assuming anything, whether the local CLI is present and which managed
//! features it can serve — OpenCode ships in more deployment shapes than the
//! other runtimes, so the design (§14) mandates a probe rather than hardcoded
//! assumptions. [`probe`] answers that: it invokes the configured binary's
//! `--version`, `--help`, and `run --help`, classifies a missing or broken
//! binary as [`ExternalAgentError::Launch`], classifies a binary that cannot emit
//! the structured `run --format json` event stream the managed adapter relies on
//! as [`ExternalAgentError::UnsupportedCapability`], and otherwise reports a
//! conservatively-detected [`ExternalRuntimeCapabilities`] set (design §14, §15).
//! It never panics.
//!
//! # Why two help pages
//!
//! OpenCode splits its features across the top-level command and the `run`
//! subcommand: the structured `--format json` stream, the `--auto` approval
//! bypass, and `--continue`/`--session` resume all live on `run`, while
//! MCP-server management (`mcp`) and session management (`session`) are top-level
//! commands. The probe therefore reads `--help` *and* `run --help` and detects
//! capabilities from the union, so nothing is missed just because it sits on the
//! other help page.
//!
//! # Offline testability
//!
//! The probe is written against the [`OpenCodeProbeExec`] trait rather than
//! spawning a process directly. Production uses [`SystemOpenCodeExec`], which runs
//! the real CLI through [`tokio::process`]; tests inject a fake exec that returns
//! canned help output, so the whole error-classification surface is exercised
//! with no OpenCode install and no network. The default
//! `cargo test --all --all-targets` build does not compile this module at all,
//! because the whole adapter is gated behind the `external-opencode` feature.
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

use super::OpenCodeConfig;

/// Captured result of one probe subcommand invocation.
///
/// This is the provider-neutral shape an [`OpenCodeProbeExec`] returns for a
/// single `opencode <args>` run: whether the process exited successfully and its
/// captured `stdout` / `stderr` decoded as lossy UTF-8. The probe inspects the
/// text but never echoes it into an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCodeProbeOutput {
    /// Whether the process exited with a success status.
    pub success: bool,
    /// Captured standard output, decoded lossily.
    pub stdout: String,
    /// Captured standard error, decoded lossily.
    pub stderr: String,
}

impl OpenCodeProbeOutput {
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

/// Runs OpenCode probe subcommands, abstracting the real process spawn.
///
/// The one method invokes `<binary> <args>` under the config's working
/// directory, environment overrides, and timeout, returning the captured
/// [`OpenCodeProbeOutput`] or the raw [`io::Error`] from spawning/waiting.
/// Splitting this out lets the probe's classification logic be unit-tested with a
/// fake exec and no real OpenCode binary.
#[async_trait]
pub trait OpenCodeProbeExec: Send + Sync {
    /// Runs `<binary> <args>` and captures its output.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the process cannot be spawned
    /// (missing binary, permission denied) or does not complete within the
    /// config's [`timeout`](OpenCodeConfig::timeout).
    async fn invoke(
        &self,
        config: &OpenCodeConfig,
        args: &[&str],
    ) -> io::Result<OpenCodeProbeOutput>;
}

/// Production [`OpenCodeProbeExec`] that spawns the real CLI via
/// [`tokio::process`].
///
/// It applies the config's working directory and environment overrides, pipes
/// the child's output, kills the child on drop, and bounds the run with the
/// config's timeout. It adds no dependency beyond tokio's process support, which
/// the crate already enables.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemOpenCodeExec;

#[async_trait]
impl OpenCodeProbeExec for SystemOpenCodeExec {
    async fn invoke(
        &self,
        config: &OpenCodeConfig,
        args: &[&str],
    ) -> io::Result<OpenCodeProbeOutput> {
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
                    "opencode probe timed out",
                ));
            }
        };

        Ok(OpenCodeProbeOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Probes the local OpenCode CLI described by `config`.
///
/// Runs the real CLI through [`SystemOpenCodeExec`]; see [`probe_with_exec`] for
/// the classification contract and [`ExternalRuntimeCapabilities`] shape.
///
/// # Errors
///
/// Returns [`ExternalAgentError::Launch`] when the binary is missing or broken,
/// or [`ExternalAgentError::UnsupportedCapability`] when it cannot emit the
/// `run --format json` event stream the managed adapter requires.
pub async fn probe(
    config: &OpenCodeConfig,
) -> Result<ExternalRuntimeCapabilities, ExternalAgentError> {
    probe_with_exec(config, &SystemOpenCodeExec).await
}

/// Probes OpenCode through an injected `exec`, returning its capabilities.
///
/// The probe runs three subcommands and classifies the outcome:
///
/// 1. `--version` — a spawn/timeout [`io::Error`] or a non-success exit means the
///    binary is missing or broken, classified as
///    [`ExternalAgentError::Launch`].
/// 2. `--help` (top level) and `run --help` — a spawn/timeout error on either, or
///    empty combined help output, is likewise a
///    [`ExternalAgentError::Launch`]. Otherwise the union of both help texts is
///    scanned for the managed features the CLI advertises.
///
/// The managed adapter drives OpenCode through its structured
/// `opencode run --format json` event stream; if the CLI does not advertise both
/// `--format` and its `json` value on `run`, the probe fails loudly with
/// [`ExternalAgentError::UnsupportedCapability`] for
/// [`ExternalCapability::Streaming`] rather than pretending the runtime is
/// usable. Every other capability is detected conservatively from the help text
/// and defaults to `false` when not clearly advertised (design §15):
///
/// - `permission_bridge` — `--auto` present on `run` (a permission-gating model
///   the bridge can drive).
/// - `resume` — `--continue`/`--session` on `run`, or the top-level `session`
///   command.
/// - `host_tools` — the top-level `mcp` command present (MCP tool injection).
/// - `usage` / `artifacts` — implied by the structured stream, whose completion
///   and file/command events carry token usage and produced-file facts.
/// - `graceful_shutdown` — always `true` for a CLI whose stdin the adapter can
///   close cleanly.
/// - `host_subagents` — left `false`; the spawn bridge is verified later, not
///   here (selecting a preset `--agent` is not host-minted subagent spawning).
///
/// # Errors
///
/// Returns [`ExternalAgentError::Launch`] or
/// [`ExternalAgentError::UnsupportedCapability`] as described above. It never
/// panics.
pub async fn probe_with_exec(
    config: &OpenCodeConfig,
    exec: &dyn OpenCodeProbeExec,
) -> Result<ExternalRuntimeCapabilities, ExternalAgentError> {
    let version = exec
        .invoke(config, &["--version"])
        .await
        .map_err(|error| launch_error(config, "querying --version", &error))?;
    if !version.success {
        return Err(ExternalAgentError::Launch {
            runtime: ExternalRuntimeKind::OpenCode,
            detail: format!(
                "opencode binary {} exited unsuccessfully for --version",
                config.binary().display()
            ),
        });
    }

    let top_help = exec
        .invoke(config, &["--help"])
        .await
        .map_err(|error| launch_error(config, "querying --help", &error))?;
    let run_help = exec
        .invoke(config, &["run", "--help"])
        .await
        .map_err(|error| launch_error(config, "querying run --help", &error))?;

    let top_text = top_help.combined();
    let run_text = run_help.combined();
    if top_text.trim().is_empty() && run_text.trim().is_empty() {
        return Err(ExternalAgentError::Launch {
            runtime: ExternalRuntimeKind::OpenCode,
            detail: format!(
                "opencode binary {} produced no --help output to probe",
                config.binary().display()
            ),
        });
    }

    let capabilities = detect_capabilities(&top_text, &run_text);
    if !capabilities.streaming {
        return Err(capabilities.unsupported(
            ExternalCapability::Streaming,
            "opencode CLI does not advertise `run --format json` structured event stream",
        ));
    }

    Ok(capabilities)
}

/// Builds a classified [`ExternalAgentError::Launch`] from a spawn/timeout error.
///
/// The `detail` names the stage and the classified [`io::ErrorKind`] plus the
/// binary path only; it never embeds the config's environment values or the
/// CLI's raw output, so a launch failure cannot leak a secret.
fn launch_error(config: &OpenCodeConfig, stage: &str, error: &io::Error) -> ExternalAgentError {
    ExternalAgentError::Launch {
        runtime: ExternalRuntimeKind::OpenCode,
        detail: format!(
            "failed launching opencode binary {} while {stage}: {:?}",
            config.binary().display(),
            error.kind()
        ),
    }
}

/// Conservatively derives the managed capabilities from the top-level and `run`
/// `--help` text.
///
/// Every flag defaults to `false` and is turned on only when the help output
/// clearly advertises the backing feature, so an unrecognized or older CLI never
/// has capabilities assumed on its behalf. The two help pages are scanned
/// together because OpenCode splits its features across them.
fn detect_capabilities(top_help: &str, run_help: &str) -> ExternalRuntimeCapabilities {
    let in_top = |needle: &str| top_help.contains(needle);
    let in_run = |needle: &str| run_help.contains(needle);

    // The structured event stream is `opencode run --format json`; require both
    // the flag and its json value so a `--format`-only-for-tables build is not
    // mistaken for a JSON event stream.
    let structured_stream = in_run("--format") && in_run("json");

    let mut capabilities = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::OpenCode);
    capabilities.streaming = structured_stream;
    // `--auto` gates permission approval, so a bridge can drive it.
    capabilities.permission_bridge = in_run("--auto");
    // Resume rides on `run --continue`/`--session` or the top-level `session`
    // management command.
    capabilities.resume = in_run("--continue") || in_run("--session") || in_top("session");
    // MCP server management (`opencode mcp`) lets the host inject tools the
    // runtime can call.
    capabilities.host_tools = in_top("mcp");
    // The structured stream's completion events report token usage and its
    // file/command events report produced artifacts, so both ride on `streaming`.
    capabilities.usage = structured_stream;
    capabilities.artifacts = structured_stream;
    // A spawned CLI can always be closed cleanly by dropping its stdin/process.
    capabilities.graceful_shutdown = true;
    // `host_subagents` stays false until a spawn bridge is verified later; a
    // preset `--agent` selector is not host-minted subagent spawning.
    capabilities
}

#[cfg(test)]
mod tests {
    use super::{
        OpenCodeProbeExec, OpenCodeProbeOutput, SystemOpenCodeExec, detect_capabilities, probe,
        probe_with_exec,
    };
    use crate::agent::external::{
        ExternalAgentError, ExternalCapability, ExternalRuntimeKind, OpenCodeConfig,
    };
    use async_trait::async_trait;
    use std::io;
    use std::sync::Mutex;

    /// A canned response for one probe subcommand: either an IO failure or a
    /// captured [`OpenCodeProbeOutput`].
    #[derive(Clone)]
    enum FakeResponse {
        Io(io::ErrorKind),
        Output(OpenCodeProbeOutput),
    }

    impl FakeResponse {
        fn ok(stdout: &str) -> Self {
            FakeResponse::Output(OpenCodeProbeOutput {
                success: true,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            })
        }

        fn into_result(self) -> io::Result<OpenCodeProbeOutput> {
            match self {
                FakeResponse::Io(kind) => Err(io::Error::new(kind, "fake probe io error")),
                FakeResponse::Output(output) => Ok(output),
            }
        }
    }

    /// Offline [`OpenCodeProbeExec`] returning canned per-subcommand responses and
    /// recording the args it was handed.
    struct FakeExec {
        version: FakeResponse,
        top_help: FakeResponse,
        run_help: FakeResponse,
        seen_args: Mutex<Vec<Vec<String>>>,
    }

    impl FakeExec {
        fn new(version: FakeResponse, top_help: FakeResponse, run_help: FakeResponse) -> Self {
            Self {
                version,
                top_help,
                run_help,
                seen_args: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl OpenCodeProbeExec for FakeExec {
        async fn invoke(
            &self,
            _config: &OpenCodeConfig,
            args: &[&str],
        ) -> io::Result<OpenCodeProbeOutput> {
            self.seen_args
                .lock()
                .expect("record args")
                .push(args.iter().map(|a| (*a).to_owned()).collect());
            match args {
                ["--version"] => self.version.clone().into_result(),
                ["--help"] => self.top_help.clone().into_result(),
                ["run", "--help"] => self.run_help.clone().into_result(),
                other => panic!("unexpected probe args: {other:?}"),
            }
        }
    }

    /// A top-level `--help` fixture advertising the managed feature set.
    const FULL_TOP_HELP: &str = "\
opencode
Usage: opencode [OPTIONS] [COMMAND]
Commands:
  run       Run opencode in non-interactive mode
  mcp       Manage Model Context Protocol servers
  session   Manage OpenCode sessions
  agent     Manage agents for OpenCode
  serve     Start a headless OpenCode server
Options:
  -m, --model <MODEL>  Model to use in the form of provider/model
";

    /// A `run --help` fixture advertising the structured stream.
    const FULL_RUN_HELP: &str = "\
Run opencode in non-interactive mode by passing a prompt directly.
Usage: opencode run [OPTIONS] [MESSAGE]...
Options:
  -c, --continue          Continue the last session
  -s, --session <ID>      Session ID to continue
  -m, --model <MODEL>     Model to use in the form of provider/model
      --agent <AGENT>     Agent to use
      --format <FORMAT>   Format: default (formatted) or json (raw JSON events)
      --auto              Auto-approve permissions that are not explicitly denied
      --dir <DIR>         Directory to run in
";

    #[tokio::test]
    async fn opencode_probe_detects_full_capabilities() {
        let exec = FakeExec::new(
            FakeResponse::ok("opencode 0.5.0"),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(FULL_RUN_HELP),
        );
        let config = OpenCodeConfig::new();

        let caps = probe_with_exec(&config, &exec)
            .await
            .expect("probe succeeds");
        assert_eq!(caps.runtime, ExternalRuntimeKind::OpenCode);
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
                vec!["run".to_owned(), "--help".to_owned()],
            ]
        );
    }

    #[tokio::test]
    async fn opencode_probe_missing_binary_is_launch_error() {
        let exec = FakeExec::new(
            FakeResponse::Io(io::ErrorKind::NotFound),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(FULL_RUN_HELP),
        );
        let config = OpenCodeConfig::new().with_binary("/no/such/opencode");

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::Launch { runtime, detail }) => {
                assert_eq!(runtime, ExternalRuntimeKind::OpenCode);
                assert!(detail.contains("/no/such/opencode"));
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn opencode_probe_nonzero_version_is_launch_error() {
        let exec = FakeExec::new(
            FakeResponse::Output(OpenCodeProbeOutput {
                success: false,
                stdout: String::new(),
                stderr: "boom".to_owned(),
            }),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(FULL_RUN_HELP),
        );
        let config = OpenCodeConfig::new();

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::Launch { .. }) => {}
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn opencode_probe_empty_help_is_launch_error() {
        let exec = FakeExec::new(
            FakeResponse::ok("opencode 0.5.0"),
            FakeResponse::ok("   \n  "),
            FakeResponse::ok("  \n"),
        );
        let config = OpenCodeConfig::new();

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::Launch { .. }) => {}
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn opencode_probe_without_json_stream_is_unsupported() {
        // A CLI whose `run` lacks the `--format json` event stream fails loudly
        // rather than being reported as a usable managed runtime.
        let plain_run_help = "\
Run opencode in non-interactive mode by passing a prompt directly.
Usage: opencode run [OPTIONS] [MESSAGE]...
Options:
  -c, --continue       Continue the last session
  -m, --model <MODEL>  Model to use in the form of provider/model
";
        let exec = FakeExec::new(
            FakeResponse::ok("opencode 0.5.0"),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(plain_run_help),
        );
        let config = OpenCodeConfig::new();

        match probe_with_exec(&config, &exec).await {
            Err(ExternalAgentError::UnsupportedCapability {
                runtime,
                capability,
                ..
            }) => {
                assert_eq!(runtime, ExternalRuntimeKind::OpenCode);
                assert_eq!(capability, ExternalCapability::Streaming);
            }
            other => panic!("expected UnsupportedCapability, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn opencode_probe_does_not_leak_env_secret() {
        // Even with an env secret configured, neither a launch failure nor an
        // unsupported-capability failure may embed the secret value.
        let secret = "sk-super-secret-key";

        let launch_exec = FakeExec::new(
            FakeResponse::Io(io::ErrorKind::PermissionDenied),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok(FULL_RUN_HELP),
        );
        let config = OpenCodeConfig::new().with_env("ANTHROPIC_API_KEY", secret);
        let launch_err = probe_with_exec(&config, &launch_exec)
            .await
            .expect_err("launch failure");
        assert!(!format!("{launch_err}").contains(secret));
        assert!(!format!("{launch_err:?}").contains(secret));

        let unsupported_exec = FakeExec::new(
            FakeResponse::ok("opencode 0.5.0"),
            FakeResponse::ok(FULL_TOP_HELP),
            FakeResponse::ok("Usage: opencode run [OPTIONS] [MESSAGE]..."),
        );
        let unsupported_err = probe_with_exec(&config, &unsupported_exec)
            .await
            .expect_err("unsupported failure");
        assert!(!format!("{unsupported_err}").contains(secret));
        assert!(!format!("{unsupported_err:?}").contains(secret));
    }

    #[tokio::test]
    async fn opencode_probe_with_missing_binary_via_system_exec_is_launch() {
        // Exercises the real SystemOpenCodeExec spawn path offline: a binary that
        // cannot exist must classify as Launch and must not panic.
        let _ = SystemOpenCodeExec;
        let config = OpenCodeConfig::new().with_binary("opencode-probe-nonexistent-binary-xyz");
        match probe(&config).await {
            Err(ExternalAgentError::Launch { runtime, .. }) => {
                assert_eq!(runtime, ExternalRuntimeKind::OpenCode);
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    #[test]
    fn detect_capabilities_defaults_unadvertised_features_off() {
        // Help text advertising only the structured stream turns on streaming
        // (plus its implied usage/artifacts and graceful shutdown) and leaves
        // every unadvertised feature off.
        let caps = detect_capabilities("", "opencode run --format json");
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
