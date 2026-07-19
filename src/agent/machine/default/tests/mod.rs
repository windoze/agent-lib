use super::DefaultAgentMachine;
use crate::{
    agent::{
        AgentInput, AgentMachine, AgentSpec, AgentState, InteractionResponse, LlmStepMode,
        LoopCursor, LoopCursorKind, LoopDoneReason, LoopPolicy, ModelRef, Notification,
        RequirementError, RequirementId, RequirementIds, RequirementKind, RequirementKindTag,
        RequirementResolution, RequirementResult, StepId, StepInput, StepRejectReason,
        ToolFailurePolicy, ToolSetRef, WorktreeRef,
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

/// A second user turn with ids disjoint from [`user_input`], used to prove that
/// a fresh turn opens after a never-resume abandon settled the machine at Idle.
fn second_user_input() -> AgentInput {
    AgentInput::user_message(
        "018f0d9c-7b6a-7c12-8f31-1234567890b5"
            .parse()
            .expect("second turn id"),
        "018f0d9c-7b6a-7c12-8f31-1234567890b6"
            .parse()
            .expect("second user message id"),
        user_message("again"),
        "018f0d9c-7b6a-7c12-8f31-1234567890b7"
            .parse()
            .expect("second assistant message id"),
        "018f0d9c-7b6a-7c12-8f31-1234567890b8"
            .parse()
            .expect("second step id"),
    )
    .expect("valid second user input")
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
fn llm_invalid_assistant_response_moves_cursor_to_error_and_discards_pending() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);

    // A non-assistant role fails the pending fold: the turn is discarded without
    // committing and the machine parks on the error cursor.
    let invalid = Response {
        message: user_message("not an assistant"),
        usage: Usage::default(),
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    };
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        id,
        RequirementResult::Llm(Ok(invalid)),
    )));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert!(outcome.notifications.is_empty());

    let LoopCursor::Error(error) = machine.cursor() else {
        panic!("an invalid assistant response must park on the error cursor");
    };
    assert!(error.message().contains("conversation operation failed"));

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert!(conversation.turns().is_empty());
}

#[test]
fn resume_with_mismatched_requirement_id_is_soft_rejected() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);

    let resolution = RequirementResolution::new(
        other_requirement_id(),
        RequirementResult::Llm(Ok(text_response("hi"))),
    );
    let outcome = machine.step(StepInput::resume(resolution));

    // A stale resume id is a caller protocol violation: the input is rejected
    // without touching the machine — cursor, pending turn, and the outstanding
    // requirement are all exactly as they were.
    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    let Some(StepRejectReason::UnknownRequirement(detail)) = outcome.rejection() else {
        panic!(
            "a stale resume id must be soft-rejected, got {:?}",
            outcome.rejection()
        );
    };
    assert!(detail.contains("but the machine awaits"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(machine.state().conversation().pending().is_some());

    // The awaited resume still lands afterwards and completes the turn.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        id,
        RequirementResult::Llm(Ok(text_response("hi"))),
    )));
    assert!(!outcome.is_rejected());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
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
fn resume_without_outstanding_requirement_is_soft_rejected() {
    let mut machine = machine(LlmStepMode::NonStreaming);

    let resolution = RequirementResolution::new(
        requirement_id(),
        RequirementResult::Llm(Ok(text_response("hi"))),
    );
    let outcome = machine.step(StepInput::resume(resolution));

    assert!(outcome.is_quiescent());
    let Some(StepRejectReason::UnknownRequirement(detail)) = outcome.rejection() else {
        panic!("a resume with nothing outstanding must be soft-rejected");
    };
    assert!(detail.contains("no outstanding requirement"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
}

#[test]
fn abandon_streaming_step_discards_turn_and_settles_idle() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);
    assert!(machine.state().conversation().pending().is_some());

    let outcome = machine.step(StepInput::abandon(id));

    // Never-resume: an outstanding LLM step is discarded wholesale, no
    // requirement is emitted, and the cursor settles to a feedable Idle.
    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert!(outcome.notifications.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(machine.cursor().pending_requirement_ids().is_empty());

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert!(conversation.turns().is_empty());
}

#[test]
fn user_message_after_a_restored_cancel_recovery_marker_opens_a_new_turn() {
    // A persisted snapshot can capture the cursor between the never-resume
    // closure and the settle back to `Idle` — the transient `CancelRecovery`
    // marker. A machine restored from it must stay feedable (M4-5): the marker
    // is a rest boundary, not a mid-turn park.
    let mut machine = machine(LlmStepMode::NonStreaming);
    park_on_need_llm(&mut machine);
    machine
        .state
        .transition_cursor(LoopCursor::cancel_recovery(
            Some(step_id()),
            crate::agent::CancelRecoveryReason::LlmInterrupted,
        ))
        .expect("streaming step -> cancel recovery is a legal edge");
    assert_eq!(machine.cursor().kind(), LoopCursorKind::CancelRecovery);

    let outcome = machine.step(StepInput::external(second_user_input()));

    // Not soft-rejected: the stale pending is discarded and a fresh turn opens.
    assert!(!outcome.is_rejected());
    assert!(outcome.is_quiescent());
    assert_eq!(outcome.requirements.len(), 1);
    let RequirementKind::NeedLlm { .. } = &outcome.requirements[0].kind else {
        panic!("a new user turn must emit NeedLlm");
    };
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_some());
    assert!(conversation.turns().is_empty());
}

#[test]
fn abandon_streaming_step_then_user_message_opens_new_turn() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);
    let _ = machine.step(StepInput::abandon(id));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);

    // A fresh user message after cancellation opens a brand-new turn.
    let outcome = machine.step(StepInput::external(second_user_input()));

    assert!(outcome.is_quiescent());
    assert_eq!(outcome.requirements.len(), 1);
    let RequirementKind::NeedLlm { .. } = &outcome.requirements[0].kind else {
        panic!("a new user turn must emit NeedLlm");
    };
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_some());
    assert!(conversation.turns().is_empty());
}

#[test]
fn abandon_with_unmatched_requirement_id_is_soft_rejected() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);

    let outcome = machine.step(StepInput::abandon(other_requirement_id()));

    assert!(outcome.is_quiescent());
    let Some(StepRejectReason::UnknownRequirement(detail)) = outcome.rejection() else {
        panic!("abandoning a non-outstanding requirement must be soft-rejected");
    };
    assert!(detail.contains("not outstanding"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(machine.state().conversation().pending().is_some());

    // Abandoning the actually outstanding requirement still works afterwards.
    let outcome = machine.step(StepInput::abandon(id));
    assert!(!outcome.is_rejected());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn abandon_without_outstanding_requirement_is_soft_rejected() {
    let mut machine = machine(LlmStepMode::NonStreaming);

    let outcome = machine.step(StepInput::abandon(requirement_id()));

    assert!(outcome.is_quiescent());
    let Some(StepRejectReason::UnknownRequirement(detail)) = outcome.rejection() else {
        panic!("abandon with no outstanding requirement must be soft-rejected");
    };
    assert!(detail.contains("no outstanding requirement"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
}

#[test]
fn user_message_mid_turn_is_soft_rejected_and_the_turn_continues() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);

    // A second user message while the first turn is parked on its LLM step is
    // rejected without destroying the in-flight turn.
    let outcome = machine.step(StepInput::external(second_user_input()));
    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert!(outcome.notifications.is_empty());
    let Some(StepRejectReason::TurnInProgress(detail)) = outcome.rejection() else {
        panic!(
            "a mid-turn user message must be soft-rejected, got {:?}",
            outcome.rejection()
        );
    };
    assert!(detail.contains("turn is in progress"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    let pending = machine
        .state()
        .conversation()
        .pending()
        .expect("the in-flight turn survives the rejected input");
    assert_eq!(pending.messages().len(), 1);

    // The original turn then completes normally.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        id,
        RequirementResult::Llm(Ok(text_response("hi"))),
    )));
    assert!(!outcome.is_rejected());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    let conversation = machine.state().conversation();
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_text(turn.messages()[0].payload(), "hello");
    assert_text(turn.messages()[1].payload(), "hi");

    // And a fresh user message is feedable again once the turn settled.
    let outcome = machine.step(StepInput::external(second_user_input()));
    assert!(!outcome.is_rejected());
    assert_eq!(outcome.requirements.len(), 1);
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
}

mod reconfig;
mod restore;
mod tools;
