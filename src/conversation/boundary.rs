//! Conversation-issued, versioned Turn-boundary tokens.
//!
//! A [`Boundary`] is a capability-like value, not a trusted integer. Its
//! position is paired with a stable Turn anchor, owner identity, and structural
//! version. Deserialization restores only those claims; every operation that
//! consumes a token must ask its [`Conversation`] to validate them again.

use super::{BoundaryError, Conversation, ConversationId, TurnId};
use serde::{Deserialize, Serialize};

mod head;

pub use head::RevertOutcome;

/// A Conversation-issued token naming one complete-Turn cut.
///
/// `turn_count == 0` and `after_turn == None` identify the zero-turn boundary.
/// Every other valid token names the Turn immediately before the cut. The
/// fields are deliberately private, so ordinary callers obtain tokens from
/// [`Conversation::valid_boundaries`] or [`Conversation::boundary_after`].
///
/// Serde input remains untrusted and must pass
/// [`Conversation::validate_boundary`] before use. In particular, a token does
/// not prove its own owner, version, range, or anchor merely because its JSON
/// shape is valid.
///
/// Boundary fields cannot be assembled directly:
///
/// ```compile_fail
/// use agent_lib::conversation::{Boundary, ConversationId};
///
/// let conversation_id: ConversationId =
///     "018f0d9c-7b6a-7c12-8f31-1234567890ab".parse().unwrap();
/// let _forged = Boundary {
///     conversation_id,
///     turn_count: 0,
///     after_turn: None,
///     version: 0,
/// };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
    conversation_id: ConversationId,
    turn_count: u64,
    after_turn: Option<TurnId>,
    version: u64,
}

impl Boundary {
    /// Creates one token from facts owned by a Conversation.
    const fn issued(
        conversation_id: ConversationId,
        turn_count: u64,
        after_turn: Option<TurnId>,
        version: u64,
    ) -> Self {
        Self {
            conversation_id,
            turn_count,
            after_turn,
            version,
        }
    }

    /// Returns the Conversation that issued this token.
    #[must_use]
    pub const fn conversation_id(&self) -> ConversationId {
        self.conversation_id
    }

    /// Returns the number of complete Turns before this cut.
    #[must_use]
    pub const fn turn_count(&self) -> u64 {
        self.turn_count
    }

    /// Returns the stable Turn immediately before this cut, if any.
    #[must_use]
    pub const fn after_turn(&self) -> Option<TurnId> {
        self.after_turn
    }

    /// Returns the structural Conversation version at which this token was issued.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }
}

impl Conversation {
    /// Returns every currently addressable Turn boundary in lineage order.
    ///
    /// The first item is always the zero-turn boundary. The remainder cover
    /// each Turn through this Conversation's lineage ceiling. After a logical
    /// revert, that includes the same-lineage suffix beyond the active head so
    /// a caller can obtain fresh redo tokens. A forked child never exposes its
    /// parent's suffix above the child's ceiling.
    #[must_use]
    pub fn valid_boundaries(&self) -> Vec<Boundary> {
        (0..=self.history.lineage_len())
            .map(|position| self.issue_boundary_at(position))
            .collect()
    }

    /// Returns the current-lineage boundary immediately after `turn_id`.
    ///
    /// Same-lineage Turns beyond a reverted head remain addressable for redo.
    /// An unknown raw identity, a detached branch, and a parent suffix above a
    /// fork ceiling receive distinct errors.
    ///
    /// # Errors
    ///
    /// Returns [`BoundaryError::UnknownTurn`] when the Turn was never retained,
    /// [`BoundaryError::TurnNotOnLineage`] for a detached raw Turn, or
    /// [`BoundaryError::BeyondForkCeiling`] when a shared backing Turn is above
    /// this Conversation's fork ceiling.
    pub fn boundary_after(&self, turn_id: TurnId) -> Result<Boundary, BoundaryError> {
        if let Some(index) = self
            .history
            .backing_lineage_turns()
            .iter()
            .position(|turn| turn.id() == turn_id)
        {
            let turn_count =
                u64::try_from(index + 1).expect("an in-memory lineage length cannot exceed u64");
            let fork_ceiling = self.lineage_len_u64();
            if turn_count > fork_ceiling {
                return Err(BoundaryError::BeyondForkCeiling {
                    turn_count,
                    fork_ceiling,
                });
            }
            return Ok(self.issue_boundary(turn_count, Some(turn_id)));
        }

        if self.history.contains_turn_id(turn_id) {
            Err(BoundaryError::TurnNotOnLineage { turn_id })
        } else {
            Err(BoundaryError::UnknownTurn { turn_id })
        }
    }

    /// Revalidates a Boundary token against current Conversation state.
    ///
    /// This is the same resolver used by every future boundary-consuming
    /// operation. Validation is read-only: every error leaves history, pending,
    /// the derived tool-call index, and structural version unchanged.
    ///
    /// # Errors
    ///
    /// Returns a classified [`BoundaryError`] for owner/version mismatch,
    /// active pending state, invalid range, fork-ceiling escape, or an anchor
    /// that does not match the claimed lineage position.
    pub fn validate_boundary(&self, boundary: &Boundary) -> Result<(), BoundaryError> {
        self.resolve_boundary(boundary).map(drop)
    }

    /// Resolves an untrusted token to a checked lineage position.
    ///
    /// Owner and version intentionally take precedence over shape checks: an
    /// old token is stale even if an ABA transition later recreates the same
    /// numeric position and Turn anchor.
    pub(crate) fn resolve_boundary(&self, boundary: &Boundary) -> Result<usize, BoundaryError> {
        if boundary.conversation_id != self.id {
            return Err(BoundaryError::OwnerMismatch {
                expected: self.id,
                actual: boundary.conversation_id,
            });
        }
        if boundary.version != self.version {
            return Err(BoundaryError::StaleBoundary {
                boundary_version: boundary.version,
                current_version: self.version,
            });
        }
        if let Some(pending) = &self.pending {
            return Err(BoundaryError::PendingTurn {
                turn_id: pending.id(),
            });
        }

        let backing_turns = self.backing_len_u64();
        if boundary.turn_count > backing_turns {
            return Err(BoundaryError::PositionOutOfRange {
                turn_count: boundary.turn_count,
                backing_turns,
            });
        }

        let fork_ceiling = self.lineage_len_u64();
        if boundary.turn_count > fork_ceiling {
            return Err(BoundaryError::BeyondForkCeiling {
                turn_count: boundary.turn_count,
                fork_ceiling,
            });
        }

        let position = usize::try_from(boundary.turn_count).map_err(|_| {
            BoundaryError::PositionOutOfRange {
                turn_count: boundary.turn_count,
                backing_turns,
            }
        })?;
        let expected = position
            .checked_sub(1)
            .and_then(|index| self.history.backing_lineage_turns().get(index))
            .map(super::Turn::id);
        if boundary.after_turn != expected {
            return Err(BoundaryError::AnchorMismatch {
                turn_count: boundary.turn_count,
                expected,
                actual: boundary.after_turn,
            });
        }

        Ok(position)
    }

    /// Issues a token from already-owned lineage facts.
    fn issue_boundary(&self, turn_count: u64, after_turn: Option<TurnId>) -> Boundary {
        Boundary::issued(self.id, turn_count, after_turn, self.version)
    }

    /// Issues a token for one already-checked current-lineage position.
    fn issue_boundary_at(&self, position: usize) -> Boundary {
        debug_assert!(position <= self.history.lineage_len());
        let turn_count =
            u64::try_from(position).expect("an in-memory lineage length cannot exceed u64");
        let after_turn = position
            .checked_sub(1)
            .and_then(|index| self.history.lineage_turns().get(index))
            .map(super::Turn::id);
        self.issue_boundary(turn_count, after_turn)
    }

    /// Converts the addressable lineage ceiling to the token's stable width.
    fn lineage_len_u64(&self) -> u64 {
        u64::try_from(self.history.lineage_len())
            .expect("an in-memory lineage length cannot exceed u64")
    }

    /// Converts the backing allocation length to the token's stable width.
    fn backing_len_u64(&self) -> u64 {
        u64::try_from(self.history.backing_lineage_turns().len())
            .expect("an in-memory lineage length cannot exceed u64")
    }
}

#[cfg(test)]
mod tests;
