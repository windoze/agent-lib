//! Classified errors produced by Conversation state transitions.

use crate::{
    conversation::{
        ArtifactId, ConversationId, MessageId, ToolCallId, TurnId, pending::PendingTurnPhase,
        projection::StrategyRef,
    },
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

    /// A step-boundary operation received a token that does not name the current head.
    #[error(
        "boundary after {boundary_turn_count} turns is not the current head after {head_turn_count} turns"
    )]
    NotCurrentHead {
        /// Number of complete turns claimed before the supplied boundary.
        boundary_turn_count: u64,
        /// Number of turns currently visible at the logical head.
        head_turn_count: u64,
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

/// A checked fork request was rejected before child state was created.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ForkError {
    /// A fork must receive a distinct child Conversation identity from its caller.
    #[error("fork child conversation id {conversation_id} matches its parent")]
    DuplicateConversationId {
        /// Reused Conversation identity.
        conversation_id: ConversationId,
    },
}

/// A projection range, artifact, or span set failed checked construction.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ProjectionError {
    /// A compaction plan was prepared for another Conversation.
    #[error(
        "compaction plan belongs to conversation {actual}, but this conversation is {expected}"
    )]
    CompactionOwnerMismatch {
        /// Identity required by the applying Conversation.
        expected: ConversationId,
        /// Identity embedded in the plan.
        actual: ConversationId,
    },

    /// A compaction plan was prepared against an older structural version.
    #[error(
        "compaction plan version {plan_version} is stale; current version is {current_version}"
    )]
    StaleCompactionPlan {
        /// Version embedded in the plan.
        plan_version: u64,
        /// Current Conversation structural version.
        current_version: u64,
    },

    /// A compaction plan's recorded head no longer matches the Conversation.
    #[error("compaction plan head {plan_head} does not match current head {current_head}")]
    CompactionHeadMismatch {
        /// Head size embedded in the plan.
        plan_head: u64,
        /// Current logical head size.
        current_head: u64,
    },

    /// A compaction plan was applied while the head sits below the lineage tip.
    ///
    /// Compacting a reverted head would build a projection that only covers the
    /// active prefix; redoing to the lineage tip afterwards would then silently
    /// drop the tail turns from the effective view. Redo to the lineage tip
    /// before compacting.
    #[error(
        "compaction is not allowed on a reverted head ({head} of {lineage_len} turns active); redo to the lineage tip first"
    )]
    CompactionOnRevertedHead {
        /// Current logical head size (turns at or before the head).
        head: u64,
        /// Total turns in the current lineage (the lineage tip).
        lineage_len: u64,
    },

    /// A compaction plan must contain at least one replacement step.
    #[error("compaction plan has no steps")]
    EmptyCompactionPlan,

    /// A persisted or caller-supplied range belongs to another Conversation.
    #[error(
        "projection range belongs to conversation {actual}, but this conversation is {expected}"
    )]
    RangeOwnerMismatch {
        /// Identity required by the validating Conversation.
        expected: ConversationId,
        /// Identity embedded in the range.
        actual: ConversationId,
    },

    /// Projection construction requires a committed consistency point.
    #[error("projection range cannot be checked while pending turn {turn_id} is active")]
    PendingTurn {
        /// Uncommitted transaction preventing a checked Turn range.
        turn_id: TurnId,
    },

    /// The start boundary is after the end boundary.
    #[error("projection range start {start} is after end {end}")]
    ReversedRange {
        /// Number of complete turns before the start boundary.
        start: u64,
        /// Number of complete turns before the end boundary.
        end: u64,
    },

    /// Zero-length projection ranges must be requested through an explicit API.
    #[error("projection range is empty at turn boundary {turn_count}")]
    EmptyRange {
        /// Boundary position shared by start and end.
        turn_count: u64,
    },

    /// The range includes turns beyond the current logical head.
    #[error("projection range end {end} exceeds current head {head}")]
    RangeBeyondHead {
        /// Number of turns covered by the end boundary.
        end: u64,
        /// Number of turns visible at the current logical head.
        head: u64,
    },

    /// A range endpoint cannot be represented in the current lineage.
    #[error("projection endpoint after {turn_count} turns exceeds lineage length {lineage_turns}")]
    RangePositionOutOfRange {
        /// Claimed number of complete turns before the endpoint.
        turn_count: u64,
        /// Number of turns in the current lineage.
        lineage_turns: u64,
    },

    /// A range endpoint references a raw Turn outside the current lineage.
    #[error("projection endpoint references detached turn {turn_id}")]
    DetachedTurn {
        /// Retained raw Turn that is no longer on the current lineage.
        turn_id: TurnId,
    },

    /// A range endpoint references no retained Turn.
    #[error("projection endpoint references unknown turn {turn_id}")]
    UnknownTurn {
        /// Unknown Turn identity.
        turn_id: TurnId,
    },

    /// The stable endpoint anchor no longer matches the current lineage.
    #[error(
        "projection endpoint anchor mismatch after {turn_count} turns: expected {expected:?}, found {actual:?}"
    )]
    RangeAnchorMismatch {
        /// Number of complete turns before the endpoint.
        turn_count: u64,
        /// Current lineage Turn immediately before the endpoint.
        expected: Option<TurnId>,
        /// Anchor embedded in the checked range.
        actual: Option<TurnId>,
    },

    /// Two artifacts use the same identity in one projection data set.
    #[error("projection artifact id {artifact_id} is duplicated")]
    DuplicateArtifactId {
        /// Reused artifact identity.
        artifact_id: ArtifactId,
    },

    /// An artifact must render at least one complete Client message.
    #[error("projection artifact {artifact_id} has no render messages")]
    EmptyArtifactMessages {
        /// Artifact without render content.
        artifact_id: ArtifactId,
    },

    /// A compacted span references no supplied artifact.
    #[error("projection compacted span references missing artifact {artifact_id}")]
    MissingArtifact {
        /// Artifact required by the span.
        artifact_id: ArtifactId,
    },

    /// A compacted span and its artifact disagree about the covered range.
    #[error("projection artifact {artifact_id} provenance range does not match its compacted span")]
    ArtifactRangeMismatch {
        /// Artifact whose provenance is inconsistent with the span.
        artifact_id: ArtifactId,
    },

    /// A compacted span and its artifact disagree about the producing strategy.
    #[error(
        "projection artifact {artifact_id} provenance strategy does not match its compacted span"
    )]
    ArtifactStrategyMismatch {
        /// Artifact whose provenance is inconsistent with the span.
        artifact_id: ArtifactId,
    },

    /// Projection spans left a raw Turn range undescribed.
    #[error("projection has a gap: expected next span at {expected_start}, found {actual_start}")]
    SpanGap {
        /// Boundary where the next span should have started.
        expected_start: u64,
        /// Boundary where the next span actually started.
        actual_start: u64,
    },

    /// Projection spans overlap a previously described raw Turn range.
    #[error(
        "projection spans overlap: expected next span no earlier than {expected_start}, found {actual_start}"
    )]
    SpanOverlap {
        /// First boundary not already covered.
        expected_start: u64,
        /// Boundary where the overlapping span starts.
        actual_start: u64,
    },

    /// Projection spans do not cover the complete current head range.
    #[error("projection ends at {actual_end}, but current head is {expected_end}")]
    IncompleteProjection {
        /// Current logical head.
        expected_end: u64,
        /// End boundary reached by the supplied spans.
        actual_end: u64,
    },

    /// A raw compaction target intersects an already compacted span.
    #[error("raw compaction target {start}..{end} intersects an existing compacted span")]
    CompactionTargetNotRaw {
        /// Target start boundary.
        start: u64,
        /// Target end boundary.
        end: u64,
    },

    /// A span compaction target cuts through an existing projection span.
    #[error("span compaction target {start}..{end} is not aligned to existing span boundaries")]
    CompactionTargetNotSpanAligned {
        /// Target start boundary.
        start: u64,
        /// Target end boundary.
        end: u64,
    },

    /// A compaction plan supplied an artifact that no step references.
    #[error("compaction artifact {artifact_id} is not referenced by any step")]
    UnreferencedCompactionArtifact {
        /// Supplied artifact that is not used by a replacement step.
        artifact_id: ArtifactId,
    },
}

/// A runtime compaction extension could not produce artifact data.
///
/// These errors are separate from [`ProjectionError`] because they describe
/// behavior resolution before a data-only [`CompactionPlan`](super::CompactionPlan)
/// is applied. Conversation state remains unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CompactionError {
    /// No runtime strategy registry or matching strategy instance was supplied.
    #[error("compaction strategy {strategy} is unresolved")]
    UnresolvedStrategy {
        /// Strategy reference requested by the compaction step.
        strategy: StrategyRef,
    },

    /// A resolver returned an instance whose identity does not match the request.
    #[error("compaction resolver returned strategy {actual}, but {expected} was requested")]
    StrategyReferenceMismatch {
        /// Strategy reference requested by the compaction step.
        expected: StrategyRef,
        /// Strategy reference reported by the resolved runtime instance.
        actual: StrategyRef,
    },

    /// A runtime strategy failed before producing a draft artifact.
    #[error("compaction strategy {strategy} failed: {message}")]
    StrategyFailed {
        /// Strategy that reported the failure.
        strategy: StrategyRef,
        /// Stable, caller-supplied failure detail.
        message: String,
    },
}

/// A consistency-point snapshot request was rejected before reading runtime state.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SnapshotError {
    /// Snapshotting requires a committed boundary and cannot include pending work.
    #[error("snapshot cannot be created while pending turn {turn_id} is active")]
    PendingTurn {
        /// Uncommitted transaction preventing a snapshot.
        turn_id: TurnId,
    },
}

/// A versioned snapshot could not be restored into live runtime state.
///
/// Restore errors carry a stable JSON-like path so callers can identify the
/// persisted fact that failed schema, history, turn, origin, projection, or
/// derived-runtime validation. No live [`Conversation`](super::Conversation)
/// is produced on any restore error.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum RestoreError {
    /// The snapshot schema version has no migration or restore handler.
    #[error("restore rejected {path}: unsupported schema version {actual}, expected {expected}")]
    UnsupportedSchemaVersion {
        /// JSON-like path of the rejected field.
        path: String,
        /// Version understood by this crate.
        expected: u32,
        /// Version found in the snapshot.
        actual: u32,
    },

    /// A serialized count cannot fit in this process's address space.
    #[error("restore rejected {path}: count {value} cannot be represented in memory")]
    CountOutOfRange {
        /// JSON-like path of the rejected field.
        path: String,
        /// Serialized count value.
        value: u64,
    },

    /// Two retained raw turn fact records used the same identity.
    #[error("restore rejected {path}: duplicate raw turn id {turn_id}")]
    DuplicateRawTurnId {
        /// JSON-like path of the duplicate id.
        path: String,
        /// Reused turn identity.
        turn_id: TurnId,
    },

    /// One retained raw turn points at a parent absent from the snapshot.
    #[error("restore rejected {path}: turn {turn_id} references missing parent {parent}")]
    MissingParent {
        /// JSON-like path of the invalid parent reference.
        path: String,
        /// Turn containing the parent reference.
        turn_id: TurnId,
        /// Missing parent identity.
        parent: TurnId,
    },

    /// Parent pointers form a cycle instead of an immutable tree.
    #[error("restore rejected {path}: parent cycle reaches turn {turn_id}")]
    ParentCycle {
        /// JSON-like path of the cyclic parent reference.
        path: String,
        /// Turn where the cycle was detected.
        turn_id: TurnId,
    },

    /// A retained raw turn is disconnected from the conversation's root tree.
    #[error("restore rejected {path}: raw turn {turn_id} is disconnected from root {root:?}")]
    DisconnectedRawTurn {
        /// JSON-like path of the disconnected turn.
        path: String,
        /// Disconnected raw turn identity.
        turn_id: TurnId,
        /// Expected root turn identity, if any raw history exists.
        root: Option<TurnId>,
    },

    /// A lineage entry names no retained raw turn fact.
    #[error("restore rejected {path}: lineage references unknown turn {turn_id}")]
    UnknownLineageTurn {
        /// JSON-like path of the invalid lineage entry.
        path: String,
        /// Missing retained raw turn identity.
        turn_id: TurnId,
    },

    /// A lineage repeats the same turn identity.
    #[error("restore rejected {path}: lineage repeats turn {turn_id}")]
    DuplicateLineageTurn {
        /// JSON-like path of the repeated lineage entry.
        path: String,
        /// Repeated turn identity.
        turn_id: TurnId,
    },

    /// Raw facts exist but no addressable lineage describes them.
    #[error("restore rejected {path}: raw history is non-empty but lineage is empty")]
    EmptyLineageWithRawTurns {
        /// JSON-like path of the invalid lineage field.
        path: String,
    },

    /// The active lineage is not ordered by each turn's parent pointer.
    #[error(
        "restore rejected {path}: lineage turn {turn_id} parent mismatch: expected {expected:?}, found {actual:?}"
    )]
    LineageParentMismatch {
        /// JSON-like path of the invalid lineage entry.
        path: String,
        /// Turn whose parent does not match the previous lineage entry.
        turn_id: TurnId,
        /// Expected parent identity at this lineage position.
        expected: Option<TurnId>,
        /// Parent identity stored on the turn fact.
        actual: Option<TurnId>,
    },

    /// The fork ceiling does not match the addressable lineage fact list.
    #[error(
        "restore rejected {path}: fork ceiling {fork_ceiling} does not match lineage length {lineage_len}"
    )]
    ForkCeilingMismatch {
        /// JSON-like path of the invalid ceiling field.
        path: String,
        /// Serialized fork ceiling.
        fork_ceiling: u64,
        /// Number of turn ids in the addressable lineage.
        lineage_len: u64,
    },

    /// The logical head lies beyond the addressable lineage ceiling.
    #[error("restore rejected {path}: head {head} exceeds fork ceiling {fork_ceiling}")]
    HeadOutOfRange {
        /// JSON-like path of the invalid head field.
        path: String,
        /// Serialized logical head position.
        head: u64,
        /// Largest addressable lineage position.
        fork_ceiling: u64,
    },

    /// A raw turn fact failed the canonical I1--I4 validator.
    #[error("restore rejected {path}: invalid turn fact: {source}")]
    InvalidTurn {
        /// JSON-like path of the invalid turn fact.
        path: String,
        /// Underlying closed-turn validation error.
        #[source]
        source: CommitError,
    },

    /// Fork provenance contradicts the child snapshot's own facts.
    #[error(
        "restore rejected {path}: fork origin parent matches child conversation {conversation_id}"
    )]
    ForkOriginSelfParent {
        /// JSON-like path of the invalid origin parent.
        path: String,
        /// Child conversation identity.
        conversation_id: ConversationId,
    },

    /// The stored fork boundary is not owned by the recorded parent.
    #[error(
        "restore rejected {path}: fork point belongs to conversation {actual}, expected parent {expected}"
    )]
    ForkPointOwnerMismatch {
        /// JSON-like path of the invalid fork boundary owner.
        path: String,
        /// Parent conversation recorded in the origin.
        expected: ConversationId,
        /// Conversation identity embedded in the fork boundary.
        actual: ConversationId,
    },

    /// The stored fork boundary points beyond the restored lineage.
    #[error(
        "restore rejected {path}: fork point {turn_count} exceeds lineage length {lineage_len}"
    )]
    ForkPointOutOfRange {
        /// JSON-like path of the invalid fork boundary position.
        path: String,
        /// Fork boundary turn count.
        turn_count: u64,
        /// Restored addressable lineage length.
        lineage_len: u64,
    },

    /// The fork boundary anchor does not match the restored lineage prefix.
    #[error(
        "restore rejected {path}: fork point anchor mismatch after {turn_count} turns: expected {expected:?}, found {actual:?}"
    )]
    ForkPointAnchorMismatch {
        /// JSON-like path of the invalid fork boundary anchor.
        path: String,
        /// Fork boundary turn count.
        turn_count: u64,
        /// Restored lineage anchor at that position.
        expected: Option<TurnId>,
        /// Anchor embedded in the fork boundary.
        actual: Option<TurnId>,
    },

    /// The projection overlay failed restore-time range or artifact validation.
    #[error("restore rejected {path}: invalid projection: {source}")]
    InvalidProjection {
        /// JSON-like path of the invalid projection field.
        path: String,
        /// Underlying projection validation error.
        #[source]
        source: ProjectionError,
    },

    /// A derived runtime index did not match an independent rebuild.
    #[error("restore rejected {path}: rebuilt tool-call index does not match full scan")]
    DerivedIndexMismatch {
        /// JSON-like path of the facts used to derive the index.
        path: String,
    },
}

/// DB-neutral row facts could not be decomposed or recomposed safely.
///
/// Row mapping errors describe storage-shape failures before a live
/// [`Conversation`](super::Conversation) is restored. Once rows produce a
/// [`ConversationSnapshot`](super::ConversationSnapshot), the normal
/// [`RestoreError`] validator remains responsible for closed-turn, parent-tree,
/// fork, and projection semantics.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum RowMappingError {
    /// A row belongs to another Conversation than the row set's owner.
    #[error(
        "row mapping rejected {path}: row belongs to conversation {actual}, expected {expected}"
    )]
    ConversationMismatch {
        /// JSON-like row path.
        path: String,
        /// Conversation identity owning the row set.
        expected: ConversationId,
        /// Conversation identity found on the row.
        actual: ConversationId,
    },

    /// Two rows use the same primary key.
    #[error("row mapping rejected {path}: duplicate primary key in {table}: {key}")]
    DuplicatePrimaryKey {
        /// JSON-like row path.
        path: String,
        /// Logical row table name.
        table: &'static str,
        /// Stable textual representation of the duplicated key.
        key: String,
    },

    /// Two rows use the same per-parent sequence number.
    #[error("row mapping rejected {path}: duplicate sequence {sequence} in {table}")]
    DuplicateSequence {
        /// JSON-like row path.
        path: String,
        /// Logical row table name.
        table: &'static str,
        /// Reused sequence value.
        sequence: u64,
    },

    /// A sequence list is not dense from zero.
    #[error("row mapping rejected {path}: {table} sequence expected {expected}, found {actual}")]
    SequenceGap {
        /// JSON-like row path.
        path: String,
        /// Logical row table name.
        table: &'static str,
        /// Next expected sequence value.
        expected: u64,
        /// Actual sequence value encountered.
        actual: u64,
    },

    /// A row references a Turn fact that is absent from retained raw rows.
    #[error("row mapping rejected {path}: missing turn row {turn_id}")]
    MissingTurnRow {
        /// JSON-like row path.
        path: String,
        /// Referenced Turn identity.
        turn_id: TurnId,
    },

    /// A retained Turn has no message rows to pass through the validator.
    #[error("row mapping rejected {path}: turn {turn_id} has no message rows")]
    MissingMessageRows {
        /// JSON-like row path.
        path: String,
        /// Turn identity missing message facts.
        turn_id: TurnId,
    },

    /// A row is not reachable from any retained raw Turn membership.
    #[error("row mapping rejected {path}: orphan {table} row {key}")]
    OrphanRow {
        /// JSON-like row path.
        path: String,
        /// Logical row table name.
        table: &'static str,
        /// Stable textual representation of the orphan row key.
        key: String,
    },

    /// A row has an internally inconsistent field combination.
    #[error("row mapping rejected {path}: invalid {table} row: {reason}")]
    InvalidRow {
        /// JSON-like row path.
        path: String,
        /// Logical row table name.
        table: &'static str,
        /// Stable diagnostic for the invalid field combination.
        reason: &'static str,
    },

    /// Projection rows could not be assembled into a valid data shape.
    #[error("row mapping rejected {path}: invalid projection rows: {source}")]
    InvalidProjectionRows {
        /// JSON-like row path.
        path: String,
        /// Projection data-shape error.
        #[source]
        source: ProjectionError,
    },

    /// A row insert set would need to update an existing immutable fact.
    #[error(
        "row mapping rejected {path}: existing {table} row {key} has different immutable facts"
    )]
    InsertConflict {
        /// JSON-like row path.
        path: String,
        /// Logical row table name.
        table: &'static str,
        /// Stable textual representation of the conflicting key.
        key: String,
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

    /// A fork request could not create an independent child conversation.
    #[error("fork rejected: {0}")]
    Fork(#[from] ForkError),

    /// A projection range, artifact, or span set failed checked construction.
    #[error("projection rejected: {0}")]
    Projection(#[from] ProjectionError),

    /// A runtime compaction extension failed before projection application.
    #[error("compaction runtime failed: {0}")]
    Compaction(#[from] CompactionError),

    /// A consistency-point snapshot could not be produced.
    #[error("snapshot rejected: {0}")]
    Snapshot(#[from] SnapshotError),

    /// A versioned snapshot could not be restored.
    #[error("restore rejected: {0}")]
    Restore(#[from] RestoreError),

    /// History and version cannot be advanced together because the version is exhausted.
    #[error("commit cannot advance history and version atomically from version {current_version}")]
    NonAtomicCommit {
        /// Exhausted current version.
        current_version: u64,
    },

    /// Head and version cannot be advanced together because the version is exhausted.
    #[error("head cannot move atomically from exhausted structural version {current_version}")]
    NonAtomicHeadMove {
        /// Exhausted current version.
        current_version: u64,
    },

    /// Projection and version cannot be advanced together because the version is exhausted.
    #[error(
        "projection cannot update atomically from exhausted structural version {current_version}"
    )]
    NonAtomicProjectionUpdate {
        /// Exhausted current version.
        current_version: u64,
    },
}
