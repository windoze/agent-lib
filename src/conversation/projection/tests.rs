//! Projection model and checked range tests.

mod strategy;

use super::{
    Artifact, ArtifactProvenance, CheckedTurnRange, CompactionPlan, CompactionStep, Projection,
    RangeEndpoint, Span, StrategyRef, TokenAccounting,
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
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use serde_json::{Map, json};
use uuid::Uuid;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_8f60_0000_0000_0000;

type RawMessageSnapshot = (MessageId, Role, String);
type RawTurnSnapshot = (TurnId, Vec<RawMessageSnapshot>);

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

fn raw_compaction_plan(
    conversation: &Conversation,
    range: CheckedTurnRange,
    id_seed: u128,
    produced_by: StrategyRef,
    label: &str,
) -> (CompactionPlan, Artifact) {
    let artifact = artifact(id_seed, range.clone(), produced_by.clone(), label);
    let plan = CompactionPlan::new(
        conversation,
        vec![CompactionStep::raw(range, artifact.id(), produced_by)],
        vec![artifact.clone()],
    );
    (plan, artifact)
}

fn span_compaction_plan(
    conversation: &Conversation,
    range: CheckedTurnRange,
    id_seed: u128,
    produced_by: StrategyRef,
    label: &str,
) -> (CompactionPlan, Artifact) {
    let artifact = artifact(id_seed, range.clone(), produced_by.clone(), label);
    let plan = CompactionPlan::new(
        conversation,
        vec![CompactionStep::spans(range, artifact.id(), produced_by)],
        vec![artifact.clone()],
    );
    (plan, artifact)
}

fn install_projection(conversation: &mut Conversation, projection: Projection) {
    conversation.projection = projection;
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

fn message_labels(messages: &[Message]) -> Vec<(Role, String)> {
    messages
        .iter()
        .map(|message| {
            let [ContentBlock::Text { text, .. }] = message.content.as_slice() else {
                panic!("projection test messages contain one text block");
            };
            (message.role, text.clone())
        })
        .collect()
}

fn raw_history_snapshot(conversation: &Conversation) -> Vec<RawTurnSnapshot> {
    conversation
        .raw_turns()
        .into_iter()
        .map(|turn| {
            let messages = turn
                .messages()
                .iter()
                .map(|message| {
                    let [ContentBlock::Text { text, .. }] = message.payload().content.as_slice()
                    else {
                        panic!("projection test messages contain one text block");
                    };
                    (message.id(), message.payload().role, text.clone())
                })
                .collect();
            (turn.id(), messages)
        })
        .collect()
}

#[test]
fn effective_view_renders_system_and_default_raw_history() {
    let mut conversation = conversation(10);

    let empty = conversation.effective_view();
    assert_eq!(empty.system(), Some("Project carefully."));
    assert!(empty.is_empty());
    assert!(empty.messages().is_empty());

    commit_text_turn(&mut conversation, 101);
    commit_text_turn(&mut conversation, 102);
    let view = conversation.effective_view();

    assert_eq!(view.system(), Some("Project carefully."));
    assert_eq!(view.len(), 4);
    assert_eq!(
        message_labels(view.messages()),
        vec![
            (Role::User, "question:101".to_owned()),
            (Role::Assistant, "answer:101".to_owned()),
            (Role::User, "question:102".to_owned()),
            (Role::Assistant, "answer:102".to_owned()),
        ]
    );

    let (system, messages) = view.into_parts();
    assert_eq!(system, Some("Project carefully.".to_owned()));
    assert_eq!(messages.len(), 4);
}

#[test]
fn effective_view_uses_artifacts_only_for_complete_compacted_spans() {
    let mut conversation = conversation(11);
    commit_text_turn(&mut conversation, 111);
    commit_text_turn(&mut conversation, 112);
    commit_text_turn(&mut conversation, 113);
    commit_text_turn(&mut conversation, 114);

    let first = range(&conversation, 0, 1);
    let compacted = range(&conversation, 1, 3);
    let last = range(&conversation, 3, 4);
    let strategy_v1 = strategy("view-v1");
    let compacted_artifact = artifact(
        1110,
        compacted.clone(),
        strategy_v1.clone(),
        "turns 112-113 summary",
    );
    let projection = Projection::new(
        &conversation,
        vec![
            Span::raw(first),
            Span::compacted(compacted, compacted_artifact.id(), strategy_v1),
            Span::raw(last),
        ],
        vec![compacted_artifact],
    )
    .expect("projection with a compacted middle span");
    install_projection(&mut conversation, projection);

    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::User, "question:111".to_owned()),
            (Role::Assistant, "answer:111".to_owned()),
            (Role::Assistant, "turns 112-113 summary".to_owned()),
            (Role::User, "question:114".to_owned()),
            (Role::Assistant, "answer:114".to_owned()),
        ]
    );

    let boundary_inside_compacted = conversation.valid_boundaries()[2];
    let redo = conversation
        .revert_to(boundary_inside_compacted)
        .expect("move head into the compacted cover")
        .old_head();
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::User, "question:111".to_owned()),
            (Role::Assistant, "answer:111".to_owned()),
            (Role::User, "question:112".to_owned()),
            (Role::Assistant, "answer:112".to_owned()),
        ]
    );

    conversation
        .revert_to(redo)
        .expect("redo to the full compacted cover");
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::User, "question:111".to_owned()),
            (Role::Assistant, "answer:111".to_owned()),
            (Role::Assistant, "turns 112-113 summary".to_owned()),
            (Role::User, "question:114".to_owned()),
            (Role::Assistant, "answer:114".to_owned()),
        ]
    );
}

#[test]
fn effective_view_clips_zero_head_and_multiple_compacted_tiers() {
    let mut conversation = conversation(12);
    commit_text_turn(&mut conversation, 121);
    commit_text_turn(&mut conversation, 122);
    commit_text_turn(&mut conversation, 123);
    commit_text_turn(&mut conversation, 124);

    let first = range(&conversation, 0, 1);
    let middle = range(&conversation, 1, 3);
    let last = range(&conversation, 3, 4);
    let first_strategy = strategy("tier-a");
    let middle_strategy = strategy("tier-b");
    let first_artifact = artifact(
        1210,
        first.clone(),
        first_strategy.clone(),
        "turn 121 summary",
    );
    let middle_artifact = artifact(
        1211,
        middle.clone(),
        middle_strategy.clone(),
        "turns 122-123 summary",
    );
    let projection = Projection::new(
        &conversation,
        vec![
            Span::compacted(first, first_artifact.id(), first_strategy),
            Span::compacted(middle, middle_artifact.id(), middle_strategy),
            Span::raw(last),
        ],
        vec![first_artifact, middle_artifact],
    )
    .expect("tiered compacted projection");
    install_projection(&mut conversation, projection);

    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::Assistant, "turn 121 summary".to_owned()),
            (Role::Assistant, "turns 122-123 summary".to_owned()),
            (Role::User, "question:124".to_owned()),
            (Role::Assistant, "answer:124".to_owned()),
        ]
    );

    let boundary_inside_second_tier = conversation.valid_boundaries()[2];
    conversation
        .revert_to(boundary_inside_second_tier)
        .expect("move into second compacted tier");
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::Assistant, "turn 121 summary".to_owned()),
            (Role::User, "question:122".to_owned()),
            (Role::Assistant, "answer:122".to_owned()),
        ]
    );

    let zero = conversation.valid_boundaries()[0];
    conversation.revert_to(zero).expect("move to zero head");
    let zero_view = conversation.effective_view();
    assert_eq!(zero_view.system(), Some("Project carefully."));
    assert!(zero_view.messages().is_empty());

    let full_head = conversation.valid_boundaries()[4];
    conversation
        .revert_to(full_head)
        .expect("redo to full head");
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::Assistant, "turn 121 summary".to_owned()),
            (Role::Assistant, "turns 122-123 summary".to_owned()),
            (Role::User, "question:124".to_owned()),
            (Role::Assistant, "answer:124".to_owned()),
        ]
    );
}

#[test]
fn fork_child_effective_view_is_limited_to_child_ceiling() {
    let mut parent = conversation(13);
    commit_text_turn(&mut parent, 131);
    commit_text_turn(&mut parent, 132);
    commit_text_turn(&mut parent, 133);

    let compacted = range(&parent, 0, 3);
    let strategy_v1 = strategy("parent-only");
    let parent_artifact = artifact(
        1310,
        compacted.clone(),
        strategy_v1.clone(),
        "parent summary including turn 133",
    );
    let parent_projection = Projection::new(
        &parent,
        vec![Span::compacted(
            compacted,
            parent_artifact.id(),
            strategy_v1,
        )],
        vec![parent_artifact],
    )
    .expect("parent full projection");
    install_projection(&mut parent, parent_projection);

    let child = parent
        .fork_at(parent.valid_boundaries()[2], conversation_id(1300))
        .expect("fork at the second turn");

    assert_eq!(
        message_labels(child.effective_view().messages()),
        vec![
            (Role::User, "question:131".to_owned()),
            (Role::Assistant, "answer:131".to_owned()),
            (Role::User, "question:132".to_owned()),
            (Role::Assistant, "answer:132".to_owned()),
        ]
    );
    assert!(
        !message_labels(child.effective_view().messages())
            .iter()
            .any(|(_, text)| text.contains("133") || text.contains("parent summary"))
    );
}

#[test]
fn pending_context_is_explicit_and_never_includes_active_partial() {
    let mut conversation = conversation(14);
    commit_text_turn(&mut conversation, 141);
    assert!(conversation.pending_context().is_none());

    begin_pending(&mut conversation, 142);
    assert_eq!(
        message_labels(
            conversation
                .pending_context()
                .expect("pending context exists")
                .messages()
        ),
        vec![(Role::User, "pending".to_owned())]
    );
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::User, "question:141".to_owned()),
            (Role::Assistant, "answer:141".to_owned()),
        ]
    );

    conversation
        .start_assistant()
        .expect("start streaming assistant");
    let block_id = BlockId::new("partial-text");
    conversation
        .push_assistant_event(StreamEvent::MessageStart {
            role: Role::Assistant,
        })
        .expect("message start");
    conversation
        .push_assistant_event(StreamEvent::BlockStart {
            id: block_id.clone(),
            kind: BlockKind::Text,
        })
        .expect("block start");
    conversation
        .push_assistant_event(StreamEvent::BlockDelta {
            id: block_id,
            delta: Delta::Text("partial should stay hidden".to_owned()),
        })
        .expect("partial delta");

    let active_context = conversation
        .pending_context()
        .expect("pending context still exists");
    assert_eq!(active_context.len(), 1);
    assert_eq!(
        message_labels(active_context.messages()),
        vec![(Role::User, "pending".to_owned())]
    );
    assert!(
        !message_labels(active_context.messages())
            .iter()
            .any(|(_, text)| text.contains("partial"))
    );
}

#[test]
fn frozen_pending_messages_only_appear_through_pending_context() {
    let mut conversation = conversation(15);
    commit_text_turn(&mut conversation, 151);
    begin_pending(&mut conversation, 152);
    conversation
        .start_assistant_response(Response {
            message: Message {
                role: Role::Assistant,
                content: vec![text("pending final answer")],
            },
            usage: Usage::default(),
            stop_reason: StopReason::normalize("end_turn"),
            extra: Map::new(),
        })
        .expect("start complete assistant");
    conversation
        .finish_assistant(message_id(1521))
        .expect("freeze final assistant");

    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::User, "question:151".to_owned()),
            (Role::Assistant, "answer:151".to_owned()),
        ]
    );
    let pending_context = conversation
        .pending_context()
        .expect("ready pending still has an explicit context");
    assert_eq!(
        message_labels(pending_context.messages()),
        vec![
            (Role::User, "pending".to_owned()),
            (Role::Assistant, "pending final answer".to_owned()),
        ]
    );
    assert_eq!(
        pending_context.into_messages().len(),
        2,
        "owned Client payloads can be appended explicitly by the caller"
    );
}

#[test]
fn apply_compaction_handles_first_pass_tiered_tail_and_revert_redo() {
    let mut conversation = conversation(16);
    commit_text_turn(&mut conversation, 161);
    commit_text_turn(&mut conversation, 162);
    commit_text_turn(&mut conversation, 163);
    commit_text_turn(&mut conversation, 164);
    let raw_before = raw_history_snapshot(&conversation);
    let initial_version = conversation.version();

    let first_range = range(&conversation, 0, 2);
    let (first_plan, first_artifact) = raw_compaction_plan(
        &conversation,
        first_range,
        1610,
        strategy("apply-first"),
        "turns 161-162 summary",
    );
    let encoded_plan = serde_json::to_string(&first_plan).expect("serialize compaction plan");
    let decoded_plan: CompactionPlan =
        serde_json::from_str(&encoded_plan).expect("deserialize compaction plan");
    assert_eq!(decoded_plan, first_plan);

    conversation
        .apply_compaction(&first_plan)
        .expect("first raw range compaction applies");
    assert_eq!(conversation.version(), initial_version + 1);
    assert_eq!(conversation.projection().spans().len(), 2);
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::Assistant, "turns 161-162 summary".to_owned()),
            (Role::User, "question:163".to_owned()),
            (Role::Assistant, "answer:163".to_owned()),
            (Role::User, "question:164".to_owned()),
            (Role::Assistant, "answer:164".to_owned()),
        ]
    );

    let tail_prefix = range(&conversation, 2, 3);
    let (tail_plan, tail_artifact) = raw_compaction_plan(
        &conversation,
        tail_prefix,
        1611,
        strategy("apply-tail"),
        "turn 163 summary",
    );
    conversation
        .apply_compaction(&tail_plan)
        .expect("raw target can split the remaining raw tail");

    assert_eq!(conversation.projection().spans().len(), 3);
    assert!(
        conversation
            .projection()
            .artifact(first_artifact.id())
            .is_some()
    );
    assert!(
        conversation
            .projection()
            .artifact(tail_artifact.id())
            .is_some()
    );
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::Assistant, "turns 161-162 summary".to_owned()),
            (Role::Assistant, "turn 163 summary".to_owned()),
            (Role::User, "question:164".to_owned()),
            (Role::Assistant, "answer:164".to_owned()),
        ]
    );
    assert_eq!(raw_history_snapshot(&conversation), raw_before);

    let inside_first_summary = conversation.valid_boundaries()[1];
    let redo = conversation
        .revert_to(inside_first_summary)
        .expect("revert into compacted cover")
        .old_head();
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::User, "question:161".to_owned()),
            (Role::Assistant, "answer:161".to_owned()),
        ]
    );
    conversation
        .revert_to(redo)
        .expect("redo to full compacted projection");
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::Assistant, "turns 161-162 summary".to_owned()),
            (Role::Assistant, "turn 163 summary".to_owned()),
            (Role::User, "question:164".to_owned()),
            (Role::Assistant, "answer:164".to_owned()),
        ]
    );
    assert_eq!(raw_history_snapshot(&conversation), raw_before);
}

#[test]
fn apply_compaction_consolidates_existing_spans_and_retains_replaced_artifacts() {
    let mut conversation = conversation(17);
    commit_text_turn(&mut conversation, 171);
    commit_text_turn(&mut conversation, 172);
    commit_text_turn(&mut conversation, 173);
    commit_text_turn(&mut conversation, 174);
    let raw_before = raw_history_snapshot(&conversation);

    let first = range(&conversation, 0, 2);
    let (first_plan, first_artifact) = raw_compaction_plan(
        &conversation,
        first,
        1710,
        strategy("tier-a"),
        "turns 171-172 summary",
    );
    conversation
        .apply_compaction(&first_plan)
        .expect("first tier applies");
    let second = range(&conversation, 2, 3);
    let (second_plan, second_artifact) = raw_compaction_plan(
        &conversation,
        second,
        1711,
        strategy("tier-b"),
        "turn 173 summary",
    );
    conversation
        .apply_compaction(&second_plan)
        .expect("second tier applies");

    let consolidated = range(&conversation, 0, 3);
    let (consolidate_plan, consolidated_artifact) = span_compaction_plan(
        &conversation,
        consolidated,
        1712,
        strategy("consolidate"),
        "turns 171-173 consolidated summary",
    );
    conversation
        .apply_compaction(&consolidate_plan)
        .expect("span target can consolidate existing summaries plus raw tail");

    assert_eq!(conversation.projection().spans().len(), 2);
    assert!(
        conversation
            .projection()
            .artifact(first_artifact.id())
            .is_some()
    );
    assert!(
        conversation
            .projection()
            .artifact(second_artifact.id())
            .is_some()
    );
    assert!(
        conversation
            .projection()
            .artifact(consolidated_artifact.id())
            .is_some()
    );
    assert_eq!(conversation.projection().artifacts().len(), 3);
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (
                Role::Assistant,
                "turns 171-173 consolidated summary".to_owned(),
            ),
            (Role::User, "question:174".to_owned()),
            (Role::Assistant, "answer:174".to_owned()),
        ]
    );
    assert_eq!(raw_history_snapshot(&conversation), raw_before);
}

#[test]
fn commit_after_compaction_preserves_overlay_and_adds_raw_tail() {
    let mut conversation = conversation(18);
    commit_text_turn(&mut conversation, 181);
    commit_text_turn(&mut conversation, 182);
    let compacted = range(&conversation, 0, 2);
    let (plan, artifact) = raw_compaction_plan(
        &conversation,
        compacted,
        1810,
        strategy("keep-on-commit"),
        "turns 181-182 summary",
    );
    conversation
        .apply_compaction(&plan)
        .expect("compaction applies before the next turn");

    commit_text_turn(&mut conversation, 183);

    assert!(conversation.projection().artifact(artifact.id()).is_some());
    assert_eq!(conversation.projection().spans().len(), 2);
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (Role::Assistant, "turns 181-182 summary".to_owned()),
            (Role::User, "question:183".to_owned()),
            (Role::Assistant, "answer:183".to_owned()),
        ]
    );
}

#[test]
fn apply_compaction_rejects_stale_pending_and_invalid_plan_shapes_atomically() {
    let mut conversation = conversation(19);
    commit_text_turn(&mut conversation, 191);
    commit_text_turn(&mut conversation, 192);

    let stale_range = range(&conversation, 0, 1);
    let (stale_plan, _) = raw_compaction_plan(
        &conversation,
        stale_range,
        1910,
        strategy("stale"),
        "stale summary",
    );
    let projection_before_stale = conversation.projection().clone();
    commit_text_turn(&mut conversation, 193);
    assert_eq!(
        conversation
            .apply_compaction(&stale_plan)
            .expect_err("version mismatch rejects stale plans"),
        ConversationError::Projection(ProjectionError::StaleCompactionPlan {
            plan_version: 2,
            current_version: 3,
        })
    );
    assert_eq!(
        conversation.projection(),
        &projection_before_stale.extend_after_commit(conversation.id(), conversation.turns(), 2,)
    );

    let pending_range = range(&conversation, 0, 1);
    let (pending_plan, _) = raw_compaction_plan(
        &conversation,
        pending_range,
        1911,
        strategy("pending"),
        "pending summary",
    );
    let projection_before_pending = conversation.projection().clone();
    begin_pending(&mut conversation, 194);
    assert_eq!(
        conversation
            .apply_compaction(&pending_plan)
            .expect_err("pending apply is deferred by rejection"),
        ConversationError::Projection(ProjectionError::PendingTurn {
            turn_id: turn_id(194),
        })
    );
    assert_eq!(conversation.projection(), &projection_before_pending);
}

#[test]
fn apply_compaction_rejects_target_and_artifact_mismatches_atomically() {
    let mut conversation = conversation(20);
    commit_text_turn(&mut conversation, 201);
    commit_text_turn(&mut conversation, 202);
    commit_text_turn(&mut conversation, 203);
    commit_text_turn(&mut conversation, 204);
    let first = range(&conversation, 0, 2);
    let (first_plan, _) = raw_compaction_plan(
        &conversation,
        first,
        2010,
        strategy("existing"),
        "turns 201-202 summary",
    );
    conversation
        .apply_compaction(&first_plan)
        .expect("seed compacted span");
    let unchanged_projection = conversation.projection().clone();
    let unchanged_raw = raw_history_snapshot(&conversation);

    let raw_inside_compacted = range(&conversation, 1, 2);
    let (raw_inside_plan, _) = raw_compaction_plan(
        &conversation,
        raw_inside_compacted,
        2011,
        strategy("bad-raw"),
        "bad raw target",
    );
    assert_eq!(
        conversation
            .apply_compaction(&raw_inside_plan)
            .expect_err("raw targets cannot intersect compacted spans"),
        ConversationError::Projection(ProjectionError::CompactionTargetNotRaw { start: 1, end: 2 })
    );
    assert_eq!(conversation.projection(), &unchanged_projection);

    let partial_span = range(&conversation, 1, 3);
    let (partial_span_plan, _) = span_compaction_plan(
        &conversation,
        partial_span,
        2012,
        strategy("bad-span"),
        "bad span target",
    );
    assert_eq!(
        conversation
            .apply_compaction(&partial_span_plan)
            .expect_err("span targets must align with current span boundaries"),
        ConversationError::Projection(ProjectionError::CompactionTargetNotSpanAligned {
            start: 1,
            end: 3,
        })
    );
    assert_eq!(conversation.projection(), &unchanged_projection);

    let tail_one = range(&conversation, 2, 3);
    let missing_artifact_plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(
            tail_one.clone(),
            artifact_id(2013),
            strategy("missing-artifact"),
        )],
        Vec::new(),
    );
    assert_eq!(
        conversation
            .apply_compaction(&missing_artifact_plan)
            .expect_err("step artifact must be supplied by the plan"),
        ConversationError::Projection(ProjectionError::MissingArtifact {
            artifact_id: artifact_id(2013),
        })
    );

    let wrong_range_artifact = artifact(
        2014,
        range(&conversation, 3, 4),
        strategy("wrong-range"),
        "wrong range",
    );
    let wrong_range_plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(
            tail_one.clone(),
            wrong_range_artifact.id(),
            strategy("wrong-range"),
        )],
        vec![wrong_range_artifact.clone()],
    );
    assert_eq!(
        conversation
            .apply_compaction(&wrong_range_plan)
            .expect_err("artifact provenance must cover the target"),
        ConversationError::Projection(ProjectionError::ArtifactRangeMismatch {
            artifact_id: wrong_range_artifact.id(),
        })
    );

    let wrong_strategy_artifact = artifact(
        2015,
        tail_one.clone(),
        strategy("actual-strategy"),
        "wrong strategy",
    );
    let wrong_strategy_plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(
            tail_one.clone(),
            wrong_strategy_artifact.id(),
            strategy("declared-strategy"),
        )],
        vec![wrong_strategy_artifact.clone()],
    );
    assert_eq!(
        conversation
            .apply_compaction(&wrong_strategy_plan)
            .expect_err("step strategy must match artifact provenance"),
        ConversationError::Projection(ProjectionError::ArtifactStrategyMismatch {
            artifact_id: wrong_strategy_artifact.id(),
        })
    );

    let overlap_a = artifact(2016, tail_one.clone(), strategy("overlap-a"), "overlap a");
    let overlap_b_range = range(&conversation, 2, 4);
    let overlap_b = artifact(
        2017,
        overlap_b_range.clone(),
        strategy("overlap-b"),
        "overlap b",
    );
    let overlap_plan = CompactionPlan::new(
        &conversation,
        vec![
            CompactionStep::raw(tail_one, overlap_a.id(), strategy("overlap-a")),
            CompactionStep::raw(overlap_b_range, overlap_b.id(), strategy("overlap-b")),
        ],
        vec![overlap_a, overlap_b],
    );
    assert_eq!(
        conversation
            .apply_compaction(&overlap_plan)
            .expect_err("later overlapping step rejects the whole plan"),
        ConversationError::Projection(ProjectionError::SpanOverlap {
            expected_start: 3,
            actual_start: 2,
        })
    );

    let extra_range = range(&conversation, 3, 4);
    let extra_artifact = artifact(
        2018,
        extra_range,
        strategy("extra"),
        "extra unreferenced artifact",
    );
    let valid_tail = range(&conversation, 2, 3);
    let valid_artifact = artifact(2019, valid_tail.clone(), strategy("valid"), "valid tail");
    let unreferenced_plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(
            valid_tail,
            valid_artifact.id(),
            strategy("valid"),
        )],
        vec![valid_artifact, extra_artifact.clone()],
    );
    assert_eq!(
        conversation
            .apply_compaction(&unreferenced_plan)
            .expect_err("plan artifacts must be referenced by a step"),
        ConversationError::Projection(ProjectionError::UnreferencedCompactionArtifact {
            artifact_id: extra_artifact.id(),
        })
    );

    let empty_plan = CompactionPlan::new(&conversation, Vec::new(), Vec::new());
    assert_eq!(
        conversation
            .apply_compaction(&empty_plan)
            .expect_err("empty plans do not update projection"),
        ConversationError::Projection(ProjectionError::EmptyCompactionPlan)
    );

    assert_eq!(conversation.projection(), &unchanged_projection);
    assert_eq!(raw_history_snapshot(&conversation), unchanged_raw);
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
