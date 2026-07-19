//! Snapshot consistency-point tests.

mod e2e;

use super::{
    CONVERSATION_ROW_SCHEMA_VERSION, CONVERSATION_SNAPSHOT_SCHEMA_VERSION,
    ConversationRowInsertSet, ConversationRows, ConversationSnapshot,
};
use crate::{
    client::Response,
    conversation::{
        AssistantFinish, CommitError, Conversation, ConversationConfig, ConversationError,
        ConversationId, MessageId, PendingTurnError, PendingTurnPhase, Projection, ProjectionError,
        RestoreError, RowMappingError, SnapshotError, Span, ToolCallId, ToolCallIndex,
        ToolCallMapping, Turn, TurnId, TurnMeta,
        projection::{
            Artifact, ArtifactProvenance, CheckedTurnRange, CompactionPlan, CompactionStep,
            StrategyRef, TokenAccounting,
        },
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::{ToolResponse, ToolStatus},
        usage::Usage,
    },
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use uuid::Uuid;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_8f70_0000_0000_0000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeState {
    version: u64,
    head: u64,
    turns: Vec<Turn>,
    lineage_turns: Vec<Turn>,
    raw_turns: Vec<Turn>,
    pending: Option<(TurnId, PendingTurnPhase, usize)>,
    tool_call_index: ToolCallIndex,
    projection: Projection,
}

fn conversation_id(seed: u128) -> ConversationId {
    ConversationId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn turn_id(seed: u128) -> TurnId {
    TurnId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn message_id(seed: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn call_id(seed: u128) -> ToolCallId {
    ToolCallId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn conversation(seed: u128) -> Conversation {
    Conversation::new(
        conversation_id(seed),
        ConversationConfig::new(Some("Persist exactly.".to_owned())),
    )
}

fn text(value: impl Into<String>) -> ContentBlock {
    ContentBlock::Text {
        text: value.into(),
        extra: Map::new(),
    }
}

fn user(value: impl Into<String>) -> Message {
    Message {
        role: Role::User,
        content: vec![text(value)],
    }
}

fn assistant_response(
    content: Vec<ContentBlock>,
    input: u32,
    output: u32,
    stop_reason: StopReason,
    request_id: &str,
) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content,
        },
        usage: Usage {
            input,
            output,
            ..Usage::default()
        },
        stop_reason: StopReason::normalize(match stop_reason {
            StopReason::ToolUse => "tool_use",
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::StopSequence => "stop_sequence",
            StopReason::Refusal => "refusal",
            StopReason::Other => "other",
        }),
        extra: Map::from_iter([("request_id".to_owned(), json!(request_id))]),
    }
}

fn begin(conversation: &mut Conversation, seed: u128) {
    conversation
        .begin_turn(
            turn_id(seed),
            message_id(seed * 10),
            user(format!("question:{seed}")),
        )
        .expect("begin turn");
}

fn freeze_response(
    conversation: &mut Conversation,
    response: Response,
    message_seed: u128,
) -> AssistantFinish {
    conversation
        .start_assistant_response(response)
        .expect("start complete response");
    conversation
        .finish_assistant(message_id(message_seed))
        .expect("finish assistant")
}

fn commit_text_turn(conversation: &mut Conversation, seed: u128) {
    begin(conversation, seed);
    assert_eq!(
        freeze_response(
            conversation,
            assistant_response(
                vec![text(format!("answer:{seed}"))],
                2,
                1,
                StopReason::EndTurn,
                "text-turn",
            ),
            seed * 10 + 1,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit text turn");
}

fn tool_use(provider_call_id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: provider_call_id.to_owned(),
        name: "lookup".to_owned(),
        input: json!({ "query": provider_call_id }),
        extra: Map::new(),
    }
}

fn tool_response(provider_call_id: &str, value: &str) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_call_id.to_owned(),
        content: vec![text(value)],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

fn commit_tool_turn(
    conversation: &mut Conversation,
    seed: u128,
    provider_call_id: &str,
    call_seed: u128,
) {
    begin(conversation, seed);
    assert_eq!(
        freeze_response(
            conversation,
            assistant_response(
                vec![text("looking"), tool_use(provider_call_id)],
                5,
                2,
                StopReason::ToolUse,
                "tool-use",
            ),
            seed * 10 + 1,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    conversation
        .register_tool_calls(vec![ToolCallMapping::new(
            provider_call_id,
            call_id(call_seed),
        )])
        .expect("register tool call");
    conversation
        .append_tool_response(
            message_id(seed * 10 + 2),
            tool_response(provider_call_id, "tool result"),
        )
        .expect("append tool response");
    assert_eq!(
        freeze_response(
            conversation,
            assistant_response(vec![text("final")], 3, 1, StopReason::EndTurn, "final"),
            seed * 10 + 3,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit tool turn");
}

fn range(conversation: &Conversation, start_index: usize, end_index: usize) -> CheckedTurnRange {
    let boundaries = conversation.valid_boundaries();
    conversation
        .checked_turn_range(boundaries[start_index], boundaries[end_index])
        .expect("checked range")
}

fn strategy(version: &str) -> StrategyRef {
    StrategyRef::new("snapshot-summary", version)
}

fn summary_artifact(
    conversation: &Conversation,
    range: CheckedTurnRange,
    id_seed: u128,
    produced_by: StrategyRef,
    label: &str,
) -> Artifact {
    let before = Usage {
        input: (conversation.effective_view().len() as u32) * 10,
        ..Usage::default()
    };
    Artifact::new(
        crate::conversation::ArtifactId::new(Uuid::from_u128(UUID_BASE + id_seed)),
        vec![Message {
            role: Role::Assistant,
            content: vec![text(label)],
        }],
        ArtifactProvenance::new(
            range,
            produced_by,
            TokenAccounting::new(
                before,
                Usage {
                    input: 4,
                    ..Usage::default()
                },
            ),
            Map::new(),
        ),
    )
    .expect("artifact has render messages")
}

fn runtime_state(conversation: &Conversation) -> RuntimeState {
    RuntimeState {
        version: conversation.version(),
        head: conversation.head().turn_count(),
        turns: conversation.turns().to_vec(),
        lineage_turns: conversation.lineage_turns().to_vec(),
        raw_turns: conversation.raw_turns().into_iter().cloned().collect(),
        pending: conversation
            .pending()
            .map(|pending| (pending.id(), pending.phase(), pending.messages().len())),
        tool_call_index: conversation.tool_call_index().clone(),
        projection: conversation.projection().clone(),
    }
}

fn raw_turn_ids(snapshot: &ConversationSnapshot) -> Vec<TurnId> {
    snapshot.history().raw_turn_ids().collect()
}

fn lineage_turn_ids(conversation: &Conversation) -> Vec<TurnId> {
    conversation.lineage_turns().iter().map(Turn::id).collect()
}

fn assert_unique_snapshot_facts(snapshot: &ConversationSnapshot) {
    let raw_ids = raw_turn_ids(snapshot);
    let unique_raw = raw_ids.iter().copied().collect::<HashSet<_>>();
    assert_eq!(unique_raw.len(), raw_ids.len(), "raw turn facts are unique");

    let json = serde_json::to_value(snapshot).expect("snapshot JSON");
    let raw_turns = json["history"]["raw_turns"]
        .as_array()
        .expect("raw turn fact array");
    let message_ids = raw_turns
        .iter()
        .flat_map(|turn| {
            turn["messages"]
                .as_array()
                .expect("turn message fact array")
                .iter()
                .map(|message| {
                    message["id"]
                        .as_str()
                        .expect("message id serializes as string")
                        .to_owned()
                })
        })
        .collect::<Vec<_>>();
    let unique_messages = message_ids.iter().cloned().collect::<HashSet<_>>();
    assert_eq!(
        unique_messages.len(),
        message_ids.len(),
        "message facts are unique inside the snapshot"
    );
}

fn assert_snapshot_rejected_without_state_change(conversation: &Conversation) {
    let before = runtime_state(conversation);
    let pending_id = conversation.pending().expect("pending").id();
    let error = conversation
        .snapshot()
        .expect_err("pending rejects snapshot");
    assert_eq!(
        error,
        ConversationError::Snapshot(SnapshotError::PendingTurn {
            turn_id: pending_id,
        })
    );
    assert_eq!(runtime_state(conversation), before);
}

fn json_keys_do_not_contain_runtime_objects(value: &Value) {
    let forbidden = [
        "pending",
        "pending_turn",
        "pending_message",
        "accumulator",
        "tool_call_index",
        "client",
        "registry",
        "resolver",
        "trigger",
        "strategy_object",
        "arc",
        "lock",
    ];
    match value {
        Value::Object(map) => {
            for key in map.keys() {
                assert!(
                    !forbidden.contains(&key.as_str()),
                    "runtime-only key `{key}` must not enter snapshot JSON"
                );
            }
            for child in map.values() {
                json_keys_do_not_contain_runtime_objects(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                json_keys_do_not_contain_runtime_objects(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn snapshot_json(conversation: &Conversation) -> Value {
    serde_json::to_value(conversation.snapshot().expect("committed snapshot"))
        .expect("snapshot JSON")
}

fn restore_error_from_json(value: Value) -> RestoreError {
    let snapshot: ConversationSnapshot =
        serde_json::from_value(value).expect("corrupted snapshot remains data-shaped");
    match Conversation::restore(snapshot).expect_err("restore rejects corrupted snapshot") {
        ConversationError::Restore(error) => error,
        other => panic!("expected restore error, got {other:?}"),
    }
}

fn assert_snapshot_deserialize_rejected(value: Value) {
    let error =
        serde_json::from_value::<ConversationSnapshot>(value).expect_err("snapshot shape rejects");
    assert!(
        !error.to_string().is_empty(),
        "serde rejection should keep a useful diagnostic"
    );
}

#[test]
fn linear_snapshot_round_trips_and_records_each_fact_once() {
    let mut conversation = conversation(1);
    commit_text_turn(&mut conversation, 10);
    commit_tool_turn(&mut conversation, 11, "call-linear", 900);

    let snapshot = conversation.snapshot().expect("committed snapshot");
    assert_eq!(
        snapshot.schema_version(),
        CONVERSATION_SNAPSHOT_SCHEMA_VERSION
    );
    assert_eq!(snapshot.id(), conversation.id());
    assert_eq!(snapshot.config(), conversation.config());
    assert_eq!(snapshot.structural_version(), conversation.version());
    assert_eq!(snapshot.origin(), None);
    assert_eq!(snapshot.history().raw_turn_count(), 2);
    assert_eq!(raw_turn_ids(&snapshot), lineage_turn_ids(&conversation));
    assert_eq!(
        snapshot.history().lineage_turn_ids(),
        lineage_turn_ids(&conversation)
    );
    assert_eq!(snapshot.history().head_turn_count(), 2);
    assert_eq!(snapshot.history().fork_ceiling_turn_count(), 2);
    assert_eq!(snapshot.projection(), conversation.projection());
    assert_unique_snapshot_facts(&snapshot);

    let json = serde_json::to_value(&snapshot).expect("snapshot JSON");
    assert_eq!(
        json["schema_version"],
        json!(CONVERSATION_SNAPSHOT_SCHEMA_VERSION)
    );
    assert_eq!(
        json["history"]["raw_turns"]
            .as_array()
            .expect("raw turn JSON array")
            .len(),
        2
    );
    json_keys_do_not_contain_runtime_objects(&json);

    let decoded: ConversationSnapshot =
        serde_json::from_value(json).expect("snapshot serde round trip");
    assert_eq!(decoded, snapshot);
}

#[test]
fn restore_round_trips_json_and_rebuilds_runtime_index() {
    let mut conversation = conversation(9);
    commit_text_turn(&mut conversation, 90);
    commit_tool_turn(&mut conversation, 91, "call-restore", 990);
    let before = runtime_state(&conversation);

    let encoded = serde_json::to_string(&conversation.snapshot().expect("snapshot"))
        .expect("serialize snapshot");
    let decoded: ConversationSnapshot =
        serde_json::from_str(&encoded).expect("deserialize snapshot");
    let restored = Conversation::restore(decoded).expect("restore snapshot");

    assert_eq!(runtime_state(&restored), before);
    assert_eq!(restored.id(), conversation.id());
    assert_eq!(restored.config(), conversation.config());
    assert_eq!(restored.origin(), conversation.origin());
    assert!(restored.pending().is_none());
    assert_eq!(restored.effective_view(), conversation.effective_view());
    assert_eq!(
        restored.tool_call_index(),
        &ToolCallIndex::rebuild(restored.turns(), restored.pending())
    );

    let try_from_restored =
        Conversation::try_from(conversation.snapshot().expect("snapshot")).expect("try_from");
    assert_eq!(runtime_state(&try_from_restored), before);
}

#[test]
fn restore_preserves_fork_origin_reverted_head_and_compacted_projection() {
    let mut parent = conversation(10);
    commit_text_turn(&mut parent, 100);
    commit_text_turn(&mut parent, 101);
    commit_text_turn(&mut parent, 102);

    let fork_point = parent.valid_boundaries()[2];
    let mut child = parent
        .fork_at(fork_point, conversation_id(10_000))
        .expect("fork child");
    commit_tool_turn(&mut child, 103, "call-child-restore", 9_103);

    let covers = range(&child, 0, 2);
    let produced_by = strategy("restore-v1");
    let artifact = summary_artifact(
        &child,
        covers.clone(),
        10_400,
        produced_by.clone(),
        "summary:100-101",
    );
    let plan = CompactionPlan::new(
        &child,
        vec![CompactionStep::raw(
            covers.clone(),
            artifact.id(),
            produced_by.clone(),
        )],
        vec![artifact],
    );
    child
        .apply_compaction(&plan)
        .expect("compact inherited prefix");
    child
        .revert_to(child.valid_boundaries()[1])
        .expect("revert inside compacted span before snapshot");

    let before = runtime_state(&child);
    let snapshot = child.snapshot().expect("reverted compacted snapshot");
    assert_eq!(snapshot.origin().expect("origin").fork_point(), fork_point);
    assert_eq!(snapshot.history().head_turn_count(), 1);
    assert_eq!(snapshot.history().fork_ceiling_turn_count(), 3);
    assert_eq!(snapshot.projection().artifacts().len(), 1);

    let restored = Conversation::restore(snapshot).expect("restore reverted compacted child");
    assert_eq!(runtime_state(&restored), before);
    assert_eq!(restored.origin(), child.origin());
    assert_eq!(restored.projection(), child.projection());
    assert_eq!(restored.effective_view(), child.effective_view());
}

#[test]
fn restore_rejects_schema_duplicate_ids_and_invalid_turn_facts() {
    let mut conversation = conversation(11);
    commit_text_turn(&mut conversation, 110);
    commit_text_turn(&mut conversation, 111);
    let value = snapshot_json(&conversation);

    let mut bad_schema = value.clone();
    bad_schema["schema_version"] = json!(CONVERSATION_SNAPSHOT_SCHEMA_VERSION + 1);
    assert!(matches!(
        restore_error_from_json(bad_schema),
        RestoreError::UnsupportedSchemaVersion {
            path,
            expected: CONVERSATION_SNAPSHOT_SCHEMA_VERSION,
            actual,
        } if path == "$.schema_version" && actual == CONVERSATION_SNAPSHOT_SCHEMA_VERSION + 1
    ));

    let mut duplicate_turn = value.clone();
    let first_turn = duplicate_turn["history"]["raw_turns"][0].clone();
    duplicate_turn["history"]["raw_turns"]
        .as_array_mut()
        .expect("raw turn array")
        .push(first_turn);
    assert!(matches!(
        restore_error_from_json(duplicate_turn),
        RestoreError::DuplicateRawTurnId { path, turn_id: duplicate_id }
            if path == "$.history.raw_turns[2].id" && duplicate_id == turn_id(110)
    ));

    let mut invalid_turn = value;
    invalid_turn["history"]["raw_turns"][0]["messages"][0]["payload"]["role"] = json!("assistant");
    assert!(matches!(
        restore_error_from_json(invalid_turn),
        RestoreError::InvalidTurn {
            path,
            source: CommitError::InvalidStartState {
                first_role: Some(Role::Assistant),
            },
        } if path == "$.history.raw_turns[0]"
    ));
}

#[test]
fn restore_rejects_missing_and_cyclic_parents() {
    let mut conversation = conversation(12);
    commit_text_turn(&mut conversation, 120);
    commit_text_turn(&mut conversation, 121);
    let value = snapshot_json(&conversation);

    let missing_parent_id = turn_id(12_999);
    let mut missing_parent = value.clone();
    missing_parent["history"]["raw_turns"][1]["parent"] = json!(missing_parent_id);
    assert!(matches!(
        restore_error_from_json(missing_parent),
        RestoreError::MissingParent {
            path,
            turn_id: restored_turn_id,
            parent,
        } if path == "$.history.raw_turns[1].parent"
            && restored_turn_id == turn_id(121)
            && parent == missing_parent_id
    ));

    let mut cycle = value;
    cycle["history"]["raw_turns"][0]["parent"] = json!(turn_id(121));
    cycle["history"]["raw_turns"][1]["parent"] = json!(turn_id(120));
    assert!(matches!(
        restore_error_from_json(cycle),
        RestoreError::ParentCycle { path, turn_id: cycle_turn_id }
            if path == "$.history.raw_turns[0].parent" && cycle_turn_id == turn_id(120)
    ));
}

#[test]
fn restore_rejects_bad_lineage_head_and_fork_origin() {
    let mut base = conversation(13);
    commit_text_turn(&mut base, 130);
    commit_text_turn(&mut base, 131);
    let value = snapshot_json(&base);

    let unknown_lineage_id = turn_id(13_999);
    let mut unknown_lineage = value.clone();
    unknown_lineage["history"]["lineage_turns"][1] = json!(unknown_lineage_id);
    assert!(matches!(
        restore_error_from_json(unknown_lineage),
        RestoreError::UnknownLineageTurn { path, turn_id }
            if path == "$.history.lineage_turns[1]" && turn_id == unknown_lineage_id
    ));

    let mut bad_head = value;
    bad_head["history"]["head_turn_count"] = json!(3);
    assert!(matches!(
        restore_error_from_json(bad_head),
        RestoreError::HeadOutOfRange {
            path,
            head: 3,
            fork_ceiling: 2,
        } if path == "$.history.head_turn_count"
    ));

    let mut parent = conversation(14);
    commit_text_turn(&mut parent, 140);
    commit_text_turn(&mut parent, 141);
    let child = parent
        .fork_at(parent.valid_boundaries()[2], conversation_id(14_000))
        .expect("fork child");
    let child_value = snapshot_json(&child);

    let mut self_parent = child_value.clone();
    self_parent["origin"]["parent"] = json!(child.id());
    assert!(matches!(
        restore_error_from_json(self_parent),
        RestoreError::ForkOriginSelfParent {
            path,
            conversation_id,
        } if path == "$.origin.parent" && conversation_id == child.id()
    ));

    let mut wrong_owner = child_value.clone();
    let wrong_parent = conversation_id(14_001);
    wrong_owner["origin"]["fork_point"]["conversation_id"] = json!(wrong_parent);
    assert!(matches!(
        restore_error_from_json(wrong_owner),
        RestoreError::ForkPointOwnerMismatch {
            path,
            expected,
            actual,
        } if path == "$.origin.fork_point.conversation_id"
            && expected == parent.id()
            && actual == wrong_parent
    ));

    let mut wrong_anchor = child_value;
    wrong_anchor["origin"]["fork_point"]["after_turn"] = json!(turn_id(140));
    assert!(matches!(
        restore_error_from_json(wrong_anchor),
        RestoreError::ForkPointAnchorMismatch {
            path,
            turn_count: 2,
            expected,
            actual,
        } if path == "$.origin.fork_point.after_turn"
            && expected == Some(turn_id(141))
            && actual == Some(turn_id(140))
    ));
}

#[test]
fn restore_rejects_projection_mismatches_and_derived_snapshot_fields() {
    let mut conversation = conversation(15);
    commit_text_turn(&mut conversation, 150);
    commit_text_turn(&mut conversation, 151);
    commit_text_turn(&mut conversation, 152);

    let covers = range(&conversation, 0, 2);
    let produced_by = strategy("projection-corruption");
    let artifact = summary_artifact(
        &conversation,
        covers.clone(),
        15_000,
        produced_by.clone(),
        "summary:150-151",
    );
    let plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(covers, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation
        .apply_compaction(&plan)
        .expect("apply compacted projection");
    let value = snapshot_json(&conversation);

    let mut wrong_owner = value.clone();
    let wrong_projection_owner = conversation_id(15_999);
    wrong_owner["projection"]["spans"][0]["covers"]["conversation_id"] =
        json!(wrong_projection_owner);
    wrong_owner["projection"]["artifacts"][0]["provenance"]["input_range"]["conversation_id"] =
        json!(wrong_projection_owner);
    assert!(matches!(
        restore_error_from_json(wrong_owner),
        RestoreError::InvalidProjection {
            path,
            source: ProjectionError::RangeOwnerMismatch { .. },
        } if path == "$.projection"
    ));

    let mut wrong_anchor = value.clone();
    wrong_anchor["projection"]["spans"][0]["covers"]["end"]["after_turn"] = json!(turn_id(150));
    wrong_anchor["projection"]["artifacts"][0]["provenance"]["input_range"]["end"]["after_turn"] =
        json!(turn_id(150));
    assert!(matches!(
        restore_error_from_json(wrong_anchor),
        RestoreError::InvalidProjection {
            path,
            source: ProjectionError::RangeAnchorMismatch { .. },
        } if path == "$.projection"
    ));

    let mut overlap = value.clone();
    overlap["projection"]["spans"][1]["turns"]["start"]["turn_count"] = json!(1);
    assert_snapshot_deserialize_rejected(overlap);

    let mut missing_artifact = value.clone();
    missing_artifact["projection"]["artifacts"] = json!([]);
    assert_snapshot_deserialize_rejected(missing_artifact);

    let mut wrong_covers = value.clone();
    wrong_covers["projection"]["artifacts"][0]["provenance"]["input_range"]["end"]["turn_count"] =
        json!(1);
    assert_snapshot_deserialize_rejected(wrong_covers);

    let mut derived_field = value;
    derived_field["tool_call_index"] = json!({ "entries": [] });
    assert_snapshot_deserialize_rejected(derived_field);
}

#[test]
fn snapshot_keeps_detached_raw_suffix_separate_from_current_lineage() {
    let mut conversation = conversation(2);
    commit_text_turn(&mut conversation, 20);
    commit_text_turn(&mut conversation, 21);
    commit_text_turn(&mut conversation, 22);
    let detached_a = turn_id(21);
    let detached_b = turn_id(22);

    let after_first = conversation.valid_boundaries()[1];
    conversation
        .revert_to(after_first)
        .expect("revert before branching");
    commit_text_turn(&mut conversation, 23);

    let snapshot = conversation.snapshot().expect("branched snapshot");
    assert_eq!(
        raw_turn_ids(&snapshot),
        vec![turn_id(20), detached_a, detached_b, turn_id(23)]
    );
    assert_eq!(
        snapshot.history().lineage_turn_ids(),
        &[turn_id(20), turn_id(23)]
    );
    assert_eq!(snapshot.history().head_turn_count(), 2);
    assert_eq!(snapshot.history().fork_ceiling_turn_count(), 2);
    assert_unique_snapshot_facts(&snapshot);
}

#[test]
fn fork_snapshot_records_origin_and_excludes_parent_suffix() {
    let mut parent = conversation(3);
    commit_text_turn(&mut parent, 30);
    commit_text_turn(&mut parent, 31);
    commit_text_turn(&mut parent, 32);

    let fork_point = parent.valid_boundaries()[2];
    let child = parent
        .fork_at(fork_point, conversation_id(3000))
        .expect("fork child");
    let snapshot = child.snapshot().expect("child snapshot");

    let origin = snapshot.origin().expect("child records origin");
    assert_eq!(origin.parent(), parent.id());
    assert_eq!(origin.fork_point(), fork_point);
    assert_eq!(snapshot.id(), child.id());
    assert_eq!(snapshot.structural_version(), 0);
    assert_eq!(raw_turn_ids(&snapshot), vec![turn_id(30), turn_id(31)]);
    assert_eq!(
        snapshot.history().lineage_turn_ids(),
        &[turn_id(30), turn_id(31)]
    );
    assert_eq!(snapshot.history().head_turn_count(), 2);
    assert_eq!(snapshot.history().fork_ceiling_turn_count(), 2);
    assert!(!raw_turn_ids(&snapshot).contains(&turn_id(32)));
    assert_unique_snapshot_facts(&snapshot);
}

#[test]
fn snapshot_preserves_projection_artifacts_and_provenance() {
    let mut conversation = conversation(4);
    commit_text_turn(&mut conversation, 40);
    commit_text_turn(&mut conversation, 41);
    commit_text_turn(&mut conversation, 42);

    let covers = range(&conversation, 0, 2);
    let produced_by = strategy("v1");
    let artifact = summary_artifact(
        &conversation,
        covers.clone(),
        4000,
        produced_by.clone(),
        "summary:40-41",
    );
    let plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(
            covers.clone(),
            artifact.id(),
            produced_by.clone(),
        )],
        vec![artifact.clone()],
    );
    conversation
        .apply_compaction(&plan)
        .expect("apply compaction before snapshot");

    let snapshot = conversation.snapshot().expect("compacted snapshot");
    assert_eq!(
        snapshot.projection().artifacts(),
        std::slice::from_ref(&artifact)
    );
    assert_eq!(snapshot.projection().spans().len(), 2);
    assert!(matches!(
        &snapshot.projection().spans()[0],
        Span::Compacted {
            covers: span_covers,
            artifact: span_artifact,
            produced_by: span_strategy,
        } if span_covers == &covers
            && span_artifact == &artifact.id()
            && span_strategy == &produced_by
    ));
    assert_eq!(
        snapshot.projection().artifacts()[0]
            .provenance()
            .input_range(),
        &covers
    );
    assert_eq!(
        snapshot.projection().artifacts()[0]
            .provenance()
            .produced_by(),
        &produced_by
    );

    let encoded = serde_json::to_string(&snapshot).expect("serialize compacted snapshot");
    let decoded: ConversationSnapshot =
        serde_json::from_str(&encoded).expect("deserialize compacted snapshot");
    assert_eq!(decoded, snapshot);
}

fn scramble_rows(rows: &mut ConversationRows) {
    rows.raw_turns.reverse();
    rows.lineage_turns.reverse();
    rows.turns.reverse();
    rows.messages.reverse();
    rows.tool_pairings.reverse();
    rows.projection_spans.reverse();
    rows.artifacts.reverse();
}

#[test]
fn rows_round_trip_snapshot_in_any_read_order_and_restore() {
    let mut conversation = conversation(16);
    commit_text_turn(&mut conversation, 160);
    commit_tool_turn(&mut conversation, 161, "call-row-round-trip", 16_100);
    commit_text_turn(&mut conversation, 162);

    let covers = range(&conversation, 0, 2);
    let produced_by = strategy("rows-v1");
    let artifact = summary_artifact(
        &conversation,
        covers.clone(),
        16_500,
        produced_by.clone(),
        "summary:160-161",
    );
    let plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(covers, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation
        .apply_compaction(&plan)
        .expect("apply projection before row export");

    let snapshot = conversation.snapshot().expect("snapshot");
    let mut rows = snapshot.to_rows().expect("snapshot decomposes to rows");
    let encoded = serde_json::to_string(&rows).expect("serialize rows");
    let mut decoded: ConversationRows = serde_json::from_str(&encoded).expect("deserialize rows");
    assert_eq!(decoded, rows);

    scramble_rows(&mut rows);
    scramble_rows(&mut decoded);

    let rebuilt = ConversationSnapshot::from_rows(rows).expect("rows rebuild snapshot");
    let decoded_rebuilt =
        ConversationSnapshot::from_rows(decoded).expect("serde rows rebuild snapshot");
    assert_eq!(rebuilt, snapshot);
    assert_eq!(decoded_rebuilt, snapshot);

    let restored = Conversation::restore(rebuilt).expect("restore row-built snapshot");
    assert_eq!(runtime_state(&restored), runtime_state(&conversation));
    assert_eq!(restored.effective_view(), conversation.effective_view());
}

#[test]
fn fork_row_insert_set_references_shared_ancestors_without_duplicate_facts() {
    let mut parent = conversation(17);
    commit_text_turn(&mut parent, 170);
    commit_tool_turn(&mut parent, 171, "call-parent-row", 17_100);

    let fork_point = parent.valid_boundaries()[2];
    let mut child = parent
        .fork_at(fork_point, conversation_id(17_000))
        .expect("fork child");
    commit_text_turn(&mut child, 172);

    let parent_rows = parent
        .snapshot()
        .expect("parent snapshot")
        .to_rows()
        .expect("parent rows");
    let child_rows = child
        .snapshot()
        .expect("child snapshot")
        .to_rows()
        .expect("child rows");

    let inserts = child_rows
        .insert_set_against(&parent_rows)
        .expect("child rows are insert-only relative to parent rows");

    assert_eq!(inserts.conversations.len(), 1);
    assert_eq!(inserts.conversations[0].conversation_id, child.id());
    assert_eq!(inserts.raw_turns.len(), 3);
    assert_eq!(inserts.lineage_turns.len(), 3);
    assert!(
        inserts
            .raw_turns
            .iter()
            .any(|row| row.turn_id == turn_id(170)),
        "child raw membership references shared parent ancestor by id"
    );
    assert_eq!(
        inserts
            .turns
            .iter()
            .map(|row| row.turn_id)
            .collect::<Vec<_>>(),
        vec![turn_id(172)]
    );
    assert!(
        inserts
            .messages
            .iter()
            .all(|row| row.turn_id == turn_id(172)),
        "shared ancestor messages are not duplicated for the child insert set"
    );
    assert!(
        inserts
            .tool_pairings
            .iter()
            .all(|row| row.turn_id == turn_id(172)),
        "shared ancestor pairings are not duplicated for the child insert set"
    );
}

#[test]
fn rows_reject_duplicate_primary_keys_missing_fks_and_bad_sequences() {
    let mut conversation = conversation(18);
    commit_text_turn(&mut conversation, 180);
    commit_text_turn(&mut conversation, 181);
    let rows = conversation
        .snapshot()
        .expect("snapshot")
        .to_rows()
        .expect("rows");

    let mut duplicate_message = rows.clone();
    let duplicate = duplicate_message.messages[0].clone();
    duplicate_message.messages.push(duplicate);
    assert!(matches!(
        duplicate_message
            .into_snapshot()
            .expect_err("duplicate message rejected"),
        RowMappingError::DuplicatePrimaryKey {
            table: "message_records",
            ..
        }
    ));

    let mut missing_turn_fk = rows.clone();
    missing_turn_fk.messages[0].turn_id = turn_id(18_999);
    assert!(matches!(
        missing_turn_fk.into_snapshot().expect_err("missing turn rejected"),
        RowMappingError::MissingTurnRow {
            turn_id: missing_id,
            ..
        } if missing_id == turn_id(18_999)
    ));

    let mut bad_sequence = rows.clone();
    bad_sequence.messages[0].message_sequence = 4;
    assert!(matches!(
        bad_sequence
            .into_snapshot()
            .expect_err("message sequence gap rejected"),
        RowMappingError::SequenceGap {
            table: "message_records",
            expected: 0,
            actual: 1 | 4,
            ..
        }
    ));

    let mut missing_messages = rows;
    let removed_turn = missing_messages.raw_turns[0].turn_id;
    missing_messages
        .messages
        .retain(|row| row.turn_id != removed_turn);
    assert!(matches!(
        missing_messages.into_snapshot().expect_err("missing messages rejected"),
        RowMappingError::MissingMessageRows { turn_id, .. } if turn_id == removed_turn
    ));
}

#[test]
fn rows_reject_projection_artifact_corruption_and_restore_catches_parent_cycles() {
    let mut conversation = conversation(19);
    commit_text_turn(&mut conversation, 190);
    commit_text_turn(&mut conversation, 191);

    let covers = range(&conversation, 0, 1);
    let produced_by = strategy("rows-corruption");
    let artifact = summary_artifact(
        &conversation,
        covers.clone(),
        19_500,
        produced_by.clone(),
        "summary:190",
    );
    let plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(covers, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation
        .apply_compaction(&plan)
        .expect("apply projection before corruption tests");
    let rows = conversation
        .snapshot()
        .expect("snapshot")
        .to_rows()
        .expect("rows");

    let mut missing_artifact = rows.clone();
    missing_artifact.artifacts.clear();
    assert!(matches!(
        missing_artifact
            .into_snapshot()
            .expect_err("missing artifact rejected"),
        RowMappingError::InvalidProjectionRows {
            source: ProjectionError::MissingArtifact { .. },
            ..
        }
    ));

    let mut wrong_artifact_owner = rows.clone();
    wrong_artifact_owner.artifacts[0].conversation_id = conversation_id(19_999);
    assert!(matches!(
        wrong_artifact_owner.into_snapshot().expect_err("foreign artifact rejected"),
        RowMappingError::ConversationMismatch { actual, expected, .. }
            if expected == conversation.id() && actual == conversation_id(19_999)
    ));

    let mut cycle = rows;
    cycle.turns[0].parent_turn_id = Some(turn_id(191));
    cycle.turns[1].parent_turn_id = Some(turn_id(190));
    let snapshot = cycle
        .into_snapshot()
        .expect("row shape can rebuild cyclic parent facts");
    assert!(matches!(
        Conversation::restore(snapshot).expect_err("restore rejects cycle"),
        ConversationError::Restore(RestoreError::ParentCycle { .. })
    ));
}

#[test]
fn rows_stamp_every_evolving_row_with_the_export_generation() {
    let mut conversation = conversation(20);
    let fresh_version = conversation
        .snapshot()
        .expect("fresh snapshot")
        .structural_version();
    commit_text_turn(&mut conversation, 200);
    commit_text_turn(&mut conversation, 201);

    let covers = range(&conversation, 0, 1);
    let produced_by = strategy("rows-generation");
    let artifact = summary_artifact(
        &conversation,
        covers.clone(),
        20_500,
        produced_by.clone(),
        "summary:200",
    );
    let plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(covers, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation
        .apply_compaction(&plan)
        .expect("apply projection before row export");

    let snapshot = conversation.snapshot().expect("snapshot");
    let rows = snapshot.to_rows().expect("rows");
    let generation = snapshot.structural_version();
    assert!(
        generation > fresh_version,
        "commit and compaction advance the structural version"
    );
    assert_eq!(rows.conversation.structural_version, generation);
    assert_eq!(
        rows.conversation.generation, generation,
        "conversation row generation equals its structural version"
    );
    assert!(
        rows.lineage_turns
            .iter()
            .all(|row| row.generation == generation),
        "every lineage membership row carries the export generation"
    );
    assert!(
        !rows.projection_spans.is_empty(),
        "compacted projection exports span rows"
    );
    assert!(
        rows.projection_spans
            .iter()
            .all(|row| row.generation == generation),
        "every projection span row carries the export generation"
    );
    assert!(
        !rows.artifacts.is_empty(),
        "compacted projection exports artifact membership rows"
    );
    assert!(
        rows.artifacts
            .iter()
            .all(|row| row.generation == generation),
        "every artifact membership row carries the export generation"
    );
}

#[test]
fn rows_reject_an_older_row_schema_version() {
    let mut conversation = conversation(21);
    commit_text_turn(&mut conversation, 210);
    let rows = conversation
        .snapshot()
        .expect("snapshot")
        .to_rows()
        .expect("rows");
    let old_version = CONVERSATION_ROW_SCHEMA_VERSION - 1;

    let mut old_conversation = rows.clone();
    old_conversation.conversation.schema_version = old_version;
    assert!(matches!(
        old_conversation
            .into_snapshot()
            .expect_err("older conversation row schema rejected"),
        RowMappingError::InvalidRow {
            path,
            table: "conversation_records",
            reason,
        } if path == "$.conversation.schema_version" && reason.contains("no migration path")
    ));

    let mut old_projection = rows.clone();
    old_projection.projection.schema_version = old_version;
    assert!(matches!(
        old_projection
            .into_snapshot()
            .expect_err("older projection row schema rejected"),
        RowMappingError::InvalidRow {
            path,
            table: "projection_records",
            reason,
        } if path == "$.projection.schema_version" && reason.contains("no migration path")
    ));

    // Rows exported before the generation column existed must fail closed at
    // deserialization rather than silently defaulting the new key.
    let mut old_json = serde_json::to_value(&rows).expect("serialize rows");
    old_json["conversation"]["schema_version"] = json!(old_version);
    old_json["projection"]["schema_version"] = json!(old_version);
    old_json["conversation"]
        .as_object_mut()
        .expect("conversation object")
        .remove("generation");
    for row in old_json["lineage_turns"]
        .as_array_mut()
        .expect("lineage rows")
    {
        row.as_object_mut()
            .expect("lineage row object")
            .remove("generation");
    }
    for row in old_json["projection_spans"]
        .as_array_mut()
        .expect("span rows")
    {
        row.as_object_mut()
            .expect("span row object")
            .remove("generation");
    }
    let error = serde_json::from_value::<ConversationRows>(old_json)
        .expect_err("pre-generation rows fail closed at deserialization");
    assert!(
        error.to_string().contains("generation"),
        "deserialization error names the missing generation column: {error}"
    );
}

#[test]
fn rows_reject_inconsistent_generations() {
    let mut conversation = conversation(22);
    commit_text_turn(&mut conversation, 220);
    commit_text_turn(&mut conversation, 221);
    let rows = conversation
        .snapshot()
        .expect("snapshot")
        .to_rows()
        .expect("rows");

    let mut bad_conversation = rows.clone();
    bad_conversation.conversation.generation += 1;
    assert!(matches!(
        bad_conversation
            .into_snapshot()
            .expect_err("generation/structural_version divergence rejected"),
        RowMappingError::InvalidRow {
            path,
            table: "conversation_records",
            ..
        } if path == "$.conversation.generation"
    ));

    let mut bad_lineage = rows.clone();
    bad_lineage.lineage_turns[0].generation += 1;
    assert!(matches!(
        bad_lineage
            .into_snapshot()
            .expect_err("foreign lineage generation rejected"),
        RowMappingError::InvalidRow {
            table: "conversation_lineage_turn_records",
            ..
        }
    ));

    let mut bad_span = rows;
    assert!(
        !bad_span.projection_spans.is_empty(),
        "committed conversation exports at least one projection span"
    );
    bad_span.projection_spans[0].generation += 1;
    assert!(matches!(
        bad_span
            .into_snapshot()
            .expect_err("foreign span generation rejected"),
        RowMappingError::InvalidRow {
            table: "projection_span_records",
            ..
        }
    ));

    // A compacted conversation stamps artifact membership rows with the same
    // generation; a divergent artifact generation is rejected as well.
    // (`self::` bypasses the local `conversation` binding shadowing the
    // module-level fixture constructor.)
    let mut compacted = self::conversation(22_000);
    commit_text_turn(&mut compacted, 222);
    let covers = range(&compacted, 0, 1);
    let produced_by = strategy("artifact-generation");
    let artifact = summary_artifact(
        &compacted,
        covers.clone(),
        22_500,
        produced_by.clone(),
        "summary:222",
    );
    let plan = CompactionPlan::new(
        &compacted,
        vec![CompactionStep::raw(covers, artifact.id(), produced_by)],
        vec![artifact],
    );
    compacted.apply_compaction(&plan).expect("compaction");
    let mut bad_artifact = compacted
        .snapshot()
        .expect("snapshot")
        .to_rows()
        .expect("rows");
    assert!(
        !bad_artifact.artifacts.is_empty(),
        "compacted conversation exports artifact membership rows"
    );
    bad_artifact.artifacts[0].generation += 1;
    assert!(matches!(
        bad_artifact
            .into_snapshot()
            .expect_err("foreign artifact generation rejected"),
        RowMappingError::InvalidRow {
            table: "artifact_records",
            ..
        }
    ));
}

/// Builds the merged multi-generation row set a store would return after two
/// export generations of the same Conversation were accumulated.
fn merged_generations(
    first: ConversationRows,
    second: ConversationRows,
) -> ConversationRowInsertSet {
    let mut merged = ConversationRowInsertSet::from(first);
    merged.merge(second.into());
    merged
}

#[test]
fn insert_set_into_snapshot_selects_the_maximum_generation() {
    let mut conversation = conversation(23);
    commit_text_turn(&mut conversation, 230);
    commit_text_turn(&mut conversation, 231);
    let first = conversation
        .snapshot()
        .expect("first snapshot")
        .to_rows()
        .expect("first rows");
    let first_generation = first.conversation.generation;

    // Evolve every generation-scoped row kind: commits advance the lineage,
    // compaction rewrites spans and artifact membership.
    commit_text_turn(&mut conversation, 232);
    let covers = range(&conversation, 0, 1);
    let produced_by = strategy("merged-generation");
    let artifact = summary_artifact(
        &conversation,
        covers.clone(),
        23_500,
        produced_by.clone(),
        "summary:230",
    );
    let plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(covers, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation
        .apply_compaction(&plan)
        .expect("compaction evolves the export generation");

    let latest_snapshot = conversation.snapshot().expect("latest snapshot");
    let second = latest_snapshot.to_rows().expect("second rows");
    assert!(
        second.conversation.generation > first_generation,
        "commit plus compaction advances the generation"
    );

    let merged = merged_generations(first, second);
    let reassembled = merged
        .into_snapshot()
        .expect("merged generations reassemble the latest state");
    assert_eq!(
        reassembled, latest_snapshot,
        "the maximum generation describes the current state"
    );
    Conversation::restore(reassembled).expect("selected snapshot restores");
}

#[test]
fn insert_set_into_snapshot_rejects_a_sparse_selected_generation() {
    let mut conversation = conversation(25);
    commit_text_turn(&mut conversation, 250);
    commit_text_turn(&mut conversation, 251);
    let first = conversation
        .snapshot()
        .expect("first snapshot")
        .to_rows()
        .expect("first rows");
    commit_text_turn(&mut conversation, 252);
    let second = conversation
        .snapshot()
        .expect("second snapshot")
        .to_rows()
        .expect("second rows");
    let max_generation = second.conversation.generation;

    // A missing lineage row inside the selected generation is a density gap.
    let mut sparse = merged_generations(first.clone(), second.clone());
    sparse
        .lineage_turns
        .retain(|row| !(row.generation == max_generation && row.lineage_sequence == 1));
    assert!(matches!(
        sparse
            .into_snapshot()
            .expect_err("a missing selected-generation lineage row is a gap"),
        RowMappingError::SequenceGap {
            table: "conversation_lineage_turn_records",
            ..
        }
    ));

    // No lineage rows at all at the selected generation is an explicit error,
    // not a vacuously dense empty lineage.
    let mut missing = merged_generations(first.clone(), second.clone());
    missing
        .lineage_turns
        .retain(|row| row.generation != max_generation);
    assert!(matches!(
        missing
            .into_snapshot()
            .expect_err("a selected generation without lineage rows is invalid"),
        RowMappingError::InvalidRow {
            path,
            table: "conversation_lineage_turn_records",
            ..
        } if path == "$.lineage_turns"
    ));

    // Same for projection span rows of the selected generation.
    let mut missing_spans = merged_generations(first, second);
    missing_spans
        .projection_spans
        .retain(|row| row.generation != max_generation);
    assert!(matches!(
        missing_spans
            .into_snapshot()
            .expect_err("a selected generation without span rows is invalid"),
        RowMappingError::InvalidRow {
            path,
            table: "projection_span_records",
            ..
        } if path == "$.projection_spans"
    ));
}

#[test]
fn insert_set_into_snapshot_rejects_conflicting_and_dangling_rows() {
    let mut conversation = conversation(27);
    commit_text_turn(&mut conversation, 270);
    commit_text_turn(&mut conversation, 271);
    let first = conversation
        .snapshot()
        .expect("first snapshot")
        .to_rows()
        .expect("first rows");
    commit_text_turn(&mut conversation, 272);
    let second = conversation
        .snapshot()
        .expect("second snapshot")
        .to_rows()
        .expect("second rows");
    let max_generation = second.conversation.generation;

    assert!(matches!(
        ConversationRowInsertSet::default()
            .into_snapshot()
            .expect_err("an empty row set has no generation to select"),
        RowMappingError::InvalidRow {
            path,
            table: "conversation_records",
            ..
        } if path == "$.conversations"
    ));

    // Two distinct conversation rows at the maximum generation make the
    // current state ambiguous.
    let mut conflicting = merged_generations(first.clone(), second.clone());
    let mut tampered = conflicting
        .conversations
        .iter()
        .find(|row| row.generation == max_generation)
        .expect("maximum generation conversation row")
        .clone();
    tampered.head_turn_count += 1;
    conflicting.conversations.push(tampered);
    assert!(matches!(
        conflicting
            .into_snapshot()
            .expect_err("conflicting rows at the maximum generation are ambiguous"),
        RowMappingError::DuplicatePrimaryKey {
            table: "conversation_records",
            ..
        }
    ));

    // Association rows newer than every conversation row signal an incomplete
    // store read.
    let mut dangling = merged_generations(first.clone(), second.clone());
    let mut future = dangling
        .lineage_turns
        .iter()
        .find(|row| row.generation == max_generation)
        .expect("selected generation lineage row")
        .clone();
    future.generation = max_generation + 1;
    dangling.lineage_turns.push(future);
    assert!(matches!(
        dangling
            .into_snapshot()
            .expect_err("association rows newer than every conversation row dangle"),
        RowMappingError::InvalidRow {
            table: "conversation_lineage_turn_records",
            reason,
            ..
        } if reason.contains("newer")
    ));

    // A shared fact key with diverging content is corrupt even though merged
    // generations legitimately repeat identical facts.
    let mut conflicted_facts = merged_generations(first.clone(), second.clone());
    let mut forged = conflicted_facts.messages[0].clone();
    forged.payload = user("forged");
    conflicted_facts.messages.push(forged);
    assert!(matches!(
        conflicted_facts
            .into_snapshot()
            .expect_err("conflicting fact rows are rejected"),
        RowMappingError::DuplicatePrimaryKey {
            table: "message_records",
            ..
        }
    ));

    // Foreign-owner rows are never silently dropped as history.
    let mut foreign = merged_generations(first, second);
    foreign.lineage_turns[0].conversation_id = conversation_id(27_999);
    assert!(matches!(
        foreign
            .into_snapshot()
            .expect_err("foreign-owner rows are rejected before generation filtering"),
        RowMappingError::ConversationMismatch { .. }
    ));
}

#[test]
fn insert_set_into_snapshot_survives_artifact_membership_changes_across_generations() {
    let mut conversation = conversation(26);
    commit_text_turn(&mut conversation, 260);
    commit_text_turn(&mut conversation, 261);
    commit_text_turn(&mut conversation, 262);

    let covers = range(&conversation, 0, 2);
    let produced_by = strategy("artifact-evolution");
    let first_artifact = summary_artifact(
        &conversation,
        covers.clone(),
        26_500,
        produced_by.clone(),
        "summary:260-261",
    );
    let plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(
            covers,
            first_artifact.id(),
            produced_by.clone(),
        )],
        vec![first_artifact.clone()],
    );
    conversation
        .apply_compaction(&plan)
        .expect("first compaction");
    let first = conversation
        .snapshot()
        .expect("first snapshot")
        .to_rows()
        .expect("first rows");
    assert!(
        first
            .artifacts
            .iter()
            .any(|row| row.artifact_id == first_artifact.id()),
        "first generation retains the first artifact"
    );

    // Revert behind the compacted range and grow a new branch: the old
    // artifact's provenance no longer matches the active head, so the next
    // compaction drops it from the retained set.
    let boundary = conversation.valid_boundaries()[1];
    conversation
        .revert_to(boundary)
        .expect("revert behind the compacted range");
    commit_text_turn(&mut conversation, 263);
    commit_text_turn(&mut conversation, 264);

    let new_covers = range(&conversation, 0, 3);
    let second_artifact = summary_artifact(
        &conversation,
        new_covers.clone(),
        26_501,
        produced_by.clone(),
        "summary:new-branch",
    );
    let new_plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::spans(
            new_covers,
            second_artifact.id(),
            produced_by,
        )],
        vec![second_artifact.clone()],
    );
    conversation
        .apply_compaction(&new_plan)
        .expect("consolidating compaction on the new branch");

    let latest_snapshot = conversation.snapshot().expect("latest snapshot");
    let second = latest_snapshot.to_rows().expect("second rows");
    assert!(
        second
            .artifacts
            .iter()
            .all(|row| row.artifact_id != first_artifact.id()),
        "the stale artifact is dropped from the new generation"
    );
    assert!(
        second
            .artifacts
            .iter()
            .any(|row| row.artifact_id == second_artifact.id()),
        "the new branch's artifact is retained"
    );

    let merged = merged_generations(first, second);
    let reassembled = merged
        .into_snapshot()
        .expect("artifact membership changes across generations stay reassemblable");
    assert_eq!(reassembled, latest_snapshot);
    Conversation::restore(reassembled).expect("selected snapshot restores");
}

#[test]
fn insert_set_against_follows_commit_evolution_without_conflict() {
    let mut conversation = conversation(30);
    commit_text_turn(&mut conversation, 300);
    commit_text_turn(&mut conversation, 301);
    let before = conversation
        .snapshot()
        .expect("first snapshot")
        .to_rows()
        .expect("first rows");

    commit_text_turn(&mut conversation, 302);
    let latest_snapshot = conversation.snapshot().expect("latest snapshot");
    let after = latest_snapshot.to_rows().expect("second rows");
    let new_generation = after.conversation.generation;
    assert!(new_generation > before.conversation.generation);

    let inserts = after
        .insert_set_against(&before)
        .expect("re-exporting an evolved conversation is insert-only");

    assert_eq!(inserts.conversations.len(), 1);
    assert_eq!(inserts.conversations[0].generation, new_generation);
    // The whole lineage re-stamps under the new generation; every lineage row
    // of the new export is a new row keyed by (id, generation, sequence).
    assert_eq!(inserts.lineage_turns.len(), after.lineage_turns.len());
    assert!(
        inserts
            .lineage_turns
            .iter()
            .all(|row| row.generation == new_generation),
        "lineage rows insert under the new generation"
    );
    // Only the newly committed turn contributes new fact rows.
    assert_eq!(
        inserts
            .raw_turns
            .iter()
            .map(|row| row.turn_id)
            .collect::<Vec<_>>(),
        vec![turn_id(302)],
        "shared raw membership rows are not duplicated"
    );
    assert_eq!(
        inserts
            .turns
            .iter()
            .map(|row| row.turn_id)
            .collect::<Vec<_>>(),
        vec![turn_id(302)]
    );
    assert!(
        inserts
            .messages
            .iter()
            .all(|row| row.turn_id == turn_id(302)),
        "only the new turn's messages insert"
    );
    assert!(inserts.projections.is_empty());
    // Span rows exist even without compaction (the raw span set); they are
    // generation-scoped, so they re-insert under the new generation.
    assert_eq!(inserts.projection_spans.len(), after.projection_spans.len());
    assert!(
        inserts
            .projection_spans
            .iter()
            .all(|row| row.generation == new_generation),
        "span rows re-insert under the new generation"
    );
    assert!(inserts.artifacts.is_empty());
}

#[test]
fn insert_set_against_follows_revert_evolution_without_conflict() {
    let mut conversation = conversation(31);
    commit_text_turn(&mut conversation, 310);
    commit_text_turn(&mut conversation, 311);
    commit_text_turn(&mut conversation, 312);
    let before = conversation
        .snapshot()
        .expect("first snapshot")
        .to_rows()
        .expect("first rows");

    // Revert and grow a different branch: the lineage slot at sequence 1 now
    // references a different turn under a new generation.
    let boundary = conversation.valid_boundaries()[1];
    conversation.revert_to(boundary).expect("revert");
    commit_text_turn(&mut conversation, 313);
    let after = conversation
        .snapshot()
        .expect("second snapshot")
        .to_rows()
        .expect("second rows");
    let new_generation = after.conversation.generation;
    assert!(new_generation > before.conversation.generation);
    assert_eq!(after.lineage_turns.len(), 2);
    assert_eq!(after.lineage_turns[1].turn_id, turn_id(313));
    assert_eq!(before.lineage_turns[1].turn_id, turn_id(311));

    let inserts = after
        .insert_set_against(&before)
        .expect("revert evolution must not conflict with the stored generation");

    assert_eq!(inserts.conversations.len(), 1);
    assert_eq!(inserts.lineage_turns.len(), 2);
    assert!(
        inserts
            .lineage_turns
            .iter()
            .all(|row| row.generation == new_generation),
        "the new branch's lineage coexists with the old generation"
    );
    assert_eq!(inserts.raw_turns.len(), 1);
    assert_eq!(inserts.raw_turns[0].turn_id, turn_id(313));
    // The merged store keeps both generations at lineage sequence 1.
    let mut merged = ConversationRowInsertSet::from(before.clone());
    merged.merge(inserts);
    let mut at_slot = merged
        .lineage_turns
        .iter()
        .filter(|row| row.lineage_sequence == 1)
        .map(|row| (row.generation, row.turn_id))
        .collect::<Vec<_>>();
    at_slot.sort();
    assert_eq!(
        at_slot,
        vec![
            (before.conversation.generation, turn_id(311)),
            (new_generation, turn_id(313)),
        ],
        "old and new branch lineage rows coexist at the same sequence"
    );
}

#[test]
fn insert_set_against_follows_compaction_evolution_without_conflict() {
    let mut conversation = conversation(32);
    commit_text_turn(&mut conversation, 320);
    commit_text_turn(&mut conversation, 321);
    let before = conversation
        .snapshot()
        .expect("first snapshot")
        .to_rows()
        .expect("first rows");
    assert!(before.artifacts.is_empty());

    let covers = range(&conversation, 0, 1);
    let produced_by = strategy("diff-compaction");
    let artifact = summary_artifact(
        &conversation,
        covers.clone(),
        32_500,
        produced_by.clone(),
        "summary:320",
    );
    let plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(covers, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation.apply_compaction(&plan).expect("compaction");
    let after = conversation
        .snapshot()
        .expect("second snapshot")
        .to_rows()
        .expect("second rows");
    let new_generation = after.conversation.generation;

    let inserts = after
        .insert_set_against(&before)
        .expect("compaction evolution must not conflict with the stored generation");

    assert_eq!(inserts.conversations.len(), 1);
    assert!(!inserts.projection_spans.is_empty());
    assert!(
        inserts
            .projection_spans
            .iter()
            .all(|row| row.generation == new_generation),
        "rewritten span rows insert under the new generation"
    );
    assert!(!inserts.artifacts.is_empty());
    assert!(
        inserts
            .artifacts
            .iter()
            .all(|row| row.generation == new_generation),
        "artifact membership rows insert under the new generation"
    );
    // Compaction does not add turns: no fact rows insert at all.
    assert!(inserts.raw_turns.is_empty());
    assert!(inserts.turns.is_empty());
    assert!(inserts.messages.is_empty());
}

#[test]
fn insert_set_against_rejects_same_generation_tampering() {
    let mut conv = conversation(34);
    commit_text_turn(&mut conv, 340);
    commit_text_turn(&mut conv, 341);
    let rows = conv.snapshot().expect("snapshot").to_rows().expect("rows");
    let generation = rows.conversation.generation;

    // Same id + same generation + different content is still a conflict.
    let mut tampered_conversation = rows.clone();
    tampered_conversation.conversation.head_turn_count += 1;
    assert!(matches!(
        tampered_conversation
            .insert_set_against(&rows)
            .expect_err("same-generation conversation drift conflicts"),
        RowMappingError::InsertConflict { table, key, .. }
            if table == "conversation_records"
                && key == format!("{}#{}", conv.id(), generation)
    ));

    // Two valid exports of the same Conversation at the same generation but
    // with diverging content (the same conversation evolved two ways) still
    // conflict: generation scoping only legalizes *newer* generations.
    // `conversation(seed)` is deterministic, so two instances built with the
    // same seed share the conversation id and replay to the same generation.
    let mut branch_a = conversation(34);
    let mut branch_b = conversation(34);
    commit_text_turn(&mut branch_a, 340);
    commit_text_turn(&mut branch_a, 341);
    commit_text_turn(&mut branch_b, 340);
    commit_text_turn(&mut branch_b, 341);
    commit_text_turn(&mut branch_a, 342);
    commit_text_turn(&mut branch_b, 343);
    let rows_a = branch_a
        .snapshot()
        .expect("branch a snapshot")
        .to_rows()
        .expect("branch a rows");
    let rows_b = branch_b
        .snapshot()
        .expect("branch b snapshot")
        .to_rows()
        .expect("branch b rows");
    assert_eq!(
        rows_a.conversation.generation,
        rows_b.conversation.generation
    );
    assert!(matches!(
        rows_b
            .insert_set_against(&rows_a)
            .expect_err("same-generation divergent content conflicts"),
        RowMappingError::InsertConflict { .. }
    ));
}

#[test]
fn insert_set_against_rejects_corrupt_rows_on_either_side() {
    let mut conversation = conversation(36);
    commit_text_turn(&mut conversation, 360);
    commit_text_turn(&mut conversation, 361);
    let rows = conversation
        .snapshot()
        .expect("snapshot")
        .to_rows()
        .expect("rows");

    // A corrupt candidate fails validation before any diffing happens, with
    // the same error `into_snapshot` reports for the same corruption.
    let mut corrupt_candidate = rows.clone();
    let duplicate = corrupt_candidate.messages[0].clone();
    corrupt_candidate.messages.push(duplicate);
    assert!(matches!(
        corrupt_candidate
            .insert_set_against(&rows)
            .expect_err("corrupt candidate rows rejected"),
        RowMappingError::DuplicatePrimaryKey {
            table: "message_records",
            ..
        }
    ));

    // Corrupt stored rows are validated too, so a poisoned store cannot be
    // diffed against silently.
    let mut corrupt_existing = rows.clone();
    let removed_turn = corrupt_existing.raw_turns[0].turn_id;
    corrupt_existing
        .messages
        .retain(|row| row.turn_id != removed_turn);
    assert!(matches!(
        rows.insert_set_against(&corrupt_existing)
            .expect_err("corrupt stored rows rejected"),
        RowMappingError::MissingMessageRows { turn_id, .. } if turn_id == removed_turn
    ));
}

#[test]
fn restored_conversation_message_id_index_rejects_reuse() {
    let mut conversation = conversation(37);
    commit_text_turn(&mut conversation, 370);
    commit_text_turn(&mut conversation, 371);
    let snapshot = conversation.snapshot().expect("snapshot");

    // The message-id index rebuilt by `History::from_restored` must cover the
    // restored raw history exactly like the append-maintained one does.
    let mut restored = Conversation::restore(snapshot).expect("restore");
    let duplicate = restored
        .begin_turn(
            turn_id(372),
            message_id(370 * 10),
            user("duplicate message"),
        )
        .expect_err("restored history index retains committed message ids");
    assert_eq!(
        duplicate,
        ConversationError::PendingTurn(PendingTurnError::DuplicateMessageId {
            message_id: message_id(370 * 10),
        })
    );
}

#[test]
fn insert_set_against_rows_merge_into_the_latest_snapshot() {
    let mut conversation = conversation(35);
    commit_text_turn(&mut conversation, 350);
    commit_text_turn(&mut conversation, 351);
    let before = conversation
        .snapshot()
        .expect("first snapshot")
        .to_rows()
        .expect("first rows");

    commit_text_turn(&mut conversation, 352);
    let latest_snapshot = conversation.snapshot().expect("latest snapshot");
    let after = latest_snapshot.to_rows().expect("second rows");

    // Simulate a store that already holds the first generation: apply the
    // insert-only diff, then reassemble the current state from both.
    let inserts = after
        .insert_set_against(&before)
        .expect("evolved export is insert-only");
    let mut store = ConversationRowInsertSet::from(before);
    store.merge(inserts);
    let reassembled = store
        .into_snapshot()
        .expect("stored generations reassemble the latest state");
    assert_eq!(reassembled, latest_snapshot);
    Conversation::restore(reassembled).expect("selected snapshot restores");
}

#[test]
fn snapshot_rejects_active_text_and_tool_partials() {
    let text_id = BlockId::new("text-partial");
    let mut text_partial = conversation(5);
    begin(&mut text_partial, 50);
    text_partial
        .start_assistant()
        .expect("start streaming assistant");
    for event in [
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: text_id.clone(),
            kind: BlockKind::Text,
        },
        StreamEvent::BlockDelta {
            id: text_id,
            delta: Delta::Text("partial answer".to_owned()),
        },
    ] {
        text_partial
            .push_assistant_event(event)
            .expect("push partial text event");
    }
    assert_snapshot_rejected_without_state_change(&text_partial);

    let tool_id = BlockId::new("tool-partial");
    let mut tool_partial = conversation(6);
    begin(&mut tool_partial, 60);
    tool_partial
        .start_assistant()
        .expect("start streaming assistant");
    for event in [
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: tool_id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "lookup".to_owned(),
                tool_call_id: "call-partial".to_owned(),
            },
        },
        StreamEvent::BlockDelta {
            id: tool_id,
            delta: Delta::Json("{\"query\":\"half".to_owned()),
        },
    ] {
        tool_partial
            .push_assistant_event(event)
            .expect("push partial tool event");
    }
    assert_snapshot_rejected_without_state_change(&tool_partial);
}

#[test]
fn snapshot_rejects_open_call_and_ready_to_commit_pending() {
    let mut open_call = conversation(7);
    begin(&mut open_call, 70);
    assert_eq!(
        freeze_response(
            &mut open_call,
            assistant_response(
                vec![tool_use("call-open")],
                5,
                1,
                StopReason::ToolUse,
                "open-call",
            ),
            701,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    open_call
        .register_tool_calls(vec![ToolCallMapping::new("call-open", call_id(970))])
        .expect("register open call");
    assert_eq!(
        open_call.pending().expect("pending").phase(),
        PendingTurnPhase::AwaitingToolResults
    );
    assert_snapshot_rejected_without_state_change(&open_call);

    let mut ready = conversation(8);
    begin(&mut ready, 80);
    assert_eq!(
        freeze_response(
            &mut ready,
            assistant_response(vec![text("done")], 2, 1, StopReason::EndTurn, "ready"),
            801,
        ),
        AssistantFinish::ReadyToCommit
    );
    assert_eq!(
        ready.pending().expect("pending").phase(),
        PendingTurnPhase::ReadyToCommit
    );
    assert_snapshot_rejected_without_state_change(&ready);
}
