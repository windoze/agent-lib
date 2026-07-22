//! Provider-neutral conversation scenarios executed through `dyn LlmClient`.

use super::{
    assertions::{assert_no_tool_call, assert_text_response, assert_weather_tool_call},
    config::IntegrationTarget,
};
use agent_lib::{
    client::{ChatRequest, Response},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        tool::{Tool, ToolStatus},
    },
};
use serde_json::{Map, json};

/// Runs text, multi-turn, and tool-result round-trip coverage for one client.
pub(super) async fn run_provider_suite(target: &IntegrationTarget) -> Result<(), String> {
    eprintln!(
        "running normalized conversation matrix through {}",
        target.label
    );
    if !target.client.capability().tool_calling {
        return Err(format!(
            "{} did not advertise tool calling through dyn LlmClient",
            target.label
        ));
    }

    run_text_scenario(target).await?;
    run_multi_turn_scenario(target).await?;
    run_tool_round_trip_scenario(target).await?;

    Ok(())
}

/// Verifies that a simple generation exposes common text, stop, and usage data.
async fn run_text_scenario(target: &IntegrationTarget) -> Result<(), String> {
    let request = chat_request(
        target,
        vec![text_message(
            Role::User,
            "Reply with exactly these two words: normalization ready",
        )],
        Vec::new(),
        Some("Follow the user's requested response format exactly."),
    );
    let response = execute_chat(target, "pure text", request).await?;

    assert_text_response(&response, &["normalization", "ready"])
        .map_err(|error| scenario_error(target, "pure text", error))
}

/// Replays the first assistant message and verifies shared multi-turn history.
async fn run_multi_turn_scenario(target: &IntegrationTarget) -> Result<(), String> {
    let system = "Keep track of facts supplied earlier in this conversation.";
    let mut messages = vec![text_message(
        Role::User,
        "Remember the code word amber-orbit. Reply with exactly: remembered",
    )];
    let first = execute_chat(
        target,
        "multi-turn first response",
        chat_request(target, messages.clone(), Vec::new(), Some(system)),
    )
    .await?;
    assert_text_response(&first, &["remembered"])
        .map_err(|error| scenario_error(target, "multi-turn first response", error))?;

    messages.push(first.message);
    messages.push(text_message(
        Role::User,
        "What code word did I ask you to remember? Reply with just the code word.",
    ));
    let second = execute_chat(
        target,
        "multi-turn follow-up",
        chat_request(target, messages, Vec::new(), Some(system)),
    )
    .await?;

    assert_text_response(&second, &["amber-orbit"])
        .map_err(|error| scenario_error(target, "multi-turn follow-up", error))
}

/// Executes a normalized tool call, feeds its result back, and checks the final
/// assistant response without changing behavior by provider.
async fn run_tool_round_trip_scenario(target: &IntegrationTarget) -> Result<(), String> {
    let system = concat!(
        "For a weather question, call get_weather exactly once before answering. ",
        "When a get_weather result is already present, do not call a tool again; ",
        "answer using that result."
    );
    let tools = vec![weather_tool()];
    let mut messages = vec![text_message(
        Role::User,
        "Use get_weather for the city Tokyo. Do not answer before using the tool.",
    )];
    let first = execute_chat(
        target,
        "tool request",
        chat_request(target, messages.clone(), tools.clone(), Some(system)),
    )
    .await?;
    let call = assert_weather_tool_call(&first)
        .map_err(|error| scenario_error(target, "tool request", error))?;

    messages.push(first.message);
    messages.push(tool_result_message(&call.id));
    let final_response = execute_chat(
        target,
        "tool result follow-up",
        chat_request(target, messages, tools, Some(system)),
    )
    .await?;
    assert_text_response(&final_response, &["tokyo", "sunny"])
        .map_err(|error| scenario_error(target, "tool result follow-up", error))?;
    assert_no_tool_call(&final_response)
        .map_err(|error| scenario_error(target, "tool result follow-up", error))
}

/// Calls the trait object, adding only diagnostic context to normalized errors.
async fn execute_chat(
    target: &IntegrationTarget,
    scenario: &str,
    request: ChatRequest,
) -> Result<Response, String> {
    target
        .client
        .chat(request)
        .await
        .map_err(|error| scenario_error(target, scenario, error.to_string()))
}

/// Builds a complete request whose only endpoint-specific value is the model.
fn chat_request(
    target: &IntegrationTarget,
    messages: Vec<Message>,
    tools: Vec<Tool>,
    system: Option<&str>,
) -> ChatRequest {
    ChatRequest {
        model: target.model.clone(),
        messages,
        tools,
        system: system.map(str::to_owned),
        max_tokens: 256,
        temperature: None,
        stream: false,
        provider_extras: None,
    }
}

/// Creates one normalized text message with an empty provider escape hatch.
fn text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
            extra: Map::new(),
        }],
    }
}

/// Defines the same strict JSON Schema tool for both provider adapters.
fn weather_tool() -> Tool {
    Tool {
        name: "get_weather".to_owned(),
        description: "Get the current weather for one city.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"],
            "additionalProperties": false
        }),
    }
}

/// Simulates successful tool execution while preserving the observed call id.
fn tool_result_message(tool_call_id: &str) -> Message {
    Message {
        role: Role::Tool,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_call_id.to_owned(),
            content: vec![ContentBlock::Text {
                text: "Tokyo weather is sunny at 24 C.".to_owned(),
                extra: Map::new(),
            }],
            status: ToolStatus::Ok,
            extra: Map::new(),
        }],
    }
}

/// Prefixes shared assertion and transport failures with useful test context.
fn scenario_error(target: &IntegrationTarget, scenario: &str, error: String) -> String {
    format!("{} {scenario} scenario failed: {error}", target.label)
}
