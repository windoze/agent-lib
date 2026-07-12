use super::{
    ToolPairing, ToolPairingData, Turn, TurnCompletion, TurnData, TurnMeta, TurnResponseMeta,
};
use crate::{
    conversation::{ConversationMessage, MessageId, ToolCallId, TurnId},
    model::{
        content::ContentBlock,
        message::Message,
        message::Role,
        normalized::{Normalized, StopReason},
        tool::ToolStatus,
        usage::Usage,
    },
};
use serde_json::{Map, Value, json};

const PARENT_ID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890ab";
const TURN_ID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890ac";
const USER_MSG_ID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890ad";
const CALL_MSG_ID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890ae";
const RESULT_MSG_ID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890af";
const FINAL_MSG_ID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890b0";
const FIRST_CALL_ID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890b1";
const SECOND_CALL_ID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890b2";

/// Builds a complete message fixture under an externally supplied id.
fn message(id: &str, role: Role, content: Vec<ContentBlock>) -> ConversationMessage {
    ConversationMessage::new(
        id.parse::<MessageId>().expect("message id"),
        Message { role, content },
    )
}

/// Builds caller-supplied metadata with normalized usage and extension data.
fn turn_meta() -> TurnMeta {
    let mut meta = TurnMeta::new(
        Usage {
            input: 40,
            output: 12,
            cache_read: 8,
            cache_write: 2,
            reasoning: 3,
            total: Some(65),
            extra: Map::from_iter([("provider_usage".to_owned(), json!("retained"))]),
        },
        Some("2026-07-13T04:30:00Z".to_owned()),
        Some("integration-test".to_owned()),
        Map::from_iter([
            (
                "messages".to_owned(),
                json!("metadata cannot override history"),
            ),
            ("trace_id".to_owned(), json!("trace-123")),
        ]),
    );
    meta.merge_pending(
        Usage::default(),
        &[
            TurnResponseMeta::new(
                CALL_MSG_ID.parse().expect("call message id"),
                Normalized::from_mapped(StopReason::ToolUse, "tool_use"),
                Map::from_iter([("request_id".to_owned(), json!("req-call"))]),
            ),
            TurnResponseMeta::new(
                FINAL_MSG_ID.parse().expect("final message id"),
                Normalized::from_mapped(StopReason::EndTurn, "end_turn"),
                Map::from_iter([("request_id".to_owned(), json!("req-final"))]),
            ),
        ],
    );
    meta
}

/// Builds a closed turn with two parallel calls answered by one tool message.
fn closed_turn() -> Turn {
    let call_message = CALL_MSG_ID.parse::<MessageId>().expect("call message id");
    let result_message = RESULT_MSG_ID
        .parse::<MessageId>()
        .expect("result message id");
    let parent = PARENT_ID.parse::<TurnId>().expect("parent turn id");

    let data = TurnData {
        id: TURN_ID.parse::<TurnId>().expect("turn id"),
        messages: vec![
            message(
                USER_MSG_ID,
                Role::User,
                vec![ContentBlock::Text {
                    text: "Compare Shanghai and Tokyo weather.".to_owned(),
                    extra: Map::new(),
                }],
            ),
            message(
                CALL_MSG_ID,
                Role::Assistant,
                vec![
                    ContentBlock::ToolUse {
                        id: "provider-shanghai".to_owned(),
                        name: "get_weather".to_owned(),
                        input: json!({ "city": "Shanghai" }),
                        extra: Map::new(),
                    },
                    ContentBlock::ToolUse {
                        id: "provider-tokyo".to_owned(),
                        name: "get_weather".to_owned(),
                        input: json!({ "city": "Tokyo" }),
                        extra: Map::new(),
                    },
                ],
            ),
            message(
                RESULT_MSG_ID,
                Role::Tool,
                vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "provider-shanghai".to_owned(),
                        content: vec![ContentBlock::Text {
                            text: "Sunny".to_owned(),
                            extra: Map::new(),
                        }],
                        status: ToolStatus::Ok,
                        extra: Map::new(),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "provider-tokyo".to_owned(),
                        content: vec![ContentBlock::Text {
                            text: "Cloudy".to_owned(),
                            extra: Map::new(),
                        }],
                        status: ToolStatus::Ok,
                        extra: Map::new(),
                    },
                ],
            ),
            message(
                FINAL_MSG_ID,
                Role::Assistant,
                vec![ContentBlock::Text {
                    text: "Shanghai is sunny; Tokyo is cloudy.".to_owned(),
                    extra: Map::new(),
                }],
            ),
        ],
        pairings: vec![
            ToolPairingData {
                call_id: FIRST_CALL_ID.parse::<ToolCallId>().expect("tool call id"),
                provider_call_id: Some("provider-shanghai".to_owned()),
                call_msg: call_message,
                result_msg: Some(result_message),
            },
            ToolPairingData {
                call_id: SECOND_CALL_ID.parse::<ToolCallId>().expect("tool call id"),
                provider_call_id: Some("provider-tokyo".to_owned()),
                call_msg: call_message,
                result_msg: Some(result_message),
            },
        ],
        parent: Some(parent),
        meta: turn_meta(),
        completion: TurnCompletion::Complete,
    };

    crate::conversation::validation::validate_turn_data(data, &[], Some(parent))
        .expect("fixture must pass the sole closed-turn validator")
}

#[test]
/// Verifies every required closed-turn field through its read-only API.
fn closed_turn_has_ordered_messages_parallel_pairings_parent_and_meta() {
    let turn = closed_turn();

    assert_eq!(turn.id().to_string(), TURN_ID);
    assert_eq!(turn.parent().expect("parent").to_string(), PARENT_ID);
    assert_eq!(turn.messages().len(), 4);
    assert_eq!(
        turn.messages()
            .iter()
            .map(|message| message.payload().role)
            .collect::<Vec<_>>(),
        vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant]
    );
    assert_eq!(turn.pairings().len(), 2);
    assert_eq!(
        turn.pairings()[0].provider_call_id(),
        Some("provider-shanghai")
    );
    assert_eq!(
        turn.pairings()[1].provider_call_id(),
        Some("provider-tokyo")
    );
    assert_eq!(turn.pairings()[0].call_msg().to_string(), CALL_MSG_ID);
    assert_eq!(turn.pairings()[0].result_msg().to_string(), RESULT_MSG_ID);
    assert_eq!(turn.meta().usage().total, Some(65));
    assert_eq!(turn.meta().timestamp(), Some("2026-07-13T04:30:00Z"));
    assert_eq!(turn.meta().source(), Some("integration-test"));
    assert_eq!(turn.meta().responses().len(), 2);
    assert_eq!(
        turn.meta().responses()[0].message_id().to_string(),
        CALL_MSG_ID
    );
    assert_eq!(
        turn.meta().responses()[1].stop_reason().value,
        StopReason::EndTurn
    );
}

#[test]
/// Verifies live serialization and untrusted DTO round-trip use one stable shape.
fn closed_turn_uses_a_stable_dto_shape_that_round_trips_as_data() {
    let turn = closed_turn();
    let encoded = serde_json::to_value(&turn).expect("serialize closed turn");

    assert_eq!(encoded["id"], json!(TURN_ID));
    assert_eq!(encoded["parent"], json!(PARENT_ID));
    assert_eq!(encoded["messages"].as_array().map(Vec::len), Some(4));
    assert_eq!(encoded["pairings"].as_array().map(Vec::len), Some(2));
    assert_eq!(encoded["pairings"][0]["call_id"], json!(FIRST_CALL_ID));
    assert_eq!(encoded["pairings"][0]["result_msg"], json!(RESULT_MSG_ID));
    assert_eq!(encoded["meta"]["usage"]["input"], json!(40));
    assert_eq!(
        encoded["meta"]["responses"][0]["message_id"],
        json!(CALL_MSG_ID)
    );
    assert_eq!(
        encoded["meta"]["responses"][1]["extra"]["request_id"],
        json!("req-final")
    );
    assert_eq!(
        encoded["meta"]["usage"]["provider_usage"],
        json!("retained")
    );
    assert_eq!(
        encoded["meta"]["extra"]["messages"],
        json!("metadata cannot override history")
    );

    let decoded = serde_json::from_value::<TurnData>(encoded.clone())
        .expect("deserialize untrusted turn data");
    assert_eq!(decoded, TurnData::from(&turn));
    assert_eq!(
        serde_json::to_value(decoded).expect("re-serialize turn data"),
        encoded
    );
}

#[test]
/// Verifies cloned turns share storage and reads preserve frozen message data.
fn cloning_a_turn_shares_storage_and_reading_preserves_message_identity_and_payload() {
    let turn = closed_turn();
    let cloned = turn.clone();
    let before = serde_json::to_value(turn.messages()).expect("serialize messages before reads");
    let ids_before = turn
        .messages()
        .iter()
        .map(ConversationMessage::id)
        .collect::<Vec<_>>();

    assert!(std::sync::Arc::ptr_eq(&turn.messages, &cloned.messages));
    assert!(std::sync::Arc::ptr_eq(&turn.pairings, &cloned.pairings));
    let _ = turn
        .messages()
        .iter()
        .map(ConversationMessage::payload)
        .count();
    let _ = turn.meta().extra().get("trace_id");

    assert_eq!(
        turn.messages()
            .iter()
            .map(ConversationMessage::id)
            .collect::<Vec<_>>(),
        ids_before
    );
    assert_eq!(
        serde_json::to_value(turn.messages()).expect("serialize messages after reads"),
        before
    );
    assert_eq!(cloned.messages(), turn.messages());
}

#[test]
/// Verifies a public closed pairing cannot deserialize without its result id.
fn closed_pairing_serde_requires_a_result_message() {
    let pairing = closed_turn().pairings()[0].clone();
    let encoded = serde_json::to_value(&pairing).expect("serialize pairing");
    let decoded = serde_json::from_value::<ToolPairing>(encoded.clone())
        .expect("deserialize complete pairing");

    assert_eq!(decoded, pairing);
    assert!(encoded["result_msg"].is_string());

    let mut missing = encoded.as_object().expect("pairing object").clone();
    missing.remove("result_msg");
    assert!(serde_json::from_value::<ToolPairing>(Value::Object(missing)).is_err());

    let mut null = encoded.as_object().expect("pairing object").clone();
    null.insert("result_msg".to_owned(), Value::Null);
    assert!(serde_json::from_value::<ToolPairing>(Value::Object(null)).is_err());

    fn requires_message_id(_: MessageId) {}
    requires_message_id(pairing.result_msg());
}

#[test]
/// Verifies only the internal DTO can retain a temporarily dangling pairing.
fn crate_private_dto_can_hold_a_pending_pairing_without_weakening_closed_view() {
    let turn = closed_turn();
    let mut draft = TurnData::from(&turn);
    draft.pairings[0] = ToolPairingData {
        result_msg: None,
        ..draft.pairings[0].clone()
    };

    let encoded = serde_json::to_value(&draft).expect("serialize pending turn data");
    assert_eq!(encoded["pairings"][0]["result_msg"], Value::Null);

    let decoded = serde_json::from_value::<TurnData>(encoded).expect("deserialize draft data");
    assert_eq!(decoded.pairings[0].result_msg, None);
    let closed_result: MessageId = turn.pairings()[0].result_msg();
    assert_eq!(closed_result.to_string(), RESULT_MSG_ID);
}

#[test]
/// Verifies metadata extensions stay nested and cannot shadow fixed fields.
fn turn_meta_round_trips_without_flattening_extensions_over_fixed_fields() {
    let meta = turn_meta();
    let encoded = serde_json::to_value(&meta).expect("serialize turn metadata");

    assert_eq!(encoded["timestamp"], json!("2026-07-13T04:30:00Z"));
    assert_eq!(encoded["source"], json!("integration-test"));
    assert_eq!(
        encoded["extra"]["messages"],
        json!("metadata cannot override history")
    );
    assert!(encoded.get("messages").is_none());

    let decoded = serde_json::from_value::<TurnMeta>(encoded).expect("deserialize turn metadata");
    assert_eq!(decoded, meta);
    assert_eq!(decoded.extra().get("trace_id"), Some(&json!("trace-123")));
}
