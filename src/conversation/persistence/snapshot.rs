//! Versioned Conversation snapshot records.

use crate::conversation::{
    Conversation, ConversationConfig, ConversationError, ConversationId, ForkOrigin, Projection,
    RestoreError, SnapshotError, ToolCallIndex, Turn, TurnId, history::History, turn::TurnData,
    validation,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    convert::TryFrom,
};

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

    /// Rebuilds a snapshot from already checked persistence rows.
    pub(crate) fn from_parts(
        schema_version: u32,
        id: ConversationId,
        config: ConversationConfig,
        structural_version: u64,
        origin: Option<ForkOrigin>,
        history: ConversationSnapshotHistory,
        projection: Projection,
    ) -> Self {
        Self {
            schema_version,
            id,
            config,
            structural_version,
            origin,
            history,
            projection,
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

    /// Returns retained raw Turn DTOs for sibling persistence modules.
    pub(crate) fn raw_turns(&self) -> &[TurnData] {
        &self.raw_turns
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

    /// Rebuilds history metadata from already grouped persistence rows.
    pub(crate) fn from_parts(
        raw_turns: Vec<TurnData>,
        lineage_turns: Vec<TurnId>,
        head_turn_count: u64,
        fork_ceiling_turn_count: u64,
    ) -> Self {
        Self {
            raw_turns,
            lineage_turns,
            head_turn_count,
            fork_ceiling_turn_count,
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
    /// ```
    /// use agent_lib::{
    ///     conversation::{
    ///         Conversation, ConversationConfig, ConversationError, ConversationId, MessageId,
    ///         SnapshotError, TurnId,
    ///     },
    ///     model::message::{Message, Role},
    /// };
    ///
    /// let conversation_id: ConversationId =
    ///     "018f0d9c-7b6a-7c12-8f31-1234567890ab".parse().unwrap();
    /// let turn_id: TurnId =
    ///     "018f0d9c-7b6a-7c12-8f31-1234567890ac".parse().unwrap();
    /// let user_message_id: MessageId =
    ///     "018f0d9c-7b6a-7c12-8f31-1234567890ad".parse().unwrap();
    /// let mut conversation = Conversation::new(conversation_id, ConversationConfig::default());
    /// conversation
    ///     .begin_turn(
    ///         turn_id,
    ///         user_message_id,
    ///         Message {
    ///             role: Role::User,
    ///             content: Vec::new(),
    ///         },
    ///     )
    ///     .unwrap();
    ///
    /// assert!(matches!(
    ///     conversation.snapshot(),
    ///     Err(ConversationError::Snapshot(SnapshotError::PendingTurn { .. }))
    /// ));
    /// ```
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

    /// Restores a live Conversation from a versioned, data-only snapshot.
    ///
    /// Restore never deserializes directly into runtime state. It first checks
    /// schema version, raw turn facts, parent pointers, lineage/head/ceiling,
    /// fork provenance, projection ranges, and artifact provenance. Only after
    /// all facts pass does it rebuild shared history nodes and the derived
    /// [`ToolCallIndex`]. Pending transactions are intentionally absent from
    /// snapshots and are never restored.
    ///
    /// # Errors
    ///
    /// Returns a path-carrying [`RestoreError`] for unsupported schema
    /// versions, invalid raw history, illegal closed turns, inconsistent fork
    /// provenance, invalid projection facts, or derived index mismatch.
    pub fn restore(snapshot: ConversationSnapshot) -> Result<Self, ConversationError> {
        Self::try_from(snapshot)
    }
}

impl TryFrom<ConversationSnapshot> for Conversation {
    type Error = ConversationError;

    fn try_from(snapshot: ConversationSnapshot) -> Result<Self, Self::Error> {
        match snapshot.schema_version {
            CONVERSATION_SNAPSHOT_SCHEMA_VERSION => restore_v1(snapshot).map_err(Into::into),
            actual => Err(RestoreError::UnsupportedSchemaVersion {
                path: "$.schema_version".to_owned(),
                expected: CONVERSATION_SNAPSHOT_SCHEMA_VERSION,
                actual,
            }
            .into()),
        }
    }
}

/// Restores the current schema version after the version gate has matched.
fn restore_v1(snapshot: ConversationSnapshot) -> Result<Conversation, RestoreError> {
    let validated_turns = validate_raw_turns(&snapshot.history.raw_turns)?;
    let raw_index = raw_turn_index(&validated_turns);
    validate_parent_graph(&validated_turns, &raw_index)?;
    validate_lineage(&snapshot.history, &raw_index)?;
    validate_origin(snapshot.id, snapshot.origin, &snapshot.history)?;

    let active_len = u64_to_usize(
        snapshot.history.head_turn_count,
        "$.history.head_turn_count",
    )?;
    let history =
        History::from_restored(validated_turns, &snapshot.history.lineage_turns, active_len);
    let tool_call_index = rebuild_tool_call_index(history.turns())?;
    let mut conversation = Conversation {
        id: snapshot.id,
        config: snapshot.config,
        history,
        projection: Projection::default(),
        pending: None,
        tool_call_index,
        version: snapshot.structural_version,
        origin: snapshot.origin,
    };

    conversation
        .validate_restored_projection(&snapshot.projection)
        .map_err(|source| RestoreError::InvalidProjection {
            path: "$.projection".to_owned(),
            source,
        })?;
    conversation.projection = snapshot.projection;

    Ok(conversation)
}

/// Validates every raw turn through the same I1--I4 gate used by commits.
fn validate_raw_turns(raw_turns: &[TurnData]) -> Result<Vec<Turn>, RestoreError> {
    let mut retained = Vec::with_capacity(raw_turns.len());
    let mut seen_turns = HashSet::with_capacity(raw_turns.len());

    for (index, data) in raw_turns.iter().cloned().enumerate() {
        let path = format!("$.history.raw_turns[{index}]");
        if !seen_turns.insert(data.id) {
            return Err(RestoreError::DuplicateRawTurnId {
                path: format!("{path}.id"),
                turn_id: data.id,
            });
        }
        let expected_parent = data.parent;
        let turn = validation::validate_turn_data(data, retained.iter(), expected_parent)
            .map_err(|source| RestoreError::InvalidTurn { path, source })?;
        retained.push(turn);
    }

    Ok(retained)
}

/// Builds an id lookup over validated raw turn facts.
fn raw_turn_index(turns: &[Turn]) -> HashMap<TurnId, usize> {
    turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.id(), index))
        .collect()
}

/// Validates parent existence, acyclicity, and single-root connectivity.
fn validate_parent_graph(
    turns: &[Turn],
    raw_index: &HashMap<TurnId, usize>,
) -> Result<(), RestoreError> {
    for (index, turn) in turns.iter().enumerate() {
        if let Some(parent) = turn.parent()
            && !raw_index.contains_key(&parent)
        {
            return Err(RestoreError::MissingParent {
                path: format!("$.history.raw_turns[{index}].parent"),
                turn_id: turn.id(),
                parent,
            });
        }
    }

    let mut marks = vec![VisitMark::Unvisited; turns.len()];
    for index in 0..turns.len() {
        visit_parent(index, turns, raw_index, &mut marks)?;
    }

    let root = turns
        .iter()
        .find(|turn| turn.parent().is_none())
        .map(Turn::id);
    for (index, turn) in turns.iter().enumerate() {
        if root_of(turn, turns, raw_index) != root {
            return Err(RestoreError::DisconnectedRawTurn {
                path: format!("$.history.raw_turns[{index}].parent"),
                turn_id: turn.id(),
                root,
            });
        }
    }

    Ok(())
}

/// DFS mark used while checking parent-pointer cycles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VisitMark {
    Unvisited,
    Visiting,
    Done,
}

/// Recursively checks parent pointers for cycles.
fn visit_parent(
    index: usize,
    turns: &[Turn],
    raw_index: &HashMap<TurnId, usize>,
    marks: &mut [VisitMark],
) -> Result<(), RestoreError> {
    match marks[index] {
        VisitMark::Done => return Ok(()),
        VisitMark::Visiting => {
            return Err(RestoreError::ParentCycle {
                path: format!("$.history.raw_turns[{index}].parent"),
                turn_id: turns[index].id(),
            });
        }
        VisitMark::Unvisited => {}
    }

    marks[index] = VisitMark::Visiting;
    if let Some(parent) = turns[index].parent()
        && let Some(parent_index) = raw_index.get(&parent).copied()
    {
        visit_parent(parent_index, turns, raw_index, marks)?;
    }
    marks[index] = VisitMark::Done;
    Ok(())
}

/// Returns the root reached by following a turn's parent pointers.
fn root_of(turn: &Turn, turns: &[Turn], raw_index: &HashMap<TurnId, usize>) -> Option<TurnId> {
    let mut current = turn;
    while let Some(parent) = current.parent() {
        let parent_index = raw_index
            .get(&parent)
            .expect("parent existence was checked before root lookup");
        current = &turns[*parent_index];
    }
    Some(current.id())
}

/// Validates the addressable lineage, logical head, and fork ceiling.
fn validate_lineage(
    history: &ConversationSnapshotHistory,
    raw_index: &HashMap<TurnId, usize>,
) -> Result<(), RestoreError> {
    let lineage_len = history.lineage_turns.len();
    let fork_ceiling = u64_to_usize(
        history.fork_ceiling_turn_count,
        "$.history.fork_ceiling_turn_count",
    )?;
    if fork_ceiling != lineage_len {
        return Err(RestoreError::ForkCeilingMismatch {
            path: "$.history.fork_ceiling_turn_count".to_owned(),
            fork_ceiling: history.fork_ceiling_turn_count,
            lineage_len: usize_to_u64(lineage_len),
        });
    }

    let head = u64_to_usize(history.head_turn_count, "$.history.head_turn_count")?;
    if head > fork_ceiling {
        return Err(RestoreError::HeadOutOfRange {
            path: "$.history.head_turn_count".to_owned(),
            head: history.head_turn_count,
            fork_ceiling: history.fork_ceiling_turn_count,
        });
    }
    if !history.raw_turns.is_empty() && history.lineage_turns.is_empty() {
        return Err(RestoreError::EmptyLineageWithRawTurns {
            path: "$.history.lineage_turns".to_owned(),
        });
    }

    let mut seen = HashSet::with_capacity(lineage_len);
    for (index, turn_id) in history.lineage_turns.iter().copied().enumerate() {
        let path = format!("$.history.lineage_turns[{index}]");
        if !seen.insert(turn_id) {
            return Err(RestoreError::DuplicateLineageTurn { path, turn_id });
        }
        let Some(raw_turn_index) = raw_index.get(&turn_id).copied() else {
            return Err(RestoreError::UnknownLineageTurn { path, turn_id });
        };
        let expected = index
            .checked_sub(1)
            .and_then(|previous| history.lineage_turns.get(previous))
            .copied();
        let actual = history.raw_turns[raw_turn_index].parent;
        if actual != expected {
            return Err(RestoreError::LineageParentMismatch {
                path,
                turn_id,
                expected,
                actual,
            });
        }
    }

    Ok(())
}

/// Validates fork provenance against the restored child facts.
fn validate_origin(
    conversation_id: ConversationId,
    origin: Option<ForkOrigin>,
    history: &ConversationSnapshotHistory,
) -> Result<(), RestoreError> {
    let Some(origin) = origin else {
        return Ok(());
    };

    if origin.parent() == conversation_id {
        return Err(RestoreError::ForkOriginSelfParent {
            path: "$.origin.parent".to_owned(),
            conversation_id,
        });
    }

    let fork_point = origin.fork_point();
    if fork_point.conversation_id() != origin.parent() {
        return Err(RestoreError::ForkPointOwnerMismatch {
            path: "$.origin.fork_point.conversation_id".to_owned(),
            expected: origin.parent(),
            actual: fork_point.conversation_id(),
        });
    }

    let lineage_len = history.lineage_turns.len();
    let position = u64_to_usize(fork_point.turn_count(), "$.origin.fork_point.turn_count")?;
    if position > lineage_len {
        return Err(RestoreError::ForkPointOutOfRange {
            path: "$.origin.fork_point.turn_count".to_owned(),
            turn_count: fork_point.turn_count(),
            lineage_len: usize_to_u64(lineage_len),
        });
    }

    let expected = position
        .checked_sub(1)
        .and_then(|index| history.lineage_turns.get(index))
        .copied();
    if fork_point.after_turn() != expected {
        return Err(RestoreError::ForkPointAnchorMismatch {
            path: "$.origin.fork_point.after_turn".to_owned(),
            turn_count: fork_point.turn_count(),
            expected,
            actual: fork_point.after_turn(),
        });
    }

    Ok(())
}

/// Rebuilds and cross-checks the derived tool-call index from closed facts.
fn rebuild_tool_call_index(turns: &[Turn]) -> Result<ToolCallIndex, RestoreError> {
    let rebuilt = ToolCallIndex::rebuild(turns, None);
    let full_scan = ToolCallIndex::rebuild(turns, None);
    if rebuilt != full_scan {
        return Err(RestoreError::DerivedIndexMismatch {
            path: "$.history.raw_turns".to_owned(),
        });
    }
    Ok(rebuilt)
}

/// Converts in-memory collection sizes to the stable snapshot integer width.
fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).expect("an in-memory history length cannot exceed u64")
}

/// Converts stable snapshot counts to in-memory collection indices.
fn u64_to_usize(value: u64, path: &'static str) -> Result<usize, RestoreError> {
    usize::try_from(value).map_err(|_| RestoreError::CountOutOfRange {
        path: path.to_owned(),
        value,
    })
}
