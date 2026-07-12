//! Executes a model-requested tool locally and sends its result back to the model.

mod support;

use agent_lib::model::{
    content::ContentBlock,
    message::{Message, Role},
    tool::{Tool, ToolStatus},
};
use serde_json::{Map, Value, json};
use std::io;
use support::{ExampleResult, chat_request, configured_target, response_text, text_message};

/// Runs the complete tool-call round-trip example.
#[tokio::main]
async fn main() -> ExampleResult<()> {
    let target = configured_target()?;
    eprintln!("running a tool round trip through {}", target.label);

    let system = concat!(
        "For a weather question, call get_weather exactly once before answering. ",
        "When a get_weather result is present, answer from it without another tool call."
    );
    let tools = vec![weather_tool()];
    let mut messages = vec![text_message(
        Role::User,
        "Use get_weather for Tokyo. Do not answer before calling the tool.",
    )];

    let first = target
        .client
        .chat(chat_request(
            &target,
            messages.clone(),
            tools.clone(),
            Some(system),
            false,
        ))
        .await?;
    let (call_id, city) = weather_call(&first.message)?;
    eprintln!("model requested get_weather({city:?}) as call {call_id}");

    messages.push(first.message);
    messages.push(weather_result(&call_id, &city));
    let final_response = target
        .client
        .chat(chat_request(&target, messages, tools, Some(system), false))
        .await?;

    if final_response
        .message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
    {
        return Err(io::Error::other("model requested a second unexpected tool call").into());
    }
    let text = response_text(&final_response);
    if text.trim().is_empty() {
        return Err(io::Error::other("final response contained no assistant text").into());
    }

    println!("{text}");
    eprintln!(
        "stop={:?}, input_tokens={}, output_tokens={}",
        final_response.stop_reason.value, final_response.usage.input, final_response.usage.output
    );
    Ok(())
}

/// Defines the provider-neutral JSON Schema exposed to the model.
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

/// Extracts and validates the single weather call from an assistant message.
fn weather_call(message: &Message) -> ExampleResult<(String, String)> {
    let calls = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => Some((id, name, input)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(id, name, input)] = calls.as_slice() else {
        return Err(
            io::Error::other(format!("expected one tool call, received {}", calls.len())).into(),
        );
    };
    if name.as_str() != "get_weather" {
        return Err(io::Error::other(format!("unexpected tool name {name:?}")).into());
    }
    let city = input
        .get("city")
        .and_then(Value::as_str)
        .filter(|city| !city.trim().is_empty())
        .ok_or_else(|| io::Error::other("get_weather input did not contain a city"))?;

    Ok(((*id).clone(), city.to_owned()))
}

/// Simulates successful local execution while preserving the model call id.
fn weather_result(tool_call_id: &str, city: &str) -> Message {
    Message {
        role: Role::Tool,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_call_id.to_owned(),
            content: vec![ContentBlock::Text {
                text: format!("{city} weather is sunny at 24 C."),
                extra: Map::new(),
            }],
            status: ToolStatus::Ok,
            extra: Map::new(),
        }],
    }
}
