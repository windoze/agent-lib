//! Opt-in integration coverage for the real OpenAI Responses Foundry endpoint.

use agent_lib::{
    adapter::openai_resp::OpenAiRespAdapter,
    client::{AuthScheme, ChatRequest, EndpointConfig},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
    },
};
use serde_json::Map;
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
    assert_eq!(response.stop_reason.value, StopReason::EndTurn);
    assert!(response.usage.input > 0);
    assert!(response.usage.output > 0);
    assert!(response.extra.contains_key("content_filters"));
}
