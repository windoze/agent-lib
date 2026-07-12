//! Focused tests for tool-result status serialization and legacy migration.

use super::ContentBlock;
use crate::model::tool::ToolStatus;
use serde_json::{Map, Value, json};

/// Builds one normalized result with caller-selected status and extensions.
fn tool_result(status: ToolStatus, extra: Map<String, Value>) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: "call_1".to_owned(),
        content: vec![ContentBlock::Text {
            text: "result".to_owned(),
            extra: Map::new(),
        }],
        status,
        extra,
    }
}

#[test]
fn normalized_tool_result_round_trips_each_status_without_legacy_flag() {
    for (status, wire_status) in [
        (ToolStatus::Ok, "ok"),
        (ToolStatus::Error, "error"),
        (ToolStatus::Denied, "denied"),
        (ToolStatus::Cancelled, "cancelled"),
    ] {
        let block = tool_result(
            status,
            Map::from_iter([("provider_trace".to_owned(), json!("trace-1"))]),
        );
        let encoded = serde_json::to_value(&block).expect("serialize normalized tool result");

        assert_eq!(encoded["status"], json!(wire_status));
        assert_eq!(encoded["provider_trace"], json!("trace-1"));
        assert!(encoded.get("is_error").is_none());
        assert_eq!(
            serde_json::from_value::<ContentBlock>(encoded)
                .expect("deserialize normalized tool result"),
            block
        );
    }
}

#[test]
fn legacy_tool_result_flags_and_omitted_success_migrate_to_status() {
    for (legacy_field, expected) in [
        (Some(false), ToolStatus::Ok),
        (Some(true), ToolStatus::Error),
        (None, ToolStatus::Ok),
    ] {
        let mut value = json!({
            "type": "tool_result",
            "tool_use_id": "call_1",
            "content": [],
            "provider_trace": "legacy"
        });
        if let Some(is_error) = legacy_field {
            value["is_error"] = json!(is_error);
        }

        let block: ContentBlock =
            serde_json::from_value(value).expect("migrate historical tool result");
        let ContentBlock::ToolResult { status, extra, .. } = &block else {
            panic!("expected tool-result block");
        };
        assert_eq!(*status, expected);
        assert_eq!(extra.get("provider_trace"), Some(&json!("legacy")));

        let normalized = serde_json::to_value(block).expect("serialize migrated result");
        assert_eq!(
            normalized["status"],
            serde_json::to_value(expected).unwrap()
        );
        assert!(normalized.get("is_error").is_none());
    }
}

#[test]
fn equivalent_new_and_legacy_status_fields_are_accepted_then_canonicalized() {
    for (status, is_error) in [("ok", false), ("error", true)] {
        let block: ContentBlock = serde_json::from_value(json!({
            "type": "tool_result",
            "tool_use_id": "call_1",
            "status": status,
            "is_error": is_error
        }))
        .expect("equivalent migration fields should be accepted");
        let normalized = serde_json::to_value(block).expect("serialize canonical result");

        assert_eq!(normalized["status"], json!(status));
        assert!(normalized.get("is_error").is_none());
    }
}

#[test]
fn conflicting_new_and_legacy_status_fields_are_rejected() {
    for (status, is_error) in [
        ("ok", true),
        ("error", false),
        ("denied", true),
        ("cancelled", true),
    ] {
        let error = serde_json::from_value::<ContentBlock>(json!({
            "type": "tool_result",
            "tool_use_id": "call_1",
            "status": status,
            "is_error": is_error
        }))
        .expect_err("contradictory migration fields must be rejected");

        assert!(error.to_string().contains("conflicting `status`"));
    }
}

#[test]
fn present_but_invalid_status_fields_do_not_acquire_legacy_defaults() {
    for invalid_fields in [
        json!({ "status": null }),
        json!({ "is_error": null }),
        json!({ "is_error": "true" }),
        json!({ "status": "unknown" }),
    ] {
        let mut value = json!({
            "type": "tool_result",
            "tool_use_id": "call_1"
        });
        value
            .as_object_mut()
            .expect("fixture is an object")
            .extend(invalid_fields.as_object().unwrap().clone());

        serde_json::from_value::<ContentBlock>(value)
            .expect_err("malformed present migration field must be rejected");
    }
}

#[test]
fn modeled_and_legacy_keys_in_extra_cannot_override_normalized_fields() {
    let block = tool_result(
        ToolStatus::Cancelled,
        Map::from_iter([
            ("type".to_owned(), json!("text")),
            ("tool_use_id".to_owned(), json!("wrong_call")),
            ("content".to_owned(), json!([])),
            ("status".to_owned(), json!("ok")),
            ("is_error".to_owned(), json!(false)),
            ("provider_trace".to_owned(), json!("kept")),
        ]),
    );
    let encoded = serde_json::to_value(block).expect("serialize authoritative fields");

    assert_eq!(encoded["type"], json!("tool_result"));
    assert_eq!(encoded["tool_use_id"], json!("call_1"));
    assert_eq!(encoded["content"].as_array().unwrap().len(), 1);
    assert_eq!(encoded["status"], json!("cancelled"));
    assert_eq!(encoded["provider_trace"], json!("kept"));
    assert!(encoded.get("is_error").is_none());
}
