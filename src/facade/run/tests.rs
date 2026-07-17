//! Unit tests for the facade result and event types.
//!
//! These tests are fully offline: they construct in-memory [`Response`] and
//! [`Usage`] values rather than calling any client, so they are fast and
//! deterministic.

use super::{IntoUserMessage, Reply, RunOutput, UsageSummary};
use crate::client::Response;
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::normalized::{Normalized, StopReason};
use crate::model::usage::Usage;
use serde_json::{Map, json};

/// Builds a text content block with no provider extras.
fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_owned(),
        extra: Map::new(),
    }
}

/// Builds a response mixing text and non-text content for aggregation tests.
fn mixed_response() -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![
                text_block("Hello, "),
                ContentBlock::ToolUse {
                    id: "toolu_1".to_owned(),
                    name: "get_weather".to_owned(),
                    input: json!({ "city": "Shanghai" }),
                    extra: Map::new(),
                },
                text_block("world"),
            ],
        },
        usage: Usage {
            input: 12,
            output: 5,
            ..Usage::default()
        },
        stop_reason: Normalized::from_mapped(StopReason::EndTurn, "end_turn"),
        extra: Map::new(),
    }
}

#[test]
fn reply_aggregates_text_blocks_in_order() {
    let reply = Reply::from(&mixed_response());

    assert_eq!(reply.text(), "Hello, world");
    assert_eq!(
        reply.usage(),
        Some(&Usage {
            input: 12,
            output: 5,
            ..Usage::default()
        })
    );
    assert_eq!(reply.stop_reason(), Some(&StopReason::EndTurn));
}

#[test]
fn run_output_preserves_non_text_content_in_response() {
    let output = RunOutput::from(mixed_response());

    // The aggregated reply only carries text ...
    assert_eq!(output.reply.text(), "Hello, world");

    // ... but the complete response still retains the non-text tool-use block.
    let response = output.response.expect("response retained");
    assert_eq!(response.message.content.len(), 3);
    assert!(matches!(
        response.message.content[1],
        ContentBlock::ToolUse { .. }
    ));

    // Milestone 1 leaves the trace/event collections empty.
    assert!(output.tool_calls.is_empty());
    assert!(output.delegations.is_empty());
    assert!(output.artifacts.is_empty());
    assert!(output.events.is_empty());

    // Supervisor usage is filled from the response; the others are zero.
    assert_eq!(output.usage.supervisor.input, 12);
    assert_eq!(output.usage.subagents, Usage::default());
    assert_eq!(output.usage.external, Usage::default());
}

#[test]
fn into_user_message_is_equivalent_across_all_input_kinds() {
    let expected = Message {
        role: Role::User,
        content: vec![text_block("hi")],
    };

    let from_str: Message = "hi".into_user_message();
    let from_string: Message = String::from("hi").into_user_message();
    let from_message: Message = Message {
        role: Role::User,
        content: vec![text_block("hi")],
    }
    .into_user_message();
    let from_blocks: Message = vec![text_block("hi")].into_user_message();

    assert_eq!(from_str, expected);
    assert_eq!(from_string, expected);
    assert_eq!(from_message, expected);
    assert_eq!(from_blocks, expected);
}

#[test]
fn usage_summary_total_sums_every_slice() {
    let summary = UsageSummary {
        supervisor: Usage {
            input: 1,
            output: 2,
            ..Usage::default()
        },
        subagents: Usage {
            input: 10,
            output: 20,
            ..Usage::default()
        },
        external: Usage {
            input: 100,
            output: 200,
            ..Usage::default()
        },
    };

    let total = summary.total();
    assert_eq!(total.input, 111);
    assert_eq!(total.output, 222);
}

#[test]
fn usage_summary_from_supervisor_and_add_helpers_accumulate() {
    let mut summary = UsageSummary::from_supervisor(Usage {
        input: 3,
        ..Usage::default()
    });
    assert_eq!(summary.supervisor.input, 3);
    assert_eq!(summary.subagents, Usage::default());
    assert_eq!(summary.external, Usage::default());

    summary.add_supervisor(Usage {
        input: 4,
        ..Usage::default()
    });
    summary.add_subagent(Usage {
        output: 7,
        ..Usage::default()
    });
    summary.add_external(Usage {
        reasoning: 9,
        ..Usage::default()
    });

    assert_eq!(summary.supervisor.input, 7);
    assert_eq!(summary.subagents.output, 7);
    assert_eq!(summary.external.reasoning, 9);

    let total = summary.total();
    assert_eq!(total.input, 7);
    assert_eq!(total.output, 7);
    assert_eq!(total.reasoning, 9);
}
