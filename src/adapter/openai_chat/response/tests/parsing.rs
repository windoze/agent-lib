//! Fixture-driven and adversarial complete-response conversion tests.

use super::*;
use crate::model::normalized::StopReason;
use serde_json::json;

/// Serializes a JSON value into the bytes expected by `parse_response`.
fn body(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("serialize response fixture")
}

#[test]
fn recorded_text_response_maps_content_usage_and_extra() {
    let response = OpenAiChatAdapter::parse_response(REAL_TEXT_RESPONSE.as_bytes())
        .expect("parse recorded chat/completions text response");

    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(response.message.content.len(), 1);
    let ContentBlock::Text { text, extra } = &response.message.content[0] else {
        panic!("choice message should become a text block");
    };
    assert_eq!(text, "Hi there");
    assert!(extra.is_empty());

    assert_eq!(*response.stop_reason.value(), StopReason::EndTurn);
    assert_eq!(response.stop_reason.raw(), Some("stop"));
    assert_eq!(response.usage.input, 13);
    assert_eq!(response.usage.output, 26);
    assert_eq!(response.usage.cache_read, 4);
    assert_eq!(response.usage.reasoning, 0);
    assert_eq!(response.usage.total, Some(39));
    assert!(response.usage.extra.is_empty());

    assert_eq!(response.extra["object"], json!("chat.completion"));
    assert_eq!(response.extra["model"], json!("deepseek-chat"));
    assert_eq!(response.extra["created"], json!(1783882282));
    assert_eq!(response.extra["system_fingerprint"], json!("fp_demo_text"));
    // choices stay in extra so logprobs and choice evidence survive (design §2.2).
    assert_eq!(response.extra["choices"][0]["logprobs"], json!(null));
    // usage is consumed into the normalized field, not duplicated in extra.
    assert!(!response.extra.contains_key("usage"));
}

#[test]
fn recorded_tool_response_maps_call_id_arguments_and_tool_stop() {
    let response = OpenAiChatAdapter::parse_response(REAL_TOOL_RESPONSE.as_bytes())
        .expect("parse recorded chat/completions tool response");

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
    assert!(extra.is_empty());
    assert_eq!(*response.stop_reason.value(), StopReason::ToolUse);
    assert_eq!(response.stop_reason.raw(), Some("tool_calls"));
    assert_eq!(response.usage.input, 56);
    assert_eq!(response.usage.output, 18);
    assert_eq!(response.usage.total, Some(74));
}

#[test]
fn recorded_reasoning_response_maps_reasoning_block_before_text() {
    let response = OpenAiChatAdapter::parse_response(REAL_REASONING_RESPONSE.as_bytes())
        .expect("parse recorded chat/completions reasoning response");

    assert_eq!(response.message.content.len(), 2);
    // Reasoning precedes text (anthropic convention, design doc §4.3).
    let ContentBlock::Thinking {
        text,
        signature,
        extra,
    } = &response.message.content[0]
    else {
        panic!("first block should be reasoning");
    };
    assert_eq!(text, "Let me think about this step by step.");
    assert_eq!(signature, &None);
    assert!(extra.is_empty());

    let ContentBlock::Text { text, .. } = &response.message.content[1] else {
        panic!("second block should be text");
    };
    assert_eq!(text, "The answer is 42.");

    assert_eq!(*response.stop_reason.value(), StopReason::EndTurn);
    assert_eq!(response.usage.reasoning, 35);
    assert_eq!(response.usage.cache_read, 6);
}

#[test]
fn finish_reason_table_maps_every_value() {
    for (finish_reason, expected, expected_raw) in [
        (Some(json!("stop")), StopReason::EndTurn, Some("stop")),
        (Some(json!("length")), StopReason::MaxTokens, Some("length")),
        (
            Some(json!("tool_calls")),
            StopReason::ToolUse,
            Some("tool_calls"),
        ),
        (
            Some(json!("content_filter")),
            StopReason::Refusal,
            Some("content_filter"),
        ),
        (
            Some(json!("future_reason")),
            StopReason::Other,
            Some("future_reason"),
        ),
        (None, StopReason::Other, None),
    ] {
        let mut choice = json!({
            "index": 0,
            "message": { "role": "assistant", "content": "ok" }
        });
        if let Some(reason) = finish_reason {
            choice["finish_reason"] = reason;
        }
        let response_value = json!({
            "object": "chat.completion",
            "choices": [choice]
        });
        let response =
            OpenAiChatAdapter::parse_response(&body(&response_value)).expect("parse fixture");
        assert_eq!(
            *response.stop_reason.value(),
            expected,
            "finish_reason mapping mismatch"
        );
        assert_eq!(
            response.stop_reason.raw(),
            expected_raw,
            "finish_reason raw mismatch"
        );
    }
}

#[test]
fn null_finish_reason_maps_to_other_without_raw() {
    let response_value = json!({
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "hi" },
            "finish_reason": null
        }]
    });
    let response = OpenAiChatAdapter::parse_response(&body(&response_value))
        .expect("parse null finish_reason");
    assert_eq!(*response.stop_reason.value(), StopReason::Other);
    assert_eq!(response.stop_reason.raw(), None);
}

#[test]
fn unknown_top_level_fields_and_choice_logprobs_stay_in_extra() {
    let response_value = json!({
        "id": "chatcmpl-extra",
        "object": "chat.completion",
        "created": 1783882400,
        "model": "deepseek-chat",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "kept" },
            "finish_reason": "stop",
            "logprobs": { "content": [{ "token": "kept", "logprob": -0.1 }] }
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 },
        "provider_trace_id": "trace-1"
    });
    let response =
        OpenAiChatAdapter::parse_response(&body(&response_value)).expect("parse extra fixture");

    assert_eq!(response.extra["provider_trace_id"], json!("trace-1"));
    assert_eq!(response.extra["id"], json!("chatcmpl-extra"));
    // The full choice object, including structured logprobs, survives in extra.
    assert_eq!(
        response.extra["choices"][0]["logprobs"]["content"][0]["token"],
        json!("kept")
    );
    assert!(!response.extra.contains_key("usage"));
}

#[test]
fn empty_arguments_parse_as_empty_object() {
    let response_value = json!({
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_empty",
                    "type": "function",
                    "function": { "name": "ping", "arguments": "" }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    let response =
        OpenAiChatAdapter::parse_response(&body(&response_value)).expect("parse empty arguments");

    let [ContentBlock::ToolUse { input, extra, .. }] = response.message.content.as_slice() else {
        panic!("expected one tool-use block");
    };
    assert_eq!(input, &json!({}));
    assert!(extra.is_empty());
}

#[test]
fn invalid_arguments_keep_raw_text_in_extra_and_null_input() {
    let response_value = json!({
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_bad",
                    "type": "function",
                    "function": { "name": "act", "arguments": "{not-json}" }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    let response =
        OpenAiChatAdapter::parse_response(&body(&response_value)).expect("parse bad arguments");

    let [ContentBlock::ToolUse { input, extra, .. }] = response.message.content.as_slice() else {
        panic!("expected one tool-use block");
    };
    assert_eq!(input, &json!(null));
    assert_eq!(
        extra[RESPONSE_EXTRA_KEY]["raw_arguments"],
        json!("{not-json}")
    );
}

#[test]
fn parallel_tool_calls_become_ordered_tool_use_blocks() {
    let response_value = json!({
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_a",
                        "type": "function",
                        "function": { "name": "first", "arguments": "{\"n\":1}" }
                    },
                    {
                        "id": "call_b",
                        "type": "function",
                        "function": { "name": "second", "arguments": "{\"n\":2}" }
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }]
    });
    let response = OpenAiChatAdapter::parse_response(&body(&response_value))
        .expect("parse parallel tool calls");

    let [first, second] = response.message.content.as_slice() else {
        panic!("expected two tool-use blocks");
    };
    let ContentBlock::ToolUse {
        id, name, input, ..
    } = first
    else {
        panic!("first block should be a tool use");
    };
    assert_eq!(id, "call_a");
    assert_eq!(name, "first");
    assert_eq!(input, &json!({ "n": 1 }));
    let ContentBlock::ToolUse {
        id, name, input, ..
    } = second
    else {
        panic!("second block should be a tool use");
    };
    assert_eq!(id, "call_b");
    assert_eq!(name, "second");
    assert_eq!(input, &json!({ "n": 2 }));
}

#[test]
fn malformed_wire_data_returns_contextual_protocol_errors() {
    let malformed = OpenAiChatAdapter::parse_response(br#"{"object":"chat.completion""#)
        .expect_err("malformed JSON should fail");
    assert!(matches!(malformed, ClientError::Protocol(_)));
    assert!(malformed.to_string().contains("response JSON"));

    for (fixture, expected) in [
        (json!([]), "must be an object"),
        (
            json!({ "object": "chat.completions", "choices": [] }),
            "must be `chat.completion`",
        ),
        (
            json!({ "object": 5, "choices": [] }),
            "field `object` must be a string",
        ),
        (
            json!({ "object": "chat.completion" }),
            "field `choices` is required",
        ),
        (
            json!({ "object": "chat.completion", "choices": [] }),
            "must contain at least one choice",
        ),
        (
            json!({ "object": "chat.completion", "choices": [{}] }),
            "choices[0].message is required",
        ),
        (
            json!({
                "object": "chat.completion",
                "choices": [{
                    "message": { "role": "user", "content": "x" }
                }]
            }),
            "role must be `assistant`",
        ),
        (
            json!({
                "object": "chat.completion",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{ "id": "c", "type": "function" }]
                    }
                }]
            }),
            "field `function` is required",
        ),
        (
            json!({
                "object": "chat.completion",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": 7
                    }
                }]
            }),
            "field `content` must be a string",
        ),
        (
            json!({
                "object": "chat.completion",
                "choices": [{
                    "message": { "role": "assistant", "content": "x" },
                    "finish_reason": 9
                }]
            }),
            "finish_reason must be a string or null",
        ),
        (
            json!({
                "object": "chat.completion",
                "choices": [{
                    "message": { "role": "assistant", "content": "x" }
                }],
                "usage": { "prompt_tokens": "lots" }
            }),
            "invalid usage object",
        ),
    ] {
        let error = OpenAiChatAdapter::parse_response(&body(&fixture))
            .expect_err("malformed fixture should fail conversion");
        assert!(
            matches!(error, ClientError::Protocol(_)),
            "expected Protocol error for {fixture}"
        );
        assert!(
            error.to_string().contains(expected),
            "expected `{expected}` in `{error}` (fixture {fixture})"
        );
    }
}
