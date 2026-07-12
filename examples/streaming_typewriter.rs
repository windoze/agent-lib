//! Prints text deltas immediately while folding the same stream into a response.

mod support;

use agent_lib::{
    model::message::Role,
    stream::{Delta, StreamEvent, accumulator::Accumulator},
};
use futures::StreamExt;
use std::io::{self, Write};
use support::{ExampleResult, chat_request, configured_target, response_text, text_message};

/// Runs the streaming typewriter example.
#[tokio::main]
async fn main() -> ExampleResult<()> {
    let target = configured_target()?;
    eprintln!("streaming a response through {}", target.label);

    let request = chat_request(
        &target,
        vec![text_message(
            Role::User,
            "Write one short sentence about incremental output.",
        )],
        Vec::new(),
        Some("Answer in one concise sentence."),
        true,
    );
    let mut stream = target.client.chat_stream(request).await?;
    let mut accumulator = Accumulator::new();

    while let Some(event) = stream.next().await {
        let event = event?;
        if let StreamEvent::BlockDelta {
            delta: Delta::Text(text),
            ..
        } = &event
        {
            print!("{text}");
            io::stdout().flush()?;
        }
        accumulator.push(event)?;
    }
    println!();

    let response = accumulator.finish()?;
    if response_text(&response).trim().is_empty() {
        return Err(io::Error::other("folded stream contained no assistant text").into());
    }
    eprintln!(
        "folded stop={:?}, input_tokens={}, output_tokens={}",
        response.stop_reason.value, response.usage.input, response.usage.output
    );
    Ok(())
}
