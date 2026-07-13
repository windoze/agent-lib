//! Internal structural-sharing checks for checked fork creation.

use super::{commit_text_turn, conversation, conversation_id, turn_id};
use std::sync::Arc;

#[test]
fn fork_shares_lineage_and_index_backing_without_copying_prefix() {
    let mut parent = conversation();
    for offset in 0_u128..128 {
        commit_text_turn(&mut parent, 10 + offset, 1_000 + offset * 2);
    }
    let fork_point = parent
        .boundary_after(turn_id(73))
        .expect("prefix turn has a checked boundary");
    let child = parent
        .fork_at(fork_point, conversation_id(900))
        .expect("fork from a valid boundary");

    assert!(Arc::ptr_eq(&parent.history.lineage, &child.history.lineage));
    assert!(Arc::ptr_eq(
        &parent.history.lineage,
        &child.history.raw.base
    ));
    assert!(Arc::ptr_eq(
        &parent.history.lineage.nodes[63],
        &child.history.lineage.nodes[63],
    ));
    assert_eq!(
        parent.history.lineage.turns.as_ptr(),
        child.history.lineage.turns.as_ptr(),
        "fork must not clone the materialized lineage Vec"
    );
    assert_eq!(
        parent.turns()[63].messages().as_ptr(),
        child.turns()[63].messages().as_ptr(),
        "fork must not clone immutable message storage"
    );
    assert_eq!(child.history.raw.base_len, 64);
    assert_eq!(child.history.lineage_len, 64);
    assert_eq!(child.history.active_len, 64);
    assert!(child.history.raw.local_tip.is_none());
    assert!(child.raw_turn(turn_id(73)).is_some());
    assert!(child.raw_turn(turn_id(74)).is_none());
    assert!(
        parent
            .tool_call_index
            .committed_ptr_eq(&child.tool_call_index)
    );
    assert_eq!(child.tool_call_index.visible_committed_turns(), 64);
}
