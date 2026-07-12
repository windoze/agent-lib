//! Opt-in integration coverage for the real Anthropic-compatible endpoint.

use agent_lib::{
    adapter::anthropic::AnthropicAdapter,
    client::{AuthScheme, ChatRequest, EndpointConfig},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::Tool,
    },
    stream::{BlockKind, Delta, StreamEvent, accumulator::Accumulator},
};
use futures::TryStreamExt;
use serde_json::{Map, json};
use std::{env, time::Duration};
use tokio::time::timeout;

/// Reads a required integration-test variable without exposing its value.
fn integration_env(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(_) => {
            eprintln!("skipping Anthropic integration test: {name} is not set");
            None
        }
    }
}

/// Builds the real Foundry adapter when both required variables are present.
fn integration_adapter() -> Option<AnthropicAdapter> {
    let base_url = integration_env("ANTHROPIC_BASE_URL")?;
    let token = integration_env("ANTHROPIC_AUTH_TOKEN")?;

    Some(AnthropicAdapter::new(EndpointConfig {
        base_url,
        auth: AuthScheme::Bearer(token),
        query_params: Vec::new(),
        extra_headers: vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())],
    }))
}

/// Constructs a text-only request while making streaming mode explicit.
fn text_request(prompt: &str, max_tokens: u32, stream: bool) -> ChatRequest {
    ChatRequest {
        model: "databricks-claude-haiku-4-5".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: prompt.to_owned(),
                extra: Map::new(),
            }],
        }],
        tools: Vec::new(),
        system: None,
        max_tokens,
        temperature: None,
        stream,
        provider_extras: None,
    }
}

/// Folds a captured event list without introducing a second aggregation path.
fn fold_events(events: &[StreamEvent]) -> Result<agent_lib::client::Response, String> {
    let mut accumulator = Accumulator::new();
    for event in events {
        accumulator
            .push(event.clone())
            .map_err(|error| error.to_string())?;
    }
    accumulator.finish().map_err(|error| error.to_string())
}

/// Calls the configured Foundry Anthropic endpoint and validates text + usage.
#[tokio::test]
#[ignore = "requires ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN"]
async fn anthropic_non_streaming_hi_returns_text_and_usage() {
    let Some(adapter) = integration_adapter() else {
        return;
    };
    let request = text_request("hi", 32, false);

    let response = timeout(Duration::from_secs(55), adapter.chat(request))
        .await
        .expect("Anthropic integration call exceeded 55 seconds")
        .expect("Anthropic integration call failed");

    let text = response.message.content.iter().find_map(|block| {
        if let ContentBlock::Text { text, .. } = block {
            Some(text.as_str())
        } else {
            None
        }
    });
    assert!(text.is_some_and(|text| !text.trim().is_empty()));
    assert!(response.usage.input > 0, "input usage should be reported");
    assert!(response.usage.output > 0, "output usage should be reported");
}

/// Calls the real SSE endpoint and validates text events plus shared folding.
#[tokio::test]
#[ignore = "requires ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN"]
async fn anthropic_streaming_count_emits_text_events_and_folds() {
    let Some(adapter) = integration_adapter() else {
        return;
    };
    let request = text_request("Count from 1 to 5, with one number per line.", 64, true);

    let (events, response) = timeout(Duration::from_secs(55), async {
        let stream = adapter
            .chat_stream(request)
            .await
            .map_err(|error| error.to_string())?;
        let events = stream
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| error.to_string())?;
        let response = fold_events(&events)?;
        Ok::<_, String>((events, response))
    })
    .await
    .expect("Anthropic streaming text call exceeded 55 seconds")
    .expect("Anthropic streaming text call failed");

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
            delta: Delta::Text(_),
            ..
        }
    )));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::MessageStop { .. })
    ));

    let text = response
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    for number in ["1", "2", "3", "4", "5"] {
        assert!(
            text.contains(number),
            "streamed text should contain {number}"
        );
    }
    assert_eq!(response.stop_reason.value, StopReason::EndTurn);
    assert!(response.usage.input > 0);
    assert!(response.usage.output > 0);
}

/// Calls the real SSE endpoint and validates raw + complete weather-tool input.
#[tokio::test]
#[ignore = "requires ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN"]
async fn anthropic_streaming_weather_tool_preserves_tokyo_input() {
    let Some(adapter) = integration_adapter() else {
        return;
    };
    let mut request = text_request(
        "What is the weather in Tokyo? You must use get_weather.",
        128,
        true,
    );
    request.tools = vec![Tool {
        name: "get_weather".to_owned(),
        description: "Get current weather for a city".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    }];

    let (events, response) = timeout(Duration::from_secs(55), async {
        let stream = adapter
            .chat_stream(request)
            .await
            .map_err(|error| error.to_string())?;
        let events = stream
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| error.to_string())?;
        let response = fold_events(&events)?;
        Ok::<_, String>((events, response))
    })
    .await
    .expect("Anthropic streaming tool call exceeded 55 seconds")
    .expect("Anthropic streaming tool call failed");

    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::BlockStart {
            kind: BlockKind::ToolInput { tool_name, .. },
            ..
        } if tool_name == "get_weather"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::BlockDelta {
            delta: Delta::Json(_),
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolInputAvailable { input, .. }
            if input.get("city").and_then(serde_json::Value::as_str) == Some("Tokyo")
    )));

    let tool = response
        .message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::ToolUse {
                name, input, id, ..
            } if name == "get_weather" => Some((id, input)),
            _ => None,
        });
    let (tool_id, input) = tool.expect("folded response should contain get_weather");
    assert!(!tool_id.is_empty());
    assert_eq!(input["city"], json!("Tokyo"));
    assert_eq!(response.stop_reason.value, StopReason::ToolUse);
    assert!(response.usage.input > 0);
    assert!(response.usage.output > 0);
}
