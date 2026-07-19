//! Structurally shared storage for immutable Conversation history.
//!
//! Closed turns live in append-only [`HistoryNode`] values. A [`History`]
//! clone only clones `Arc` handles; it never walks the lineage or clones
//! message payloads. The current lineage is a derived, read-only slice, while
//! the raw log retains nodes that are no longer on that lineage.

use crate::conversation::{MessageId, ToolCallId, Turn, TurnId};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

mod index;

pub use index::{ToolCallIndex, ToolCallLocation, ToolCallLocationKind};

fn message_id_set<'a>(turns: impl IntoIterator<Item = &'a Turn>) -> HashSet<MessageId> {
    turns
        .into_iter()
        .flat_map(Turn::messages)
        .map(crate::conversation::ConversationMessage::id)
        .collect()
}

/// Append-only raw history plus the currently effective lineage.
///
/// This type is crate-private because callers may only advance it through the
/// validated Conversation commit gate. Its `Clone` implementation is O(1):
/// every potentially long component is held behind an [`Arc`].
#[derive(Clone)]
pub(crate) struct History {
    raw: RawHistory,
    lineage: Arc<Lineage>,
    message_ids: Arc<HashSet<MessageId>>,
    lineage_len: usize,
    active_len: usize,
}

impl History {
    /// Creates an empty history with one shared empty lineage allocation.
    pub(crate) fn new() -> Self {
        let lineage = Arc::new(Lineage::default());
        Self {
            raw: RawHistory::new(lineage.clone()),
            lineage,
            message_ids: Arc::new(HashSet::new()),
            lineage_len: 0,
            active_len: 0,
        }
    }

    /// Returns the current effective lineage as the legacy read-only slice.
    pub(crate) fn turns(&self) -> &[Turn] {
        &self.lineage.turns[..self.active_len]
    }

    /// Returns every addressable turn on this lineage, including redo suffixes.
    ///
    /// `lineage_len` is independent from the backing allocation so a fork can
    /// share its parent's lineage while imposing a strict fork ceiling.
    pub(crate) fn lineage_turns(&self) -> &[Turn] {
        &self.lineage.turns[..self.lineage_len]
    }

    /// Returns the shared backing lineage, including turns beyond a fork ceiling.
    ///
    /// Boundary validation uses this only to distinguish an invalid raw range
    /// from a parent suffix that a forked child is forbidden to address.
    pub(crate) fn backing_lineage_turns(&self) -> &[Turn] {
        &self.lineage.turns
    }

    /// Returns the number of turns addressable by this Conversation lineage.
    pub(crate) const fn lineage_len(&self) -> usize {
        self.lineage_len
    }

    /// Returns the number of turns at or before the logical head.
    pub(crate) const fn active_len(&self) -> usize {
        self.active_len
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
        let message_ids = Arc::make_mut(&mut self.message_ids);
        message_ids.extend(
            turn.messages()
                .iter()
                .map(crate::conversation::ConversationMessage::id),
        );

        let mut nodes = self.lineage.nodes[..self.active_len].to_vec();
        let mut turns = self.lineage.turns[..self.active_len].to_vec();
        nodes.push(node);
        turns.push(turn);
        self.lineage = Arc::new(Lineage { nodes, turns });
        self.active_len += 1;
        self.lineage_len = self.active_len;
    }

    /// Shares an addressable lineage prefix without copying retained turns.
    ///
    /// The returned history sees only the selected prefix as raw inherited
    /// state. Turns later in the backing allocation remain useful for precise
    /// fork-ceiling diagnostics but cannot be queried as child raw history.
    pub(crate) fn shared_prefix(&self, lineage_len: usize) -> Option<Self> {
        if lineage_len > self.lineage_len {
            return None;
        }

        Some(Self {
            raw: RawHistory::from_shared_lineage(self.lineage.clone(), lineage_len),
            lineage: self.lineage.clone(),
            message_ids: Arc::new(message_id_set(&self.lineage.turns[..lineage_len])),
            lineage_len,
            active_len: lineage_len,
        })
    }

    /// Rebuilds runtime history from already-validated persisted facts.
    ///
    /// Restore validation owns all semantic checks before calling this helper:
    /// raw turn identities are unique, every parent exists, parent pointers are
    /// acyclic, the lineage ids exist, and `active_len <= lineage_turn_ids.len()`.
    /// This function only recreates the append-only raw scope and addressable
    /// lineage storage without exposing a second public commit path.
    pub(crate) fn from_restored(
        raw_turns: Vec<Turn>,
        lineage_turn_ids: &[TurnId],
        active_len: usize,
    ) -> Self {
        debug_assert!(active_len <= lineage_turn_ids.len());

        let empty = Arc::new(Lineage::default());
        let turns_by_id = raw_turns
            .iter()
            .cloned()
            .map(|turn| (turn.id(), turn))
            .collect::<HashMap<_, _>>();
        let mut nodes_by_id = HashMap::<TurnId, Arc<HistoryNode>>::new();

        for turn in &raw_turns {
            build_restored_node(turn.id(), &turns_by_id, &mut nodes_by_id);
        }

        let mut raw = RawHistory::new(empty);
        for turn in &raw_turns {
            let node = nodes_by_id
                .get(&turn.id())
                .expect("restore validation made every raw turn node buildable")
                .clone();
            raw.append(node);
        }

        let nodes = lineage_turn_ids
            .iter()
            .map(|turn_id| {
                nodes_by_id
                    .get(turn_id)
                    .expect("restore validation made every lineage id a raw turn")
                    .clone()
            })
            .collect::<Vec<_>>();
        let turns = lineage_turn_ids
            .iter()
            .map(|turn_id| {
                turns_by_id
                    .get(turn_id)
                    .expect("restore validation made every lineage id a raw turn")
                    .clone()
            })
            .collect::<Vec<_>>();
        let lineage_len = turns.len();

        Self {
            raw,
            lineage: Arc::new(Lineage { nodes, turns }),
            message_ids: Arc::new(message_id_set(&raw_turns)),
            lineage_len,
            active_len,
        }
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
        self.message_ids.contains(&message_id)
    }

    /// Collects framework call identities across every retained raw branch.
    pub(crate) fn retained_tool_call_ids(&self) -> impl Iterator<Item = ToolCallId> + '_ {
        self.raw_turns()
            .into_iter()
            .flat_map(Turn::pairings)
            .map(crate::conversation::ToolPairing::call_id)
    }

    /// Moves the effective tip after a Conversation-level boundary check.
    ///
    /// Keeping this primitive crate-private prevents storage callers from
    /// bypassing owner/version/anchor validation. Moving the head never edits
    /// the immutable lineage or append-only raw scope.
    pub(crate) fn move_head_to(&mut self, active_len: usize) {
        debug_assert!(active_len <= self.lineage_len);
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
            .field("lineage_len", &self.lineage_len)
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

impl Drop for HistoryNode {
    /// Unchains uniquely owned ancestors iteratively.
    ///
    /// Dropping the last handle to a long lineage tip would otherwise recurse
    /// through one `Arc` drop per ancestor and overflow the stack on long
    /// histories (M3-4). The walk stops at the first shared node, whose
    /// remaining owners keep it alive.
    fn drop(&mut self) {
        let mut cursor = self.parent.take();
        while let Some(strong) = cursor {
            match Arc::try_unwrap(strong) {
                Ok(mut node) => cursor = node.parent.take(),
                Err(_) => break,
            }
        }
    }
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

    /// Creates a fork scope over one immutable parent-lineage prefix.
    fn from_shared_lineage(base: Arc<Lineage>, base_len: usize) -> Self {
        debug_assert!(base_len <= base.turns.len());
        Self {
            base,
            base_len,
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

impl Drop for RawEntry {
    /// Unchains uniquely owned cons cells iteratively.
    ///
    /// Dropping a long local raw log would otherwise recurse through one `Arc`
    /// drop per entry and overflow the stack on long histories (M3-4). The
    /// walk stops at the first shared entry, whose remaining owners keep it
    /// alive.
    fn drop(&mut self) {
        let mut cursor = self.previous.take();
        while let Some(strong) = cursor {
            match Arc::try_unwrap(strong) {
                Ok(mut entry) => cursor = entry.previous.take(),
                Err(_) => break,
            }
        }
    }
}

/// Iteratively recreates a parent-pointer node from validated turn facts.
///
/// The walk climbs to the nearest already-built ancestor with an explicit
/// stack, then builds back down so every parent exists before its child.
/// Recursing would make the call-stack depth equal to the chain length and
/// overflow the stack on long restored histories (M3-4).
fn build_restored_node(
    turn_id: TurnId,
    turns_by_id: &HashMap<TurnId, Turn>,
    nodes_by_id: &mut HashMap<TurnId, Arc<HistoryNode>>,
) -> Arc<HistoryNode> {
    // Climb to the nearest built ancestor, recording the unbuilt chain.
    let mut pending = Vec::new();
    let mut cursor = turn_id;
    while !nodes_by_id.contains_key(&cursor) {
        pending.push(cursor);
        let turn = turns_by_id
            .get(&cursor)
            .expect("restore validation made every parent reference a raw turn");
        let Some(parent) = turn.parent() else {
            break;
        };
        cursor = parent;
    }
    // Build back down so every parent is inserted before its child.
    for id in pending.into_iter().rev() {
        let turn = turns_by_id
            .get(&id)
            .expect("restore validation made every parent reference a raw turn")
            .clone();
        let parent = turn.parent().map(|parent_id| {
            nodes_by_id
                .get(&parent_id)
                .expect("parents are built before their children")
                .clone()
        });
        let node = Arc::new(HistoryNode { turn, parent });
        nodes_by_id.insert(id, node);
    }
    nodes_by_id
        .get(&turn_id)
        .expect("the requested node was built above")
        .clone()
}

#[cfg(test)]
mod tests;
