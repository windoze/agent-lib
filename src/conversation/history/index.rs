//! Rebuildable tool-call lookup acceleration derived from Conversation facts.

use crate::{
    conversation::{
        MessageId, PendingTurn, PendingTurnPhase, ToolCallId, ToolPairing, Turn, TurnId,
    },
    model::content::ContentBlock,
};
use std::collections::{HashMap, HashSet};

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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCallIndex {
    entries: Vec<ToolCallLocation>,
    committed_len: usize,
    by_call_id: HashMap<ToolCallId, usize>,
    by_provider_call_id: HashMap<String, Vec<usize>>,
}

impl ToolCallIndex {
    /// Returns the number of calls visible in the current lineage and pending.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether the current lineage and pending contain no tool calls.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates through committed calls and then pending calls in message order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &ToolCallLocation> {
        self.entries.iter()
    }

    /// Finds one mapped call by its framework-owned stable identity.
    #[must_use]
    pub fn by_call_id(&self, call_id: ToolCallId) -> Option<&ToolCallLocation> {
        self.by_call_id
            .get(&call_id)
            .map(|index| &self.entries[*index])
    }

    /// Finds every current call carrying the provider id.
    ///
    /// Multiple results are possible because provider call ids are scoped to
    /// provider interactions rather than globally assigned by Conversation.
    pub fn by_provider_call_id<'a>(
        &'a self,
        provider_call_id: &str,
    ) -> impl Iterator<Item = &'a ToolCallLocation> + 'a {
        self.by_provider_call_id
            .get(provider_call_id)
            .into_iter()
            .flatten()
            .map(|index| &self.entries[*index])
    }

    /// Reconstructs the entire derived index from authoritative facts.
    ///
    /// The supplied turns must be in current-lineage order. Rebuilding never
    /// changes those facts and is useful after head, fork, or restore changes;
    /// the result remains an acceleration value rather than validation proof.
    #[must_use]
    pub fn rebuild(turns: &[Turn], pending: Option<&PendingTurn>) -> Self {
        let mut index = Self::default();
        for turn in turns {
            index.push_committed_turn(turn);
        }
        index.replace_pending(pending);
        index
    }

    /// Adds one newly committed turn without rescanning older closed turns.
    pub(crate) fn push_committed_turn(&mut self, turn: &Turn) {
        self.remove_pending();
        self.extend(turn_locations(turn));
        self.committed_len = self.entries.len();
    }

    /// Replaces only transaction-local records after a pending transition.
    pub(crate) fn replace_pending(&mut self, pending: Option<&PendingTurn>) {
        self.remove_pending();
        if let Some(pending) = pending {
            self.extend(pending_locations(pending));
        }
    }

    /// Adds records while maintaining both lookup tables.
    fn extend(&mut self, locations: impl IntoIterator<Item = ToolCallLocation>) {
        for location in locations {
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
    }

    /// Removes the previous pending suffix while preserving committed lookups.
    fn remove_pending(&mut self) {
        if self.entries.len() == self.committed_len {
            return;
        }
        for location in self.entries.drain(self.committed_len..) {
            if let Some(call_id) = location.call_id {
                self.by_call_id.remove(&call_id);
            }
        }
        self.by_provider_call_id.retain(|_, indices| {
            indices.retain(|index| *index < self.committed_len);
            !indices.is_empty()
        });
    }
}

/// Derives complete call locations from one validator-certified turn.
fn turn_locations(turn: &Turn) -> Vec<ToolCallLocation> {
    turn.pairings()
        .iter()
        .map(|pairing| ToolCallLocation {
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
