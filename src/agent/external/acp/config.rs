//! Launch configuration for the managed ACP (Agent Client Protocol) adapter.
//!
//! [`AcpConfig`] is the data-only recipe a host hands the ACP adapter so it can
//! later spawn an ACP **agent** subprocess and drive it as a JSON-RPC **client**
//! (the connection and handshake IO land in M10-2/M10-3). Unlike the three CLI
//! adapters — each of which pins one binary and maps a permission mode onto a
//! launch flag — a single ACP adapter drives *any* ACP agent, so the launch line
//! is fully general: an arbitrary `binary` plus `args`.
//!
//! # Configuration inheritance and injection
//!
//! Every in-tree ACP agent (`claude-agent-acp`, `codex-acp`, `opencode acp`)
//! reads credentials and settings from *its own* CLI's default config files and
//! stored login — `agent-lib` never carries a provider API key. To make a
//! machine that is already logged in "just work", the adapter **inherits the
//! host process environment by default** ([`inherit_env`](AcpConfig::inherit_env)
//! is `true`). To make local testing against *non-default* config possible (the
//! in-tree agents are typically driven with overridden config directories), the
//! config can **inject or replace** environment entries via
//! [`env`](AcpConfig::env) — for example `CODEX_HOME` / `CODEX_CONFIG` for Codex,
//! `OPENCODE_CONFIG` / `OPENCODE_CONFIG_DIR` / `XDG_CONFIG_HOME` for OpenCode, or
//! a `claude --settings <file>` flag threaded through [`args`](AcpConfig::args)
//! for Claude. Overrides are layered *on top of* the inherited environment, and
//! clearing inheritance ([`without_inherited_env`](AcpConfig::without_inherited_env))
//! leaves only the overrides, so a caller that opts out must re-supply essentials
//! like `HOME` itself.
//!
//! # Secret hygiene
//!
//! [`env`](AcpConfig::env) may carry credentials pointing at a login store. The
//! manual [`Debug`] and [`Display`](std::fmt::Display) impls therefore print only environment
//! variable *names* with a `<redacted>` placeholder, never their values, so a
//! config that lands in a log or panic message cannot leak a secret (design
//! constraint "任何可能包含 secret … 的日志/错误必须脱敏"). Serialization is a
//! separate, deliberate persistence path and does round-trip the values.
//! `AcpConfig` has **no** API-key field at all: credentials stay entirely on the
//! wrapped CLI side.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::external::ExternalPermissionMode;

/// The `claude-agent-acp` Node bridge binary (Zed's Claude Agent SDK adapter).
const CLAUDE_AGENT_ACP_BINARY: &str = "claude-agent-acp";

/// The `codex-acp` Node bridge binary (spawns a real `codex app-server`).
const CODEX_ACP_BINARY: &str = "codex-acp";

/// The OpenCode binary; its built-in ACP mode is the `acp` subcommand.
const OPENCODE_BINARY: &str = "opencode";

/// The OpenCode subcommand that speaks ACP over stdio.
const OPENCODE_ACP_SUBCOMMAND: &str = "acp";

/// The default connection/handshake timeout applied when a caller sets none.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Data-only launch configuration for the managed ACP adapter.
///
/// Build one with a preset constructor ([`claude_agent_acp`](Self::claude_agent_acp),
/// [`codex_acp`](Self::codex_acp), [`opencode_acp`](Self::opencode_acp)) or the
/// general [`new`](Self::new), then refine it with the chained `with_*` setters.
/// The config is plain, serializable data: it round-trips through serde so a host
/// can persist it, and it carries no live process, connection, or task handle —
/// those stay behind the adapter (added in M10-3).
///
/// # Fields
///
/// - [`binary`](Self::binary): the ACP agent executable to spawn.
/// - [`args`](Self::args): the launch arguments (for example `["acp"]` for
///   OpenCode's built-in ACP mode, or a `--settings` flag for Claude).
/// - [`env`](Self::env): environment overrides layered onto the child process,
///   redacted in [`Debug`]/[`Display`](std::fmt::Display). Never an API key — used to point a CLI at
///   a non-default config directory/file.
/// - [`inherit_env`](Self::inherit_env): whether the child inherits the host
///   process environment (default `true`); clearing it leaves only `env`.
/// - [`working_dir`](Self::working_dir): the directory (typically the agent's
///   worktree) the agent runs in; `None` inherits the parent's directory.
/// - [`permission_mode`](Self::permission_mode): the provider-neutral default
///   answer strategy for an ACP `session/request_permission` (the real answering
///   logic lands in M10-3).
/// - [`timeout`](Self::timeout): the wall-clock bound applied to connection and
///   handshake IO.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpConfig {
    binary: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    inherit_env: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_dir: Option<PathBuf>,
    permission_mode: ExternalPermissionMode,
    timeout: Duration,
}

impl AcpConfig {
    /// Creates a config for an arbitrary ACP agent launch line.
    ///
    /// Inherits the host environment, prompts on permission-gated actions, and
    /// uses the default timeout. Use the preset constructors for the three
    /// in-tree agents.
    #[must_use]
    pub fn new(
        binary: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            binary: binary.into(),
            args: args.into_iter().map(Into::into).collect(),
            env: BTreeMap::new(),
            inherit_env: true,
            working_dir: None,
            permission_mode: ExternalPermissionMode::Prompt,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Preset for Zed's `claude-agent-acp` bridge (no launch arguments).
    ///
    /// The bridge embeds `@anthropic-ai/claude-agent-sdk`, which reads **Claude
    /// Code's** `~/.claude` config and stored login automatically; `agent-lib`
    /// supplies no Anthropic API key. Point the bridge at non-default settings
    /// via a `claude --settings <file>` flag through [`with_arg`](Self::with_arg)
    /// or a config-directory env override.
    #[must_use]
    pub fn claude_agent_acp() -> Self {
        Self::new(CLAUDE_AGENT_ACP_BINARY, Vec::<String>::new())
    }

    /// Preset for Zed's `codex-acp` bridge (no launch arguments).
    ///
    /// The bridge spawns a real `codex app-server`, inheriting **Codex's**
    /// `~/.codex` config and `auth.json` login; `agent-lib` supplies no OpenAI API
    /// key. Point it at non-default config via `CODEX_HOME` / `CODEX_CONFIG` (and
    /// the codex binary via `CODEX_PATH`) through [`with_env`](Self::with_env).
    #[must_use]
    pub fn codex_acp() -> Self {
        Self::new(CODEX_ACP_BINARY, Vec::<String>::new())
    }

    /// Preset for OpenCode's built-in ACP mode (`opencode acp`).
    ///
    /// OpenCode speaks ACP itself — there is no separate bridge process — and
    /// reads its own `~/.config/opencode/opencode.json` and login. Point it at
    /// non-default config via `OPENCODE_CONFIG` / `OPENCODE_CONFIG_DIR` /
    /// `XDG_CONFIG_HOME` through [`with_env`](Self::with_env).
    #[must_use]
    pub fn opencode_acp() -> Self {
        Self::new(OPENCODE_BINARY, [OPENCODE_ACP_SUBCOMMAND])
    }

    /// Overrides the ACP agent binary path.
    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Appends one launch argument.
    #[must_use]
    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Replaces the full launch argument list.
    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Adds or replaces one environment override layered onto the child process.
    ///
    /// Overrides apply on top of the inherited environment (unless inheritance is
    /// cleared). Values may be secrets; they are redacted in [`Debug`]/[`Display`](std::fmt::Display)
    /// but preserved by serialization. This must never carry a provider API key —
    /// use it only to point a wrapped CLI at a specific config directory/file.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Clears host-environment inheritance so the child sees only [`env`](Self::env).
    ///
    /// A caller that opts out is responsible for re-supplying essentials the
    /// wrapped CLI needs to find its config/login (for example `HOME`), otherwise
    /// the bridge cannot locate credentials and will fail.
    #[must_use]
    pub const fn without_inherited_env(mut self) -> Self {
        self.inherit_env = false;
        self
    }

    /// Sets whether the child inherits the host process environment.
    #[must_use]
    pub const fn with_inherit_env(mut self, inherit: bool) -> Self {
        self.inherit_env = inherit;
        self
    }

    /// Sets the working directory (typically the agent's worktree) the agent runs
    /// in; `None` inherits the parent process's directory.
    #[must_use]
    pub fn with_working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }

    /// Sets the provider-neutral default answer strategy for an ACP
    /// `session/request_permission`.
    ///
    /// This does *not* map onto a launch flag (unlike the CLI adapters). It
    /// records how the adapter should answer a runtime permission request by
    /// default when it is not bridged to a host interaction: [`Plan`] refuses
    /// mutating actions, [`BypassPermissions`] auto-allows, and the others prompt.
    /// The actual answering logic lands in M10-3.
    ///
    /// [`Plan`]: ExternalPermissionMode::Plan
    /// [`BypassPermissions`]: ExternalPermissionMode::BypassPermissions
    #[must_use]
    pub const fn with_permission_mode(mut self, mode: ExternalPermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    /// Sets the connection/handshake timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Returns the configured ACP agent binary path.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Returns the launch arguments.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Returns the environment overrides layered onto the child process.
    #[must_use]
    pub const fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    /// Returns whether the child inherits the host process environment.
    #[must_use]
    pub const fn inherit_env(&self) -> bool {
        self.inherit_env
    }

    /// Returns the working directory the agent runs in, if one was set.
    #[must_use]
    pub fn working_dir(&self) -> Option<&Path> {
        self.working_dir.as_deref()
    }

    /// Returns the provider-neutral default permission-answer strategy.
    #[must_use]
    pub const fn permission_mode(&self) -> ExternalPermissionMode {
        self.permission_mode
    }

    /// Returns the connection/handshake timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Resolves the effective child-process environment from a parent environment.
    ///
    /// When [`inherit_env`](Self::inherit_env) is `true` the result starts from
    /// `parent_env` (production passes `std::env::vars()`; tests pass a synthetic
    /// map) and then layers [`env`](Self::env) on top, so an override replaces an
    /// inherited entry of the same key. When inheritance is off the result is just
    /// the overrides. This is a pure function so the spawn environment can be
    /// asserted without launching a process.
    #[must_use]
    pub fn resolved_env<I>(&self, parent_env: I) -> BTreeMap<String, String>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut resolved = BTreeMap::new();
        if self.inherit_env {
            resolved.extend(parent_env);
        }
        for (key, value) in &self.env {
            resolved.insert(key.clone(), value.clone());
        }
        resolved
    }
}

impl std::fmt::Debug for AcpConfig {
    /// Redacts environment values so a logged config cannot leak a secret.
    ///
    /// Every other field is stable, non-secret configuration and is shown as-is;
    /// [`env`](AcpConfig::env) is rendered as its keys mapped to a `"<redacted>"`
    /// placeholder so the *shape* is debuggable without exposing values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_env: BTreeMap<&String, &str> =
            self.env.keys().map(|key| (key, "<redacted>")).collect();
        f.debug_struct("AcpConfig")
            .field("binary", &self.binary)
            .field("args", &self.args)
            .field("env", &redacted_env)
            .field("inherit_env", &self.inherit_env)
            .field("working_dir", &self.working_dir)
            .field("permission_mode", &self.permission_mode)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl std::fmt::Display for AcpConfig {
    /// Renders a compact, secret-free one-line summary of the launch line.
    ///
    /// Only the binary, argument count, and the *names* of any env overrides are
    /// shown; override values are never printed, so a `Display`ed config cannot
    /// leak a credential-pointing value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AcpConfig(binary={:?}, args={}, inherit_env={}",
            self.binary,
            self.args.len(),
            self.inherit_env,
        )?;
        if !self.env.is_empty() {
            let keys: Vec<&str> = self.env.keys().map(String::as_str).collect();
            write!(f, ", env_overrides=[{}]", keys.join(", "))?;
        }
        write!(f, ")")
    }
}

/// Serde default for [`AcpConfig::inherit_env`].
const fn default_true() -> bool {
    true
}

/// `skip_serializing_if` predicate keeping the common `inherit_env = true` out of
/// the serialized form.
const fn is_true(value: &bool) -> bool {
    *value
}

#[cfg(test)]
mod tests {
    use super::{
        AcpConfig, CLAUDE_AGENT_ACP_BINARY, CODEX_ACP_BINARY, DEFAULT_TIMEOUT, OPENCODE_BINARY,
    };
    use crate::agent::external::ExternalPermissionMode;
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn acp_config_presets_carry_expected_launch_lines() {
        let claude = AcpConfig::claude_agent_acp();
        assert_eq!(claude.binary(), Path::new(CLAUDE_AGENT_ACP_BINARY));
        assert!(claude.args().is_empty());

        let codex = AcpConfig::codex_acp();
        assert_eq!(codex.binary(), Path::new(CODEX_ACP_BINARY));
        assert!(codex.args().is_empty());

        // OpenCode ships ACP as a subcommand, so the preset carries `acp` in args.
        let opencode = AcpConfig::opencode_acp();
        assert_eq!(opencode.binary(), Path::new(OPENCODE_BINARY));
        assert_eq!(opencode.args(), ["acp"]);

        // Every preset inherits the host env, prompts by default, and never
        // carries a key: the config type has no API-key concept, only `env`.
        for config in [&claude, &codex, &opencode] {
            assert!(config.inherit_env());
            assert_eq!(config.permission_mode(), ExternalPermissionMode::Prompt);
            assert!(config.env().is_empty());
            assert_eq!(config.timeout(), DEFAULT_TIMEOUT);
        }
    }

    #[test]
    fn acp_config_roundtrip() {
        let config = AcpConfig::codex_acp()
            .with_working_dir("/tmp/worktree")
            .with_permission_mode(ExternalPermissionMode::Plan)
            .with_env("CODEX_HOME", "/tmp/test-codex")
            .with_timeout(Duration::from_secs(90));

        let encoded = serde_json::to_string(&config).expect("serialize config");
        let decoded: AcpConfig = serde_json::from_str(&encoded).expect("deserialize config");
        assert_eq!(decoded, config);

        // The common `inherit_env = true` and empty collections are skipped, and
        // there is no API-key field to serialize.
        let default_value = serde_json::to_value(AcpConfig::opencode_acp()).expect("serialize");
        let obj = default_value.as_object().expect("config is an object");
        assert!(!obj.contains_key("inherit_env"));
        assert!(!obj.contains_key("env"));
        assert!(!obj.contains_key("working_dir"));

        // An opted-out, override-carrying config round-trips including the flag.
        let cleared = AcpConfig::opencode_acp()
            .without_inherited_env()
            .with_env("HOME", "/tmp/home")
            .with_env("OPENCODE_CONFIG_DIR", "/tmp/oc");
        let encoded = serde_json::to_string(&cleared).expect("serialize cleared");
        let decoded: AcpConfig = serde_json::from_str(&encoded).expect("deserialize cleared");
        assert_eq!(decoded, cleared);
        assert!(!decoded.inherit_env());
    }

    #[test]
    fn acp_config_debug_and_display_redact_env_secrets() {
        let secret = "sk-super-secret-login-pointer";
        let config = AcpConfig::codex_acp().with_env("CODEX_CONFIG", secret);

        let debug = format!("{config:?}");
        assert!(debug.contains("CODEX_CONFIG"));
        assert!(debug.contains("<redacted>"));
        assert!(
            !debug.contains(secret),
            "debug output leaked the env secret: {debug}"
        );

        let display = format!("{config}");
        assert!(display.contains("CODEX_CONFIG"));
        assert!(
            !display.contains(secret),
            "display output leaked the env secret: {display}"
        );
    }

    #[test]
    fn acp_config_resolved_env_inherits_by_default_and_injects_overrides() {
        let parent = || {
            [
                ("HOME".to_owned(), "/home/agent".to_owned()),
                ("CODEX_HOME".to_owned(), "/home/agent/.codex".to_owned()),
            ]
        };

        // Default inheritance keeps the parent entries and layers overrides on
        // top, so an injected key wins and a new key appears.
        let injected = AcpConfig::codex_acp().with_env("CODEX_HOME", "/tmp/test-codex");
        let env = injected.resolved_env(parent());
        assert_eq!(env.get("HOME").map(String::as_str), Some("/home/agent"));
        assert_eq!(
            env.get("CODEX_HOME").map(String::as_str),
            Some("/tmp/test-codex"),
            "override must replace the inherited value"
        );

        // Opting out of inheritance drops parent entries, leaving only overrides.
        let cleared = AcpConfig::opencode_acp()
            .without_inherited_env()
            .with_env("OPENCODE_CONFIG_DIR", "/tmp/oc");
        let env = cleared.resolved_env(parent());
        assert!(
            !env.contains_key("HOME"),
            "parent env must not pass through"
        );
        assert_eq!(
            env.get("OPENCODE_CONFIG_DIR").map(String::as_str),
            Some("/tmp/oc")
        );
        assert_eq!(env.len(), 1);
    }
}
