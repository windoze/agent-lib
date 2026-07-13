//! Focused M3-1 fixtures for structural history and derived indexing.

use super::{ToolCallIndex, ToolCallLocationKind};
use crate::{
    client::Response,
    conversation::{
        AssistantFinish, Conversation, ConversationConfig, ConversationId, MessageId, ToolCallId,
        ToolCallMapping, TurnId, TurnMeta,
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

mod index;
mod retention;
mod sharing;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_8f40_0000_0000_0000;

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

/// Creates one deterministic external framework call identity.
pub(super) fn call_id(seed: u128) -> ToolCallId {
    ToolCallId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates an empty test Conversation without internal identity generation.
pub(super) fn conversation() -> Conversation {
    Conversation::new(
        conversation_id(1),
        ConversationConfig::new(Some("Answer exactly.".to_owned())),
    )
}

/// Creates one provider-neutral text block.
pub(super) fn text(value: impl Into<String>) -> ContentBlock {
    ContentBlock::Text {
        text: value.into(),
        extra: Map::new(),
    }
}

/// Creates one complete user payload.
pub(super) fn user(value: impl Into<String>) -> Message {
    Message {
        role: Role::User,
        content: vec![text(value)],
    }
}

/// Creates one complete assistant response for pending freeze.
pub(super) fn response(content: Vec<ContentBlock>, stop_reason: StopReason) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content,
        },
        usage: Usage::default(),
        stop_reason: StopReason::normalize(match stop_reason {
            StopReason::ToolUse => "tool_use",
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::StopSequence => "stop_sequence",
            StopReason::Refusal => "refusal",
            StopReason::Other => "other",
        }),
        extra: Map::new(),
    }
}

/// Creates one complete assistant tool-use block.
pub(super) fn tool_use(provider_call_id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: provider_call_id.to_owned(),
        name: "lookup".to_owned(),
        input: json!({ "query": provider_call_id }),
        extra: Map::new(),
    }
}

/// Creates one complete successful tool response.
pub(super) fn tool_response(provider_call_id: &str) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_call_id.to_owned(),
        content: vec![text(format!("result:{provider_call_id}"))],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

/// Begins a pending turn with deterministic external identities.
pub(super) fn begin(conversation: &mut Conversation, turn_seed: u128, user_message_seed: u128) {
    conversation
        .begin_turn(
            turn_id(turn_seed),
            message_id(user_message_seed),
            user(format!("question:{turn_seed}")),
        )
        .expect("begin pending turn");
}

/// Freezes one complete response and returns its state-machine outcome.
pub(super) fn freeze(
    conversation: &mut Conversation,
    response: Response,
    message_seed: u128,
) -> AssistantFinish {
    conversation
        .start_assistant_response(response)
        .expect("start complete response");
    conversation
        .finish_assistant(message_id(message_seed))
        .expect("freeze complete response")
}

/// Commits one minimal text-only turn.
pub(super) fn commit_text_turn(
    conversation: &mut Conversation,
    turn_seed: u128,
    user_message_seed: u128,
) {
    begin(conversation, turn_seed, user_message_seed);
    assert_eq!(
        freeze(
            conversation,
            response(
                vec![text(format!("answer:{turn_seed}"))],
                StopReason::EndTurn
            ),
            user_message_seed + 1,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit text turn");
}

/// Freezes one tool-use batch and registers its framework identities.
pub(super) fn register_batch(
    conversation: &mut Conversation,
    calls: &[(&str, u128)],
    assistant_message_seed: u128,
) {
    assert_eq!(
        freeze(
            conversation,
            response(
                calls
                    .iter()
                    .map(|(provider_call_id, _)| tool_use(provider_call_id))
                    .collect(),
                StopReason::ToolUse,
            ),
            assistant_message_seed,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    conversation
        .register_tool_calls(
            calls
                .iter()
                .rev()
                .map(|(provider_call_id, call_seed)| {
                    ToolCallMapping::new(*provider_call_id, call_id(*call_seed))
                })
                .collect(),
        )
        .expect("register exact tool mappings");
}

/// Compares the incrementally maintained index with a fresh fact rebuild.
pub(super) fn assert_index_matches_rebuild(conversation: &Conversation) {
    let rebuilt = ToolCallIndex::rebuild(conversation.turns(), conversation.pending());
    assert_eq!(conversation.tool_call_index(), &rebuilt);
}
