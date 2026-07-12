//! Focused tests for Anthropic complete-response parsing and transport.

use super::*;
use crate::{
    client::{AuthScheme, EndpointConfig},
    model::message::Message,
};
use serde_json::Map;

mod parsing;
mod transport;

/// Real Foundry greeting response captured on 2026-07-13, with its id redacted.
const REAL_TEXT_RESPONSE: &str = include_str!("fixtures/text_response.json");

/// Real Foundry tool response captured on 2026-07-13, with ids redacted.
const REAL_TOOL_RESPONSE: &str = include_str!("fixtures/tool_response.json");

/// Constructs a small valid non-streaming request for transport tests.
fn minimal_request() -> ChatRequest {
    ChatRequest {
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
    }
}

/// Builds an unauthenticated local endpoint configuration for mock servers.
fn local_endpoint(base_url: String) -> EndpointConfig {
    EndpointConfig {
        base_url,
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    }
}
