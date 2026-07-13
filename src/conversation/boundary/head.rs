//! Logical-head movement over one checked Conversation lineage.

use super::{Boundary, Conversation};
use crate::conversation::ConversationError;

/// Observable result of one checked logical-head operation.
///
/// Both boundaries are freshly issued under the Conversation version that
/// exists after the operation. Consequently, after a real move the old head
/// can be supplied to [`Conversation::revert_to`] as a redo token, while the
/// caller-supplied pre-move token is stale. A move to the current head is an
/// explicit no-op and preserves the structural version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevertOutcome {
    old_head: Boundary,
    new_head: Boundary,
}

impl RevertOutcome {
    /// Creates an outcome from Conversation-issued post-operation tokens.
    const fn new(old_head: Boundary, new_head: Boundary) -> Self {
        Self { old_head, new_head }
    }

    /// Returns the effective head before the operation, reissued afterward.
    #[must_use]
    pub const fn old_head(&self) -> Boundary {
        self.old_head
    }

    /// Returns the effective head after the operation.
    #[must_use]
    pub const fn new_head(&self) -> Boundary {
        self.new_head
    }

    /// Reports whether the operation changed the effective Turn prefix.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.old_head.turn_count() != self.new_head.turn_count()
    }
}

impl Conversation {
    /// Returns a freshly issued token for the current logical head.
    ///
    /// Unlike [`valid_boundaries`](Self::valid_boundaries), this query does not
    /// enumerate the redo suffix. The returned token remains subject to normal
    /// pending and structural-version validation when consumed.
    #[must_use]
    pub fn head(&self) -> Boundary {
        self.issue_boundary_at(self.history.active_len())
    }

    /// Moves the logical head to a checked complete-Turn boundary.
    ///
    /// Moving backward is a revert; moving forward along the same addressable
    /// lineage is a redo. No Turn or message is deleted. A later commit from a
    /// reverted head creates a replacement suffix, after which the old suffix
    /// remains available only through raw-history queries.
    ///
    /// Every real move advances the structural version and rebuilds the
    /// derived tool-call index from the newly effective prefix. The returned
    /// [`RevertOutcome`] contains old and new head tokens signed at that new
    /// version. Targeting the current head succeeds as a no-op and does not
    /// advance the version.
    ///
    /// # Errors
    ///
    /// Returns a classified boundary error for a foreign, stale, pending,
    /// out-of-range, fork-ceiling, or anchor-invalid token. Returns
    /// [`ConversationError::NonAtomicHeadMove`] if the structural version
    /// cannot advance. Every error preserves head, history, pending state,
    /// index, and version.
    pub fn revert_to(&mut self, boundary: Boundary) -> Result<RevertOutcome, ConversationError> {
        let target_position = self.resolve_boundary(&boundary)?;
        let old_position = self.history.active_len();
        if target_position == old_position {
            let head = self.head();
            return Ok(RevertOutcome::new(head, head));
        }

        let next_version =
            self.version
                .checked_add(1)
                .ok_or(ConversationError::NonAtomicHeadMove {
                    current_version: self.version,
                })?;
        self.history.move_head_to(target_position);
        self.tool_call_index.scope_committed_turns(target_position);
        self.version = next_version;

        let old_head = self.issue_boundary_at(old_position);
        let new_head = self.issue_boundary_at(target_position);
        debug_assert_eq!(new_head, self.head());
        Ok(RevertOutcome::new(old_head, new_head))
    }
}
