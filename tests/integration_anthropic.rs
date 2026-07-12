//! Opt-in integration coverage for the real Anthropic-compatible endpoint.

use agent_lib::{
    adapter::anthropic::AnthropicAdapter,
    client::{AuthScheme, ChatRequest, EndpointConfig},
    model::{
        content::ContentBlock,
        message::{Message, Role},
    },
};
use serde_json::Map;
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

/// Calls the configured Foundry Anthropic endpoint and validates text + usage.
#[tokio::test]
#[ignore = "requires ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN"]
async fn anthropic_non_streaming_hi_returns_text_and_usage() {
    let Some(base_url) = integration_env("ANTHROPIC_BASE_URL") else {
        return;
    };
    let Some(token) = integration_env("ANTHROPIC_AUTH_TOKEN") else {
        return;
    };
    let adapter = AnthropicAdapter::new(EndpointConfig {
        base_url,
        auth: AuthScheme::Bearer(token),
        query_params: Vec::new(),
        extra_headers: vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())],
    });
    let request = ChatRequest {
        model: "databricks-claude-haiku-4-5".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_owned(),
                extra: Map::new(),
            }],
        }],
        tools: Vec::new(),
        system: None,
        max_tokens: 32,
        temperature: None,
        stream: false,
        provider_extras: None,
    };

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
