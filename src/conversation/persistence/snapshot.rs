//! Versioned Conversation snapshot records.

use crate::conversation::{
    Conversation, ConversationConfig, ConversationError, ConversationId, ForkOrigin, Projection,
    SnapshotError, TurnId, turn::TurnData,
};
use serde::{Deserialize, Serialize};

/// Current JSON schema version emitted by [`Conversation::snapshot`].
///
/// Restore and migration code must inspect this value before trusting a
/// snapshot's remaining fields. M5-1 only records the versioned data shape; it
/// does not deserialize directly into a live [`Conversation`].
pub const CONVERSATION_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// A versioned, data-only record of one committed Conversation consistency point.
///
/// The snapshot contains retained raw closed Turns, the current addressable
/// lineage, logical head, structural version, fork provenance, and projection
/// overlay. It deliberately excludes pending transactions, accumulators,
/// derived tool-call indexes, shared-memory handles, clients, registries, and
/// runtime compaction strategy or trigger objects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSnapshot {
    schema_version: u32,
    id: ConversationId,
    config: ConversationConfig,
    structural_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<ForkOrigin>,
    history: ConversationSnapshotHistory,
    projection: Projection,
}

impl ConversationSnapshot {
    /// Returns the snapshot schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the Conversation identity recorded by this snapshot.
    #[must_use]
    pub const fn id(&self) -> ConversationId {
        self.id
    }

    /// Returns Conversation-level configuration kept outside message history.
    #[must_use]
    pub const fn config(&self) -> &ConversationConfig {
        &self.config
    }

    /// Returns the structural version observed at the consistency point.
    #[must_use]
    pub const fn structural_version(&self) -> u64 {
        self.structural_version
    }

    /// Returns fork provenance when the Conversation is a child branch.
    #[must_use]
    pub const fn origin(&self) -> Option<ForkOrigin> {
        self.origin
    }

    /// Returns retained raw history and lineage metadata.
    #[must_use]
    pub const fn history(&self) -> &ConversationSnapshotHistory {
        &self.history
    }

    /// Returns the non-destructive projection overlay and retained artifacts.
    #[must_use]
    pub const fn projection(&self) -> &Projection {
        &self.projection
    }

    /// Builds a snapshot from a Conversation after the pending gate has passed.
    fn from_conversation(conversation: &Conversation) -> Self {
        Self {
            schema_version: CONVERSATION_SNAPSHOT_SCHEMA_VERSION,
            id: conversation.id,
            config: conversation.config.clone(),
            structural_version: conversation.version,
            origin: conversation.origin,
            history: ConversationSnapshotHistory::from_conversation(conversation),
            projection: conversation.projection.clone(),
        }
    }
}

/// Retained raw Turn facts plus the active lineage metadata needed for restore.
///
/// `raw_turns` stores each retained raw Turn fact exactly once. `lineage_turns`
/// references those facts by stable [`TurnId`] and includes any same-lineage
/// redo suffix. `head_turn_count` clips the effective view, while
/// `fork_ceiling_turn_count` records the largest addressable lineage position
/// for this Conversation snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSnapshotHistory {
    raw_turns: Vec<TurnData>,
    lineage_turns: Vec<TurnId>,
    head_turn_count: u64,
    fork_ceiling_turn_count: u64,
}

impl ConversationSnapshotHistory {
    /// Returns the number of retained raw Turn fact records.
    #[must_use]
    pub fn raw_turn_count(&self) -> usize {
        self.raw_turns.len()
    }

    /// Iterates retained raw Turn identities in deterministic insertion order.
    pub fn raw_turn_ids(&self) -> impl Iterator<Item = TurnId> + '_ {
        self.raw_turns.iter().map(|turn| turn.id)
    }

    /// Returns the current addressable lineage as stable Turn identities.
    #[must_use]
    pub fn lineage_turn_ids(&self) -> &[TurnId] {
        &self.lineage_turns
    }

    /// Returns the number of lineage Turns visible at the logical head.
    #[must_use]
    pub const fn head_turn_count(&self) -> u64 {
        self.head_turn_count
    }

    /// Returns the largest addressable lineage position for boundary restore.
    #[must_use]
    pub const fn fork_ceiling_turn_count(&self) -> u64 {
        self.fork_ceiling_turn_count
    }

    /// Copies committed facts from the runtime history without derived caches.
    fn from_conversation(conversation: &Conversation) -> Self {
        let raw_turns = conversation
            .history
            .raw_turns()
            .into_iter()
            .map(TurnData::from)
            .collect();
        let lineage_turns = conversation
            .history
            .lineage_turns()
            .iter()
            .map(crate::conversation::Turn::id)
            .collect();
        Self {
            raw_turns,
            lineage_turns,
            head_turn_count: usize_to_u64(conversation.history.active_len()),
            fork_ceiling_turn_count: usize_to_u64(conversation.history.lineage_len()),
        }
    }
}

impl Conversation {
    /// Captures a versioned data-only snapshot at a committed consistency point.
    ///
    /// Snapshotting never cancels, finishes, or serializes pending work. If a
    /// pending turn exists, the method returns a classified error and leaves the
    /// Conversation unchanged. Derived indexes and runtime strategy/trigger
    /// handles are intentionally omitted because they can be rebuilt or
    /// resolved from the persisted facts later.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::PendingTurn`] when an uncommitted transaction
    /// is active.
    pub fn snapshot(&self) -> Result<ConversationSnapshot, ConversationError> {
        if let Some(pending) = &self.pending {
            return Err(SnapshotError::PendingTurn {
                turn_id: pending.id(),
            }
            .into());
        }
        Ok(ConversationSnapshot::from_conversation(self))
    }
}

/// Converts in-memory collection sizes to the stable snapshot integer width.
fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).expect("an in-memory history length cannot exceed u64")
}
