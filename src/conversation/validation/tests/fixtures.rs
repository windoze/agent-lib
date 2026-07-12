use crate::{
    conversation::{
        Conversation, ConversationConfig, ConversationId, ConversationMessage, MessageId,
        ToolCallId, Turn, TurnId, TurnMeta,
        turn::{ToolPairingData, TurnCompletion, TurnData},
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        tool::ToolStatus,
    },
};
use serde_json::{Map, json};
use std::collections::HashSet;
use uuid::Uuid;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_8f31_0000_0000_0000;

/// Creates a deterministic caller-supplied Conversation id.
pub(super) fn conversation_id(seed: u128) -> ConversationId {
    ConversationId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates a deterministic caller-supplied Turn id.
pub(super) fn turn_id(seed: u128) -> TurnId {
    TurnId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates a deterministic caller-supplied Message id.
pub(super) fn message_id(seed: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates a deterministic caller-supplied framework ToolCall id.
pub(super) fn tool_call_id(seed: u128) -> ToolCallId {
    ToolCallId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates an empty conversation without generating identity internally.
pub(super) fn conversation() -> Conversation {
    Conversation::new(
        conversation_id(1),
        ConversationConfig::new(Some("Answer precisely.".to_owned())),
    )
}

/// Freezes a complete test message under an externally supplied id.
pub(super) fn message(seed: u128, role: Role, content: Vec<ContentBlock>) -> ConversationMessage {
    ConversationMessage::new(message_id(seed), Message { role, content })
}

/// Builds one text block with no provider extensions.
pub(super) fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_owned(),
        extra: Map::new(),
    }
}

/// Builds one image block accepted by both request adapters for user/tool input.
pub(super) fn image() -> ContentBlock {
    ContentBlock::Image {
        source: crate::model::content::ImageSource::Url {
            url: "https://example.test/image.png".to_owned(),
            extra: Map::new(),
        },
        extra: Map::new(),
    }
}

/// Builds one complete provider call block.
pub(super) fn tool_use(provider_call_id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: provider_call_id.to_owned(),
        name: "lookup".to_owned(),
        input: json!({ "query": provider_call_id }),
        extra: Map::new(),
    }
}

/// Builds one complete provider result block.
pub(super) fn tool_result(provider_call_id: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: provider_call_id.to_owned(),
        content: vec![text(&format!("result for {provider_call_id}"))],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

/// Builds an explicit framework/provider pairing for one complete call.
pub(super) fn pairing(
    call_seed: u128,
    provider_call_id: &str,
    call_message_seed: u128,
    result_message_seed: u128,
) -> ToolPairingData {
    ToolPairingData {
        call_id: tool_call_id(call_seed),
        provider_call_id: Some(provider_call_id.to_owned()),
        call_msg: message_id(call_message_seed),
        result_msg: Some(message_id(result_message_seed)),
    }
}

/// Builds complete candidate data without exposing a live Turn constructor.
pub(super) fn draft(
    turn_seed: u128,
    parent: Option<TurnId>,
    messages: Vec<ConversationMessage>,
    pairings: Vec<ToolPairingData>,
) -> TurnData {
    TurnData {
        id: turn_id(turn_seed),
        messages,
        pairings,
        parent,
        meta: TurnMeta::default(),
        completion: TurnCompletion::Complete,
    }
}

/// Builds a minimal complete text turn.
pub(super) fn text_draft(turn_seed: u128, parent: Option<TurnId>, message_seed: u128) -> TurnData {
    draft(
        turn_seed,
        parent,
        vec![
            message(message_seed, Role::User, vec![text("question")]),
            message(message_seed + 1, Role::Assistant, vec![text("answer")]),
        ],
        Vec::new(),
    )
}

/// Builds a complete single-call turn used as a mutation baseline.
pub(super) fn single_tool_draft(
    turn_seed: u128,
    parent: Option<TurnId>,
    message_seed: u128,
    call_seed: u128,
    provider_call_id: &str,
) -> TurnData {
    draft(
        turn_seed,
        parent,
        vec![
            message(message_seed, Role::User, vec![text("question")]),
            message(
                message_seed + 1,
                Role::Assistant,
                vec![text("checking"), tool_use(provider_call_id)],
            ),
            message(
                message_seed + 2,
                Role::Tool,
                vec![tool_result(provider_call_id)],
            ),
            message(message_seed + 3, Role::Assistant, vec![text("final")]),
        ],
        vec![pairing(
            call_seed,
            provider_call_id,
            message_seed + 1,
            message_seed + 2,
        )],
    )
}

/// Re-checks I1--I4 from a live Turn's read-only surface.
pub(super) fn assert_closed_invariants(turn: &Turn) {
    let message_ids = turn
        .messages()
        .iter()
        .map(ConversationMessage::id)
        .collect::<HashSet<_>>();
    assert_eq!(message_ids.len(), turn.messages().len(), "I4 message ids");
    assert_eq!(
        turn.messages()
            .first()
            .map(|message| message.payload().role),
        Some(Role::User),
        "I2 start"
    );
    assert_eq!(
        turn.messages().last().map(|message| message.payload().role),
        Some(Role::Assistant),
        "I2 end"
    );
    assert!(
        turn.messages()
            .iter()
            .all(|message| message.payload().role != Role::System),
        "I2 no system role"
    );

    let mut provider_calls = HashSet::new();
    let mut provider_results = HashSet::new();
    for message in turn.messages() {
        for block in &message.payload().content {
            match block {
                ContentBlock::ToolUse { id, input, .. } => {
                    assert_eq!(message.payload().role, Role::Assistant, "I2 call role");
                    serde_json::to_string(input).expect("I3 stores complete parsed JSON values");
                    assert!(provider_calls.insert(id.as_str()), "I1 unique call");
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    assert_eq!(message.payload().role, Role::Tool, "I2 result role");
                    assert!(
                        provider_results.insert(tool_use_id.as_str()),
                        "I1 one result"
                    );
                }
                _ => {}
            }
        }
    }
    assert_eq!(provider_calls, provider_results, "I1 calls equal results");

    let framework_calls = turn
        .pairings()
        .iter()
        .map(crate::conversation::ToolPairing::call_id)
        .collect::<HashSet<_>>();
    assert_eq!(framework_calls.len(), turn.pairings().len(), "I4 call ids");
    assert_eq!(
        turn.pairings().len(),
        provider_calls.len(),
        "I1 pairing count"
    );
    for pairing in turn.pairings() {
        assert!(message_ids.contains(&pairing.call_msg()), "I1 call message");
        assert!(
            message_ids.contains(&pairing.result_msg()),
            "I1 result message"
        );
        if let Some(provider_call_id) = pairing.provider_call_id() {
            assert!(
                provider_calls.contains(provider_call_id),
                "I1 provider call"
            );
            assert!(
                provider_results.contains(provider_call_id),
                "I1 provider result"
            );
        } else {
            let call = turn
                .messages()
                .iter()
                .find(|message| message.id() == pairing.call_msg())
                .expect("I1 call anchor");
            let result = turn
                .messages()
                .iter()
                .find(|message| message.id() == pairing.result_msg())
                .expect("I1 result anchor");
            let candidates = call
                .payload()
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, .. } => Some(id),
                    _ => None,
                })
                .filter(|id| {
                    result.payload().content.iter().any(|block| {
                        matches!(
                            block,
                            ContentBlock::ToolResult { tool_use_id, .. }
                                if tool_use_id == *id
                        )
                    })
                })
                .count();
            assert_eq!(candidates, 1, "I1 optional provider id is unambiguous");
        }
    }

    let encoded = serde_json::to_value(turn).expect("serialize certified turn");
    assert!(encoded.get("completion").is_none(), "I3 no pending marker");
}
