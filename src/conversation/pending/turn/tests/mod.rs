use crate::{
    client::Response,
    conversation::{
        AssistantFinish, Conversation, ConversationConfig, ConversationId, ConversationMessage,
        MessageId, PendingToolCall, PendingTurnPhase, ToolCallId, ToolCallMapping, Turn, TurnId,
        TurnResponseMeta,
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
use serde_json::{Map, json};
use uuid::Uuid;

mod cancel;
mod errors;
mod review;
mod success;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_8f32_0000_0000_0000;

pub(super) fn conversation_id(seed: u128) -> ConversationId {
    ConversationId::new(Uuid::from_u128(UUID_BASE + seed))
}

pub(super) fn turn_id(seed: u128) -> TurnId {
    TurnId::new(Uuid::from_u128(UUID_BASE + seed))
}

pub(super) fn message_id(seed: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(UUID_BASE + seed))
}

pub(super) fn call_id(seed: u128) -> ToolCallId {
    ToolCallId::new(Uuid::from_u128(UUID_BASE + seed))
}

pub(super) fn conversation() -> Conversation {
    Conversation::new(
        conversation_id(1),
        ConversationConfig::new(Some("Answer precisely.".to_owned())),
    )
}

pub(super) fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_owned(),
        extra: Map::new(),
    }
}

pub(super) fn tool_use(provider_call_id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: provider_call_id.to_owned(),
        name: "lookup".to_owned(),
        input: json!({ "query": provider_call_id }),
        extra: Map::new(),
    }
}

pub(super) fn user(value: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![text(value)],
    }
}

pub(super) fn assistant_response(
    content: Vec<ContentBlock>,
    input: u32,
    output: u32,
    stop_reason: StopReason,
    request: &str,
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
        stop_reason: normalized_stop(stop_reason),
        extra: Map::from_iter([("request_id".to_owned(), json!(request))]),
    }
}

pub(super) fn tool_response(provider_call_id: &str, value: &str) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_call_id.to_owned(),
        content: vec![text(value)],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

pub(super) fn mapping(provider_call_id: &str, seed: u128) -> ToolCallMapping {
    ToolCallMapping::new(provider_call_id, call_id(seed))
}

pub(super) fn begin(conversation: &mut Conversation, turn_seed: u128, user_seed: u128) {
    conversation
        .begin_turn(turn_id(turn_seed), message_id(user_seed), user("question"))
        .expect("begin pending turn");
}

pub(super) fn freeze_response(
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

pub(super) fn push_streamed_tool_response(
    conversation: &mut Conversation,
    provider_call_id: &str,
    input_tokens: u32,
    output_tokens: u32,
    request: &str,
) {
    let block_id = BlockId::new(format!("block-{provider_call_id}"));
    conversation
        .start_assistant()
        .expect("start streaming assistant");
    for event in [
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: block_id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "lookup".to_owned(),
                tool_call_id: provider_call_id.to_owned(),
            },
        },
        StreamEvent::BlockDelta {
            id: block_id.clone(),
            delta: Delta::Json(format!("{{\"query\":\"{provider_call_id}")),
        },
        StreamEvent::BlockDelta {
            id: block_id.clone(),
            delta: Delta::Json("\"}".to_owned()),
        },
        StreamEvent::BlockStop { id: block_id },
        StreamEvent::Usage(Usage {
            input: input_tokens,
            output: output_tokens,
            ..Usage::default()
        }),
        StreamEvent::ResponseMetadata {
            extra: Map::from_iter([("request_id".to_owned(), json!(request))]),
        },
        StreamEvent::MessageStop {
            stop_reason: normalized_stop(StopReason::ToolUse),
        },
    ] {
        conversation
            .push_assistant_event(event)
            .expect("push streamed response event");
    }
}

pub(super) fn normalized_stop(reason: StopReason) -> Normalized<StopReason> {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CommittedView {
    id: ConversationId,
    config: ConversationConfig,
    turns: Vec<Turn>,
    version: u64,
}

pub(super) fn committed_view(conversation: &Conversation) -> CommittedView {
    CommittedView {
        id: conversation.id(),
        config: conversation.config().clone(),
        turns: conversation.turns().to_vec(),
        version: conversation.version(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingView {
    id: TurnId,
    parent: Option<TurnId>,
    phase: PendingTurnPhase,
    messages: Vec<ConversationMessage>,
    tool_calls: Vec<PendingToolCall>,
    usage: Usage,
    responses: Vec<TurnResponseMeta>,
    unmapped: Vec<String>,
}

pub(super) fn pending_view(conversation: &Conversation) -> PendingView {
    let pending = conversation.pending().expect("pending turn");
    PendingView {
        id: pending.id(),
        parent: pending.parent(),
        phase: pending.phase(),
        messages: pending.messages().to_vec(),
        tool_calls: pending.tool_calls().to_vec(),
        usage: pending.usage().clone(),
        responses: pending.responses().to_vec(),
        unmapped: pending.unmapped_provider_call_ids().to_vec(),
    }
}
