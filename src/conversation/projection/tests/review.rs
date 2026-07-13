//! Milestone 4 review matrix across projection, compaction, revert, fork, and pending cuts.

use super::{
    begin_pending, commit_text_turn, conversation, conversation_id, message_labels, range,
    raw_compaction_plan, raw_history_snapshot, span_compaction_plan, strategy, turn_id,
};
use crate::{
    conversation::{ConversationError, ProjectionError, Span},
    model::message::Role,
};

#[test]
fn review_matrix_preserves_raw_history_head_clipping_fork_scope_and_pending_isolation() {
    let mut parent = conversation(260);
    commit_text_turn(&mut parent, 261);
    commit_text_turn(&mut parent, 262);
    commit_text_turn(&mut parent, 263);
    commit_text_turn(&mut parent, 264);
    commit_text_turn(&mut parent, 265);
    let raw_before = raw_history_snapshot(&parent);

    let (first_plan, first_artifact) = raw_compaction_plan(
        &parent,
        range(&parent, 0, 2),
        2600,
        strategy("review-tier-a"),
        "turns 261-262 tier summary",
    );
    parent
        .apply_compaction(&first_plan)
        .expect("first tiered compaction applies");

    let (second_plan, second_artifact) = raw_compaction_plan(
        &parent,
        range(&parent, 2, 4),
        2601,
        strategy("review-tier-b"),
        "turns 263-264 tier summary",
    );
    parent
        .apply_compaction(&second_plan)
        .expect("second tiered compaction applies");

    let (consolidate_plan, consolidated_artifact) = span_compaction_plan(
        &parent,
        range(&parent, 0, 4),
        2602,
        strategy("review-consolidate"),
        "turns 261-264 consolidated summary",
    );
    let encoded_plan = serde_json::to_string(&consolidate_plan).expect("serialize data-only plan");
    assert!(encoded_plan.contains("review-consolidate"));
    assert!(!encoded_plan.contains("registry"));
    assert!(!encoded_plan.contains("client"));
    assert!(!encoded_plan.contains("agent_loop"));

    parent
        .apply_compaction(&consolidate_plan)
        .expect("summary-of-summaries compaction applies");

    assert_eq!(
        raw_history_snapshot(&parent),
        raw_before,
        "compaction only changes projection overlay, never raw turns/messages"
    );
    assert_eq!(parent.projection().spans().len(), 2);
    assert!(matches!(
        parent.projection().spans()[0],
        Span::Compacted { .. }
    ));
    assert!(matches!(parent.projection().spans()[1], Span::Raw { .. }));
    assert_eq!(parent.projection().artifacts().len(), 3);
    assert!(
        parent.projection().artifact(first_artifact.id()).is_some(),
        "replaced tier artifact remains available for provenance/audit"
    );
    assert!(
        parent.projection().artifact(second_artifact.id()).is_some(),
        "second replaced tier artifact remains available for provenance/audit"
    );
    let consolidated = parent
        .projection()
        .artifact(consolidated_artifact.id())
        .expect("consolidated artifact is retained");
    assert_eq!(
        consolidated.provenance().input_range().start_turn_count(),
        0
    );
    assert_eq!(consolidated.provenance().input_range().end_turn_count(), 4);
    assert_eq!(consolidated.provenance().produced_by().name(), "summary");
    assert_eq!(
        consolidated.provenance().produced_by().version(),
        "review-consolidate"
    );
    assert_eq!(consolidated.provenance().tokens().before().input, 100);
    assert_eq!(consolidated.provenance().tokens().after().input, 12);

    assert_eq!(
        message_labels(parent.effective_view().messages()),
        vec![
            (
                Role::Assistant,
                "turns 261-264 consolidated summary".to_owned(),
            ),
            (Role::User, "question:265".to_owned()),
            (Role::Assistant, "answer:265".to_owned()),
        ]
    );

    let inside_consolidated_cover = parent.valid_boundaries()[2];
    let redo = parent
        .revert_to(inside_consolidated_cover)
        .expect("revert into consolidated cover")
        .old_head();
    assert_eq!(
        message_labels(parent.effective_view().messages()),
        vec![
            (Role::User, "question:261".to_owned()),
            (Role::Assistant, "answer:261".to_owned()),
            (Role::User, "question:262".to_owned()),
            (Role::Assistant, "answer:262".to_owned()),
        ],
        "head inside a compacted cover renders only the visible raw prefix"
    );
    assert!(
        !message_labels(parent.effective_view().messages())
            .iter()
            .any(|(_, text)| {
                text.contains("summary") || text.contains("263") || text.contains("264")
            }),
        "effective_view must not leak future summary content after revert"
    );
    assert_eq!(raw_history_snapshot(&parent), raw_before);

    parent
        .revert_to(redo)
        .expect("redo to the full projection cover");
    assert_eq!(
        message_labels(parent.effective_view().messages()),
        vec![
            (
                Role::Assistant,
                "turns 261-264 consolidated summary".to_owned(),
            ),
            (Role::User, "question:265".to_owned()),
            (Role::Assistant, "answer:265".to_owned()),
        ],
        "redo restores compacted rendering only after the full cover is visible"
    );

    let child = parent
        .fork_at(parent.valid_boundaries()[3], conversation_id(2609))
        .expect("fork from inside the parent projection cover");
    assert_eq!(raw_history_snapshot(&child), raw_before[..3].to_vec());
    assert_eq!(child.projection().artifacts().len(), 0);
    assert_eq!(
        message_labels(child.effective_view().messages()),
        vec![
            (Role::User, "question:261".to_owned()),
            (Role::Assistant, "answer:261".to_owned()),
            (Role::User, "question:262".to_owned()),
            (Role::Assistant, "answer:262".to_owned()),
            (Role::User, "question:263".to_owned()),
            (Role::Assistant, "answer:263".to_owned()),
        ],
        "fork child renders only its ceiling-limited raw prefix"
    );
    assert!(
        !message_labels(child.effective_view().messages())
            .iter()
            .any(|(_, text)| {
                text.contains("summary") || text.contains("264") || text.contains("265")
            }),
        "parent summary and parent suffix do not become child facts"
    );

    let pending_plan = raw_compaction_plan(
        &parent,
        range(&parent, 4, 5),
        2603,
        strategy("review-pending"),
        "turn 265 pending summary",
    )
    .0;
    let projection_before_pending = parent.projection().clone();
    let raw_before_pending = raw_history_snapshot(&parent);
    begin_pending(&mut parent, 266);

    assert_eq!(
        parent
            .apply_compaction(&pending_plan)
            .expect_err("pending turns cannot be compacted or covered"),
        ConversationError::Projection(ProjectionError::PendingTurn {
            turn_id: turn_id(266),
        })
    );
    assert_eq!(parent.projection(), &projection_before_pending);
    assert_eq!(raw_history_snapshot(&parent), raw_before_pending);
    assert_eq!(
        message_labels(parent.effective_view().messages()),
        vec![
            (
                Role::Assistant,
                "turns 261-264 consolidated summary".to_owned(),
            ),
            (Role::User, "question:265".to_owned()),
            (Role::Assistant, "answer:265".to_owned()),
        ],
        "committed effective_view remains isolated from pending"
    );
    assert_eq!(
        message_labels(
            parent
                .pending_context()
                .expect("pending context is explicit")
                .messages()
        ),
        vec![(Role::User, "pending".to_owned())]
    );
}
