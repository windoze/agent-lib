use super::{
    AgentRuntimeHandles, AgentState, AgentStateError, CursorRequirement, LoopCursor,
    LoopCursorKind, LoopDoneReason, PivotSource, QueuedPivot, QueuedReconfig, ReconfigRequest,
    ToolSetPatch, ToolWaitRequirements,
};
use crate::{
    agent::{
        AgentId, AgentPath, AgentSlot, LoopPolicy, ModelRef, RequirementId, SkillId, StepId,
        ToolFailurePolicy, ToolSetId, ToolSetRef, WorktreeRef,
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
        tool::Tool,
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

fn tool_call_id_2() -> crate::conversation::ToolCallId {
    "018f0d9c-7b6a-7c12-8f31-1234567890e9"
        .parse()
        .expect("second tool call id")
}

fn requirement_id(offset: u8) -> RequirementId {
    format!("018f0d9c-7b6a-7c12-8f31-1234567890c{offset:x}")
        .parse()
        .expect("requirement id")
}

fn non_root_origin() -> AgentPath {
    AgentPath::from_slots(vec![AgentSlot::new(2), AgentSlot::new(7)])
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

fn tool(name: &str) -> Tool {
    Tool {
        name: name.to_owned(),
        description: format!("Tool {name}."),
        input_schema: json!({"type": "object"}),
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
        .queue_reconfig(QueuedReconfig::ActivateSkill {
            skill_id: skill_id(4),
        })
        .expect("queue reconfig");
    state
        .transition_cursor(LoopCursor::streaming_step(step_id(), None))
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
    let awaiting_tool = LoopCursor::awaiting_tool(step_id(), vec![tool_call_id()], None)
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
        .transition_cursor(LoopCursor::streaming_step(step_id(), None))
        .expect("idle can start streaming");
    state
        .transition_cursor(LoopCursor::done(LoopDoneReason::Completed))
        .expect("streaming can finish");
    let terminal_error = state
        .transition_cursor(LoopCursor::streaming_step(step_id(), None))
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
    let empty = LoopCursor::awaiting_tool(step_id(), Vec::new(), None)
        .expect_err("empty tool wait must fail");
    assert_eq!(empty, AgentStateError::EmptyToolWait);

    let duplicate =
        LoopCursor::awaiting_tool(step_id(), vec![tool_call_id(), tool_call_id()], None)
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
fn queued_reconfig_conflicts_do_not_partially_modify_state() {
    let mut state = AgentState::new(spec(), committed_conversation());
    state
        .replace_active_skills(vec![skill_id(2)])
        .expect("active skill set");

    let duplicate = state
        .queue_reconfig(ReconfigRequest::ActivateSkill {
            skill_id: skill_id(2),
        })
        .expect_err("already-active skill is rejected");

    assert_eq!(
        duplicate,
        AgentStateError::SkillAlreadyActive {
            skill_id: skill_id(2)
        }
    );
    assert!(state.queued_reconfigs().is_empty());
    assert_eq!(state.active_skills(), &[skill_id(2)]);

    state
        .queue_reconfig(ReconfigRequest::set_system_prompt_overlay(
            Some("first overlay".to_owned()),
            0,
        ))
        .expect("first overlay is queued");
    let stale = state
        .queue_reconfig(ReconfigRequest::set_system_prompt_overlay(
            Some("stale overlay".to_owned()),
            0,
        ))
        .expect_err("stale overlay version is rejected against queued plan");

    assert_eq!(
        stale,
        AgentStateError::SystemOverlayVersionConflict {
            expected: 0,
            actual: 1,
        }
    );
    assert_eq!(state.queued_reconfigs().len(), 1);
    assert_eq!(state.system_prompt_overlay(), None);
    assert_eq!(state.system_prompt_overlay_version(), 0);
}

#[test]
fn queued_tool_set_patch_applies_atomically_to_current_config() {
    let initial_tools = ToolSetRef::new(tool_set_id(), vec![tool("old"), tool("keep")]);
    let mut agent_spec = spec();
    agent_spec = crate::agent::AgentSpec::new(
        agent_spec.id(),
        agent_spec.worktree().clone(),
        agent_spec.system_prompt().map(ToOwned::to_owned),
        initial_tools.clone(),
        agent_spec.model().clone(),
        *agent_spec.loop_policy(),
    );
    let mut state = AgentState::new(agent_spec, committed_conversation());
    let patch = ToolSetPatch::new(
        initial_tools.id(),
        tool_set_id(),
        vec!["old".to_owned()],
        vec![tool("new")],
    )
    .expect("valid tool patch");
    state
        .queue_reconfig(ReconfigRequest::PatchToolSet { patch })
        .expect("patch queues successfully");
    let application = state
        .queued_reconfig_application()
        .expect("queued patch is valid")
        .expect("application exists");
    state.apply_reconfig_application(application);

    assert!(state.queued_reconfigs().is_empty());
    assert_eq!(
        state.current_tool_set().tools(),
        &[tool("keep"), tool("new")]
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
fn state_json_has_expected_top_level_data_shape() {
    let state = AgentState::new(spec(), committed_conversation());
    let encoded = serde_json::to_value(state).expect("serialize state");
    let object = encoded.as_object().expect("state object");
    let keys = object.keys().cloned().collect::<Vec<_>>();

    assert_eq!(keys, vec!["conversation", "loop_cursor", "spec"]);
    assert_eq!(encoded["loop_cursor"], json!({"state": "idle"}));
    assert_eq!(encoded["active_skills"], Value::Null);
}

#[test]
fn streaming_step_cursor_round_trips_requirement_binding() {
    let cursor = LoopCursor::streaming_step(
        step_id(),
        Some(CursorRequirement::new(requirement_id(1), non_root_origin())),
    );

    let encoded = serde_json::to_value(&cursor).expect("serialize streaming cursor");
    assert_eq!(encoded["state"], json!("streaming_step"));
    assert_eq!(
        encoded["data"]["requirement"]["id"],
        json!(requirement_id(1).to_string())
    );
    assert_eq!(encoded["data"]["requirement"]["origin"], json!([2, 7]));

    let decoded: LoopCursor =
        serde_json::from_value(encoded).expect("deserialize streaming cursor");
    assert_eq!(decoded, cursor);
    assert_eq!(decoded.pending_requirement_ids(), vec![requirement_id(1)]);
}

#[test]
fn root_requirement_origin_is_omitted_from_wire() {
    let cursor =
        LoopCursor::streaming_step(step_id(), Some(CursorRequirement::root(requirement_id(2))));

    let encoded = serde_json::to_value(&cursor).expect("serialize rooted cursor");
    assert!(encoded["data"]["requirement"].get("origin").is_none());

    let decoded: LoopCursor = serde_json::from_value(encoded).expect("deserialize rooted cursor");
    assert_eq!(decoded, cursor);
    assert!(
        match &decoded {
            LoopCursor::StreamingStep(step) =>
                step.requirement().expect("bound").origin().is_root(),
            _ => false,
        },
        "restored origin defaults to root"
    );
}

#[test]
fn legacy_streaming_step_cursor_omits_requirement() {
    let cursor = LoopCursor::streaming_step(step_id(), None);
    let encoded = serde_json::to_value(&cursor).expect("serialize legacy cursor");
    assert!(encoded["data"].get("requirement").is_none());
    assert!(cursor.pending_requirement_ids().is_empty());

    let decoded: LoopCursor = serde_json::from_value(encoded).expect("deserialize legacy cursor");
    assert_eq!(decoded, cursor);
}

#[test]
fn awaiting_tool_cursor_round_trips_requirement_ids() {
    let mut ids = std::collections::BTreeMap::new();
    ids.insert(tool_call_id(), requirement_id(3));
    ids.insert(tool_call_id_2(), requirement_id(4));
    let cursor = LoopCursor::awaiting_tool(
        step_id(),
        vec![tool_call_id(), tool_call_id_2()],
        Some(ToolWaitRequirements::root(ids)),
    )
    .expect("valid awaiting-tool cursor");

    let encoded = serde_json::to_value(&cursor).expect("serialize awaiting-tool cursor");
    let decoded: LoopCursor =
        serde_json::from_value(encoded).expect("deserialize awaiting-tool cursor");
    assert_eq!(decoded, cursor);

    let mut pending = decoded.pending_requirement_ids();
    pending.sort();
    assert_eq!(pending, vec![requirement_id(3), requirement_id(4)]);
}

#[test]
fn awaiting_tool_requirement_binding_must_cover_call_set() {
    let mut ids = std::collections::BTreeMap::new();
    ids.insert(tool_call_id(), requirement_id(3));
    let missing = LoopCursor::awaiting_tool(
        step_id(),
        vec![tool_call_id(), tool_call_id_2()],
        Some(ToolWaitRequirements::root(ids)),
    )
    .expect_err("missing binding for a call must fail");
    assert_eq!(
        missing,
        AgentStateError::ToolRequirementMismatch {
            call_id: tool_call_id_2(),
        }
    );

    let mut extra = std::collections::BTreeMap::new();
    extra.insert(tool_call_id(), requirement_id(3));
    extra.insert(tool_call_id_2(), requirement_id(4));
    let stray = LoopCursor::awaiting_tool(
        step_id(),
        vec![tool_call_id()],
        Some(ToolWaitRequirements::root(extra)),
    )
    .expect_err("binding for an unawaited call must fail");
    assert_eq!(
        stray,
        AgentStateError::ToolRequirementMismatch {
            call_id: tool_call_id_2(),
        }
    );
}

#[test]
fn awaiting_approval_cursor_round_trips_requirement_binding() {
    let cursor = LoopCursor::awaiting_approval(
        step_id(),
        tool_call_id(),
        Some(CursorRequirement::root(requirement_id(5))),
    );

    let encoded = serde_json::to_value(&cursor).expect("serialize approval cursor");
    let decoded: LoopCursor = serde_json::from_value(encoded).expect("deserialize approval cursor");
    assert_eq!(decoded, cursor);
    assert_eq!(decoded.pending_requirement_ids(), vec![requirement_id(5)]);
}

#[test]
fn requirement_free_cursors_report_no_pending_requirements() {
    assert!(LoopCursor::Idle.pending_requirement_ids().is_empty());
    assert!(
        LoopCursor::done(LoopDoneReason::Completed)
            .pending_requirement_ids()
            .is_empty()
    );
    let cursor = LoopCursor::awaiting_tool(step_id(), vec![tool_call_id()], None)
        .expect("legacy awaiting-tool cursor");
    assert!(cursor.pending_requirement_ids().is_empty());
}

#[test]
fn agent_state_round_trips_streaming_cursor_requirement() {
    let mut state = AgentState::new(spec(), committed_conversation());
    state
        .transition_cursor(LoopCursor::streaming_step(
            step_id(),
            Some(CursorRequirement::new(requirement_id(6), non_root_origin())),
        ))
        .expect("start streaming step");

    let encoded = serde_json::to_value(&state).expect("serialize agent state");
    let decoded: AgentState = serde_json::from_value(encoded).expect("deserialize agent state");
    assert_eq!(decoded.loop_cursor(), state.loop_cursor());
    assert_eq!(
        decoded.loop_cursor().pending_requirement_ids(),
        vec![requirement_id(6)]
    );
}
