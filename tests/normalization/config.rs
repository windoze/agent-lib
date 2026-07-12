//! Real endpoint construction kept separate from provider-neutral scenarios.

use agent_lib::{
    adapter::{anthropic::AnthropicAdapter, openai_resp::OpenAiRespAdapter},
    client::{AuthScheme, EndpointConfig, LlmClient},
};
use std::{env, time::Duration};

/// One runtime-selected client and its endpoint-specific model identifier.
pub(super) struct IntegrationTarget {
    /// Human-readable context used only in diagnostics.
    pub(super) label: &'static str,
    /// Deployment name placed into otherwise provider-neutral requests.
    pub(super) model: &'static str,
    /// Dynamic client used by every scenario after endpoint construction.
    pub(super) client: Box<dyn LlmClient>,
}

/// Supported real endpoints in the deterministic matrix order.
#[derive(Clone, Copy)]
enum Provider {
    Anthropic,
    OpenAiResponses,
}

/// Builds all targets whose complete credential sets are available.
pub(super) fn configured_targets() -> Result<Vec<IntegrationTarget>, String> {
    [Provider::Anthropic, Provider::OpenAiResponses]
        .into_iter()
        .filter_map(build_target)
        .collect()
}

/// Selects only endpoint construction details; scenarios never inspect this
/// provider discriminator or branch on provider behavior.
fn build_target(provider: Provider) -> Option<Result<IntegrationTarget, String>> {
    match provider {
        Provider::Anthropic => build_anthropic_target(),
        Provider::OpenAiResponses => build_openai_target(),
    }
}

/// Creates the Anthropic Messages dynamic client when both variables exist.
fn build_anthropic_target() -> Option<Result<IntegrationTarget, String>> {
    let base_url = integration_env("Anthropic", "ANTHROPIC_BASE_URL")?;
    let token = integration_env("Anthropic", "ANTHROPIC_AUTH_TOKEN")?;
    let endpoint = EndpointConfig {
        base_url,
        auth: AuthScheme::Bearer(token),
        query_params: Vec::new(),
        extra_headers: vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())],
    };

    Some(
        integration_http_client().map(|http_client| IntegrationTarget {
            label: "Anthropic Messages",
            model: "databricks-claude-haiku-4-5",
            client: Box::new(AnthropicAdapter::with_http_client(endpoint, http_client)),
        }),
    )
}

/// Creates the OpenAI Responses dynamic client when both variables exist.
fn build_openai_target() -> Option<Result<IntegrationTarget, String>> {
    let base_url = integration_env("OpenAI Responses", "OPENAI_BASE_URL")?;
    let api_key = integration_env("OpenAI Responses", "OPENAI_API_KEY")?;
    let endpoint = EndpointConfig {
        base_url,
        auth: AuthScheme::Header {
            name: "api-key".to_owned(),
            value: api_key,
        },
        query_params: vec![("api-version".to_owned(), "2025-04-01-preview".to_owned())],
        extra_headers: Vec::new(),
    };

    Some(
        integration_http_client().map(|http_client| IntegrationTarget {
            label: "OpenAI Responses",
            model: "gpt-5.5",
            client: Box::new(OpenAiRespAdapter::with_http_client(endpoint, http_client)),
        }),
    )
}

/// Reads a secret-bearing environment variable without printing its value.
fn integration_env(provider: &str, name: &str) -> Option<String> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(_) => {
            eprintln!("skipping {provider} normalization scenarios: {name} is not set");
            None
        }
    }
}

/// Gives transport calls a deadline below the test's overall 55-second cap.
fn integration_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|error| format!("failed to build integration HTTP client: {error}"))
}
