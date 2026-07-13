//! Replacement-suffix construction after a logical revert.

use super::super::{
    assert_index_matches_rebuild, call_id, commit_text_turn, commit_tool_turn, conversation,
    turn_id,
};
use crate::conversation::BoundaryError;

#[test]
fn commit_after_revert_builds_a_new_parent_path_and_detaches_the_old_suffix() {
    let mut conversation = conversation(230);
    commit_text_turn(&mut conversation, 231);
    commit_tool_turn(&mut conversation, 232, "old-branch-call", 9_232);
    commit_text_turn(&mut conversation, 233);
    let old_branch_message_storage = conversation
        .raw_turn(turn_id(232))
        .expect("old branch Turn exists")
        .messages()
        .as_ptr();

    let branch_point = conversation
        .boundary_after(turn_id(231))
        .expect("branch point has a checked boundary");
    let reverted = conversation
        .revert_to(branch_point)
        .expect("move head to branch point");
    let freshly_reissued_old_head = reverted.old_head();

    commit_tool_turn(&mut conversation, 234, "new-branch-call", 9_234);

    assert_eq!(
        conversation
            .turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(231), turn_id(234)]
    );
    assert_eq!(
        conversation
            .lineage_turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(231), turn_id(234)],
        "the old suffix is no longer redo-addressable after branching"
    );
    assert_eq!(
        conversation
            .raw_turns()
            .into_iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(231), turn_id(232), turn_id(233), turn_id(234)],
        "all branches remain in append-only raw insertion order"
    );
    assert_eq!(
        conversation
            .raw_turn(turn_id(232))
            .expect("detached tool Turn remains retained")
            .messages()
            .as_ptr(),
        old_branch_message_storage
    );

    let first = conversation.raw_turn(turn_id(231)).expect("root Turn");
    let old_second = conversation.raw_turn(turn_id(232)).expect("old child");
    let old_third = conversation.raw_turn(turn_id(233)).expect("old grandchild");
    let new_second = conversation.raw_turn(turn_id(234)).expect("new child");
    assert_eq!(first.parent(), None);
    assert_eq!(old_second.parent(), Some(turn_id(231)));
    assert_eq!(old_third.parent(), Some(turn_id(232)));
    assert_eq!(new_second.parent(), Some(turn_id(231)));

    assert!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(9_232))
            .is_none(),
        "detached branch calls do not leak into the effective index"
    );
    assert!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(9_234))
            .is_some()
    );
    assert_index_matches_rebuild(&conversation);
    assert_eq!(
        conversation
            .boundary_after(turn_id(232))
            .expect_err("detached suffix can no longer be redone"),
        BoundaryError::TurnNotOnLineage {
            turn_id: turn_id(232),
        }
    );
    assert_eq!(
        conversation
            .validate_boundary(&freshly_reissued_old_head)
            .expect_err("the branch commit invalidates pre-commit redo tokens"),
        BoundaryError::StaleBoundary {
            boundary_version: freshly_reissued_old_head.version(),
            current_version: conversation.version(),
        }
    );

    let boundaries = conversation.valid_boundaries();
    assert_eq!(
        boundaries
            .iter()
            .map(|boundary| boundary.after_turn())
            .collect::<Vec<_>>(),
        vec![None, Some(turn_id(231)), Some(turn_id(234))]
    );
    conversation
        .revert_to(boundaries[0])
        .expect("the replacement lineage can be reverted again");
    assert!(conversation.turns().is_empty());
    assert_eq!(conversation.raw_turns().len(), 4);

    let replacement_tip = conversation
        .boundary_after(turn_id(234))
        .expect("replacement suffix alone remains redo-addressable");
    conversation
        .revert_to(replacement_tip)
        .expect("replacement lineage can be redone");
    assert_eq!(
        conversation
            .turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(231), turn_id(234)]
    );
    assert_index_matches_rebuild(&conversation);
}
