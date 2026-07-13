//! Classified rejection and whole-state atomicity for head movement.

use super::super::{
    begin_pending, commit_text_turn, conversation, conversation_id, forged_boundary, snapshot,
    turn_id,
};
use crate::conversation::{BoundaryError, ConversationError};

#[test]
fn foreign_stale_and_forged_tokens_leave_every_conversation_component_unchanged() {
    let source = conversation(240);
    let mut target = conversation(241);
    commit_text_turn(&mut target, 242);
    commit_text_turn(&mut target, 243);

    let foreign = source.head();
    let before_foreign = snapshot(&target);
    assert_eq!(
        target
            .revert_to(foreign)
            .expect_err("foreign owner must be rejected"),
        ConversationError::Boundary(BoundaryError::OwnerMismatch {
            expected: target.id(),
            actual: source.id(),
        })
    );
    assert_eq!(snapshot(&target), before_foreign);

    let stale = target
        .boundary_after(turn_id(242))
        .expect("first Turn boundary");
    commit_text_turn(&mut target, 244);
    let before_stale = snapshot(&target);
    assert_eq!(
        target
            .revert_to(stale)
            .expect_err("old structural version must be rejected"),
        ConversationError::Boundary(BoundaryError::StaleBoundary {
            boundary_version: stale.version(),
            current_version: target.version(),
        })
    );
    assert_eq!(snapshot(&target), before_stale);

    let forged = forged_boundary(target.id(), 2, Some(turn_id(244)), target.version());
    let before_forged = snapshot(&target);
    assert_eq!(
        target
            .revert_to(forged)
            .expect_err("mismatched lineage anchor must be rejected"),
        ConversationError::Boundary(BoundaryError::AnchorMismatch {
            turn_count: 2,
            expected: Some(turn_id(243)),
            actual: Some(turn_id(244)),
        })
    );
    assert_eq!(snapshot(&target), before_forged);
}

#[test]
fn pending_transaction_blocks_head_movement_without_being_discarded() {
    let mut conversation = conversation(250);
    commit_text_turn(&mut conversation, 251);
    let zero = conversation.valid_boundaries()[0];
    begin_pending(&mut conversation, 252);
    let before = snapshot(&conversation);

    assert_eq!(
        conversation
            .revert_to(zero)
            .expect_err("pending state is not a head-movement boundary"),
        ConversationError::Boundary(BoundaryError::PendingTurn {
            turn_id: turn_id(252),
        })
    );
    assert_eq!(snapshot(&conversation), before);
}

#[test]
fn exhausted_structural_version_rejects_a_real_move_atomically() {
    let mut conversation = conversation(260);
    commit_text_turn(&mut conversation, 261);
    conversation.version = u64::MAX;
    let zero = conversation.valid_boundaries()[0];
    let before = snapshot(&conversation);

    assert_eq!(
        conversation
            .revert_to(zero)
            .expect_err("version exhaustion prevents an atomic move"),
        ConversationError::NonAtomicHeadMove {
            current_version: u64::MAX,
        }
    );
    assert_eq!(snapshot(&conversation), before);
}

#[test]
fn detached_turn_claim_cannot_be_forged_into_a_current_lineage_target() {
    let mut conversation = conversation(270);
    commit_text_turn(&mut conversation, 271);
    commit_text_turn(&mut conversation, 272);
    let first = conversation
        .boundary_after(turn_id(271))
        .expect("branch point boundary");
    conversation.revert_to(first).expect("move to branch point");
    commit_text_turn(&mut conversation, 273);

    let forged = forged_boundary(
        conversation_id(270),
        2,
        Some(turn_id(272)),
        conversation.version(),
    );
    let before = snapshot(&conversation);
    assert_eq!(
        conversation
            .revert_to(forged)
            .expect_err("detached raw anchor is not on the current lineage"),
        ConversationError::Boundary(BoundaryError::AnchorMismatch {
            turn_count: 2,
            expected: Some(turn_id(273)),
            actual: Some(turn_id(272)),
        })
    );
    assert_eq!(snapshot(&conversation), before);
}
