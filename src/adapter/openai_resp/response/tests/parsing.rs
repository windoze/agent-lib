//! Fixture-driven and adversarial complete-response conversion tests.

use super::*;
use crate::model::normalized::StopReason;
use serde_json::json;

#[test]
fn recorded_text_response_maps_items_usage_and_azure_metadata() {
    let response = OpenAiRespAdapter::parse_response(REAL_TEXT_RESPONSE.as_bytes())
        .expect("parse recorded OpenAI text response");

    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(response.message.content.len(), 2);
    let ContentBlock::Thinking {
        text,
        signature,
        extra,
    } = &response.message.content[0]
    else {
        panic!("first output item should become a reasoning block");
    };
    assert!(text.is_empty());
    assert_eq!(signature, &None);
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["item"]["id"],
        json!("rs_recorded_text")
    );
    assert_eq!(extra[RESPONSE_EXTRA_KEY]["item"]["summary"], json!([]));

    let ContentBlock::Text { text, extra } = &response.message.content[1] else {
        panic!("second output item should become a text block");
    };
    assert_eq!(text, "Hi there");
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["item"]["id"],
        json!("msg_recorded_text")
    );
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["item"]["phase"],
        json!("final_answer")
    );
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["content"]["annotations"],
        json!([])
    );
    assert_eq!(extra[RESPONSE_EXTRA_KEY]["content"]["logprobs"], json!([]));

    assert_eq!(*response.stop_reason.value(), StopReason::EndTurn);
    assert_eq!(response.stop_reason.raw(), Some("completed"));
    assert_eq!(response.usage.input, 13);
    assert_eq!(response.usage.output, 26);
    assert_eq!(response.usage.cache_read, 4);
    assert_eq!(response.usage.reasoning, 18);
    assert_eq!(response.usage.total, Some(39));
    assert!(response.usage.extra.is_empty());

    assert_eq!(response.extra["object"], json!("response"));
    assert_eq!(response.extra["model"], json!("gpt-5.5"));
    assert!(response.extra.contains_key("content_filters"));
    assert!(!response.extra.contains_key("status"));
    assert!(!response.extra.contains_key("output"));
    assert!(!response.extra.contains_key("usage"));
}

#[test]
fn recorded_tool_response_maps_call_id_arguments_and_tool_stop() {
    let response = OpenAiRespAdapter::parse_response(REAL_TOOL_RESPONSE.as_bytes())
        .expect("parse recorded OpenAI tool response");

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
    assert_eq!(id, "call_recorded_weather");
    assert_eq!(name, "get_weather");
    assert_eq!(input, &json!({ "city": "Tokyo" }));
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["item"]["id"],
        json!("fc_recorded_weather")
    );
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["item"]["status"],
        json!("completed")
    );
    assert_eq!(*response.stop_reason.value(), StopReason::ToolUse);
    assert_eq!(response.stop_reason.raw(), Some("completed"));
    assert_eq!(response.usage.input, 56);
    assert_eq!(response.usage.output, 18);
    assert_eq!(response.usage.reasoning, 0);
}

#[test]
fn reasoning_refusal_and_unknown_items_preserve_structured_evidence() {
    let body = json!({
        "id": "resp_extensions",
        "object": "response",
        "status": "completed",
        "output": [
            {
                "id": "rs_extensions",
                "type": "reasoning",
                "content": [
                    { "type": "reasoning_text", "text": "First step." },
                    { "type": "reasoning_text", "text": "Second step." }
                ],
                "summary": [{ "type": "summary_text", "text": "Short summary." }],
                "encrypted_content": "encrypted-123",
                "provider_reasoning_field": true
            },
            {
                "id": "msg_extensions",
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Partial answer.",
                        "annotations": [{ "type": "url_citation", "url": "https://example.test" }]
                    },
                    {
                        "type": "refusal",
                        "refusal": "I cannot help with that.",
                        "provider_refusal_code": "policy"
                    },
                    {
                        "type": "future_content",
                        "payload": { "kept": true }
                    }
                ]
            },
            {
                "id": "ws_extensions",
                "type": "web_search_call",
                "status": "completed",
                "action": { "type": "search", "query": "example" }
            }
        ],
        "usage": null,
        "provider_top_level": { "kept": true }
    });
    let body = serde_json::to_vec(&body).expect("serialize extension fixture");

    let response =
        OpenAiRespAdapter::parse_response(&body).expect("parse response extension fixture");

    let ContentBlock::Thinking {
        text,
        signature,
        extra,
    } = &response.message.content[0]
    else {
        panic!("first block should be reasoning");
    };
    assert_eq!(text, "First step.\nSecond step.");
    assert_eq!(signature.as_deref(), Some("encrypted-123"));
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["item"]["provider_reasoning_field"],
        json!(true)
    );
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["item"]["summary"],
        json!([{ "type": "summary_text", "text": "Short summary." }])
    );

    let ContentBlock::Text { text, extra } = &response.message.content[1] else {
        panic!("second block should be output text");
    };
    assert_eq!(text, "Partial answer.");
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["content"]["annotations"][0]["url"],
        json!("https://example.test")
    );
    let ContentBlock::Text { text, extra } = &response.message.content[2] else {
        panic!("third block should be refusal text");
    };
    assert_eq!(text, "I cannot help with that.");
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["content"]["provider_refusal_code"],
        json!("policy")
    );
    let ContentBlock::Unknown { type_name, raw } = &response.message.content[3] else {
        panic!("future message content should be retained as unknown block");
    };
    assert_eq!(type_name.as_deref(), Some("future_content"));
    assert_eq!(raw["type"], json!("future_content"));
    assert_eq!(raw["payload"], json!({ "kept": true }));

    assert_eq!(*response.stop_reason.value(), StopReason::Refusal);
    assert_eq!(response.stop_reason.raw(), Some("refusal"));
    assert_eq!(response.usage, Usage::default());
    assert_eq!(
        response.extra["provider_top_level"],
        json!({ "kept": true })
    );
    let unmodeled = response.extra[UNMODELED_OUTPUT_KEY]
        .as_array()
        .expect("unknown output evidence should be an array");
    assert_eq!(unmodeled.len(), 1);
    assert_eq!(unmodeled[0]["type"], json!("web_search_call"));
}

#[test]
fn incomplete_and_filtered_responses_map_specific_stop_reasons() {
    let incomplete = serde_json::to_vec(&json!({
        "object": "response",
        "status": "incomplete",
        "incomplete_details": { "reason": "max_output_tokens" },
        "output": [],
        "usage": { "input_tokens": 7, "output_tokens": 4 }
    }))
    .expect("serialize incomplete fixture");
    let response = OpenAiRespAdapter::parse_response(&incomplete)
        .expect("parse max-output incomplete response");
    assert_eq!(*response.stop_reason.value(), StopReason::MaxTokens);
    assert_eq!(response.stop_reason.raw(), Some("max_output_tokens"));
    assert_eq!(
        response.extra["incomplete_details"],
        json!({ "reason": "max_output_tokens" })
    );

    let filtered = serde_json::to_vec(&json!({
        "object": "response",
        "status": "completed",
        "content_filters": [{ "blocked": true, "source_type": "completion" }],
        "output": [],
        "usage": {}
    }))
    .expect("serialize filtered fixture");
    let response = OpenAiRespAdapter::parse_response(&filtered).expect("parse filtered response");
    assert_eq!(*response.stop_reason.value(), StopReason::Refusal);
    assert_eq!(response.stop_reason.raw(), Some("content_filter"));

    let future = serde_json::to_vec(&json!({
        "object": "response",
        "status": "paused_by_provider",
        "output": []
    }))
    .expect("serialize future-status fixture");
    let response = OpenAiRespAdapter::parse_response(&future).expect("parse future status");
    assert_eq!(*response.stop_reason.value(), StopReason::Other);
    assert_eq!(response.stop_reason.raw(), Some("paused_by_provider"));
}

#[test]
fn malformed_wire_data_returns_contextual_protocol_errors() {
    let malformed = OpenAiRespAdapter::parse_response(br#"{"object":"response""#)
        .expect_err("malformed JSON should fail");
    assert!(matches!(malformed, ClientError::Protocol(_)));
    assert!(malformed.to_string().contains("response JSON"));

    for (fixture, expected) in [
        (json!([]), "must be an object"),
        (
            json!({ "object": "list", "status": "completed", "output": [] }),
            "must be `response`",
        ),
        (
            json!({ "object": "response", "status": "completed" }),
            "field `output` is required",
        ),
        (
            json!({
                "object": "response",
                "status": "completed",
                "output": [{
                    "type": "message",
                    "role": "user",
                    "content": []
                }]
            }),
            "role must be `assistant`",
        ),
        (
            json!({
                "object": "response",
                "status": "completed",
                "output": [{
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "tool",
                    "arguments": "{not-json}"
                }]
            }),
            "invalid JSON arguments",
        ),
        (
            json!({
                "object": "response",
                "status": "completed",
                "output": [],
                "content_filters": {}
            }),
            "content_filters must be an array",
        ),
        (
            json!({
                "object": "response",
                "status": "completed",
                "output": [],
                "usage": { "input": 1, "input_tokens": 2 }
            }),
            "invalid usage object",
        ),
    ] {
        let bytes = serde_json::to_vec(&fixture).expect("serialize malformed fixture");
        let error = OpenAiRespAdapter::parse_response(&bytes)
            .expect_err("malformed fixture should fail conversion");
        assert!(matches!(error, ClientError::Protocol(_)));
        assert!(
            error.to_string().contains(expected),
            "expected `{expected}` in `{error}`"
        );
    }
}

#[test]
fn empty_function_arguments_response_parse_as_empty_object() {
    let fixture = json!({
        "object": "response",
        "status": "completed",
        "output": [{
            "type": "function_call",
            "call_id": "call_empty",
            "name": "ping",
            "arguments": ""
        }]
    });
    let bytes = serde_json::to_vec(&fixture).expect("serialize response fixture");
    let response = OpenAiRespAdapter::parse_response(&bytes)
        .expect("empty function arguments should parse as an empty object");

    let [
        ContentBlock::ToolUse {
            id, name, input, ..
        },
    ] = response.message.content.as_slice()
    else {
        panic!("expected one tool-use block");
    };
    assert_eq!(id, "call_empty");
    assert_eq!(name, "ping");
    assert_eq!(input, &json!({}));
}
