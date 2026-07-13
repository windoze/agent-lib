//! Milestone 3 review matrix across branch, revert, fork, and pending cuts.

use super::{
    assert_index_matches_rebuild, begin_pending, call_id, commit_text_turn, commit_tool_turn,
    conversation, conversation_id, snapshot, turn_id,
};
use crate::conversation::{BoundaryError, ConversationError};

fn turn_ids(conversation: &crate::conversation::Conversation) -> Vec<crate::conversation::TurnId> {
    conversation.turns().iter().map(|turn| turn.id()).collect()
}

fn raw_turn_ids(
    conversation: &crate::conversation::Conversation,
) -> Vec<crate::conversation::TurnId> {
    conversation
        .raw_turns()
        .into_iter()
        .map(|turn| turn.id())
        .collect()
}

#[test]
fn review_matrix_preserves_parent_tree_raw_retention_active_views_and_index_isolation() {
    let mut parent = conversation(400);
    commit_text_turn(&mut parent, 401);
    commit_tool_turn(&mut parent, 402, "shared-call", 9_402);
    commit_tool_turn(&mut parent, 403, "detached-parent-call", 9_403);
    let detached_parent_message_storage = parent
        .raw_turn(turn_id(403))
        .expect("old parent suffix exists")
        .messages()
        .as_ptr();

    let branch_point = parent
        .boundary_after(turn_id(402))
        .expect("middle boundary belongs to parent lineage");
    parent
        .revert_to(branch_point)
        .expect("parent can move head back to the branch point");
    commit_tool_turn(&mut parent, 404, "replacement-parent-call", 9_404);

    assert_eq!(
        turn_ids(&parent),
        vec![turn_id(401), turn_id(402), turn_id(404)]
    );
    assert_eq!(
        raw_turn_ids(&parent),
        vec![turn_id(401), turn_id(402), turn_id(403), turn_id(404)]
    );
    assert_eq!(parent.raw_turn(turn_id(401)).expect("root").parent(), None);
    assert_eq!(
        parent
            .raw_turn(turn_id(402))
            .expect("shared child")
            .parent(),
        Some(turn_id(401))
    );
    assert_eq!(
        parent
            .raw_turn(turn_id(403))
            .expect("detached child")
            .parent(),
        Some(turn_id(402))
    );
    assert_eq!(
        parent
            .raw_turn(turn_id(404))
            .expect("replacement child")
            .parent(),
        Some(turn_id(402))
    );
    assert_eq!(
        parent
            .raw_turn(turn_id(403))
            .expect("detached suffix remains retained")
            .messages()
            .as_ptr(),
        detached_parent_message_storage
    );
    assert!(
        parent
            .tool_call_index()
            .by_call_id(call_id(9_402))
            .is_some(),
        "shared call remains visible on the replacement parent branch"
    );
    assert!(
        parent
            .tool_call_index()
            .by_call_id(call_id(9_403))
            .is_none(),
        "detached parent suffix calls do not enter the effective index"
    );
    assert!(
        parent
            .tool_call_index()
            .by_call_id(call_id(9_404))
            .is_some(),
        "replacement parent call is indexed"
    );
    assert_index_matches_rebuild(&parent);

    let fork_point = parent
        .boundary_after(turn_id(402))
        .expect("fresh parent token for the shared prefix");
    let mut child = parent
        .fork_at(fork_point, conversation_id(499))
        .expect("fork from the shared prefix");

    assert_eq!(child.origin().expect("fork origin").parent(), parent.id());
    assert_eq!(
        child.origin().expect("fork origin").fork_point(),
        fork_point
    );
    assert_eq!(turn_ids(&child), vec![turn_id(401), turn_id(402)]);
    assert_eq!(raw_turn_ids(&child), vec![turn_id(401), turn_id(402)]);
    assert!(child.raw_turn(turn_id(403)).is_none());
    assert!(child.raw_turn(turn_id(404)).is_none());
    assert_eq!(
        child
            .boundary_after(turn_id(404))
            .expect_err("parent replacement suffix is above the child fork ceiling"),
        BoundaryError::BeyondForkCeiling {
            turn_count: 3,
            fork_ceiling: 2,
        }
    );
    assert_eq!(
        child
            .boundary_after(turn_id(403))
            .expect_err("detached parent suffix is not a child fact"),
        BoundaryError::UnknownTurn {
            turn_id: turn_id(403),
        }
    );
    assert_eq!(
        parent
            .raw_turn(turn_id(402))
            .expect("shared parent turn")
            .messages()
            .as_ptr(),
        child
            .raw_turn(turn_id(402))
            .expect("shared child turn")
            .messages()
            .as_ptr(),
        "fork shares immutable message storage instead of re-id/re-clone"
    );
    assert!(child.tool_call_index().by_call_id(call_id(9_402)).is_some());
    assert!(child.tool_call_index().by_call_id(call_id(9_403)).is_none());
    assert!(child.tool_call_index().by_call_id(call_id(9_404)).is_none());
    assert_index_matches_rebuild(&child);

    commit_tool_turn(&mut child, 405, "child-call", 9_405);

    assert_eq!(
        turn_ids(&parent),
        vec![turn_id(401), turn_id(402), turn_id(404)]
    );
    assert_eq!(
        raw_turn_ids(&parent),
        vec![turn_id(401), turn_id(402), turn_id(403), turn_id(404)]
    );
    assert!(parent.raw_turn(turn_id(405)).is_none());
    assert_eq!(
        turn_ids(&child),
        vec![turn_id(401), turn_id(402), turn_id(405)]
    );
    assert_eq!(
        raw_turn_ids(&child),
        vec![turn_id(401), turn_id(402), turn_id(405)]
    );
    assert_eq!(
        child.raw_turn(turn_id(405)).expect("child suffix").parent(),
        Some(turn_id(402))
    );
    assert!(child.tool_call_index().by_call_id(call_id(9_402)).is_some());
    assert!(child.tool_call_index().by_call_id(call_id(9_405)).is_some());
    assert!(child.tool_call_index().by_call_id(call_id(9_404)).is_none());
    assert_index_matches_rebuild(&parent);
    assert_index_matches_rebuild(&child);

    let child_prefix = child
        .boundary_after(turn_id(401))
        .expect("child can revert within its own lineage");
    let child_redo = child
        .revert_to(child_prefix)
        .expect("child revert is independent of parent")
        .old_head();
    assert_eq!(turn_ids(&child), vec![turn_id(401)]);
    assert!(child.tool_call_index().by_call_id(call_id(9_402)).is_none());
    assert!(child.tool_call_index().by_call_id(call_id(9_405)).is_none());
    assert_eq!(
        turn_ids(&parent),
        vec![turn_id(401), turn_id(402), turn_id(404)]
    );
    assert!(
        parent
            .tool_call_index()
            .by_call_id(call_id(9_404))
            .is_some()
    );
    assert_index_matches_rebuild(&child);

    child
        .revert_to(child_redo)
        .expect("fresh post-revert token redoes the child suffix");
    assert_eq!(
        turn_ids(&child),
        vec![turn_id(401), turn_id(402), turn_id(405)]
    );
    assert!(child.tool_call_index().by_call_id(call_id(9_402)).is_some());
    assert!(child.tool_call_index().by_call_id(call_id(9_405)).is_some());
    assert_index_matches_rebuild(&child);

    let parent_head = parent.head();
    begin_pending(&mut parent, 406);
    let pending_parent = snapshot(&parent);
    assert_eq!(
        parent
            .validate_boundary(&parent_head)
            .expect_err("boundary consumption is forbidden while pending exists"),
        BoundaryError::PendingTurn {
            turn_id: turn_id(406),
        }
    );
    assert_eq!(snapshot(&parent), pending_parent);
    assert_eq!(
        parent
            .fork_at(parent_head, conversation_id(498))
            .expect_err("fork is forbidden while pending exists"),
        ConversationError::Boundary(BoundaryError::PendingTurn {
            turn_id: turn_id(406),
        })
    );
    assert_eq!(snapshot(&parent), pending_parent);
    assert_eq!(
        parent
            .revert_to(parent_head)
            .expect_err("revert is forbidden while pending exists"),
        ConversationError::Boundary(BoundaryError::PendingTurn {
            turn_id: turn_id(406),
        })
    );
    assert_eq!(snapshot(&parent), pending_parent);
}
