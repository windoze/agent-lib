//! Facade-layer error type.
//!
//! [`FacadeError`] intentionally exposes fewer variants than the underlying
//! `client`/`conversation`/`agent` errors while still preserving the original
//! failure as an error `source`. This keeps the ergonomic facade API simple
//! without discarding the diagnostic chain the lower layers already produce.

use thiserror::Error;

use crate::agent::AgentError;
use crate::client::ClientError;
use crate::conversation::ConversationError;

/// A single error type covering every fallible facade operation.
///
/// The facade is an assembly layer: most variants simply wrap a lower-layer
/// error (`Client`, `Conversation`) so callers keep the underlying
/// [`std::error::Error::source`] chain. `Config`, `UnexpectedToolUse`, and
/// `InvalidState` describe facade-specific conditions.
///
/// This enum is `#[non_exhaustive]`: later milestones add variants for the
/// Agent facade, tools, approval/permission denials, loop limits, unhandled
/// requirements, delegation, external sessions, and restore (see
/// `docs/facade-api.md` §16). New variants are additive, so match arms should
/// keep a catch-all.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FacadeError {
    /// A provider/model configuration value was missing or invalid.
    ///
    /// Returned, for example, when an environment-based
    /// [`crate::facade::ProviderConfig`] constructor cannot read a required
    /// variable, or when a builder is finalized without a mandatory field.
    #[error("facade configuration error: {0}")]
    Config(String),

    /// A client-layer request or decode failed.
    #[error("client error: {0}")]
    Client(#[from] ClientError),

    /// A Conversation operation was rejected.
    #[error("conversation error: {0}")]
    Conversation(#[from] ConversationError),

    /// An Agent-layer drive failed while running the loop.
    ///
    /// Wraps an [`AgentError`] surfaced either directly from the driver
    /// ([`crate::agent::drain`]) or reconstructed from a terminal
    /// [`crate::agent::LoopCursor::Error`] the machine came to rest on (see
    /// `docs/facade-api.md` §16). The underlying
    /// [`std::error::Error::source`] chain is preserved.
    #[error("agent error: {0}")]
    Agent(#[from] AgentError),

    /// The Agent loop hit its step / tool-round limit before the model produced
    /// a final assistant response.
    ///
    /// The facade bounds a run by `max_steps` and `max_tool_rounds` (see
    /// `docs/facade-api.md` §8.4); when a model keeps requesting tools past that
    /// budget the run fails fast with this variant rather than looping forever.
    #[error("agent loop step or tool-round limit exceeded")]
    LoopLimitExceeded,

    /// The configured run budget was exhausted before the turn could complete.
    ///
    /// Returned when [`AgentBuilder::budget`](crate::facade::AgentBuilder::budget)
    /// or [`AgentRestoreBuilder::budget`](crate::facade::AgentRestoreBuilder::budget)
    /// configured a count-like run budget and the shared driver budget ledger
    /// refused to start or resume more work. This is distinct from
    /// [`LoopLimitExceeded`](Self::LoopLimitExceeded), which is the facade's
    /// per-turn loop guard rather than the cross-cutting [`BudgetLimits`](crate::agent::BudgetLimits)
    /// ledger.
    #[error("agent run budget exhausted")]
    BudgetExhausted,

    /// The model returned a tool-use block where none is allowed.
    ///
    /// The Chat facade never executes tools, so a tool-use response is a hard
    /// error rather than a loop step (see `docs/facade-api.md` §5.3).
    #[error("model returned an unexpected tool-use block")]
    UnexpectedToolUse,

    /// A managed external delegate start was refused by the approval policy.
    ///
    /// Ordinary typed-tool denials do not use this variant: the machine feeds a
    /// denied tool result back to the model and the run continues. This variant is
    /// reserved for external delegate starts refused by `auto_deny`, a
    /// non-approving `ask` handler, or a headless `ask` with no handler (see
    /// `docs/facade-api.md` §9.2, §16).
    #[error("tool execution was denied by the approval policy")]
    ApprovalDenied,

    /// A privileged agent action was refused by the permission policy.
    ///
    /// Reserved for the managed-external and permission-bearing runtimes
    /// (`docs/facade-api.md` §9.2, §16): a
    /// [`crate::agent::InteractionKind::Permission`] request that resolves to a
    /// deny surfaces here. The default in-library machine never emits a
    /// permission interaction, so the facade denies them by default.
    #[error("a privileged action was denied by the permission policy")]
    PermissionDenied,

    /// Two tools were registered under the same name.
    ///
    /// Raised at build time when typed [`crate::facade::Tool`] values collide
    /// with each other or with an escape-hatch registry / declaration list (see
    /// `docs/facade-api.md` §7.3).
    #[error("duplicate tool name `{name}`")]
    DuplicateTool {
        /// The tool name that was registered more than once.
        name: String,
    },

    /// A managed external agent requested a run mode its runtime cannot fulfill.
    ///
    /// Raised at build time by
    /// [`ManagedExternalAgentBuilder::build`](crate::facade::ManagedExternalAgentBuilder::build)
    /// when the requested [`ExternalRunMode`](crate::facade::ExternalRunMode)
    /// needs a managed capability the target runtime does not advertise (see
    /// `docs/facade-api.md` §11.3). Rather than silently pretending a runtime
    /// supports host-tool injection or resume, construction fails fast so a host
    /// can pick a supported mode or a different runtime.
    #[error(
        "external runtime `{runtime}` does not support run mode `{mode}` \
         (missing capabilities: {missing}; capability source: {capability_source})"
    )]
    UnsupportedExternalMode {
        /// Stable label of the runtime that could not fulfill the mode.
        runtime: String,
        /// Stable label of the requested run mode.
        mode: &'static str,
        /// Comma-separated capabilities the runtime is missing for the mode.
        missing: String,
        /// Stable label of the capability view's provenance
        /// ([`CapabilitySource`](crate::facade::CapabilitySource)) the check was
        /// made against — `declared`, `supplied`, `probed`, or `negotiated` — so a
        /// host can tell a conservative static baseline apart from a verified
        /// grade.
        capability_source: &'static str,
    },

    /// A managed external agent was asked for a capability its current
    /// capability view does not support.
    ///
    /// Raised by
    /// [`ManagedExternalAgent::require_capability`](crate::facade::ManagedExternalAgent::require_capability)
    /// when a host gates a managed feature (host tools, permission bridge, …)
    /// against the agent's *currently held*
    /// [`ExternalAgentCapabilities`](crate::facade::ExternalAgentCapabilities) and
    /// the runtime does not advertise it. The check honors the view's
    /// [`CapabilitySource`](crate::facade::CapabilitySource): once
    /// [`build_with_default_session_handler`](crate::facade::ManagedExternalAgentBuilder::build_with_default_session_handler)
    /// has folded in a [`Probed`](crate::facade::CapabilitySource::Probed) grade,
    /// the judgment reflects what the live runtime actually reported rather than
    /// the conservative declared baseline (see `docs/facade-api.md` §11.3). The
    /// message names the runtime, capability, and provenance and carries no
    /// runtime output or credentials.
    #[error(
        "external runtime `{runtime}` does not support capability `{capability}` \
         (capability source: {capability_source})"
    )]
    UnsupportedExternalCapability {
        /// Stable label of the runtime that lacks the capability.
        runtime: String,
        /// Stable label of the capability that was requested.
        capability: &'static str,
        /// Stable label of the capability view's provenance
        /// ([`CapabilitySource`](crate::facade::CapabilitySource)) the check was
        /// made against — `declared`, `supplied`, `probed`, or `negotiated`.
        capability_source: &'static str,
    },

    /// A managed external delegate could not be driven to fulfill a delegation.
    ///
    /// Raised while fulfilling an `ask_<name>` external delegation (M4-2) when
    /// the managed agent has no runtime
    /// [`ExternalSessionHandler`](crate::agent::ExternalSessionHandler) attached
    /// (nothing can advance the session), or when driving the
    /// [`ExternalAgentMachine`](crate::agent::ExternalAgentMachine) fails before
    /// it reaches a terminal cursor. The facade fails fast with this variant
    /// rather than silently degrading an unconfigured or broken external
    /// delegate (see `docs/facade-api.md` §11.2, §16). The message is a stable,
    /// non-secret description; runtime output and credentials are never included.
    #[error("external agent `{name}` error: {message}")]
    ExternalAgent {
        /// The registration name of the external delegate that failed.
        name: String,
        /// A stable, non-secret description of the failure.
        message: String,
    },

    /// The facade was driven into a state its API cannot service.
    #[error("facade invalid state: {0}")]
    InvalidState(String),
}
