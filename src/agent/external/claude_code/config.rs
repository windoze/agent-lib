//! Launch configuration for the managed Claude Code runtime adapter.
//!
//! [`ClaudeCodeConfig`] is the data-only recipe a host hands the Claude Code
//! adapter so it can *probe* the CLI (this milestone) and later *launch* live
//! sessions (M6-3). It records the binary to invoke, environment overrides, the
//! working directory / worktree, the provider-neutral permission mode, an
//! optional model and profile, and a probe/launch timeout. It holds no live
//! process, channel, or task handle — those stay behind the adapter and the
//! [`ExternalRuntimeHandles`](crate::agent::external::ExternalRuntimeHandles)
//! boundary (design §4, §12.1).
//!
//! # Secret hygiene
//!
//! [`env`](ClaudeCodeConfig::env) can carry credentials (an `ANTHROPIC_API_KEY`,
//! an auth token). The manual [`Debug`] impl therefore prints only the
//! environment variable *names* with redacted values, never the values
//! themselves, so a config that lands in a log or a panic message cannot leak a
//! secret (design constraint "任何可能包含 secret … 的日志/错误必须脱敏").
//! Serialization is a separate, deliberate persistence path and does round-trip
//! the values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::external::ExternalPermissionMode;

/// The default Claude Code CLI binary looked up on `PATH`.
const DEFAULT_BINARY: &str = "claude";

/// The default probe/launch timeout applied when a caller does not set one.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The default per-line stdout idle timeout for a live session.
///
/// This is deliberately far longer than [`DEFAULT_TIMEOUT`]: a turn can run a
/// silent build or test suite for minutes without emitting a frame, and that
/// silence must not be mistaken for a dead CLI.
const DEFAULT_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// The default grace period a session close waits for the CLI to exit on its
/// own (after stdin EOF) before force-killing the child.
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// Serde default for [`ClaudeCodeConfig::read_idle_timeout`], so configs
/// persisted before the field existed still deserialize.
const fn default_read_idle_timeout() -> Duration {
    DEFAULT_READ_IDLE_TIMEOUT
}

/// Serde default for [`ClaudeCodeConfig::shutdown_grace`], so configs
/// persisted before the field existed still deserialize.
const fn default_shutdown_grace() -> Duration {
    DEFAULT_SHUTDOWN_GRACE
}

/// Data-only launch configuration for the managed Claude Code adapter.
///
/// Build one with [`ClaudeCodeConfig::new`] (or [`Default`], which uses the
/// `claude` binary on `PATH`) and refine it with the chained `with_*` setters.
/// The config is plain, serializable data: it round-trips through serde so a
/// host can persist it, and it carries no live handles.
///
/// # Fields
///
/// - [`binary`](Self::binary): the CLI executable, defaulting to `claude`
///   resolved on `PATH`; override it with an absolute path for a pinned install.
/// - [`env`](Self::env): extra environment variables layered onto the child
///   process (for example a scoped `ANTHROPIC_API_KEY`). Redacted in [`Debug`].
/// - [`working_dir`](Self::working_dir): the directory (typically the agent's
///   worktree) the CLI runs in; `None` inherits the parent's directory.
/// - [`permission_mode`](Self::permission_mode): the provider-neutral
///   [`ExternalPermissionMode`] mapped onto Claude's `--permission-mode` flag.
/// - [`model`](Self::model) / [`profile`](Self::profile): optional `--model` and
///   host-side profile selectors.
/// - [`timeout`](Self::timeout): the wall-clock bound applied to a probe
///   invocation and to the launch handshake.
/// - [`read_idle_timeout`](Self::read_idle_timeout): the per-line stdout idle
///   bound for a live session — how long the CLI may stay silent between
///   frames before the session is declared lost.
/// - [`shutdown_grace`](Self::shutdown_grace): how long a session close waits
///   for a graceful exit (after stdin EOF) before force-killing the child.
///
/// # The three timeouts
///
/// [`timeout`](Self::timeout) bounds only one-shot control operations
/// (probe, launch). Steady-state session IO uses the other two:
/// [`read_idle_timeout`](Self::read_idle_timeout) guards each stdout line read
/// so a long silent command (a build, a test suite) is not mistaken for a
/// dead CLI, and [`shutdown_grace`](Self::shutdown_grace) bounds the graceful
/// close before the fallback kill. All three are independent knobs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeCodeConfig {
    binary: PathBuf,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_dir: Option<PathBuf>,
    permission_mode: ExternalPermissionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    timeout: Duration,
    #[serde(default = "default_read_idle_timeout")]
    read_idle_timeout: Duration,
    #[serde(default = "default_shutdown_grace")]
    shutdown_grace: Duration,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from(DEFAULT_BINARY),
            env: BTreeMap::new(),
            working_dir: None,
            permission_mode: ExternalPermissionMode::Prompt,
            model: None,
            profile: None,
            timeout: DEFAULT_TIMEOUT,
            read_idle_timeout: DEFAULT_READ_IDLE_TIMEOUT,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        }
    }
}

impl ClaudeCodeConfig {
    /// Creates a permissive config equal to [`Default`]: the `claude` binary on
    /// `PATH`, no env overrides, prompt-on-action permissions, and the default
    /// timeout.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the CLI binary path (default: `claude` resolved on `PATH`).
    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Adds or replaces one environment variable layered onto the child process.
    ///
    /// Values may be secrets; they are redacted in [`Debug`] but preserved by
    /// serialization.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Sets the working directory (typically the agent's worktree) the CLI runs
    /// in; `None` inherits the parent process's directory.
    #[must_use]
    pub fn with_working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }

    /// Sets the provider-neutral permission mode mapped onto `--permission-mode`.
    #[must_use]
    pub const fn with_permission_mode(mut self, mode: ExternalPermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    /// Sets the optional `--model` selector.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Sets the optional host-side profile selector.
    #[must_use]
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = Some(profile.into());
        self
    }

    /// Sets the probe/launch timeout.
    ///
    /// This bounds only probe invocations and the launch handshake; it does
    /// **not** bound steady-state session reads (see
    /// [`with_read_idle_timeout`](Self::with_read_idle_timeout)) or the
    /// graceful close (see [`with_shutdown_grace`](Self::with_shutdown_grace)).
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the per-line stdout idle timeout for a live session.
    ///
    /// Each stdout line read is bounded by this duration; exceeding it
    /// declares the session lost. The default (10 minutes) leaves room for
    /// long silent commands such as builds or test suites.
    #[must_use]
    pub const fn with_read_idle_timeout(mut self, read_idle_timeout: Duration) -> Self {
        self.read_idle_timeout = read_idle_timeout;
        self
    }

    /// Sets the grace period a session close waits for the CLI to exit on its
    /// own (after stdin EOF) before force-killing the child.
    #[must_use]
    pub const fn with_shutdown_grace(mut self, shutdown_grace: Duration) -> Self {
        self.shutdown_grace = shutdown_grace;
        self
    }

    /// Returns the configured CLI binary path.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Returns the environment overrides layered onto the child process.
    #[must_use]
    pub const fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    /// Returns the working directory the CLI runs in, if one was set.
    #[must_use]
    pub fn working_dir(&self) -> Option<&Path> {
        self.working_dir.as_deref()
    }

    /// Returns the provider-neutral permission mode.
    #[must_use]
    pub const fn permission_mode(&self) -> ExternalPermissionMode {
        self.permission_mode
    }

    /// Returns the optional `--model` selector.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Returns the optional host-side profile selector.
    #[must_use]
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// Returns the probe/launch timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Returns the per-line stdout idle timeout for a live session.
    #[must_use]
    pub const fn read_idle_timeout(&self) -> Duration {
        self.read_idle_timeout
    }

    /// Returns the grace period a session close waits before force-killing.
    #[must_use]
    pub const fn shutdown_grace(&self) -> Duration {
        self.shutdown_grace
    }

    /// Maps the configured [`ExternalPermissionMode`] onto the Claude Code
    /// `--permission-mode` argument value.
    ///
    /// The mapping follows the current Claude CLI vocabulary
    /// (`default` / `acceptEdits` / `plan` / `bypassPermissions`), so the
    /// provider-neutral [`Prompt`](ExternalPermissionMode::Prompt) maps to
    /// Claude's interactive `default` mode (design §12.1). The runtime's output
    /// never widens the host's permission boundary regardless of this value.
    #[must_use]
    pub const fn permission_mode_arg(&self) -> &'static str {
        match self.permission_mode {
            ExternalPermissionMode::Prompt => "default",
            ExternalPermissionMode::AcceptEdits => "acceptEdits",
            ExternalPermissionMode::Plan => "plan",
            ExternalPermissionMode::BypassPermissions => "bypassPermissions",
        }
    }

    /// Builds the base managed-mode CLI arguments for a live session.
    ///
    /// This is the structured-stream launch shape from design §12.1
    /// (`--print --output-format stream-json --input-format stream-json
    /// --permission-mode <mode>` plus an optional `--model`). The adapter's live
    /// session (M6-3) appends the per-turn prompt and resume flags; the probe
    /// does not use these arguments — it inspects `--version` and `--help`
    /// instead.
    #[must_use]
    pub fn base_session_args(&self) -> Vec<String> {
        let mut args = vec![
            "--print".to_owned(),
            "--output-format".to_owned(),
            "stream-json".to_owned(),
            "--input-format".to_owned(),
            "stream-json".to_owned(),
            "--permission-mode".to_owned(),
            self.permission_mode_arg().to_owned(),
        ];
        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }
        args
    }
}

impl std::fmt::Debug for ClaudeCodeConfig {
    /// Redacts environment values so a logged config cannot leak a secret.
    ///
    /// Every other field is stable, non-secret configuration and is shown as-is;
    /// [`env`](ClaudeCodeConfig::env) is rendered as its keys mapped to a
    /// `"<redacted>"` placeholder so the *shape* is debuggable without exposing
    /// credential values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_env: BTreeMap<&String, &str> =
            self.env.keys().map(|key| (key, "<redacted>")).collect();
        f.debug_struct("ClaudeCodeConfig")
            .field("binary", &self.binary)
            .field("env", &redacted_env)
            .field("working_dir", &self.working_dir)
            .field("permission_mode", &self.permission_mode)
            .field("model", &self.model)
            .field("profile", &self.profile)
            .field("timeout", &self.timeout)
            .field("read_idle_timeout", &self.read_idle_timeout)
            .field("shutdown_grace", &self.shutdown_grace)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClaudeCodeConfig, DEFAULT_BINARY, DEFAULT_READ_IDLE_TIMEOUT, DEFAULT_SHUTDOWN_GRACE,
        DEFAULT_TIMEOUT,
    };
    use crate::agent::external::ExternalPermissionMode;
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn claude_code_config_defaults_are_permissive() {
        let config = ClaudeCodeConfig::default();
        assert_eq!(config.binary(), Path::new(DEFAULT_BINARY));
        assert!(config.env().is_empty());
        assert!(config.working_dir().is_none());
        assert_eq!(config.permission_mode(), ExternalPermissionMode::Prompt);
        assert!(config.model().is_none());
        assert!(config.profile().is_none());
        assert_eq!(config.timeout(), DEFAULT_TIMEOUT);
        assert_eq!(config.read_idle_timeout(), DEFAULT_READ_IDLE_TIMEOUT);
        assert_eq!(config.shutdown_grace(), DEFAULT_SHUTDOWN_GRACE);
    }

    #[test]
    fn claude_code_config_permission_mode_arg_maps_every_mode() {
        let cases = [
            (ExternalPermissionMode::Prompt, "default"),
            (ExternalPermissionMode::AcceptEdits, "acceptEdits"),
            (ExternalPermissionMode::Plan, "plan"),
            (
                ExternalPermissionMode::BypassPermissions,
                "bypassPermissions",
            ),
        ];
        for (mode, expected) in cases {
            let config = ClaudeCodeConfig::new().with_permission_mode(mode);
            assert_eq!(config.permission_mode_arg(), expected);
        }
    }

    #[test]
    fn claude_code_config_base_session_args_are_structured_stream() {
        let config = ClaudeCodeConfig::new()
            .with_permission_mode(ExternalPermissionMode::AcceptEdits)
            .with_model("claude-sonnet");
        let args = config.base_session_args();
        assert_eq!(
            args,
            vec![
                "--print",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--permission-mode",
                "acceptEdits",
                "--model",
                "claude-sonnet",
            ]
        );

        // Without a model the `--model` pair is omitted entirely.
        let no_model = ClaudeCodeConfig::new().base_session_args();
        assert!(!no_model.iter().any(|arg| arg == "--model"));
    }

    #[test]
    fn claude_code_config_roundtrips_through_serde() {
        let config = ClaudeCodeConfig::new()
            .with_binary("/opt/claude/bin/claude")
            .with_env("ANTHROPIC_API_KEY", "sk-secret-value")
            .with_working_dir("/tmp/worktree")
            .with_permission_mode(ExternalPermissionMode::Plan)
            .with_model("claude-opus")
            .with_profile("reviewer")
            .with_timeout(Duration::from_secs(90))
            .with_read_idle_timeout(Duration::from_secs(1200))
            .with_shutdown_grace(Duration::from_secs(45));

        let encoded = serde_json::to_string(&config).expect("serialize config");
        let decoded: ClaudeCodeConfig = serde_json::from_str(&encoded).expect("deserialize config");
        assert_eq!(decoded, config);

        // The permissive default serializes without the skipped optional fields.
        let default_encoded =
            serde_json::to_value(ClaudeCodeConfig::default()).expect("serialize default");
        let obj = default_encoded.as_object().expect("config is an object");
        assert!(!obj.contains_key("env"));
        assert!(!obj.contains_key("working_dir"));
        assert!(!obj.contains_key("model"));
        assert!(!obj.contains_key("profile"));
    }

    #[test]
    fn claude_code_config_old_json_without_idle_fields_uses_defaults() {
        // Configs persisted before `read_idle_timeout`/`shutdown_grace` existed
        // must still deserialize, picking up the new defaults rather than the
        // (short) probe/launch timeout.
        let mut legacy = serde_json::to_value(ClaudeCodeConfig::new()).expect("serialize");
        let obj = legacy.as_object_mut().expect("config is an object");
        obj.remove("read_idle_timeout");
        obj.remove("shutdown_grace");

        let decoded: ClaudeCodeConfig =
            serde_json::from_value(legacy).expect("deserialize legacy config");
        assert_eq!(decoded.read_idle_timeout(), DEFAULT_READ_IDLE_TIMEOUT);
        assert_eq!(decoded.shutdown_grace(), DEFAULT_SHUTDOWN_GRACE);
        assert_eq!(decoded.timeout(), DEFAULT_TIMEOUT);
    }

    #[test]
    fn claude_code_config_debug_redacts_env_secrets() {
        let secret = "sk-super-secret-key";
        let config = ClaudeCodeConfig::new().with_env("ANTHROPIC_API_KEY", secret);

        let rendered = format!("{config:?}");
        // The variable name is debuggable, but the value must never appear.
        assert!(rendered.contains("ANTHROPIC_API_KEY"));
        assert!(rendered.contains("<redacted>"));
        assert!(
            !rendered.contains(secret),
            "debug output leaked the env secret: {rendered}"
        );
    }
}
