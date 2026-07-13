//! Successful backward/forward head movement and derived-view synchronization.

use super::super::{
    assert_index_matches_rebuild, call_id, commit_text_turn, commit_tool_turn, conversation,
    snapshot, turn_id,
};
use crate::conversation::BoundaryError;

#[test]
fn repeated_revert_redo_and_zero_moves_preserve_raw_facts_and_refresh_index() {
    let mut conversation = conversation(200);
    commit_text_turn(&mut conversation, 210);
    commit_tool_turn(&mut conversation, 211, "redo-call", 9_211);
    commit_text_turn(&mut conversation, 212);

    let raw_before = conversation
        .raw_turns()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let message_storage_before = conversation
        .raw_turns()
        .into_iter()
        .map(|turn| turn.messages().as_ptr())
        .collect::<Vec<_>>();
    let supplied_first = conversation
        .boundary_after(turn_id(210))
        .expect("first turn has a boundary");
    let version_before = conversation.version();

    let reverted = conversation
        .revert_to(supplied_first)
        .expect("backward head movement succeeds");

    assert!(reverted.changed());
    assert_eq!(reverted.old_head().after_turn(), Some(turn_id(212)));
    assert_eq!(reverted.new_head().after_turn(), Some(turn_id(210)));
    assert_eq!(conversation.head(), reverted.new_head());
    assert_eq!(conversation.version(), version_before + 1);
    assert_eq!(reverted.old_head().version(), conversation.version());
    assert_eq!(reverted.new_head().version(), conversation.version());
    conversation
        .validate_boundary(&reverted.old_head())
        .expect("reissued old head is a valid redo token");
    conversation
        .validate_boundary(&reverted.new_head())
        .expect("reissued new head is valid");
    assert_eq!(
        conversation
            .validate_boundary(&supplied_first)
            .expect_err("the caller's pre-move token is stale"),
        BoundaryError::StaleBoundary {
            boundary_version: supplied_first.version(),
            current_version: conversation.version(),
        }
    );
    assert_eq!(
        conversation
            .turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(210)]
    );
    assert_eq!(
        conversation
            .lineage_turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(210), turn_id(211), turn_id(212)]
    );
    assert!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(9_211))
            .is_none(),
        "a call beyond head is not currently visible"
    );
    assert_index_matches_rebuild(&conversation);

    let fresh_last = conversation
        .boundary_after(turn_id(212))
        .expect("same-lineage suffix receives a fresh redo token");
    let redone = conversation
        .revert_to(fresh_last)
        .expect("forward head movement performs redo");

    assert!(redone.changed());
    assert_eq!(redone.old_head().after_turn(), Some(turn_id(210)));
    assert_eq!(redone.new_head().after_turn(), Some(turn_id(212)));
    assert_eq!(conversation.turns(), conversation.lineage_turns());
    assert!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(9_211))
            .is_some(),
        "redo restores calls derived from the visible prefix"
    );
    assert_index_matches_rebuild(&conversation);

    let zero = conversation.valid_boundaries()[0];
    conversation
        .revert_to(zero)
        .expect("zero boundary is a valid logical head");
    assert!(conversation.turns().is_empty());
    assert_eq!(conversation.head().turn_count(), 0);
    assert!(conversation.tool_call_index().is_empty());
    assert_eq!(conversation.lineage_turns().len(), 3);
    assert_index_matches_rebuild(&conversation);

    let fresh_middle = conversation
        .boundary_after(turn_id(211))
        .expect("middle suffix boundary remains redo-addressable");
    conversation
        .revert_to(fresh_middle)
        .expect("redo may stop at any complete Turn boundary");
    assert_eq!(
        conversation
            .turns()
            .iter()
            .map(|turn| turn.id())
            .collect::<Vec<_>>(),
        vec![turn_id(210), turn_id(211)]
    );
    assert!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(9_211))
            .is_some()
    );
    assert_eq!(
        conversation
            .raw_turns()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        raw_before,
        "head movement never edits raw Turn facts"
    );
    assert_eq!(
        conversation
            .raw_turns()
            .into_iter()
            .map(|turn| turn.messages().as_ptr())
            .collect::<Vec<_>>(),
        message_storage_before,
        "head movement preserves shared immutable message storage"
    );
    assert_index_matches_rebuild(&conversation);
}

#[test]
fn targeting_the_current_head_is_an_observable_version_preserving_noop() {
    let mut conversation = conversation(201);
    commit_text_turn(&mut conversation, 220);
    let current = conversation.head();
    let before = snapshot(&conversation);

    let outcome = conversation
        .revert_to(current)
        .expect("current head is a valid target");

    assert!(!outcome.changed());
    assert_eq!(outcome.old_head(), current);
    assert_eq!(outcome.new_head(), current);
    assert_eq!(snapshot(&conversation), before);
}
