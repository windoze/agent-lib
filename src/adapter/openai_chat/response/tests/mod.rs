//! Complete-response parsing and non-streaming transport tests.

use super::*;
use crate::{
    adapter::openai_chat::RESPONSE_EXTRA_KEY,
    client::{AuthScheme, EndpointConfig},
    model::{content::ContentBlock, message::Message},
};
use serde_json::Map;

/// Constructs a small valid non-streaming request for transport tests.
fn minimal_request() -> ChatRequest {
    ChatRequest {
        model: "deepseek-chat".to_owned(),
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

/// Sanitized recording of a plain-text chat/completions response.
const REAL_TEXT_RESPONSE: &str = include_str!("fixtures/text_response.json");

/// Sanitized recording of a tool-call chat/completions response.
const REAL_TOOL_RESPONSE: &str = include_str!("fixtures/tool_response.json");

/// Sanitized recording of a chat/completions response carrying `reasoning_content`.
const REAL_REASONING_RESPONSE: &str = include_str!("fixtures/reasoning_response.json");

mod parsing;
mod transport;
