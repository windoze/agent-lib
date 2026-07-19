//! Request conversion and endpoint-configuration tests.

use super::input::message_to_items;
use super::*;
use crate::{
    adapter::openai_resp::RESPONSE_EXTRA_KEY,
    client::{AuthScheme, EndpointConfig},
    model::{
        content::{ContentBlock, ImageSource},
        extras::ProviderExtras,
        message::{Message, Role},
        tool::{Tool, ToolStatus},
    },
};
use reqwest::{
    Method, Request,
    header::{AUTHORIZATION, CONTENT_TYPE},
};
use serde_json::{Map, Value, json};

/// Creates an empty provider-field map for concise fixtures.
fn empty_extra() -> Map<String, Value> {
    Map::new()
}

/// Returns a small valid request for error and transport tests.
fn minimal_request() -> ChatRequest {
    ChatRequest {
        model: "gpt-5.5".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_owned(),
                extra: empty_extra(),
            }],
        }],
        tools: Vec::new(),
        system: None,
        max_tokens: 64,
        temperature: None,
        stream: false,
        provider_extras: None,
    }
}

/// Decodes the buffered JSON body produced by reqwest's request builder.
fn request_body(request: &Request) -> Value {
    let bytes = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .expect("JSON request body should be buffered");
    serde_json::from_slice(bytes).expect("request body should contain valid JSON")
}

#[test]
fn complete_chat_request_maps_to_responses_items_and_endpoint_shape() {
    let endpoint = EndpointConfig {
        base_url: "https://openai.example.test/openai/v1/".to_owned(),
        auth: AuthScheme::Header {
            name: "api-key".to_owned(),
            value: "secret-key".to_owned(),
        },
        query_params: vec![
            ("api-version".to_owned(), "2025-04-01-preview".to_owned()),
            ("feature".to_owned(), "one".to_owned()),
            ("feature".to_owned(), "two".to_owned()),
        ],
        extra_headers: vec![("x-trace-id".to_owned(), "trace-123".to_owned())],
    };
    let request = ChatRequest {
        model: "gpt-5.5".to_owned(),
        messages: vec![
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "What's the weather?".to_owned(),
                        extra: Map::from_iter([
                            ("type".to_owned(), json!("wrong")),
                            ("input_tag".to_owned(), json!("kept")),
                        ]),
                    },
                    ContentBlock::Image {
                        source: ImageSource::Url {
                            url: "https://example.test/weather.png".to_owned(),
                            extra: Map::from_iter([("detail".to_owned(), json!("high"))]),
                        },
                        extra: Map::from_iter([("provider_hint".to_owned(), json!(true))]),
                    },
                    ContentBlock::Image {
                        source: ImageSource::Base64 {
                            media_type: "image/png".to_owned(),
                            data: "iVBORw0KGgo=".to_owned(),
                            extra: empty_extra(),
                        },
                        extra: empty_extra(),
                    },
                ],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "I'll check.".to_owned(),
                        extra: empty_extra(),
                    },
                    ContentBlock::Thinking {
                        text: "Use the weather function.".to_owned(),
                        signature: Some("encrypted-reasoning".to_owned()),
                        extra: empty_extra(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_weather_1".to_owned(),
                        name: "get_weather".to_owned(),
                        input: json!({ "city": "Tokyo" }),
                        extra: empty_extra(),
                    },
                ],
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_weather_1".to_owned(),
                    content: vec![ContentBlock::Text {
                        text: "sunny".to_owned(),
                        extra: empty_extra(),
                    }],
                    status: ToolStatus::Ok,
                    extra: empty_extra(),
                }],
            },
        ],
        tools: vec![Tool {
            name: "get_weather".to_owned(),
            description: "Get current weather for a city.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        }],
        system: Some("Answer concisely.".to_owned()),
        max_tokens: 1_024,
        temperature: Some(0.25),
        stream: false,
        provider_extras: Some(ProviderExtras {
            provider: ProviderId::OpenAiResp,
            fields: Map::from_iter([
                ("store".to_owned(), json!(false)),
                ("reasoning".to_owned(), json!({ "effort": "high" })),
            ]),
        }),
    };
    let adapter = OpenAiRespAdapter::new(endpoint.clone());

    let built = adapter
        .build_request(&request)
        .expect("build OpenAI Responses request");

    assert_eq!(adapter.endpoint(), &endpoint);
    assert_eq!(built.method(), Method::POST);
    assert_eq!(built.url().path(), "/openai/v1/responses");
    assert_eq!(
        built
            .url()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>(),
        vec![
            ("api-version".to_owned(), "2025-04-01-preview".to_owned()),
            ("feature".to_owned(), "one".to_owned()),
            ("feature".to_owned(), "two".to_owned()),
        ]
    );
    assert_eq!(built.headers()["api-key"], "secret-key");
    assert_eq!(built.headers()[CONTENT_TYPE], "application/json");
    assert_eq!(built.headers()["x-trace-id"], "trace-123");
    assert!(!built.headers().contains_key(AUTHORIZATION));
    assert_eq!(
        request_body(&built),
        json!({
            "model": "gpt-5.5",
            "input": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "What's the weather?",
                            "input_tag": "kept"
                        },
                        {
                            "type": "input_image",
                            "image_url": "https://example.test/weather.png",
                            "detail": "high",
                            "provider_hint": true
                        },
                        {
                            "type": "input_image",
                            "image_url": "data:image/png;base64,iVBORw0KGgo="
                        }
                    ]
                },
                {
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "I'll check." }]
                },
                {
                    "type": "reasoning",
                    "summary": [{
                        "type": "summary_text",
                        "text": "Use the weather function."
                    }],
                    "encrypted_content": "encrypted-reasoning"
                },
                {
                    "type": "function_call",
                    "call_id": "call_weather_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Tokyo\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_weather_1",
                    "output": "sunny",
                    "status": "completed"
                }
            ],
            "instructions": "Answer concisely.",
            "max_output_tokens": 1024,
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "description": "Get current weather for a city.",
                "parameters": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }
            }],
            "temperature": 0.25,
            "stream": false,
            "store": false,
            "reasoning": { "effort": "high" }
        })
    );
}

#[test]
fn assistant_text_uses_output_vocabulary_and_replays_refusal_kind() {
    let plain = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "normalized answer".to_owned(),
            extra: Map::from_iter([
                ("type".to_owned(), json!("input_text")),
                ("text".to_owned(), json!("stale answer")),
                ("refusal".to_owned(), json!("stale refusal")),
                ("provider_hint".to_owned(), json!(true)),
            ]),
        }],
    };
    assert_eq!(
        message_to_items(0, &plain).expect("map plain assistant text"),
        vec![json!({
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": "normalized answer",
                "provider_hint": true
            }]
        })]
    );

    let refusal = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "normalized refusal".to_owned(),
            extra: Map::from_iter([(
                RESPONSE_EXTRA_KEY.to_owned(),
                json!({
                    "item": { "id": "msg_refusal", "type": "message" },
                    "content": {
                        "type": "refusal",
                        "refusal": "wire refusal",
                        "annotations": []
                    }
                }),
            )]),
        }],
    };
    assert_eq!(
        message_to_items(0, &refusal).expect("replay assistant refusal"),
        vec![json!({
            "role": "assistant",
            "content": [{
                "type": "refusal",
                "refusal": "normalized refusal"
            }]
        })]
    );
}

#[test]
fn unknown_content_block_request_serializes_raw_value() {
    let raw = json!({
        "type": "future_content",
        "payload": { "kept": true }
    });
    let message = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Unknown {
            type_name: Some("future_content".to_owned()),
            raw: raw.clone(),
        }],
    };

    let items = message_to_items(0, &message).expect("serialize unknown content block");

    assert_eq!(
        items,
        vec![json!({
            "role": "assistant",
            "content": [raw]
        })]
    );
}

#[test]
fn parsed_item_metadata_is_replayed_but_modeled_fields_win() {
    let mut request = minimal_request();
    request.messages = vec![Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                text: "normalized summary".to_owned(),
                signature: Some("normalized-encrypted".to_owned()),
                extra: Map::from_iter([(
                    RESPONSE_EXTRA_KEY.to_owned(),
                    json!({
                        "item": {
                            "id": "rs_recorded",
                            "type": "wrong",
                            "summary": [{ "type": "summary_text", "text": "wire summary" }],
                            "encrypted_content": "old-encrypted"
                        }
                    }),
                )]),
            },
            ContentBlock::ToolUse {
                id: "call_normalized".to_owned(),
                name: "normalized_tool".to_owned(),
                input: json!({ "value": 2 }),
                extra: Map::from_iter([(
                    RESPONSE_EXTRA_KEY.to_owned(),
                    json!({
                        "item": {
                            "id": "fc_recorded",
                            "status": "completed",
                            "type": "wrong",
                            "call_id": "wrong",
                            "arguments": "{}"
                        }
                    }),
                )]),
            },
        ],
    }];
    let adapter = OpenAiRespAdapter::new(EndpointConfig {
        base_url: "https://example.test/v1".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    });

    let body = request_body(
        &adapter
            .build_request(&request)
            .expect("build replay request"),
    );

    assert_eq!(
        body["input"][0],
        json!({
            "id": "rs_recorded",
            "type": "reasoning",
            "summary": [{ "type": "summary_text", "text": "wire summary" }],
            "encrypted_content": "normalized-encrypted"
        })
    );
    assert_eq!(
        body["input"][1],
        json!({
            "id": "fc_recorded",
            "status": "completed",
            "type": "function_call",
            "call_id": "call_normalized",
            "name": "normalized_tool",
            "arguments": "{\"value\":2}"
        })
    );
}

#[test]
fn multimodal_error_tool_result_uses_list_output_and_incomplete_status() {
    let message = Message {
        role: Role::Tool,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_owned(),
            content: vec![
                ContentBlock::Text {
                    text: "lookup failed".to_owned(),
                    extra: Map::from_iter([("language".to_owned(), json!("en"))]),
                },
                ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://example.test/error.png".to_owned(),
                        extra: empty_extra(),
                    },
                    extra: empty_extra(),
                },
            ],
            status: ToolStatus::Error,
            extra: Map::from_iter([("provider_trace".to_owned(), json!("trace-1"))]),
        }],
    };

    let items = message_to_items(0, &message).expect("map multimodal tool result");

    assert_eq!(
        items,
        vec![json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": [
                { "type": "input_text", "text": "lookup failed", "language": "en" },
                {
                    "type": "input_image",
                    "image_url": "https://example.test/error.png"
                }
            ],
            "status": "incomplete",
            "provider_trace": "trace-1"
        })]
    );
}

#[test]
fn each_tool_status_maps_to_responses_terminal_state_without_mutating_source() {
    for (status, expected_status) in [
        (ToolStatus::Ok, "completed"),
        (ToolStatus::Error, "incomplete"),
        (ToolStatus::Denied, "incomplete"),
        (ToolStatus::Cancelled, "incomplete"),
    ] {
        let message = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".to_owned(),
                content: vec![ContentBlock::Text {
                    text: "result".to_owned(),
                    extra: empty_extra(),
                }],
                status,
                extra: Map::from_iter([
                    ("status".to_owned(), json!("wrong")),
                    ("is_error".to_owned(), json!(false)),
                    ("provider_trace".to_owned(), json!("trace-1")),
                ]),
            }],
        };
        let original = message.clone();
        let items = message_to_items(0, &message).expect("map normalized tool status");

        assert_eq!(items[0]["status"], json!(expected_status));
        assert!(items[0].get("is_error").is_none());
        assert_eq!(items[0]["provider_trace"], json!("trace-1"));
        assert_eq!(message, original);
    }
}

#[test]
fn invalid_roles_blocks_and_foreign_extras_are_rejected() {
    let mut request = minimal_request();
    request.messages[0].role = Role::System;
    let error = serialize_body(&request).expect_err("system message should be rejected");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("ChatRequest.system"));

    request = minimal_request();
    request.messages[0].content = vec![ContentBlock::ToolUse {
        id: "call_1".to_owned(),
        name: "tool".to_owned(),
        input: json!({}),
        extra: empty_extra(),
    }];
    let error = serialize_body(&request).expect_err("user tool call should be rejected");
    assert!(error.to_string().contains("not valid for User role"));

    request = minimal_request();
    request.messages[0] = Message {
        role: Role::Tool,
        content: vec![ContentBlock::Text {
            text: "unlinked result".to_owned(),
            extra: empty_extra(),
        }],
    };
    let error = serialize_body(&request).expect_err("unlinked tool text should be rejected");
    assert!(error.to_string().contains("not valid for Tool role"));

    request = minimal_request();
    request.messages[0] = Message {
        role: Role::Tool,
        content: Vec::new(),
    };
    let error = serialize_body(&request).expect_err("empty tool message should be rejected");
    assert!(error.to_string().contains("contains no tool results"));

    request = minimal_request();
    request.messages[0] = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.test/assistant.png".to_owned(),
                extra: empty_extra(),
            },
            extra: empty_extra(),
        }],
    };
    let error = serialize_body(&request).expect_err("assistant image should be rejected");
    assert!(error.to_string().contains("assistant image blocks"));

    request = minimal_request();
    request.messages[0] = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "future output".to_owned(),
            extra: Map::from_iter([(
                RESPONSE_EXTRA_KEY.to_owned(),
                json!({ "content": { "type": "future_text" } }),
            )]),
        }],
    };
    let error = serialize_body(&request).expect_err("unknown assistant text kind should fail");
    assert!(error.to_string().contains("unsupported content type"));

    request = minimal_request();
    request.provider_extras = Some(ProviderExtras {
        provider: ProviderId::Anthropic,
        fields: Map::from_iter([("top_k".to_owned(), json!(20))]),
    });
    let error = serialize_body(&request).expect_err("foreign extras should be rejected");
    assert!(error.to_string().contains("Anthropic"));
    assert!(error.to_string().contains("OpenAiResp"));
}

#[test]
fn optional_fields_auth_variants_and_malformed_endpoint_are_observable() {
    let header_adapter = OpenAiRespAdapter::new(EndpointConfig {
        base_url: "https://openai.example.test/v1".to_owned(),
        auth: AuthScheme::Bearer("token".to_owned()),
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    });
    let built = header_adapter
        .build_request(&minimal_request())
        .expect("build minimal request");
    let body = request_body(&built);

    assert_eq!(built.url().path(), "/v1/responses");
    assert_eq!(built.url().query(), None);
    assert_eq!(built.headers()[AUTHORIZATION], "Bearer token");
    assert_eq!(body["stream"], json!(false));
    assert!(body.get("instructions").is_none());
    assert!(body.get("tools").is_none());
    assert!(body.get("temperature").is_none());

    let malformed_url = OpenAiRespAdapter::new(EndpointConfig {
        base_url: "://not a URL".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    })
    .build_request(&minimal_request())
    .expect_err("malformed URL should fail before network use");
    assert!(matches!(malformed_url, ClientError::Other(_)));
    assert!(malformed_url.to_string().contains("invalid base URL"));

    let malformed_header = OpenAiRespAdapter::new(EndpointConfig {
        base_url: "https://openai.example.test".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: vec![("bad\nheader".to_owned(), "value".to_owned())],
    })
    .build_request(&minimal_request())
    .expect_err("malformed header should fail before network use");
    assert!(matches!(malformed_header, ClientError::Other(_)));
    assert!(malformed_header.to_string().contains("invalid header name"));
}
