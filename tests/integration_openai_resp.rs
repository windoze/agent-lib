//! Opt-in integration coverage for the real OpenAI Responses Foundry endpoint.

use agent_lib::{
    adapter::openai_resp::OpenAiRespAdapter,
    client::{AuthScheme, ChatRequest, EndpointConfig},
    model::{
        content::ContentBlock,
        extras::{ProviderExtras, ProviderId},
        message::{Message, Role},
        normalized::StopReason,
        tool::Tool,
    },
    stream::{
        BlockKind, Delta, StreamEvent,
        accumulator::{Accumulator, AccumulatorError},
    },
};
use futures::TryStreamExt;
use serde_json::{Map, json};
use std::time::Duration;
use tokio::time::timeout;

/// Creates an adapter only when both endpoint environment variables exist.
fn integration_adapter() -> Option<OpenAiRespAdapter> {
    let base_url = match std::env::var("OPENAI_BASE_URL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping: OPENAI_BASE_URL is not configured");
            return None;
        }
    };
    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping: OPENAI_API_KEY is not configured");
            return None;
        }
    };
    let endpoint = EndpointConfig {
        base_url,
        auth: AuthScheme::Header {
            name: "api-key".to_owned(),
            value: api_key,
        },
        query_params: vec![("api-version".to_owned(), "2025-04-01-preview".to_owned())],
        extra_headers: Vec::new(),
    };
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(50))
        .build()
        .expect("build integration HTTP client");

    Some(OpenAiRespAdapter::with_http_client(endpoint, http_client))
}

/// Builds one provider-neutral text request for the real endpoint.
fn text_request(prompt: &str) -> ChatRequest {
    ChatRequest {
        model: "gpt-5.5".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: prompt.to_owned(),
                extra: Map::new(),
            }],
        }],
        tools: Vec::new(),
        system: None,
        max_tokens: 128,
        temperature: None,
        stream: false,
        provider_extras: None,
    }
}

/// Builds a forced weather-tool request so the real stream deterministically
/// exercises function-call arguments.
fn tool_request() -> ChatRequest {
    ChatRequest {
        model: "gpt-5.5".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Use get_weather for Tokyo. Do not answer directly.".to_owned(),
                extra: Map::new(),
            }],
        }],
        tools: vec![Tool {
            name: "get_weather".to_owned(),
            description: "Get weather for a city".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                },
                "required": ["city"],
                "additionalProperties": false
            }),
        }],
        system: None,
        max_tokens: 128,
        temperature: None,
        stream: true,
        provider_extras: Some(ProviderExtras {
            provider: ProviderId::OpenAiResp,
            fields: Map::from_iter([(
                "tool_choice".to_owned(),
                json!({ "type": "function", "name": "get_weather" }),
            )]),
        }),
    }
}

/// Folds one already collected event vector through the shared accumulator.
fn fold_events(events: &[StreamEvent]) -> Result<agent_lib::client::Response, AccumulatorError> {
    let mut accumulator = Accumulator::new();
    for event in events {
        accumulator.push(event.clone())?;
    }
    accumulator.finish()
}

/// Starts and fully consumes one real SSE request under the per-test time
/// limit required for integration calls.
async fn collect_stream(adapter: &OpenAiRespAdapter, request: ChatRequest) -> Vec<StreamEvent> {
    timeout(Duration::from_secs(55), async {
        adapter
            .chat_stream(request)
            .await
            .expect("OpenAI Responses stream failed to start")
            .try_collect::<Vec<_>>()
            .await
            .expect("OpenAI Responses stream failed while decoding")
    })
    .await
    .expect("OpenAI Responses streaming call exceeded 55 seconds")
}

/// Calls the configured real endpoint and validates normalized text and usage.
#[tokio::test]
#[ignore = "requires OPENAI_BASE_URL and OPENAI_API_KEY"]
async fn openai_responses_non_streaming_text_returns_content_and_usage() {
    let Some(adapter) = integration_adapter() else {
        return;
    };

    let response = timeout(
        Duration::from_secs(55),
        adapter.chat(text_request("Say hi in exactly two words.")),
    )
    .await
    .expect("OpenAI Responses non-streaming call exceeded 55 seconds")
    .expect("OpenAI Responses non-streaming call failed");

    let text = response
        .message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } if !text.is_empty() => Some(text),
            _ => None,
        });
    assert!(text.is_some(), "response should contain non-empty text");
    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(*response.stop_reason.value(), StopReason::EndTurn);
    assert!(response.usage.input > 0);
    assert!(response.usage.output > 0);
    assert!(response.extra.contains_key("content_filters"));
}

/// Calls the real streaming endpoint and validates text events plus the shared
/// accumulator result.
#[tokio::test]
#[ignore = "requires OPENAI_BASE_URL and OPENAI_API_KEY"]
async fn openai_responses_streaming_text_yields_foldable_events() {
    let Some(adapter) = integration_adapter() else {
        return;
    };
    let mut request = text_request("Reply with exactly: hi there");
    request.stream = true;

    let events = collect_stream(&adapter, request).await;
    assert!(matches!(
        events.first(),
        Some(StreamEvent::MessageStart {
            role: Role::Assistant
        })
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::BlockStart {
            kind: BlockKind::Text,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::BlockDelta {
            delta: Delta::Text(text),
            ..
        } if !text.is_empty()
    )));
    assert!(events.iter().any(
        |event| matches!(event, StreamEvent::Usage(usage) if usage.input > 0 && usage.output > 0)
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ResponseMetadata { extra } if extra.contains_key("content_filters")
    )));

    let response = fold_events(&events).expect("fold real text stream");
    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(*response.stop_reason.value(), StopReason::EndTurn);
    assert!(response.message.content.iter().any(|block| matches!(
        block,
        ContentBlock::Text { text, .. } if !text.is_empty()
    )));
    assert!(response.extra.contains_key("content_filters"));
}

/// Calls the real streaming endpoint with a forced function and validates raw
/// argument fragments, the complete-input boundary, and normalized tool use.
#[tokio::test]
#[ignore = "requires OPENAI_BASE_URL and OPENAI_API_KEY"]
async fn openai_responses_streaming_tool_call_yields_complete_input() {
    let Some(adapter) = integration_adapter() else {
        return;
    };

    let events = collect_stream(&adapter, tool_request()).await;
    let tool_block = events.iter().find_map(|event| match event {
        StreamEvent::BlockStart {
            id,
            kind:
                BlockKind::ToolInput {
                    tool_name,
                    tool_call_id,
                },
        } => Some((id, tool_name, tool_call_id)),
        _ => None,
    });
    let (block_id, tool_name, tool_call_id) =
        tool_block.expect("real stream should start a tool-input block");
    assert_eq!(tool_name, "get_weather");
    assert!(!tool_call_id.is_empty());

    let arguments = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::BlockDelta {
                id,
                delta: Delta::Json(fragment),
            } if id == block_id => Some(fragment.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&arguments)
            .expect("real argument fragments should form complete JSON"),
        json!({ "city": "Tokyo" })
    );
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolInputAvailable { id, input }
            if id == block_id && input == &json!({ "city": "Tokyo" })
    )));

    let response = fold_events(&events).expect("fold real tool stream");
    assert_eq!(*response.stop_reason.value(), StopReason::ToolUse);
    assert!(response.message.content.iter().any(|block| matches!(
        block,
        ContentBlock::ToolUse { id, name, input, .. }
            if id == tool_call_id
                && name == "get_weather"
                && input == &json!({ "city": "Tokyo" })
    )));
    assert!(response.usage.input > 0);
    assert!(response.usage.output > 0);
}
