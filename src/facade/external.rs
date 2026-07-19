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
//! [`ExternalAgentMachine`]) and the external
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
use std::sync::{Arc, Mutex};
#[cfg(any(
    feature = "external-claude-code",
    feature = "external-codex",
    feature = "external-opencode",
    feature = "external-acp"
))]
use std::time::Duration;

pub use crate::agent::external::RegistryExternalSessionHandler;
use crate::agent::external::{
    ExternalAgentMachine, ExternalAgentSpec, ExternalAgentState, ExternalArtifactRef,
    ExternalSessionPolicy, ExternalSessionRef, ExternalSessionRegistry, ExternalStreamPolicy,
    WorktreeIsolation,
};
use crate::agent::{
    AgentError, AgentId, AgentInput, AgentMachine, AgentSpecRef, ApprovalDecision,
    ApprovalResponse, DrivingSubagentHandler, ExternalCapability, ExternalPermissionMode,
    ExternalRuntimeCapabilities, ExternalRuntimeKind, ExternalSessionHandler, HandlerScope,
    Interaction, InteractionHandler, InteractionKind, InteractionOrigin, InteractionResponse,
    LoopCursor, PermissionResponse, RequirementIds, RequirementKindTag, RequirementResult,
    RunContext, RunId, ScopePop, SpawnedChild, StepInput, StepOutcome, SubagentHandler,
    SubagentOutput, SubagentSpawner, ToolSetRef, TraceNodeId, TurnDone, WorktreeRef,
};
use crate::conversation::{Conversation, ConversationConfig};
use crate::facade::agent::final_turn_summary;
use crate::facade::collab::CollabBridge;
use crate::facade::delegate::DEFAULT_MAX_DELEGATION_DEPTH;
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::ArtifactRef;
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::usage::Usage;
use async_trait::async_trait;
use serde_json::Map;

#[cfg(feature = "external-acp")]
use crate::agent::external::{
    ACP_RUNTIME_LABEL, AcpConfig, AcpNegotiatedCapabilities, acp_runtime_kind,
    capabilities_from_initialize,
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
    /// [`ExternalAgentMachine`] reifies each
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

///
/// This pairs the registration `name` (which mints the `ask_<name>` delegation
/// tool, §13.1) with the data-first [`ManagedExternalAgent`] recipe that is
/// driven when the supervising model routes work to it (M4-2). It mirrors
/// [`LocalSubagent`](crate::facade::LocalSubagent) for the external side: the
/// live runtime is assembled only when a delegation is fulfilled.
#[derive(Clone, Debug)]
pub struct ManagedExternalDelegate {
    name: String,
    agent: ManagedExternalAgent,
}

impl ManagedExternalDelegate {
    /// Stamps `name` onto `agent`, forming a registered external delegate.
    #[must_use]
    pub(crate) fn new(name: impl Into<String>, agent: ManagedExternalAgent) -> Self {
        Self {
            name: name.into(),
            agent,
        }
    }

    /// Returns the delegate's registration name (the `ask_<name>` stem).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns a terse description advertised on the delegation tool.
    ///
    /// A managed external agent carries no free-form description, so this is a
    /// generated one naming the backing runtime and run mode.
    #[must_use]
    pub fn description(&self) -> String {
        format!(
            "Delegate a task to the `{}` managed external agent ({} runtime, {} mode).",
            self.name,
            runtime_label(self.agent.runtime()),
            self.agent.mode().as_str()
        )
    }

    /// Returns the data-first managed external agent recipe.
    #[must_use]
    pub const fn agent(&self) -> &ManagedExternalAgent {
        &self.agent
    }
}

/// The policy for reconciling a managed external delegate's previously-live
/// session when an [`Agent`](crate::facade::Agent) is restored from an
/// [`AgentSnapshot`](crate::facade::AgentSnapshot) (`docs/facade-api.md` §15.3,
/// `PLAN.md` R6).
///
/// An [`AgentSnapshot`](crate::facade::AgentSnapshot) captures only *data* about
/// a managed external delegate's last-known session (its runtime kind, worktree,
/// session id, last status, artifact and transcript refs) — never the live
/// process, SDK client, or credentials (§15.2). When such a snapshot is
/// restored, the previously-live external runtime is gone, so the caller must
/// declare how to reconcile it:
///
/// - [`MarkInterrupted`](Self::MarkInterrupted) (the default) records the
///   delegate as interrupted and does **not** touch the external runtime, so the
///   caller can inspect [`RunOutput`](crate::facade::RunOutput) or the snapshot
///   and decide to continue, cancel, manually repair, or restart. This is the
///   safe default because a coding agent may already have changed the worktree,
///   so a blind restart is risky (R6).
/// - [`AttachOrFail`](Self::AttachOrFail) re-attaches to the recorded session and
///   fails fast if it cannot (no re-registered runtime handler, no resumable
///   session, or a runtime that does not support resume). Reserved for read-only
///   / resumable external agents where re-attaching is safe (R6).
/// - [`RestartFromBrief`](Self::RestartFromBrief) discards the recorded session
///   and lets the next run start the delegate afresh from its task brief.
///
/// ```
/// use agent_lib::facade::RestoreExternal;
///
/// // The safe default leaves the external runtime untouched.
/// assert_eq!(RestoreExternal::default(), RestoreExternal::MarkInterrupted);
/// assert_eq!(RestoreExternal::AttachOrFail.as_str(), "attach_or_fail");
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreExternal {
    /// Re-attach to the recorded session; fail fast if it cannot be attached.
    AttachOrFail,
    /// Mark the delegate interrupted without touching the external runtime
    /// (the safe default).
    #[default]
    MarkInterrupted,
    /// Discard the recorded session and start the delegate afresh next run.
    RestartFromBrief,
}

impl RestoreExternal {
    /// Returns the stable snake_case label of this policy.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AttachOrFail => "attach_or_fail",
            Self::MarkInterrupted => "mark_interrupted",
            Self::RestartFromBrief => "restart_from_brief",
        }
    }
}

impl std::fmt::Display for RestoreExternal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The last-known lifecycle status of a managed external delegate's session, as
/// captured in an [`AgentSnapshot`](crate::facade::AgentSnapshot) (data-only,
/// §15.2).
///
/// This is a coarse, serializable status a host can inspect after a restore to
/// decide how to proceed; it carries no runtime handle or credential.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalDelegateStatus {
    /// No session has been driven for this delegate yet.
    #[default]
    Pending,
    /// The last driven session completed cleanly.
    Completed,
    /// The last driven session failed or was cancel-abandoned.
    Failed,
    /// The session was marked interrupted by an
    /// [`AgentSnapshot`](crate::facade::AgentSnapshot) restore under
    /// [`RestoreExternal::MarkInterrupted`].
    Interrupted,
}

impl ExternalDelegateStatus {
    /// Returns the stable snake_case label of this status.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

impl std::fmt::Display for ExternalDelegateStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The retained last-known session facts for one managed external delegate.
///
/// The [`Agent`](crate::facade::Agent) updates this after a `run_full` drive so a
/// later [`snapshot`](crate::facade::Agent::snapshot) can persist the delegate's
/// data-only session state (status, resumable [`ExternalSessionRef`], and any
/// reported [`ArtifactRef`]s). It never holds a process handle, SDK client, or
/// credential.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RetainedExternalSession {
    /// The delegate's last-known coarse status.
    pub status: ExternalDelegateStatus,
    /// The resumable session facts reported by the last drive, if any.
    pub session: Option<ExternalSessionRef>,
    /// Artifacts reported by the last completed drive, in order.
    pub artifacts: Vec<ArtifactRef>,
}

/// The facts captured from a driven external delegation.
///
/// A [`RecordingExternalMachine`] snapshots these off the
/// [`ExternalAgentState`] after every step, so the last write reflects the final
/// state whether the session ran to completion or was abandoned on cancel.
#[derive(Clone, Debug, Default)]
pub(crate) struct ExternalDriveOutcome {
    /// The session's final summary text, folded back as the tool result.
    pub summary: String,
    /// Token usage reported by the runtime for the delegated turn.
    pub usage: Usage,
    /// Artifacts (patches/diffs/test results/files) the session reported.
    pub artifacts: Vec<ArtifactRef>,
    /// Whether the machine reached its terminal `Done` cursor.
    pub completed: bool,
    /// Whether the abandoned session left a live runtime for the handle layer to
    /// sweep (the cancel cleanup marker, design §6.4).
    pub cleanup_required: bool,
    /// The resumable session facts the runtime reported, if any. Captured so a
    /// later [`Agent`](crate::facade::Agent) snapshot can persist the delegate's
    /// data-only session id / transcript / resume token (§15.2).
    pub session: Option<ExternalSessionRef>,
}

/// A shared, single-slot capture of an [`ExternalDriveOutcome`].
type ExternalOutcomeSlot = Arc<Mutex<Option<ExternalDriveOutcome>>>;

/// Wraps an [`ExternalAgentMachine`] to capture its terminal facts and bridge
/// its collab observations.
///
/// The [`SubagentSpawner`] only observes the drained [`TurnDone`], never the
/// child machine state, so this wrapper snapshots the current
/// [`ExternalAgentState`] into a shared slot after every step. On a
/// `Completed` step it captures the committed turn's summary/usage plus the
/// recorded artifacts; on a cancel `Abandon` step it captures the
/// [`cleanup_required`](ExternalAgentState::cleanup_required) marker. The
/// [`drive_external`] caller then reads the slot to fold the result back and
/// record the delegation trace, artifacts, and usage.
///
/// Every step's notifications are also handed to the [`CollabBridge`], which
/// reflects the delegate's `send_message` / `plan_update` / `blackboard_post`
/// observations into the facade's provisioned collab substrate (§14 末段). A
/// machine replays each observation exactly once (design §5.5), so the bridge
/// absorbs each collab event a single time.
struct RecordingExternalMachine {
    inner: ExternalAgentMachine,
    slot: ExternalOutcomeSlot,
    /// The delegate's name, attributed as the sender of bridged collab writes.
    from: String,
    /// Bridge into the facade's provisioned collab substrate.
    bridge: CollabBridge,
}

impl AgentMachine for RecordingExternalMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        let outcome = self.inner.step(input);
        let state = self.inner.state();
        let completed = matches!(self.inner.cursor(), LoopCursor::Done(_));
        let (summary, usage, _stop) = final_turn_summary(state.conversation());
        let artifacts = state.artifacts().iter().map(map_artifact).collect();
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = Some(ExternalDriveOutcome {
            summary,
            usage,
            artifacts,
            completed,
            cleanup_required: state.cleanup_required(),
            session: state.session().cloned(),
        });
        self.bridge
            .absorb_notifications(&self.from, &outcome.notifications);
        outcome
    }

    fn cursor(&self) -> &LoopCursor {
        self.inner.cursor()
    }
}

/// The child external session's own drain layer: it serves only the
/// `NeedExternalSession` family through the injected handler.
///
/// Other requirements the external machine could emit (a bridged
/// `NeedInteraction`, `NeedTool`, or `NeedSubagent`) pop to the outer layer. The
/// facade installs [`ExternalInteractionScope`] outside this child layer so
/// external permission prompts can be answered by the supervisor-injected
/// interaction handler while unsupported families still surface as unhandled
/// requirements instead of being silently dropped.
struct ExternalChildScope {
    external: Arc<dyn ExternalSessionHandler>,
}

impl HandlerScope for ExternalChildScope {
    fn external(&self) -> Option<&dyn ExternalSessionHandler> {
        Some(self.external.as_ref())
    }
}

/// The outer layer for an external child drive.
///
/// When the supervisor supplied an async interaction handler, this scope answers
/// external runtime permission prompts through it with delegate attribution. When
/// no handler is present the scope deliberately stays headless for interactions;
/// `drive_external` turns the resulting `UnhandledRequirement` into a clearer
/// facade error.
struct ExternalInteractionScope {
    interaction: Option<ExternalInteractionRouter>,
}

impl ExternalInteractionScope {
    /// Builds the optional interaction route for one external delegate.
    fn new(delegate: String, parent: Option<Arc<dyn InteractionHandler>>) -> Self {
        Self {
            interaction: parent.map(|parent| ExternalInteractionRouter { delegate, parent }),
        }
    }
}

impl HandlerScope for ExternalInteractionScope {
    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        self.interaction
            .as_ref()
            .map(|router| router as &dyn InteractionHandler)
    }
}

/// Routes an external child interaction to the supervisor's injected handler.
struct ExternalInteractionRouter {
    delegate: String,
    parent: Arc<dyn InteractionHandler>,
}

#[async_trait]
impl InteractionHandler for ExternalInteractionRouter {
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult {
        let routed = request
            .clone()
            .with_origin(InteractionOrigin::new(self.delegate.clone(), ctx.depth()));
        tokio::select! {
            biased;
            _ = ctx.cancellation().cancelled() => cancelled_external_interaction_result(&routed),
            result = self.parent.fulfill(&routed, ctx) => result,
        }
    }
}

/// Builds an in-family interaction result when cancellation wins the route.
fn cancelled_external_interaction_result(request: &Interaction) -> RequirementResult {
    let response = match request.kind() {
        InteractionKind::Approval { call_id, .. } => {
            InteractionResponse::Approval(ApprovalResponse::new(
                request.step_id(),
                *call_id,
                ApprovalDecision::Deny,
                Some("interaction cancelled".to_owned()),
            ))
        }
        InteractionKind::Question { .. } => InteractionResponse::answer(String::new()),
        InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
        InteractionKind::Permission { request } => InteractionResponse::Permission(
            PermissionResponse::cancel(request.action_id().to_owned()),
        ),
    };
    RequirementResult::Interaction(response)
}

/// Turns one external delegation into a drivable [`ExternalAgentMachine`], its
/// scope, and its opening input.
///
/// Built fresh per delegation call so its capture `slot` is call-local. The
/// external [`ExternalAgentSpec`] is rebuilt from the delegate's data-first
/// [`ManagedExternalAgent`]: its runtime kind, worktree, permission mode, and an
/// empty tool set (host tools are an M4-3+ capability). The scope serves the
/// machine's `NeedExternalSession` requirements through the delegate's injected
/// [`ExternalSessionHandler`].
struct FacadeExternalSpawner {
    name: String,
    agent_id: AgentId,
    runtime: ExternalRuntimeKind,
    worktree: WorktreeRef,
    policy: ExternalSessionPolicy,
    handler: Arc<dyn ExternalSessionHandler>,
    ids: FacadeIds,
    task: String,
    slot: ExternalOutcomeSlot,
    /// Bridge the child machine's collab observations flow into (§14 末段).
    bridge: CollabBridge,
}

impl SubagentSpawner for FacadeExternalSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        // See `FacadeSubagentSpawner::child_ids`: the freshly minted run id keeps
        // the trace node id unique across repeated drives of the same external
        // delegate within one run (a fixed `external:{name}` would collide).
        let run_id = self.ids.run_id();
        let node = TraceNodeId::new(format!("external:{}:{run_id}", self.name));
        Ok((run_id, node))
    }

    fn spawn(
        &self,
        _spec_ref: &AgentSpecRef,
        _brief: &Interaction,
        _result_schema: Option<&serde_json::Value>,
    ) -> Result<SpawnedChild, AgentError> {
        let spec = ExternalAgentSpec::new(
            self.agent_id,
            self.runtime.clone(),
            self.worktree.clone(),
            None,
            ToolSetRef::new(self.ids.tool_set_id(), Vec::new()),
            self.policy,
        );
        let state = ExternalAgentState::new(
            spec,
            Conversation::new(self.ids.conversation_id(), ConversationConfig::new(None)),
        );
        let requirement_ids: Arc<dyn RequirementIds> = Arc::new(self.ids.clone());
        let machine = ExternalAgentMachine::new(state, requirement_ids);
        let recording = RecordingExternalMachine {
            inner: machine,
            slot: self.slot.clone(),
            from: self.name.clone(),
            bridge: self.bridge.clone(),
        };

        let scope = ExternalChildScope {
            external: self.handler.clone(),
        };

        let user = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: self.task.clone(),
                extra: Map::new(),
            }],
        };
        let opening = AgentInput::user_message(
            self.ids.turn_id(),
            self.ids.message_id(),
            user,
            self.ids.message_id(),
            self.ids.step_id(),
        )?;

        Ok(SpawnedChild {
            machine: Box::new(recording),
            scope: Box::new(scope),
            opening,
        })
    }

    fn summarize(&self, _done: &TurnDone) -> SubagentOutput {
        let summary = self
            .slot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .map(|captured| captured.summary.clone())
            .unwrap_or_default();
        SubagentOutput { summary }
    }
}

/// Drives one managed external delegation to its next terminal state, returning
/// the captured [`ExternalDriveOutcome`].
///
/// The external agent is driven the same way a local subagent is (M3-2): through
/// the reference [`DrivingSubagentHandler`], so it shares the host's scope
/// derivation, cancel propagation, budget ledger, and trace node. The child
/// machine is an [`ExternalAgentMachine`] whose `NeedExternalSession`
/// requirements are served by the delegate's injected
/// [`ExternalSessionHandler`] (design §11.2). External `NeedInteraction`
/// requirements pop to an outer route that uses the supervisor-injected
/// [`InteractionHandler`] when present, adding delegate/depth attribution before
/// the answer is fed back to the runtime. A cancelled `ctx` makes the drive
/// abandon the outstanding session step, so the returned outcome carries the
/// runtime cleanup marker.
///
/// # Automatic session cleanup (M3-2)
///
/// A drive that ends without a committed session — cancel-abandoned
/// ([`cleanup_required`](ExternalDriveOutcome::cleanup_required)) or failed
/// before reaching its terminal cursor — may have left a live runtime in the
/// handler's registry, so this helper force-closes it before returning:
/// [`ExternalSessionHandler::cleanup_agent`] is called with the drive's
/// freshly minted agent id, which scopes the sweep to exactly this drive's
/// sessions. The shipped registry-backed handler forwards that to
/// [`ExternalSessionRegistry::cleanup_agent`], running the adapter's shutdown
/// (a best-effort `session/cancel` plus transport close, process-group
/// termination for a real child) and feeding each session's disposition into
/// the registry's worktree policy; the dispositions are also recorded into the
/// run trace (best effort). A host that does nothing extra therefore leaks no
/// subprocess. A *committed* drive keeps its live session untouched — the
/// clean-teardown / dirty-retention worktree policy is unchanged.
///
/// # Errors
///
/// Returns [`FacadeError::ExternalAgent`] when the delegate has no session
/// handler attached, or when the drive fails before reaching a terminal cursor.
pub(crate) async fn drive_external(
    name: &str,
    agent: &ManagedExternalAgent,
    ids: &FacadeIds,
    task: String,
    collab: &CollabBridge,
    parent_interaction: Option<Arc<dyn InteractionHandler>>,
    ctx: &RunContext,
) -> Result<ExternalDriveOutcome, FacadeError> {
    let Some(session_handler) = agent.session_handler() else {
        return Err(FacadeError::ExternalAgent {
            name: name.to_owned(),
            message: "no runtime session handler is attached; call \
                      ManagedExternalAgentBuilder::session_handler(..) to drive it"
                .to_owned(),
        });
    };

    let worktree = agent
        .worktree()
        .cloned()
        .unwrap_or_else(|| WorktreeRef::new("."));
    let policy = ExternalSessionPolicy {
        permission_mode: agent.permission_mode(),
        isolation: WorktreeIsolation::EphemeralGitWorktree,
        max_turns: None,
        stream_events: ExternalStreamPolicy::Buffered,
    };

    let slot: ExternalOutcomeSlot = Arc::new(Mutex::new(None));
    let agent_id = ids.agent_id();
    let spawner = Arc::new(FacadeExternalSpawner {
        name: name.to_owned(),
        agent_id,
        runtime: agent.runtime().clone(),
        worktree,
        policy,
        handler: session_handler.clone(),
        ids: ids.clone(),
        task: task.clone(),
        slot: slot.clone(),
        bridge: collab.clone(),
    });
    let handler = DrivingSubagentHandler::new(spawner, DEFAULT_MAX_DELEGATION_DEPTH);

    let spec_ref = AgentSpecRef(agent_id);
    let brief = Interaction::question(ids.step_id(), task);
    let interaction_scope = ExternalInteractionScope::new(name.to_owned(), parent_interaction);
    let mut outer = ScopePop::new(&interaction_scope, None);

    let result = handler
        .fulfill(&spec_ref, &brief, None, &mut outer, ctx)
        .await;

    let captured = slot
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
        .unwrap_or_default();

    // M3-2: a drive that did not commit its session — cancel-abandoned
    // (`cleanup_required`) or failed before reaching a terminal cursor — may
    // have left a live runtime in the handler's registry; sweep it so a host
    // that does nothing extra leaks no subprocess. The freshly minted
    // `agent_id` scopes the sweep to exactly this drive's sessions, and a
    // committed drive is left untouched (worktree teardown/retention policy
    // unchanged). Each swept session's disposition is recorded into the run
    // trace, best effort, mirroring the adapter mid-session close audit.
    if !captured.completed {
        let dispositions = session_handler.cleanup_agent(agent_id).await;
        for (seq, disposition) in dispositions.into_iter().enumerate() {
            let id = TraceNodeId::new(format!("external-cleanup-sweep/{}/{seq}", ctx.run_id()));
            let _ = ctx.trace().record_external_shutdown(id, disposition);
        }
    }

    match result {
        RequirementResult::Subagent(Ok(_output)) => Ok(captured),
        RequirementResult::Subagent(Err(error)) => Err(FacadeError::ExternalAgent {
            name: name.to_owned(),
            message: external_drive_error_message(&error),
        }),
        other => Err(FacadeError::ExternalAgent {
            name: name.to_owned(),
            message: format!(
                "external drive returned an unexpected `{}` result",
                other.tag()
            ),
        }),
    }
}

/// Renders external drive failures with a targeted interaction-routing message.
fn external_drive_error_message(error: &AgentError) -> String {
    if matches!(
        error,
        AgentError::UnhandledRequirement {
            kind: RequirementKindTag::Interaction,
            ..
        }
    ) {
        return "external agent requested permission but no interaction handler is available to answer it"
            .to_owned();
    }

    error.to_string()
}

/// Projects an agent-layer [`ExternalArtifactRef`] into the facade
/// [`ArtifactRef`] surface.
///
/// The facade artifact reference carries only a locating `path`; the agent-layer
/// reference may leave `path` unset (for example a bare test result), so this
/// falls back to the opaque stored `reference` and finally the untrusted
/// `summary` so an artifact is never advertised without a locator. Only
/// references are copied — never inline diffs — keeping the mapping
/// redaction-safe (design §11).
fn map_artifact(artifact: &ExternalArtifactRef) -> ArtifactRef {
    let path = artifact
        .path
        .clone()
        .or_else(|| artifact.reference.clone())
        .unwrap_or_else(|| artifact.summary.clone());
    ArtifactRef { path }
}

#[cfg(test)]
mod tests {
    use super::{
        CapabilitySource, ExternalAgentCapabilities, ExternalRunMode, ManagedExternalAgent,
        declared_capabilities,
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
                capability_source,
            } => {
                assert_eq!(runtime, "codex");
                assert_eq!(mode, "managed_with_tools");
                assert_eq!(missing, "host_tools");
                // The check was made against the preset's declared baseline.
                assert_eq!(capability_source, "declared");
            }
            other => panic!("expected UnsupportedExternalMode, got {other:?}"),
        }
    }

    #[test]
    fn preset_capabilities_are_declared() {
        // A preset seeds the runtime's conservative *declared* baseline, not a
        // verified grade.
        let codex = ManagedExternalAgent::codex().build().expect("build codex");
        assert_eq!(codex.capabilities().source(), CapabilitySource::Declared);
    }

    #[test]
    fn from_runtime_capabilities_is_supplied() {
        // The generic public wrapper records caller-supplied provenance so a
        // manual `.capabilities(..)` path is not conflated with a declared or
        // probed grade.
        let caps = ExternalAgentCapabilities::from_runtime_capabilities(declared_capabilities(
            &ExternalRuntimeKind::Codex,
        ));
        assert_eq!(caps.source(), CapabilitySource::Supplied);
        assert_eq!(
            ExternalAgentCapabilities::supplied(declared_capabilities(&ExternalRuntimeKind::Codex))
                .source(),
            CapabilitySource::Supplied
        );
    }

    #[test]
    fn supplied_capabilities_flow_through_builder() {
        // Folding a caller-built capability set through `.capabilities(..)`
        // preserves its `Supplied` provenance on the built agent.
        let supplied =
            ExternalAgentCapabilities::supplied(declared_capabilities(&ExternalRuntimeKind::Codex));
        let codex = ManagedExternalAgent::codex()
            .capabilities(supplied)
            .build()
            .expect("build codex");
        assert_eq!(codex.capabilities().source(), CapabilitySource::Supplied);
    }

    #[test]
    fn probed_capabilities_are_probed() {
        // The probe-provenance constructor tags its view accordingly; the default
        // handler builder (M4-4) folds such a view in after a real probe.
        let caps = ExternalAgentCapabilities::probed(declared_capabilities(
            &ExternalRuntimeKind::ClaudeCode,
        ));
        assert_eq!(caps.source(), CapabilitySource::Probed);
    }

    #[test]
    fn capability_source_labels_match_serde() {
        for source in [
            CapabilitySource::Declared,
            CapabilitySource::Supplied,
            CapabilitySource::Probed,
            CapabilitySource::Negotiated,
        ] {
            let json = serde_json::to_value(source).expect("serialize source");
            assert_eq!(json, serde_json::Value::String(source.as_str().to_owned()));
            assert_eq!(source.to_string(), source.as_str());
        }
        assert_eq!(CapabilitySource::default(), CapabilitySource::Declared);
    }

    #[test]
    fn capabilities_source_defaults_when_absent_from_serde() {
        // A view decoded from data that predates the source model falls back to
        // the conservative `Declared` baseline rather than failing.
        let mut encoded = serde_json::to_value(
            ManagedExternalAgent::codex()
                .build()
                .expect("build codex")
                .capabilities(),
        )
        .expect("serialize caps");
        encoded
            .as_object_mut()
            .expect("caps object")
            .remove("source");
        let decoded: ExternalAgentCapabilities =
            serde_json::from_value(encoded).expect("deserialize legacy caps");
        assert_eq!(decoded.source(), CapabilitySource::Declared);
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
        // The pre-negotiation baseline is a static declared floor, not a live
        // handshake result.
        assert_eq!(base.capabilities().source(), CapabilitySource::Declared);

        // Attachable fails fast before load_session is negotiated.
        let error = ManagedExternalAgent::opencode_acp()
            .mode(ExternalRunMode::Attachable)
            .build()
            .expect_err("resume must be negotiated first");
        assert!(matches!(
            error,
            FacadeError::UnsupportedExternalMode {
                capability_source: "declared",
                ..
            }
        ));

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
        // A real negotiation result is tagged as such.
        assert_eq!(
            attachable.capabilities().source(),
            CapabilitySource::Negotiated
        );
        assert_eq!(attachable.mode(), ExternalRunMode::Attachable);
    }

    #[tokio::test]
    async fn drive_external_marks_cleanup_on_cancel() {
        use super::drive_external;
        use crate::agent::{
            BudgetLimits, ExternalSessionHandler, ExternalSessionRequest, RequirementResult,
            RunContext,
        };
        use crate::facade::collab::CollabBridge;
        use crate::facade::ids::FacadeIds;
        use async_trait::async_trait;
        use std::sync::Arc;

        // A handler that must never be invoked: a pre-cancelled drive abandons the
        // session's opening `NeedExternalSession` before reaching `fulfill`.
        struct NeverInvokedHandler;

        #[async_trait]
        impl ExternalSessionHandler for NeverInvokedHandler {
            async fn fulfill(
                &self,
                _request: &ExternalSessionRequest,
                _ctx: &RunContext,
            ) -> RequirementResult {
                panic!("the session handler must not run when the drive is cancelled");
            }
        }

        let coder = ManagedExternalAgent::claude_code()
            .session_handler(Arc::new(NeverInvokedHandler))
            .build()
            .expect("managed external agent builds");

        let ids = FacadeIds::seeded(7);
        let ctx = RunContext::new_root(
            ids.run_id(),
            BudgetLimits::unbounded(),
            ids.trace_root("external-cancel"),
        );
        ctx.cancellation().cancel();

        let outcome = drive_external(
            "coder",
            &coder,
            &ids,
            "refactor".to_owned(),
            &CollabBridge::default(),
            None,
            &ctx,
        )
        .await
        .expect("a cancelled drive still returns its captured outcome");

        // The abandoned session left a cleanup marker for the handle layer to
        // sweep, and never reached a completed state (design §6.4).
        assert!(
            outcome.cleanup_required,
            "a cancelled external session leaves a cleanup marker"
        );
        assert!(
            !outcome.completed,
            "a cancelled external session did not complete"
        );
        assert!(outcome.artifacts.is_empty());
    }

    #[cfg(feature = "external-acp")]
    fn acp_permission_interaction(
        ids: &crate::facade::ids::FacadeIds,
    ) -> (crate::agent::Interaction, crate::agent::AgentId) {
        use crate::agent::{PermissionCategory, PermissionRequest, PermissionRisk};

        let actor = ids.agent_id();
        let request = PermissionRequest::new(
            "act-1".to_owned(),
            actor,
            PermissionCategory::Shell,
            "run `cargo test`".to_owned(),
            serde_json::Value::Null,
            PermissionRisk::Medium,
            Some("verify the refactor".to_owned()),
        );
        (
            crate::agent::Interaction::permission(ids.step_id(), request),
            actor,
        )
    }

    #[cfg(feature = "external-acp")]
    fn acp_completed_output(summary: &str) -> crate::agent::external::ExternalAgentOutput {
        crate::agent::external::ExternalAgentOutput {
            summary: summary.to_owned(),
            artifacts: Vec::new(),
            usage: None,
            cost_micros: None,
        }
    }

    #[cfg(feature = "external-acp")]
    fn external_root_context(ids: &crate::facade::ids::FacadeIds) -> crate::agent::RunContext {
        crate::agent::RunContext::new_root(
            ids.run_id(),
            crate::agent::BudgetLimits::unbounded(),
            ids.trace_root("external-interaction-route"),
        )
    }

    #[cfg(feature = "external-acp")]
    fn acp_session_ref() -> crate::agent::external::ExternalSessionRef {
        crate::agent::external::ExternalSessionRef {
            runtime: crate::agent::external::acp_runtime_kind(),
            session_id: Some("sess-1".to_owned()),
            transcript_ref: None,
            resume_token: None,
            last_event_seq: None,
        }
    }

    #[cfg(feature = "external-acp")]
    fn acp_permission_pause(
        interaction: crate::agent::Interaction,
    ) -> crate::agent::external::ExternalSessionResult {
        use crate::agent::external::{ExternalAgentEvent, ExternalObservedEvent};

        crate::agent::external::ExternalSessionResult::PausedForInteraction {
            session: acp_session_ref(),
            action_id: "act-1".to_owned(),
            request: interaction,
            observations: ExternalObservedEvent::unsequenced_for_tests(vec![
                ExternalAgentEvent::PermissionRequested {
                    action_id: "act-1".to_owned(),
                    summary: "run `cargo test`".to_owned(),
                },
            ]),
        }
    }

    #[cfg(feature = "external-acp")]
    fn acp_completed(summary: &str) -> crate::agent::external::ExternalSessionResult {
        crate::agent::external::ExternalSessionResult::Completed {
            session: acp_session_ref(),
            output: acp_completed_output(summary),
            observations: Vec::new(),
        }
    }

    #[cfg(feature = "external-acp")]
    struct ScriptedExternalHandler {
        steps: std::sync::Mutex<
            std::collections::VecDeque<crate::agent::external::ExternalSessionResult>,
        >,
        requests:
            std::sync::Arc<std::sync::Mutex<Vec<crate::agent::external::ExternalSessionRequest>>>,
    }

    #[cfg(feature = "external-acp")]
    impl ScriptedExternalHandler {
        fn new(
            steps: impl IntoIterator<Item = crate::agent::external::ExternalSessionResult>,
        ) -> Self {
            Self {
                steps: std::sync::Mutex::new(steps.into_iter().collect()),
                requests: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn log(
            &self,
        ) -> std::sync::Arc<std::sync::Mutex<Vec<crate::agent::external::ExternalSessionRequest>>>
        {
            self.requests.clone()
        }
    }

    #[cfg(feature = "external-acp")]
    #[async_trait::async_trait]
    impl crate::agent::ExternalSessionHandler for ScriptedExternalHandler {
        async fn fulfill(
            &self,
            request: &crate::agent::external::ExternalSessionRequest,
            _ctx: &crate::agent::RunContext,
        ) -> crate::agent::RequirementResult {
            self.requests
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(request.clone());
            let result = self
                .steps
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .pop_front()
                .unwrap_or_else(|| crate::agent::external::ExternalSessionResult::Failed {
                    session: None,
                    error: crate::agent::external::ExternalAgentError::Runtime {
                        code: None,
                        message: "scripted external handler exhausted".to_owned(),
                        runtime_output: None,
                    },
                    observations: Vec::new(),
                });
            crate::agent::RequirementResult::ExternalSession(Box::new(result))
        }
    }

    #[cfg(feature = "external-acp")]
    struct RecordingParentInteractionHandler {
        requests: std::sync::Arc<std::sync::Mutex<Vec<crate::agent::Interaction>>>,
    }

    #[cfg(feature = "external-acp")]
    impl RecordingParentInteractionHandler {
        fn new() -> Self {
            Self {
                requests: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn log(&self) -> std::sync::Arc<std::sync::Mutex<Vec<crate::agent::Interaction>>> {
            self.requests.clone()
        }
    }

    #[cfg(feature = "external-acp")]
    #[async_trait::async_trait]
    impl crate::agent::InteractionHandler for RecordingParentInteractionHandler {
        async fn fulfill(
            &self,
            request: &crate::agent::Interaction,
            _ctx: &crate::agent::RunContext,
        ) -> crate::agent::RequirementResult {
            self.requests
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(request.clone());
            let crate::agent::InteractionKind::Permission { request } = request.kind() else {
                panic!("expected permission interaction, got {:?}", request.kind());
            };
            crate::agent::RequirementResult::Interaction(
                crate::agent::InteractionResponse::Permission(
                    crate::agent::PermissionResponse::approve(request.action_id().to_owned()),
                ),
            )
        }
    }

    /// A capturing `AsyncWrite` recording every byte the ACP session writes.
    #[cfg(feature = "external-acp")]
    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    #[cfg(feature = "external-acp")]
    impl tokio::io::AsyncWrite for SharedWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.0
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// An async reader that serves scripted bytes and then pends forever,
    /// modelling a live but silent ACP agent that never writes another line.
    #[cfg(feature = "external-acp")]
    struct ScriptedThenSilent {
        scripted: std::io::Cursor<Vec<u8>>,
    }

    #[cfg(feature = "external-acp")]
    impl tokio::io::AsyncRead for ScriptedThenSilent {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            #[allow(clippy::cast_possible_truncation)]
            if self.scripted.position() < self.scripted.get_ref().len() as u64 {
                return std::pin::Pin::new(&mut self.scripted).poll_read(cx, buf);
            }
            std::task::Poll::Pending
        }
    }

    /// A fake ACP launcher whose agent answers the handshake from a script and
    /// then stays silent forever, capturing every written frame.
    #[cfg(feature = "external-acp")]
    struct SilentTurnLauncher {
        handshake: std::sync::Mutex<Option<String>>,
        written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    #[cfg(feature = "external-acp")]
    impl SilentTurnLauncher {
        fn new(lines: &[&str]) -> Self {
            // Every scripted line must be newline-terminated: the reader never
            // reports EOF, so an unterminated tail would pend forever.
            Self {
                handshake: std::sync::Mutex::new(Some(format!("{}\n", lines.join("\n")))),
                written: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn written(&self) -> String {
            String::from_utf8(
                self.written
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .clone(),
            )
            .expect("utf8 frames")
        }
    }

    #[cfg(feature = "external-acp")]
    #[async_trait::async_trait]
    impl crate::agent::external::AcpLauncher for SilentTurnLauncher {
        async fn launch(
            &self,
            _config: &crate::agent::external::AcpConfig,
        ) -> Result<
            crate::agent::external::SpawnedAcpAgent,
            crate::agent::external::ExternalAgentError,
        > {
            let script = self
                .handshake
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .take()
                .unwrap_or_default();
            let reader = ScriptedThenSilent {
                scripted: std::io::Cursor::new(script.into_bytes()),
            };
            let writer = SharedWriter(std::sync::Arc::clone(&self.written));
            // A read timeout far beyond the test's settle bound: settling fast
            // proves cancellation — not the IO timeout — ended the wait.
            Ok(crate::agent::external::SpawnedAcpAgent::new(
                writer,
                reader,
                std::time::Duration::from_secs(60),
            ))
        }
    }

    /// A worktree manager that hands out synthetic prepared paths and records
    /// every cleanup call, so the test can watch the sweep's worktree wiring
    /// without touching a real filesystem.
    #[cfg(feature = "external-acp")]
    struct RecordingWorktreeManager {
        cleanups: std::sync::Mutex<
            Vec<(
                crate::agent::WorktreeRef,
                crate::agent::external::ExternalSessionShutdown,
            )>,
        >,
    }

    #[cfg(feature = "external-acp")]
    impl RecordingWorktreeManager {
        fn new() -> Self {
            Self {
                cleanups: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn cleanups(
            &self,
        ) -> Vec<(
            crate::agent::WorktreeRef,
            crate::agent::external::ExternalSessionShutdown,
        )> {
            self.cleanups
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }
    }

    #[cfg(feature = "external-acp")]
    #[async_trait::async_trait]
    impl crate::agent::external::WorktreeManager for RecordingWorktreeManager {
        async fn prepare(
            &self,
            agent_id: crate::agent::AgentId,
            base: &crate::agent::WorktreeRef,
            isolation: crate::agent::external::WorktreeIsolation,
        ) -> Result<crate::agent::external::PreparedWorktree, crate::agent::external::WorktreeError>
        {
            Ok(crate::agent::external::PreparedWorktree::new(
                agent_id,
                isolation,
                base.clone(),
                true,
            )
            .with_base_repo(base.clone()))
        }

        async fn cleanup(
            &self,
            prepared: crate::agent::external::PreparedWorktree,
            disposition: crate::agent::external::ExternalSessionShutdown,
        ) -> Result<
            crate::agent::external::WorktreeCleanupOutcome,
            crate::agent::external::WorktreeError,
        > {
            self.cleanups
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push((prepared.worktree().clone(), disposition));
            Ok(crate::agent::external::WorktreeCleanupOutcome::new(
                prepared.isolation(),
                prepared.worktree().clone(),
                true,
                disposition.leaves_residual_side_effects(),
            ))
        }
    }

    /// M3-2: a cancelled facade drive force-closes the abandoned session
    /// through the handler's registry with no host involvement — the runtime
    /// observes `session/cancel`, the live handle is deregistered (no dangling
    /// handle, no leaked subprocess), and the ephemeral worktree is swept with
    /// the session's shutdown disposition.
    #[cfg(feature = "external-acp")]
    #[tokio::test]
    async fn drive_external_cancel_sweeps_live_session_and_worktree() {
        use super::drive_external;
        use crate::agent::external::{
            AcpAdapter, AcpLauncher, ExternalRuntimeAdapter, ExternalSessionRegistry,
            ExternalSessionShutdown, WorktreeManager,
        };
        use crate::agent::{BudgetLimits, RunContext, TraceNodeKind};
        use crate::facade::collab::CollabBridge;
        use crate::facade::ids::FacadeIds;
        use std::sync::Arc;
        use std::time::Duration;

        let launcher = Arc::new(SilentTurnLauncher::new(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}"#,
        ]));
        let adapter = AcpAdapter::with_launcher(
            crate::agent::external::AcpConfig::opencode_acp(),
            Arc::clone(&launcher) as Arc<dyn AcpLauncher>,
        );
        let worktrees = Arc::new(RecordingWorktreeManager::new());
        let registry = Arc::new(ExternalSessionRegistry::with_worktree_manager(
            Arc::new(adapter) as Arc<dyn ExternalRuntimeAdapter>,
            Arc::clone(&worktrees) as Arc<dyn WorktreeManager>,
        ));
        let session_handler = Arc::new(
            crate::agent::external::RegistryExternalSessionHandler::new(Arc::clone(&registry)),
        );

        let agent = ManagedExternalAgent::opencode_acp()
            .session_handler(session_handler)
            .build()
            .expect("managed ACP external agent builds");

        let ids = FacadeIds::seeded(13);
        let ctx = RunContext::new_root(
            ids.run_id(),
            BudgetLimits::unbounded(),
            ids.trace_root("external-cancel-sweep"),
        );
        let token = ctx.cancellation().clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token.cancel();
        });

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            drive_external(
                "coder",
                &agent,
                &ids,
                "refactor".to_owned(),
                &CollabBridge::default(),
                None,
                &ctx,
            ),
        )
        .await
        .expect("a cancelled drive settles in seconds, not after the read timeout")
        .expect("a cancelled drive still returns its captured outcome");
        canceller.await.expect("canceller task");

        assert!(
            outcome.cleanup_required,
            "a cancelled external session leaves a cleanup marker"
        );
        assert!(!outcome.completed);

        // The abandoned session was force-closed with no host involvement: the
        // adapter's shutdown sent session/cancel and closed the transport, and
        // the registry deregistered the live handle.
        assert!(
            launcher.written().contains(r#""method":"session/cancel""#),
            "the sweep reached the live runtime: {}",
            launcher.written()
        );
        assert_eq!(
            registry.live_len(),
            0,
            "no dangling handle after the cancelled drive"
        );

        // The ephemeral worktree was swept exactly once with the session's
        // shutdown disposition (the childless stand-in closes gracefully).
        let cleanups = worktrees.cleanups();
        assert_eq!(cleanups.len(), 1, "one swept session, one worktree cleanup");
        assert_eq!(cleanups[0].1, ExternalSessionShutdown::Graceful);

        // The sweep's disposition was audited into the run trace.
        let shutdown_nodes = ctx
            .trace()
            .records()
            .into_iter()
            .filter(|record| matches!(record.kind(), TraceNodeKind::ExternalShutdown { .. }))
            .count();
        assert_eq!(shutdown_nodes, 1, "the sweep is recorded in the trace");
    }

    #[cfg(feature = "external-acp")]
    #[tokio::test]
    async fn drive_external_routes_permission_interaction_to_parent_handler() {
        use super::drive_external;
        use crate::agent::{
            ExternalSessionInput, InteractionHandler, InteractionKind, InteractionResponse,
            PermissionDecision,
        };
        use crate::facade::collab::CollabBridge;
        use crate::facade::ids::FacadeIds;
        use std::sync::Arc;

        let ids = FacadeIds::seeded(11);
        let (interaction, actor) = acp_permission_interaction(&ids);
        let runtime = ScriptedExternalHandler::new([
            acp_permission_pause(interaction),
            acp_completed("external complete"),
        ]);
        let external_log = runtime.log();

        let parent = RecordingParentInteractionHandler::new();
        let parent_log = parent.log();
        let parent_handler: Arc<dyn InteractionHandler> = Arc::new(parent);

        let agent = ManagedExternalAgent::opencode_acp()
            .session_handler(Arc::new(runtime))
            .build()
            .expect("managed ACP external agent builds");
        let ctx = external_root_context(&ids);

        let outcome = drive_external(
            "coder",
            &agent,
            &ids,
            "refactor".to_owned(),
            &CollabBridge::default(),
            Some(parent_handler),
            &ctx,
        )
        .await
        .expect("parent interaction handler resolves the external permission prompt");

        assert!(outcome.completed);
        assert_eq!(outcome.summary, "external complete");

        let interaction_records = parent_log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        assert_eq!(interaction_records.len(), 1);
        let request = &interaction_records[0];
        let origin = request
            .origin
            .as_deref()
            .expect("external route marks origin");
        assert_eq!(origin.delegate, "coder");
        assert_eq!(origin.depth, 1);
        match request.kind() {
            InteractionKind::Permission { request } => {
                assert_eq!(request.action_id(), "act-1");
                assert_eq!(request.actor(), actor);
            }
            other => panic!("expected permission interaction, got {other:?}"),
        }

        let external_records = external_log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        assert_eq!(external_records.len(), 2);
        assert!(matches!(
            external_records[0].input,
            ExternalSessionInput::Start { .. }
        ));
        match &external_records[1].input {
            ExternalSessionInput::RespondInteraction {
                action_id,
                response,
            } => {
                assert_eq!(action_id, "act-1");
                match response {
                    InteractionResponse::Permission(response) => {
                        assert_eq!(response.action_id(), "act-1");
                        assert_eq!(response.decision(), &PermissionDecision::Approve);
                    }
                    other => panic!("expected permission response, got {other:?}"),
                }
            }
            other => panic!("expected RespondInteraction, got {other:?}"),
        }
    }

    #[cfg(feature = "external-acp")]
    #[tokio::test]
    async fn drive_external_permission_without_parent_handler_fails_clearly() {
        use super::drive_external;
        use crate::facade::collab::CollabBridge;
        use crate::facade::ids::FacadeIds;
        use std::sync::Arc;

        let ids = FacadeIds::seeded(12);
        let (interaction, _actor) = acp_permission_interaction(&ids);
        let runtime = ScriptedExternalHandler::new([acp_permission_pause(interaction)]);
        let external_log = runtime.log();

        let agent = ManagedExternalAgent::opencode_acp()
            .session_handler(Arc::new(runtime))
            .build()
            .expect("managed ACP external agent builds");
        let ctx = external_root_context(&ids);

        let error = drive_external(
            "coder",
            &agent,
            &ids,
            "refactor".to_owned(),
            &CollabBridge::default(),
            None,
            &ctx,
        )
        .await
        .expect_err("permission prompt without parent handler must fail clearly");

        match error {
            FacadeError::ExternalAgent { name, message } => {
                assert_eq!(name, "coder");
                assert!(
                    message.contains("external agent requested permission")
                        && message.contains("no interaction handler"),
                    "unexpected error message: {message}"
                );
            }
            other => panic!("expected ExternalAgent error, got {other:?}"),
        }

        let external_records = external_log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        assert_eq!(external_records.len(), 1);
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

    // A runtime whose adapter feature is not compiled into this build fails fast
    // with an explicit "enable the feature" message rather than degrading
    // silently. Codex is used because this arm only runs when its feature is off.
    #[cfg(not(feature = "external-codex"))]
    #[tokio::test]
    async fn build_with_default_session_handler_fails_fast_when_feature_disabled() {
        let error = ManagedExternalAgent::codex()
            .build_with_default_session_handler()
            .await
            .expect_err("a runtime with no compiled adapter must fail fast");
        match error {
            FacadeError::ExternalAgent { name, message } => {
                assert_eq!(name, "codex");
                assert!(
                    message.contains("external-codex"),
                    "the message must name the feature to enable, got: {message}"
                );
                // The fail-fast message names only the feature to enable — never a
                // launch line, environment variable, or credential.
                assert!(
                    !message.contains("KEY") && !message.contains("TOKEN"),
                    "the fail-fast message must not leak a secret, got: {message}"
                );
            }
            other => panic!("expected a fail-fast ExternalAgent error, got {other:?}"),
        }
    }

    // A caller-supplied handler is honored verbatim by the one-call build path:
    // it short-circuits the probe, so the manual/custom-handler path keeps working
    // regardless of which `external-*` features are compiled in.
    #[tokio::test]
    async fn build_with_default_session_handler_honors_supplied_handler() {
        use crate::agent::{
            ExternalSessionHandler, ExternalSessionRequest, RequirementResult, RunContext,
        };
        use async_trait::async_trait;
        use std::sync::Arc;

        struct NeverInvokedHandler;

        #[async_trait]
        impl ExternalSessionHandler for NeverInvokedHandler {
            async fn fulfill(
                &self,
                _request: &ExternalSessionRequest,
                _ctx: &RunContext,
            ) -> RequirementResult {
                panic!("the supplied handler must not run during assembly");
            }
        }

        let agent = ManagedExternalAgent::codex()
            .session_handler(Arc::new(NeverInvokedHandler))
            .build_with_default_session_handler()
            .await
            .expect("a supplied handler short-circuits the default assembly");
        assert!(
            agent.session_handler().is_some(),
            "the supplied handler must flow through to the built agent"
        );
    }

    // The manual `.session_handler(..).build()` path stays usable on its own.
    #[test]
    fn manual_session_handler_path_still_builds() {
        use crate::agent::{
            ExternalSessionHandler, ExternalSessionRequest, RequirementResult, RunContext,
        };
        use async_trait::async_trait;
        use std::sync::Arc;

        struct NeverInvokedHandler;

        #[async_trait]
        impl ExternalSessionHandler for NeverInvokedHandler {
            async fn fulfill(
                &self,
                _request: &ExternalSessionRequest,
                _ctx: &RunContext,
            ) -> RequirementResult {
                unreachable!()
            }
        }

        let agent = ManagedExternalAgent::codex()
            .session_handler(Arc::new(NeverInvokedHandler))
            .build()
            .expect("the manual session-handler path still builds");
        assert!(agent.session_handler().is_some());
    }

    // A runtime whose adapter feature is not compiled into this build fails fast
    // with an explicit "enable the feature" message rather than degrading
    // silently. Codex is used because this arm only runs when its feature is off.
    #[cfg(not(feature = "external-codex"))]
    #[tokio::test]
    async fn default_handler_fails_fast_when_runtime_feature_disabled() {
        use super::default_external_session_handler;

        let codex = ManagedExternalAgent::codex().build().expect("build codex");
        let error = default_external_session_handler(&codex)
            .await
            .expect_err("a runtime with no compiled adapter must fail fast");
        match error {
            FacadeError::ExternalAgent { name, message } => {
                assert_eq!(name, "codex");
                assert!(
                    message.contains("external-codex"),
                    "the message must name the feature to enable, got: {message}"
                );
            }
            other => panic!("expected a fail-fast ExternalAgent error, got {other:?}"),
        }
    }

    // When the adapter feature *is* compiled in, a missing/broken CLI binary makes
    // the capability probe fail fast with a non-secret error rather than silently
    // building a degraded handler. An absolute non-existent path guarantees the
    // probe's spawn fails offline without touching PATH.
    #[cfg(feature = "external-claude-code")]
    #[tokio::test]
    async fn default_handler_fails_fast_when_cli_binary_is_missing() {
        use super::default_external_session_handler;

        let claude = ManagedExternalAgent::claude_code()
            .binary("/nonexistent-agent-lib/claude-probe-target")
            .build()
            .expect("build claude");
        let error = default_external_session_handler(&claude)
            .await
            .expect_err("a missing CLI binary must make the probe fail fast");
        match error {
            FacadeError::ExternalAgent { name, message } => {
                assert_eq!(name, "claude_code");
                assert!(
                    !message.is_empty(),
                    "the fail-fast error must carry a non-empty, non-secret message"
                );
            }
            other => panic!("expected a fail-fast ExternalAgent error, got {other:?}"),
        }
    }

    // A probe can report a *narrower* grade than the declared baseline. Once a
    // `Probed` view is folded in, the capability gate follows it — a capability
    // the declared baseline advertises but the probe did not verify is rejected,
    // and the classified error names the capability and provenance without a
    // secret.
    #[test]
    fn require_capability_gates_against_probed_view() {
        // The Claude Code declared baseline advertises a permission bridge; model
        // a probe that verified streaming but not the permission bridge or host
        // tools.
        let mut probed_caps = declared_capabilities(&ExternalRuntimeKind::ClaudeCode);
        assert!(
            probed_caps.permission_bridge,
            "the Claude Code declared baseline advertises a permission bridge"
        );
        probed_caps.permission_bridge = false;
        probed_caps.host_tools = false;

        let agent = ManagedExternalAgent::claude_code()
            .mode(ExternalRunMode::BlackBox)
            .capabilities(ExternalAgentCapabilities::probed(probed_caps))
            .build()
            .expect("black-box needs no capability, so the build succeeds");

        // The agent holds the probed view, not the declared baseline.
        assert_eq!(agent.capabilities().source(), CapabilitySource::Probed);

        // A capability the probe verified passes the gate.
        agent
            .require_capability(ExternalCapability::Streaming)
            .expect("streaming was probed as supported");

        // A capability the declared baseline advertises but the probe did not is
        // rejected — the probed view wins over the declared one.
        let error = agent
            .require_capability(ExternalCapability::PermissionBridge)
            .expect_err("the permission bridge was not verified by the probe");
        match error {
            FacadeError::UnsupportedExternalCapability {
                runtime,
                capability,
                capability_source,
            } => {
                assert_eq!(runtime, "claude_code");
                assert_eq!(capability, "permission_bridge");
                assert_eq!(capability_source, "probed");
            }
            other => panic!("expected UnsupportedExternalCapability, got {other:?}"),
        }

        // The rendered error names the capability and provenance but never a
        // launch line, environment variable, or credential.
        let rendered = agent
            .require_capability(ExternalCapability::HostTools)
            .expect_err("host tools were not verified by the probe")
            .to_string();
        assert!(
            rendered.contains("host_tools"),
            "the error must name the capability, got: {rendered}"
        );
        assert!(
            rendered.contains("probed"),
            "the error must name the capability source, got: {rendered}"
        );
        assert!(
            !rendered.contains("KEY") && !rendered.contains("TOKEN"),
            "the capability error must not leak a secret, got: {rendered}"
        );
    }

    // A declared-baseline preset still reports the `declared` provenance in the
    // gate error, so a host can tell a conservative baseline apart from a probed
    // grade.
    #[test]
    fn require_capability_reports_declared_provenance_for_a_preset() {
        let codex = ManagedExternalAgent::codex().build().expect("build codex");
        // Codex's declared baseline honestly advertises streaming.
        codex
            .require_capability(ExternalCapability::Streaming)
            .expect("codex declares streaming");
        let error = codex
            .require_capability(ExternalCapability::HostTools)
            .expect_err("codex declares no host-tool bridge");
        match error {
            FacadeError::UnsupportedExternalCapability {
                runtime,
                capability,
                capability_source,
            } => {
                assert_eq!(runtime, "codex");
                assert_eq!(capability, "host_tools");
                assert_eq!(capability_source, "declared");
            }
            other => panic!("expected UnsupportedExternalCapability, got {other:?}"),
        }
    }

    // The capabilities-returning helper fails fast with the same non-secret
    // "enable the feature" error when the runtime's adapter feature is off, so a
    // host that wants the probed view never gets a silent no-op.
    #[cfg(not(feature = "external-codex"))]
    #[tokio::test]
    async fn default_handler_with_capabilities_fails_fast_when_feature_disabled() {
        use super::default_external_session_handler_with_capabilities;

        let codex = ManagedExternalAgent::codex().build().expect("build codex");
        let error = default_external_session_handler_with_capabilities(&codex)
            .await
            .expect_err("a runtime with no compiled adapter must fail fast");
        match error {
            FacadeError::ExternalAgent { name, message } => {
                assert_eq!(name, "codex");
                assert!(
                    message.contains("external-codex"),
                    "the message must name the feature to enable, got: {message}"
                );
                assert!(
                    !message.contains("KEY") && !message.contains("TOKEN"),
                    "the fail-fast message must not leak a secret, got: {message}"
                );
            }
            other => panic!("expected a fail-fast ExternalAgent error, got {other:?}"),
        }
    }
}
