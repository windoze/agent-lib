use super::*;
use crate::model::{extras::ProviderExtras, message::Message, tool::ToolStatus};
use reqwest::{Method, header::AUTHORIZATION};
use serde_json::{Map, json};

/// Creates an empty provider-field map for concise content fixtures.
fn empty_extra() -> Map<String, Value> {
    Map::new()
}

/// Returns a small valid request for transport-configuration tests.
fn minimal_request() -> ChatRequest {
    ChatRequest {
        model: "databricks-claude-haiku-4-5".to_owned(),
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

/// Decodes the in-memory JSON body produced by reqwest's request builder.
fn request_body(request: &Request) -> Value {
    let bytes = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .expect("JSON request body should be buffered");
    serde_json::from_slice(bytes).expect("request body should contain valid JSON")
}

#[test]
fn complete_chat_request_maps_to_anthropic_messages_wire_shape() {
    let endpoint = EndpointConfig {
        base_url: "https://anthropic.example.test/proxy/".to_owned(),
        auth: AuthScheme::Bearer("secret-token".to_owned()),
        query_params: vec![
            ("api-version".to_owned(), "2026-01-01".to_owned()),
            ("feature".to_owned(), "images".to_owned()),
            ("feature".to_owned(), "tools".to_owned()),
        ],
        extra_headers: vec![
            ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
            ("content-type".to_owned(), "application/json".to_owned()),
            ("x-trace-id".to_owned(), "trace-123".to_owned()),
        ],
    };
    let adapter = AnthropicAdapter::new(endpoint.clone());
    let request = ChatRequest {
        model: "databricks-claude-haiku-4-5".to_owned(),
        messages: vec![
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "What's the weather?".to_owned(),
                        extra: Map::from_iter([
                            ("type".to_owned(), json!("wrong")),
                            ("text".to_owned(), json!("wrong")),
                            ("cache_control".to_owned(), json!({ "type": "ephemeral" })),
                        ]),
                    },
                    ContentBlock::Image {
                        source: ImageSource::Url {
                            url: "https://example.test/weather.png".to_owned(),
                            extra: Map::from_iter([("source_hint".to_owned(), json!("remote"))]),
                        },
                        extra: Map::from_iter([("quality".to_owned(), json!("high"))]),
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
                    ContentBlock::Thinking {
                        text: "I should call the weather tool.".to_owned(),
                        signature: Some("sig-123".to_owned()),
                        extra: Map::from_iter([("provider_note".to_owned(), json!(true))]),
                    },
                    ContentBlock::ToolUse {
                        id: "toolu_123".to_owned(),
                        name: "get_weather".to_owned(),
                        input: json!({ "city": "Shanghai" }),
                        extra: Map::from_iter([(
                            "cache_control".to_owned(),
                            json!({ "type": "ephemeral" }),
                        )]),
                    },
                ],
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_123".to_owned(),
                    content: vec![ContentBlock::Text {
                        text: "Weather service unavailable".to_owned(),
                        extra: empty_extra(),
                    }],
                    status: ToolStatus::Error,
                    extra: Map::from_iter([("provider_status".to_owned(), json!(503))]),
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
        stream: true,
        provider_extras: Some(ProviderExtras {
            provider: ProviderId::Anthropic,
            fields: Map::from_iter([
                ("top_k".to_owned(), json!(20)),
                ("metadata".to_owned(), json!({ "user_id": "user-123" })),
            ]),
        }),
    };

    let built = adapter
        .build_request(&request)
        .expect("build Anthropic request");

    assert_eq!(adapter.endpoint(), &endpoint);
    assert_eq!(built.method(), Method::POST);
    assert_eq!(built.url().path(), "/proxy/v1/messages");
    assert_eq!(
        built
            .url()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>(),
        vec![
            ("api-version".to_owned(), "2026-01-01".to_owned()),
            ("feature".to_owned(), "images".to_owned()),
            ("feature".to_owned(), "tools".to_owned()),
        ]
    );
    assert_eq!(
        built.headers()[AUTHORIZATION].to_str().unwrap(),
        "Bearer secret-token"
    );
    assert_eq!(built.headers()["anthropic-version"], "2023-06-01");
    assert_eq!(built.headers()[CONTENT_TYPE], "application/json");
    assert_eq!(built.headers()["x-trace-id"], "trace-123");
    assert_eq!(
        request_body(&built),
        json!({
            "model": "databricks-claude-haiku-4-5",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "text",
                            "text": "What's the weather?",
                            "cache_control": { "type": "ephemeral" }
                        },
                        {
                            "type": "image",
                            "source": {
                                "type": "url",
                                "url": "https://example.test/weather.png",
                                "source_hint": "remote"
                            },
                            "quality": "high"
                        },
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": "iVBORw0KGgo="
                            }
                        }
                    ]
                },
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "thinking",
                            "thinking": "I should call the weather tool.",
                            "signature": "sig-123",
                            "provider_note": true
                        },
                        {
                            "type": "tool_use",
                            "id": "toolu_123",
                            "name": "get_weather",
                            "input": { "city": "Shanghai" },
                            "cache_control": { "type": "ephemeral" }
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "toolu_123",
                            "content": [
                                {
                                    "type": "text",
                                    "text": "Weather service unavailable"
                                }
                            ],
                            "is_error": true,
                            "provider_status": 503
                        }
                    ]
                }
            ],
            "system": "Answer concisely.",
            "max_tokens": 1024,
            "tools": [
                {
                    "name": "get_weather",
                    "description": "Get current weather for a city.",
                    "input_schema": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                        "required": ["city"]
                    }
                }
            ],
            "temperature": 0.25,
            "stream": true,
            "top_k": 20,
            "metadata": { "user_id": "user-123" }
        })
    );
}

#[test]
fn each_tool_status_maps_to_anthropic_error_boolean_without_mutating_source() {
    for (status, expected_is_error) in [
        (ToolStatus::Ok, None),
        (ToolStatus::Error, Some(true)),
        (ToolStatus::Denied, Some(true)),
        (ToolStatus::Cancelled, Some(true)),
    ] {
        let block = ContentBlock::ToolResult {
            tool_use_id: "toolu_123".to_owned(),
            content: vec![ContentBlock::Text {
                text: "result".to_owned(),
                extra: empty_extra(),
            }],
            status,
            extra: Map::from_iter([
                ("is_error".to_owned(), json!(false)),
                ("status".to_owned(), json!("wrong")),
                ("provider_trace".to_owned(), json!("trace-1")),
            ]),
        };
        let original = block.clone();
        let wire = content_to_wire(&block);

        assert_eq!(
            wire.get("is_error").and_then(Value::as_bool),
            expected_is_error
        );
        assert!(wire.get("status").is_none());
        assert_eq!(wire["provider_trace"], json!("trace-1"));
        assert_eq!(block, original);
    }
}

#[test]
fn optional_fields_are_omitted_and_header_auth_is_applied() {
    let endpoint = EndpointConfig {
        base_url: "https://anthropic.example.test/gateway".to_owned(),
        auth: AuthScheme::Header {
            name: "x-api-key".to_owned(),
            value: "direct-key".to_owned(),
        },
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    };
    let built = AnthropicAdapter::new(endpoint)
        .build_request(&minimal_request())
        .expect("build minimal request");
    let body = request_body(&built);

    assert_eq!(built.url().path(), "/gateway/v1/messages");
    assert_eq!(built.url().query(), None);
    assert_eq!(built.headers()["x-api-key"], "direct-key");
    assert_eq!(built.headers()[CONTENT_TYPE], "application/json");
    assert!(!built.headers().contains_key(AUTHORIZATION));
    assert_eq!(body["stream"], json!(false));
    assert!(body.get("system").is_none());
    assert!(body.get("tools").is_none());
    assert!(body.get("temperature").is_none());
}

#[test]
fn no_auth_sends_no_authentication_header() {
    let endpoint = EndpointConfig {
        base_url: "http://localhost:8080".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    };
    let built = AnthropicAdapter::new(endpoint)
        .build_request(&minimal_request())
        .expect("build unauthenticated request");

    assert!(!built.headers().contains_key(AUTHORIZATION));
    assert!(!built.headers().contains_key("x-api-key"));
}

#[test]
fn system_role_in_messages_is_rejected_in_favor_of_system_field() {
    let mut chat = minimal_request();
    chat.messages.insert(
        0,
        Message {
            role: Role::System,
            content: vec![ContentBlock::Text {
                text: "Do not put this in messages.".to_owned(),
                extra: empty_extra(),
            }],
        },
    );

    let error = serialize_body(&chat).expect_err("system message role should be invalid");

    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("message 0 has system role"));
    assert!(error.to_string().contains("ChatRequest.system"));
}

#[test]
fn extras_for_another_provider_are_rejected_observably() {
    let mut chat = minimal_request();
    chat.provider_extras = Some(ProviderExtras {
        provider: ProviderId::OpenAiResp,
        fields: Map::from_iter([("reasoning".to_owned(), json!({ "effort": "high" }))]),
    });

    let error = serialize_body(&chat).expect_err("foreign extras should not be discarded");

    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("OpenAiResp"));
    assert!(error.to_string().contains("Anthropic"));
}

#[test]
fn malformed_endpoint_values_return_errors_before_network_use() {
    let malformed_url = EndpointConfig {
        base_url: "://not a URL".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    };
    let url_error = AnthropicAdapter::new(malformed_url)
        .build_request(&minimal_request())
        .expect_err("malformed URL should fail request construction");
    assert!(matches!(url_error, ClientError::Other(_)));
    assert!(url_error.to_string().contains("invalid base URL"));

    let malformed_header = EndpointConfig {
        base_url: "https://anthropic.example.test".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: vec![("bad\nheader".to_owned(), "value".to_owned())],
    };
    let header_error = AnthropicAdapter::new(malformed_header)
        .build_request(&minimal_request())
        .expect_err("malformed header should fail request construction");
    assert!(matches!(header_error, ClientError::Other(_)));
    assert!(header_error.to_string().contains("invalid header name"));
}
