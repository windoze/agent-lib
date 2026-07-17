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
//! directory with the spawned process's current directory (equivalent to
//! `--dir`). Runtime permission gating on `run` is expressed only by the single
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
///   invocation (and, later, to launch handshakes).
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
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
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
    /// `--continue` / `--session <id>` shape); the working directory is applied to
    /// the spawned process rather than passed as `--dir`. The probe does not use
    /// these arguments — it inspects `--version` / `--help` / `run --help`
    /// instead.
    #[must_use]
    pub fn base_run_args(&self) -> Vec<String> {
        let mut args = vec!["run".to_owned(), "--format".to_owned(), "json".to_owned()];
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
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_BINARY, DEFAULT_TIMEOUT, OpenCodeConfig};
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
            .with_timeout(Duration::from_secs(90));

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
