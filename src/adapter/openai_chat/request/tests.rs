//! Request conversion and endpoint-configuration tests.

use super::input::message_to_wire;
use super::*;
use crate::{
    client::{AuthScheme, EndpointConfig},
    model::{
        content::{ContentBlock, ImageSource},
        extras::{ProviderExtras, ProviderId},
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
        model: "deepseek-chat".to_owned(),
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

/// Builds an adapter whose endpoint is irrelevant for body-only assertions.
fn body_only_adapter() -> OpenAiChatAdapter {
    OpenAiChatAdapter::new(EndpointConfig {
        base_url: "https://api.deepseek.com/v1".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    })
}

/// Covers design doc §4.2 end to end: system as the first message, user text,
/// assistant aggregation, tool result, nested `function` tools, sampling,
/// stream-off, and endpoint shape.
#[test]
fn complete_chat_request_maps_to_chat_completions_body_and_endpoint_shape() {
    let endpoint = EndpointConfig {
        base_url: "https://api.deepseek.com/v1/".to_owned(),
        auth: AuthScheme::Bearer("sk-deepseek-secret".to_owned()),
        query_params: vec![("preview".to_owned(), "thinking".to_owned())],
        extra_headers: vec![("x-trace-id".to_owned(), "trace-123".to_owned())],
    };
    let request = ChatRequest {
        model: "deepseek-chat".to_owned(),
        messages: vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "What's the weather?".to_owned(),
                    extra: empty_extra(),
                }],
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
                        signature: Some("ignored-by-chat".to_owned()),
                        extra: empty_extra(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".to_owned(),
                        name: "get_weather".to_owned(),
                        input: json!({ "city": "Tokyo" }),
                        extra: empty_extra(),
                    },
                ],
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_owned(),
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
        provider_extras: None,
    };
    let adapter = OpenAiChatAdapter::new(endpoint.clone());

    let built = adapter
        .build_request(&request)
        .expect("build chat/completions request");

    assert_eq!(adapter.endpoint(), &endpoint);
    assert_eq!(built.method(), Method::POST);
    assert_eq!(built.url().path(), "/v1/chat/completions");
    assert_eq!(
        built
            .url()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>(),
        vec![("preview".to_owned(), "thinking".to_owned())]
    );
    assert_eq!(built.headers()[AUTHORIZATION], "Bearer sk-deepseek-secret");
    assert_eq!(built.headers()[CONTENT_TYPE], "application/json");
    assert_eq!(built.headers()["x-trace-id"], "trace-123");
    assert_eq!(
        request_body(&built),
        json!({
            "model": "deepseek-chat",
            "messages": [
                { "role": "system", "content": "Answer concisely." },
                { "role": "user", "content": "What's the weather?" },
                {
                    "role": "assistant",
                    "content": "I'll check.",
                    "reasoning_content": "Use the weather function.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"Tokyo\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "sunny" }
            ],
            "max_tokens": 1024,
            "stream": false,
            "temperature": 0.25,
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get current weather for a city.",
                    "parameters": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                        "required": ["city"]
                    }
                }
            }]
        })
    );
}

/// Verifies the §5.1 multi-turn rule: one assistant message carrying text,
/// reasoning, and tool calls collapses into a single chat message that retains
/// all three fields (the DeepSeek 400 defense line).
#[test]
fn assistant_message_aggregates_content_reasoning_and_tool_calls_into_one_message() {
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                text: "I should call the tool.".to_owned(),
                signature: None,
                extra: empty_extra(),
            },
            ContentBlock::ToolUse {
                id: "call_42".to_owned(),
                name: "search".to_owned(),
                input: json!({ "q": "rust async" }),
                extra: empty_extra(),
            },
            ContentBlock::Text {
                text: "Let me look that up.".to_owned(),
                extra: empty_extra(),
            },
            ContentBlock::ToolUse {
                id: "call_43".to_owned(),
                name: "search".to_owned(),
                input: json!({ "q": "tokio runtime" }),
                extra: empty_extra(),
            },
        ],
    };

    let items = message_to_wire(0, &message).expect("aggregate assistant message");

    assert_eq!(
        items,
        vec![json!({
            "role": "assistant",
            "content": "Let me look that up.",
            "reasoning_content": "I should call the tool.",
            "tool_calls": [
                {
                    "id": "call_42",
                    "type": "function",
                    "function": { "name": "search", "arguments": "{\"q\":\"rust async\"}" }
                },
                {
                    "id": "call_43",
                    "type": "function",
                    "function": { "name": "search", "arguments": "{\"q\":\"tokio runtime\"}" }
                }
            ]
        })]
    );

    // An assistant message with tool calls and reasoning but no text yields a
    // null `content`, matching how chat/completions encodes tool-only turns.
    let tool_only = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                text: "reasoning only".to_owned(),
                signature: None,
                extra: empty_extra(),
            },
            ContentBlock::ToolUse {
                id: "call_1".to_owned(),
                name: "act".to_owned(),
                input: json!({}),
                extra: empty_extra(),
            },
        ],
    };
    let items = message_to_wire(0, &tool_only).expect("aggregate tool-only assistant");
    assert_eq!(items[0]["content"], Value::Null);
    assert_eq!(items[0]["reasoning_content"], json!("reasoning only"));
    assert!(items[0]["tool_calls"].is_array());
}

/// `stream=true` injects `stream_options.include_usage`; `stream=false` omits it.
#[test]
fn stream_flag_controls_include_usage_injection() {
    let adapter = body_only_adapter();

    let mut streaming = minimal_request();
    streaming.stream = true;
    let body = request_body(
        &adapter
            .build_request(&streaming)
            .expect("build streaming request"),
    );
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["stream_options"], json!({ "include_usage": true }));

    let non_streaming = minimal_request();
    let body = request_body(
        &adapter
            .build_request(&non_streaming)
            .expect("build non-streaming request"),
    );
    assert_eq!(body["stream"], json!(false));
    assert!(body.get("stream_options").is_none());
}

/// Tool result content is flattened to text; non-`Ok` outcomes are merged into
/// the text via a status marker; images in results are dropped (lossy).
#[test]
fn tool_result_flattens_to_text_and_merges_non_ok_status() {
    for (status, expected_content) in [
        (ToolStatus::Ok, "lookup failed"),
        (ToolStatus::Error, "[tool error] lookup failed"),
        (ToolStatus::Denied, "[tool denied] lookup failed"),
        (ToolStatus::Cancelled, "[tool cancelled] lookup failed"),
    ] {
        let message = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".to_owned(),
                content: vec![ContentBlock::Text {
                    text: "lookup failed".to_owned(),
                    extra: empty_extra(),
                }],
                status,
                extra: empty_extra(),
            }],
        };

        let items = message_to_wire(0, &message).expect("map tool result status");

        assert_eq!(
            items,
            vec![json!({
                "role": "tool",
                "tool_call_id": "call_1",
                "content": expected_content
            })]
        );
    }

    // Multiple results on one tool message become multiple tool messages;
    // multimodal (text + image) results keep only the text.
    let multimodal = Message {
        role: Role::Tool,
        content: vec![
            ContentBlock::ToolResult {
                tool_use_id: "call_a".to_owned(),
                content: vec![
                    ContentBlock::Text {
                        text: "first".to_owned(),
                        extra: empty_extra(),
                    },
                    ContentBlock::Image {
                        source: ImageSource::Url {
                            url: "https://example.test/chart.png".to_owned(),
                            extra: empty_extra(),
                        },
                        extra: empty_extra(),
                    },
                ],
                status: ToolStatus::Ok,
                extra: empty_extra(),
            },
            ContentBlock::ToolResult {
                tool_use_id: "call_b".to_owned(),
                content: vec![ContentBlock::Text {
                    text: "second".to_owned(),
                    extra: empty_extra(),
                }],
                status: ToolStatus::Ok,
                extra: empty_extra(),
            },
        ],
    };
    let items = message_to_wire(0, &multimodal).expect("map multimodal tool results");
    assert_eq!(
        items,
        vec![
            json!({ "role": "tool", "tool_call_id": "call_a", "content": "first" }),
            json!({ "role": "tool", "tool_call_id": "call_b", "content": "second" }),
        ]
    );
}

/// Matching provider extras override body fields and add new ones; a provider
/// mismatch is rejected (design doc §4.2 last row).
#[test]
fn provider_extras_override_body_fields_and_mismatch_is_rejected() {
    let mut request = minimal_request();
    request.max_tokens = 64;
    request.provider_extras = Some(ProviderExtras {
        provider: ProviderId::OpenAiChat,
        fields: Map::from_iter([
            ("max_tokens".to_owned(), json!(256)),
            ("top_p".to_owned(), json!(0.5)),
        ]),
    });

    let body = serialize_body(&request).expect("merge matching extras");
    assert_eq!(body["max_tokens"], json!(256));
    assert_eq!(body["top_p"], json!(0.5));

    request.provider_extras = Some(ProviderExtras {
        provider: ProviderId::Anthropic,
        fields: Map::from_iter([("top_k".to_owned(), json!(20))]),
    });
    let error = serialize_body(&request).expect_err("foreign extras should be rejected");
    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("Anthropic"));
    assert!(error.to_string().contains("OpenAiChat"));
}

/// Tools serialize with the chat/completions `function` nesting.
#[test]
fn tools_serialize_with_nested_function_shape() {
    let tool = Tool {
        name: "get_weather".to_owned(),
        description: "Get current weather for a city.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    };
    assert_eq!(
        super::input::tool_to_wire(&tool),
        json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather for a city.",
                "parameters": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }
            }
        })
    );
}

/// User messages with an image use the multimodal array form (vision input).
#[test]
fn user_message_with_image_uses_multimodal_array_form() {
    let message = Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: "describe this".to_owned(),
                extra: empty_extra(),
            },
            ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.test/photo.png".to_owned(),
                    extra: Map::from_iter([("detail".to_owned(), json!("high"))]),
                },
                extra: empty_extra(),
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
    };

    let items = message_to_wire(0, &message).expect("map multimodal user message");

    assert_eq!(
        items,
        vec![json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "describe this" },
                { "type": "image_url", "image_url": { "url": "https://example.test/photo.png", "detail": "high" } },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgo=" } }
            ]
        })]
    );
}

/// Invalid roles, role/block mismatches, and empty tool messages are rejected
/// as protocol errors before any network use.
#[test]
fn invalid_roles_blocks_and_empty_tool_messages_are_rejected() {
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
    request.messages[0].content = vec![ContentBlock::Thinking {
        text: "hm".to_owned(),
        signature: None,
        extra: empty_extra(),
    }];
    let error = serialize_body(&request).expect_err("user thinking should be rejected");
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
                url: "https://example.test/a.png".to_owned(),
                extra: empty_extra(),
            },
            extra: empty_extra(),
        }],
    };
    let error = serialize_body(&request).expect_err("assistant image should be rejected");
    assert!(error.to_string().contains("not valid for Assistant role"));
}

/// Auth schemes, optional-field omission, and malformed endpoint configuration
/// are observable through the built request.
#[test]
fn auth_variants_optional_fields_and_malformed_endpoint_are_observable() {
    let bearer_adapter = OpenAiChatAdapter::new(EndpointConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        auth: AuthScheme::Bearer("token".to_owned()),
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    });
    let built = bearer_adapter
        .build_request(&minimal_request())
        .expect("build minimal request");
    let body = request_body(&built);

    assert_eq!(built.url().path(), "/chat/completions");
    assert_eq!(built.url().query(), None);
    assert_eq!(built.headers()[AUTHORIZATION], "Bearer token");
    assert_eq!(body["stream"], json!(false));
    assert!(body.get("temperature").is_none());
    assert!(body.get("tools").is_none());
    assert!(body.get("stream_options").is_none());

    let none_adapter = OpenAiChatAdapter::new(EndpointConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    });
    let built = none_adapter
        .build_request(&minimal_request())
        .expect("build none-auth request");
    assert!(!built.headers().contains_key(AUTHORIZATION));

    let malformed_url = OpenAiChatAdapter::new(EndpointConfig {
        base_url: "://not a URL".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    })
    .build_request(&minimal_request())
    .expect_err("malformed URL should fail before network use");
    assert!(matches!(malformed_url, ClientError::Other(_)));
    assert!(malformed_url.to_string().contains("invalid base URL"));

    let malformed_header = OpenAiChatAdapter::new(EndpointConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: vec![("bad\nheader".to_owned(), "value".to_owned())],
    })
    .build_request(&minimal_request())
    .expect_err("malformed header should fail before network use");
    assert!(matches!(malformed_header, ClientError::Other(_)));
    assert!(malformed_header.to_string().contains("invalid header name"));
}
