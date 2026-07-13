//! Rebuildable tool-call lookup acceleration derived from Conversation facts.

use crate::{
    conversation::{
        MessageId, PendingTurn, PendingTurnPhase, ToolCallId, ToolPairing, Turn, TurnId,
    },
    model::content::ContentBlock,
};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

/// Whether a tool-call location comes from closed history or current pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolCallLocationKind {
    /// The call and its result belong to a validator-certified closed turn.
    Committed,
    /// The call belongs to the unique pending turn and may still be open.
    Pending,
}

/// Derived coordinates for one provider tool call.
///
/// An unmapped pending call has no framework [`ToolCallId`] yet. Closed calls
/// always have both framework identity and result message; the optional fields
/// let one read the same index while a pending transaction advances.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallLocation {
    turn_position: usize,
    kind: ToolCallLocationKind,
    turn_id: TurnId,
    call_id: Option<ToolCallId>,
    provider_call_id: String,
    call_message_id: MessageId,
    result_message_id: Option<MessageId>,
}

impl ToolCallLocation {
    /// Returns whether this record is closed or still transaction-local.
    #[must_use]
    pub const fn kind(&self) -> ToolCallLocationKind {
        self.kind
    }

    /// Returns the turn that owns the call.
    #[must_use]
    pub const fn turn_id(&self) -> TurnId {
        self.turn_id
    }

    /// Returns the framework identity once pending mapping has occurred.
    #[must_use]
    pub const fn call_id(&self) -> Option<ToolCallId> {
        self.call_id
    }

    /// Returns the provider identity stored in tool-use/result content.
    #[must_use]
    pub fn provider_call_id(&self) -> &str {
        &self.provider_call_id
    }

    /// Returns the immutable assistant message containing the tool use.
    #[must_use]
    pub const fn call_message_id(&self) -> MessageId {
        self.call_message_id
    }

    /// Returns the immutable tool message after the call has closed.
    #[must_use]
    pub const fn result_message_id(&self) -> Option<MessageId> {
        self.result_message_id
    }
}

/// A non-serialized acceleration structure derived from current facts.
///
/// Framework call ids are conversation-wide unique. Provider ids are not
/// assumed to be unique across turns, so provider lookup yields every matching
/// location in current-lineage order. This value is never a pairing source of
/// truth: it can always be rebuilt from closed turns plus the current pending
/// transaction.
#[derive(Clone)]
pub struct ToolCallIndex {
    committed: Arc<CommittedIndex>,
    visible_committed_turns: usize,
    visible_committed_entries: usize,
    pending: Vec<ToolCallLocation>,
    pending_by_call_id: HashMap<ToolCallId, usize>,
    pending_by_provider_call_id: HashMap<String, Vec<usize>>,
}

impl ToolCallIndex {
    /// Returns the number of calls visible in the current lineage and pending.
    #[must_use]
    pub fn len(&self) -> usize {
        self.visible_committed_entries + self.pending.len()
    }

    /// Reports whether the current lineage and pending contain no tool calls.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterates through committed calls and then pending calls in message order.
    pub fn iter(&self) -> impl Iterator<Item = &ToolCallLocation> {
        self.committed.entries[..self.visible_committed_entries]
            .iter()
            .chain(self.pending.iter())
    }

    /// Finds one mapped call by its framework-owned stable identity.
    #[must_use]
    pub fn by_call_id(&self, call_id: ToolCallId) -> Option<&ToolCallLocation> {
        if let Some(index) = self.committed.by_call_id.get(&call_id)
            && *index < self.visible_committed_entries
        {
            return Some(&self.committed.entries[*index]);
        }
        self.pending_by_call_id
            .get(&call_id)
            .map(|index| &self.pending[*index])
    }

    /// Finds every current call carrying the provider id.
    ///
    /// Multiple results are possible because provider call ids are scoped to
    /// provider interactions rather than globally assigned by Conversation.
    pub fn by_provider_call_id<'a>(
        &'a self,
        provider_call_id: &str,
    ) -> impl Iterator<Item = &'a ToolCallLocation> + 'a {
        let committed = self
            .committed
            .by_provider_call_id
            .get(provider_call_id)
            .into_iter()
            .flatten()
            .filter(|index| **index < self.visible_committed_entries)
            .map(|index| &self.committed.entries[*index]);
        let pending = self
            .pending_by_provider_call_id
            .get(provider_call_id)
            .into_iter()
            .flatten()
            .map(|index| &self.pending[*index]);
        committed.chain(pending)
    }

    /// Reconstructs the entire derived index from authoritative facts.
    ///
    /// The supplied turns must be in current-lineage order. Rebuilding never
    /// changes those facts and is useful after head, fork, or restore changes;
    /// the result remains an acceleration value rather than validation proof.
    #[must_use]
    pub fn rebuild(turns: &[Turn], pending: Option<&PendingTurn>) -> Self {
        let committed = Arc::new(CommittedIndex::from_turns(turns));
        let visible_committed_entries = committed.entries.len();
        let mut index = Self {
            committed,
            visible_committed_turns: turns.len(),
            visible_committed_entries,
            pending: Vec::new(),
            pending_by_call_id: HashMap::new(),
            pending_by_provider_call_id: HashMap::new(),
        };
        index.replace_pending(pending);
        index
    }

    /// Adds one newly committed turn without rescanning older closed turns.
    pub(crate) fn push_committed_turn(&mut self, turn: &Turn) {
        self.clear_pending();
        let turn_position = self.visible_committed_turns;
        if self.visible_committed_turns == self.committed.turn_end_entries.len()
            && Arc::strong_count(&self.committed) == 1
        {
            let committed = Arc::make_mut(&mut self.committed);
            committed.push_turn(turn, turn_position);
        } else {
            let mut committed = self
                .committed
                .visible_prefix(self.visible_committed_turns, self.visible_committed_entries);
            committed.push_turn(turn, turn_position);
            self.committed = Arc::new(committed);
        }
        self.visible_committed_turns += 1;
        self.visible_committed_entries = self
            .committed
            .entry_count_for_turns(self.visible_committed_turns)
            .expect("the appended turn count is present in the committed index");
    }

    /// Replaces only transaction-local records after a pending transition.
    pub(crate) fn replace_pending(&mut self, pending: Option<&PendingTurn>) {
        self.clear_pending();
        if let Some(pending) = pending {
            self.extend_pending(pending_locations(pending));
        }
    }

    /// Moves the visible committed prefix without rebuilding the shared index.
    pub(crate) fn scope_committed_turns(&mut self, turn_count: usize) {
        self.visible_committed_entries = self
            .committed
            .entry_count_for_turns(turn_count)
            .expect("lineage position has a committed index entry count");
        self.visible_committed_turns = turn_count;
        self.clear_pending();
    }

    /// Creates an independent index view over a shared committed prefix.
    pub(crate) fn fork_scope(&self, turn_count: usize) -> Option<Self> {
        let visible_committed_entries = self.committed.entry_count_for_turns(turn_count)?;
        Some(Self {
            committed: self.committed.clone(),
            visible_committed_turns: turn_count,
            visible_committed_entries,
            pending: Vec::new(),
            pending_by_call_id: HashMap::new(),
            pending_by_provider_call_id: HashMap::new(),
        })
    }

    /// Reports whether two indexes share the same committed backing allocation.
    #[cfg(test)]
    pub(crate) fn committed_ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.committed, &other.committed)
    }

    /// Returns the committed turn ceiling currently visible through this index.
    #[cfg(test)]
    pub(crate) const fn visible_committed_turns(&self) -> usize {
        self.visible_committed_turns
    }

    /// Adds pending records while maintaining transaction-local lookup tables.
    fn extend_pending(&mut self, locations: impl IntoIterator<Item = ToolCallLocation>) {
        for location in locations {
            let index = self.pending.len();
            if let Some(call_id) = location.call_id {
                debug_assert!(self.by_call_id(call_id).is_none());
                let previous = self.pending_by_call_id.insert(call_id, index);
                debug_assert!(previous.is_none(), "pending framework call ids are unique");
            }
            self.pending_by_provider_call_id
                .entry(location.provider_call_id.clone())
                .or_default()
                .push(index);
            self.pending.push(location);
        }
    }

    /// Removes transaction-local records while preserving shared committed data.
    fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_by_call_id.clear();
        self.pending_by_provider_call_id.clear();
    }
}

impl Default for ToolCallIndex {
    fn default() -> Self {
        Self {
            committed: Arc::new(CommittedIndex::default()),
            visible_committed_turns: 0,
            visible_committed_entries: 0,
            pending: Vec::new(),
            pending_by_call_id: HashMap::new(),
            pending_by_provider_call_id: HashMap::new(),
        }
    }
}

impl fmt::Debug for ToolCallIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCallIndex")
            .field("visible_committed_turns", &self.visible_committed_turns)
            .field("entries", &self.iter().collect::<Vec<_>>())
            .finish()
    }
}

impl PartialEq for ToolCallIndex {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl Eq for ToolCallIndex {}

/// Shared committed-call index for one addressable lineage.
#[derive(Clone, Debug, Default)]
struct CommittedIndex {
    entries: Vec<ToolCallLocation>,
    by_call_id: HashMap<ToolCallId, usize>,
    by_provider_call_id: HashMap<String, Vec<usize>>,
    turn_end_entries: Vec<usize>,
}

impl CommittedIndex {
    /// Builds a complete committed index from already-ordered lineage turns.
    fn from_turns(turns: &[Turn]) -> Self {
        let mut index = Self::default();
        for (turn_position, turn) in turns.iter().enumerate() {
            index.push_turn(turn, turn_position);
        }
        index
    }

    /// Copies only the currently visible prefix when a new branch is committed.
    fn visible_prefix(&self, visible_turns: usize, visible_entries: usize) -> Self {
        debug_assert!(visible_turns <= self.turn_end_entries.len());
        debug_assert!(visible_entries <= self.entries.len());
        let mut index = Self {
            entries: self.entries[..visible_entries].to_vec(),
            by_call_id: HashMap::new(),
            by_provider_call_id: HashMap::new(),
            turn_end_entries: self.turn_end_entries[..visible_turns].to_vec(),
        };
        index.rebuild_lookup_tables();
        index
    }

    /// Adds every tool call from one committed turn at a known lineage position.
    fn push_turn(&mut self, turn: &Turn, turn_position: usize) {
        for location in turn_locations(turn, turn_position) {
            let index = self.entries.len();
            if let Some(call_id) = location.call_id {
                let previous = self.by_call_id.insert(call_id, index);
                debug_assert!(
                    previous.is_none(),
                    "validated framework call ids are unique"
                );
            }
            self.by_provider_call_id
                .entry(location.provider_call_id.clone())
                .or_default()
                .push(index);
            self.entries.push(location);
        }
        self.turn_end_entries.push(self.entries.len());
    }

    /// Returns how many committed call entries fall inside `turn_count` turns.
    fn entry_count_for_turns(&self, turn_count: usize) -> Option<usize> {
        if turn_count == 0 {
            return Some(0);
        }
        self.turn_end_entries.get(turn_count - 1).copied()
    }

    /// Rebuilds lookup maps after prefix copying.
    fn rebuild_lookup_tables(&mut self) {
        self.by_call_id.clear();
        self.by_provider_call_id.clear();
        for (index, location) in self.entries.iter().enumerate() {
            if let Some(call_id) = location.call_id {
                let previous = self.by_call_id.insert(call_id, index);
                debug_assert!(
                    previous.is_none(),
                    "validated framework call ids are unique"
                );
            }
            self.by_provider_call_id
                .entry(location.provider_call_id.clone())
                .or_default()
                .push(index);
        }
    }
}

/// Derives complete call locations from one validator-certified turn.
fn turn_locations(turn: &Turn, turn_position: usize) -> Vec<ToolCallLocation> {
    turn.pairings()
        .iter()
        .map(|pairing| ToolCallLocation {
            turn_position,
            kind: ToolCallLocationKind::Committed,
            turn_id: turn.id(),
            call_id: Some(pairing.call_id()),
            provider_call_id: resolved_provider_call_id(turn, pairing),
            call_message_id: pairing.call_msg(),
            result_message_id: Some(pairing.result_msg()),
        })
        .collect()
}

/// Resolves the optional persisted provider id from certified message anchors.
fn resolved_provider_call_id(turn: &Turn, pairing: &ToolPairing) -> String {
    if let Some(provider_call_id) = pairing.provider_call_id() {
        return provider_call_id.to_owned();
    }

    let result_ids = turn
        .messages()
        .iter()
        .find(|message| message.id() == pairing.result_msg())
        .into_iter()
        .flat_map(|message| &message.payload().content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            ContentBlock::Text { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::ToolUse { .. }
            | ContentBlock::Thinking { .. } => None,
        })
        .collect::<HashSet<_>>();
    let mut candidates = turn
        .messages()
        .iter()
        .find(|message| message.id() == pairing.call_msg())
        .into_iter()
        .flat_map(|message| &message.payload().content)
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } if result_ids.contains(id.as_str()) => {
                Some(id.as_str())
            }
            ContentBlock::Text { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::ToolUse { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Thinking { .. } => None,
        });
    let provider_call_id = candidates
        .next()
        .expect("validated optional provider id has one anchored content match");
    debug_assert!(candidates.next().is_none());
    provider_call_id.to_owned()
}

/// Derives mapped and not-yet-mapped call locations from current pending.
fn pending_locations(pending: &PendingTurn) -> Vec<ToolCallLocation> {
    let mut locations = pending
        .tool_calls()
        .iter()
        .map(|call| ToolCallLocation {
            turn_position: usize::MAX,
            kind: ToolCallLocationKind::Pending,
            turn_id: pending.id(),
            call_id: Some(call.call_id()),
            provider_call_id: call.provider_call_id().to_owned(),
            call_message_id: call.call_message_id(),
            result_message_id: call.result_message_id(),
        })
        .collect::<Vec<_>>();

    if pending.phase() == PendingTurnPhase::AwaitingToolCallMappings {
        let call_message_id = pending
            .messages()
            .last()
            .expect("unmapped calls belong to a frozen assistant message")
            .id();
        locations.extend(
            pending
                .unmapped_provider_call_ids()
                .iter()
                .map(|provider_call_id| ToolCallLocation {
                    turn_position: usize::MAX,
                    kind: ToolCallLocationKind::Pending,
                    turn_id: pending.id(),
                    call_id: None,
                    provider_call_id: provider_call_id.clone(),
                    call_message_id,
                    result_message_id: None,
                }),
        );
    }
    locations
}
