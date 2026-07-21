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
//! [`ExternalAgentMachine`](crate::agent::external::ExternalAgentMachine)) and the external
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
use std::sync::Arc;
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode",
    feature = "external-acp"
))]
use std::time::Duration;

use crate::agent::external::ExternalSessionRegistry;
pub use crate::agent::external::RegistryExternalSessionHandler;
use crate::agent::{
    ExternalCapability, ExternalPermissionMode, ExternalRuntimeCapabilities, ExternalRuntimeKind,
    ExternalSessionHandler, WorktreeRef,
};
use crate::facade::error::FacadeError;

#[cfg(feature = "external-acp")]
use crate::agent::external::{
    ACP_RUNTIME_LABEL, AcpConfig, AcpNegotiatedCapabilities, acp_runtime_kind,
    capabilities_from_initialize,
};

mod delegate;

pub(crate) use delegate::drive_external;
pub use delegate::{
    ExternalDelegateStatus, ExternalDriveOutcome, ManagedExternalDelegate, RestoreExternal,
    RetainedExternalSession, run_external_once,
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

/// Where a [`ExternalAgentCapabilities`] view came from — its *provenance*.
///
/// A capability grade is only as trustworthy as its source. A static
/// [`Declared`](Self::Declared) baseline is a conservative guess; a
/// [`Probed`](Self::Probed) or [`Negotiated`](Self::Negotiated) view reflects what
/// a real runtime actually advertised. Carrying the source lets a caller (and the
/// build-time capability check) tell "the adapter *claims* this" apart from "we
/// *verified* this", instead of treating every grade as equally authoritative
/// (§11.3, design §15).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    /// The adapter's or preset's static declaration — a conservative starting
    /// point, not verified against a live runtime. This is the default a preset
    /// constructor attaches before a probe or negotiation refines it.
    #[default]
    Declared,
    /// Supplied by the caller through
    /// [`ManagedExternalAgentBuilder::capabilities`] (for example after folding
    /// in an out-of-band probe of their own).
    Supplied,
    /// Obtained by probing the live CLI runtime (or its registry-backed handler),
    /// so it reflects what the local binary actually reports.
    Probed,
    /// Obtained through an ACP `initialize` negotiation with the running agent.
    Negotiated,
}

impl CapabilitySource {
    /// Returns the stable, non-secret label for the source.
    ///
    /// The label matches the serde representation and is safe to embed in error
    /// messages and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Supplied => "supplied",
            Self::Probed => "probed",
            Self::Negotiated => "negotiated",
        }
    }
}

impl std::fmt::Display for CapabilitySource {
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
///
/// Every view also records its [`CapabilitySource`] (accessible with
/// [`source`](Self::source)), so a caller can tell a static
/// [`Declared`](CapabilitySource::Declared) baseline apart from a
/// [`Probed`](CapabilitySource::Probed) or
/// [`Negotiated`](CapabilitySource::Negotiated) grade that was verified against a
/// live runtime.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentCapabilities {
    inner: ExternalRuntimeCapabilities,
    /// The provenance of this capability view (§11.3). Defaults to
    /// [`CapabilitySource::Declared`] so views decoded from older snapshots that
    /// predate the source model are treated as the conservative baseline.
    #[serde(default)]
    source: CapabilitySource,
}

impl ExternalAgentCapabilities {
    /// Wraps a lower-layer capability set as the facade view.
    ///
    /// The provenance is recorded as [`CapabilitySource::Supplied`]: this is the
    /// generic public constructor a caller reaches for when handing the facade a
    /// capability set of their own (for example to pass to
    /// [`ManagedExternalAgentBuilder::capabilities`]). Preset baselines use
    /// [`declared`](Self::declared) and probe results use [`probed`](Self::probed)
    /// so their provenance is not conflated with a caller-supplied grade.
    #[must_use]
    pub fn from_runtime_capabilities(inner: ExternalRuntimeCapabilities) -> Self {
        Self::with_source(inner, CapabilitySource::Supplied)
    }

    /// Wraps a capability set with an explicit [`CapabilitySource`].
    #[must_use]
    fn with_source(inner: ExternalRuntimeCapabilities, source: CapabilitySource) -> Self {
        Self { inner, source }
    }

    /// Wraps an adapter/preset's static declared baseline
    /// ([`CapabilitySource::Declared`]).
    #[must_use]
    pub fn declared(inner: ExternalRuntimeCapabilities) -> Self {
        Self::with_source(inner, CapabilitySource::Declared)
    }

    /// Wraps a caller-supplied capability set ([`CapabilitySource::Supplied`]).
    #[must_use]
    pub fn supplied(inner: ExternalRuntimeCapabilities) -> Self {
        Self::with_source(inner, CapabilitySource::Supplied)
    }

    /// Wraps a capability set obtained by probing the live runtime
    /// ([`CapabilitySource::Probed`]).
    #[must_use]
    pub fn probed(inner: ExternalRuntimeCapabilities) -> Self {
        Self::with_source(inner, CapabilitySource::Probed)
    }

    /// Builds the facade view from a negotiated ACP `initialize` handshake.
    ///
    /// The mapping is the pure [`capabilities_from_initialize`] projection: the
    /// three protocol-guaranteed bits (streaming, permission bridge, graceful
    /// shutdown) plus `resume` iff the agent advertised `session/load`. The
    /// provenance is recorded as [`CapabilitySource::Negotiated`].
    #[cfg(feature = "external-acp")]
    #[must_use]
    pub fn from_acp_negotiation(negotiated: &AcpNegotiatedCapabilities) -> Self {
        Self::with_source(
            capabilities_from_initialize(negotiated),
            CapabilitySource::Negotiated,
        )
    }

    /// Returns the runtime these capabilities describe.
    #[must_use]
    pub const fn runtime(&self) -> &ExternalRuntimeKind {
        &self.inner.runtime
    }

    /// Returns the provenance of this capability view.
    ///
    /// A [`Declared`](CapabilitySource::Declared) source is a conservative static
    /// baseline; [`Probed`](CapabilitySource::Probed) and
    /// [`Negotiated`](CapabilitySource::Negotiated) reflect what a live runtime
    /// actually advertised (§11.3).
    #[must_use]
    pub const fn source(&self) -> CapabilitySource {
        self.source
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
#[derive(Clone)]
pub struct ManagedExternalAgent {
    runtime: ExternalRuntimeKind,
    mode: ExternalRunMode,
    capabilities: ExternalAgentCapabilities,
    worktree: Option<WorktreeRef>,
    binary: Option<PathBuf>,
    model: Option<String>,
    args: Vec<String>,
    permission_mode: ExternalPermissionMode,
    /// The runtime IO seam that advances the managed session (M4-2).
    ///
    /// A [`ManagedExternalAgent`] is data-first, but driving it to fulfill a
    /// delegation needs *some* [`ExternalSessionHandler`] to advance the real
    /// (or scripted) runtime. That handler is the one non-data attachment: it is
    /// held behind an `Arc`, never serialized, and rendered opaquely by
    /// [`Debug`]. Presets leave it `None`; attach one with
    /// [`ManagedExternalAgentBuilder::session_handler`].
    session_handler: Option<Arc<dyn ExternalSessionHandler>>,
}

impl std::fmt::Debug for ManagedExternalAgent {
    /// Renders the data fields, treating the session handler as opaque so no
    /// live runtime attachment leaks into diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedExternalAgent")
            .field("runtime", &self.runtime)
            .field("mode", &self.mode)
            .field("capabilities", &self.capabilities)
            .field("worktree", &self.worktree)
            .field("binary", &self.binary)
            .field("model", &self.model)
            .field("args", &self.args)
            .field("permission_mode", &self.permission_mode)
            .field("has_session_handler", &self.session_handler.is_some())
            .finish()
    }
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

    /// Gates a managed feature against the agent's **currently held** capability
    /// view, honoring its [`CapabilitySource`].
    ///
    /// A host calls this before requesting a capability-bearing behavior (host
    /// tools, a permission bridge, resume, …) so an unsupported request fails
    /// fast rather than silently degrading. The judgment is made against
    /// [`capabilities()`](Self::capabilities): once
    /// [`build_with_default_session_handler`](ManagedExternalAgentBuilder::build_with_default_session_handler)
    /// has folded in a [`Probed`](CapabilitySource::Probed) grade, this reflects
    /// what the live runtime actually reported, not the conservative declared
    /// baseline (§11.3) — so a capability the declared baseline advertises but the
    /// probe did not is correctly rejected.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::UnsupportedExternalCapability`] naming the runtime,
    /// the capability, and the view's [`CapabilitySource`] when the capability is
    /// not supported. The message carries no runtime output or credentials.
    pub fn require_capability(&self, capability: ExternalCapability) -> Result<(), FacadeError> {
        if self.capabilities.supports(capability) {
            return Ok(());
        }
        Err(FacadeError::UnsupportedExternalCapability {
            runtime: runtime_label(&self.runtime),
            capability: capability.as_str(),
            capability_source: self.capabilities.source().as_str(),
        })
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

    /// Returns the injected runtime session handler, if one was attached.
    ///
    /// This is the IO seam a delegation drive advances (M4-2). It is `None`
    /// unless a host attached one with
    /// [`ManagedExternalAgentBuilder::session_handler`].
    #[must_use]
    pub(crate) fn session_handler(&self) -> Option<&Arc<dyn ExternalSessionHandler>> {
        self.session_handler.as_ref()
    }

    /// Reconstructs a data-only recipe from restored [`AgentSnapshot`] parts.
    ///
    /// A snapshot restores an external delegate's data (runtime, mode, worktree,
    /// model, args, permission mode) but never its runtime session handler,
    /// binary override, or credentials (§15.2). This rebuilds the data-only
    /// recipe without a session handler (re-supplied on restore through
    /// [`AgentRestoreBuilder::external_agent`](crate::facade::AgentRestoreBuilder::external_agent))
    /// and without re-validating the run mode: the spec was already validated
    /// when the agent was first built, and the true capabilities are re-probed
    /// only when the delegate is actually driven (§11.3). The capability baseline
    /// stored here is the runtime's conservative declared set.
    #[must_use]
    pub(crate) fn from_restored_parts(
        runtime: ExternalRuntimeKind,
        mode: ExternalRunMode,
        worktree: Option<WorktreeRef>,
        model: Option<String>,
        args: Vec<String>,
        permission_mode: ExternalPermissionMode,
    ) -> Self {
        let capabilities = ExternalAgentCapabilities::declared(declared_capabilities(&runtime));
        Self {
            runtime,
            mode,
            capabilities,
            worktree,
            binary: None,
            model,
            args,
            permission_mode,
            session_handler: None,
        }
    }
}

/// Builder for a [`ManagedExternalAgent`], reached through a preset constructor.
///
/// The mode defaults to [`ExternalRunMode::Managed`] and the permission mode to
/// [`ExternalPermissionMode::Prompt`] (the safest). [`build`](Self::build)
/// validates the requested mode against the runtime's capabilities.
#[derive(Clone)]
pub struct ManagedExternalAgentBuilder {
    runtime: ExternalRuntimeKind,
    mode: ExternalRunMode,
    capabilities: ExternalAgentCapabilities,
    worktree: Option<WorktreeRef>,
    binary: Option<PathBuf>,
    model: Option<String>,
    args: Vec<String>,
    permission_mode: ExternalPermissionMode,
    session_handler: Option<Arc<dyn ExternalSessionHandler>>,
}

impl std::fmt::Debug for ManagedExternalAgentBuilder {
    /// Mirrors [`ManagedExternalAgent`]'s [`Debug`]: data fields verbatim, the
    /// session handler opaque.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedExternalAgentBuilder")
            .field("runtime", &self.runtime)
            .field("mode", &self.mode)
            .field("capabilities", &self.capabilities)
            .field("worktree", &self.worktree)
            .field("binary", &self.binary)
            .field("model", &self.model)
            .field("args", &self.args)
            .field("permission_mode", &self.permission_mode)
            .field("has_session_handler", &self.session_handler.is_some())
            .finish()
    }
}

impl ManagedExternalAgentBuilder {
    /// Builds a builder for a named CLI runtime with its declared baseline
    /// capabilities.
    fn for_runtime(runtime: ExternalRuntimeKind) -> Self {
        let capabilities = ExternalAgentCapabilities::declared(declared_capabilities(&runtime));
        Self {
            runtime,
            mode: ExternalRunMode::Managed,
            capabilities,
            worktree: None,
            binary: None,
            model: None,
            args: Vec::new(),
            permission_mode: ExternalPermissionMode::Prompt,
            session_handler: None,
        }
    }

    /// Builds a builder from an ACP launch config, seeding launch data and the
    /// protocol-guaranteed (pre-negotiation) capability baseline.
    ///
    /// The baseline is tagged [`CapabilitySource::Declared`]: it is the static
    /// pre-negotiation floor, not the result of a live `initialize` handshake. A
    /// real [`acp_negotiated`](Self::acp_negotiated) call folds in the
    /// [`Negotiated`](CapabilitySource::Negotiated) grade once the agent responds.
    #[cfg(feature = "external-acp")]
    fn from_acp_config(config: AcpConfig) -> Self {
        let capabilities = ExternalAgentCapabilities::declared(capabilities_from_initialize(
            &AcpNegotiatedCapabilities::none(),
        ));
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
            session_handler: None,
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

    /// Attaches the runtime session handler that advances this managed agent's
    /// sessions (M4-2).
    ///
    /// A [`ManagedExternalAgent`] stays data-first, but *driving* it as an
    /// external delegate needs an [`ExternalSessionHandler`]: the sans-io
    /// [`ExternalAgentMachine`](crate::agent::external::ExternalAgentMachine) reifies each
    /// runtime round-trip as a `NeedExternalSession` requirement, and this
    /// handler is what advances the real (or, in tests, scripted) runtime to its
    /// next decision point. It is the one non-data attachment on the spec — held
    /// behind an `Arc`, never serialized, and treated as opaque by
    /// [`Debug`]; a snapshot persists only the data fields (M4-3).
    ///
    /// Without a handler an external delegation fails fast rather than silently
    /// degrading (see [`FacadeError::ExternalAgent`]). A host wires a real
    /// registry-backed handler here (behind the matching `external-*` feature);
    /// offline tests inject a scripted handler.
    #[must_use]
    pub fn session_handler(mut self, handler: Arc<dyn ExternalSessionHandler>) -> Self {
        self.session_handler = Some(handler);
        self
    }

    /// Validates the requested mode against the runtime's capabilities and
    /// produces the [`ManagedExternalAgent`] spec.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::UnsupportedExternalMode`] when the runtime does not
    /// support every capability the requested [`ExternalRunMode`] needs. The error
    /// names the runtime, the mode, the missing capabilities, and the
    /// [`CapabilitySource`] the check was made against (so a host can tell a
    /// conservative [`Declared`](CapabilitySource::Declared) baseline apart from a
    /// verified [`Probed`](CapabilitySource::Probed) or
    /// [`Negotiated`](CapabilitySource::Negotiated) grade), so a host can pick a
    /// supported grade (see [`ExternalAgentCapabilities::supported_modes`]) or a
    /// different runtime instead of degrading silently.
    pub fn build(self) -> Result<ManagedExternalAgent, FacadeError> {
        validate_external_mode(&self.runtime, self.mode, &self.capabilities)?;

        Ok(ManagedExternalAgent {
            runtime: self.runtime,
            mode: self.mode,
            capabilities: self.capabilities,
            worktree: self.worktree,
            binary: self.binary,
            model: self.model,
            args: self.args,
            permission_mode: self.permission_mode,
            session_handler: self.session_handler,
        })
    }

    /// Validates and builds the [`ManagedExternalAgent`], then ensures it carries
    /// a runtime session handler so it is ready to drive without a second
    /// assembly pass.
    ///
    /// This is the one-call ergonomic path that replaces the round-trip of
    /// building an agent, handing it to [`default_external_session_handler`], and
    /// then rebuilding the builder with [`session_handler`](Self::session_handler):
    ///
    /// - If a handler was already supplied with
    ///   [`session_handler`](Self::session_handler), it is honored verbatim and no
    ///   probe runs. This keeps the manual/custom-handler path intact and lets a
    ///   host (or an offline test) inject a scripted handler through the same
    ///   entry point.
    /// - Otherwise the agent's runtime is probed and the official
    ///   registry-backed handler is assembled via
    ///   [`default_external_session_handler_with_capabilities`] and attached to
    ///   the returned agent. The probe result is folded back in as the agent's
    ///   capability view, tagged [`CapabilitySource::Probed`], so every later
    ///   capability judgment (`capabilities()`,
    ///   [`require_capability`](ManagedExternalAgent::require_capability)) reflects
    ///   what the live runtime actually reported rather than the conservative
    ///   declared baseline (§11.3). Because the probed grade can be narrower than
    ///   the declared one, the requested [`ExternalRunMode`] is re-validated
    ///   against it: if the live runtime does not advertise a capability the mode
    ///   needs, construction fails fast with
    ///   [`FacadeError::UnsupportedExternalMode`] (now naming the `probed` source)
    ///   rather than returning an agent whose probed view contradicts its mode.
    ///
    /// The ACP arm negotiates capabilities through the live `initialize`
    /// handshake per session rather than an offline probe, so it attaches the
    /// handler without overriding the declared/negotiated view.
    ///
    /// The default build pulls in **no** CLI-adapter machinery, so when the
    /// matching `external-*` feature is not compiled in this fails fast with the
    /// exact same non-secret [`FacadeError::ExternalAgent`] "enable the feature"
    /// message the standalone [`default_external_session_handler`] returns —
    /// never a silent no-op.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::UnsupportedExternalMode`] when the requested mode is
    /// not supported by the runtime's capabilities — checked first against the
    /// declared baseline (as [`build`](Self::build) does) and again against the
    /// probed view — or [`FacadeError::ExternalAgent`] when the default handler
    /// cannot be assembled because the runtime's adapter feature is not compiled
    /// in or its capability probe fails (missing binary, unauthenticated CLI). The
    /// probe error carries no credentials, so a host can treat it as a
    /// conservative skip rather than a crash.
    pub async fn build_with_default_session_handler(
        self,
    ) -> Result<ManagedExternalAgent, FacadeError> {
        // A caller-supplied handler wins: honor the custom/manual path and skip
        // the probe entirely (the default is only assembled to fill the gap).
        if self.session_handler.is_some() {
            return self.build();
        }
        let mut agent = self.build()?;
        let (handler, probed) = default_external_session_handler_with_capabilities(&agent).await?;
        agent.session_handler = Some(handler);
        // Fold the probe result in as the agent's real capability view so later
        // judgments are made against verified capabilities, not the declared
        // baseline. Re-validate the mode: the probed grade may be narrower.
        if let Some(probed) = probed {
            validate_external_mode(agent.runtime(), agent.mode(), &probed)?;
            agent.capabilities = probed;
        }
        Ok(agent)
    }
}

/// Validates that `capabilities` can serve `mode`, mapping a gap to a non-secret
/// [`FacadeError::UnsupportedExternalMode`] that names the missing capabilities
/// and the view's [`CapabilitySource`].
///
/// Shared by [`ManagedExternalAgentBuilder::build`] (against the declared
/// baseline) and
/// [`build_with_default_session_handler`](ManagedExternalAgentBuilder::build_with_default_session_handler)
/// (again against the probed view), so the "probed wins" rule applies to the
/// mode grade too.
fn validate_external_mode(
    runtime: &ExternalRuntimeKind,
    mode: ExternalRunMode,
    capabilities: &ExternalAgentCapabilities,
) -> Result<(), FacadeError> {
    if capabilities.supports_mode(mode) {
        return Ok(());
    }
    let missing = capabilities
        .missing_for_mode(mode)
        .into_iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(FacadeError::UnsupportedExternalMode {
        runtime: runtime_label(runtime),
        mode: mode.as_str(),
        missing,
        capability_source: capabilities.source().as_str(),
    })
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

/// The IO and probe timeout the default session handler applies to a managed
/// runtime.
///
/// This bounds both the capability probe and every live-session read the
/// handler drives. It is generous (two minutes) because a managed coding-agent
/// turn can legitimately think for a while before it reaches its next decision
/// point; a host that needs a different bound builds the adapter and
/// [`RegistryExternalSessionHandler`] directly instead of using this default.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode",
    feature = "external-acp"
))]
const DEFAULT_EXTERNAL_IO_TIMEOUT: Duration = Duration::from_secs(120);

/// Builds the official registry-backed [`ExternalSessionHandler`] for a
/// [`ManagedExternalAgent`], probing the live runtime and wiring the matching
/// adapter behind an [`ExternalSessionRegistry`].
///
/// This is the "last mile" a host would otherwise hand-assemble: it selects the
/// live runtime adapter for [`agent.runtime()`](ManagedExternalAgent::runtime),
/// builds its launch config from the managed spec (worktree, binary, model, and
/// permission mode), runs the runtime's capability probe (for the CLI runtimes)
/// so the reported capabilities are the verified intersection rather than the
/// declared baseline (§11.3), and wraps the probed adapter in a fresh registry.
/// The registry owns a default
/// [`GitWorktreeManager`](crate::agent::external::GitWorktreeManager), so the
/// delegate policy's `isolation` level is applied inside the library on every
/// session start/resume and cleaned up with the session's shutdown disposition
/// (M2-7, `docs/managed-external-agent.md` §16). The returned handler is ready
/// to inject with
/// [`ManagedExternalAgentBuilder::session_handler`]. A cancelled or failed
/// facade drive is force-closed automatically through
/// [`ExternalSessionHandler::cleanup_agent`] (M3-2), so a host that does
/// nothing extra leaks no subprocess; because this returns the concrete
/// [`RegistryExternalSessionHandler`] (which coerces to
/// `Arc<dyn ExternalSessionHandler>` at the injection point), a host also
/// keeps the [`registry`](RegistryExternalSessionHandler::registry) accessor
/// to force-close a *completed* session it is done with, or to sweep ahead of
/// teardown, with
/// [`cleanup_agent`](ExternalSessionRegistry::cleanup_agent).
///
/// Each runtime arm is gated on its `external-*` feature. The ACP arm builds the
/// single shared ACP adapter and lets the live `initialize` handshake negotiate
/// capabilities per session, so no offline probe runs for it.
///
/// # Errors
///
/// Returns [`FacadeError::ExternalAgent`] with a stable, non-secret message when:
///
/// - the runtime's adapter feature is not compiled in (fail-fast rather than
///   silently doing nothing), or
/// - the capability probe fails because the CLI binary is missing, is not
///   usable, or does not advertise a required capability. The classified
///   [`ExternalAgentError`](crate::agent::external::ExternalAgentError) is
///   surfaced verbatim (it carries no credentials), so a host can treat a
///   missing binary / unauthenticated CLI as a conservative skip instead of a
///   crash, exactly as the managed examples do — never a silent capability
///   downgrade.
pub async fn default_external_session_handler(
    agent: &ManagedExternalAgent,
) -> Result<Arc<RegistryExternalSessionHandler>, FacadeError> {
    let (handler, _capabilities) =
        default_external_session_handler_with_capabilities(agent).await?;
    Ok(handler)
}

/// Assembles the official registry-backed [`ExternalSessionHandler`] like
/// [`default_external_session_handler`] and, for the CLI runtimes, also returns
/// the [`Probed`](CapabilitySource::Probed) capability view the probe reported.
///
/// This is the seam
/// [`build_with_default_session_handler`](ManagedExternalAgentBuilder::build_with_default_session_handler)
/// uses to fold the probe result back into the agent so its capability view is
/// the verified grade rather than the declared baseline (§11.3). The returned
/// capabilities are:
///
/// - `Some(view)` tagged [`CapabilitySource::Probed`] for the CLI runtimes
///   (Claude Code, Codex, OpenCode), which run an offline capability probe, and
/// - `None` for ACP, which negotiates capabilities through the live `initialize`
///   handshake per session rather than an offline probe — the caller keeps its
///   declared/negotiated view.
///
/// # Errors
///
/// Same non-secret [`FacadeError::ExternalAgent`] conditions as
/// [`default_external_session_handler`]: the runtime's adapter feature is not
/// compiled in, or its capability probe fails (missing binary, unauthenticated
/// CLI). Credentials are never included, so a host can treat a probe failure as a
/// conservative skip.
pub async fn default_external_session_handler_with_capabilities(
    agent: &ManagedExternalAgent,
) -> Result<
    (
        Arc<RegistryExternalSessionHandler>,
        Option<ExternalAgentCapabilities>,
    ),
    FacadeError,
> {
    let (registry, probed) = build_default_registry(agent).await?;
    let handler = Arc::new(RegistryExternalSessionHandler::new(Arc::new(registry)));
    let capabilities = probed.map(ExternalAgentCapabilities::probed);
    Ok((handler, capabilities))
}

/// Selects the live adapter for `agent`'s runtime and wraps it in a registry,
/// returning the probe result alongside for the CLI runtimes.
///
/// Every named arm is feature-gated; a runtime whose feature is off falls
/// through to the catch-all, which fails fast with an explicit "enable the
/// feature" message rather than degrading silently. The second tuple element is
/// the probed [`ExternalRuntimeCapabilities`] for the CLI runtimes and `None` for
/// ACP (which negotiates per session).
async fn build_default_registry(
    agent: &ManagedExternalAgent,
) -> Result<(ExternalSessionRegistry, Option<ExternalRuntimeCapabilities>), FacadeError> {
    match agent.runtime() {
        #[cfg(feature = "external-claude-code")]
        ExternalRuntimeKind::ClaudeCode => {
            let (registry, probed) = build_claude_code_registry(agent).await?;
            Ok((registry, Some(probed)))
        }
        #[cfg(feature = "external-codex")]
        ExternalRuntimeKind::Codex => {
            let (registry, probed) = build_codex_registry(agent).await?;
            Ok((registry, Some(probed)))
        }
        #[cfg(feature = "external-opencode")]
        ExternalRuntimeKind::OpenCode => {
            let (registry, probed) = build_opencode_registry(agent).await?;
            Ok((registry, Some(probed)))
        }
        #[cfg(feature = "external-acp")]
        ExternalRuntimeKind::Custom(label) if label == ACP_RUNTIME_LABEL => {
            Ok((build_acp_registry(agent)?, None))
        }
        other => Err(runtime_feature_disabled(other)),
    }
}

/// Builds the fail-fast error for a runtime whose adapter feature is not
/// compiled into this build.
fn runtime_feature_disabled(runtime: &ExternalRuntimeKind) -> FacadeError {
    let label = runtime_label(runtime);
    FacadeError::ExternalAgent {
        name: label.clone(),
        message: format!(
            "no managed runtime adapter is compiled in for `{label}`; rebuild with the matching \
             `external-*` feature (external-claude-code / external-codex / external-opencode / \
             external-acp) to build its default session handler"
        ),
    }
}

/// Lifts a probe/launch failure into a non-secret [`FacadeError`].
///
/// The classified [`ExternalAgentError`](crate::agent::external::ExternalAgentError)
/// carries no credentials, so it is surfaced verbatim; a host reads it to decide
/// whether to skip (missing binary, unauthenticated CLI) or fail.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode"
))]
fn external_probe_error(
    runtime: &ExternalRuntimeKind,
    error: &crate::agent::external::ExternalAgentError,
) -> FacadeError {
    FacadeError::ExternalAgent {
        name: runtime_label(runtime),
        message: format!("could not build the default session handler: {error}"),
    }
}

/// Resolves the worktree working directory from the managed spec, if one was set.
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode",
    feature = "external-acp"
))]
fn agent_working_dir(agent: &ManagedExternalAgent) -> Option<PathBuf> {
    agent
        .worktree()
        .map(|worktree| worktree.path().to_path_buf())
}

/// Probes the local Claude Code CLI and wraps it in a registry, returning the
/// probed capabilities alongside so the caller can fold them into the agent's
/// capability view.
#[cfg(feature = "external-claude-code")]
async fn build_claude_code_registry(
    agent: &ManagedExternalAgent,
) -> Result<(ExternalSessionRegistry, ExternalRuntimeCapabilities), FacadeError> {
    use crate::agent::external::{ClaudeCodeAdapter, ClaudeCodeConfig, probe};

    let mut config = ClaudeCodeConfig::new()
        .with_permission_mode(agent.permission_mode())
        .with_timeout(DEFAULT_EXTERNAL_IO_TIMEOUT);
    if let Some(working_dir) = agent_working_dir(agent) {
        config = config.with_working_dir(working_dir);
    }
    if let Some(binary) = agent.binary() {
        config = config.with_binary(binary);
    }
    if let Some(model) = agent.model() {
        config = config.with_model(model);
    }
    let probed = probe(&config)
        .await
        .map_err(|error| external_probe_error(agent.runtime(), &error))?;
    let adapter = ClaudeCodeAdapter::with_probed_capabilities(config, &probed);
    Ok((ExternalSessionRegistry::new(Arc::new(adapter)), probed))
}

/// Probes the local Codex CLI and wraps it in a registry, returning the probed
/// capabilities alongside so the caller can fold them into the agent's capability
/// view.
#[cfg(feature = "external-codex")]
async fn build_codex_registry(
    agent: &ManagedExternalAgent,
) -> Result<(ExternalSessionRegistry, ExternalRuntimeCapabilities), FacadeError> {
    use crate::agent::external::{CodexAdapter, CodexConfig, codex_probe};

    let mut config = CodexConfig::new()
        .with_permission_mode(agent.permission_mode())
        .with_timeout(DEFAULT_EXTERNAL_IO_TIMEOUT);
    if let Some(working_dir) = agent_working_dir(agent) {
        config = config.with_working_dir(working_dir);
    }
    if let Some(binary) = agent.binary() {
        config = config.with_binary(binary);
    }
    if let Some(model) = agent.model() {
        config = config.with_model(model);
    }
    let probed = codex_probe(&config)
        .await
        .map_err(|error| external_probe_error(agent.runtime(), &error))?;
    let adapter = CodexAdapter::with_probed_capabilities(config, &probed);
    Ok((ExternalSessionRegistry::new(Arc::new(adapter)), probed))
}

/// Probes the local OpenCode CLI and wraps it in a registry, returning the probed
/// capabilities alongside so the caller can fold them into the agent's capability
/// view.
#[cfg(feature = "external-opencode")]
async fn build_opencode_registry(
    agent: &ManagedExternalAgent,
) -> Result<(ExternalSessionRegistry, ExternalRuntimeCapabilities), FacadeError> {
    use crate::agent::external::{OpenCodeAdapter, OpenCodeConfig, opencode_probe};

    let mut config = OpenCodeConfig::new()
        .with_permission_mode(agent.permission_mode())
        .with_timeout(DEFAULT_EXTERNAL_IO_TIMEOUT);
    if let Some(working_dir) = agent_working_dir(agent) {
        config = config.with_working_dir(working_dir);
    }
    if let Some(binary) = agent.binary() {
        config = config.with_binary(binary);
    }
    if let Some(model) = agent.model() {
        config = config.with_model(model);
    }
    let probed = opencode_probe(&config)
        .await
        .map_err(|error| external_probe_error(agent.runtime(), &error))?;
    let adapter = OpenCodeAdapter::with_probed_capabilities(config, &probed);
    Ok((ExternalSessionRegistry::new(Arc::new(adapter)), probed))
}

/// Rebuilds the ACP launch config from the managed spec and wraps the shared ACP
/// adapter in a registry.
///
/// ACP negotiates capabilities through the live `initialize` handshake rather
/// than an offline probe, so this arm is synchronous: the adapter reports its
/// implemented features and each session refines them on connect.
#[cfg(feature = "external-acp")]
fn build_acp_registry(
    agent: &ManagedExternalAgent,
) -> Result<ExternalSessionRegistry, FacadeError> {
    use crate::agent::external::AcpAdapter;

    let binary = agent.binary().ok_or_else(|| FacadeError::ExternalAgent {
        name: runtime_label(agent.runtime()),
        message: "the ACP managed agent has no launch binary; build it with \
                  ManagedExternalAgent::acp(..) or an ACP preset"
            .to_owned(),
    })?;
    let mut config = AcpConfig::new(binary, agent.args().iter().cloned())
        .with_permission_mode(agent.permission_mode())
        .with_timeout(DEFAULT_EXTERNAL_IO_TIMEOUT);
    if let Some(working_dir) = agent_working_dir(agent) {
        config = config.with_working_dir(working_dir);
    }
    let adapter = AcpAdapter::new(config);
    Ok(ExternalSessionRegistry::new(Arc::new(adapter)))
}

#[cfg(test)]
mod tests;
