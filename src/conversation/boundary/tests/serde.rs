//! Stable Boundary token serde and forged-token validation tests.

use super::{commit_text_turn, conversation, forged_boundary, snapshot, turn_id};
use crate::conversation::{Boundary, BoundaryError};
use serde_json::json;

#[test]
fn issued_boundary_has_stable_serde_shape_and_round_trips_as_a_token() {
    let mut conversation = conversation(20);
    commit_text_turn(&mut conversation, 200);
    let issued = conversation
        .boundary_after(turn_id(200))
        .expect("committed turn boundary");

    let encoded = serde_json::to_value(issued).expect("serialize boundary token");

    assert_eq!(
        encoded,
        json!({
            "conversation_id": conversation.id(),
            "turn_count": 1,
            "after_turn": turn_id(200),
            "version": conversation.version(),
        })
    );
    let decoded: Boundary = serde_json::from_value(encoded).expect("deserialize token claims");
    assert_eq!(decoded, issued);
    conversation
        .validate_boundary(&decoded)
        .expect("owning Conversation validates restored token");
}

#[test]
fn forged_position_and_zero_anchor_are_classified_without_state_change() {
    let mut conversation = conversation(21);
    commit_text_turn(&mut conversation, 210);
    let before = snapshot(&conversation);

    let out_of_range = forged_boundary(
        conversation.id(),
        2,
        Some(turn_id(210)),
        conversation.version(),
    );
    assert_eq!(
        conversation
            .validate_boundary(&out_of_range)
            .expect_err("serde range claim needs Conversation proof"),
        BoundaryError::PositionOutOfRange {
            turn_count: 2,
            backing_turns: 1,
        }
    );

    let bad_zero = forged_boundary(
        conversation.id(),
        0,
        Some(turn_id(210)),
        conversation.version(),
    );
    assert_eq!(
        conversation
            .validate_boundary(&bad_zero)
            .expect_err("zero boundary cannot carry a Turn anchor"),
        BoundaryError::AnchorMismatch {
            turn_count: 0,
            expected: None,
            actual: Some(turn_id(210)),
        }
    );

    let missing_anchor = forged_boundary(conversation.id(), 1, None, conversation.version());
    assert_eq!(
        conversation
            .validate_boundary(&missing_anchor)
            .expect_err("nonzero boundary needs its exact Turn anchor"),
        BoundaryError::AnchorMismatch {
            turn_count: 1,
            expected: Some(turn_id(210)),
            actual: None,
        }
    );
    assert_eq!(snapshot(&conversation), before);
}

#[test]
fn serde_treats_missing_optional_anchor_as_none_but_rejects_extended_shapes() {
    let conversation = conversation(22);
    let complete = json!({
        "conversation_id": conversation.id(),
        "turn_count": 0,
        "after_turn": null,
        "version": 0,
    });

    let mut missing_anchor = complete.clone();
    missing_anchor
        .as_object_mut()
        .expect("boundary object")
        .remove("after_turn");
    let decoded: Boundary = serde_json::from_value(missing_anchor)
        .expect("serde represents a missing optional anchor as None");
    assert_eq!(decoded.after_turn(), None);
    conversation
        .validate_boundary(&decoded)
        .expect("missing anchor is canonical for the zero boundary");

    let mut extended = complete;
    extended
        .as_object_mut()
        .expect("boundary object")
        .insert("trusted".to_owned(), json!(true));
    assert!(
        serde_json::from_value::<Boundary>(extended).is_err(),
        "unknown fields cannot smuggle a trust marker"
    );
}
