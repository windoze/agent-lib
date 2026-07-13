//! Projection model and checked range tests.

use super::{
    Artifact, ArtifactProvenance, CheckedTurnRange, Projection, RangeEndpoint, Span, StrategyRef,
    TokenAccounting,
};
use crate::{
    client::Response,
    conversation::{
        ArtifactId, BoundaryError, Conversation, ConversationConfig, ConversationError,
        ConversationId, MessageId, ProjectionError, TurnId, TurnMeta,
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        usage::Usage,
    },
};
use serde_json::{Map, json};
use uuid::Uuid;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_8f60_0000_0000_0000;

fn conversation_id(seed: u128) -> ConversationId {
    ConversationId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn turn_id(seed: u128) -> TurnId {
    TurnId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn message_id(seed: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn artifact_id(seed: u128) -> ArtifactId {
    ArtifactId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn conversation(seed: u128) -> Conversation {
    Conversation::new(
        conversation_id(seed),
        ConversationConfig::new(Some("Project carefully.".to_owned())),
    )
}

fn text(value: impl Into<String>) -> ContentBlock {
    ContentBlock::Text {
        text: value.into(),
        extra: Map::new(),
    }
}

fn summary_message(value: impl Into<String>) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![text(value)],
    }
}

fn commit_text_turn(conversation: &mut Conversation, seed: u128) {
    conversation
        .begin_turn(
            turn_id(seed),
            message_id(seed * 10),
            Message {
                role: Role::User,
                content: vec![text(format!("question:{seed}"))],
            },
        )
        .expect("begin text turn");
    conversation
        .start_assistant_response(Response {
            message: Message {
                role: Role::Assistant,
                content: vec![text(format!("answer:{seed}"))],
            },
            usage: Usage::default(),
            stop_reason: StopReason::normalize("end_turn"),
            extra: Map::new(),
        })
        .expect("start assistant response");
    conversation
        .finish_assistant(message_id(seed * 10 + 1))
        .expect("finish assistant response");
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit text turn");
}

fn begin_pending(conversation: &mut Conversation, seed: u128) {
    conversation
        .begin_turn(
            turn_id(seed),
            message_id(seed * 10),
            Message {
                role: Role::User,
                content: vec![text("pending")],
            },
        )
        .expect("begin pending turn");
}

fn strategy(version: &str) -> StrategyRef {
    StrategyRef::new("summary", version)
}

fn accounting(before: u32, after: u32) -> TokenAccounting {
    TokenAccounting::new(
        Usage {
            input: before,
            ..Usage::default()
        },
        Usage {
            input: after,
            ..Usage::default()
        },
    )
}

fn artifact(
    id_seed: u128,
    range: CheckedTurnRange,
    produced_by: StrategyRef,
    label: &str,
) -> Artifact {
    Artifact::new(
        artifact_id(id_seed),
        vec![summary_message(label)],
        ArtifactProvenance::new(range, produced_by, accounting(100, 12), Map::new()),
    )
    .expect("valid artifact")
}

fn range(conversation: &Conversation, start_index: usize, end_index: usize) -> CheckedTurnRange {
    let boundaries = conversation.valid_boundaries();
    conversation
        .checked_turn_range(boundaries[start_index], boundaries[end_index])
        .expect("checked range")
}

fn forged_range(
    conversation_id: ConversationId,
    start_turn_count: u64,
    start_after_turn: Option<TurnId>,
    end_turn_count: u64,
    end_after_turn: Option<TurnId>,
) -> CheckedTurnRange {
    CheckedTurnRange {
        conversation_id,
        start: RangeEndpoint {
            turn_count: start_turn_count,
            after_turn: start_after_turn,
        },
        end: RangeEndpoint {
            turn_count: end_turn_count,
            after_turn: end_after_turn,
        },
    }
}

#[test]
fn raw_projection_and_compacted_artifacts_round_trip_through_serde() {
    let mut conversation = conversation(1);
    commit_text_turn(&mut conversation, 11);
    commit_text_turn(&mut conversation, 12);
    commit_text_turn(&mut conversation, 13);

    assert_eq!(conversation.projection().spans().len(), 1);
    assert!(matches!(
        conversation.projection().spans()[0],
        Span::Raw { .. }
    ));

    let first = range(&conversation, 0, 1);
    let middle = range(&conversation, 1, 2);
    let last = range(&conversation, 2, 3);
    let strategy_v1 = strategy("v1");
    let middle_artifact = artifact(90, middle.clone(), strategy_v1.clone(), "middle summary");
    let projection = Projection::new(
        &conversation,
        vec![
            Span::raw(first.clone()),
            Span::compacted(middle.clone(), middle_artifact.id(), strategy_v1.clone()),
            Span::raw(last.clone()),
        ],
        vec![middle_artifact.clone()],
    )
    .expect("projection covers the complete head");

    assert_eq!(projection.spans().len(), 3);
    assert_eq!(
        projection
            .artifact(middle_artifact.id())
            .expect("artifact lookup")
            .messages()[0],
        summary_message("middle summary")
    );
    assert_eq!(
        projection.artifacts()[0]
            .provenance()
            .tokens()
            .before()
            .input,
        100
    );
    assert_eq!(middle_artifact.provenance().produced_by(), &strategy_v1);

    let encoded = serde_json::to_string(&projection).expect("serialize projection");
    let decoded: Projection = serde_json::from_str(&encoded).expect("deserialize projection");
    assert_eq!(decoded, projection);
}

#[test]
fn multiple_compacted_artifacts_can_describe_tiered_projection_segments() {
    let mut conversation = conversation(2);
    commit_text_turn(&mut conversation, 21);
    commit_text_turn(&mut conversation, 22);
    commit_text_turn(&mut conversation, 23);
    commit_text_turn(&mut conversation, 24);

    let first = range(&conversation, 0, 1);
    let middle = range(&conversation, 1, 3);
    let last = range(&conversation, 3, 4);
    let first_strategy = strategy("tier-1");
    let middle_strategy = strategy("tier-2");
    let first_artifact = artifact(91, first.clone(), first_strategy.clone(), "first summary");
    let middle_artifact = artifact(
        92,
        middle.clone(),
        middle_strategy.clone(),
        "middle summary",
    );

    let projection = Projection::new(
        &conversation,
        vec![
            Span::compacted(first.clone(), first_artifact.id(), first_strategy),
            Span::compacted(middle.clone(), middle_artifact.id(), middle_strategy),
            Span::raw(last),
        ],
        vec![first_artifact, middle_artifact],
    )
    .expect("multiple artifact spans can cover the head");

    assert_eq!(projection.spans().len(), 3);
    assert_eq!(projection.artifacts().len(), 2);
}

#[test]
fn checked_ranges_revalidate_after_structural_version_changes() {
    let mut conversation = conversation(3);
    commit_text_turn(&mut conversation, 31);
    commit_text_turn(&mut conversation, 32);
    let first_turn = range(&conversation, 0, 1);
    let old_boundary = conversation.valid_boundaries()[1];

    commit_text_turn(&mut conversation, 33);

    assert_eq!(
        conversation
            .validate_boundary(&old_boundary)
            .expect_err("old boundary token is stale"),
        BoundaryError::StaleBoundary {
            boundary_version: 2,
            current_version: 3,
        }
    );
    conversation
        .validate_checked_turn_range(&first_turn)
        .expect("range revalidates by stable anchors, not boundary version");
}

#[test]
fn checked_range_rejects_foreign_pending_reversed_and_beyond_head_inputs() {
    let mut conversation = conversation(4);
    commit_text_turn(&mut conversation, 41);
    commit_text_turn(&mut conversation, 42);
    let boundaries = conversation.valid_boundaries();

    let foreign = self::conversation(5);
    let foreign_start = foreign.valid_boundaries()[0];
    assert!(matches!(
        conversation.checked_turn_range(foreign_start, boundaries[1]),
        Err(ConversationError::Boundary(
            BoundaryError::OwnerMismatch { .. }
        ))
    ));

    assert_eq!(
        conversation
            .checked_turn_range(boundaries[2], boundaries[1])
            .expect_err("reversed ranges are rejected"),
        ConversationError::Projection(ProjectionError::ReversedRange { start: 2, end: 1 })
    );

    let first_boundary = boundaries[1];
    conversation
        .revert_to(first_boundary)
        .expect("move head before redo suffix");
    let redo_boundaries = conversation.valid_boundaries();
    assert_eq!(
        conversation
            .checked_turn_range(redo_boundaries[0], redo_boundaries[2])
            .expect_err("ranges cannot cover turns beyond head"),
        ConversationError::Projection(ProjectionError::RangeBeyondHead { end: 2, head: 1 })
    );

    let checked = conversation
        .checked_turn_range(redo_boundaries[0], redo_boundaries[1])
        .expect("range before pending");
    begin_pending(&mut conversation, 43);
    assert_eq!(
        conversation
            .validate_checked_turn_range(&checked)
            .expect_err("pending blocks projection range use"),
        ConversationError::Projection(ProjectionError::PendingTurn {
            turn_id: turn_id(43),
        })
    );
}

#[test]
fn projection_rejects_gaps_overlaps_missing_artifacts_and_duplicate_artifact_ids() {
    let mut conversation = conversation(6);
    commit_text_turn(&mut conversation, 61);
    commit_text_turn(&mut conversation, 62);

    let first = range(&conversation, 0, 1);
    let second = range(&conversation, 1, 2);
    let both = range(&conversation, 0, 2);
    let strategy_v1 = strategy("v1");
    let valid_artifact = artifact(93, first.clone(), strategy_v1.clone(), "summary");

    assert_eq!(
        Projection::new(&conversation, vec![Span::raw(second.clone())], Vec::new())
            .expect_err("gap before first span"),
        ConversationError::Projection(ProjectionError::SpanGap {
            expected_start: 0,
            actual_start: 1,
        })
    );

    assert_eq!(
        Projection::new(
            &conversation,
            vec![Span::raw(both), Span::raw(second.clone())],
            Vec::new(),
        )
        .expect_err("second span overlaps first"),
        ConversationError::Projection(ProjectionError::SpanOverlap {
            expected_start: 2,
            actual_start: 1,
        })
    );

    assert_eq!(
        Projection::new(&conversation, vec![Span::raw(first.clone())], Vec::new())
            .expect_err("projection must cover the full head"),
        ConversationError::Projection(ProjectionError::IncompleteProjection {
            expected_end: 2,
            actual_end: 1,
        })
    );

    assert_eq!(
        Projection::new(
            &conversation,
            vec![
                Span::compacted(first.clone(), valid_artifact.id(), strategy_v1.clone()),
                Span::raw(second.clone()),
            ],
            Vec::new(),
        )
        .expect_err("compacted span must reference a supplied artifact"),
        ConversationError::Projection(ProjectionError::MissingArtifact {
            artifact_id: valid_artifact.id(),
        })
    );

    assert_eq!(
        Projection::new(
            &conversation,
            vec![
                Span::compacted(first.clone(), valid_artifact.id(), strategy_v1),
                Span::raw(second),
            ],
            vec![valid_artifact.clone(), valid_artifact.clone()],
        )
        .expect_err("duplicate artifact ids are rejected"),
        ConversationError::Projection(ProjectionError::DuplicateArtifactId {
            artifact_id: valid_artifact.id(),
        })
    );
}

#[test]
fn projection_rejects_artifact_provenance_mismatches_and_empty_render_content() {
    let mut conversation = conversation(7);
    commit_text_turn(&mut conversation, 71);
    commit_text_turn(&mut conversation, 72);

    let first = range(&conversation, 0, 1);
    let second = range(&conversation, 1, 2);
    let strategy_v1 = strategy("v1");
    let strategy_v2 = strategy("v2");
    let wrong_range_artifact = artifact(94, second.clone(), strategy_v1.clone(), "wrong range");
    let wrong_strategy_artifact =
        artifact(95, first.clone(), strategy_v2.clone(), "wrong strategy");

    assert_eq!(
        Artifact::new(
            artifact_id(96),
            Vec::new(),
            ArtifactProvenance::new(
                first.clone(),
                strategy_v1.clone(),
                accounting(10, 1),
                Map::new(),
            ),
        )
        .expect_err("artifact render messages are required"),
        ProjectionError::EmptyArtifactMessages {
            artifact_id: artifact_id(96),
        }
    );

    assert_eq!(
        Projection::new(
            &conversation,
            vec![
                Span::compacted(
                    first.clone(),
                    wrong_range_artifact.id(),
                    strategy_v1.clone()
                ),
                Span::raw(second.clone()),
            ],
            vec![wrong_range_artifact.clone()],
        )
        .expect_err("artifact provenance range must match span"),
        ConversationError::Projection(ProjectionError::ArtifactRangeMismatch {
            artifact_id: wrong_range_artifact.id(),
        })
    );

    assert_eq!(
        Projection::new(
            &conversation,
            vec![
                Span::compacted(first.clone(), wrong_strategy_artifact.id(), strategy_v1),
                Span::raw(second),
            ],
            vec![wrong_strategy_artifact.clone()],
        )
        .expect_err("artifact provenance strategy must match span"),
        ConversationError::Projection(ProjectionError::ArtifactStrategyMismatch {
            artifact_id: wrong_strategy_artifact.id(),
        })
    );
}

#[test]
fn checked_range_rejects_unknown_and_detached_turn_anchors() {
    let mut conversation = conversation(8);
    commit_text_turn(&mut conversation, 81);
    commit_text_turn(&mut conversation, 82);
    commit_text_turn(&mut conversation, 83);

    let unknown = forged_range(conversation.id(), 0, None, 1, Some(turn_id(999)));
    assert_eq!(
        conversation
            .validate_checked_turn_range(&unknown)
            .expect_err("unknown anchor rejected"),
        ConversationError::Projection(ProjectionError::UnknownTurn {
            turn_id: turn_id(999),
        })
    );

    let detached_range = range(&conversation, 2, 3);
    let branch_point = conversation
        .boundary_after(turn_id(82))
        .expect("boundary before old suffix");
    conversation
        .revert_to(branch_point)
        .expect("revert before old suffix");
    commit_text_turn(&mut conversation, 84);

    assert_eq!(
        conversation
            .validate_checked_turn_range(&detached_range)
            .expect_err("old suffix anchor is detached after branch commit"),
        ConversationError::Projection(ProjectionError::DetachedTurn {
            turn_id: turn_id(83),
        })
    );
}

#[test]
fn serde_rejects_locally_invalid_artifact_and_projection_shapes() {
    let mut conversation = conversation(9);
    commit_text_turn(&mut conversation, 91);
    let first = range(&conversation, 0, 1);
    let strategy_v1 = strategy("v1");

    let empty_artifact = json!({
        "id": artifact_id(97),
        "messages": [],
        "provenance": ArtifactProvenance::new(
            first.clone(),
            strategy_v1.clone(),
            accounting(10, 1),
            Map::new(),
        ),
    });
    let artifact_error =
        serde_json::from_value::<Artifact>(empty_artifact).expect_err("empty artifact rejected");
    assert!(
        artifact_error
            .to_string()
            .contains("has no render messages"),
        "{artifact_error}"
    );

    let missing_artifact_projection = json!({
        "spans": [{
            "kind": "compacted",
            "covers": first,
            "artifact": artifact_id(98),
            "produced_by": strategy_v1,
        }],
        "artifacts": [],
    });
    let projection_error = serde_json::from_value::<Projection>(missing_artifact_projection)
        .expect_err("missing artifact rejected");
    assert!(
        projection_error
            .to_string()
            .contains("references missing artifact"),
        "{projection_error}"
    );
}
