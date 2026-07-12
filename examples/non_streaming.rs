//! Sends one complete-response request through the selected provider adapter.

mod support;

use agent_lib::model::message::Role;
use std::io;
use support::{ExampleResult, chat_request, configured_target, response_text, text_message};

/// Runs the non-streaming client example.
#[tokio::main]
async fn main() -> ExampleResult<()> {
    let target = configured_target()?;
    eprintln!("sending a non-streaming request through {}", target.label);

    let request = chat_request(
        &target,
        vec![text_message(
            Role::User,
            "Explain provider-neutral LLM clients in one short sentence.",
        )],
        Vec::new(),
        Some("Answer concisely."),
        false,
    );
    let response = target.client.chat(request).await?;
    let text = response_text(&response);
    if text.trim().is_empty() {
        return Err(io::Error::other("response contained no assistant text").into());
    }

    println!("{text}");
    eprintln!(
        "stop={:?}, input_tokens={}, output_tokens={}",
        response.stop_reason.value, response.usage.input, response.usage.output
    );
    Ok(())
}
