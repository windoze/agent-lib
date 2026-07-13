//! Classified errors produced by Conversation state transitions.

use crate::{
    conversation::{ConversationId, MessageId, ToolCallId, TurnId, pending::PendingTurnPhase},
    model::message::Role,
    stream::accumulator::AccumulatorError,
};
use std::{fmt, sync::Arc};
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

/// A pending assistant message could not advance or freeze safely.
///
/// Accumulator failures retain the original Client-layer error as a source so
/// callers can still distinguish block lifecycle, incomplete JSON, and
/// provider error events without Conversation duplicating that taxonomy.
#[derive(Clone, Debug)]
pub enum PendingMessageError {
    /// A normalized stream event violated the Client accumulator contract.
    Accumulator(Arc<AccumulatorError>),

    /// A previous accumulation or freeze error made the partial state unusable.
    Terminal,

    /// The message was already frozen and cannot produce another identity.
    AlreadyFrozen,

    /// Stream events cannot be appended to an already complete response.
    StreamEventForCompleteResponse,

    /// LLM responses must freeze as assistant messages.
    InvalidResponseRole {
        /// Role carried by the complete Client response.
        actual: Role,
    },
}

impl From<AccumulatorError> for PendingMessageError {
    fn from(source: AccumulatorError) -> Self {
        Self::Accumulator(Arc::new(source))
    }
}

impl fmt::Display for PendingMessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accumulator(source) => {
                write!(formatter, "pending message accumulation failed: {source}")
            }
            Self::Terminal => {
                formatter.write_str("pending message is terminal after a previous error")
            }
            Self::AlreadyFrozen => formatter.write_str("pending message has already been frozen"),
            Self::StreamEventForCompleteResponse => {
                formatter.write_str("a complete non-streaming response cannot accept stream events")
            }
            Self::InvalidResponseRole { actual } => write!(
                formatter,
                "pending LLM response must have assistant role, found {actual:?}"
            ),
        }
    }
}

impl std::error::Error for PendingMessageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Accumulator(source) => Some(source.as_ref()),
            Self::Terminal
            | Self::AlreadyFrozen
            | Self::StreamEventForCompleteResponse
            | Self::InvalidResponseRole { .. } => None,
        }
    }
}

impl PartialEq for PendingMessageError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Accumulator(left), Self::Accumulator(right)) => {
                left.to_string() == right.to_string()
            }
            (Self::Terminal, Self::Terminal)
            | (Self::AlreadyFrozen, Self::AlreadyFrozen)
            | (Self::StreamEventForCompleteResponse, Self::StreamEventForCompleteResponse) => true,
            (
                Self::InvalidResponseRole { actual: left },
                Self::InvalidResponseRole { actual: right },
            ) => left == right,
            _ => false,
        }
    }
}

impl Eq for PendingMessageError {}

/// A pending-turn transition was rejected without changing committed history.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PendingTurnError {
    /// A second turn was requested while another transaction is active.
    #[error("pending turn {turn_id} is already active")]
    AlreadyPending {
        /// Identity of the transaction that must finish or be cancelled first.
        turn_id: TurnId,
    },

    /// An operation requiring pending state was called at a committed boundary.
    #[error("the conversation has no pending turn")]
    NoPending,

    /// A pending turn tried to reuse a committed turn identity.
    #[error("turn id {turn_id} is already committed in this conversation")]
    DuplicateTurnId {
        /// Reused turn identity.
        turn_id: TurnId,
    },

    /// A pending message tried to reuse a frozen identity.
    #[error("message id {message_id} is not unique in this conversation")]
    DuplicateMessageId {
        /// Reused message identity.
        message_id: MessageId,
    },

    /// A mapping tried to reuse a framework call identity.
    #[error("tool call id {call_id} is not unique in this conversation")]
    DuplicateToolCallId {
        /// Reused framework identity.
        call_id: ToolCallId,
    },

    /// The first payload was not authored by the external user.
    #[error("a pending turn must begin with a user payload, found {actual:?}")]
    InvalidUserRole {
        /// Role supplied by the caller.
        actual: Role,
    },

    /// A user payload contained content forbidden by the canonical grammar.
    #[error("a pending user payload cannot contain a {block} block")]
    InvalidUserBlock {
        /// Rejected top-level content category.
        block: ContentBlockKind,
    },

    /// The operation is not legal in the pending turn's current phase.
    #[error("cannot {operation}: expected {expected}, found {actual:?}")]
    InvalidTransition {
        /// Human-readable operation being attempted.
        operation: &'static str,
        /// Human-readable phase required by the operation.
        expected: &'static str,
        /// Actual strongly typed phase.
        actual: PendingTurnPhase,
    },

    /// One assistant message repeated a provider call identity, or reused it later.
    #[error("provider call id `{provider_call_id}` is duplicated in the pending turn")]
    DuplicateProviderCallId {
        /// Reused provider identity.
        provider_call_id: String,
    },

    /// The caller supplied more than one mapping for a provider call.
    #[error("provider call `{provider_call_id}` has more than one ToolCallId mapping")]
    DuplicateToolCallMapping {
        /// Multiply mapped provider identity.
        provider_call_id: String,
    },

    /// A frozen provider call did not receive its required framework identity.
    #[error("provider call `{provider_call_id}` is missing a ToolCallId mapping")]
    MissingToolCallMapping {
        /// Unmapped provider identity.
        provider_call_id: String,
    },

    /// A mapping named no tool use in the frozen assistant response.
    #[error("ToolCallId mapping names unknown provider call `{provider_call_id}`")]
    UnknownToolCallMapping {
        /// Provider identity absent from the response awaiting mappings.
        provider_call_id: String,
    },

    /// A tool-result block did not answer any call registered in this turn.
    #[error("tool result names unknown provider call `{provider_call_id}`")]
    UnknownToolResult {
        /// Unregistered provider identity.
        provider_call_id: String,
    },

    /// A registered call already has one immutable result message.
    #[error("provider call `{provider_call_id}` already has a tool result")]
    DuplicateToolResult {
        /// Provider identity being answered twice.
        provider_call_id: String,
    },

    /// The result API received a non-result content block.
    #[error("expected a complete tool-result block, found {actual}")]
    InvalidToolResultBlock {
        /// Actual content category.
        actual: ContentBlockKind,
    },

    /// A tool result carried nested content unsupported by the canonical model.
    #[error("tool result for `{provider_call_id}` cannot contain nested {block}")]
    InvalidToolResultContent {
        /// Provider call being answered.
        provider_call_id: String,
        /// Rejected nested content category.
        block: ContentBlockKind,
    },
}

/// A pending-turn cancellation request was rejected atomically.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CancelError {
    /// Cancellation was requested at a committed boundary.
    #[error("the conversation has no pending turn to cancel")]
    NoPending,

    /// The selected disposition cannot apply to the pending phase.
    #[error("cannot {disposition}: pending turn is in phase {actual:?}")]
    InvalidTransition {
        /// Human-readable cancellation action.
        disposition: &'static str,
        /// Phase that rejected the action.
        actual: PendingTurnPhase,
    },

    /// Frozen tool-use content repeated a provider call identity.
    #[error("provider call id `{provider_call_id}` is duplicated in the pending turn")]
    DuplicateProviderCallId {
        /// Repeated provider identity.
        provider_call_id: String,
    },

    /// The caller supplied more than one synthetic result for one open call.
    #[error("provider call `{provider_call_id}` has more than one cancellation result")]
    DuplicateCancellationResult {
        /// Multiply closed provider identity.
        provider_call_id: String,
    },

    /// An open call has no caller-supplied identities for its synthetic result.
    #[error("provider call `{provider_call_id}` is missing a cancellation result")]
    MissingCancellationResult {
        /// Open provider identity missing from the cancellation request.
        provider_call_id: String,
    },

    /// A synthetic result names no currently open provider call.
    #[error(
        "cancellation result names unknown or already closed provider call `{provider_call_id}`"
    )]
    UnknownCancellationResult {
        /// Provider identity absent from the open-call set.
        provider_call_id: String,
    },

    /// A registered provider call was paired with a different framework id.
    #[error(
        "provider call `{provider_call_id}` is mapped to tool call {expected}, not supplied id {actual}"
    )]
    ToolCallIdMismatch {
        /// Provider identity whose mapping changed.
        provider_call_id: String,
        /// Existing framework identity.
        expected: ToolCallId,
        /// Conflicting caller-supplied identity.
        actual: ToolCallId,
    },

    /// A cancellation request tried to reuse a framework call identity.
    #[error("tool call id {call_id} is not unique in this conversation")]
    DuplicateToolCallId {
        /// Reused framework identity.
        call_id: ToolCallId,
    },

    /// A synthetic result or final assistant tried to reuse a message id.
    #[error("message id {message_id} is not unique in this conversation")]
    DuplicateMessageId {
        /// Reused message identity.
        message_id: MessageId,
    },

    /// The complete final response could not cross the assistant freeze boundary.
    #[error("cancel final assistant could not freeze: {source}")]
    InvalidFinalResponse {
        /// Existing pending-message classification retained as the source.
        #[source]
        source: PendingMessageError,
    },

    /// A cancellation commit tried to end with another tool invocation.
    #[error("cancel final assistant contains tool use `{provider_call_id}`")]
    FinalAssistantContainsToolUse {
        /// Provider call that would leave the turn open again.
        provider_call_id: String,
    },
}

/// A boundary token was unknown or invalid for the current Conversation state.
///
/// Validation is deliberately Conversation-relative: serde can recover the
/// token fields, but only the owning Conversation can prove their lineage,
/// structural version, and consistency-point meaning.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BoundaryError {
    /// The token belongs to another Conversation identity.
    #[error("boundary belongs to conversation {actual}, but this conversation is {expected}")]
    OwnerMismatch {
        /// Identity required by the validating Conversation.
        expected: ConversationId,
        /// Identity embedded in the supplied token.
        actual: ConversationId,
    },

    /// The Conversation structure changed after this token was issued.
    #[error(
        "boundary was issued at structural version {boundary_version}, current version is {current_version}"
    )]
    StaleBoundary {
        /// Structural version embedded in the token.
        boundary_version: u64,
        /// Structural version currently owned by the Conversation.
        current_version: u64,
    },

    /// Boundary-consuming operations require a committed consistency point.
    #[error("boundary cannot be consumed while pending turn {turn_id} is active")]
    PendingTurn {
        /// Uncommitted transaction preventing a history cut.
        turn_id: TurnId,
    },

    /// A token position exceeds even the immutable backing lineage.
    #[error("boundary after {turn_count} turns exceeds backing lineage length {backing_turns}")]
    PositionOutOfRange {
        /// Number of complete turns claimed before the boundary.
        turn_count: u64,
        /// Number of turns available in the backing lineage.
        backing_turns: u64,
    },

    /// A token addresses a parent suffix hidden above a fork's ceiling.
    #[error("boundary after {turn_count} turns exceeds this fork's lineage ceiling {fork_ceiling}")]
    BeyondForkCeiling {
        /// Number of complete turns claimed before the boundary.
        turn_count: u64,
        /// Largest inherited lineage position this fork may address.
        fork_ceiling: u64,
    },

    /// The stable Turn anchor does not match the token's lineage position.
    #[error(
        "boundary anchor mismatch after {turn_count} turns: expected {expected:?}, found {actual:?}"
    )]
    AnchorMismatch {
        /// Number of complete turns before the boundary.
        turn_count: u64,
        /// Turn that actually precedes this lineage position.
        expected: Option<TurnId>,
        /// Turn anchor embedded in the supplied token.
        actual: Option<TurnId>,
    },

    /// `boundary_after` was asked about a Turn absent from retained raw history.
    #[error("turn {turn_id} is unknown to this conversation")]
    UnknownTurn {
        /// Unknown Turn identity.
        turn_id: TurnId,
    },

    /// The Turn is retained for debugging but belongs to another lineage.
    #[error("turn {turn_id} is retained but not on the current lineage")]
    TurnNotOnLineage {
        /// Detached raw Turn identity.
        turn_id: TurnId,
    },
}

/// A Conversation operation failed without changing committed state.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ConversationError {
    /// A boundary token failed owner/version/lineage consistency checks.
    #[error("boundary operation rejected: {0}")]
    Boundary(#[from] BoundaryError),

    /// The candidate turn failed closed-turn validation.
    #[error("turn commit rejected: {0}")]
    Commit(#[from] CommitError),

    /// A partial or complete assistant response could not freeze safely.
    #[error("pending message operation failed: {0}")]
    PendingMessage(
        #[from]
        #[source]
        PendingMessageError,
    ),

    /// A pending turn could not begin or advance in its current state.
    #[error("pending turn operation failed: {0}")]
    PendingTurn(#[from] PendingTurnError),

    /// A pending cancellation could not be prepared or applied atomically.
    #[error("pending turn cancellation failed: {0}")]
    Cancel(#[from] CancelError),

    /// History and version cannot be advanced together because the version is exhausted.
    #[error("commit cannot advance history and version atomically from version {current_version}")]
    NonAtomicCommit {
        /// Exhausted current version.
        current_version: u64,
    },
}
