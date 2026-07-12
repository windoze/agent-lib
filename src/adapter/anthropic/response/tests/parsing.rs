//! Complete-response conversion tests using real and adversarial wire bodies.

use super::*;
use crate::model::normalized::StopReason;
use serde_json::json;

/// Verifies a recorded text response maps tokens and preserves wire metadata.
#[test]
fn recorded_text_response_maps_to_complete_response() {
    let response = AnthropicAdapter::parse_response(REAL_TEXT_RESPONSE.as_bytes())
        .expect("parse recorded Anthropic text response");

    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(response.message.content.len(), 1);
    let ContentBlock::Text { text, extra } = &response.message.content[0] else {
        panic!("recorded response should contain a text block");
    };
    assert_eq!(text, "Hi there friend.");
    assert!(extra.is_empty());

    assert_eq!(response.stop_reason.value, StopReason::EndTurn);
    assert_eq!(response.stop_reason.raw.as_deref(), Some("end_turn"));
    assert_eq!(response.usage.input, 14);
    assert_eq!(response.usage.output, 7);
    assert_eq!(response.usage.cache_write, 0);
    assert_eq!(response.usage.cache_read, 0);
    assert_eq!(
        response.usage.extra.get("cache_creation"),
        Some(&json!({
            "ephemeral_5m_input_tokens": 0,
            "ephemeral_1h_input_tokens": 0
        }))
    );

    assert_eq!(
        response.extra.get("model"),
        Some(&json!("claude-haiku-4-5-20251001"))
    );
    assert_eq!(response.extra.get("type"), Some(&json!("message")));
    assert_eq!(response.extra.get("stop_sequence"), Some(&json!(null)));
    assert!(response.extra.contains_key("id"));
    assert!(!response.extra.contains_key("role"));
}

/// Verifies the recorded tool response retains ids, parsed input, and raw stop reason.
#[test]
fn recorded_tool_response_maps_tool_use_and_usage() {
    let response = AnthropicAdapter::parse_response(REAL_TOOL_RESPONSE.as_bytes())
        .expect("parse recorded Anthropic tool response");

    assert_eq!(response.message.content.len(), 1);
    let ContentBlock::ToolUse {
        id,
        name,
        input,
        extra,
    } = &response.message.content[0]
    else {
        panic!("recorded response should contain a tool-use block");
    };
    assert_eq!(id, "toolu_bdrk_recorded_weather");
    assert_eq!(name, "get_weather");
    assert_eq!(input, &json!({ "city": "Tokyo" }));
    assert!(extra.is_empty());
    assert_eq!(response.stop_reason.value, StopReason::ToolUse);
    assert_eq!(response.stop_reason.raw.as_deref(), Some("tool_use"));
    assert_eq!(response.usage.input, 571);
    assert_eq!(response.usage.output, 54);
}

/// Exercises thinking conversion and every response-side escape-hatch level.
#[test]
fn thinking_unknown_stop_and_provider_fields_are_preserved() {
    let body = json!({
        "id": "msg_extension_test",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "thinking",
                "thinking": "I should verify the answer.",
                "signature": "sig-123",
                "provider_note": { "verified": true }
            },
            {
                "type": "text",
                "text": "Verified.",
                "citations": [{ "kind": "provider_citation", "offset": 0 }]
            }
        ],
        "stop_reason": "future_provider_reason",
        "usage": {
            "input_tokens": 29,
            "output_tokens": 11,
            "cache_creation_input_tokens": 7,
            "cache_read_input_tokens": 5,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 2,
                "ephemeral_1h_input_tokens": 5
            },
            "service_tier": "standard"
        },
        "provider_trace": { "region": "test" }
    });
    let body = serde_json::to_vec(&body).expect("serialize response fixture");

    let response =
        AnthropicAdapter::parse_response(&body).expect("parse response with provider fields");

    let ContentBlock::Thinking {
        text,
        signature,
        extra,
    } = &response.message.content[0]
    else {
        panic!("first block should be thinking");
    };
    assert_eq!(text, "I should verify the answer.");
    assert_eq!(signature.as_deref(), Some("sig-123"));
    assert_eq!(
        extra.get("provider_note"),
        Some(&json!({ "verified": true }))
    );

    let ContentBlock::Text { extra, .. } = &response.message.content[1] else {
        panic!("second block should be text");
    };
    assert_eq!(
        extra.get("citations"),
        Some(&json!([{ "kind": "provider_citation", "offset": 0 }]))
    );

    assert_eq!(response.stop_reason.value, StopReason::Other);
    assert_eq!(
        response.stop_reason.raw.as_deref(),
        Some("future_provider_reason")
    );
    assert_eq!(response.usage.input, 29);
    assert_eq!(response.usage.output, 11);
    assert_eq!(response.usage.cache_write, 7);
    assert_eq!(response.usage.cache_read, 5);
    assert_eq!(
        response.usage.extra.get("cache_creation"),
        Some(&json!({
            "ephemeral_5m_input_tokens": 2,
            "ephemeral_1h_input_tokens": 5
        }))
    );
    assert_eq!(
        response.usage.extra.get("service_tier"),
        Some(&json!("standard"))
    );
    assert_eq!(
        response.extra.get("provider_trace"),
        Some(&json!({ "region": "test" }))
    );
}

/// Ensures malformed JSON, invalid roles, and conflicting usage fail observably.
#[test]
fn malformed_wire_data_returns_protocol_errors() {
    let malformed = AnthropicAdapter::parse_response(br#"{"role":"assistant""#)
        .expect_err("malformed JSON should fail");
    assert!(matches!(malformed, ClientError::Protocol(_)));
    assert!(malformed.to_string().contains("response JSON"));

    let wrong_role = serde_json::to_vec(&json!({
        "role": "user",
        "content": [],
        "stop_reason": "end_turn",
        "usage": {}
    }))
    .expect("serialize wrong-role fixture");
    let wrong_role = AnthropicAdapter::parse_response(&wrong_role)
        .expect_err("non-assistant response role should fail");
    assert!(matches!(wrong_role, ClientError::Protocol(_)));

    let conflicting_usage = serde_json::to_vec(&json!({
        "role": "assistant",
        "content": [],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 3,
            "input": 4
        }
    }))
    .expect("serialize conflicting usage fixture");
    let conflicting_usage = AnthropicAdapter::parse_response(&conflicting_usage)
        .expect_err("conflicting usage aliases should fail");
    assert!(matches!(conflicting_usage, ClientError::Protocol(_)));
    assert!(
        conflicting_usage
            .to_string()
            .contains("conflicting usage fields")
    );
}
