//! Fixtures shared by Boundary signing and rejection tests.

use super::super::Boundary;
use crate::{
    client::Response,
    conversation::{
        AssistantFinish, Conversation, ConversationConfig, ConversationId, MessageId,
        PendingTurnPhase, ToolCallIndex, Turn, TurnId, TurnMeta,
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

mod negative;
mod positive;
mod serde;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_8f50_0000_0000_0000;

/// Captures every mutable Conversation component relevant to Boundary errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StateSnapshot {
    version: u64,
    turns: Vec<Turn>,
    pending: Option<(TurnId, PendingTurnPhase, Vec<MessageId>)>,
    tool_call_index: ToolCallIndex,
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
        turns: conversation.turns().to_vec(),
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
    }
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
    let history = parent
        .history
        .shared_prefix(lineage_len)
        .expect("fork ceiling belongs to parent lineage");
    let tool_call_index = ToolCallIndex::rebuild(history.turns(), None);
    Conversation {
        id: child_id,
        config: parent.config.clone(),
        history,
        pending: None,
        tool_call_index,
        version: 0,
    }
}
