//! Provider-neutral Conversation identity and immutable committed history.
//!
//! This module layers stable identities and Conversation-level configuration
//! around complete Client [`Message`] values.
//! It does not change the Client wire model: system instructions remain
//! configuration, while message ids remain in [`ConversationMessage`]. Closed
//! turns can enter history only through the crate-private validated commit path
//! used by the pending layer. Cancellation discards active partials and either
//! drops the whole pending turn, restores an assistant boundary with explicit
//! cancelled tool results, or validates a caller-supplied final response before
//! committing. Complete-Turn cuts use Conversation-issued [`Boundary`] tokens
//! whose owner, structural version, lineage position/anchor, fork ceiling, and
//! pending consistency state are checked before consumption. The logical head
//! can move backward or forward without deleting raw Turns; committing from a
//! reverted head creates a new parent-pointer suffix while the old branch stays
//! available to read-only raw-history queries. A checked fork creates a new
//! Conversation identity and [`ForkOrigin`] while sharing immutable prefix
//! storage; parent suffixes above the fork point stay outside the child
//! raw/debug/persistence facts. [`Projection`] is a non-destructive overlay on
//! raw history: its spans cover complete Turn ranges checked through
//! [`CheckedTurnRange`], compacted spans point at provenance-carrying
//! [`Artifact`] values, [`CompactionPlan`] stores data-only overlay rewrite
//! intent, dyn-safe [`CompactionStrategy`] and synchronous
//! [`CompactionTrigger`] values live only in the runtime layer,
//! [`Conversation::apply_compaction`] validates owner/version/head, targets,
//! artifacts, and provenance before atomically replacing the projection,
//! [`Conversation::effective_view`] renders a head-clipped Client-ready
//! committed context, and [`Conversation::pending_context`] keeps frozen
//! pending payloads separate from active partials. [`ConversationSnapshot`]
//! records committed consistency-point facts for persistence without serializing
//! pending state, derived indexes, shared-memory handles, clients, registries,
//! or runtime strategy/trigger objects; [`Conversation::restore`] revalidates
//! those facts before rebuilding runtime history and derived indexes.
//! [`ConversationRows`] can decompose that snapshot into DB-neutral rows and
//! reassemble the same data snapshot, but it does not construct live history or
//! permit updates to immutable message payload rows. Raw messages remain
//! unchanged.

pub mod boundary;
pub mod config;
pub mod error;
mod history;
pub mod id;
pub mod message;
pub mod pending;
pub mod persistence;
pub mod projection;
pub mod turn;
mod validation;

pub use boundary::{Boundary, ForkOrigin, RevertOutcome};
pub use config::ConversationConfig;
pub use error::{
    BoundaryError, CancelError, CommitError, CompactionError, ContentBlockKind, ConversationError,
    ForkError, PairingMessageKind, PendingMessageError, PendingTurnError, ProjectionError,
    RestoreError, RowMappingError, SnapshotError,
};
pub use history::{ToolCallIndex, ToolCallLocation, ToolCallLocationKind};
pub use id::{ArtifactId, ConversationId, MessageId, ToolCallId, TurnId};
pub use message::ConversationMessage;
pub use pending::{
    AssistantFinish, CANCELLED_TOOL_RESULT_TEXT, CancelDisposition, CancelOutcome,
    CancelledToolResult, FrozenMessage, PendingMessage, PendingToolCall, PendingTurn,
    PendingTurnPhase, ToolCallMapping,
};
pub use persistence::{
    ArtifactRecord, CONVERSATION_ROW_SCHEMA_VERSION, CONVERSATION_SNAPSHOT_SCHEMA_VERSION,
    ConversationLineageTurnRecord, ConversationRecord, ConversationRowInsertSet, ConversationRows,
    ConversationSnapshot, ConversationSnapshotHistory, ConversationTurnRecord, MessageRecord,
    ProjectionRecord, ProjectionSpanKind, ProjectionSpanRecord, ToolPairingRecord, TurnRecord,
};
pub use projection::{
    Artifact, ArtifactDraft, ArtifactProvenance, CheckedTurnRange, CompactCtx, CompactionInput,
    CompactionPlan, CompactionStep, CompactionStrategy, CompactionStrategyResolver,
    CompactionTarget, CompactionTrigger, CompactionTriggerOutcome, DeferredUntilBoundary,
    EffectiveView, PendingContext, Projection, Span, StrategyRef, TokenAccounting,
    materialize_compaction_plan, run_compaction_strategy,
};
pub use turn::{ToolPairing, Turn, TurnMeta, TurnResponseMeta};

use crate::{
    client::Response,
    model::{content::ContentBlock, message::Message, tool::ToolResponse},
    stream::StreamEvent,
};
use std::collections::HashSet;
use turn::TurnData;

/// One provider-neutral conversation with immutable committed turns.
///
/// A new value is empty and receives all identity and configuration from the
/// caller. Public code can inspect committed state but cannot push raw turns;
/// every pending transaction uses the same crate-private atomic commit gate.
///
/// Committed history cannot be extended through its read-only slice:
///
/// ```compile_fail
/// use agent_lib::conversation::{Conversation, ConversationConfig, ConversationId};
///
/// let id: ConversationId =
///     "018f0d9c-7b6a-7c12-8f31-1234567890ab".parse().unwrap();
/// let mut conversation = Conversation::new(id, ConversationConfig::default());
/// conversation.turns().push(todo!());
/// ```
#[derive(Debug)]
pub struct Conversation {
    id: ConversationId,
    config: ConversationConfig,
    history: history::History,
    projection: Projection,
    pending: Option<PendingTurn>,
    tool_call_index: ToolCallIndex,
    version: u64,
    origin: Option<ForkOrigin>,
}

impl Conversation {
    /// Creates an empty conversation under an externally supplied identity.
    #[must_use]
    pub fn new(id: ConversationId, config: ConversationConfig) -> Self {
        Self {
            id,
            config,
            history: history::History::new(),
            projection: Projection::default(),
            pending: None,
            tool_call_index: ToolCallIndex::default(),
            version: 0,
            origin: None,
        }
    }

    /// Returns this conversation's externally supplied stable identity.
    #[must_use]
    pub const fn id(&self) -> ConversationId {
        self.id
    }

    /// Returns configuration kept outside immutable message history.
    #[must_use]
    pub const fn config(&self) -> &ConversationConfig {
        &self.config
    }

    /// Returns the current closed lineage in order through a read-only slice.
    ///
    /// Raw nodes detached by a later branch remain available through
    /// [`raw_turn`](Self::raw_turn), but do not enter this effective view.
    #[must_use]
    pub fn turns(&self) -> &[Turn] {
        self.history.turns()
    }

    /// Returns every Turn on the current addressable lineage in order.
    ///
    /// This includes a redo suffix beyond the logical [`head`](Self::head), if
    /// the Conversation has been reverted. Use [`turns`](Self::turns) for the
    /// currently effective prefix and raw queries for detached branches.
    #[must_use]
    pub fn lineage_turns(&self) -> &[Turn] {
        self.history.lineage_turns()
    }

    /// Returns all retained raw Turns in deterministic insertion order.
    ///
    /// The returned vector contains shared read-only references. It can include
    /// detached suffixes and therefore must not be treated as the effective
    /// Conversation view; [`turns`](Self::turns) remains head-clipped.
    #[must_use]
    pub fn raw_turns(&self) -> Vec<&Turn> {
        self.history.raw_turns()
    }

    /// Finds an immutable retained raw turn, including a detached suffix.
    ///
    /// This is a debug/persistence lookup only. Returning a shared reference
    /// does not make raw history a second commit or mutation path.
    #[must_use]
    pub fn raw_turn(&self, turn_id: TurnId) -> Option<&Turn> {
        self.history.raw_turn(turn_id)
    }

    /// Returns the rebuildable tool-call index for the current lineage/pending.
    #[must_use]
    pub const fn tool_call_index(&self) -> &ToolCallIndex {
        &self.tool_call_index
    }

    /// Returns the unique uncommitted transaction through a read-only view.
    #[must_use]
    pub const fn pending(&self) -> Option<&PendingTurn> {
        self.pending.as_ref()
    }

    /// Returns the monotonic version advanced by each structural history change.
    ///
    /// Successful commits, real logical-head moves, and atomic projection
    /// compaction updates advance it. Forked children start their own version
    /// domain at zero, so parent and child [`Boundary`] owners remain distinct
    /// even when their numeric versions match.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Returns the parent Conversation and checked cut that created this fork.
    #[must_use]
    pub const fn origin(&self) -> Option<ForkOrigin> {
        self.origin
    }

    /// Begins one transaction from a complete external user payload.
    ///
    /// The caller supplies both identities. The user message remains outside
    /// committed history until a final assistant response passes
    /// [`commit_pending`](Self::commit_pending).
    pub fn begin_turn(
        &mut self,
        turn_id: TurnId,
        user_message_id: MessageId,
        user_payload: Message,
    ) -> Result<(), ConversationError> {
        if let Some(pending) = &self.pending {
            return Err(PendingTurnError::AlreadyPending {
                turn_id: pending.id(),
            }
            .into());
        }
        if self.history.contains_turn_id(turn_id) {
            return Err(PendingTurnError::DuplicateTurnId { turn_id }.into());
        }
        if self.retained_message_exists(user_message_id) {
            return Err(PendingTurnError::DuplicateMessageId {
                message_id: user_message_id,
            }
            .into());
        }

        let parent = self.history.tip_id();
        let pending = PendingTurn::new(turn_id, parent, user_message_id, user_payload)?;
        self.pending = Some(pending);
        self.refresh_pending_index();
        Ok(())
    }

    /// Starts one streaming assistant response at the current step boundary.
    pub fn start_assistant(&mut self) -> Result<(), ConversationError> {
        self.pending_mut()?.start_assistant().map_err(Into::into)
    }

    /// Starts an assistant response from complete non-streaming Client data.
    pub fn start_assistant_response(
        &mut self,
        response: Response,
    ) -> Result<(), ConversationError> {
        self.pending_mut()?
            .start_assistant_response(response)
            .map_err(Into::into)
    }

    /// Folds one normalized event into the active assistant response.
    pub fn push_assistant_event(&mut self, event: StreamEvent) -> Result<(), ConversationError> {
        self.pending_mut()?.push_assistant_event(event)
    }

    /// Freezes the active assistant response under an external message id.
    ///
    /// A tool-free response makes the transaction ready to commit. A response
    /// containing tool uses freezes first, then requires an exact call to
    /// [`register_tool_calls`](Self::register_tool_calls).
    pub fn finish_assistant(
        &mut self,
        message_id: MessageId,
    ) -> Result<AssistantFinish, ConversationError> {
        if self.retained_message_exists(message_id) {
            return Err(PendingTurnError::DuplicateMessageId { message_id }.into());
        }
        let outcome = self.pending_mut()?.finish_assistant(message_id)?;
        self.refresh_pending_index();
        Ok(outcome)
    }

    /// Registers framework identities for every tool use in the last assistant.
    ///
    /// The mapping is exact and atomic: missing, extra, duplicate, or
    /// conversation-wide reused identities leave the pending bookkeeping
    /// unchanged so the caller can retry with a corrected set.
    pub fn register_tool_calls(
        &mut self,
        mappings: Vec<ToolCallMapping>,
    ) -> Result<(), ConversationError> {
        let retained_call_ids = self
            .history
            .retained_tool_call_ids()
            .collect::<HashSet<_>>();
        self.pending_mut()?
            .register_tool_calls(mappings, &retained_call_ids)?;
        self.refresh_pending_index();
        Ok(())
    }

    /// Adds a complete tool response as one immutable tool-role message.
    ///
    /// The provider call id must name a registered open call. A call can be
    /// closed exactly once; all parallel calls must close before another
    /// assistant response can start.
    pub fn append_tool_response(
        &mut self,
        message_id: MessageId,
        response: ToolResponse,
    ) -> Result<ToolCallId, ConversationError> {
        if self.retained_message_exists(message_id) {
            return Err(PendingTurnError::DuplicateMessageId { message_id }.into());
        }
        let call_id = self
            .pending_mut()?
            .append_tool_response(message_id, response)
            .map_err(ConversationError::from)?;
        self.refresh_pending_index();
        Ok(call_id)
    }

    /// Adds one already-normalized complete tool-result content block.
    ///
    /// Non-result blocks and invalid nested tool content are rejected before
    /// pending messages or call bookkeeping change.
    pub fn append_tool_result(
        &mut self,
        message_id: MessageId,
        block: ContentBlock,
    ) -> Result<ToolCallId, ConversationError> {
        if self.retained_message_exists(message_id) {
            return Err(PendingTurnError::DuplicateMessageId { message_id }.into());
        }
        let call_id = self
            .pending_mut()?
            .append_tool_result(message_id, block)
            .map_err(ConversationError::from)?;
        self.refresh_pending_index();
        Ok(call_id)
    }

    /// Validates and atomically commits a ready pending turn.
    ///
    /// Response usage and metadata are merged into `meta` only in the draft
    /// sent through the sole M1 validator. Any phase, validation, parent, or
    /// version error leaves committed history and the pending transaction
    /// available for inspection or later cancellation.
    pub fn commit_pending(&mut self, meta: TurnMeta) -> Result<TurnId, ConversationError> {
        let data = self
            .pending
            .as_ref()
            .ok_or(PendingTurnError::NoPending)?
            .turn_data(meta)?;
        let turn_id = self.commit_draft(data)?;
        self.pending = None;
        Ok(turn_id)
    }

    /// Returns the pending transaction mutably only to this module's checked API.
    fn pending_mut(&mut self) -> Result<&mut PendingTurn, ConversationError> {
        self.pending
            .as_mut()
            .ok_or_else(|| PendingTurnError::NoPending.into())
    }

    /// Scans all retained raw branches for a conversation-wide message id.
    fn retained_message_exists(&self, message_id: MessageId) -> bool {
        self.history.contains_message_id(message_id)
    }

    /// Synchronizes only the transaction-local suffix of the derived index.
    pub(in crate::conversation) fn refresh_pending_index(&mut self) {
        self.tool_call_index.replace_pending(self.pending.as_ref());
    }

    /// Validates a complete draft and advances history/version as one operation.
    ///
    /// Every fallible precondition is checked before either field is changed.
    /// This remains crate-private so M2 pending transitions, rather than raw
    /// containers, become the public way to build committed history.
    pub(crate) fn commit_draft(&mut self, data: TurnData) -> Result<TurnId, ConversationError> {
        let next_version =
            self.version
                .checked_add(1)
                .ok_or(ConversationError::NonAtomicCommit {
                    current_version: self.version,
                })?;
        let expected_parent = self.history.tip_id();
        let turn = validation::validate_turn_data(data, self.history.raw_turns(), expected_parent)?;
        let turn_id = turn.id();
        let previous_active_len = self.history.turns().len();
        let previous_projection = self.projection.clone();

        self.history.append(turn);
        self.projection = previous_projection.extend_after_commit(
            self.id,
            self.history.turns(),
            previous_active_len,
        );
        let committed = self
            .history
            .turns()
            .last()
            .expect("an appended history has one effective tip");
        self.tool_call_index.push_committed_turn(committed);
        self.version = next_version;
        Ok(turn_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{Conversation, ConversationConfig, ConversationId, ConversationMessage, MessageId};
    use crate::model::message::{Message, Role};

    fn message(id: &str, role: Role) -> ConversationMessage {
        ConversationMessage::new(
            id.parse::<MessageId>().expect("message id"),
            Message {
                role,
                content: Vec::new(),
            },
        )
    }

    #[test]
    fn system_configuration_does_not_enter_the_payload_role_sequence() {
        let config = ConversationConfig::new(Some("Answer safely.".to_owned()));
        let history = [
            message("018f0d9c-7b6a-7c12-8f31-1234567890ad", Role::User),
            message("018f0d9c-7b6a-7c12-8f31-1234567890ae", Role::Assistant),
        ];

        let roles = history
            .iter()
            .map(|message| message.payload().role)
            .collect::<Vec<_>>();

        assert_eq!(config.system(), Some("Answer safely."));
        assert_eq!(roles, vec![Role::User, Role::Assistant]);
        assert!(!roles.contains(&Role::System));
    }

    #[test]
    fn new_conversation_is_empty_and_preserves_external_identity_and_config() {
        let id = "018f0d9c-7b6a-7c12-8f31-1234567890ac"
            .parse::<ConversationId>()
            .expect("conversation id");
        let config = ConversationConfig::new(Some("Answer safely.".to_owned()));
        let conversation = Conversation::new(id, config.clone());

        assert_eq!(conversation.id(), id);
        assert_eq!(conversation.config(), &config);
        assert!(conversation.turns().is_empty());
        assert_eq!(conversation.version(), 0);
        assert_eq!(conversation.origin(), None);
    }
}
