//! Snapshot consistency-point tests.

use super::{CONVERSATION_SNAPSHOT_SCHEMA_VERSION, ConversationSnapshot};
use crate::{
    client::Response,
    conversation::{
        AssistantFinish, Conversation, ConversationConfig, ConversationError, ConversationId,
        MessageId, PendingTurnPhase, Projection, SnapshotError, Span, ToolCallId, ToolCallIndex,
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
