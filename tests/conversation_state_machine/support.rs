//! Public-API state-machine acceptance tests for Conversation Core.

use agent_lib::{
    client::Response,
    conversation::{
        Artifact, ArtifactId, ArtifactProvenance, AssistantFinish, Boundary, BoundaryError,
        CheckedTurnRange, CompactionPlan, CompactionStep, Conversation, ConversationConfig,
        ConversationId, ConversationRows, ConversationSnapshot, MessageId, PendingTurnPhase,
        Projection, StrategyRef, TokenAccounting, ToolCallId, ToolCallIndex, ToolCallMapping, Turn,
        TurnId, TurnMeta,
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::{Normalized, StopReason},
        tool::{ToolResponse, ToolStatus},
        usage::Usage,
    },
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

mod assertions;

pub(crate) use assertions::{
    assert_can_commit_followup, assert_previous_raw_snapshots_unchanged,
    assert_state_machine_invariants, text_values,
};

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_9f00_0000_0000_0000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeState {
    id: ConversationId,
    version: u64,
    head: Boundary,
    current_turns: Vec<ObservedTurn>,
    lineage_turn_ids: Vec<TurnId>,
    raw_turns: Vec<ObservedTurn>,
    pending: Option<ObservedPending>,
    projection: Projection,
    index: ToolCallIndex,
    effective_messages: Vec<Message>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedTurn {
    id: TurnId,
    parent: Option<TurnId>,
    messages: Vec<ObservedMessage>,
    pairings: Vec<ObservedPairing>,
    serialized: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedMessage {
    id: MessageId,
    role: Role,
    payload: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedPairing {
    call_id: ToolCallId,
    provider_call_id: Option<String>,
    call_msg: MessageId,
    result_msg: MessageId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedPending {
    id: TurnId,
    parent: Option<TurnId>,
    phase: PendingTurnPhase,
    messages: Vec<ObservedMessage>,
    tool_calls: Vec<ObservedPendingToolCall>,
    usage: Usage,
    responses: Value,
    unmapped_provider_call_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedPendingToolCall {
    call_id: ToolCallId,
    provider_call_id: String,
    call_message_id: MessageId,
    result_message_id: Option<MessageId>,
}

pub(crate) fn conversation_id(seed: u128) -> ConversationId {
    ConversationId::new(Uuid::from_u128(UUID_BASE + seed))
}

pub(crate) fn turn_id(seed: u128) -> TurnId {
    TurnId::new(Uuid::from_u128(UUID_BASE + seed))
}

pub(crate) fn message_id(seed: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(UUID_BASE + seed))
}

pub(crate) fn call_id(seed: u128) -> ToolCallId {
    ToolCallId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn artifact_id(seed: u128) -> ArtifactId {
    ArtifactId::new(Uuid::from_u128(UUID_BASE + seed))
}

pub(crate) fn conversation(seed: u128) -> Conversation {
    Conversation::new(
        conversation_id(seed),
        ConversationConfig::new(Some("Exercise every public transition.".to_owned())),
    )
}

pub(crate) fn text(value: impl Into<String>) -> ContentBlock {
    ContentBlock::Text {
        text: value.into(),
        extra: Map::new(),
    }
}

fn thinking(value: impl Into<String>) -> ContentBlock {
    ContentBlock::Thinking {
        text: value.into(),
        signature: Some("state-machine-signature".to_owned()),
        extra: Map::new(),
    }
}

fn user(label: impl Into<String>) -> Message {
    Message {
        role: Role::User,
        content: vec![text(label)],
    }
}

pub(crate) fn assistant_response(
    content: Vec<ContentBlock>,
    usage: Usage,
    stop_reason: StopReason,
    request_id: &str,
) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content,
        },
        usage,
        stop_reason: normalized_stop(stop_reason),
        extra: Map::from_iter([("request_id".to_owned(), json!(request_id))]),
    }
}

fn tool_use(provider_call_id: &str, tool_name: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: provider_call_id.to_owned(),
        name: tool_name.to_owned(),
        input: json!({ "query": provider_call_id }),
        extra: Map::new(),
    }
}

pub(crate) fn tool_response(
    provider_call_id: &str,
    value: &str,
    status: ToolStatus,
) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_call_id.to_owned(),
        content: vec![text(value)],
        status,
        extra: Map::new(),
    }
}

pub(crate) fn usage(input: u32, output: u32) -> Usage {
    Usage {
        input,
        output,
        ..Usage::default()
    }
}

fn normalized_stop(reason: StopReason) -> Normalized<StopReason> {
    let raw = match reason {
        StopReason::ToolUse => "tool_use",
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::Refusal => "refusal",
        StopReason::Other => "other",
    };
    Normalized::from_mapped(reason, raw)
}

pub(crate) fn explicit_meta(seed: u128, source: &str) -> TurnMeta {
    TurnMeta::new(
        Usage::default(),
        Some(format!("2026-07-13T12:{:02}:00Z", seed % 60)),
        Some(source.to_owned()),
        Map::from_iter([("state_machine_seed".to_owned(), json!(seed))]),
    )
}

pub(crate) fn begin(conversation: &mut Conversation, seed: u128, label: &str) {
    conversation
        .begin_turn(
            turn_id(seed),
            message_id(seed * 100),
            user(format!("user:{label}:{seed}")),
        )
        .unwrap_or_else(|error| panic!("begin {label} turn {seed} failed: {error:?}"));
}

pub(crate) fn finish_complete_response(
    conversation: &mut Conversation,
    response: Response,
    message_seed: u128,
) -> AssistantFinish {
    conversation
        .start_assistant_response(response)
        .expect("start complete assistant response");
    conversation
        .finish_assistant(message_id(message_seed))
        .expect("finish complete assistant response")
}

pub(crate) fn commit_text_turn(conversation: &mut Conversation, seed: u128, answer: &str) {
    begin(conversation, seed, answer);
    assert_eq!(
        finish_complete_response(
            conversation,
            assistant_response(
                vec![text(format!("assistant:{answer}:{seed}"))],
                usage(2, 1),
                StopReason::EndTurn,
                answer,
            ),
            seed * 100 + 1,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(explicit_meta(seed, answer))
        .expect("commit text turn");
}

pub(crate) fn commit_tool_turn(
    conversation: &mut Conversation,
    seed: u128,
    provider_call_id: &str,
    framework_call_seed: u128,
    final_label: &str,
) {
    begin(conversation, seed, final_label);
    assert_eq!(
        finish_complete_response(
            conversation,
            assistant_response(
                vec![
                    text(format!("assistant:tool-request:{provider_call_id}")),
                    thinking("checking tool budget"),
                    tool_use(provider_call_id, "lookup"),
                ],
                usage(5, 2),
                StopReason::ToolUse,
                "tool-request",
            ),
            seed * 100 + 1,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    conversation
        .register_tool_calls(vec![ToolCallMapping::new(
            provider_call_id,
            call_id(framework_call_seed),
        )])
        .expect("register tool call");
    conversation
        .append_tool_response(
            message_id(seed * 100 + 2),
            tool_response(provider_call_id, "tool result", ToolStatus::Ok),
        )
        .expect("append tool response");
    assert_eq!(
        finish_complete_response(
            conversation,
            assistant_response(
                vec![text(format!("assistant:{final_label}:{seed}"))],
                usage(3, 1),
                StopReason::EndTurn,
                "tool-final",
            ),
            seed * 100 + 3,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(explicit_meta(seed, final_label))
        .expect("commit tool turn");
}

pub(crate) fn stream_parallel_tool_uses(
    conversation: &mut Conversation,
    assistant_message_seed: u128,
    left_provider_call_id: &str,
    right_provider_call_id: &str,
) {
    let text_id = BlockId::new(format!("text-{assistant_message_seed}"));
    let left_id = BlockId::new(format!("left-{assistant_message_seed}"));
    let right_id = BlockId::new(format!("right-{assistant_message_seed}"));

    conversation
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
            id: text_id.clone(),
            delta: Delta::Text("parallel lookup ".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: text_id.clone(),
            delta: Delta::Text("started".to_owned()),
        },
        StreamEvent::BlockStop {
            id: text_id.clone(),
        },
        StreamEvent::BlockStart {
            id: left_id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "lookup_left".to_owned(),
                tool_call_id: left_provider_call_id.to_owned(),
            },
        },
        StreamEvent::BlockStart {
            id: right_id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "lookup_right".to_owned(),
                tool_call_id: right_provider_call_id.to_owned(),
            },
        },
        StreamEvent::BlockDelta {
            id: left_id.clone(),
            delta: Delta::Json("{\"side\":\"lef".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: right_id.clone(),
            delta: Delta::Json("{\"side\":\"rig".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: left_id.clone(),
            delta: Delta::Json("t\"}".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: right_id.clone(),
            delta: Delta::Json("ht\"}".to_owned()),
        },
        StreamEvent::BlockStop {
            id: right_id.clone(),
        },
        StreamEvent::BlockStop {
            id: left_id.clone(),
        },
        StreamEvent::Usage(usage(9, 4)),
        StreamEvent::ResponseMetadata {
            extra: Map::from_iter([("request_id".to_owned(), json!("stream-parallel"))]),
        },
        StreamEvent::MessageStop {
            stop_reason: normalized_stop(StopReason::ToolUse),
        },
    ] {
        conversation
            .push_assistant_event(event)
            .expect("push streamed parallel tool event");
    }

    assert_eq!(
        conversation
            .finish_assistant(message_id(assistant_message_seed))
            .expect("finish streamed parallel response"),
        AssistantFinish::RequiresToolCallMappings
    );
}

fn checked_range(
    conversation: &Conversation,
    start_index: usize,
    end_index: usize,
) -> CheckedTurnRange {
    let boundaries = conversation.valid_boundaries();
    conversation
        .checked_turn_range(boundaries[start_index], boundaries[end_index])
        .expect("create checked turn range")
}

fn strategy(version: &str) -> StrategyRef {
    StrategyRef::new("state-machine-summary", version)
}

fn summary_artifact(
    conversation: &Conversation,
    range: CheckedTurnRange,
    id_seed: u128,
    produced_by: StrategyRef,
    label: &str,
) -> Artifact {
    Artifact::new(
        artifact_id(id_seed),
        vec![Message {
            role: Role::Assistant,
            content: vec![text(label)],
        }],
        ArtifactProvenance::new(
            range,
            produced_by,
            TokenAccounting::new(
                Usage {
                    input: (conversation.effective_view().len() as u32) * 10,
                    ..Usage::default()
                },
                Usage {
                    input: 3,
                    ..Usage::default()
                },
            ),
            Map::new(),
        ),
    )
    .expect("summary artifact must have render messages")
}

pub(crate) fn apply_raw_compaction(
    conversation: &mut Conversation,
    start: usize,
    end: usize,
    artifact_seed: u128,
    strategy_version: &str,
    label: &str,
) {
    let range = checked_range(conversation, start, end);
    let produced_by = strategy(strategy_version);
    let artifact = summary_artifact(
        conversation,
        range.clone(),
        artifact_seed,
        produced_by.clone(),
        label,
    );
    let plan = CompactionPlan::new(
        conversation,
        vec![CompactionStep::raw(range, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation
        .apply_compaction(&plan)
        .expect("apply raw compaction");
}

pub(crate) fn apply_span_compaction(
    conversation: &mut Conversation,
    start: usize,
    end: usize,
    artifact_seed: u128,
    strategy_version: &str,
    label: &str,
) {
    let range = checked_range(conversation, start, end);
    let produced_by = strategy(strategy_version);
    let artifact = summary_artifact(
        conversation,
        range.clone(),
        artifact_seed,
        produced_by.clone(),
        label,
    );
    let plan = CompactionPlan::new(
        conversation,
        vec![CompactionStep::spans(range, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation
        .apply_compaction(&plan)
        .expect("apply span compaction");
}

pub(crate) fn snapshot_restore_via_json_and_rows(label: &str, conversation: &Conversation) {
    let before = runtime_state(conversation);
    let snapshot = conversation
        .snapshot()
        .unwrap_or_else(|error| panic!("{label}: snapshot failed: {error:?}"));
    let encoded_snapshot = serde_json::to_string(&snapshot)
        .unwrap_or_else(|error| panic!("{label}: encode snapshot failed: {error:?}"));
    let decoded_snapshot: ConversationSnapshot = serde_json::from_str(&encoded_snapshot)
        .unwrap_or_else(|error| panic!("{label}: decode snapshot failed: {error:?}"));
    let restored_from_json = Conversation::restore(decoded_snapshot)
        .unwrap_or_else(|error| panic!("{label}: restore JSON snapshot failed: {error:?}"));
    assert_eq!(
        runtime_state(&restored_from_json),
        before,
        "{label}: JSON restore changed observable state"
    );

    let rows = snapshot
        .to_rows()
        .unwrap_or_else(|error| panic!("{label}: snapshot to rows failed: {error:?}"));
    let encoded_rows = serde_json::to_string(&rows)
        .unwrap_or_else(|error| panic!("{label}: encode rows: {error}"));
    let decoded_rows: ConversationRows = serde_json::from_str(&encoded_rows)
        .unwrap_or_else(|error| panic!("{label}: decode DB-neutral row payload failed: {error:?}"));
    let row_snapshot = ConversationSnapshot::from_rows(decoded_rows)
        .unwrap_or_else(|error| panic!("{label}: rows to snapshot failed: {error:?}"));
    let restored_from_rows = Conversation::restore(row_snapshot)
        .unwrap_or_else(|error| panic!("{label}: restore row snapshot failed: {error:?}"));
    assert_eq!(
        runtime_state(&restored_from_rows),
        before,
        "{label}: row restore changed observable state"
    );
}

pub(crate) fn runtime_state(conversation: &Conversation) -> RuntimeState {
    RuntimeState {
        id: conversation.id(),
        version: conversation.version(),
        head: conversation.head(),
        current_turns: conversation.turns().iter().map(observe_turn).collect(),
        lineage_turn_ids: conversation.lineage_turns().iter().map(Turn::id).collect(),
        raw_turns: conversation
            .raw_turns()
            .into_iter()
            .map(observe_turn)
            .collect(),
        pending: conversation.pending().map(|pending| ObservedPending {
            id: pending.id(),
            parent: pending.parent(),
            phase: pending.phase(),
            messages: pending.messages().iter().map(observe_message).collect(),
            tool_calls: pending
                .tool_calls()
                .iter()
                .map(|tool_call| ObservedPendingToolCall {
                    call_id: tool_call.call_id(),
                    provider_call_id: tool_call.provider_call_id().to_owned(),
                    call_message_id: tool_call.call_message_id(),
                    result_message_id: tool_call.result_message_id(),
                })
                .collect(),
            usage: pending.usage().clone(),
            responses: serde_json::to_value(pending.responses())
                .expect("pending response metadata serializes"),
            unmapped_provider_call_ids: pending.unmapped_provider_call_ids().to_vec(),
        }),
        projection: conversation.projection().clone(),
        index: conversation.tool_call_index().clone(),
        effective_messages: conversation.effective_view().messages().to_vec(),
    }
}

pub(crate) fn raw_snapshots(conversation: &Conversation) -> Vec<ObservedTurn> {
    conversation
        .raw_turns()
        .into_iter()
        .map(observe_turn)
        .collect()
}

fn observe_turn(turn: &Turn) -> ObservedTurn {
    ObservedTurn {
        id: turn.id(),
        parent: turn.parent(),
        messages: turn.messages().iter().map(observe_message).collect(),
        pairings: turn
            .pairings()
            .iter()
            .map(|pairing| ObservedPairing {
                call_id: pairing.call_id(),
                provider_call_id: pairing.provider_call_id().map(ToOwned::to_owned),
                call_msg: pairing.call_msg(),
                result_msg: pairing.result_msg(),
            })
            .collect(),
        serialized: serde_json::to_value(turn).expect("turn serializes through DTO shape"),
    }
}

fn observe_message(message: &agent_lib::conversation::ConversationMessage) -> ObservedMessage {
    ObservedMessage {
        id: message.id(),
        role: message.payload().role,
        payload: serde_json::to_value(message.payload()).expect("message payload serializes"),
    }
}
