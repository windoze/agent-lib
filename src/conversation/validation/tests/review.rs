//! Milestone review tests spanning the validator and both request mappers.

use super::fixtures::{
    assert_closed_invariants, conversation, draft, image, message, pairing, text, tool_result,
    tool_use,
};
use crate::{
    adapter::{anthropic::AnthropicAdapter, openai_resp::OpenAiRespAdapter},
    client::{AuthScheme, ChatRequest, EndpointConfig},
    model::{content::ContentBlock, message::Role, tool::Tool},
};
use serde_json::{Map, Value, json};

/// Builds caller-owned transport configuration without enabling network I/O.
fn endpoint() -> EndpointConfig {
    EndpointConfig {
        base_url: "https://example.test".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    }
}

/// Builds a complete reasoning block accepted in the assistant role.
fn thinking(value: &str) -> ContentBlock {
    ContentBlock::Thinking {
        text: value.to_owned(),
        signature: Some(format!("signature-{value}")),
        extra: Map::new(),
    }
}

/// Adds multimodal output to a complete tool-result fixture.
fn multimodal_tool_result(provider_call_id: &str) -> ContentBlock {
    let mut result = tool_result(provider_call_id);
    let ContentBlock::ToolResult { content, .. } = &mut result else {
        unreachable!("the fixture always returns a tool-result block")
    };
    content.push(image());
    result
}

/// Decodes the buffered JSON body produced by a request mapper.
fn request_body(request: &reqwest::Request) -> Value {
    let bytes = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .expect("JSON request body is buffered");
    serde_json::from_slice(bytes).expect("request body is valid JSON")
}

/// Counts top-level wire items with one modeled type discriminator.
fn count_items(items: &[Value], item_type: &str) -> usize {
    items
        .iter()
        .filter(|item| item.get("type") == Some(&Value::String(item_type.to_owned())))
        .count()
}

#[test]
fn one_validated_canonical_turn_is_accepted_by_both_request_mappers() {
    let mut conversation = conversation();
    let candidate = draft(
        10,
        None,
        vec![
            message(100, Role::User, vec![text("compare two sources"), image()]),
            message(
                101,
                Role::Assistant,
                vec![
                    thinking("plan"),
                    text("checking both"),
                    tool_use("parallel-a"),
                    tool_use("parallel-b"),
                ],
            ),
            message(102, Role::Tool, vec![multimodal_tool_result("parallel-a")]),
            message(103, Role::Tool, vec![tool_result("parallel-b")]),
            message(
                104,
                Role::Assistant,
                vec![thinking("synthesize"), text("combined answer")],
            ),
        ],
        vec![
            pairing(500, "parallel-a", 101, 102),
            pairing(501, "parallel-b", 101, 103),
        ],
    );

    conversation
        .commit_draft(candidate)
        .expect("canonical turn passes the sole validator");
    let turn = &conversation.turns()[0];
    assert_closed_invariants(turn);

    let messages = turn
        .messages()
        .iter()
        .map(|message| message.payload().clone())
        .collect::<Vec<_>>();
    let original_messages = messages.clone();
    let request = ChatRequest {
        model: "review-model".to_owned(),
        messages,
        tools: vec![Tool {
            name: "lookup".to_owned(),
            description: "Look up one source.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        }],
        system: conversation.config().system().map(str::to_owned),
        max_tokens: 128,
        temperature: None,
        stream: false,
        provider_extras: None,
    };

    let anthropic = AnthropicAdapter::new(endpoint())
        .build_request(&request)
        .expect("Anthropic mapper accepts canonical history");
    let openai = OpenAiRespAdapter::new(endpoint())
        .build_request(&request)
        .expect("OpenAI Responses mapper accepts canonical history");

    assert_eq!(
        request.messages, original_messages,
        "mapping must not alter immutable payloads"
    );

    let anthropic_body = request_body(&anthropic);
    assert_eq!(anthropic_body["system"], json!("Answer precisely."));
    let anthropic_messages = anthropic_body["messages"]
        .as_array()
        .expect("Anthropic messages array");
    assert!(
        anthropic_messages.iter().all(|message| {
            matches!(message["role"].as_str(), Some("user") | Some("assistant"))
        })
    );
    let anthropic_blocks = anthropic_messages
        .iter()
        .flat_map(|message| {
            message["content"]
                .as_array()
                .expect("Anthropic content array")
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(count_items(&anthropic_blocks, "tool_use"), 2);
    assert_eq!(count_items(&anthropic_blocks, "tool_result"), 2);

    let openai_body = request_body(&openai);
    assert_eq!(openai_body["instructions"], json!("Answer precisely."));
    let openai_items = openai_body["input"]
        .as_array()
        .expect("OpenAI Responses input array");
    assert!(openai_items.iter().all(|item| item["role"] != "system"));
    assert_eq!(count_items(openai_items, "function_call"), 2);
    assert_eq!(count_items(openai_items, "function_call_output"), 2);
}
