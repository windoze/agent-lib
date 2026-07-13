//! Structurally shared storage for immutable Conversation history.
//!
//! Closed turns live in append-only [`HistoryNode`] values. A [`History`]
//! clone only clones `Arc` handles; it never walks the lineage or clones
//! message payloads. The current lineage is a derived, read-only slice, while
//! the raw log retains nodes that are no longer on that lineage.

use crate::conversation::{MessageId, ToolCallId, Turn, TurnId};
use std::{fmt, sync::Arc};

mod index;

pub use index::{ToolCallIndex, ToolCallLocation, ToolCallLocationKind};

/// Append-only raw history plus the currently effective lineage.
///
/// This type is crate-private because callers may only advance it through the
/// validated Conversation commit gate. Its `Clone` implementation is O(1):
/// every potentially long component is held behind an [`Arc`].
#[derive(Clone)]
pub(crate) struct History {
    raw: RawHistory,
    lineage: Arc<Lineage>,
    active_len: usize,
}

impl History {
    /// Creates an empty history with one shared empty lineage allocation.
    pub(crate) fn new() -> Self {
        let lineage = Arc::new(Lineage::default());
        Self {
            raw: RawHistory::new(lineage.clone()),
            lineage,
            active_len: 0,
        }
    }

    /// Returns the current effective lineage as the legacy read-only slice.
    pub(crate) fn turns(&self) -> &[Turn] {
        &self.lineage.turns[..self.active_len]
    }

    /// Returns the effective tip node, or `None` at the zero-turn boundary.
    fn tip_node(&self) -> Option<&Arc<HistoryNode>> {
        self.active_len
            .checked_sub(1)
            .and_then(|index| self.lineage.nodes.get(index))
    }

    /// Returns the current effective parent identity for a new turn.
    pub(crate) fn tip_id(&self) -> Option<TurnId> {
        self.tip_node().map(|node| node.turn.id())
    }

    /// Appends one already-validated turn without changing an existing node.
    ///
    /// When `active_len` points before the end of the stored lineage, this
    /// creates a new lineage from that prefix. The old suffix remains reachable
    /// from the append-only raw log.
    pub(crate) fn append(&mut self, turn: Turn) {
        debug_assert_eq!(turn.parent(), self.tip_id());

        let node = Arc::new(HistoryNode {
            turn: turn.clone(),
            parent: self.tip_node().cloned(),
        });
        debug_assert_eq!(
            node.parent.as_ref().map(|parent| parent.turn.id()),
            turn.parent()
        );
        self.raw.append(node.clone());

        let mut nodes = self.lineage.nodes[..self.active_len].to_vec();
        let mut turns = self.lineage.turns[..self.active_len].to_vec();
        nodes.push(node);
        turns.push(turn);
        self.lineage = Arc::new(Lineage { nodes, turns });
        self.active_len += 1;
    }

    /// Finds a retained raw turn by stable identity, including hidden suffixes.
    pub(crate) fn raw_turn(&self, turn_id: TurnId) -> Option<&Turn> {
        self.raw.find(turn_id).map(|node| &node.turn)
    }

    /// Collects every retained raw turn in deterministic insertion order.
    ///
    /// The temporary reference vector is intentionally not stored as another
    /// fact source. It is used by validation and identity checks that must see
    /// detached nodes as well as the current lineage.
    pub(crate) fn raw_turns(&self) -> Vec<&Turn> {
        self.raw.turns()
    }

    /// Reports whether any retained branch already owns a turn identity.
    pub(crate) fn contains_turn_id(&self, turn_id: TurnId) -> bool {
        self.raw_turn(turn_id).is_some()
    }

    /// Reports whether any retained branch already owns a message identity.
    pub(crate) fn contains_message_id(&self, message_id: MessageId) -> bool {
        self.raw_turns().into_iter().any(|turn| {
            turn.messages()
                .iter()
                .any(|message| message.id() == message_id)
        })
    }

    /// Collects framework call identities across every retained raw branch.
    pub(crate) fn retained_tool_call_ids(&self) -> impl Iterator<Item = ToolCallId> + '_ {
        self.raw_turns()
            .into_iter()
            .flat_map(Turn::pairings)
            .map(crate::conversation::ToolPairing::call_id)
    }

    /// Moves the effective tip in tests that exercise the M3-1 storage layer.
    ///
    /// Public checked head movement is introduced by the later Boundary task;
    /// this hook exists only to prove now that a replacement suffix leaves raw
    /// nodes intact and excluded from the effective lineage.
    #[cfg(test)]
    fn set_active_len_for_test(&mut self, active_len: usize) {
        assert!(active_len <= self.lineage.turns.len());
        self.active_len = active_len;
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for History {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("History")
            .field("raw_len", &self.raw.len())
            .field("active_len", &self.active_len)
            .field("tip", &self.tip_id())
            .finish()
    }
}

/// One immutable node in the parent-pointer history tree.
struct HistoryNode {
    turn: Turn,
    parent: Option<Arc<HistoryNode>>,
}

/// A materialized current-lineage view shared by history clones and forks.
#[derive(Default)]
struct Lineage {
    nodes: Vec<Arc<HistoryNode>>,
    turns: Vec<Turn>,
}

/// A scoped raw base plus append-only local nodes.
///
/// `base` is empty for a root Conversation. The later fork task can point it
/// at an ancestor-lineage prefix in O(1), while local commits remain an
/// immutable cons list. Keeping the fields active in all reads establishes the
/// required visibility boundary without exposing it publicly.
#[derive(Clone)]
struct RawHistory {
    base: Arc<Lineage>,
    base_len: usize,
    local_tip: Option<Arc<RawEntry>>,
    local_len: usize,
}

impl RawHistory {
    /// Creates a root raw scope with no inherited or local nodes.
    fn new(empty_lineage: Arc<Lineage>) -> Self {
        Self {
            base: empty_lineage,
            base_len: 0,
            local_tip: None,
            local_len: 0,
        }
    }

    /// Adds one immutable node to the local raw log.
    fn append(&mut self, node: Arc<HistoryNode>) {
        self.local_tip = Some(Arc::new(RawEntry {
            node,
            previous: self.local_tip.clone(),
        }));
        self.local_len += 1;
    }

    /// Returns the number of retained nodes in this raw visibility scope.
    fn len(&self) -> usize {
        self.base_len + self.local_len
    }

    /// Finds a node without treating the current lineage as the fact source.
    fn find(&self, turn_id: TurnId) -> Option<&HistoryNode> {
        let mut cursor = self.local_tip.as_deref();
        while let Some(entry) = cursor {
            if entry.node.turn.id() == turn_id {
                return Some(&entry.node);
            }
            cursor = entry.previous.as_deref();
        }
        self.base.nodes[..self.base_len]
            .iter()
            .find(|node| node.turn.id() == turn_id)
            .map(Arc::as_ref)
    }

    /// Produces inherited nodes followed by local nodes in insertion order.
    fn turns(&self) -> Vec<&Turn> {
        let mut turns = self.base.nodes[..self.base_len]
            .iter()
            .map(|node| &node.turn)
            .collect::<Vec<_>>();
        let mut local = Vec::with_capacity(self.local_len);
        let mut cursor = self.local_tip.as_deref();
        while let Some(entry) = cursor {
            local.push(&entry.node.turn);
            cursor = entry.previous.as_deref();
        }
        local.reverse();
        turns.extend(local);
        turns
    }
}

/// One entry in the persistent local raw log.
struct RawEntry {
    node: Arc<HistoryNode>,
    previous: Option<Arc<RawEntry>>,
}

#[cfg(test)]
mod tests;
