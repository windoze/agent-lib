//! Cross-provider acceptance tests for documented capabilities and raw response evidence.

use agent_lib::{
    adapter::{anthropic::AnthropicAdapter, openai_resp::OpenAiRespAdapter},
    client::{
        ANTHROPIC_DEFAULT_CAPABILITY, AuthScheme, EndpointConfig, LlmClient,
        OPENAI_RESP_DEFAULT_CAPABILITY, Response,
    },
};
use serde_json::Value;

const ANTHROPIC_TEXT_RESPONSE: &[u8] =
    include_bytes!("../src/adapter/anthropic/response/tests/fixtures/text_response.json");
const OPENAI_TEXT_RESPONSE: &[u8] =
    include_bytes!("../src/adapter/openai_resp/response/tests/fixtures/text_response.json");

/// Builds an inert endpoint configuration for capability inspection without network access.
fn offline_endpoint() -> EndpointConfig {
    EndpointConfig {
        base_url: "https://example.invalid".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    }
}

/// Round-trips a normalized response to prove its escape-hatch evidence is persistent data.
fn round_trip(response: &Response) -> Response {
    let encoded = serde_json::to_vec(response).expect("serialize normalized response");
    serde_json::from_slice(&encoded).expect("deserialize normalized response")
}

/// Ensures each concrete client advertises the protocol default documented in the matrix.
#[test]
fn adapters_expose_their_documented_protocol_defaults() {
    let anthropic: Box<dyn LlmClient> = Box::new(AnthropicAdapter::new(offline_endpoint()));
    let openai: Box<dyn LlmClient> = Box::new(OpenAiRespAdapter::new(offline_endpoint()));

    assert_eq!(anthropic.capability(), &*ANTHROPIC_DEFAULT_CAPABILITY);
    assert_eq!(openai.capability(), &*OPENAI_RESP_DEFAULT_CAPABILITY);
}

/// Proves Foundry's nested Anthropic cache-creation details are retained exactly.
#[test]
fn anthropic_cache_creation_details_survive_normalization_and_serde() {
    let raw: Value =
        serde_json::from_slice(ANTHROPIC_TEXT_RESPONSE).expect("parse raw Anthropic fixture");
    let expected = raw
        .pointer("/usage/cache_creation")
        .expect("fixture should contain cache creation details");

    let response = AnthropicAdapter::parse_response(ANTHROPIC_TEXT_RESPONSE)
        .expect("normalize Anthropic fixture");
    assert_eq!(response.usage.extra.get("cache_creation"), Some(expected));
    assert_eq!(response.usage.cache_write, 0);
    assert_eq!(response.usage.cache_read, 0);

    let restored = round_trip(&response);
    assert_eq!(restored.usage.extra.get("cache_creation"), Some(expected));
}

/// Proves Azure's complete content-filter evidence is retained exactly at response scope.
#[test]
fn azure_content_filters_survive_normalization_and_serde() {
    let raw: Value =
        serde_json::from_slice(OPENAI_TEXT_RESPONSE).expect("parse raw OpenAI fixture");
    let expected = raw
        .get("content_filters")
        .expect("fixture should contain Azure content filters");

    let response =
        OpenAiRespAdapter::parse_response(OPENAI_TEXT_RESPONSE).expect("normalize OpenAI fixture");
    assert_eq!(response.extra.get("content_filters"), Some(expected));
    assert_eq!(response.usage.cache_read, 4);
    assert_eq!(response.usage.reasoning, 18);

    let restored = round_trip(&response);
    assert_eq!(restored.extra.get("content_filters"), Some(expected));
}
