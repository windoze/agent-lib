//! Tool-phase tests for [`DefaultAgentMachine`](super::super::DefaultAgentMachine).
//!
//! Each test drives the pure machine synchronously through the effect protocol:
//! a `NeedLlm` result carrying tool-use blocks opens a tool phase, the machine
//! emits `NeedTool`/`NeedInteraction`, and folded results advance the phase until
//! the model returns a final text turn. The fixtures below supply host-owned ids
//! and approval decisions from deterministic scripts (the library never mints
//! ids on its own).

use super::*;
use crate::{
    agent::{
        ApprovalRequirement, ApprovalResponse, InteractionKind, NoApprovalPolicy,
        ToolApprovalPolicy, ToolExecutionIds, ToolRuntimeError,
    },
    conversation::ToolCallId,
    model::tool::{ToolCall, ToolResponse, ToolStatus},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

// Disjoint id ranges so no two host-supplied ids collide within one turn.
const TOOL_CALL_BASE: u128 = 0x0100_0000;
const RESULT_MESSAGE_BASE: u128 = 0x0200_0000;
const CONTINUATION_MESSAGE_BASE: u128 = 0x0300_0000;
const CONTINUATION_STEP_BASE: u128 = 0x0400_0000;
const TOOL_REQUIREMENT_BASE: u128 = 0x0500_0000;

fn tool_call_id(index: u128) -> ToolCallId {
    ToolCallId::new(Uuid::from_u128(TOOL_CALL_BASE + index))
}

fn result_message_id(index: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(RESULT_MESSAGE_BASE + index))
}

fn continuation_message_id(index: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(CONTINUATION_MESSAGE_BASE + index))
}

fn continuation_step_id(index: u128) -> StepId {
    StepId::new(Uuid::from_u128(CONTINUATION_STEP_BASE + index))
}

fn scripted_requirement_id(index: u128) -> RequirementId {
    RequirementId::new(Uuid::from_u128(TOOL_REQUIREMENT_BASE + index))
}

/// Requirement id source that hands out distinct ids from a fixed pool.
#[derive(Debug)]
struct ScriptedRequirementIds {
    ids: Vec<RequirementId>,
    cursor: AtomicUsize,
}

impl ScriptedRequirementIds {
    fn new() -> Self {
        Self {
            ids: (0..32).map(scripted_requirement_id).collect(),
            cursor: AtomicUsize::new(0),
        }
    }
}

impl RequirementIds for ScriptedRequirementIds {
    fn next_requirement_id(
        &self,
        kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        let index = self.cursor.fetch_add(1, Ordering::SeqCst);
        self.ids
            .get(index)
            .copied()
            .ok_or(RequirementError::IdUnavailable { kind: kind_tag })
    }
}

/// Host id source that draws tool ids and continuation ids from fixed pools, in
/// call order. Predictable so tests can address specific calls by index.
#[derive(Debug)]
struct ScriptedToolIds {
    tool_call_ids: Vec<ToolCallId>,
    result_message_ids: Vec<MessageId>,
    assistant_message_ids: Vec<MessageId>,
    step_ids: Vec<StepId>,
    tool_call_cursor: AtomicUsize,
    result_cursor: AtomicUsize,
    assistant_cursor: AtomicUsize,
    step_cursor: AtomicUsize,
}

impl ScriptedToolIds {
    fn new() -> Self {
        Self {
            tool_call_ids: (0..8).map(tool_call_id).collect(),
            result_message_ids: (0..8).map(result_message_id).collect(),
            assistant_message_ids: (0..8).map(continuation_message_id).collect(),
            step_ids: (0..8).map(continuation_step_id).collect(),
            tool_call_cursor: AtomicUsize::new(0),
            result_cursor: AtomicUsize::new(0),
            assistant_cursor: AtomicUsize::new(0),
            step_cursor: AtomicUsize::new(0),
        }
    }
}

impl ToolExecutionIds for ScriptedToolIds {
    fn tool_call_id(&self, call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        let index = self.tool_call_cursor.fetch_add(1, Ordering::SeqCst);
        self.tool_call_ids
            .get(index)
            .copied()
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: format!("tool call `{}`", call.id),
            })
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        let index = self.result_cursor.fetch_add(1, Ordering::SeqCst);
        self.result_message_ids
            .get(index)
            .copied()
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: format!("tool result for `{}`", call.id),
            })
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        let index = self.assistant_cursor.fetch_add(1, Ordering::SeqCst);
        self.assistant_message_ids
            .get(index)
            .copied()
            .ok_or(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation message".to_owned(),
            })
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        let index = self.step_cursor.fetch_add(1, Ordering::SeqCst);
        self.step_ids
            .get(index)
            .copied()
            .ok_or(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation step".to_owned(),
            })
    }
}

/// Approval policy that pauses only for tools whose name is listed.
#[derive(Debug)]
struct ApproveByName {
    require: Vec<String>,
}

impl ApproveByName {
    fn new(require: &[&str]) -> Self {
        Self {
            require: require.iter().map(|name| (*name).to_owned()).collect(),
        }
    }
}

impl ToolApprovalPolicy for ApproveByName {
    fn approval_requirement(&self, _call_id: ToolCallId, call: &ToolCall) -> ApprovalRequirement {
        if self.require.iter().any(|name| name == &call.name) {
            ApprovalRequirement::RequireApproval {
                reason: Some(format!("approve {}", call.name)),
            }
        } else {
            ApprovalRequirement::AutoApprove
        }
    }
}

/// Approval policy that pauses for every tool call.
#[derive(Debug)]
struct AlwaysApprove;

impl ToolApprovalPolicy for AlwaysApprove {
    fn approval_requirement(&self, _call_id: ToolCallId, _call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::RequireApproval { reason: None }
    }
}

/// Builds a machine over the default spec wired with a scripted id source and
/// the given approval policy.
fn tool_machine(policy: Arc<dyn ToolApprovalPolicy>) -> DefaultAgentMachine {
    DefaultAgentMachine::new(
        state(),
        LlmStepMode::NonStreaming,
        Arc::new(ScriptedRequirementIds::new()),
    )
    .with_tool_execution_ids(Arc::new(ScriptedToolIds::new()))
    .with_approval_policy(policy)
}

/// Builds a machine over `state` (for non-default loop policies) wired the same.
fn tool_machine_over(
    state: AgentState,
    policy: Arc<dyn ToolApprovalPolicy>,
) -> DefaultAgentMachine {
    DefaultAgentMachine::new(
        state,
        LlmStepMode::NonStreaming,
        Arc::new(ScriptedRequirementIds::new()),
    )
    .with_tool_execution_ids(Arc::new(ScriptedToolIds::new()))
    .with_approval_policy(policy)
}

/// Builds an agent state whose loop policy differs from the default fixture.
fn state_with_policy(policy: LoopPolicy) -> AgentState {
    let spec = AgentSpec::new(
        agent_id(),
        WorktreeRef::new("/repo/agent-lib"),
        Some("Spec fallback system.".to_owned()),
        ToolSetRef::new(tool_set_id(), Vec::new()),
        ModelRef::new("gpt-5.5", nz(512), Some(0.1), None),
        policy,
    );
    AgentState::new(
        spec,
        Conversation::new(
            conversation_id(),
            ConversationConfig::new(Some("Conversation system.".to_owned())),
        ),
    )
}

fn tool_use_block(provider_id: &str, name: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: provider_id.to_owned(),
        name: name.to_owned(),
        input: json!({}),
        extra: Map::new(),
    }
}

fn tool_use_response_with(blocks: Vec<ContentBlock>) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: blocks,
        },
        usage: Usage::default(),
        stop_reason: StopReason::normalize("tool_use"),
        extra: Map::new(),
    }
}

fn single_tool_response(provider_id: &str, name: &str) -> Response {
    tool_use_response_with(vec![tool_use_block(provider_id, name)])
}

fn tool_ok(provider_id: &str, text: &str) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_id.to_owned(),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
            extra: Map::new(),
        }],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

fn approval(response: ApprovalResponse) -> RequirementResult {
    RequirementResult::Interaction(InteractionResponse::Approval(response))
}

/// Resumes a `NeedLlm` step with `response` and returns the resulting outcome.
fn resume_llm(machine: &mut DefaultAgentMachine, id: RequirementId, response: Response) -> Outcome {
    machine.step(StepInput::resume(RequirementResolution::new(
        id,
        RequirementResult::Llm(Ok(response)),
    )))
}

type Outcome = crate::agent::StepOutcome;

fn need_tool(outcome: &Outcome, index: usize) -> (RequirementId, ToolCallId, &str) {
    let requirement = &outcome.requirements[index];
    let RequirementKind::NeedTool { call_id, call } = &requirement.kind else {
        panic!(
            "expected a NeedTool requirement, got {:?}",
            requirement.kind
        );
    };
    (requirement.id, *call_id, call.name.as_str())
}

fn need_interaction(outcome: &Outcome, index: usize) -> (RequirementId, StepId, ToolCallId) {
    let requirement = &outcome.requirements[index];
    let RequirementKind::NeedInteraction { request } = &requirement.kind else {
        panic!(
            "expected a NeedInteraction requirement, got {:?}",
            requirement.kind
        );
    };
    let InteractionKind::Approval { call_id, .. } = request.kind() else {
        panic!("expected an approval interaction, got {:?}", request.kind());
    };
    (requirement.id, request.step_id(), *call_id)
}

fn assert_tool_started(
    notification: &Notification,
    expected_step: StepId,
    expected_call: ToolCallId,
) {
    let Notification::ToolCallStarted(started) = notification else {
        panic!("expected a tool-start notification, got {notification:?}");
    };
    assert_eq!(started.step_id(), expected_step);
    assert_eq!(started.call_id(), expected_call);
}

fn assert_tool_finished(
    notification: &Notification,
    expected_step: StepId,
    expected_call: ToolCallId,
) -> ToolStatus {
    let Notification::ToolCallFinished(finished) = notification else {
        panic!("expected a tool-finish notification, got {notification:?}");
    };
    assert_eq!(finished.step_id(), expected_step);
    assert_eq!(finished.call_id(), expected_call);
    finished.response().status
}

#[test]
fn single_auto_tool_call_runs_then_model_finishes() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);

    // The model asks for one tool.
    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    assert_eq!(outcome.requirements.len(), 1);
    let (tool_req, call_id, name) = need_tool(&outcome, 0);
    assert_eq!(call_id, tool_call_id(0));
    assert_eq!(name, "get_weather");
    assert_eq!(outcome.notifications.len(), 1);
    assert_tool_started(&outcome.notifications[0], step_id(), tool_call_id(0));

    // The tool result folds back and the machine asks the model to continue.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_req,
        RequirementResult::Tool(Ok(tool_ok("call-weather", "sunny"))),
    )));
    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(outcome.requirements.len(), 1);
    let RequirementKind::NeedLlm { .. } = &outcome.requirements[0].kind else {
        panic!("post-tool step must emit NeedLlm");
    };
    let llm2 = outcome.requirements[0].id;
    assert_eq!(outcome.notifications.len(), 2);
    assert_eq!(
        assert_tool_finished(&outcome.notifications[0], step_id(), tool_call_id(0)),
        ToolStatus::Ok
    );
    let Notification::StepBoundary(boundary) = &outcome.notifications[1] else {
        panic!("tool step must close with a step boundary");
    };
    assert_eq!(boundary.step_id(), step_id());

    // The final text response commits the turn.
    let _ = resume_llm(&mut machine, llm2, text_response("done"));
    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 4);
    assert_text(turn.messages()[3].payload(), "done");
}

#[test]
fn parallel_tool_batch_resumes_out_of_order() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);

    let outcome = resume_llm(
        &mut machine,
        llm_id,
        tool_use_response_with(vec![
            tool_use_block("call-a", "first_tool"),
            tool_use_block("call-b", "second_tool"),
        ]),
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    assert_eq!(outcome.requirements.len(), 2);
    let (req_a, call_a, _) = need_tool(&outcome, 0);
    let (req_b, call_b, _) = need_tool(&outcome, 1);
    assert_eq!(call_a, tool_call_id(0));
    assert_eq!(call_b, tool_call_id(1));
    assert_eq!(outcome.notifications.len(), 2);

    // Resolve the second call first: the batch is not yet idle, so the machine
    // stays parked with no new requirement.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        req_b,
        RequirementResult::Tool(Ok(tool_ok("call-b", "beta"))),
    )));
    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    assert!(outcome.requirements.is_empty());
    assert_eq!(outcome.notifications.len(), 1);
    assert_tool_finished(&outcome.notifications[0], step_id(), call_b);

    // Resolving the first call drains the batch and continues to the model.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        req_a,
        RequirementResult::Tool(Ok(tool_ok("call-a", "alpha"))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(outcome.requirements.len(), 1);
    let llm2 = outcome.requirements[0].id;
    assert_tool_finished(&outcome.notifications[0], step_id(), call_a);

    let _ = resume_llm(&mut machine, llm2, text_response("both done"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    let conversation = machine.state().conversation();
    let turn = &conversation.turns()[0];
    // user + assistant(2 tool-use) + 2 tool results + assistant(text).
    assert_eq!(turn.messages().len(), 5);
    assert_text(turn.messages()[4].payload(), "both done");
}

#[test]
fn tool_error_returns_to_model_under_return_error_policy() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);

    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    let (tool_req, call_id, _) = need_tool(&outcome, 0);

    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_req,
        RequirementResult::Tool(Err(ToolRuntimeError::UnknownTool {
            name: "get_weather".to_owned(),
        })),
    )));
    // The failed call still folds back and the loop self-heals into a new step.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        assert_tool_finished(&outcome.notifications[0], step_id(), call_id),
        ToolStatus::Error
    );
    let llm2 = outcome.requirements[0].id;

    let _ = resume_llm(&mut machine, llm2, text_response("recovered"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    let turn = &machine.state().conversation().turns()[0];
    assert_eq!(turn.messages().len(), 4);
}

#[test]
fn tool_error_stops_run_under_stop_policy() {
    let state = state_with_policy(LoopPolicy::new(nz(8), nz(1), ToolFailurePolicy::StopRun));
    let mut machine = tool_machine_over(state, Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);

    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    let (tool_req, _, _) = need_tool(&outcome, 0);

    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_req,
        RequirementResult::Tool(Err(ToolRuntimeError::UnknownTool {
            name: "get_weather".to_owned(),
        })),
    )));
    assert!(outcome.is_quiescent());
    let LoopCursor::Error(error) = machine.cursor() else {
        panic!("StopRun must park on the error cursor");
    };
    assert!(error.message().contains("get_weather"));
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn approval_grant_runs_the_tool() {
    let mut machine = tool_machine(Arc::new(AlwaysApprove));
    let llm_id = park_on_need_llm(&mut machine);

    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingApproval);
    assert!(outcome.notifications.is_empty());
    let (approval_req, req_step, req_call) = need_interaction(&outcome, 0);
    assert_eq!(req_step, step_id());
    assert_eq!(req_call, tool_call_id(0));

    // Approving emits the NeedTool that had been gated.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        approval_req,
        approval(ApprovalResponse::approve(step_id(), tool_call_id(0))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    let (tool_req, call_id, _) = need_tool(&outcome, 0);
    assert_eq!(call_id, tool_call_id(0));
    assert_tool_started(&outcome.notifications[0], step_id(), tool_call_id(0));

    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_req,
        RequirementResult::Tool(Ok(tool_ok("call-weather", "sunny"))),
    )));
    let llm2 = outcome.requirements[0].id;
    let _ = resume_llm(&mut machine, llm2, text_response("done"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    let turn = &machine.state().conversation().turns()[0];
    assert_eq!(turn.messages().len(), 4);
}

#[test]
fn approval_denial_synthesizes_result_and_continues() {
    let mut machine = tool_machine(Arc::new(AlwaysApprove));
    let llm_id = park_on_need_llm(&mut machine);

    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    let (approval_req, _, _) = need_interaction(&outcome, 0);

    // Denying appends a synthesized denied result and continues to the model.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        approval_req,
        approval(ApprovalResponse::deny(
            step_id(),
            tool_call_id(0),
            Some("not allowed".to_owned()),
        )),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        assert_tool_finished(&outcome.notifications[0], step_id(), tool_call_id(0)),
        ToolStatus::Denied
    );
    let llm2 = outcome.requirements[0].id;

    let _ = resume_llm(&mut machine, llm2, text_response("understood"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    let turn = &machine.state().conversation().turns()[0];
    // user + assistant(tool-use) + denied result + assistant(text).
    assert_eq!(turn.messages().len(), 4);
}

#[test]
fn approval_cancel_marks_result_cancelled() {
    let mut machine = tool_machine(Arc::new(AlwaysApprove));
    let llm_id = park_on_need_llm(&mut machine);

    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    let (approval_req, _, _) = need_interaction(&outcome, 0);

    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        approval_req,
        approval(ApprovalResponse::cancel(step_id(), tool_call_id(0), None)),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        assert_tool_finished(&outcome.notifications[0], step_id(), tool_call_id(0)),
        ToolStatus::Cancelled
    );
}

#[test]
fn mixed_batch_runs_auto_then_gates_approval() {
    let mut machine = tool_machine(Arc::new(ApproveByName::new(&["delete_files"])));
    let llm_id = park_on_need_llm(&mut machine);

    // One auto-approved read and one guarded delete in the same assistant turn.
    let outcome = resume_llm(
        &mut machine,
        llm_id,
        tool_use_response_with(vec![
            tool_use_block("call-read", "get_weather"),
            tool_use_block("call-delete", "delete_files"),
        ]),
    );
    // The auto call fires immediately; the guarded call waits.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    assert_eq!(outcome.requirements.len(), 1);
    let (read_req, read_call, name) = need_tool(&outcome, 0);
    assert_eq!(read_call, tool_call_id(0));
    assert_eq!(name, "get_weather");

    // Resolving the auto call surfaces the approval for the guarded call.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        read_req,
        RequirementResult::Tool(Ok(tool_ok("call-read", "sunny"))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingApproval);
    let (approval_req, _, approval_call) = need_interaction(&outcome, 0);
    assert_eq!(approval_call, tool_call_id(1));
    assert_tool_finished(&outcome.notifications[0], step_id(), read_call);

    // Approving runs the guarded call.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        approval_req,
        approval(ApprovalResponse::approve(step_id(), tool_call_id(1))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    let (delete_req, delete_call, _) = need_tool(&outcome, 0);
    assert_eq!(delete_call, tool_call_id(1));

    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        delete_req,
        RequirementResult::Tool(Ok(tool_ok("call-delete", "deleted"))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    let llm2 = outcome.requirements[0].id;

    let _ = resume_llm(&mut machine, llm2, text_response("all done"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    let turn = &machine.state().conversation().turns()[0];
    // user + assistant(2 tool-use) + 2 results + assistant(text).
    assert_eq!(turn.messages().len(), 5);
}

#[test]
fn multi_round_tool_then_tool_then_text() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);

    // Round 1: a tool call.
    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-1", "first_tool"),
    );
    let (tool_req_1, call_1, _) = need_tool(&outcome, 0);
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_req_1,
        RequirementResult::Tool(Ok(tool_ok("call-1", "one"))),
    )));
    let llm2 = outcome.requirements[0].id;

    // Round 2: the continuation step asks for another tool.
    let outcome = resume_llm(
        &mut machine,
        llm2,
        single_tool_response("call-2", "second_tool"),
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    let (tool_req_2, call_2, _) = need_tool(&outcome, 0);
    assert_ne!(call_1, call_2);
    // The second tool phase runs under the continuation step id.
    assert_tool_started(&outcome.notifications[0], continuation_step_id(0), call_2);

    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_req_2,
        RequirementResult::Tool(Ok(tool_ok("call-2", "two"))),
    )));
    let llm3 = outcome.requirements[0].id;

    // Round 3: final text commits the whole turn.
    let _ = resume_llm(&mut machine, llm3, text_response("finished"));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    let turn = &machine.state().conversation().turns()[0];
    // user + a1(tool) + r1 + a2(tool) + r2 + a3(text).
    assert_eq!(turn.messages().len(), 6);
    assert_text(turn.messages()[5].payload(), "finished");
}

#[test]
fn step_limit_stops_before_next_model_step() {
    let state = state_with_policy(LoopPolicy::new(
        nz(1),
        nz(1),
        ToolFailurePolicy::ReturnErrorToModel,
    ));
    let mut machine = tool_machine_over(state, Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);

    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    let (tool_req, call_id, _) = need_tool(&outcome, 0);

    // Resolving the tool would start a second step, but the limit is one.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_req,
        RequirementResult::Tool(Ok(tool_ok("call-weather", "sunny"))),
    )));
    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    // The tool step boundary is still emitted before the failure.
    assert_tool_finished(&outcome.notifications[0], step_id(), call_id);
    let Notification::StepBoundary(_) = &outcome.notifications[1] else {
        panic!("the tool step boundary must precede the step-limit failure");
    };
    let LoopCursor::Error(error) = machine.cursor() else {
        panic!("the step limit must park on the error cursor");
    };
    assert!(error.message().contains("step limit"));
    assert!(machine.state().conversation().pending().is_none());
}

#[test]
fn tool_resume_with_unknown_requirement_fails() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);
    let _ = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );

    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        other_requirement_id(),
        RequirementResult::Tool(Ok(tool_ok("call-weather", "sunny"))),
    )));
    assert!(outcome.is_quiescent());
    let LoopCursor::Error(error) = machine.cursor() else {
        panic!("an unknown tool requirement must park on the error cursor");
    };
    assert!(error.message().contains("not an in-flight tool call"));
}

#[test]
fn tool_resume_with_wrong_result_kind_fails() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);
    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    let (tool_req, _, _) = need_tool(&outcome, 0);

    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_req,
        RequirementResult::Interaction(InteractionResponse::Answer("no".to_owned())),
    )));
    assert!(outcome.is_quiescent());
    let LoopCursor::Error(error) = machine.cursor() else {
        panic!("a mismatched result kind must park on the error cursor");
    };
    assert!(error.message().contains("NeedTool"));
}

#[test]
fn approval_resume_rejecting_wrong_call_fails() {
    let mut machine = tool_machine(Arc::new(AlwaysApprove));
    let llm_id = park_on_need_llm(&mut machine);
    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    let (approval_req, _, _) = need_interaction(&outcome, 0);

    // The approval addresses a different tool call than the one in flight.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        approval_req,
        approval(ApprovalResponse::approve(step_id(), tool_call_id(7))),
    )));
    assert!(outcome.is_quiescent());
    let LoopCursor::Error(error) = machine.cursor() else {
        panic!("an approval for the wrong call must park on the error cursor");
    };
    assert!(error.message().contains("interaction result rejected"));
}

#[test]
fn abandon_tool_batch_synthesizes_cancelled_results_and_settles_idle() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);

    // The model asks for two tools; both are emitted as one auto batch.
    let outcome = resume_llm(
        &mut machine,
        llm_id,
        tool_use_response_with(vec![
            tool_use_block("call-a", "first_tool"),
            tool_use_block("call-b", "second_tool"),
        ]),
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    let (req_a, call_a, _) = need_tool(&outcome, 0);
    let (req_b, _call_b, _) = need_tool(&outcome, 1);

    // Tool A resolves with a real result; the batch stays parked on B.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        req_a,
        RequirementResult::Tool(Ok(tool_ok("call-a", "alpha"))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    assert!(outcome.requirements.is_empty());

    // Abandoning the still-open call B cancels the whole turn: A keeps its real
    // result, B is closed by a synthesized cancelled result, and the cursor
    // settles to a feedable Idle with a coherent (no open tool_use) pending turn.
    let outcome = machine.step(StepInput::abandon(req_b));
    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert!(outcome.notifications.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(machine.cursor().pending_requirement_ids().is_empty());

    let conversation = machine.state().conversation();
    let pending = conversation
        .pending()
        .expect("a tool abandon resumes a coherent pending turn");
    assert_eq!(pending.open_calls().count(), 0);
    assert_eq!(pending.tool_calls().len(), 2);
    // user + assistant(2 tool-use) + result(A) + result(B, cancelled).
    assert_eq!(pending.messages().len(), 4);
    assert!(conversation.turns().is_empty());
    let _ = call_a;
}

#[test]
fn abandon_awaiting_approval_synthesizes_cancelled_result() {
    let mut machine = tool_machine(Arc::new(AlwaysApprove));
    let llm_id = park_on_need_llm(&mut machine);

    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingApproval);
    let (approval_req, _, _) = need_interaction(&outcome, 0);

    // Cancelling while parked on an approval closes the gated call.
    let outcome = machine.step(StepInput::abandon(approval_req));
    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);

    let conversation = machine.state().conversation();
    let pending = conversation
        .pending()
        .expect("an approval abandon resumes a coherent pending turn");
    assert_eq!(pending.open_calls().count(), 0);
    assert_eq!(pending.tool_calls().len(), 1);
    // user + assistant(tool-use) + result(cancelled).
    assert_eq!(pending.messages().len(), 3);
}

#[test]
fn abandon_tool_batch_then_user_message_opens_new_turn() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);

    let outcome = resume_llm(
        &mut machine,
        llm_id,
        single_tool_response("call-weather", "get_weather"),
    );
    let (tool_req, _, _) = need_tool(&outcome, 0);
    let _ = machine.step(StepInput::abandon(tool_req));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(machine.state().conversation().pending().is_some());

    // A fresh user message discards the interrupted turn and opens a new one.
    let outcome = machine.step(StepInput::external(second_user_input()));
    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    let RequirementKind::NeedLlm { .. } = &outcome.requirements[0].kind else {
        panic!("a new user turn must emit NeedLlm");
    };
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_some());
    assert_eq!(conversation.pending().expect("pending").messages().len(), 1);
    assert!(conversation.turns().is_empty());
}
