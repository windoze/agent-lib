//! Facade-layer error type.
//!
//! [`FacadeError`] intentionally exposes fewer variants than the underlying
//! `client`/`conversation`/`agent` errors while still preserving the original
//! failure as an error `source`. This keeps the ergonomic facade API simple
//! without discarding the diagnostic chain the lower layers already produce.

use thiserror::Error;

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

    /// The model returned a tool-use block where none is allowed.
    ///
    /// The Chat facade never executes tools, so a tool-use response is a hard
    /// error rather than a loop step (see `docs/facade-api.md` §5.3).
    #[error("model returned an unexpected tool-use block")]
    UnexpectedToolUse,

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

    /// The facade was driven into a state its API cannot service.
    #[error("facade invalid state: {0}")]
    InvalidState(String),
}
