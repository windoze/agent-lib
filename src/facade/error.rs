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

    /// The model returned a tool-use block where none is allowed.
    ///
    /// The Chat facade never executes tools, so a tool-use response is a hard
    /// error rather than a loop step (see `docs/facade-api.md` §5.3).
    #[error("model returned an unexpected tool-use block")]
    UnexpectedToolUse,

    /// A tool call was refused by the approval policy (or by a headless run with
    /// no interaction handler to service an `ask`).
    ///
    /// Surfaced when [`crate::facade::Approval::auto_deny`] is in effect, when an
    /// `ask` handler returns a non-approving [`crate::facade::ApprovalDecision`],
    /// or when a tool requires approval in a headless run that has no handler to
    /// answer it (see `docs/facade-api.md` §9.2, §16). A denial never blocks: the
    /// run fails fast rather than waiting for input that cannot arrive.
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
         (missing capabilities: {missing})"
    )]
    UnsupportedExternalMode {
        /// Stable label of the runtime that could not fulfill the mode.
        runtime: String,
        /// Stable label of the requested run mode.
        mode: &'static str,
        /// Comma-separated capabilities the runtime is missing for the mode.
        missing: String,
    },

    /// The facade was driven into a state its API cannot service.
    #[error("facade invalid state: {0}")]
    InvalidState(String),
}
