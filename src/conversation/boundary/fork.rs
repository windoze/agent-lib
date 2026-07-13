//! Checked O(1) fork creation from a complete-Turn boundary.

use super::{Boundary, Conversation};
use crate::conversation::{ConversationError, ConversationId, ForkError};
use serde::{Deserialize, Serialize};

/// Provenance recorded on a Conversation created by [`Conversation::fork_at`].
///
/// The fork point is the parent-issued [`Boundary`] that was valid when the
/// child was created. The child signs its own future boundaries with its own
/// [`ConversationId`] and structural version; this origin is provenance, not a
/// reusable child boundary token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkOrigin {
    parent: ConversationId,
    fork_point: Boundary,
}

impl ForkOrigin {
    /// Creates provenance from a checked parent boundary.
    const fn new(parent: ConversationId, fork_point: Boundary) -> Self {
        Self { parent, fork_point }
    }

    /// Returns the Conversation that issued the fork boundary.
    #[must_use]
    pub const fn parent(&self) -> ConversationId {
        self.parent
    }

    /// Returns the parent-issued boundary where the child lineage starts.
    #[must_use]
    pub const fn fork_point(&self) -> Boundary {
        self.fork_point
    }
}

impl Conversation {
    /// Creates a child Conversation from a checked complete-Turn boundary.
    ///
    /// The child receives caller-supplied identity and fresh structural version
    /// zero. It shares the immutable parent lineage backing allocation but its
    /// raw/debug visibility scope contains only ancestors through `boundary`;
    /// parent suffixes above the fork point are not child facts, boundaries, or
    /// raw turns. Later parent and child commits, cancels, reverts, and derived
    /// index updates are independent.
    ///
    /// # Errors
    ///
    /// Returns a classified boundary error for foreign, stale, pending,
    /// out-of-range, fork-ceiling, or anchor-invalid tokens. Returns
    /// [`ForkError::DuplicateConversationId`] when the supplied child id equals
    /// this Conversation's id. Every error leaves the parent unchanged.
    pub fn fork_at(
        &self,
        boundary: Boundary,
        new_conversation_id: ConversationId,
    ) -> Result<Self, ConversationError> {
        if new_conversation_id == self.id {
            return Err(ForkError::DuplicateConversationId {
                conversation_id: new_conversation_id,
            }
            .into());
        }

        let fork_position = self.resolve_boundary(&boundary)?;
        let history = self
            .history
            .shared_prefix(fork_position)
            .expect("resolved boundary position belongs to this lineage");
        let tool_call_index = self
            .tool_call_index
            .fork_scope(fork_position)
            .expect("resolved boundary position has a committed index scope");
        let projection = crate::conversation::Projection::raw_for_active_turns(
            new_conversation_id,
            history.turns(),
        );

        Ok(Self {
            id: new_conversation_id,
            config: self.config.clone(),
            history,
            projection,
            pending: None,
            tool_call_index,
            version: 0,
            origin: Some(ForkOrigin::new(self.id, boundary)),
        })
    }
}
