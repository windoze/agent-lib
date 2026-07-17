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

use crate::agent::external::{
    ExternalAgentMachine, ExternalAgentSpec, ExternalAgentState, ExternalArtifactRef,
    ExternalSessionPolicy, ExternalSessionRef, ExternalStreamPolicy, WorktreeIsolation,
};
use crate::agent::{
    AgentError, AgentId, AgentInput, AgentMachine, AgentSpecRef, DrivingSubagentHandler,
    ExternalCapability, ExternalPermissionMode, ExternalRuntimeCapabilities, ExternalRuntimeKind,
    ExternalSessionHandler, HandlerScope, Interaction, LoopCursor, RequirementIds,
    RequirementResult, RunContext, RunId, ScopePop, SpawnedChild, StepInput, StepOutcome,
    SubagentHandler, SubagentOutput, SubagentSpawner, ToolSetRef, TraceNodeId, TurnDone,
    WorktreeRef,
};
use crate::conversation::{Conversation, ConversationConfig};
use crate::facade::agent::final_turn_summary;
use crate::facade::delegate::DEFAULT_MAX_DELEGATION_DEPTH;
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::ArtifactRef;
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::usage::Usage;
use serde_json::Map;

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
        let capabilities =
            ExternalAgentCapabilities::from_runtime_capabilities(declared_capabilities(&runtime));
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
            session_handler: None,
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
            session_handler: self.session_handler,
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

/// A named managed external delegate registered on an [`Agent`](crate::facade::Agent).
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
pub(crate) struct RetainedExternalSession {
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

/// Wraps an [`ExternalAgentMachine`] to capture its terminal facts.
///
/// The [`SubagentSpawner`] only observes the drained [`TurnDone`], never the
/// child machine state, so this wrapper snapshots the current
/// [`ExternalAgentState`] into a shared slot after every step. On a
/// `Completed` step it captures the committed turn's summary/usage plus the
/// recorded artifacts; on a cancel `Abandon` step it captures the
/// [`cleanup_required`](ExternalAgentState::cleanup_required) marker. The
/// [`drive_external`] caller then reads the slot to fold the result back and
/// record the delegation trace, artifacts, and usage.
struct RecordingExternalMachine {
    inner: ExternalAgentMachine,
    slot: ExternalOutcomeSlot,
}

impl AgentMachine for RecordingExternalMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        let outcome = self.inner.step(input);
        let state = self.inner.state();
        let completed = matches!(self.inner.cursor(), LoopCursor::Done(_));
        let (summary, usage, _stop) = final_turn_summary(state.conversation());
        let artifacts = state.artifacts().iter().map(map_artifact).collect();
        *self.slot.lock().expect("external outcome slot poisoned") = Some(ExternalDriveOutcome {
            summary,
            usage,
            artifacts,
            completed,
            cleanup_required: state.cleanup_required(),
            session: state.session().cloned(),
        });
        outcome
    }

    fn cursor(&self) -> &LoopCursor {
        self.inner.cursor()
    }
}

/// The child external session's own drain layer: it serves only the
/// `NeedExternalSession` family through the injected handler.
///
/// Every other requirement the external machine could emit (a bridged
/// `NeedInteraction`, `NeedTool`, or `NeedSubagent`) pops to the outer layer;
/// M4-2 wires a headless [`EmptyExternalScope`] there, so a request the base
/// approval/host path does not serve surfaces as an
/// [`UnhandledRequirement`](crate::agent::AgentError::UnhandledRequirement)
/// rather than being silently dropped. The richer external approval wiring lands
/// in M4-3.
struct ExternalChildScope {
    external: Arc<dyn ExternalSessionHandler>,
}

impl HandlerScope for ExternalChildScope {
    fn external(&self) -> Option<&dyn ExternalSessionHandler> {
        Some(self.external.as_ref())
    }
}

/// An empty outer layer for the external child drive.
#[derive(Default)]
struct EmptyExternalScope;

impl HandlerScope for EmptyExternalScope {}

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
}

impl SubagentSpawner for FacadeExternalSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        Ok((
            self.ids.run_id(),
            TraceNodeId::new(format!("external:{}", self.name)),
        ))
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
            .expect("external outcome slot poisoned")
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
/// [`ExternalSessionHandler`] (design §11.2). A cancelled `ctx` makes the drive
/// abandon the outstanding session step, so the returned outcome carries the
/// runtime cleanup marker.
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
    ctx: &RunContext,
) -> Result<ExternalDriveOutcome, FacadeError> {
    let Some(handler) = agent.session_handler() else {
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
        handler: handler.clone(),
        ids: ids.clone(),
        task: task.clone(),
        slot: slot.clone(),
    });
    let handler = DrivingSubagentHandler::new(spawner, DEFAULT_MAX_DELEGATION_DEPTH);

    let spec_ref = AgentSpecRef(agent_id);
    let brief = Interaction::question(ids.step_id(), task);
    let empty = EmptyExternalScope;
    let mut outer = ScopePop::new(&empty, None);

    let result = handler
        .fulfill(&spec_ref, &brief, None, &mut outer, ctx)
        .await;

    let captured = slot
        .lock()
        .expect("external outcome slot poisoned")
        .clone()
        .unwrap_or_default();

    match result {
        RequirementResult::Subagent(Ok(_output)) => Ok(captured),
        RequirementResult::Subagent(Err(error)) => Err(FacadeError::ExternalAgent {
            name: name.to_owned(),
            message: error.to_string(),
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

    #[tokio::test]
    async fn drive_external_marks_cleanup_on_cancel() {
        use super::drive_external;
        use crate::agent::{
            BudgetLimits, ExternalSessionHandler, ExternalSessionRequest, RequirementResult,
            RunContext,
        };
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

        let outcome = drive_external("coder", &coder, &ids, "refactor".to_owned(), &ctx)
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
