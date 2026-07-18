//! Launch configuration for the managed Codex runtime adapter.
//!
//! [`CodexConfig`] is the data-only recipe a host hands the Codex adapter so it
//! can *probe* the CLI (this milestone) and later *launch* live `codex exec`
//! sessions (M7-3). It records the binary to invoke, environment overrides, the
//! working directory / worktree, the provider-neutral permission mode (mapped
//! onto Codex's sandbox + approval flags), an optional model and profile, and a
//! probe/launch timeout. It holds no live process, channel, or task handle —
//! those stay behind the adapter and the
//! [`ExternalRuntimeHandles`](crate::agent::external::ExternalRuntimeHandles)
//! boundary (design §4, §12).
//!
//! # CLI argument ordering
//!
//! The Codex CLI splits its flags between the top-level command and the `exec`
//! subcommand. The approval policy (`-a/--ask-for-approval`) is a *top-level*
//! flag and must appear **before** the `exec` subcommand, while the sandbox
//! policy (`-s/--sandbox`), the structured `--json` stream, `--skip-git-repo-check`,
//! `--model`, and `--profile` are `exec` flags that follow it. [`base_exec_args`](CodexConfig::base_exec_args)
//! emits that exact order so a caller cannot accidentally place a global flag
//! after the subcommand (a common Codex CLI footgun called out in the design).
//!
//! # Secret hygiene
//!
//! [`env`](CodexConfig::env) can carry credentials (an `OPENAI_API_KEY`, an auth
//! token). The manual [`Debug`] impl therefore prints only the environment
//! variable *names* with redacted values, never the values themselves, so a
//! config that lands in a log or a panic message cannot leak a secret (design
//! constraint "任何可能包含 secret … 的日志/错误必须脱敏"). Serialization is a
//! separate, deliberate persistence path and does round-trip the values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::external::ExternalPermissionMode;

/// The default Codex CLI binary looked up on `PATH`.
const DEFAULT_BINARY: &str = "codex";

/// The default probe/launch timeout applied when a caller does not set one.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The default per-line stdout idle timeout for a live `codex exec` turn
/// process.
///
/// This is deliberately far longer than [`DEFAULT_TIMEOUT`]: a turn can run a
/// silent build or test suite for minutes without emitting a frame, and that
/// silence must not be mistaken for a dead CLI.
const DEFAULT_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// The default grace period a close waits for the turn process to exit on its
/// own before force-killing the child.
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// Serde default for [`CodexConfig::read_idle_timeout`], so configs persisted
/// before the field existed still deserialize.
const fn default_read_idle_timeout() -> Duration {
    DEFAULT_READ_IDLE_TIMEOUT
}

/// Serde default for [`CodexConfig::shutdown_grace`], so configs persisted
/// before the field existed still deserialize.
const fn default_shutdown_grace() -> Duration {
    DEFAULT_SHUTDOWN_GRACE
}

/// Data-only launch configuration for the managed Codex adapter.
///
/// Build one with [`CodexConfig::new`] (or [`Default`], which uses the `codex`
/// binary on `PATH`) and refine it with the chained `with_*` setters. The config
/// is plain, serializable data: it round-trips through serde so a host can
/// persist it, and it carries no live handles.
///
/// # Fields
///
/// - [`binary`](Self::binary): the CLI executable, defaulting to `codex`
///   resolved on `PATH`; override it with an absolute path for a pinned install.
/// - [`env`](Self::env): extra environment variables layered onto the child
///   process (for example a scoped `OPENAI_API_KEY`). Redacted in [`Debug`].
/// - [`working_dir`](Self::working_dir): the directory (typically the agent's
///   worktree) the CLI runs in; `None` inherits the parent's directory.
/// - [`permission_mode`](Self::permission_mode): the provider-neutral
///   [`ExternalPermissionMode`] mapped onto Codex's `-a/--ask-for-approval` and
///   `-s/--sandbox` flags.
/// - [`model`](Self::model) / [`profile`](Self::profile): optional `-m/--model`
///   and `-p/--profile` selectors.
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
/// `codex exec` is a one-shot process per turn, the idle bound applies within
/// a single turn rather than across a long-lived session.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexConfig {
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

impl Default for CodexConfig {
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

impl CodexConfig {
    /// Creates a permissive config equal to [`Default`]: the `codex` binary on
    /// `PATH`, no env overrides, prompt-on-action permissions, and the default
    /// timeout.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the CLI binary path (default: `codex` resolved on `PATH`).
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

    /// Sets the provider-neutral permission mode mapped onto Codex's approval and
    /// sandbox flags.
    #[must_use]
    pub const fn with_permission_mode(mut self, mode: ExternalPermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    /// Sets the optional `-m/--model` selector.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Sets the optional `-p/--profile` selector.
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

    /// Returns the optional `-p/--profile` selector.
    #[must_use]
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
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

    /// Maps the configured [`ExternalPermissionMode`] onto Codex's
    /// `-a/--ask-for-approval` policy value.
    ///
    /// The mapping follows the current Codex CLI vocabulary
    /// (`untrusted` / `on-request` / `never`), pairing with
    /// [`sandbox_mode_arg`](Self::sandbox_mode_arg):
    ///
    /// - [`Prompt`](ExternalPermissionMode::Prompt) → `untrusted`: only trusted
    ///   commands run unattended; anything else escalates to the host.
    /// - [`AcceptEdits`](ExternalPermissionMode::AcceptEdits) → `on-request`: the
    ///   model asks for approval only when it judges an action risky, while
    ///   worktree edits ride on the `workspace-write` sandbox.
    /// - [`Plan`](ExternalPermissionMode::Plan) → `never`: read-only planning has
    ///   nothing to approve.
    /// - [`BypassPermissions`](ExternalPermissionMode::BypassPermissions) →
    ///   `never`: the host accepts full responsibility.
    ///
    /// The runtime's output never widens the host's permission boundary
    /// regardless of this value (design §10).
    #[must_use]
    pub const fn approval_policy_arg(&self) -> &'static str {
        match self.permission_mode {
            ExternalPermissionMode::Prompt => "untrusted",
            ExternalPermissionMode::AcceptEdits => "on-request",
            ExternalPermissionMode::Plan | ExternalPermissionMode::BypassPermissions => "never",
        }
    }

    /// Maps the configured [`ExternalPermissionMode`] onto Codex's `-s/--sandbox`
    /// policy value.
    ///
    /// The mapping follows the current Codex CLI vocabulary
    /// (`read-only` / `workspace-write` / `danger-full-access`):
    ///
    /// - [`Prompt`](ExternalPermissionMode::Prompt) and
    ///   [`Plan`](ExternalPermissionMode::Plan) → `read-only`: no filesystem
    ///   mutation without an approved escalation.
    /// - [`AcceptEdits`](ExternalPermissionMode::AcceptEdits) → `workspace-write`:
    ///   edits inside the worktree are allowed without prompting.
    /// - [`BypassPermissions`](ExternalPermissionMode::BypassPermissions) →
    ///   `danger-full-access`: unrestricted, host takes responsibility.
    #[must_use]
    pub const fn sandbox_mode_arg(&self) -> &'static str {
        match self.permission_mode {
            ExternalPermissionMode::Prompt | ExternalPermissionMode::Plan => "read-only",
            ExternalPermissionMode::AcceptEdits => "workspace-write",
            ExternalPermissionMode::BypassPermissions => "danger-full-access",
        }
    }

    /// Builds the base managed-mode CLI arguments for a live `codex exec` session.
    ///
    /// This is the structured-stream launch shape from design §12: the top-level
    /// approval flag first (`-a <policy>`), then the `exec` subcommand, then its
    /// flags (`--json` for the JSONL event stream, `-s <sandbox>`,
    /// `--skip-git-repo-check` so a fresh worktree that is not yet a git repo does
    /// not abort the launch, and the optional `--model` / `--profile` selectors).
    /// The adapter's live session (M7-3) appends the per-turn prompt (and, for a
    /// resume, an `exec resume <id>` shape); the working directory is applied to
    /// the spawned process rather than passed as `--cd`. The probe does not use
    /// these arguments — it inspects `--version` / `--help` / `exec --help`
    /// instead.
    #[must_use]
    pub fn base_exec_args(&self) -> Vec<String> {
        let mut args = vec![
            "-a".to_owned(),
            self.approval_policy_arg().to_owned(),
            "exec".to_owned(),
            "--json".to_owned(),
            "-s".to_owned(),
            self.sandbox_mode_arg().to_owned(),
            "--skip-git-repo-check".to_owned(),
        ];
        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }
        if let Some(profile) = &self.profile {
            args.push("--profile".to_owned());
            args.push(profile.clone());
        }
        args
    }

    /// Builds the base managed-mode CLI arguments for resuming a live session
    /// with `codex exec resume <session_id>`.
    ///
    /// The `resume` subcommand does **not** accept the `-s/--sandbox` or
    /// `-p/--profile` flags that [`base_exec_args`](Self::base_exec_args) places
    /// after `exec` (verified against the current CLI: `codex exec resume --help`
    /// advertises only `--json`, `--skip-git-repo-check`, and `-m/--model`). The
    /// sandbox policy, model, and profile are therefore hoisted to their
    /// *top-level* positions before the `exec` subcommand — where the CLI also
    /// accepts them — while the approval policy stays top-level as it does for a
    /// fresh launch and `--json` / `--skip-git-repo-check` ride on the `resume`
    /// subcommand. The adapter's live session (M7-3) appends the per-turn
    /// follow-up message after the session id; the working directory is applied
    /// to the spawned process rather than passed as `--cd`.
    #[must_use]
    pub fn base_resume_args(&self, session_id: &str) -> Vec<String> {
        let mut args = vec![
            "-a".to_owned(),
            self.approval_policy_arg().to_owned(),
            "-s".to_owned(),
            self.sandbox_mode_arg().to_owned(),
        ];
        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }
        if let Some(profile) = &self.profile {
            args.push("--profile".to_owned());
            args.push(profile.clone());
        }
        args.push("exec".to_owned());
        args.push("resume".to_owned());
        args.push("--json".to_owned());
        args.push("--skip-git-repo-check".to_owned());
        args.push(session_id.to_owned());
        args
    }
}

impl std::fmt::Debug for CodexConfig {
    /// Redacts environment values so a logged config cannot leak a secret.
    ///
    /// Every other field is stable, non-secret configuration and is shown as-is;
    /// [`env`](CodexConfig::env) is rendered as its keys mapped to a
    /// `"<redacted>"` placeholder so the *shape* is debuggable without exposing
    /// credential values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_env: BTreeMap<&String, &str> =
            self.env.keys().map(|key| (key, "<redacted>")).collect();
        f.debug_struct("CodexConfig")
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
        CodexConfig, DEFAULT_BINARY, DEFAULT_READ_IDLE_TIMEOUT, DEFAULT_SHUTDOWN_GRACE,
        DEFAULT_TIMEOUT,
    };
    use crate::agent::external::ExternalPermissionMode;
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn codex_config_defaults_are_permissive() {
        let config = CodexConfig::default();
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
    fn codex_config_approval_and_sandbox_map_every_mode() {
        let cases = [
            (ExternalPermissionMode::Prompt, "untrusted", "read-only"),
            (
                ExternalPermissionMode::AcceptEdits,
                "on-request",
                "workspace-write",
            ),
            (ExternalPermissionMode::Plan, "never", "read-only"),
            (
                ExternalPermissionMode::BypassPermissions,
                "never",
                "danger-full-access",
            ),
        ];
        for (mode, approval, sandbox) in cases {
            let config = CodexConfig::new().with_permission_mode(mode);
            assert_eq!(config.approval_policy_arg(), approval);
            assert_eq!(config.sandbox_mode_arg(), sandbox);
        }
    }

    #[test]
    fn codex_config_base_exec_args_put_global_flag_before_subcommand() {
        let config = CodexConfig::new()
            .with_permission_mode(ExternalPermissionMode::AcceptEdits)
            .with_model("gpt-5-codex")
            .with_profile("reviewer");
        let args = config.base_exec_args();
        assert_eq!(
            args,
            vec![
                "-a",
                "on-request",
                "exec",
                "--json",
                "-s",
                "workspace-write",
                "--skip-git-repo-check",
                "--model",
                "gpt-5-codex",
                "--profile",
                "reviewer",
            ]
        );

        // The global approval flag must precede the `exec` subcommand.
        let approval_pos = args.iter().position(|a| a == "-a").expect("approval flag");
        let exec_pos = args.iter().position(|a| a == "exec").expect("exec subcmd");
        assert!(
            approval_pos < exec_pos,
            "global -a must come before exec: {args:?}"
        );

        // Without model/profile those pairs are omitted entirely.
        let bare = CodexConfig::new().base_exec_args();
        assert!(!bare.iter().any(|arg| arg == "--model"));
        assert!(!bare.iter().any(|arg| arg == "--profile"));
    }

    #[test]
    fn codex_config_base_resume_args_hoist_sandbox_and_selectors_before_exec() {
        let config = CodexConfig::new()
            .with_permission_mode(ExternalPermissionMode::AcceptEdits)
            .with_model("gpt-5-codex")
            .with_profile("reviewer");
        let args = config.base_resume_args("thread-42");
        assert_eq!(
            args,
            vec![
                "-a",
                "on-request",
                "-s",
                "workspace-write",
                "--model",
                "gpt-5-codex",
                "--profile",
                "reviewer",
                "exec",
                "resume",
                "--json",
                "--skip-git-repo-check",
                "thread-42",
            ]
        );

        // The `resume` subcommand rejects `-s`/`-p`, so both the sandbox flag and
        // the profile selector must precede the `exec` subcommand.
        let exec_pos = args.iter().position(|a| a == "exec").expect("exec subcmd");
        let sandbox_pos = args.iter().position(|a| a == "-s").expect("sandbox flag");
        let profile_pos = args
            .iter()
            .position(|a| a == "--profile")
            .expect("profile flag");
        assert!(sandbox_pos < exec_pos, "-s must precede exec: {args:?}");
        assert!(
            profile_pos < exec_pos,
            "--profile must precede exec: {args:?}"
        );
        // The session id is the trailing positional argument (the message the
        // adapter appends follows it).
        assert_eq!(args.last().map(String::as_str), Some("thread-42"));

        // Without model/profile those pairs are omitted entirely.
        let bare = CodexConfig::new().base_resume_args("thread-1");
        assert!(!bare.iter().any(|arg| arg == "--model"));
        assert!(!bare.iter().any(|arg| arg == "--profile"));
        assert_eq!(
            bare,
            vec![
                "-a",
                "untrusted",
                "-s",
                "read-only",
                "exec",
                "resume",
                "--json",
                "--skip-git-repo-check",
                "thread-1",
            ]
        );
    }

    #[test]
    fn codex_config_roundtrips_through_serde() {
        let config = CodexConfig::new()
            .with_binary("/opt/codex/bin/codex")
            .with_env("OPENAI_API_KEY", "sk-secret-value")
            .with_working_dir("/tmp/worktree")
            .with_permission_mode(ExternalPermissionMode::Plan)
            .with_model("gpt-5-codex")
            .with_profile("reviewer")
            .with_timeout(Duration::from_secs(90))
            .with_read_idle_timeout(Duration::from_secs(1200))
            .with_shutdown_grace(Duration::from_secs(45));

        let encoded = serde_json::to_string(&config).expect("serialize config");
        let decoded: CodexConfig = serde_json::from_str(&encoded).expect("deserialize config");
        assert_eq!(decoded, config);

        // The permissive default serializes without the skipped optional fields.
        let default_encoded =
            serde_json::to_value(CodexConfig::default()).expect("serialize default");
        let obj = default_encoded.as_object().expect("config is an object");
        assert!(!obj.contains_key("env"));
        assert!(!obj.contains_key("working_dir"));
        assert!(!obj.contains_key("model"));
        assert!(!obj.contains_key("profile"));
    }

    #[test]
    fn codex_config_old_json_without_idle_fields_uses_defaults() {
        // Configs persisted before `read_idle_timeout`/`shutdown_grace` existed
        // must still deserialize, picking up the new defaults rather than the
        // (short) probe/launch timeout.
        let mut legacy = serde_json::to_value(CodexConfig::new()).expect("serialize");
        let obj = legacy.as_object_mut().expect("config is an object");
        obj.remove("read_idle_timeout");
        obj.remove("shutdown_grace");

        let decoded: CodexConfig =
            serde_json::from_value(legacy).expect("deserialize legacy config");
        assert_eq!(decoded.read_idle_timeout(), DEFAULT_READ_IDLE_TIMEOUT);
        assert_eq!(decoded.shutdown_grace(), DEFAULT_SHUTDOWN_GRACE);
        assert_eq!(decoded.timeout(), DEFAULT_TIMEOUT);
    }

    #[test]
    fn codex_config_debug_redacts_env_secrets() {
        let secret = "sk-super-secret-key";
        let config = CodexConfig::new().with_env("OPENAI_API_KEY", secret);

        let rendered = format!("{config:?}");
        // The variable name is debuggable, but the value must never appear.
        assert!(rendered.contains("OPENAI_API_KEY"));
        assert!(rendered.contains("<redacted>"));
        assert!(
            !rendered.contains(secret),
            "debug output leaked the env secret: {rendered}"
        );
    }
}
