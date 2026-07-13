//! Fixtures shared by Boundary signing and rejection tests.

use super::super::Boundary;
use crate::{
    client::Response,
    conversation::{
        AssistantFinish, Conversation, ConversationConfig, ConversationId, ForkOrigin, MessageId,
        PendingTurnPhase, ToolCallId, ToolCallIndex, ToolCallMapping, Turn, TurnId, TurnMeta,
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::{ToolResponse, ToolStatus},
        usage::Usage,
    },
};
use serde_json::{Map, json};
use uuid::Uuid;

mod fork;
mod negative;
mod positive;
mod revert;
mod review;
mod serde;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_8f50_0000_0000_0000;

/// Captures every mutable Conversation component relevant to Boundary errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StateSnapshot {
    version: u64,
    head: Boundary,
    turns: Vec<Turn>,
    lineage_turns: Vec<Turn>,
    raw_turns: Vec<Turn>,
    pending: Option<(TurnId, PendingTurnPhase, Vec<MessageId>)>,
    tool_call_index: ToolCallIndex,
    origin: Option<ForkOrigin>,
}

/// Creates one deterministic external Conversation identity.
pub(super) fn conversation_id(seed: u128) -> ConversationId {
    ConversationId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates one deterministic external Turn identity.
pub(super) fn turn_id(seed: u128) -> TurnId {
    TurnId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates one deterministic external Message identity.
pub(super) fn message_id(seed: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates one deterministic external framework tool-call identity.
pub(super) fn call_id(seed: u128) -> ToolCallId {
    ToolCallId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates an empty Conversation with a deterministic caller-owned identity.
pub(super) fn conversation(seed: u128) -> Conversation {
    Conversation::new(
        conversation_id(seed),
        ConversationConfig::new(Some("Keep boundaries exact.".to_owned())),
    )
}

/// Creates one normalized text block.
fn text(value: impl Into<String>) -> ContentBlock {
    ContentBlock::Text {
        text: value.into(),
        extra: Map::new(),
    }
}

/// Begins one complete user/assistant turn and commits it through the validator.
pub(super) fn commit_text_turn(conversation: &mut Conversation, seed: u128) {
    conversation
        .begin_turn(
            turn_id(seed),
            message_id(seed * 10),
            Message {
                role: Role::User,
                content: vec![text(format!("question:{seed}"))],
            },
        )
        .expect("begin complete text turn");
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
        .expect("start complete assistant response");
    assert_eq!(
        conversation
            .finish_assistant(message_id(seed * 10 + 1))
            .expect("freeze complete assistant response"),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit validator-certified turn");
}

/// Commits one complete tool round-trip through the public pending API.
pub(super) fn commit_tool_turn(
    conversation: &mut Conversation,
    seed: u128,
    provider_call_id: &str,
    call_seed: u128,
) {
    conversation
        .begin_turn(
            turn_id(seed),
            message_id(seed * 10),
            Message {
                role: Role::User,
                content: vec![text(format!("tool-question:{seed}"))],
            },
        )
        .expect("begin tool turn");
    conversation
        .start_assistant_response(Response {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: provider_call_id.to_owned(),
                    name: "lookup".to_owned(),
                    input: json!({ "seed": seed }),
                    extra: Map::new(),
                }],
            },
            usage: Usage::default(),
            stop_reason: StopReason::normalize("tool_use"),
            extra: Map::new(),
        })
        .expect("start tool-use response");
    assert_eq!(
        conversation
            .finish_assistant(message_id(seed * 10 + 1))
            .expect("freeze tool-use response"),
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
            ToolResponse {
                tool_call_id: provider_call_id.to_owned(),
                content: vec![text(format!("tool-result:{seed}"))],
                status: ToolStatus::Ok,
                extra: Map::new(),
            },
        )
        .expect("append tool response");
    conversation
        .start_assistant_response(Response {
            message: Message {
                role: Role::Assistant,
                content: vec![text(format!("tool-answer:{seed}"))],
            },
            usage: Usage::default(),
            stop_reason: StopReason::normalize("end_turn"),
            extra: Map::new(),
        })
        .expect("start final assistant response");
    assert_eq!(
        conversation
            .finish_assistant(message_id(seed * 10 + 3))
            .expect("freeze final assistant response"),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit validator-certified tool turn");
}

/// Begins a pending transaction without committing it.
pub(super) fn begin_pending(conversation: &mut Conversation, seed: u128) {
    conversation
        .begin_turn(
            turn_id(seed),
            message_id(seed * 10),
            Message {
                role: Role::User,
                content: vec![text(format!("pending:{seed}"))],
            },
        )
        .expect("begin pending transaction");
}

/// Captures state so a classified rejection can be proven read-only.
pub(super) fn snapshot(conversation: &Conversation) -> StateSnapshot {
    StateSnapshot {
        version: conversation.version(),
        head: conversation.head(),
        turns: conversation.turns().to_vec(),
        lineage_turns: conversation.lineage_turns().to_vec(),
        raw_turns: conversation.raw_turns().into_iter().cloned().collect(),
        pending: conversation.pending().map(|pending| {
            (
                pending.id(),
                pending.phase(),
                pending
                    .messages()
                    .iter()
                    .map(|message| message.id())
                    .collect(),
            )
        }),
        tool_call_index: conversation.tool_call_index().clone(),
        origin: conversation.origin(),
    }
}

/// Confirms the maintained lookup accelerator equals a fact-only rebuild.
pub(super) fn assert_index_matches_rebuild(conversation: &Conversation) {
    let rebuilt = ToolCallIndex::rebuild(conversation.turns(), conversation.pending());
    assert_eq!(conversation.tool_call_index(), &rebuilt);
}

/// Deserializes a structurally well-formed but untrusted Boundary token.
pub(super) fn forged_boundary(
    owner: ConversationId,
    turn_count: u64,
    after_turn: Option<TurnId>,
    version: u64,
) -> Boundary {
    serde_json::from_value(json!({
        "conversation_id": owner,
        "turn_count": turn_count,
        "after_turn": after_turn,
        "version": version,
    }))
    .expect("token has the public serde data shape")
}

/// Builds the exact shared-prefix history shape that M3-4 will wrap publicly.
pub(super) fn shared_prefix_child(
    parent: &Conversation,
    child_id: ConversationId,
    lineage_len: usize,
) -> Conversation {
    let boundary = parent
        .valid_boundaries()
        .get(lineage_len)
        .copied()
        .expect("fork ceiling belongs to parent lineage");
    parent
        .fork_at(boundary, child_id)
        .expect("shared-prefix child is a valid fork")
}
