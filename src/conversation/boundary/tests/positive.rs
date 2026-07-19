//! Successful zero, head, redo-suffix, and fork-prefix Boundary behavior.

use super::{
    commit_text_turn, conversation, conversation_id, message_id, shared_prefix_child, turn_id,
};
use crate::{
    conversation::{ConversationError, PendingTurnError},
    model::{
        content::ContentBlock,
        message::{Message, Role},
    },
};

#[test]
fn empty_conversation_exposes_one_valid_zero_boundary() {
    let conversation = conversation(1);

    let boundaries = conversation.valid_boundaries();

    assert_eq!(boundaries.len(), 1);
    let zero = boundaries[0];
    assert_eq!(zero.conversation_id(), conversation.id());
    assert_eq!(zero.turn_count(), 0);
    assert_eq!(zero.after_turn(), None);
    assert_eq!(zero.version(), 0);
    conversation
        .validate_boundary(&zero)
        .expect("fresh zero boundary is valid");
}

#[test]
fn every_complete_turn_has_one_ordered_boundary_and_direct_lookup() {
    let mut conversation = conversation(2);
    for seed in 10..13 {
        commit_text_turn(&mut conversation, seed);
    }

    let boundaries = conversation.valid_boundaries();

    assert_eq!(boundaries.len(), 4);
    assert_eq!(
        boundaries
            .iter()
            .map(|boundary| boundary.turn_count())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        boundaries
            .iter()
            .map(|boundary| boundary.after_turn())
            .collect::<Vec<_>>(),
        vec![
            None,
            Some(turn_id(10)),
            Some(turn_id(11)),
            Some(turn_id(12))
        ]
    );
    assert!(
        boundaries
            .iter()
            .all(|boundary| boundary.version() == conversation.version())
    );

    for (turn, boundary) in conversation.turns().iter().zip(&boundaries[1..]) {
        assert_eq!(
            conversation
                .boundary_after(turn.id())
                .expect("current-lineage turn has a boundary"),
            *boundary
        );
        conversation
            .validate_boundary(boundary)
            .expect("fresh boundary validates");
    }
}

#[test]
fn reverted_head_still_issues_fresh_boundaries_for_same_lineage_redo_suffix() {
    let mut conversation = conversation(3);
    for seed in 20..23 {
        commit_text_turn(&mut conversation, seed);
    }

    let first_turn = conversation
        .boundary_after(turn_id(20))
        .expect("first turn has a checked boundary");
    conversation
        .revert_to(first_turn)
        .expect("checked head movement succeeds");

    assert_eq!(conversation.turns().len(), 1, "active head moved back");
    let boundaries = conversation.valid_boundaries();
    assert_eq!(boundaries.len(), 4, "redo suffix remains addressable");
    assert_eq!(boundaries[1].after_turn(), Some(turn_id(20)));
    assert_eq!(boundaries[2].after_turn(), Some(turn_id(21)));
    assert_eq!(boundaries[3].after_turn(), Some(turn_id(22)));
    assert_eq!(
        conversation
            .boundary_after(turn_id(22))
            .expect("future suffix has a fresh redo token"),
        boundaries[3]
    );
    for boundary in boundaries {
        conversation
            .validate_boundary(&boundary)
            .expect("zero, head, and future redo boundaries are valid");
    }
}

#[test]
fn shared_prefix_child_exposes_only_its_ceiling_without_copying_turn_messages() {
    let mut parent = conversation(4);
    for seed in 30..33 {
        commit_text_turn(&mut parent, seed);
    }
    let child = shared_prefix_child(&parent, conversation_id(5), 1);

    let boundaries = child.valid_boundaries();

    assert_eq!(child.turns().len(), 1);
    assert_eq!(boundaries.len(), 2);
    assert_eq!(boundaries[1].after_turn(), Some(turn_id(30)));
    assert_eq!(child.raw_turn(turn_id(30)), parent.raw_turn(turn_id(30)));
    assert!(child.raw_turn(turn_id(31)).is_none());
    assert_eq!(
        child.turns()[0].messages().as_ptr(),
        parent.turns()[0].messages().as_ptr(),
        "shared prefix keeps immutable message storage"
    );
    child
        .validate_boundary(&boundaries[1])
        .expect("child ceiling boundary validates under child owner/version");
}

#[test]
fn shared_prefix_child_message_id_index_covers_only_its_ceiling() {
    let mut parent = conversation(6);
    for seed in 40..43 {
        commit_text_turn(&mut parent, seed);
    }
    let mut child = shared_prefix_child(&parent, conversation_id(7), 1);
    let user = |value: &str| Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: value.to_owned(),
            extra: Default::default(),
        }],
    };

    // Message ids from the retained prefix stay reserved for the child: the
    // fork-built index must cover the shared prefix, not just turns the child
    // appended itself.
    let duplicate = child
        .begin_turn(
            turn_id(50),
            message_id(40 * 10),
            user("duplicate prefix message"),
        )
        .expect_err("prefix message id is retained in the child index");
    assert_eq!(
        duplicate,
        ConversationError::PendingTurn(PendingTurnError::DuplicateMessageId {
            message_id: message_id(40 * 10),
        })
    );

    // Message ids from turns beyond the fork ceiling are not part of the
    // child's raw history, so the child may reuse them.
    child
        .begin_turn(
            turn_id(51),
            message_id(41 * 10),
            user("beyond-ceiling message"),
        )
        .expect("beyond-ceiling message id is not retained for the child");
}
