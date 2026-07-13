use super::DefaultAgentMachine;
use crate::{
    agent::{
        AgentInput, AgentMachine, AgentSpec, AgentState, InteractionResponse, LlmStepMode,
        LoopCursor, LoopCursorKind, LoopPolicy, ModelRef, Notification, RequirementError,
        RequirementId, RequirementIds, RequirementKind, RequirementKindTag, RequirementResolution,
        RequirementResult, StepId, StepInput, ToolFailurePolicy, ToolSetRef, WorktreeRef,
    },
    client::{ClientError, Response},
    conversation::{Conversation, ConversationConfig, MessageId, TurnId},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        usage::Usage,
    },
};
use serde_json::{Map, json};
use std::{num::NonZeroU32, sync::Arc};

#[derive(Debug)]
struct FixedRequirementIds(RequirementId);

impl RequirementIds for FixedRequirementIds {
    fn next_requirement_id(
        &self,
        _kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        Ok(self.0)
    }
}

fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("non-zero test value")
}

fn agent_id() -> crate::agent::AgentId {
    "018f0d9c-7b6a-7c12-8f31-123456789001"
        .parse()
        .expect("agent id")
}

fn tool_set_id() -> crate::agent::ToolSetId {
    "018f0d9c-7b6a-7c12-8f31-123456789002"
        .parse()
        .expect("tool set id")
}

fn conversation_id() -> crate::conversation::ConversationId {
    "018f0d9c-7b6a-7c12-8f31-123456789004"
        .parse()
        .expect("conversation id")
}

fn turn_id() -> TurnId {
    "018f0d9c-7b6a-7c12-8f31-123456789005"
        .parse()
        .expect("turn id")
}

fn user_message_id() -> MessageId {
    "018f0d9c-7b6a-7c12-8f31-123456789006"
        .parse()
        .expect("user message id")
}

fn assistant_message_id() -> MessageId {
    "018f0d9c-7b6a-7c12-8f31-123456789007"
        .parse()
        .expect("assistant message id")
}

fn step_id() -> StepId {
    "018f0d9c-7b6a-7c12-8f31-123456789008"
        .parse()
        .expect("step id")
}

fn requirement_id() -> RequirementId {
    RequirementId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a1").expect("requirement id")
}

fn other_requirement_id() -> RequirementId {
    RequirementId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a2").expect("requirement id")
}

fn spec() -> AgentSpec {
    AgentSpec::new(
        agent_id(),
        WorktreeRef::new("/repo/agent-lib"),
        Some("Spec fallback system.".to_owned()),
        ToolSetRef::new(tool_set_id(), Vec::new()),
        ModelRef::new("gpt-5.5", nz(512), Some(0.1), None),
        LoopPolicy::new(nz(8), nz(1), ToolFailurePolicy::ReturnErrorToModel),
    )
}

fn state() -> AgentState {
    AgentState::new(
        spec(),
        Conversation::new(
            conversation_id(),
            ConversationConfig::new(Some("Conversation system.".to_owned())),
        ),
    )
}

fn machine(mode: LlmStepMode) -> DefaultAgentMachine {
    DefaultAgentMachine::new(
        state(),
        mode,
        Arc::new(FixedRequirementIds(requirement_id())),
    )
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
            extra: Map::new(),
        }],
    }
}

fn user_input() -> AgentInput {
    AgentInput::user_message(
        turn_id(),
        user_message_id(),
        user_message("hello"),
        assistant_message_id(),
        step_id(),
    )
    .expect("valid user input")
}

fn text_response(text: &str) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        },
        usage: Usage::default(),
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

fn tool_use_response() -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call-weather".to_owned(),
                name: "get_weather".to_owned(),
                input: json!({ "city": "Shanghai" }),
                extra: Map::new(),
            }],
        },
        usage: Usage::default(),
        stop_reason: StopReason::normalize("tool_use"),
        extra: Map::new(),
    }
}

fn assert_text(message: &Message, expected: &str) {
    match message.content.as_slice() {
        [ContentBlock::Text { text, .. }] => assert_eq!(text, expected),
        other => panic!("expected a single text block, got {other:?}"),
    }
}

/// Drives the machine from Idle to a blocked `StreamingStep` and returns the
/// emitted `NeedLlm` requirement id.
fn park_on_need_llm(machine: &mut DefaultAgentMachine) -> RequirementId {
    let outcome = machine.step(StepInput::external(user_input()));
    assert!(outcome.is_quiescent());
    assert_eq!(outcome.requirements.len(), 1);
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    outcome.requirements[0].id
}

#[test]
fn user_message_emits_need_llm_and_parks_on_streaming_step() {
    let mut machine = machine(LlmStepMode::NonStreaming);

    let outcome = machine.step(StepInput::external(user_input()));

    assert!(outcome.is_quiescent());
    assert!(outcome.notifications.is_empty());
    assert_eq!(outcome.requirements.len(), 1);

    let requirement = &outcome.requirements[0];
    assert_eq!(requirement.id, requirement_id());
    assert!(requirement.origin.is_root());

    let RequirementKind::NeedLlm { request, mode } = &requirement.kind else {
        panic!("text turn must emit NeedLlm, got {:?}", requirement.kind);
    };
    assert_eq!(*mode, LlmStepMode::NonStreaming);
    assert!(!request.stream);
    assert_eq!(request.model, "gpt-5.5");
    assert_eq!(request.max_tokens, 512);
    assert_eq!(request.messages.len(), 1);
    assert_text(&request.messages[0], "hello");

    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![requirement_id()]
    );
    assert!(machine.state().conversation().pending().is_some());
}

#[test]
fn streaming_mode_requests_stream_transport() {
    let mut machine = machine(LlmStepMode::Streaming);

    let outcome = machine.step(StepInput::external(user_input()));

    let RequirementKind::NeedLlm { request, mode } = &outcome.requirements[0].kind else {
        panic!("expected NeedLlm");
    };
    assert_eq!(*mode, LlmStepMode::Streaming);
    assert!(request.stream);
}

#[test]
fn llm_text_response_commits_turn_and_emits_step_boundary() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);

    let resolution =
        RequirementResolution::new(id, RequirementResult::Llm(Ok(text_response("hi"))));
    let outcome = machine.step(StepInput::resume(resolution));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(outcome.notifications.len(), 1);
    let Notification::StepBoundary(boundary) = &outcome.notifications[0] else {
        panic!("expected a step-boundary notification");
    };
    assert_eq!(boundary.step_id(), step_id());
    assert_eq!(boundary.boundary().turn_count(), 1);

    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    assert!(machine.cursor().pending_requirement_ids().is_empty());

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 2);
    assert_text(turn.messages()[0].payload(), "hello");
    assert_text(turn.messages()[1].payload(), "hi");
}

#[test]
fn llm_client_error_moves_cursor_to_error_and_discards_pending() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);

    let resolution = RequirementResolution::new(
        id,
        RequirementResult::Llm(Err(ClientError::Other("boom".to_owned()))),
    );
    let outcome = machine.step(StepInput::resume(resolution));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert!(outcome.notifications.is_empty());

    let LoopCursor::Error(error) = machine.cursor() else {
        panic!("client error must park on the error cursor");
    };
    assert!(error.message().contains("boom"));

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert!(conversation.turns().is_empty());
}

#[test]
fn resume_with_mismatched_requirement_id_fails() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let _id = park_on_need_llm(&mut machine);

    let resolution = RequirementResolution::new(
        other_requirement_id(),
        RequirementResult::Llm(Ok(text_response("hi"))),
    );
    let outcome = machine.step(StepInput::resume(resolution));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn resume_with_wrong_result_kind_fails() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);

    let resolution = RequirementResolution::new(
        id,
        RequirementResult::Interaction(InteractionResponse::Answer("no".to_owned())),
    );
    let outcome = machine.step(StepInput::resume(resolution));

    assert!(outcome.is_quiescent());
    let LoopCursor::Error(error) = machine.cursor() else {
        panic!("type-mismatched result must park on the error cursor");
    };
    assert!(error.message().contains("interaction"));
}

#[test]
fn tool_use_response_without_tool_id_source_fails() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);

    let resolution =
        RequirementResolution::new(id, RequirementResult::Llm(Ok(tool_use_response())));
    let outcome = machine.step(StepInput::resume(resolution));

    assert!(outcome.is_quiescent());
    let LoopCursor::Error(error) = machine.cursor() else {
        panic!("a tool-use response with no tool id source must park on the error cursor");
    };
    assert!(error.message().contains("tool id unavailable"));

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert!(conversation.turns().is_empty());
}

#[test]
fn resume_without_outstanding_requirement_fails() {
    let mut machine = machine(LlmStepMode::NonStreaming);

    let resolution = RequirementResolution::new(
        requirement_id(),
        RequirementResult::Llm(Ok(text_response("hi"))),
    );
    let outcome = machine.step(StepInput::resume(resolution));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

mod tools;
