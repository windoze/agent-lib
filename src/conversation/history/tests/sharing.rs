//! Structural-sharing and append immutability tests.

use super::{commit_text_turn, conversation, turn_id};
use std::sync::Arc;

#[test]
fn long_history_clone_shares_all_handles_and_append_preserves_prefix() {
    let mut conversation = conversation();
    for offset in 0_u128..128 {
        commit_text_turn(&mut conversation, 10 + offset, 1_000 + offset * 2);
    }

    let cloned = conversation.history.clone();
    assert!(Arc::ptr_eq(&conversation.history.lineage, &cloned.lineage));
    let shared_raw_tip = cloned.raw.local_tip.as_ref().expect("raw tip").clone();
    assert!(Arc::ptr_eq(
        conversation
            .history
            .raw
            .local_tip
            .as_ref()
            .expect("raw tip"),
        &shared_raw_tip
    ));
    assert_eq!(cloned.turns().len(), 128);

    let first_message_storage = cloned.turns()[0].messages().as_ptr();
    let first_payload = cloned.turns()[0].messages()[0].payload().clone();
    commit_text_turn(&mut conversation, 500, 5_000);

    assert_eq!(conversation.turns().len(), 129);
    assert_eq!(cloned.turns().len(), 128);
    assert!(cloned.raw_turn(turn_id(500)).is_none());
    assert_eq!(
        conversation.turns()[0].messages().as_ptr(),
        first_message_storage
    );
    assert_eq!(
        conversation.turns()[0].messages()[0].payload(),
        &first_payload
    );
    let appended_tip = conversation
        .history
        .raw
        .local_tip
        .as_ref()
        .expect("appended raw tip");
    assert!(Arc::ptr_eq(
        appended_tip.previous.as_ref().expect("shared raw prefix"),
        &shared_raw_tip
    ));
    assert_eq!(
        cloned.raw_turn(turn_id(10)).expect("shared first raw turn"),
        conversation
            .raw_turn(turn_id(10))
            .expect("first raw turn remains")
    );
}
