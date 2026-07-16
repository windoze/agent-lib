//! Internal per-step error type for
//! [`DefaultAgentMachine`](super::DefaultAgentMachine).
//!
//! [`AgentMachine::step`](crate::agent::AgentMachine::step) cannot return a
//! `Result`: a failed step must still settle the machine on a quiescent
//! [`LoopCursor::Error`](crate::agent::LoopCursor::Error). Historically every
//! fallible call inside the machine was hand-written as
//! `if let Err(error) = ... { return self.fail(format!("...: {error}")); }`.
//!
//! [`StepError`] gives the machine's internal methods a `Result` layer so those
//! branches collapse into `?`, while the single fold back into
//! `LoopCursor::Error` happens once at the outermost `step()` boundary (刀 (C),
//! migration doc §2). It is deliberately **crate-internal and never exposed on
//! the public API**; [`StepError::message`] reproduces the existing
//! `self.fail(..)` text byte-for-byte so no observable runtime semantics change.
//!
//! M1-2 wires the variants into the machine's fallible methods; the single fold
//! back into `LoopCursor::Error` at the outermost `step()` boundary lands in
//! M1-3.

use crate::{
    agent::{AgentStateError, RequirementError, ToolRuntimeError},
    conversation::ConversationError,
};

/// A failure produced while computing one machine step.
///
/// Each non-[`Protocol`](StepError::Protocol) variant wraps a typed error so the
/// corresponding fallible call can propagate with `?`. [`Protocol`](StepError::Protocol)
/// carries the pre-formatted text of a protocol/phase violation (for example a
/// resume landing on the wrong cursor or a missing in-flight scratch id) or a
/// driver-supplied result error that has no dedicated typed source.
///
/// This type is only ever folded into
/// [`LoopCursor::Error`](crate::agent::LoopCursor::Error) at the outermost
/// `step()` layer; it is not part of the public API.
#[derive(Debug)]
pub(super) enum StepError {
    /// A [`Conversation`](crate::conversation::Conversation) boundary operation
    /// failed (e.g. `begin_turn`, `finish_assistant`, `commit_pending`).
    Conversation(ConversationError),
    /// A non-cursor [`AgentState`](crate::agent::AgentState) operation failed
    /// (e.g. queued reconfig application, pivot validation).
    State(AgentStateError),
    /// A [`LoopCursor`](crate::agent::LoopCursor) transition
    /// (`transition_cursor`) was rejected. Kept distinct from
    /// [`State`](StepError::State) because it wraps the same error type but
    /// carries a different, byte-stable message prefix.
    CursorTransition(AgentStateError),
    /// Minting a tool-runtime id or folding a tool result failed.
    ToolRuntime(ToolRuntimeError),
    /// Minting the next [`RequirementId`](crate::agent::RequirementId) failed.
    Requirement(RequirementError),
    /// A protocol/phase violation or driver-supplied error already rendered to
    /// its final human-readable text; passed through verbatim.
    Protocol(String),
}

impl StepError {
    /// Renders the stable, human-readable message folded into the error cursor.
    ///
    /// The prefixes match the legacy `self.fail(format!(..))` call sites exactly
    /// so existing error-text assertions keep passing unchanged.
    pub(super) fn message(&self) -> String {
        match self {
            StepError::Conversation(error) => format!("conversation operation failed: {error}"),
            StepError::State(error) => format!("agent state operation failed: {error}"),
            StepError::CursorTransition(error) => format!("cursor transition failed: {error}"),
            StepError::ToolRuntime(error) => format!("tool runtime operation failed: {error}"),
            StepError::Requirement(error) => format!("requirement id unavailable: {error}"),
            StepError::Protocol(message) => message.clone(),
        }
    }
}

// `AgentStateError` already provides `From<ConversationError>`, so both `From`
// impls are written explicitly to keep `?` unambiguous and route each source
// type to its own variant (migration doc §2).
impl From<ConversationError> for StepError {
    fn from(error: ConversationError) -> Self {
        StepError::Conversation(error)
    }
}

impl From<AgentStateError> for StepError {
    fn from(error: AgentStateError) -> Self {
        StepError::State(error)
    }
}

impl From<ToolRuntimeError> for StepError {
    fn from(error: ToolRuntimeError) -> Self {
        StepError::ToolRuntime(error)
    }
}

impl From<RequirementError> for StepError {
    fn from(error: RequirementError) -> Self {
        StepError::Requirement(error)
    }
}
