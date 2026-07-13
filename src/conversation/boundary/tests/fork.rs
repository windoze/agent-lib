//! Checked fork behavior over public Conversation APIs.

use super::{
    assert_index_matches_rebuild, begin_pending, call_id, commit_text_turn, commit_tool_turn,
    conversation, conversation_id, snapshot, turn_id,
};
use crate::conversation::{BoundaryError, ConversationError, ForkError};

#[test]
fn fork_records_origin_and_reissues_child_owned_boundaries() {
    let mut parent = conversation(300);
    commit_text_turn(&mut parent, 301);
    commit_tool_turn(&mut parent, 302, "shared-call", 9_302);
    commit_tool_turn(&mut parent, 303, "hidden-suffix-call", 9_303);
    let fork_point = parent
        .boundary_after(turn_id(302))
        .expect("tool turn boundary belongs to parent");

    let child = parent
        .fork_at(fork_point, conversation_id(399))
        .expect("valid boundary forks a child");

    let origin = child.origin().expect("fork child records provenance");
    assert_eq!(origin.parent(), parent.id());
    assert_eq!(origin.fork_point(), fork_point);
    assert_eq!(child.id(), conversation_id(399));
    assert_eq!(child.config(), parent.config());
    assert_eq!(child.version(), 0);
    assert_eq!(child.turns().len(), 2);
    assert_eq!(
        child
            .turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(301), turn_id(302)]
    );
    assert_eq!(
        child
            .lineage_turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(301), turn_id(302)]
    );
    assert_eq!(
        child
            .raw_turns()
            .into_iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(301), turn_id(302)]
    );
    assert!(child.raw_turn(turn_id(303)).is_none());
    assert_eq!(
        child
            .boundary_after(turn_id(303))
            .expect_err("parent suffix cannot become a child boundary"),
        BoundaryError::BeyondForkCeiling {
            turn_count: 3,
            fork_ceiling: 2,
        }
    );
    assert_eq!(child.valid_boundaries().len(), 3);
    assert!(child.tool_call_index().by_call_id(call_id(9_302)).is_some());
    assert!(
        child.tool_call_index().by_call_id(call_id(9_303)).is_none(),
        "child lookup must not expose parent suffix calls"
    );
    assert!(
        !format!("{:?}", child.tool_call_index()).contains("hidden-suffix-call"),
        "child index Debug must not expose hidden committed backing"
    );
    assert_index_matches_rebuild(&child);

    let child_head = child.head();
    assert_eq!(child_head.conversation_id(), child.id());
    assert_eq!(child_head.turn_count(), 2);
    assert_eq!(child_head.after_turn(), Some(turn_id(302)));
    assert_eq!(child_head.version(), 0);
    assert_eq!(
        child
            .validate_boundary(&fork_point)
            .expect_err("parent token cannot be consumed by child"),
        BoundaryError::OwnerMismatch {
            expected: child.id(),
            actual: parent.id(),
        }
    );
    assert_eq!(
        parent
            .validate_boundary(&child_head)
            .expect_err("child token cannot be consumed by parent"),
        BoundaryError::OwnerMismatch {
            expected: parent.id(),
            actual: child.id(),
        }
    );
}

#[test]
fn parent_and_child_advance_independently_after_fork() {
    let mut parent = conversation(310);
    commit_text_turn(&mut parent, 311);
    commit_tool_turn(&mut parent, 312, "parent-shared-call", 9_312);
    commit_tool_turn(&mut parent, 313, "parent-suffix-call", 9_313);
    let fork_point = parent
        .boundary_after(turn_id(312))
        .expect("middle boundary");
    let mut child = parent
        .fork_at(fork_point, conversation_id(319))
        .expect("fork child");

    commit_tool_turn(&mut parent, 314, "parent-new-call", 9_314);
    commit_tool_turn(&mut child, 315, "child-new-call", 9_315);

    assert_eq!(
        parent
            .turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(311), turn_id(312), turn_id(313), turn_id(314)]
    );
    assert_eq!(
        child
            .turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(311), turn_id(312), turn_id(315)]
    );
    assert_eq!(child.turns()[2].parent(), Some(turn_id(312)));
    assert!(parent.raw_turn(turn_id(315)).is_none());
    assert!(child.raw_turn(turn_id(313)).is_none());
    assert!(child.raw_turn(turn_id(314)).is_none());
    assert!(
        parent
            .tool_call_index()
            .by_call_id(call_id(9_314))
            .is_some()
    );
    assert!(
        parent
            .tool_call_index()
            .by_call_id(call_id(9_315))
            .is_none()
    );
    assert!(child.tool_call_index().by_call_id(call_id(9_315)).is_some());
    assert!(
        child.tool_call_index().by_call_id(call_id(9_313)).is_none(),
        "parent suffix above the fork point is not a child fact"
    );
    assert_index_matches_rebuild(&parent);
    assert_index_matches_rebuild(&child);
}

#[test]
fn fork_rejects_pending_parent_foreign_token_and_duplicate_child_id_atomically() {
    let mut parent = conversation(320);
    let other = conversation(321);
    commit_text_turn(&mut parent, 322);
    let parent_head = parent.head();

    let duplicate_id_before = snapshot(&parent);
    assert_eq!(
        parent
            .fork_at(parent_head, parent.id())
            .expect_err("child id must be distinct from parent id"),
        ConversationError::Fork(ForkError::DuplicateConversationId {
            conversation_id: parent.id(),
        })
    );
    assert_eq!(snapshot(&parent), duplicate_id_before);

    let foreign_before = snapshot(&parent);
    assert_eq!(
        parent
            .fork_at(other.head(), conversation_id(329))
            .expect_err("foreign boundary owner is rejected"),
        ConversationError::Boundary(BoundaryError::OwnerMismatch {
            expected: parent.id(),
            actual: other.id(),
        })
    );
    assert_eq!(snapshot(&parent), foreign_before);

    begin_pending(&mut parent, 323);
    let pending_before = snapshot(&parent);
    assert_eq!(
        parent
            .fork_at(parent_head, conversation_id(330))
            .expect_err("pending parent is not at a consistency point"),
        ConversationError::Boundary(BoundaryError::PendingTurn {
            turn_id: turn_id(323),
        })
    );
    assert_eq!(snapshot(&parent), pending_before);
}
