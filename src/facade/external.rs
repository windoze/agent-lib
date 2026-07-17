//! Managed external agent construction surface for the
//! [`Agent`](crate::facade::Agent) facade (`docs/facade-api.md` §11).
//!
//! A *managed external agent* is a supervised session of an external
//! coding-agent runtime (Claude Code, Codex, OpenCode, or any ACP agent). It is
//! **not** an ordinary [`Tool`](crate::facade::Tool): it owns a worktree, streams
//! events, may raise permission requests, and reports artifacts and usage, so the
//! facade models it as an *external delegate* rather than a flat function call
//! (§11.1). This milestone (M4-1) lands the **construction and capability-grading**
//! slice only: the data-first [`ManagedExternalAgent`] spec, its
//! [`ManagedExternalAgentBuilder`] with per-runtime presets, the
//! [`ExternalRunMode`] capability grade, and the [`ExternalAgentCapabilities`]
//! facade view. Wiring a built spec into a running delegate (`NeedSubagent` →
//! [`ExternalAgentMachine`](crate::agent::ExternalAgentMachine)) and the external
//! approval/restore policy land in later milestones (M4-2, M4-3).
//!
//! # Capability grading is negotiated, not assumed (§11.3)
//!
//! Each managed runtime advertises a set of managed features
//! ([`ExternalRuntimeCapabilities`]): streaming, resume, permission bridging,
//! host-tool injection, artifacts, usage, graceful shutdown, and live
//! reconfiguration. A caller asks for a *grade* of managed behavior via
//! [`ExternalRunMode`], and [`ManagedExternalAgentBuilder::build`] checks that the
//! target runtime can actually fulfill it. If it cannot, construction **fails
//! fast** with [`FacadeError::UnsupportedExternalMode`] rather than silently
//! pretending the feature exists.
//!
//! The baseline capabilities attached by the preset constructors mirror each
//! adapter's *declared* capabilities and are a conservative starting point. A
//! real run refines them from a capability probe (CLI adapters) or an
//! `initialize` handshake (ACP): the facade never hard-codes an unverified grade.
//! For ACP the negotiated result is folded in with the feature-gated
//! `ExternalAgentCapabilities::from_acp_negotiation` /
//! `ManagedExternalAgentBuilder::acp_negotiated` (feature `external-acp`).
//!
//! ```
//! # fn demo() -> Result<(), agent_lib::facade::FacadeError> {
//! use agent_lib::facade::{ExternalRunMode, ManagedExternalAgent};
//!
//! // Codex runs as a managed session with streaming events (no host-tool bridge).
//! let codex = ManagedExternalAgent::codex()
//!     .worktree("/home/me/repos/app")
//!     .mode(ExternalRunMode::Managed)
//!     .build()?;
//! assert_eq!(codex.mode(), ExternalRunMode::Managed);
//!
//! // Asking a runtime for a grade it cannot serve fails fast rather than
//! // degrading silently: no current runtime injects host tools.
//! let err = ManagedExternalAgent::codex()
//!     .mode(ExternalRunMode::ManagedWithTools)
//!     .build()
//!     .unwrap_err();
//! assert!(matches!(
//!     err,
//!     agent_lib::facade::FacadeError::UnsupportedExternalMode { .. }
//! ));
//! # Ok(())
//! # }
//! # demo().unwrap();
//! ```

use std::path::{Path, PathBuf};

use crate::agent::{
    ExternalCapability, ExternalPermissionMode, ExternalRuntimeCapabilities, ExternalRuntimeKind,
    WorktreeRef,
};
use crate::facade::error::FacadeError;

#[cfg(feature = "external-acp")]
use crate::agent::external::{
    AcpConfig, AcpNegotiatedCapabilities, acp_runtime_kind, capabilities_from_initialize,
};

/// The grade of managed behavior a caller asks a runtime to provide (§11.3).
///
/// Each grade names a set of managed features the runtime must support, checked
/// at [`build`](ManagedExternalAgentBuilder::build) time against the runtime's
/// [`ExternalAgentCapabilities`]. The grades are *not* a strict linear order:
/// [`ManagedWithTools`](Self::ManagedWithTools) and [`Attachable`](Self::Attachable)
/// each extend [`Managed`](Self::Managed) along a different axis (host tooling vs.
/// session resume).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalRunMode {
    /// Black-box execution: run the task and return only a final summary. Needs
    /// no managed feature, so every runtime supports it.
    BlackBox,
    /// A managed session with fine-grained streaming events (and permission
    /// requests where the runtime bridges them). Requires
    /// [`Streaming`](ExternalCapability::Streaming).
    Managed,
    /// A managed session that can additionally inject host-provided tools the
    /// runtime calls back into. Requires
    /// [`Streaming`](ExternalCapability::Streaming) and
    /// [`HostTools`](ExternalCapability::HostTools).
    ManagedWithTools,
    /// A long-lived session that can be attached to and resumed across runs.
    /// Requires [`Streaming`](ExternalCapability::Streaming) and
    /// [`Resume`](ExternalCapability::Resume).
    Attachable,
}

impl ExternalRunMode {
    /// Every run mode, in ascending grade order, for exhaustive iteration.
    pub const ALL: [ExternalRunMode; 4] = [
        ExternalRunMode::BlackBox,
        ExternalRunMode::Managed,
        ExternalRunMode::ManagedWithTools,
        ExternalRunMode::Attachable,
    ];

    /// Returns the stable, human-readable label for this mode.
    ///
    /// The label matches the serde representation and carries no runtime input,
    /// so it is safe to embed in diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ExternalRunMode::BlackBox => "black_box",
            ExternalRunMode::Managed => "managed",
            ExternalRunMode::ManagedWithTools => "managed_with_tools",
            ExternalRunMode::Attachable => "attachable",
        }
    }

    /// Returns the managed capabilities a runtime must support to serve this mode.
    #[must_use]
    pub const fn required_capabilities(self) -> &'static [ExternalCapability] {
        match self {
            ExternalRunMode::BlackBox => &[],
            ExternalRunMode::Managed => &[ExternalCapability::Streaming],
            ExternalRunMode::ManagedWithTools => {
                &[ExternalCapability::Streaming, ExternalCapability::HostTools]
            }
            ExternalRunMode::Attachable => {
                &[ExternalCapability::Streaming, ExternalCapability::Resume]
            }
        }
    }
}

impl std::fmt::Display for ExternalRunMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The facade view of what managed features a runtime session can fulfill.
///
/// This wraps the lower-layer [`ExternalRuntimeCapabilities`] and adds the
/// facade's [`ExternalRunMode`] grading queries. A value starts from the
/// conservative baseline the preset constructors attach (mirroring each adapter's
/// declared capabilities) and is refined by a probe or an ACP `initialize`
/// negotiation before or during a real run — the facade never claims a feature it
/// has not verified (§11.3, design §15).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentCapabilities {
    inner: ExternalRuntimeCapabilities,
}

impl ExternalAgentCapabilities {
    /// Wraps a lower-layer capability set as the facade view.
    #[must_use]
    pub fn from_runtime_capabilities(inner: ExternalRuntimeCapabilities) -> Self {
        Self { inner }
    }

    /// Builds the facade view from a negotiated ACP `initialize` handshake.
    ///
    /// The mapping is the pure [`capabilities_from_initialize`] projection: the
    /// three protocol-guaranteed bits (streaming, permission bridge, graceful
    /// shutdown) plus `resume` iff the agent advertised `session/load`.
    #[cfg(feature = "external-acp")]
    #[must_use]
    pub fn from_acp_negotiation(negotiated: &AcpNegotiatedCapabilities) -> Self {
        Self::from_runtime_capabilities(capabilities_from_initialize(negotiated))
    }

    /// Returns the runtime these capabilities describe.
    #[must_use]
    pub const fn runtime(&self) -> &ExternalRuntimeKind {
        &self.inner.runtime
    }

    /// Reports whether `capability` is supported.
    #[must_use]
    pub fn supports(&self, capability: ExternalCapability) -> bool {
        self.inner.supports(capability)
    }

    /// Reports whether every capability `mode` requires is supported.
    #[must_use]
    pub fn supports_mode(&self, mode: ExternalRunMode) -> bool {
        mode.required_capabilities()
            .iter()
            .all(|capability| self.inner.supports(*capability))
    }

    /// Returns the capabilities `mode` requires but this runtime does not support.
    ///
    /// An empty result means the mode is fully supported.
    #[must_use]
    pub fn missing_for_mode(&self, mode: ExternalRunMode) -> Vec<ExternalCapability> {
        mode.required_capabilities()
            .iter()
            .copied()
            .filter(|capability| !self.inner.supports(*capability))
            .collect()
    }

    /// Returns every [`ExternalRunMode`] this runtime can fully serve.
    ///
    /// Useful for a caller that wants to degrade to a supported grade instead of
    /// failing fast (§11.3).
    #[must_use]
    pub fn supported_modes(&self) -> Vec<ExternalRunMode> {
        ExternalRunMode::ALL
            .into_iter()
            .filter(|mode| self.supports_mode(*mode))
            .collect()
    }

    /// Returns the wrapped lower-layer capability set.
    #[must_use]
    pub const fn as_runtime_capabilities(&self) -> &ExternalRuntimeCapabilities {
        &self.inner
    }

    /// Consumes the view, returning the wrapped lower-layer capability set.
    #[must_use]
    pub fn into_runtime_capabilities(self) -> ExternalRuntimeCapabilities {
        self.inner
    }
}

/// A data-first specification of a managed external coding-agent runtime (§11).
///
/// Like [`LocalSubagent`](crate::facade::LocalSubagent), this is a *recipe*, not a
/// live session: it carries the runtime kind, the requested [`ExternalRunMode`],
/// the validated [`ExternalAgentCapabilities`], and the launch data (worktree,
/// binary, model, args, permission mode). It holds **no** process handle, client,
/// or credential — those are built only when a delegation is fulfilled (M4-2), so
/// the spec stays cheap to clone, inspect, and (later) snapshot.
///
/// Build one with a preset constructor:
/// [`claude_code`](Self::claude_code), [`codex`](Self::codex),
/// [`opencode`](Self::opencode), or — under the `external-acp` feature — the ACP
/// presets `acp`, `claude_agent_acp`, `codex_acp`, `opencode_acp`, and
/// `gemini_acp`.
#[derive(Clone, Debug)]
pub struct ManagedExternalAgent {
    runtime: ExternalRuntimeKind,
    mode: ExternalRunMode,
    capabilities: ExternalAgentCapabilities,
    worktree: Option<WorktreeRef>,
    binary: Option<PathBuf>,
    model: Option<String>,
    args: Vec<String>,
    permission_mode: ExternalPermissionMode,
}

impl ManagedExternalAgent {
    /// Starts a builder for the managed **Claude Code** CLI runtime.
    ///
    /// The declared baseline supports streaming, resume, a permission bridge,
    /// artifacts, usage, and graceful shutdown; host-tool/subagent injection is
    /// off. Refined by a capability probe at run time.
    #[must_use]
    pub fn claude_code() -> ManagedExternalAgentBuilder {
        ManagedExternalAgentBuilder::for_runtime(ExternalRuntimeKind::ClaudeCode)
    }

    /// Starts a builder for the managed **Codex** CLI runtime.
    ///
    /// The declared baseline supports streaming, resume, artifacts, usage, and
    /// graceful shutdown; it has **no** permission bridge and no host-tool
    /// injection. Refined by a capability probe at run time.
    #[must_use]
    pub fn codex() -> ManagedExternalAgentBuilder {
        ManagedExternalAgentBuilder::for_runtime(ExternalRuntimeKind::Codex)
    }

    /// Starts a builder for the managed **OpenCode** CLI runtime.
    ///
    /// The declared baseline matches Codex: streaming, resume, artifacts, usage,
    /// and graceful shutdown, with **no** permission bridge or host-tool
    /// injection. Refined by a capability probe at run time.
    #[must_use]
    pub fn opencode() -> ManagedExternalAgentBuilder {
        ManagedExternalAgentBuilder::for_runtime(ExternalRuntimeKind::OpenCode)
    }

    /// Starts a builder for a managed **ACP** agent launched as `binary args…`
    /// (feature `external-acp`).
    ///
    /// ACP is a single standard protocol, so one adapter drives any ACP agent;
    /// the launch line selects it. The declared baseline reflects the
    /// protocol-guaranteed bits (streaming, permission bridge, graceful
    /// shutdown); `resume` turns on only once an `initialize` handshake advertises
    /// `session/load` — fold it in with
    /// [`acp_negotiated`](ManagedExternalAgentBuilder::acp_negotiated).
    #[cfg(feature = "external-acp")]
    #[must_use]
    pub fn acp(
        binary: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> ManagedExternalAgentBuilder {
        ManagedExternalAgentBuilder::from_acp_config(AcpConfig::new(binary, args))
    }

    /// ACP preset for Zed's `claude-agent-acp` bridge (feature `external-acp`).
    #[cfg(feature = "external-acp")]
    #[must_use]
    pub fn claude_agent_acp() -> ManagedExternalAgentBuilder {
        ManagedExternalAgentBuilder::from_acp_config(AcpConfig::claude_agent_acp())
    }

    /// ACP preset for Zed's `codex-acp` bridge (feature `external-acp`).
    #[cfg(feature = "external-acp")]
    #[must_use]
    pub fn codex_acp() -> ManagedExternalAgentBuilder {
        ManagedExternalAgentBuilder::from_acp_config(AcpConfig::codex_acp())
    }

    /// ACP preset for OpenCode's built-in `opencode acp` mode (feature
    /// `external-acp`).
    #[cfg(feature = "external-acp")]
    #[must_use]
    pub fn opencode_acp() -> ManagedExternalAgentBuilder {
        ManagedExternalAgentBuilder::from_acp_config(AcpConfig::opencode_acp())
    }

    /// ACP preset for Gemini CLI's experimental ACP mode (`gemini
    /// --experimental-acp`, feature `external-acp`).
    #[cfg(feature = "external-acp")]
    #[must_use]
    pub fn gemini_acp() -> ManagedExternalAgentBuilder {
        ManagedExternalAgentBuilder::from_acp_config(AcpConfig::new(
            "gemini",
            ["--experimental-acp"],
        ))
    }

    /// Returns the runtime this managed agent drives.
    #[must_use]
    pub const fn runtime(&self) -> &ExternalRuntimeKind {
        &self.runtime
    }

    /// Returns the validated run mode.
    #[must_use]
    pub const fn mode(&self) -> ExternalRunMode {
        self.mode
    }

    /// Returns the capabilities the mode was validated against.
    #[must_use]
    pub const fn capabilities(&self) -> &ExternalAgentCapabilities {
        &self.capabilities
    }

    /// Returns the worktree the runtime is confined to, if one was set.
    #[must_use]
    pub fn worktree(&self) -> Option<&WorktreeRef> {
        self.worktree.as_ref()
    }

    /// Returns the runtime binary override, if one was set.
    #[must_use]
    pub fn binary(&self) -> Option<&Path> {
        self.binary.as_deref()
    }

    /// Returns the pinned model, if one was set.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Returns the extra launch arguments.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Returns the permission mode applied to gated actions.
    #[must_use]
    pub const fn permission_mode(&self) -> ExternalPermissionMode {
        self.permission_mode
    }
}

/// Builder for a [`ManagedExternalAgent`], reached through a preset constructor.
///
/// The mode defaults to [`ExternalRunMode::Managed`] and the permission mode to
/// [`ExternalPermissionMode::Prompt`] (the safest). [`build`](Self::build)
/// validates the requested mode against the runtime's capabilities.
#[derive(Clone, Debug)]
pub struct ManagedExternalAgentBuilder {
    runtime: ExternalRuntimeKind,
    mode: ExternalRunMode,
    capabilities: ExternalAgentCapabilities,
    worktree: Option<WorktreeRef>,
    binary: Option<PathBuf>,
    model: Option<String>,
    args: Vec<String>,
    permission_mode: ExternalPermissionMode,
}

impl ManagedExternalAgentBuilder {
    /// Builds a builder for a named CLI runtime with its declared baseline
    /// capabilities.
    fn for_runtime(runtime: ExternalRuntimeKind) -> Self {
        let capabilities =
            ExternalAgentCapabilities::from_runtime_capabilities(declared_capabilities(&runtime));
        Self {
            runtime,
            mode: ExternalRunMode::Managed,
            capabilities,
            worktree: None,
            binary: None,
            model: None,
            args: Vec::new(),
            permission_mode: ExternalPermissionMode::Prompt,
        }
    }

    /// Builds a builder from an ACP launch config, seeding launch data and the
    /// protocol-guaranteed (pre-negotiation) capability baseline.
    #[cfg(feature = "external-acp")]
    fn from_acp_config(config: AcpConfig) -> Self {
        let capabilities =
            ExternalAgentCapabilities::from_acp_negotiation(&AcpNegotiatedCapabilities::none());
        Self {
            runtime: acp_runtime_kind(),
            mode: ExternalRunMode::Managed,
            capabilities,
            worktree: config
                .working_dir()
                .map(|dir| WorktreeRef::new(dir.to_path_buf())),
            binary: Some(config.binary().to_path_buf()),
            model: None,
            args: config.args().to_vec(),
            permission_mode: config.permission_mode(),
        }
    }

    /// Sets the requested run mode (default [`ExternalRunMode::Managed`]).
    #[must_use]
    pub const fn mode(mut self, mode: ExternalRunMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the worktree the runtime is confined to.
    #[must_use]
    pub fn worktree(mut self, worktree: impl Into<PathBuf>) -> Self {
        self.worktree = Some(WorktreeRef::new(worktree));
        self
    }

    /// Overrides the runtime binary path.
    #[must_use]
    pub fn binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = Some(binary.into());
        self
    }

    /// Pins a (usually cheaper) model for the runtime.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Appends one launch argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Replaces the full launch argument list.
    #[must_use]
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the permission mode for gated actions (default
    /// [`ExternalPermissionMode::Prompt`]).
    #[must_use]
    pub const fn permission_mode(mut self, permission_mode: ExternalPermissionMode) -> Self {
        self.permission_mode = permission_mode;
        self
    }

    /// Replaces the capability set the mode is validated against.
    ///
    /// Use this to fold in the result of a real capability probe so the grade is
    /// checked against verified capabilities rather than the declared baseline
    /// (§11.3). For ACP the ergonomic path is `acp_negotiated` (feature
    /// `external-acp`).
    #[must_use]
    pub fn capabilities(mut self, capabilities: ExternalAgentCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Folds a negotiated ACP `initialize` handshake into the capability set
    /// (feature `external-acp`).
    ///
    /// This replaces the pre-negotiation baseline so grades like
    /// [`Attachable`](ExternalRunMode::Attachable) become available once the agent
    /// advertises `session/load`.
    #[cfg(feature = "external-acp")]
    #[must_use]
    pub fn acp_negotiated(mut self, negotiated: &AcpNegotiatedCapabilities) -> Self {
        self.capabilities = ExternalAgentCapabilities::from_acp_negotiation(negotiated);
        self
    }

    /// Validates the requested mode against the runtime's capabilities and
    /// produces the [`ManagedExternalAgent`] spec.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::UnsupportedExternalMode`] when the runtime does not
    /// support every capability the requested [`ExternalRunMode`] needs. The error
    /// names the runtime, the mode, and the missing capabilities, so a host can
    /// pick a supported grade (see
    /// [`ExternalAgentCapabilities::supported_modes`]) or a different runtime
    /// instead of degrading silently.
    pub fn build(self) -> Result<ManagedExternalAgent, FacadeError> {
        if !self.capabilities.supports_mode(self.mode) {
            let missing = self
                .capabilities
                .missing_for_mode(self.mode)
                .into_iter()
                .map(|capability| capability.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(FacadeError::UnsupportedExternalMode {
                runtime: runtime_label(&self.runtime),
                mode: self.mode.as_str(),
                missing,
            });
        }

        Ok(ManagedExternalAgent {
            runtime: self.runtime,
            mode: self.mode,
            capabilities: self.capabilities,
            worktree: self.worktree,
            binary: self.binary,
            model: self.model,
            args: self.args,
            permission_mode: self.permission_mode,
        })
    }
}

/// Returns the stable, non-secret label for a runtime kind.
///
/// The label matches the [`ExternalRuntimeKind`] serde representation for the
/// named runtimes and carries the free-form identifier verbatim for
/// [`Custom`](ExternalRuntimeKind::Custom). It contains no runtime output, so it
/// is safe to embed in error messages and logs.
fn runtime_label(runtime: &ExternalRuntimeKind) -> String {
    match runtime {
        ExternalRuntimeKind::ClaudeCode => "claude_code".to_owned(),
        ExternalRuntimeKind::Codex => "codex".to_owned(),
        ExternalRuntimeKind::OpenCode => "opencode".to_owned(),
        ExternalRuntimeKind::Custom(label) => label.clone(),
    }
}

/// Returns the declared baseline capabilities for a named CLI runtime.
///
/// These mirror each adapter's `implemented_capabilities()` and are the
/// conservative starting point the presets attach before a probe refines them at
/// run time (§11.3); they are never presented as verified truth. Claude Code
/// declares a permission bridge; Codex and OpenCode do not. Unknown
/// [`Custom`](ExternalRuntimeKind::Custom) runtimes assume nothing beyond the
/// all-unsupported baseline (ACP presets seed their own baseline instead).
fn declared_capabilities(runtime: &ExternalRuntimeKind) -> ExternalRuntimeCapabilities {
    let mut capabilities = ExternalRuntimeCapabilities::none(runtime.clone());
    match runtime {
        ExternalRuntimeKind::ClaudeCode => {
            capabilities.streaming = true;
            capabilities.resume = true;
            capabilities.permission_bridge = true;
            capabilities.artifacts = true;
            capabilities.usage = true;
            capabilities.graceful_shutdown = true;
        }
        ExternalRuntimeKind::Codex | ExternalRuntimeKind::OpenCode => {
            capabilities.streaming = true;
            capabilities.resume = true;
            capabilities.artifacts = true;
            capabilities.usage = true;
            capabilities.graceful_shutdown = true;
        }
        ExternalRuntimeKind::Custom(_) => {}
    }
    capabilities
}

#[cfg(test)]
mod tests {
    use super::{
        ExternalAgentCapabilities, ExternalRunMode, ManagedExternalAgent, declared_capabilities,
    };
    use crate::agent::{ExternalCapability, ExternalPermissionMode, ExternalRuntimeKind};
    use crate::facade::error::FacadeError;

    #[test]
    fn run_mode_labels_match_serde() {
        for mode in ExternalRunMode::ALL {
            let json = serde_json::to_value(mode).expect("serialize mode");
            assert_eq!(json, serde_json::Value::String(mode.as_str().to_owned()));
            assert_eq!(mode.to_string(), mode.as_str());
        }
    }

    #[test]
    fn cli_presets_carry_expected_runtime_and_defaults() {
        let claude = ManagedExternalAgent::claude_code()
            .build()
            .expect("build claude");
        assert_eq!(claude.runtime(), &ExternalRuntimeKind::ClaudeCode);
        assert_eq!(claude.mode(), ExternalRunMode::Managed);
        assert_eq!(claude.permission_mode(), ExternalPermissionMode::Prompt);
        assert!(claude.worktree().is_none());
        assert!(claude.binary().is_none());
        // Claude Code declares a permission bridge; Codex/OpenCode do not.
        assert!(
            claude
                .capabilities()
                .supports(ExternalCapability::PermissionBridge)
        );

        let codex = ManagedExternalAgent::codex().build().expect("build codex");
        assert_eq!(codex.runtime(), &ExternalRuntimeKind::Codex);
        assert!(
            !codex
                .capabilities()
                .supports(ExternalCapability::PermissionBridge)
        );

        let opencode = ManagedExternalAgent::opencode()
            .build()
            .expect("build opencode");
        assert_eq!(opencode.runtime(), &ExternalRuntimeKind::OpenCode);
        assert!(
            !opencode
                .capabilities()
                .supports(ExternalCapability::PermissionBridge)
        );
    }

    #[test]
    fn builder_records_launch_data() {
        let codex = ManagedExternalAgent::codex()
            .worktree("/tmp/repo")
            .model("gpt-5-mini")
            .arg("--foo")
            .permission_mode(ExternalPermissionMode::AcceptEdits)
            .mode(ExternalRunMode::Attachable)
            .build()
            .expect("build codex");

        assert_eq!(
            codex.worktree().map(|w| w.path().to_path_buf()),
            Some("/tmp/repo".into())
        );
        assert_eq!(codex.model(), Some("gpt-5-mini"));
        assert_eq!(codex.args(), ["--foo"]);
        assert_eq!(codex.permission_mode(), ExternalPermissionMode::AcceptEdits);
        // Attachable needs streaming + resume, both declared by Codex.
        assert_eq!(codex.mode(), ExternalRunMode::Attachable);
    }

    #[test]
    fn args_replaces_full_list() {
        let codex = ManagedExternalAgent::codex()
            .arg("--first")
            .args(["--a", "--b"])
            .build()
            .expect("build codex");
        assert_eq!(codex.args(), ["--a", "--b"]);
    }

    #[test]
    fn unsupported_mode_fails_fast_with_missing_capabilities() {
        // No current runtime injects host tools, so ManagedWithTools fails fast.
        let error = ManagedExternalAgent::codex()
            .mode(ExternalRunMode::ManagedWithTools)
            .build()
            .expect_err("host-tool grade must be rejected");

        match error {
            FacadeError::UnsupportedExternalMode {
                runtime,
                mode,
                missing,
            } => {
                assert_eq!(runtime, "codex");
                assert_eq!(mode, "managed_with_tools");
                assert_eq!(missing, "host_tools");
            }
            other => panic!("expected UnsupportedExternalMode, got {other:?}"),
        }
    }

    #[test]
    fn black_box_is_always_supported() {
        // Even a bare custom runtime with no declared capabilities serves BlackBox.
        let caps = ExternalAgentCapabilities::from_runtime_capabilities(declared_capabilities(
            &ExternalRuntimeKind::Custom("bespoke".to_owned()),
        ));
        assert!(caps.supports_mode(ExternalRunMode::BlackBox));
        assert!(!caps.supports_mode(ExternalRunMode::Managed));
        assert_eq!(caps.supported_modes(), vec![ExternalRunMode::BlackBox]);
    }

    #[test]
    fn supported_modes_reflect_declared_capabilities() {
        // Codex: streaming + resume but no host tools → BlackBox/Managed/Attachable.
        let codex = ManagedExternalAgent::codex().build().expect("build codex");
        assert_eq!(
            codex.capabilities().supported_modes(),
            vec![
                ExternalRunMode::BlackBox,
                ExternalRunMode::Managed,
                ExternalRunMode::Attachable,
            ]
        );
        assert!(
            !codex
                .capabilities()
                .supports_mode(ExternalRunMode::ManagedWithTools)
        );
        assert_eq!(
            codex
                .capabilities()
                .missing_for_mode(ExternalRunMode::ManagedWithTools),
            vec![ExternalCapability::HostTools]
        );
    }

    #[test]
    fn capabilities_view_roundtrips_and_exposes_inner() {
        let codex = ManagedExternalAgent::codex().build().expect("build codex");
        let caps = codex.capabilities();
        assert_eq!(caps.runtime(), &ExternalRuntimeKind::Codex);
        assert_eq!(
            caps.as_runtime_capabilities().runtime,
            ExternalRuntimeKind::Codex
        );
        let encoded = serde_json::to_value(caps).expect("serialize caps");
        let decoded: ExternalAgentCapabilities =
            serde_json::from_value(encoded).expect("deserialize caps");
        assert_eq!(&decoded, caps);
    }

    #[cfg(feature = "external-acp")]
    #[test]
    fn acp_presets_map_negotiated_capabilities() {
        use crate::agent::external::{AcpNegotiatedCapabilities, acp_runtime_kind};

        // Pre-negotiation baseline: streaming + permission bridge + graceful
        // shutdown, but resume is off, so Attachable is not yet available.
        let base = ManagedExternalAgent::opencode_acp()
            .build()
            .expect("build acp");
        assert_eq!(base.runtime(), &acp_runtime_kind());
        assert!(
            base.capabilities()
                .supports(ExternalCapability::PermissionBridge)
        );
        assert!(!base.capabilities().supports(ExternalCapability::Resume));
        assert!(
            !base
                .capabilities()
                .supports_mode(ExternalRunMode::Attachable)
        );

        // Attachable fails fast before load_session is negotiated.
        let error = ManagedExternalAgent::opencode_acp()
            .mode(ExternalRunMode::Attachable)
            .build()
            .expect_err("resume must be negotiated first");
        assert!(matches!(error, FacadeError::UnsupportedExternalMode { .. }));

        // Folding in a handshake that advertised session/load enables resume and
        // therefore the Attachable grade.
        let negotiated = AcpNegotiatedCapabilities::none().with_load_session(true);
        let attachable = ManagedExternalAgent::opencode_acp()
            .acp_negotiated(&negotiated)
            .mode(ExternalRunMode::Attachable)
            .build()
            .expect("resume available after negotiation");
        assert!(
            attachable
                .capabilities()
                .supports(ExternalCapability::Resume)
        );
        assert_eq!(attachable.mode(), ExternalRunMode::Attachable);
    }

    #[cfg(feature = "external-acp")]
    #[test]
    fn acp_arbitrary_launch_line_records_binary_and_args() {
        let agent = ManagedExternalAgent::acp("gemini", ["--experimental-acp"])
            .build()
            .expect("build acp");
        assert_eq!(
            agent.binary().map(std::path::Path::to_path_buf),
            Some("gemini".into())
        );
        assert_eq!(agent.args(), ["--experimental-acp"]);
    }
}
