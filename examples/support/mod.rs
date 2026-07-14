//! Shared runtime configuration and provider-neutral builders for the examples.
//!
//! Each example uses a different subset of these helpers, so unused-function
//! warnings here are expected for any single example binary.
#![allow(dead_code)]

use agent_lib::{
    adapter::{anthropic::AnthropicAdapter, openai_resp::OpenAiRespAdapter},
    client::{AuthScheme, ChatRequest, EndpointConfig, LlmClient, Response},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        tool::Tool,
    },
};
use serde_json::Map;
use std::{env, error::Error, io, time::Duration};

/// Error type shared by the standalone example binaries.
pub type ExampleResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// One environment-selected client and its endpoint-specific model name.
pub struct ExampleTarget {
    /// Human-readable provider label used in example output.
    pub label: &'static str,
    /// Model or deployment name sent in normalized requests.
    pub model: String,
    /// Provider-neutral dynamic client used by the example workflow.
    pub client: Box<dyn LlmClient>,
}

/// Builds the provider selected by `AGENT_LIB_PROVIDER` from environment data.
pub fn configured_target() -> ExampleResult<ExampleTarget> {
    let provider = required_env("AGENT_LIB_PROVIDER")?.to_ascii_lowercase();
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()?;

    match provider.as_str() {
        "anthropic" => anthropic_target(http_client),
        "openai" | "openai-responses" | "openai_resp" => openai_target(http_client),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "AGENT_LIB_PROVIDER must be `anthropic` or `openai`",
        )
        .into()),
    }
}

/// Creates a normalized request for an ordered message history.
pub fn chat_request(
    target: &ExampleTarget,
    messages: Vec<Message>,
    tools: Vec<Tool>,
    system: Option<&str>,
    stream: bool,
) -> ChatRequest {
    ChatRequest {
        model: target.model.clone(),
        messages,
        tools,
        system: system.map(str::to_owned),
        max_tokens: 256,
        temperature: None,
        stream,
        provider_extras: None,
    }
}

/// Creates one provider-neutral text message with no provider extras.
pub fn text_message(role: Role, text: impl Into<String>) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.into(),
            extra: Map::new(),
        }],
    }
}

/// Concatenates assistant-visible text while omitting reasoning and tool data.
pub fn response_text(response: &Response) -> String {
    response
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Constructs the Anthropic Messages adapter from its documented variables.
fn anthropic_target(http_client: reqwest::Client) -> ExampleResult<ExampleTarget> {
    let endpoint = EndpointConfig {
        base_url: required_env("ANTHROPIC_BASE_URL")?,
        auth: AuthScheme::Bearer(required_env("ANTHROPIC_AUTH_TOKEN")?),
        query_params: Vec::new(),
        extra_headers: vec![(
            "anthropic-version".to_owned(),
            optional_env("ANTHROPIC_VERSION", "2023-06-01"),
        )],
    };

    Ok(ExampleTarget {
        label: "Anthropic Messages",
        model: optional_env("ANTHROPIC_MODEL", "databricks-claude-haiku-4-5"),
        client: Box::new(AnthropicAdapter::with_http_client(endpoint, http_client)),
    })
}

/// Constructs the OpenAI Responses adapter from its documented variables.
fn openai_target(http_client: reqwest::Client) -> ExampleResult<ExampleTarget> {
    let endpoint = EndpointConfig {
        base_url: required_env("OPENAI_BASE_URL")?,
        auth: AuthScheme::Header {
            name: "api-key".to_owned(),
            value: required_env("OPENAI_API_KEY")?,
        },
        query_params: vec![(
            "api-version".to_owned(),
            optional_env("OPENAI_API_VERSION", "2025-04-01-preview"),
        )],
        extra_headers: Vec::new(),
    };

    Ok(ExampleTarget {
        label: "OpenAI Responses",
        model: optional_env("OPENAI_MODEL", "gpt-5.5"),
        client: Box::new(OpenAiRespAdapter::with_http_client(endpoint, http_client)),
    })
}

/// Reads a required non-empty variable without exposing its value in errors.
fn required_env(name: &str) -> io::Result<String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) | Err(_) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("required environment variable {name} is not set"),
        )),
    }
}

/// Reads a non-empty optional variable or returns the tested default value.
fn optional_env(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}
