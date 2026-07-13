//! Classified Boundary failures and read-only rejection guarantees.

use super::{
    begin_pending, commit_text_turn, conversation, conversation_id, forged_boundary,
    shared_prefix_child, snapshot, turn_id,
};
use crate::conversation::BoundaryError;

#[test]
fn token_from_another_conversation_is_rejected_before_other_claims() {
    let source = conversation(10);
    let target = conversation(11);
    let foreign = source.valid_boundaries()[0];
    let before = snapshot(&target);

    let error = target
        .validate_boundary(&foreign)
        .expect_err("owner mismatch must reject token");

    assert_eq!(
        error,
        BoundaryError::OwnerMismatch {
            expected: target.id(),
            actual: source.id(),
        }
    );
    assert_eq!(snapshot(&target), before);
}

#[test]
fn structural_version_rejects_same_position_and_anchor_after_aba_change() {
    let mut conversation = conversation(12);
    commit_text_turn(&mut conversation, 100);
    let old = conversation
        .boundary_after(turn_id(100))
        .expect("first commit boundary");

    commit_text_turn(&mut conversation, 101);
    let fresh = conversation
        .boundary_after(turn_id(100))
        .expect("same position remains addressable");
    let before = snapshot(&conversation);

    assert_eq!(old.conversation_id(), fresh.conversation_id());
    assert_eq!(old.turn_count(), fresh.turn_count());
    assert_eq!(old.after_turn(), fresh.after_turn());
    assert_ne!(old.version(), fresh.version());
    assert_eq!(
        conversation
            .validate_boundary(&old)
            .expect_err("old token cannot survive structural ABA"),
        BoundaryError::StaleBoundary {
            boundary_version: old.version(),
            current_version: conversation.version(),
        }
    );
    conversation
        .validate_boundary(&fresh)
        .expect("fresh same-position token validates");
    assert_eq!(snapshot(&conversation), before);
}

#[test]
fn active_pending_turn_blocks_boundary_consumption_without_discarding_it() {
    let mut conversation = conversation(13);
    commit_text_turn(&mut conversation, 110);
    let boundary = conversation.valid_boundaries()[1];
    begin_pending(&mut conversation, 111);
    let before = snapshot(&conversation);

    let error = conversation
        .validate_boundary(&boundary)
        .expect_err("pending state is not a committed cut");

    assert_eq!(
        error,
        BoundaryError::PendingTurn {
            turn_id: turn_id(111),
        }
    );
    assert_eq!(snapshot(&conversation), before);
}

#[test]
fn boundary_after_distinguishes_unknown_and_detached_raw_turns() {
    let mut conversation = conversation(14);
    let empty_before = snapshot(&conversation);
    assert_eq!(
        conversation
            .boundary_after(turn_id(120))
            .expect_err("unknown turn has no boundary"),
        BoundaryError::UnknownTurn {
            turn_id: turn_id(120),
        }
    );
    assert_eq!(snapshot(&conversation), empty_before);

    commit_text_turn(&mut conversation, 121);
    commit_text_turn(&mut conversation, 122);
    let first_turn = conversation
        .boundary_after(turn_id(121))
        .expect("first turn has a checked boundary");
    conversation
        .revert_to(first_turn)
        .expect("checked head movement succeeds");
    commit_text_turn(&mut conversation, 123);
    let branched_before = snapshot(&conversation);

    assert!(conversation.raw_turn(turn_id(122)).is_some());
    assert_eq!(
        conversation
            .boundary_after(turn_id(122))
            .expect_err("detached raw suffix is not current-lineage redo"),
        BoundaryError::TurnNotOnLineage {
            turn_id: turn_id(122),
        }
    );
    assert_eq!(snapshot(&conversation), branched_before);
}

#[test]
fn child_rejects_parent_suffix_above_its_fork_ceiling() {
    let mut parent = conversation(15);
    for seed in 130..133 {
        commit_text_turn(&mut parent, seed);
    }
    let child = shared_prefix_child(&parent, conversation_id(16), 1);
    let before = snapshot(&child);

    assert_eq!(
        child
            .boundary_after(turn_id(131))
            .expect_err("child cannot issue parent-suffix token"),
        BoundaryError::BeyondForkCeiling {
            turn_count: 2,
            fork_ceiling: 1,
        }
    );

    let forged = forged_boundary(child.id(), 2, Some(turn_id(131)), child.version());
    assert_eq!(
        child
            .validate_boundary(&forged)
            .expect_err("forged child-owner token cannot cross ceiling"),
        BoundaryError::BeyondForkCeiling {
            turn_count: 2,
            fork_ceiling: 1,
        }
    );
    assert_eq!(snapshot(&child), before);
}
