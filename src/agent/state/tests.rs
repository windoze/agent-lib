use super::{
    AgentRuntimeHandles, AgentState, AgentStateError, LoopCursor, LoopCursorKind, LoopDoneReason,
    PivotSource, QueuedPivot, QueuedReconfig,
};
use crate::{
    agent::{
        AgentId, LoopPolicy, ModelRef, SkillId, StepId, ToolFailurePolicy, ToolSetId, ToolSetRef,
        WorktreeRef,
    },
    client::Response,
    conversation::{
        AssistantFinish, Conversation, ConversationConfig, ConversationId, MessageId, TurnId,
        TurnMeta,
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        usage::Usage,
    },
};
use serde_json::{Map, Value, json};
use std::num::NonZeroU32;

fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("test value is non-zero")
}

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890d0"
        .parse()
        .expect("agent id")
}

fn tool_set_id() -> ToolSetId {
    "018f0d9c-7b6a-7c12-8f31-1234567890d1"
        .parse()
        .expect("tool set id")
}

fn skill_id(offset: u8) -> SkillId {
    format!("018f0d9c-7b6a-7c12-8f31-1234567890d{offset}")
        .parse()
        .expect("skill id")
}

fn step_id() -> StepId {
    "018f0d9c-7b6a-7c12-8f31-1234567890d4"
        .parse()
        .expect("step id")
}

fn tool_call_id() -> crate::conversation::ToolCallId {
    "018f0d9c-7b6a-7c12-8f31-1234567890d5"
        .parse()
        .expect("tool call id")
}

fn message_id(offset: u8) -> MessageId {
    format!("018f0d9c-7b6a-7c12-8f31-1234567890e{offset}")
        .parse()
        .expect("message id")
}

fn spec() -> crate::agent::AgentSpec {
    crate::agent::AgentSpec::new(
        agent_id(),
        WorktreeRef::new("/repo/agent-lib"),
        Some("Answer concisely.".to_owned()),
        ToolSetRef::new(tool_set_id(), Vec::new()),
        ModelRef::new("gpt-5.5", nz(512), None, None),
        LoopPolicy::new(nz(8), nz(2), ToolFailurePolicy::ReturnErrorToModel),
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

fn assistant_response(text: &str) -> Response {
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

fn committed_conversation() -> Conversation {
    let conversation_id: ConversationId = "018f0d9c-7b6a-7c12-8f31-1234567890df"
        .parse()
        .expect("conversation id");
    let turn_id: TurnId = "018f0d9c-7b6a-7c12-8f31-1234567890de"
        .parse()
        .expect("turn id");
    let mut conversation = Conversation::new(
        conversation_id,
        ConversationConfig::new(Some("Answer concisely.".to_owned())),
    );
    conversation
        .begin_turn(turn_id, message_id(0), user_message("hello"))
        .expect("begin turn");
    conversation
        .start_assistant_response(assistant_response("hi"))
        .expect("assistant response");
    let finish = conversation
        .finish_assistant(message_id(1))
        .expect("finish assistant");
    assert_eq!(finish, AssistantFinish::ReadyToCommit);
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit pending");
    conversation
}

#[test]
fn agent_state_serde_round_trips_through_conversation_snapshot() {
    let mut state = AgentState::new(spec(), committed_conversation());
    state
        .replace_active_skills(vec![skill_id(2), skill_id(3)])
        .expect("active skills are unique");
    state
        .queue_pivot(
            QueuedPivot::new(message_id(2), user_message("pivot"), PivotSource::Human)
                .expect("user pivot"),
        )
        .expect("queue pivot");
    state
        .queue_reconfig(QueuedReconfig::ActivateSkill {
            skill_id: skill_id(2),
        })
        .expect("queue reconfig");
    state
        .transition_cursor(LoopCursor::streaming_step(step_id()))
        .expect("start streaming step");

    let encoded = serde_json::to_value(&state).expect("serialize agent state");
    assert_eq!(encoded["spec"]["id"], json!(agent_id().to_string()));
    assert_eq!(
        encoded["conversation"]["id"],
        json!("018f0d9c-7b6a-7c12-8f31-1234567890df")
    );
    assert_eq!(
        encoded["conversation"]["history"]["raw_turns"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(encoded.get("client").is_none());
    assert!(encoded.get("tool_registry").is_none());
    assert!(encoded.get("runtime").is_none());

    let decoded: AgentState = serde_json::from_value(encoded).expect("deserialize agent state");
    assert_eq!(decoded.spec_id(), agent_id());
    assert_eq!(
        decoded.conversation().id().to_string(),
        "018f0d9c-7b6a-7c12-8f31-1234567890df"
    );
    assert_eq!(decoded.conversation().turns().len(), 1);
    assert_eq!(decoded.active_skills(), &[skill_id(2), skill_id(3)]);
    assert_eq!(decoded.queued_pivots().len(), 1);
    assert_eq!(decoded.queued_reconfigs().len(), 1);
    assert_eq!(decoded.loop_cursor().kind(), LoopCursorKind::StreamingStep);
}

#[test]
fn agent_state_deserialize_revalidates_conversation_snapshot() {
    let state = AgentState::new(spec(), committed_conversation());
    let mut encoded = serde_json::to_value(&state).expect("serialize agent state");
    encoded["conversation"]["schema_version"] = json!(999);

    let error = serde_json::from_value::<AgentState>(encoded)
        .expect_err("unsupported conversation snapshot schema must fail");

    assert!(error.to_string().contains("unsupported schema version"));
}

#[test]
fn serializing_state_with_pending_conversation_is_rejected() {
    let conversation_id: ConversationId = "018f0d9c-7b6a-7c12-8f31-1234567890f0"
        .parse()
        .expect("conversation id");
    let turn_id: TurnId = "018f0d9c-7b6a-7c12-8f31-1234567890f1"
        .parse()
        .expect("turn id");
    let mut conversation = Conversation::new(conversation_id, ConversationConfig::default());
    conversation
        .begin_turn(turn_id, message_id(3), user_message("pending"))
        .expect("begin pending turn");
    let state = AgentState::new(spec(), conversation);

    let error = serde_json::to_value(&state)
        .expect_err("pending conversation cannot produce an AgentState snapshot");

    assert!(error.to_string().contains("pending turn"));
}

#[test]
fn duplicate_active_skills_are_rejected() {
    let mut state = AgentState::new(spec(), committed_conversation());

    let error = state
        .replace_active_skills(vec![skill_id(2), skill_id(2)])
        .expect_err("duplicate skills must be rejected");

    assert_eq!(
        error,
        AgentStateError::DuplicateSkill {
            skill_id: skill_id(2)
        }
    );
}

#[test]
fn illegal_cursor_transition_is_rejected() {
    let mut state = AgentState::new(spec(), committed_conversation());
    let awaiting_tool = LoopCursor::awaiting_tool(step_id(), vec![tool_call_id()])
        .expect("valid awaiting-tool cursor");

    let error = state
        .transition_cursor(awaiting_tool)
        .expect_err("idle cannot jump straight to awaiting tool");

    assert_eq!(
        error,
        AgentStateError::InvalidCursorTransition {
            from: LoopCursorKind::Idle,
            to: LoopCursorKind::AwaitingTool,
        }
    );

    state
        .transition_cursor(LoopCursor::streaming_step(step_id()))
        .expect("idle can start streaming");
    state
        .transition_cursor(LoopCursor::done(LoopDoneReason::Completed))
        .expect("streaming can finish");
    let terminal_error = state
        .transition_cursor(LoopCursor::streaming_step(step_id()))
        .expect_err("terminal cursor cannot restart unchecked");
    assert_eq!(
        terminal_error,
        AgentStateError::InvalidCursorTransition {
            from: LoopCursorKind::Done,
            to: LoopCursorKind::StreamingStep,
        }
    );
}

#[test]
fn awaiting_tool_cursor_requires_non_empty_unique_calls() {
    let empty =
        LoopCursor::awaiting_tool(step_id(), Vec::new()).expect_err("empty tool wait must fail");
    assert_eq!(empty, AgentStateError::EmptyToolWait);

    let duplicate = LoopCursor::awaiting_tool(step_id(), vec![tool_call_id(), tool_call_id()])
        .expect_err("duplicate call ids must fail");
    assert_eq!(
        duplicate,
        AgentStateError::DuplicateToolCall {
            call_id: tool_call_id()
        }
    );
}

#[test]
fn queued_pivot_accepts_only_user_messages() {
    let invalid = QueuedPivot::new(
        message_id(4),
        Message {
            role: Role::Assistant,
            content: Vec::new(),
        },
        PivotSource::Human,
    )
    .expect_err("assistant pivot must fail");

    assert_eq!(
        invalid,
        AgentStateError::InvalidPivotRole {
            actual: Role::Assistant
        }
    );
}

#[test]
fn queued_reconfig_rejects_duplicate_replacement_skills() {
    let error = QueuedReconfig::replace_active_skills(vec![skill_id(2), skill_id(2)])
        .expect_err("duplicate replacement skills must fail");

    assert_eq!(
        error,
        AgentStateError::DuplicateSkill {
            skill_id: skill_id(2)
        }
    );
}

#[test]
fn runtime_handles_are_kept_outside_agent_state_serde() {
    let _handles = AgentRuntimeHandles::with_handles(
        "client-handle",
        "tool-registry-handle",
        Some("mcp-session"),
        Some("approval-responder"),
        Some("task-handle"),
    );
    let state = AgentState::new(spec(), committed_conversation());
    let encoded = serde_json::to_value(state).expect("serialize state");
    let object = encoded.as_object().expect("state object");

    for forbidden in [
        "client",
        "llm_client",
        "tool_registry",
        "mcp_session",
        "approval_responder",
        "task_handle",
        "runtime",
        "stream",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "runtime handle key must not be serialized: {forbidden}"
        );
    }
}

#[test]
fn agent_state_deserialize_rejects_invalid_queued_data() {
    let state = AgentState::new(spec(), committed_conversation());
    let mut encoded = serde_json::to_value(state).expect("serialize state");
    encoded["queued_pivots"] = json!([
        {
            "message_id": "018f0d9c-7b6a-7c12-8f31-1234567890e5",
            "message": { "role": "assistant", "content": [] },
            "source": { "source": "human" }
        }
    ]);

    let error = serde_json::from_value::<AgentState>(encoded)
        .expect_err("invalid queued pivot must fail state restore");

    assert!(error.to_string().contains("Role::User"));
}

#[test]
fn state_json_has_expected_top_level_data_shape() {
    let state = AgentState::new(spec(), committed_conversation());
    let encoded = serde_json::to_value(state).expect("serialize state");
    let object = encoded.as_object().expect("state object");
    let keys = object.keys().cloned().collect::<Vec<_>>();

    assert_eq!(keys, vec!["conversation", "loop_cursor", "spec"]);
    assert_eq!(encoded["loop_cursor"], json!({"state": "idle"}));
    assert_eq!(encoded["active_skills"], Value::Null);
}
