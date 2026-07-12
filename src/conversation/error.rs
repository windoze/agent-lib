//! Classified errors produced by Conversation state transitions.

use crate::{
    conversation::{MessageId, ToolCallId, TurnId},
    model::message::Role,
};
use std::fmt;
use thiserror::Error;

/// The content-block category reported by role/content validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentBlockKind {
    /// A plain text block.
    Text,
    /// An image block.
    Image,
    /// A complete tool invocation.
    ToolUse,
    /// A complete tool result.
    ToolResult,
    /// A model thinking/reasoning block.
    Thinking,
}

impl fmt::Display for ContentBlockKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::ToolUse => "tool_use",
            Self::ToolResult => "tool_result",
            Self::Thinking => "thinking",
        };
        formatter.write_str(name)
    }
}

/// Which message reference in a [`ToolPairing`](super::ToolPairing) failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingMessageKind {
    /// The message expected to contain the tool-use block.
    Call,
    /// The message expected to contain the tool-result block.
    Result,
}

impl fmt::Display for PairingMessageKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Call => formatter.write_str("call"),
            Self::Result => formatter.write_str("result"),
        }
    }
}

/// A candidate turn failed one of the closed-history commit invariants.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CommitError {
    /// The candidate reuses a turn identity already present in the conversation.
    #[error("turn id {turn_id} already exists in this conversation")]
    DuplicateTurnId {
        /// Reused turn identity.
        turn_id: TurnId,
    },

    /// A message identity is duplicated within the candidate or committed history.
    #[error("message id {message_id} is not unique in this conversation")]
    DuplicateMessageId {
        /// Reused message identity.
        message_id: MessageId,
    },

    /// A framework tool-call identity is duplicated in the conversation.
    #[error("tool call id {call_id} is not unique in this conversation")]
    DuplicateToolCallId {
        /// Reused framework tool-call identity.
        call_id: ToolCallId,
    },

    /// A provider call id identifies more than one call or pairing in a turn.
    #[error("provider call id `{provider_call_id}` is duplicated in the turn")]
    DuplicateProviderCallId {
        /// Reused provider call id.
        provider_call_id: String,
    },

    /// More than one result block tries to consume the same provider call.
    #[error("provider call `{provider_call_id}` has more than one tool result")]
    DuplicateToolResult {
        /// Provider call id consumed more than once.
        provider_call_id: String,
        /// Message containing the first result.
        first_result_msg: MessageId,
        /// Message containing the duplicate result.
        duplicate_result_msg: MessageId,
    },

    /// A tool-result block has no tool-use block in the same turn.
    #[error(
        "tool result for provider call `{provider_call_id}` in message {result_msg} is orphaned"
    )]
    OrphanToolResult {
        /// Provider call id named by the orphan result.
        provider_call_id: String,
        /// Message containing the orphan result.
        result_msg: MessageId,
    },

    /// A tool-use block has no complete result in the same turn.
    #[error(
        "provider call `{provider_call_id}` in message {call_msg} is dangling without a result"
    )]
    DanglingProviderCall {
        /// Provider call id left open.
        provider_call_id: String,
        /// Message containing the open call.
        call_msg: MessageId,
    },

    /// A missing provider id cannot be inferred uniquely from message anchors.
    #[error("tool pairing {call_id} has no uniquely resolvable provider call id")]
    MissingProviderCallId {
        /// Framework pairing identity whose provider counterpart is ambiguous.
        call_id: ToolCallId,
    },

    /// Complete call/result content is not represented by an explicit pairing.
    #[error("provider call `{provider_call_id}` has no explicit tool pairing")]
    MissingToolPairing {
        /// Provider call id missing from the pairing table.
        provider_call_id: String,
    },

    /// A pairing names a provider call that has no tool-use content block.
    #[error("tool pairing {call_id} names unknown provider call `{provider_call_id}`")]
    OrphanToolPairing {
        /// Framework pairing identity.
        call_id: ToolCallId,
        /// Provider call id absent from call content.
        provider_call_id: String,
    },

    /// A pairing points into a previously committed turn.
    #[error("tool pairing {call_id} has a cross-turn {kind} message reference to {message_id}")]
    CrossTurnPairing {
        /// Framework pairing identity.
        call_id: ToolCallId,
        /// Whether the bad reference is the call or result side.
        kind: PairingMessageKind,
        /// Referenced message in committed history.
        message_id: MessageId,
    },

    /// A pairing points to a message that does not exist in this conversation.
    #[error("tool pairing {call_id} references unknown {kind} message {message_id}")]
    UnknownPairingMessage {
        /// Framework pairing identity.
        call_id: ToolCallId,
        /// Whether the bad reference is the call or result side.
        kind: PairingMessageKind,
        /// Unknown message identity.
        message_id: MessageId,
    },

    /// A pairing references a current-turn message other than the matching block's message.
    #[error(
        "tool pairing {call_id} points at {actual} for its {kind}, but provider call `{provider_call_id}` is in {expected}"
    )]
    PairingMessageMismatch {
        /// Framework pairing identity.
        call_id: ToolCallId,
        /// Provider call being correlated.
        provider_call_id: String,
        /// Whether the mismatch is on the call or result side.
        kind: PairingMessageKind,
        /// Message containing the matching content block.
        expected: MessageId,
        /// Message named by the pairing.
        actual: MessageId,
    },

    /// A message role contains a top-level block that the canonical model forbids.
    #[error("message {message_id} with role {role:?} cannot contain a {block} block")]
    InvalidRoleBlock {
        /// Invalid message identity.
        message_id: MessageId,
        /// Message role that cannot carry the block.
        role: Role,
        /// Rejected block category.
        block: ContentBlockKind,
    },

    /// A tool-result payload contains a block that cannot be sent by both adapters.
    #[error(
        "tool result for `{provider_call_id}` in message {message_id} cannot contain a nested {block} block"
    )]
    InvalidToolResultContent {
        /// Tool result message identity.
        message_id: MessageId,
        /// Provider call id being answered.
        provider_call_id: String,
        /// Rejected nested block category.
        block: ContentBlockKind,
    },

    /// A system-role message was placed into committed history.
    #[error("system-role message {message_id} is forbidden in a closed turn")]
    SystemRole {
        /// Forbidden system message identity.
        message_id: MessageId,
    },

    /// A tool-role message contains no result to correlate.
    #[error("tool message {message_id} contains no tool-result blocks")]
    EmptyToolMessage {
        /// Empty tool message identity.
        message_id: MessageId,
    },

    /// The turn does not start with exactly one external user message.
    #[error("closed turn must start with a user message, found {first_role:?}")]
    InvalidStartState {
        /// Actual first role, or `None` for an empty turn.
        first_role: Option<Role>,
    },

    /// The turn does not end with an assistant message free of tool calls.
    #[error(
        "closed turn has invalid end state: last role {last_role:?}, open calls={has_open_calls}"
    )]
    InvalidEndState {
        /// Actual last role, or `None` for an empty turn.
        last_role: Option<Role>,
        /// Whether provider calls remain unanswered at the end.
        has_open_calls: bool,
    },

    /// A role appears where the canonical role state machine expects another state.
    #[error("message {message_id} has unexpected role {actual:?}; expected {expected}")]
    UnexpectedRole {
        /// Message whose role is out of sequence.
        message_id: MessageId,
        /// Actual role.
        actual: Role,
        /// Human-readable expected state.
        expected: &'static str,
    },

    /// Draft-only state proves that complete Client content has not been frozen yet.
    #[error("turn contains unfinished content at {message_id:?}: {detail}")]
    IncompleteContent {
        /// Message containing unfinished content, or `None` for turn-level pending state.
        message_id: Option<MessageId>,
        /// Description of the incomplete draft state.
        detail: &'static str,
    },

    /// The candidate does not extend the current committed head.
    #[error("turn parent mismatch: expected {expected:?}, found {actual:?}")]
    ParentMismatch {
        /// Current committed head.
        expected: Option<TurnId>,
        /// Parent supplied by the candidate.
        actual: Option<TurnId>,
    },
}

/// A Conversation operation failed without changing committed state.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ConversationError {
    /// The candidate turn failed closed-turn validation.
    #[error("turn commit rejected: {0}")]
    Commit(#[from] CommitError),

    /// History and version cannot be advanced together because the version is exhausted.
    #[error("commit cannot advance history and version atomically from version {current_version}")]
    NonAtomicCommit {
        /// Exhausted current version.
        current_version: u64,
    },
}
