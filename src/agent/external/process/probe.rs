//! Shared capability-probe core for the managed CLI runtimes.
//!
//! The three managed CLI adapters (Claude Code, Codex, OpenCode) run the same
//! probe protocol against their binary: a `--version` sanity check, then one or
//! more `--help` pages whose combined text is scanned for the managed features
//! the CLI advertises. They differ only in the binary/config plumbing, the
//! runtime label, the help pages they read, and the conservative capability
//! detection over the help text. This module single-sources the shared protocol
//! — [`ProbeOutput`], [`invoke_probe`], [`probe_cli`], and [`launch_error`] — so
//! each runtime's `probe` module keeps only its public exec trait, its config
//! plumbing, and its detection rules.
//!
//! Secret hygiene matches the per-runtime probes this replaces: neither the
//! probe nor its errors embed environment values or raw CLI output, so a probe
//! failure surfaced to a log cannot leak a credential.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::agent::external::{
    ExternalAgentError, ExternalCapability, ExternalRuntimeCapabilities, ExternalRuntimeKind,
};

/// Captured result of one probe subcommand invocation.
///
/// This is the provider-neutral shape a runtime's probe exec returns for a
/// single `<binary> <args>` run: whether the process exited successfully and
/// its captured `stdout` / `stderr` decoded as lossy UTF-8. The probe inspects
/// the text but never echoes it into an error. Each managed CLI runtime
/// re-exports this type under its own public name (`ProbeOutput` /
/// `CodexProbeOutput` / `OpenCodeProbeOutput`).
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

/// Runs `<binary> <args>` under the shared probe launch settings.
///
/// This is the spawn half every managed CLI probe exec performs: null stdin,
/// piped output, `kill_on_drop`, the config's working directory and environment
/// overrides, and a wall-clock timeout that classifies as
/// [`io::ErrorKind::TimedOut`] carrying the runtime's own `timeout_message`.
pub(crate) async fn invoke_probe(
    binary: &Path,
    args: &[&str],
    working_dir: Option<&Path>,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    timeout_message: &'static str,
) -> io::Result<ProbeOutput> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }
    for (key, value) in env {
        command.env(key, value);
    }

    let child = command.spawn()?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(result) => result?,
        Err(_elapsed) => {
            return Err(io::Error::new(io::ErrorKind::TimedOut, timeout_message));
        }
    };

    Ok(ProbeOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Drives the shared probe protocol: `--version`, then every help page in
/// `help_pages`, then conservative capability detection over the combined help
/// text.
///
/// `invoke` runs one probe subcommand and captures its [`ProbeOutput`];
/// `runtime` / `display_name` label the classified errors; `detect` derives the
/// capability set from the combined help texts (one entry per requested page,
/// in order); `streaming_detail` is the diagnostic used when the detected set
/// lacks streaming, which every managed CLI adapter requires.
///
/// # Errors
///
/// Returns [`ExternalAgentError::Launch`] when the binary is missing, broken,
/// exits unsuccessfully for `--version`, or produces no help output, and
/// [`ExternalAgentError::UnsupportedCapability`] when the detected capabilities
/// lack the structured stream the managed adapter requires. It never panics.
pub(crate) async fn probe_cli<I, D>(
    mut invoke: I,
    binary: &Path,
    runtime: ExternalRuntimeKind,
    display_name: &'static str,
    help_pages: &[&[&str]],
    detect: D,
    streaming_detail: &'static str,
) -> Result<ExternalRuntimeCapabilities, ExternalAgentError>
where
    I: AsyncFnMut(&[&str]) -> io::Result<ProbeOutput>,
    D: Fn(&[String]) -> ExternalRuntimeCapabilities,
{
    let version = invoke(&["--version"]).await.map_err(|error| {
        launch_error(&runtime, display_name, binary, "querying --version", &error)
    })?;
    if !version.success {
        return Err(ExternalAgentError::Launch {
            runtime,
            detail: format!(
                "{display_name} binary {} exited unsuccessfully for --version",
                binary.display()
            ),
        });
    }

    let mut help_texts = Vec::with_capacity(help_pages.len());
    for page in help_pages {
        let help = invoke(page).await.map_err(|error| {
            launch_error(
                &runtime,
                display_name,
                binary,
                &format!("querying {}", page.join(" ")),
                &error,
            )
        })?;
        help_texts.push(help.combined());
    }
    if help_texts.iter().all(|text| text.trim().is_empty()) {
        return Err(ExternalAgentError::Launch {
            runtime,
            detail: format!(
                "{display_name} binary {} produced no --help output to probe",
                binary.display()
            ),
        });
    }

    let capabilities = detect(&help_texts);
    if !capabilities.streaming {
        return Err(capabilities.unsupported(ExternalCapability::Streaming, streaming_detail));
    }

    Ok(capabilities)
}

/// Builds a classified [`ExternalAgentError::Launch`] from a spawn/timeout error.
///
/// The `detail` names the stage and the classified [`io::ErrorKind`] plus the
/// binary path only; it never embeds the config's environment values or the
/// CLI's raw output, so a launch failure cannot leak a secret.
pub(crate) fn launch_error(
    runtime: &ExternalRuntimeKind,
    display_name: &'static str,
    binary: &Path,
    stage: &str,
    error: &io::Error,
) -> ExternalAgentError {
    ExternalAgentError::Launch {
        runtime: runtime.clone(),
        detail: format!(
            "failed launching {display_name} binary {} while {stage}: {:?}",
            binary.display(),
            error.kind()
        ),
    }
}
