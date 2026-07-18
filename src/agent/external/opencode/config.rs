//! Launch configuration for the managed OpenCode runtime adapter.
//!
//! [`OpenCodeConfig`] is the data-only recipe a host hands the OpenCode adapter
//! so it can *probe* the CLI (this milestone) and later *launch* live
//! `opencode run` sessions (M8-3). It records the binary to invoke, environment
//! overrides, the working directory / worktree, the provider-neutral permission
//! mode (mapped onto OpenCode's `--auto` approval flag), an optional model and
//! preset agent, and a probe/launch timeout. It holds no live process, channel,
//! or task handle — those stay behind the adapter and the
//! [`ExternalRuntimeHandles`](crate::agent::external::ExternalRuntimeHandles)
//! boundary (design §4, §14).
//!
//! # CLI argument layout
//!
//! OpenCode's non-interactive entry point is the `run` subcommand. The structured
//! event stream is selected with `--format json` (OpenCode's raw JSON event
//! format; note it is *not* a bare `--json` flag), the model with
//! `-m/--model provider/model`, a preset agent with `--agent`, and the working
//! directory with an explicit `--dir <path>`. OpenCode resolves its project /
//! file operations from `--dir` (falling back to the inherited `$PWD`), *not*
//! from the child's OS-level current directory alone, so the working directory
//! must be passed as `--dir` for worktree isolation; the launcher also applies
//! it as the spawned process's cwd as a belt-and-suspenders measure. Runtime
//! permission gating on `run` is expressed only by the single
//! `--auto` flag ("auto-approve permissions that are not explicitly denied");
//! finer-grained read-only / accept-edits behaviour lives in OpenCode's
//! agent/permission configuration selected via `--agent`, not in dedicated `run`
//! flags. [`base_run_args`](OpenCodeConfig::base_run_args) emits the managed-mode
//! launch shape and maps the neutral permission mode conservatively:
//! [`auto_approve`](OpenCodeConfig::auto_approve) is `true` only for
//! [`BypassPermissions`](ExternalPermissionMode::BypassPermissions), so a lesser
//! mode never silently widens the host's permission boundary by passing
//! `--auto`.
//!
//! # Secret hygiene
//!
//! [`env`](OpenCodeConfig::env) can carry credentials (a provider API key, an
//! auth token). The manual [`Debug`] impl therefore prints only the environment
//! variable *names* with redacted values, never the values themselves, so a
//! config that lands in a log or a panic message cannot leak a secret (design
//! constraint "任何可能包含 secret … 的日志/错误必须脱敏"). Serialization is a
//! separate, deliberate persistence path and does round-trip the values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::external::ExternalPermissionMode;

/// The default OpenCode CLI binary looked up on `PATH`.
const DEFAULT_BINARY: &str = "opencode";

/// The default probe/launch timeout applied when a caller does not set one.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The default per-line stdout idle timeout for a live `opencode run` turn
/// process.
///
/// This is deliberately far longer than [`DEFAULT_TIMEOUT`]: a turn can run a
/// silent build or test suite for minutes without emitting a frame, and that
/// silence must not be mistaken for a dead CLI.
const DEFAULT_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// The default grace period a close waits for the turn process to exit on its
/// own before force-killing the child.
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// Serde default for [`OpenCodeConfig::read_idle_timeout`], so configs
/// persisted before the field existed still deserialize.
const fn default_read_idle_timeout() -> Duration {
    DEFAULT_READ_IDLE_TIMEOUT
}

/// Serde default for [`OpenCodeConfig::shutdown_grace`], so configs persisted
/// before the field existed still deserialize.
const fn default_shutdown_grace() -> Duration {
    DEFAULT_SHUTDOWN_GRACE
}

/// Data-only launch configuration for the managed OpenCode adapter.
///
/// Build one with [`OpenCodeConfig::new`] (or [`Default`], which uses the
/// `opencode` binary on `PATH`) and refine it with the chained `with_*` setters.
/// The config is plain, serializable data: it round-trips through serde so a host
/// can persist it, and it carries no live handles.
///
/// # Fields
///
/// - [`binary`](Self::binary): the CLI executable, defaulting to `opencode`
///   resolved on `PATH`; override it with an absolute path for a pinned install.
/// - [`env`](Self::env): extra environment variables layered onto the child
///   process (for example a scoped provider API key). Redacted in [`Debug`].
/// - [`working_dir`](Self::working_dir): the directory (typically the agent's
///   worktree) the CLI runs in; `None` inherits the parent's directory.
/// - [`permission_mode`](Self::permission_mode): the provider-neutral
///   [`ExternalPermissionMode`] mapped onto OpenCode's `--auto` approval flag.
/// - [`model`](Self::model): optional `-m/--model provider/model` selector.
/// - [`agent`](Self::agent): optional `--agent` preset selector (OpenCode's
///   agent/permission configuration).
/// - [`timeout`](Self::timeout): the wall-clock bound applied to a probe
///   invocation and to the launch handshake.
/// - [`read_idle_timeout`](Self::read_idle_timeout): the per-line stdout idle
///   bound for a live turn process — how long the CLI may stay silent between
///   frames before the turn is declared lost.
/// - [`shutdown_grace`](Self::shutdown_grace): how long a close waits for the
///   turn process to exit on its own before force-killing the child.
///
/// # The three timeouts
///
/// [`timeout`](Self::timeout) bounds only one-shot control operations
/// (probe, launch). Steady-state session IO uses the other two:
/// [`read_idle_timeout`](Self::read_idle_timeout) guards each stdout line read
/// so a long silent command (a build, a test suite) is not mistaken for a
/// dead CLI, and [`shutdown_grace`](Self::shutdown_grace) bounds the graceful
/// close before the fallback kill. All three are independent knobs. Because
/// `opencode run` is a one-shot process per turn, the idle bound applies
/// within a single turn rather than across a long-lived session.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenCodeConfig {
    binary: PathBuf,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_dir: Option<PathBuf>,
    permission_mode: ExternalPermissionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    timeout: Duration,
    #[serde(default = "default_read_idle_timeout")]
    read_idle_timeout: Duration,
    #[serde(default = "default_shutdown_grace")]
    shutdown_grace: Duration,
}

impl Default for OpenCodeConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from(DEFAULT_BINARY),
            env: BTreeMap::new(),
            working_dir: None,
            permission_mode: ExternalPermissionMode::Prompt,
            model: None,
            agent: None,
            timeout: DEFAULT_TIMEOUT,
            read_idle_timeout: DEFAULT_READ_IDLE_TIMEOUT,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        }
    }
}

impl OpenCodeConfig {
    /// Creates a config equal to [`Default`]: the `opencode` binary on `PATH`, no
    /// env overrides, prompt-on-action permissions, and the default timeout.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the CLI binary path (default: `opencode` resolved on `PATH`).
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

    /// Sets the provider-neutral permission mode mapped onto OpenCode's `--auto`
    /// approval flag.
    #[must_use]
    pub const fn with_permission_mode(mut self, mode: ExternalPermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    /// Sets the optional `-m/--model provider/model` selector.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Sets the optional `--agent` preset selector.
    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
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

    /// Sets the per-line stdout idle timeout for a live turn process.
    ///
    /// Each stdout line read is bounded by this duration; exceeding it
    /// declares the session lost. The default (10 minutes) leaves room for
    /// long silent commands such as builds or test suites.
    #[must_use]
    pub const fn with_read_idle_timeout(mut self, read_idle_timeout: Duration) -> Self {
        self.read_idle_timeout = read_idle_timeout;
        self
    }

    /// Sets the grace period a close waits for the turn process to exit on its
    /// own before force-killing the child.
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

    /// Returns the optional `-m/--model` selector.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Returns the optional `--agent` preset selector.
    #[must_use]
    pub fn agent(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Returns the probe/launch timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Returns the per-line stdout idle timeout for a live turn process.
    #[must_use]
    pub const fn read_idle_timeout(&self) -> Duration {
        self.read_idle_timeout
    }

    /// Returns the grace period a close waits before force-killing.
    #[must_use]
    pub const fn shutdown_grace(&self) -> Duration {
        self.shutdown_grace
    }

    /// Whether the configured permission mode maps onto OpenCode's `--auto`
    /// approval flag.
    ///
    /// `--auto` means "auto-approve permissions that are not explicitly denied",
    /// i.e. a blanket bypass of the runtime's approval prompts. Only
    /// [`BypassPermissions`](ExternalPermissionMode::BypassPermissions), where the
    /// host explicitly accepts full responsibility, maps onto it. Every other
    /// mode leaves `--auto` off so gated actions are routed to the host's
    /// permission bridge (or denied by OpenCode's default configuration) rather
    /// than silently auto-approved:
    ///
    /// - [`Prompt`](ExternalPermissionMode::Prompt): every gated action needs
    ///   approval, so nothing is pre-authorized.
    /// - [`AcceptEdits`](ExternalPermissionMode::AcceptEdits): OpenCode's `run`
    ///   flags cannot express "auto-approve only worktree edits", so passing
    ///   `--auto` would over-approve; the narrower policy is expressed through an
    ///   [`agent`](Self::agent) preset instead.
    /// - [`Plan`](ExternalPermissionMode::Plan): read-only planning has nothing to
    ///   approve.
    ///
    /// The runtime's output never widens the host's permission boundary
    /// regardless of this value (design §10).
    #[must_use]
    pub const fn auto_approve(&self) -> bool {
        matches!(
            self.permission_mode,
            ExternalPermissionMode::BypassPermissions
        )
    }

    /// Builds the base managed-mode CLI arguments for a live `opencode run`
    /// session.
    ///
    /// This is the structured-stream launch shape from design §14: the `run`
    /// subcommand, then `--format json` to select OpenCode's raw JSON event
    /// stream, then the `--auto` approval bypass when (and only when) the
    /// permission mode is
    /// [`BypassPermissions`](ExternalPermissionMode::BypassPermissions), and
    /// finally the optional `--model` / `--agent` selectors. The adapter's live
    /// session (M8-3) appends the per-turn prompt (and, for a resume, a
    /// `--session <id>` shape); when a [`working_dir`](Self::working_dir) is set
    /// it is passed as an explicit `--dir <path>`, because OpenCode resolves its
    /// file operations from `--dir`/`$PWD` and *not* from the child's OS-level
    /// cwd alone — omitting it lets OpenCode write into the launching checkout's
    /// directory (the inherited `$PWD`) instead of the intended worktree. The
    /// probe does not use these arguments — it inspects `--version` / `--help` /
    /// `run --help` instead.
    #[must_use]
    pub fn base_run_args(&self) -> Vec<String> {
        let mut args = vec!["run".to_owned(), "--format".to_owned(), "json".to_owned()];
        if let Some(dir) = &self.working_dir {
            args.push("--dir".to_owned());
            args.push(dir.to_string_lossy().into_owned());
        }
        if self.auto_approve() {
            args.push("--auto".to_owned());
        }
        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }
        if let Some(agent) = &self.agent {
            args.push("--agent".to_owned());
            args.push(agent.clone());
        }
        args
    }

    /// Builds the base managed-mode CLI arguments for *resuming* a live
    /// `opencode run` session by its runtime-assigned id.
    ///
    /// OpenCode has no separate resume subcommand: a follow-up turn is a fresh
    /// `opencode run` process that continues an existing session with the
    /// `-s/--session <ID>` flag (verified against the CLI reference:
    /// `opencode run` accepts `--continue`/`--session` alongside `--format`,
    /// `--auto`, `--model`, and `--agent`). This therefore reuses the whole
    /// [`base_run_args`](Self::base_run_args) launch shape — `run --format json`
    /// plus the conditional `--auto` / `--model` / `--agent` selectors — and
    /// appends `--session <session_id>`. The adapter's live session (M8-3)
    /// appends the per-turn follow-up message as the trailing positional
    /// argument; the working directory rides along as the explicit `--dir <path>`
    /// emitted by the inherited [`base_run_args`](Self::base_run_args) shape.
    #[must_use]
    pub fn base_resume_args(&self, session_id: &str) -> Vec<String> {
        let mut args = self.base_run_args();
        args.push("--session".to_owned());
        args.push(session_id.to_owned());
        args
    }
}

impl std::fmt::Debug for OpenCodeConfig {
    /// Redacts environment values so a logged config cannot leak a secret.
    ///
    /// Every other field is stable, non-secret configuration and is shown as-is;
    /// [`env`](OpenCodeConfig::env) is rendered as its keys mapped to a
    /// `"<redacted>"` placeholder so the *shape* is debuggable without exposing
    /// credential values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_env: BTreeMap<&String, &str> =
            self.env.keys().map(|key| (key, "<redacted>")).collect();
        f.debug_struct("OpenCodeConfig")
            .field("binary", &self.binary)
            .field("env", &redacted_env)
            .field("working_dir", &self.working_dir)
            .field("permission_mode", &self.permission_mode)
            .field("model", &self.model)
            .field("agent", &self.agent)
            .field("timeout", &self.timeout)
            .field("read_idle_timeout", &self.read_idle_timeout)
            .field("shutdown_grace", &self.shutdown_grace)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_BINARY, DEFAULT_READ_IDLE_TIMEOUT, DEFAULT_SHUTDOWN_GRACE, DEFAULT_TIMEOUT,
        OpenCodeConfig,
    };
    use crate::agent::external::ExternalPermissionMode;
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn opencode_config_defaults_are_permissive() {
        let config = OpenCodeConfig::default();
        assert_eq!(config.binary(), Path::new(DEFAULT_BINARY));
        assert!(config.env().is_empty());
        assert!(config.working_dir().is_none());
        assert_eq!(config.permission_mode(), ExternalPermissionMode::Prompt);
        assert!(config.model().is_none());
        assert!(config.agent().is_none());
        assert_eq!(config.timeout(), DEFAULT_TIMEOUT);
        assert_eq!(config.read_idle_timeout(), DEFAULT_READ_IDLE_TIMEOUT);
        assert_eq!(config.shutdown_grace(), DEFAULT_SHUTDOWN_GRACE);
    }

    #[test]
    fn opencode_config_auto_approve_only_for_bypass() {
        let cases = [
            (ExternalPermissionMode::Prompt, false),
            (ExternalPermissionMode::AcceptEdits, false),
            (ExternalPermissionMode::Plan, false),
            (ExternalPermissionMode::BypassPermissions, true),
        ];
        for (mode, expected) in cases {
            let config = OpenCodeConfig::new().with_permission_mode(mode);
            assert_eq!(config.auto_approve(), expected, "mode={mode:?}");
        }
    }

    #[test]
    fn opencode_config_base_run_args_select_json_stream() {
        // A bypassing config with model + agent emits the full managed shape.
        let config = OpenCodeConfig::new()
            .with_permission_mode(ExternalPermissionMode::BypassPermissions)
            .with_model("anthropic/claude-sonnet-4")
            .with_agent("build");
        assert_eq!(
            config.base_run_args(),
            vec![
                "run",
                "--format",
                "json",
                "--auto",
                "--model",
                "anthropic/claude-sonnet-4",
                "--agent",
                "build",
            ]
        );

        // The `run` subcommand and its `--format json` stream selector lead.
        let args = config.base_run_args();
        assert_eq!(args.first().map(String::as_str), Some("run"));
        let format_pos = args.iter().position(|a| a == "--format").expect("--format");
        assert_eq!(args.get(format_pos + 1).map(String::as_str), Some("json"));

        // A non-bypassing config omits `--auto` and any unset selectors.
        let prompt = OpenCodeConfig::new().base_run_args();
        assert_eq!(prompt, vec!["run", "--format", "json"]);
        assert!(!prompt.iter().any(|arg| arg == "--auto"));
        assert!(!prompt.iter().any(|arg| arg == "--model"));
        assert!(!prompt.iter().any(|arg| arg == "--agent"));
        // With no working directory there is no `--dir`.
        assert!(!prompt.iter().any(|arg| arg == "--dir"));
    }

    #[test]
    fn opencode_config_base_run_args_pass_working_dir_as_dir() {
        // The working directory must be passed explicitly as `--dir <path>`:
        // OpenCode resolves file operations from `--dir`/`$PWD`, not the child's
        // OS-level cwd alone, so relying on the spawned process's current
        // directory leaks writes into the launching checkout (design §14).
        let config = OpenCodeConfig::new().with_working_dir("/tmp/scratch-worktree");
        let args = config.base_run_args();
        let dir_pos = args
            .iter()
            .position(|a| a == "--dir")
            .expect("--dir is present when a working_dir is configured");
        assert_eq!(
            args.get(dir_pos + 1).map(String::as_str),
            Some("/tmp/scratch-worktree")
        );
        // `run --format json` still leads before `--dir`.
        assert_eq!(&args[..3], &["run", "--format", "json"]);

        // Resume inherits the same `--dir` from the shared run shape.
        let resume = config.base_resume_args("ses_dir");
        assert!(
            resume
                .windows(2)
                .any(|w| w == ["--dir", "/tmp/scratch-worktree"])
        );
    }

    #[test]
    fn opencode_config_base_resume_args_continue_a_session() {
        // Resume reuses the full run launch shape and appends `--session <id>`.
        let config = OpenCodeConfig::new()
            .with_permission_mode(ExternalPermissionMode::BypassPermissions)
            .with_model("anthropic/claude-sonnet-4")
            .with_agent("build");
        let resume = config.base_resume_args("ses_abc123");
        assert_eq!(
            resume,
            vec![
                "run",
                "--format",
                "json",
                "--auto",
                "--model",
                "anthropic/claude-sonnet-4",
                "--agent",
                "build",
                "--session",
                "ses_abc123",
            ]
        );
        // Resume shares the `run --format json` prefix with a fresh launch.
        assert!(resume.starts_with(config.base_run_args().as_slice()));

        // A permissive default resume is just the JSON run shape plus `--session`.
        let bare = OpenCodeConfig::new().base_resume_args("ses_xyz");
        assert_eq!(
            bare,
            vec!["run", "--format", "json", "--session", "ses_xyz"]
        );
        assert!(!bare.iter().any(|arg| arg == "--auto"));
    }

    #[test]
    fn opencode_config_roundtrips_through_serde() {
        let config = OpenCodeConfig::new()
            .with_binary("/opt/opencode/bin/opencode")
            .with_env("ANTHROPIC_API_KEY", "sk-secret-value")
            .with_working_dir("/tmp/worktree")
            .with_permission_mode(ExternalPermissionMode::Plan)
            .with_model("anthropic/claude-sonnet-4")
            .with_agent("plan")
            .with_timeout(Duration::from_secs(90))
            .with_read_idle_timeout(Duration::from_secs(1200))
            .with_shutdown_grace(Duration::from_secs(45));

        let encoded = serde_json::to_string(&config).expect("serialize config");
        let decoded: OpenCodeConfig = serde_json::from_str(&encoded).expect("deserialize config");
        assert_eq!(decoded, config);

        // The permissive default serializes without the skipped optional fields.
        let default_encoded =
            serde_json::to_value(OpenCodeConfig::default()).expect("serialize default");
        let obj = default_encoded.as_object().expect("config is an object");
        assert!(!obj.contains_key("env"));
        assert!(!obj.contains_key("working_dir"));
        assert!(!obj.contains_key("model"));
        assert!(!obj.contains_key("agent"));
    }

    #[test]
    fn opencode_config_old_json_without_idle_fields_uses_defaults() {
        // Configs persisted before `read_idle_timeout`/`shutdown_grace` existed
        // must still deserialize, picking up the new defaults rather than the
        // (short) probe/launch timeout.
        let mut legacy = serde_json::to_value(OpenCodeConfig::new()).expect("serialize");
        let obj = legacy.as_object_mut().expect("config is an object");
        obj.remove("read_idle_timeout");
        obj.remove("shutdown_grace");

        let decoded: OpenCodeConfig =
            serde_json::from_value(legacy).expect("deserialize legacy config");
        assert_eq!(decoded.read_idle_timeout(), DEFAULT_READ_IDLE_TIMEOUT);
        assert_eq!(decoded.shutdown_grace(), DEFAULT_SHUTDOWN_GRACE);
        assert_eq!(decoded.timeout(), DEFAULT_TIMEOUT);
    }

    #[test]
    fn opencode_config_debug_redacts_env_secrets() {
        let secret = "sk-super-secret-key";
        let config = OpenCodeConfig::new().with_env("ANTHROPIC_API_KEY", secret);

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
