//! Unit tests for the facade result and event types.
//!
//! These tests are fully offline: they construct in-memory [`Response`] and
//! [`Usage`] values rather than calling any client, so they are fast and
//! deterministic.

use super::{
    ApprovalRequest, ArtifactRef, DelegationMessage, DelegationProgress, DelegationStatus,
    DelegationTrace, EscalationTrace, IntoUserMessage, RawEventKind, Reply, RunEvent, RunOutput,
    ToolTrace, UsageSummary, WireRunEvent,
};
use crate::agent::Notification;
use crate::client::Response;
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::normalized::{Normalized, StopReason};
use crate::model::usage::Usage;
use crate::stream::StreamEvent;
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

/// Serializes a wire event to JSON, deserializes it back, and asserts the
/// projection is faithful under a full `serde_json` round-trip.
fn assert_wire_round_trips(wire: &WireRunEvent) {
    let json = serde_json::to_string(wire).expect("wire event serializes");
    let back: WireRunEvent = serde_json::from_str(&json).expect("wire event deserializes");
    assert_eq!(&back, wire, "wire event should survive a serde round-trip");
}

/// Every normalized [`RunEvent`] variant projects to a lossless
/// [`WireRunEvent`] that survives a `serde_json` round-trip.
#[test]
fn normalized_run_events_project_losslessly_and_round_trip() {
    let tool = ToolTrace {
        name: "get_weather".to_owned(),
        call_id: "toolu_1".to_owned(),
    };
    let delegation = DelegationTrace {
        delegate: "researcher".to_owned(),
        status: DelegationStatus::Completed,
        usage: Usage {
            input: 3,
            output: 4,
            ..Usage::default()
        },
    };

    let cases = vec![
        (
            RunEvent::TextDelta("hello".to_owned()),
            WireRunEvent::TextDelta("hello".to_owned()),
        ),
        (
            RunEvent::ToolStarted(tool.clone()),
            WireRunEvent::ToolStarted(tool.clone()),
        ),
        (
            RunEvent::ToolFinished(tool.clone()),
            WireRunEvent::ToolFinished(tool.clone()),
        ),
        (
            RunEvent::ApprovalRequested(ApprovalRequest {
                tool_name: "get_weather".to_owned(),
                call_id: "call-1".to_owned(),
                reason: Some("approve execution of tool `get_weather`".to_owned()),
                input: Some("{\"city\":\"Shanghai\"}".to_owned()),
            }),
            WireRunEvent::ApprovalRequested(ApprovalRequest {
                tool_name: "get_weather".to_owned(),
                call_id: "call-1".to_owned(),
                reason: Some("approve execution of tool `get_weather`".to_owned()),
                input: Some("{\"city\":\"Shanghai\"}".to_owned()),
            }),
        ),
        (
            RunEvent::DelegationStarted(delegation.clone()),
            WireRunEvent::DelegationStarted(delegation.clone()),
        ),
        (
            RunEvent::DelegationProgress(DelegationProgress {
                delegate: "researcher".to_owned(),
                message: "50%".to_owned(),
            }),
            WireRunEvent::DelegationProgress(DelegationProgress {
                delegate: "researcher".to_owned(),
                message: "50%".to_owned(),
            }),
        ),
        (
            RunEvent::DelegationMessage(DelegationMessage {
                delegate: "researcher".to_owned(),
                message: "note".to_owned(),
            }),
            WireRunEvent::DelegationMessage(DelegationMessage {
                delegate: "researcher".to_owned(),
                message: "note".to_owned(),
            }),
        ),
        (
            RunEvent::DelegationArtifact(ArtifactRef {
                path: "out/report.md".to_owned(),
            }),
            WireRunEvent::DelegationArtifact(ArtifactRef {
                path: "out/report.md".to_owned(),
            }),
        ),
        (
            RunEvent::DelegationFinished(delegation.clone()),
            WireRunEvent::DelegationFinished(delegation.clone()),
        ),
        (
            RunEvent::DelegationFailed(delegation.clone()),
            WireRunEvent::DelegationFailed(delegation.clone()),
        ),
        (
            RunEvent::Escalated(EscalationTrace {
                from: "junior".to_owned(),
                to: "senior".to_owned(),
            }),
            WireRunEvent::Escalated(EscalationTrace {
                from: "junior".to_owned(),
                to: "senior".to_owned(),
            }),
        ),
    ];

    for (event, expected) in cases {
        let wire = event.to_wire();
        assert_eq!(wire, expected, "projection should forward the payload");
        assert_wire_round_trips(&wire);
    }
}

/// The `Done` variant projects its [`RunOutput`] into a serializable
/// [`WireRunOutput`], recursively projecting nested events (including any raw
/// escape hatch, which degrades to an opaque marker).
#[test]
fn done_projects_run_output_and_nested_events_round_trip() {
    let mut output = RunOutput::from(mixed_response());
    output.tool_calls.push(ToolTrace {
        name: "get_weather".to_owned(),
        call_id: "toolu_1".to_owned(),
    });
    // A nested raw escape hatch must degrade to an opaque marker in the projection.
    output.events.push(RunEvent::TextDelta("chunk".to_owned()));
    output
        .events
        .push(RunEvent::RawNotification(Notification::Llm(
            StreamEvent::MessageStart {
                role: Role::Assistant,
            },
        )));

    let event = RunEvent::Done(Box::new(output.clone()));
    let wire = event.to_wire();

    let WireRunEvent::Done(wire_output) = &wire else {
        panic!("expected a Done projection");
    };
    assert_eq!(wire_output.reply, output.reply);
    assert_eq!(wire_output.response, output.response);
    assert_eq!(wire_output.tool_calls, output.tool_calls);
    assert_eq!(wire_output.events.len(), 2);
    assert_eq!(
        wire_output.events[0],
        WireRunEvent::TextDelta("chunk".to_owned())
    );
    assert_eq!(
        wire_output.events[1],
        WireRunEvent::Raw(RawEventKind::Notification)
    );

    assert_wire_round_trips(&wire);
}

/// Both raw escape hatches project to a serializable opaque marker that records
/// which hatch fired, without carrying the underlying payload.
#[test]
fn raw_escape_hatches_project_to_opaque_markers() {
    let stream = RunEvent::RawStream(StreamEvent::MessageStart {
        role: Role::Assistant,
    });
    let notification = RunEvent::RawNotification(Notification::Llm(StreamEvent::MessageStart {
        role: Role::Assistant,
    }));

    let stream_wire = stream.to_wire();
    let notification_wire = notification.to_wire();

    assert_eq!(stream_wire, WireRunEvent::Raw(RawEventKind::Stream));
    assert_eq!(
        notification_wire,
        WireRunEvent::Raw(RawEventKind::Notification)
    );

    // The markers are distinguishable and serialize/round-trip without panic.
    assert_ne!(stream_wire, notification_wire);
    assert_wire_round_trips(&stream_wire);
    assert_wire_round_trips(&notification_wire);
}

/// The projection uses an adjacently tagged, `snake_case` wire shape matching
/// the rest of the crate's event encodings.
#[test]
fn wire_event_uses_adjacently_tagged_snake_case_shape() {
    let text = WireRunEvent::TextDelta("hi".to_owned());
    assert_eq!(
        serde_json::to_value(&text).expect("serializes"),
        json!({ "type": "text_delta", "data": "hi" })
    );

    let raw = WireRunEvent::Raw(RawEventKind::Stream);
    assert_eq!(
        serde_json::to_value(&raw).expect("serializes"),
        json!({ "type": "raw", "data": "stream" })
    );
}
