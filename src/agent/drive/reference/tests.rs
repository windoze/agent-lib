//! Equivalence tests: the reference driver replays the legacy loop's turns.
//!
//! Each test drives a [`DefaultAgentMachine`] through [`drive_turn`] over a
//! single [`ReferenceScope`] and asserts the committed
//! [`Conversation`](crate::conversation::Conversation) and the drained
//! [`Notification`] sequence match the corresponding
//! [`DefaultAgentLoop`](crate::agent::DefaultAgentLoop) integration test in
//! `src/agent/loop_driver/default/tests.rs`. The `DefaultAgentLoop` and its
//! tests are left untouched (migration doc §10, stage 2); these are additional
//! equivalence evidence for the sans-io path.
//!
//! The fakes and builders below are the migratable subset of the legacy loop's
//! test fixtures (fake client, tool registry, host id sources, approval policy),
//! reused here so the two paths run against identical scripts.

use super::{ApprovalInteractionHandler, ReferenceScope, drive_turn};
use crate::{
    agent::{
        AgentInput, AgentSpec, ApprovalDecision, ApprovalRequirement, ApprovalResponse,
        BudgetLimits, DefaultAgentMachine, Interaction, InteractionKind, InteractionResponse,
        LlmStepMode, LoopCursor, LoopCursorKind, LoopPolicy, ModelRef, Notification,
        ReconfigRequest, RequirementError, RequirementId, RequirementIds, RequirementKindTag,
        RequirementResult, RunContext, RunId, StaticToolRegistryResolver, StepId,
        ToolApprovalPolicy, ToolExecutionIds, ToolFailurePolicy, ToolRegistry, ToolRuntimeError,
        ToolSetRef, TraceNodeId, WorktreeRef,
    },
    client::{Capability, ChatRequest, ClientError, LlmClient, Response},
    conversation::{
        Conversation, ConversationConfig, ConversationId, MessageId, ToolCallId, TurnId,
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::{Tool, ToolCall, ToolResponse, ToolStatus},
        usage::Usage,
    },
    stream::StreamEvent,
};
use async_trait::async_trait;
use futures::{StreamExt, stream};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    num::NonZeroU32,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

// ----- fakes migrated from the legacy loop tests -----

/// Fake [`LlmClient`] returning scripted `chat` responses in order.
#[derive(Debug)]
struct FakeClient {
    capability: Capability,
    chat_results: Mutex<VecDeque<Result<Response, ClientError>>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl FakeClient {
    fn with_chats(results: Vec<Result<Response, ClientError>>) -> Self {
        Self {
            capability: Capability::default(),
            chat_results: Mutex::new(VecDeque::from(results)),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("requests mutex").len()
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("requests mutex").clone()
    }
}

#[async_trait]
impl LlmClient for FakeClient {
    fn capability(&self) -> &Capability {
        &self.capability
    }

    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        self.requests.lock().expect("requests mutex").push(request);
        self.chat_results
            .lock()
            .expect("chat results mutex")
            .pop_front()
            .expect("fake chat result")
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError>
    {
        Ok(stream::iter(Vec::<Result<StreamEvent, ClientError>>::new()).boxed())
    }
}

/// Fake [`ToolRegistry`] returning scripted execution results in order.
#[derive(Debug)]
struct FakeToolRegistry {
    declarations: Vec<Tool>,
    results: Mutex<VecDeque<Result<ToolResponse, ToolRuntimeError>>>,
    calls: Mutex<Vec<(ToolCallId, ToolCall)>>,
}

impl FakeToolRegistry {
    fn new(results: Vec<Result<ToolResponse, ToolRuntimeError>>) -> Self {
        Self {
            declarations: vec![weather_tool()],
            results: Mutex::new(VecDeque::from(results)),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn with_declarations(
        declarations: Vec<Tool>,
        results: Vec<Result<ToolResponse, ToolRuntimeError>>,
    ) -> Self {
        Self {
            declarations,
            results: Mutex::new(VecDeque::from(results)),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(ToolCallId, ToolCall)> {
        self.calls.lock().expect("tool calls mutex").clone()
    }
}

#[async_trait]
impl ToolRegistry for FakeToolRegistry {
    fn declarations(&self) -> Vec<Tool> {
        self.declarations.clone()
    }

    async fn execute(
        &self,
        call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        self.calls
            .lock()
            .expect("tool calls mutex")
            .push((call_id, call));
        self.results
            .lock()
            .expect("tool results mutex")
            .pop_front()
            .expect("fake tool result")
    }
}

/// Approval policy that requires approval for every tool call.
#[derive(Debug)]
struct RequireApprovalPolicy {
    reason: Option<String>,
}

impl RequireApprovalPolicy {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
        }
    }
}

impl ToolApprovalPolicy for RequireApprovalPolicy {
    fn approval_requirement(&self, _call_id: ToolCallId, _call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::required(self.reason.clone())
    }
}

/// Requirement id source handing out distinct ids from a fixed pool.
#[derive(Debug)]
struct ScriptedRequirementIds {
    ids: Vec<RequirementId>,
    cursor: Mutex<usize>,
}

impl ScriptedRequirementIds {
    fn new() -> Self {
        Self {
            ids: (0..64u128)
                .map(|index| RequirementId::new(Uuid::from_u128(0x0500_0000 + index)))
                .collect(),
            cursor: Mutex::new(0),
        }
    }
}

impl RequirementIds for ScriptedRequirementIds {
    fn next_requirement_id(
        &self,
        kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        let mut cursor = self.cursor.lock().expect("requirement id cursor");
        let index = *cursor;
        *cursor += 1;
        self.ids
            .get(index)
            .copied()
            .ok_or(RequirementError::IdUnavailable { kind: kind_tag })
    }
}

/// Host id source drawing tool/continuation ids from fixed pools, in call order.
#[derive(Debug)]
struct FakeToolIds {
    tool_call_ids: Mutex<VecDeque<ToolCallId>>,
    result_message_ids: Mutex<VecDeque<MessageId>>,
    assistant_message_ids: Mutex<VecDeque<MessageId>>,
    step_ids: Mutex<VecDeque<StepId>>,
}

impl FakeToolIds {
    fn new(
        tool_call_ids: Vec<ToolCallId>,
        result_message_ids: Vec<MessageId>,
        assistant_message_ids: Vec<MessageId>,
        step_ids: Vec<StepId>,
    ) -> Self {
        Self {
            tool_call_ids: Mutex::new(VecDeque::from(tool_call_ids)),
            result_message_ids: Mutex::new(VecDeque::from(result_message_ids)),
            assistant_message_ids: Mutex::new(VecDeque::from(assistant_message_ids)),
            step_ids: Mutex::new(VecDeque::from(step_ids)),
        }
    }
}

impl ToolExecutionIds for FakeToolIds {
    fn tool_call_id(&self, call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        self.tool_call_ids
            .lock()
            .expect("tool call id mutex")
            .pop_front()
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: format!("tool call `{}`", call.id),
            })
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        self.result_message_ids
            .lock()
            .expect("tool result id mutex")
            .pop_front()
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: format!("tool result `{}`", call.id),
            })
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        self.assistant_message_ids
            .lock()
            .expect("assistant id mutex")
            .pop_front()
            .ok_or(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation message".to_owned(),
            })
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        self.step_ids
            .lock()
            .expect("step id mutex")
            .pop_front()
            .ok_or(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation step".to_owned(),
            })
    }
}

/// Attended interaction backend scripting a decision per tool-call id.
///
/// Models a human resolving each approval differently (the reference-driver
/// counterpart of the legacy loop's per-call `respond_approval`).
#[derive(Debug)]
struct ScriptedApprovalInteraction {
    decisions: BTreeMap<ToolCallId, (ApprovalDecision, Option<String>)>,
}

impl ScriptedApprovalInteraction {
    fn new(decisions: Vec<(ToolCallId, ApprovalDecision, Option<String>)>) -> Self {
        Self {
            decisions: decisions
                .into_iter()
                .map(|(call_id, decision, message)| (call_id, (decision, message)))
                .collect(),
        }
    }
}

#[async_trait]
impl crate::agent::InteractionHandler for ScriptedApprovalInteraction {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        let InteractionKind::Approval { call_id, .. } = request.kind() else {
            panic!("scripted interaction only answers approvals");
        };
        let (decision, message) = self
            .decisions
            .get(call_id)
            .cloned()
            .expect("scripted decision for call");
        RequirementResult::Interaction(InteractionResponse::Approval(ApprovalResponse::new(
            request.step_id(),
            *call_id,
            decision,
            message,
        )))
    }
}

/// A scope reusing [`ReferenceScope`]'s llm/tool handlers with a borrowed
/// scripted interaction backend (per-call approval decisions).
struct ComposedScope<'a> {
    base: ReferenceScope,
    interaction: &'a ScriptedApprovalInteraction,
}

impl crate::agent::HandlerScope for ComposedScope<'_> {
    fn llm(&self) -> Option<&dyn crate::agent::LlmHandler> {
        self.base.llm()
    }

    fn tool(&self) -> Option<&dyn crate::agent::ToolHandler> {
        self.base.tool()
    }

    fn interaction(&self) -> Option<&dyn crate::agent::InteractionHandler> {
        Some(self.interaction)
    }
}

// ----- builders migrated from the legacy loop tests -----

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

fn run_id() -> RunId {
    "018f0d9c-7b6a-7c12-8f31-123456789003"
        .parse()
        .expect("run id")
}

fn conversation_id() -> ConversationId {
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

fn message_id_seed(seed: u64) -> MessageId {
    format!("018f0d9c-7b6a-7c12-8f31-{seed:012x}")
        .parse()
        .expect("message id")
}

fn tool_call_id_seed(seed: u64) -> ToolCallId {
    format!("018f0d9c-7b6a-7c12-8f31-{seed:012x}")
        .parse()
        .expect("tool call id")
}

fn step_id_seed(seed: u64) -> StepId {
    format!("018f0d9c-7b6a-7c12-8f31-{seed:012x}")
        .parse()
        .expect("step id")
}

fn weather_tool() -> Tool {
    Tool {
        name: "get_weather".to_owned(),
        description: "Look up weather for a city.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    }
}

fn calendar_tool() -> Tool {
    Tool {
        name: "read_calendar".to_owned(),
        description: "Read calendar availability.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": { "day": { "type": "string" } },
            "required": ["day"]
        }),
    }
}

fn replacement_tool_set_id() -> crate::agent::ToolSetId {
    "018f0d9c-7b6a-7c12-8f31-1234567890c1"
        .parse()
        .expect("replacement tool set id")
}

fn replacement_tool_set() -> ToolSetRef {
    ToolSetRef::new(replacement_tool_set_id(), vec![calendar_tool()])
}

fn spec() -> AgentSpec {
    spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel)
}

fn spec_with_tools(max_parallel_tools: u32, failure_policy: ToolFailurePolicy) -> AgentSpec {
    AgentSpec::new(
        agent_id(),
        WorktreeRef::new("/repo/agent-lib"),
        Some("Spec fallback system.".to_owned()),
        ToolSetRef::new(tool_set_id(), vec![weather_tool()]),
        ModelRef::new("gpt-5.5", nz(512), Some(0.1), None),
        LoopPolicy::new(nz(8), nz(max_parallel_tools), failure_policy),
    )
}

fn context() -> RunContext {
    RunContext::new_root(
        run_id(),
        BudgetLimits::unbounded(),
        TraceNodeId::new("root"),
    )
}

fn state_with_spec(spec: AgentSpec) -> crate::agent::AgentState {
    crate::agent::AgentState::new(
        spec,
        Conversation::new(
            conversation_id(),
            ConversationConfig::new(Some("Conversation system.".to_owned())),
        ),
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

fn assistant_response(text: &str, usage: Usage) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        },
        usage,
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

fn tool_use_response(calls: Vec<(&str, &str, Value)>, usage: Usage) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: calls
                .into_iter()
                .map(|(id, name, input)| ContentBlock::ToolUse {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    input,
                    extra: Map::new(),
                })
                .collect(),
        },
        usage,
        stop_reason: StopReason::normalize("tool_use"),
        extra: Map::new(),
    }
}

fn tool_response(provider_call_id: &str, text: &str, status: ToolStatus) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_call_id.to_owned(),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
            extra: Map::new(),
        }],
        status,
        extra: Map::new(),
    }
}

fn usage(input: u32, output: u32) -> Usage {
    Usage {
        input,
        output,
        ..Usage::default()
    }
}

fn input() -> AgentInput {
    AgentInput::user_message(
        turn_id(),
        user_message_id(),
        user_message("hello"),
        assistant_message_id(),
        step_id(),
    )
    .expect("valid user input")
}

fn machine_with(
    spec: AgentSpec,
    ids: Arc<FakeToolIds>,
    policy: Arc<dyn ToolApprovalPolicy>,
) -> DefaultAgentMachine {
    DefaultAgentMachine::new(
        state_with_spec(spec),
        LlmStepMode::NonStreaming,
        Arc::new(ScriptedRequirementIds::new()),
    )
    .with_tool_execution_ids(ids)
    .with_approval_policy(policy)
}

// ----- notification classification helpers -----

fn assert_text(message: &Message, expected: &str) {
    assert_eq!(message.content.len(), 1);
    let ContentBlock::Text { text, .. } = &message.content[0] else {
        panic!("expected text content");
    };
    assert_eq!(text, expected);
}

fn assert_tool_result(message: &Message, expected_call_id: &str, expected_status: ToolStatus) {
    assert_eq!(message.role, Role::Tool);
    assert_eq!(message.content.len(), 1);
    let ContentBlock::ToolResult {
        tool_use_id,
        status,
        ..
    } = &message.content[0]
    else {
        panic!("expected tool result content");
    };
    assert_eq!(tool_use_id, expected_call_id);
    assert_eq!(*status, expected_status);
}

// ----- equivalence tests -----

#[tokio::test]
async fn reference_text_only_matches_default_loop() {
    let response_usage = usage(3, 5);
    let client = Arc::new(FakeClient::with_chats(vec![Ok(assistant_response(
        "hi",
        response_usage.clone(),
    ))]));
    let registry = Arc::new(FakeToolRegistry::new(Vec::new()));
    let mut machine = DefaultAgentMachine::new(
        state_with_spec(spec()),
        LlmStepMode::NonStreaming,
        Arc::new(ScriptedRequirementIds::new()),
    );
    let scope = ReferenceScope::new(client.clone(), registry.clone());
    let ctx = context();

    let done = drive_turn(&mut machine, input(), &scope, &ctx)
        .await
        .expect("reference driver completes the text turn");

    // Terminal state: one committed turn, no pending, cursor Done.
    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    assert_eq!(machine.state().loop_cursor().kind(), LoopCursorKind::Done);
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 2);
    assert_text(turn.messages()[0].payload(), "hello");
    assert_text(turn.messages()[1].payload(), "hi");
    assert_eq!(turn.meta().usage(), &response_usage);
    assert_eq!(conversation.version(), 1);

    // Notification sequence: exactly one StepBoundary (turn_count 1).
    let notifications = done.notifications();
    assert_eq!(notifications.len(), 1);
    let Notification::StepBoundary(boundary) = &notifications[0] else {
        panic!("only event is the step boundary");
    };
    assert_eq!(boundary.step_id(), step_id());
    assert_eq!(boundary.boundary().turn_count(), 1);

    assert_eq!(client.request_count(), 1);
    assert!(registry.calls().is_empty());
}

#[tokio::test]
async fn reference_single_tool_matches_default_loop() {
    let client = Arc::new(FakeClient::with_chats(vec![
        Ok(tool_use_response(
            vec![("call-weather", "get_weather", json!({ "city": "Shanghai" }))],
            usage(5, 2),
        )),
        Ok(assistant_response("sunny in Shanghai", usage(7, 4))),
    ]));
    let registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(100)],
        vec![message_id_seed(101)],
        vec![message_id_seed(102)],
        vec![step_id_seed(103)],
    ));
    let mut machine = machine_with(
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
        ids,
        Arc::new(crate::agent::NoApprovalPolicy),
    );
    let scope = ReferenceScope::new(client.clone(), registry.clone());
    let ctx = context();

    let done = drive_turn(&mut machine, input(), &scope, &ctx)
        .await
        .expect("reference driver completes the tool turn");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 4);
    assert_text(turn.messages()[0].payload(), "hello");
    assert_eq!(turn.messages()[1].payload().role, Role::Assistant);
    assert_tool_result(turn.messages()[2].payload(), "call-weather", ToolStatus::Ok);
    assert_text(turn.messages()[3].payload(), "sunny in Shanghai");
    assert_eq!(turn.pairings().len(), 1);
    assert_eq!(turn.pairings()[0].call_id(), tool_call_id_seed(100));
    assert_eq!(turn.pairings()[0].result_msg(), message_id_seed(101));

    // ToolCallStarted, ToolCallFinished, tool StepBoundary, final StepBoundary.
    let notifications = done.notifications();
    assert_eq!(notifications.len(), 4);
    assert!(matches!(notifications[0], Notification::ToolCallStarted(_)));
    let Notification::ToolCallFinished(finished) = &notifications[1] else {
        panic!("second notification finishes the tool");
    };
    assert_eq!(finished.call_id(), tool_call_id_seed(100));
    assert_eq!(finished.response().status, ToolStatus::Ok);
    let Notification::StepBoundary(tool_boundary) = &notifications[2] else {
        panic!("third notification is the tool step boundary");
    };
    assert_eq!(tool_boundary.boundary().turn_count(), 0);
    assert!(tool_boundary.metadata().is_empty());
    let Notification::StepBoundary(final_boundary) = &notifications[3] else {
        panic!("fourth notification is the final step boundary");
    };
    assert_eq!(final_boundary.boundary().turn_count(), 1);

    assert_eq!(client.request_count(), 2);
    let calls = registry.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, tool_call_id_seed(100));
    assert_eq!(calls[0].1.name, "get_weather");
}

#[tokio::test]
async fn reference_parallel_tools_matches_default_loop() {
    let client = Arc::new(FakeClient::with_chats(vec![
        Ok(tool_use_response(
            vec![
                ("call-a", "get_weather", json!({ "city": "Shanghai" })),
                ("call-b", "get_weather", json!({ "city": "Tokyo" })),
            ],
            usage(8, 3),
        )),
        Ok(assistant_response("both checked", usage(9, 5))),
    ]));
    let registry = Arc::new(FakeToolRegistry::new(vec![
        Ok(tool_response("call-a", "Sunny", ToolStatus::Ok)),
        Ok(tool_response("call-b", "Rain", ToolStatus::Ok)),
    ]));
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(200), tool_call_id_seed(201)],
        vec![message_id_seed(202), message_id_seed(203)],
        vec![message_id_seed(204)],
        vec![step_id_seed(205)],
    ));
    let mut machine = machine_with(
        spec_with_tools(2, ToolFailurePolicy::ReturnErrorToModel),
        ids,
        Arc::new(crate::agent::NoApprovalPolicy),
    );
    let scope = ReferenceScope::new(client, registry);
    let ctx = context();

    let done = drive_turn(&mut machine, input(), &scope, &ctx)
        .await
        .expect("reference driver completes the parallel tool turn");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    // Both tools start before either finishes, then two finishes.
    let notifications = done.notifications();
    assert!(matches!(notifications[0], Notification::ToolCallStarted(_)));
    assert!(matches!(notifications[1], Notification::ToolCallStarted(_)));
    assert!(matches!(
        notifications[2],
        Notification::ToolCallFinished(_)
    ));
    assert!(matches!(
        notifications[3],
        Notification::ToolCallFinished(_)
    ));

    let conversation = machine.state().conversation();
    let turn = &conversation.turns()[0];
    assert_eq!(turn.pairings().len(), 2);
    assert_eq!(turn.pairings()[0].call_id(), tool_call_id_seed(200));
    assert_eq!(turn.pairings()[0].result_msg(), message_id_seed(202));
    assert_eq!(turn.pairings()[1].call_id(), tool_call_id_seed(201));
    assert_eq!(turn.pairings()[1].result_msg(), message_id_seed(203));
}

#[tokio::test]
async fn reference_tool_failure_self_heal_matches_default_loop() {
    let client = Arc::new(FakeClient::with_chats(vec![
        Ok(tool_use_response(
            vec![
                ("call-denied", "get_weather", json!({ "city": "Private" })),
                ("call-error", "get_weather", json!({ "city": "Nowhere" })),
            ],
            usage(8, 3),
        )),
        Ok(assistant_response(
            "I recovered from tool results",
            usage(9, 5),
        )),
    ]));
    let registry = Arc::new(FakeToolRegistry::new(vec![
        Ok(tool_response(
            "call-denied",
            "policy denied",
            ToolStatus::Denied,
        )),
        Err(ToolRuntimeError::ExecutionFailed {
            tool_name: "get_weather".to_owned(),
            message: "backend unavailable".to_owned(),
        }),
    ]));
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(300), tool_call_id_seed(301)],
        vec![message_id_seed(302), message_id_seed(303)],
        vec![message_id_seed(304)],
        vec![step_id_seed(305)],
    ));
    let mut machine = machine_with(
        spec_with_tools(2, ToolFailurePolicy::ReturnErrorToModel),
        ids,
        Arc::new(crate::agent::NoApprovalPolicy),
    );
    let scope = ReferenceScope::new(client, registry);
    let ctx = context();

    let done = drive_turn(&mut machine, input(), &scope, &ctx)
        .await
        .expect("tool failures are returned to the model");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    let finished_statuses = done
        .notifications()
        .iter()
        .filter_map(|event| match event {
            Notification::ToolCallFinished(finished) => Some(finished.response().status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        finished_statuses,
        vec![ToolStatus::Denied, ToolStatus::Error]
    );

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    let turn = &conversation.turns()[0];
    assert_tool_result(
        turn.messages()[2].payload(),
        "call-denied",
        ToolStatus::Denied,
    );
    assert_tool_result(
        turn.messages()[3].payload(),
        "call-error",
        ToolStatus::Error,
    );
    assert_text(
        turn.messages()[4].payload(),
        "I recovered from tool results",
    );
}

#[tokio::test]
async fn reference_approval_approve_matches_default_loop() {
    let client = Arc::new(FakeClient::with_chats(vec![
        Ok(tool_use_response(
            vec![("call-weather", "get_weather", json!({ "city": "Shanghai" }))],
            usage(5, 2),
        )),
        Ok(assistant_response("approved result used", usage(7, 4))),
    ]));
    let registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(700)],
        vec![message_id_seed(701)],
        vec![message_id_seed(702)],
        vec![step_id_seed(703)],
    ));
    let mut machine = machine_with(
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
        ids,
        Arc::new(RequireApprovalPolicy::new("human approval required")),
    );
    let scope = ReferenceScope::new(client, registry.clone())
        .with_interaction(ApprovalInteractionHandler::approve());
    let ctx = context();

    let done = drive_turn(&mut machine, input(), &scope, &ctx)
        .await
        .expect("approved tool turn completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    // The approved call starts, finishes Ok, then two boundaries close the turn.
    let notifications = done.notifications();
    assert!(matches!(notifications[0], Notification::ToolCallStarted(_)));
    let Notification::ToolCallFinished(finished) = &notifications[1] else {
        panic!("approved tool finishes");
    };
    assert_eq!(finished.response().status, ToolStatus::Ok);
    assert!(matches!(notifications[2], Notification::StepBoundary(_)));
    assert!(matches!(notifications[3], Notification::StepBoundary(_)));

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    let turn = &conversation.turns()[0];
    assert_tool_result(turn.messages()[2].payload(), "call-weather", ToolStatus::Ok);
    assert_text(turn.messages()[3].payload(), "approved result used");
    assert_eq!(registry.calls().len(), 1);
}

#[tokio::test]
async fn reference_headless_scope_surfaces_unhandled_approval() {
    // Run mode = scope wiring (migration doc §4.4 / §6): the *same* machine
    // `reference_approval_approve_matches_default_loop` drives to completion under
    // an attended scope instead surfaces a classified `UnhandledRequirement` under
    // a headless top-level scope with no interaction backend — never a silent skip
    // or hang, and the guarded tool never runs.
    let client = Arc::new(FakeClient::with_chats(vec![Ok(tool_use_response(
        vec![("call-weather", "get_weather", json!({ "city": "Shanghai" }))],
        usage(5, 2),
    ))]));
    let registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(710)],
        vec![message_id_seed(711)],
        vec![message_id_seed(712)],
        vec![step_id_seed(713)],
    ));
    let mut machine = machine_with(
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
        ids,
        Arc::new(RequireApprovalPolicy::new("human approval required")),
    );
    // Headless: identical wiring to the approve test, minus the interaction backend.
    let scope = ReferenceScope::new(client, registry.clone());
    let ctx = context();

    let error = drive_turn(&mut machine, input(), &scope, &ctx)
        .await
        .expect_err("a headless top scope cannot fulfill the approval");

    assert_eq!(
        error.kind(),
        crate::agent::AgentErrorKind::UnhandledRequirement
    );
    match error {
        crate::agent::AgentError::UnhandledRequirement { kind, .. } => {
            assert_eq!(kind, RequirementKindTag::Interaction);
        }
        other => panic!("expected UnhandledRequirement, got {other:?}"),
    }
    // The approval was neither auto-granted nor skipped: the guarded tool never ran.
    assert!(registry.calls().is_empty());
}

#[tokio::test]
async fn reference_approval_deny_matches_default_loop() {
    let client = Arc::new(FakeClient::with_chats(vec![
        Ok(tool_use_response(
            vec![
                ("call-deny", "get_weather", json!({ "city": "Private" })),
                ("call-timeout", "get_weather", json!({ "city": "Slow" })),
                ("call-cancel", "get_weather", json!({ "city": "Cancelled" })),
            ],
            usage(8, 3),
        )),
        Ok(assistant_response(
            "handled approval decisions",
            usage(9, 5),
        )),
    ]));
    let registry = Arc::new(FakeToolRegistry::new(Vec::new()));
    let ids = Arc::new(FakeToolIds::new(
        vec![
            tool_call_id_seed(720),
            tool_call_id_seed(721),
            tool_call_id_seed(722),
        ],
        vec![
            message_id_seed(723),
            message_id_seed(724),
            message_id_seed(725),
        ],
        vec![message_id_seed(726)],
        vec![step_id_seed(727)],
    ));
    let mut machine = machine_with(
        spec_with_tools(3, ToolFailurePolicy::ReturnErrorToModel),
        ids,
        Arc::new(RequireApprovalPolicy::new("approval required")),
    );
    let interaction = ScriptedApprovalInteraction::new(vec![
        (
            tool_call_id_seed(720),
            ApprovalDecision::Deny,
            Some("denied by policy".to_owned()),
        ),
        (
            tool_call_id_seed(721),
            ApprovalDecision::Timeout,
            Some("approval timed out".to_owned()),
        ),
        (
            tool_call_id_seed(722),
            ApprovalDecision::Cancel,
            Some("cancelled by approver".to_owned()),
        ),
    ]);
    let scope = ComposedScope {
        base: ReferenceScope::new(client, registry.clone()),
        interaction: &interaction,
    };
    let ctx = context();

    let done = crate::agent::drain(&mut machine, input(), &scope, None, &ctx)
        .await
        .expect("loop recovers after denials");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    let finished_statuses = done
        .notifications()
        .iter()
        .filter_map(|event| match event {
            Notification::ToolCallFinished(finished) => Some(finished.response().status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        finished_statuses,
        vec![
            ToolStatus::Denied,
            ToolStatus::Denied,
            ToolStatus::Cancelled
        ]
    );

    let conversation = machine.state().conversation();
    let turn = &conversation.turns()[0];
    assert_tool_result(
        turn.messages()[2].payload(),
        "call-deny",
        ToolStatus::Denied,
    );
    assert_tool_result(
        turn.messages()[3].payload(),
        "call-timeout",
        ToolStatus::Denied,
    );
    assert_tool_result(
        turn.messages()[4].payload(),
        "call-cancel",
        ToolStatus::Cancelled,
    );
    assert_text(turn.messages()[5].payload(), "handled approval decisions");
    assert!(registry.calls().is_empty());
}

// ----- cancellation (M4-1) -----

/// LLM handler that cancels the run context as it returns its one scripted
/// response, modelling a "stop" signal that arrives mid-turn.
struct CancellingLlmHandler {
    response: Mutex<Option<Response>>,
}

#[async_trait]
impl crate::agent::LlmHandler for CancellingLlmHandler {
    async fn fulfill(
        &self,
        _request: &ChatRequest,
        _mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        ctx.cancellation().cancel();
        let response = self
            .response
            .lock()
            .expect("response mutex")
            .take()
            .expect("exactly one scripted llm response");
        RequirementResult::Llm(Ok(response))
    }
}

/// Tool handler that must never run: cancellation abandons the batch first.
struct PanicToolHandler;

#[async_trait]
impl crate::agent::ToolHandler for PanicToolHandler {
    async fn fulfill(
        &self,
        _call_id: ToolCallId,
        _call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        panic!("cancellation must abandon the tool batch before any tool executes");
    }
}

/// Scope wiring the cancelling LLM handler with a tool handler that panics.
struct CancelScope {
    llm: CancellingLlmHandler,
    tool: PanicToolHandler,
}

impl crate::agent::HandlerScope for CancelScope {
    fn llm(&self) -> Option<&dyn crate::agent::LlmHandler> {
        Some(&self.llm)
    }

    fn tool(&self) -> Option<&dyn crate::agent::ToolHandler> {
        Some(&self.tool)
    }
}

#[tokio::test]
async fn reference_cancel_during_tool_wait_abandons_turn() {
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(100)],
        vec![message_id_seed(101)],
        vec![message_id_seed(102)],
        vec![step_id_seed(103)],
    ));
    let mut machine = machine_with(
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
        ids,
        Arc::new(crate::agent::NoApprovalPolicy),
    );
    let scope = CancelScope {
        llm: CancellingLlmHandler {
            response: Mutex::new(Some(tool_use_response(
                vec![("call-weather", "get_weather", json!({ "city": "Shanghai" }))],
                usage(5, 2),
            ))),
        },
        tool: PanicToolHandler,
    };
    let ctx = context();

    let done = crate::agent::drain(&mut machine, input(), &scope, None, &ctx)
        .await
        .expect("a cancelled turn drains to a rest state");

    // Never-resume: the emitted tool batch is abandoned, the cursor settles to a
    // feedable Idle, and the pending turn is coherent (its tool_use closed by a
    // synthesized cancelled result) with nothing committed to history.
    assert!(matches!(done.cursor(), LoopCursor::Idle));
    assert_eq!(machine.state().loop_cursor().kind(), LoopCursorKind::Idle);
    let conversation = machine.state().conversation();
    let pending = conversation
        .pending()
        .expect("cancellation leaves a coherent pending turn");
    assert_eq!(pending.open_calls().count(), 0);
    assert_eq!(pending.tool_calls().len(), 1);
    assert!(conversation.turns().is_empty());

    // The turn never resumes, so no tool ran and no step boundary was emitted.
    assert!(done.notifications().iter().all(|event| !matches!(
        event,
        Notification::ToolCallFinished(_) | Notification::StepBoundary(_)
    )));
}

#[tokio::test]
async fn reference_new_turn_after_cancel_starts_fresh() {
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(100)],
        vec![message_id_seed(101)],
        vec![message_id_seed(102)],
        vec![step_id_seed(103)],
    ));
    let mut machine = machine_with(
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
        ids,
        Arc::new(crate::agent::NoApprovalPolicy),
    );
    let cancel_scope = CancelScope {
        llm: CancellingLlmHandler {
            response: Mutex::new(Some(tool_use_response(
                vec![("call-weather", "get_weather", json!({ "city": "Shanghai" }))],
                usage(5, 2),
            ))),
        },
        tool: PanicToolHandler,
    };
    let ctx = context();
    let _ = crate::agent::drain(&mut machine, input(), &cancel_scope, None, &ctx)
        .await
        .expect("first turn is cancelled");
    assert!(matches!(machine.state().loop_cursor(), LoopCursor::Idle));

    // A fresh, uncancelled turn discards the interrupted pending and completes.
    let client = Arc::new(FakeClient::with_chats(vec![Ok(assistant_response(
        "hello again",
        usage(3, 5),
    ))]));
    let registry = Arc::new(FakeToolRegistry::new(Vec::new()));
    let scope = ReferenceScope::new(client, registry);
    let fresh_ctx = context();
    let second_input = AgentInput::user_message(
        format!("018f0d9c-7b6a-7c12-8f31-{:012x}", 900_u64)
            .parse()
            .expect("second turn id"),
        message_id_seed(901),
        user_message("hello again"),
        message_id_seed(902),
        step_id_seed(903),
    )
    .expect("valid second user input");

    let done = drive_turn(&mut machine, second_input, &scope, &fresh_ctx)
        .await
        .expect("the follow-up turn completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 2);
    assert_text(turn.messages()[1].payload(), "hello again");
}

// ----- turn-boundary reconfiguration tests -----

/// A reconfiguration queued while the machine is idle is applied at the start of
/// the next turn: the driver resolves and installs the new registry through the
/// [`ReconfigRegistryHandler`], and the opening request already reflects the new
/// tool set and system-prompt overlay. A start-of-turn application writes no
/// `reconfigs` boundary metadata (only a during-turn change does).
#[tokio::test]
async fn reference_idle_queued_reconfig_applies_at_next_turn_start() {
    let client = Arc::new(FakeClient::with_chats(vec![Ok(assistant_response(
        "done",
        usage(3, 5),
    ))]));
    let registry = Arc::new(FakeToolRegistry::new(Vec::new()));
    let ids = Arc::new(FakeToolIds::new(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ));
    let mut machine = machine_with(spec(), ids, Arc::new(crate::agent::NoApprovalPolicy));

    // Queue the reconfiguration while idle, before the turn opens.
    machine
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("Use calendar context.".to_owned()),
            0,
        ))
        .expect("system overlay reconfig queued");
    machine
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement_tool_set(),
        })
        .expect("tool set reconfig queued");

    let scope = ReferenceScope::new(client.clone(), registry);
    let ctx = context();
    let done = drive_turn(&mut machine, input(), &scope, &ctx)
        .await
        .expect("the reconfigured turn completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    // The opening request already advertises the new tool set + overlay.
    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].tools, vec![calendar_tool()]);
    assert_eq!(
        requests[0].system.as_deref(),
        Some("Conversation system.\n\nUse calendar context.")
    );

    // A start-of-turn application carries no reconfig boundary metadata.
    let boundaries: Vec<_> = done
        .notifications()
        .iter()
        .filter_map(|event| match event {
            Notification::StepBoundary(boundary) => Some(boundary),
            _ => None,
        })
        .collect();
    assert_eq!(boundaries.len(), 1);
    assert!(boundaries[0].metadata().get("reconfigs").is_none());

    // State reflects the applied reconfiguration for subsequent turns.
    assert!(machine.state().queued_reconfigs().is_empty());
    assert_eq!(
        machine.state().system_prompt_overlay(),
        Some("Use calendar context.")
    );
    assert_eq!(machine.state().current_tool_set(), &replacement_tool_set());
}

/// A tool-set reconfiguration swaps the *executable* registry end-to-end: the
/// reference driver resolves the queued set through a
/// [`StaticToolRegistryResolver`], installs the resolved registry into the
/// shared slot, and the ensuing tool call runs against the new registry while
/// the old one is never touched.
#[tokio::test]
async fn reference_reconfig_swaps_executable_registry_end_to_end() {
    let client = Arc::new(FakeClient::with_chats(vec![
        Ok(tool_use_response(
            vec![("call-cal", "read_calendar", json!({ "day": "Monday" }))],
            usage(5, 2),
        )),
        Ok(assistant_response("checked calendar", usage(3, 5))),
    ]));
    let old_registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let new_registry = Arc::new(FakeToolRegistry::with_declarations(
        vec![calendar_tool()],
        vec![Ok(tool_response(
            "call-cal",
            "Free all day",
            ToolStatus::Ok,
        ))],
    ));
    let mut resolver = StaticToolRegistryResolver::new();
    resolver
        .insert(tool_set_id(), old_registry.clone())
        .expect("initial registry inserted");
    resolver
        .insert(replacement_tool_set_id(), new_registry.clone())
        .expect("replacement registry inserted");

    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(1_600)],
        vec![message_id_seed(1_601)],
        vec![message_id_seed(1_602)],
        vec![step_id_seed(1_603)],
    ));
    let mut machine = machine_with(spec(), ids, Arc::new(crate::agent::NoApprovalPolicy));
    machine
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement_tool_set(),
        })
        .expect("tool set reconfig queued while idle");

    let scope = ReferenceScope::new(client.clone(), old_registry.clone())
        .with_tool_registry_resolver(Arc::new(resolver));
    let ctx = context();
    let done = drive_turn(&mut machine, input(), &scope, &ctx)
        .await
        .expect("the reconfigured tool turn completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    // The swapped-in registry executed the call; the old registry never did.
    assert_eq!(new_registry.calls().len(), 1);
    assert!(old_registry.calls().is_empty());
    assert_eq!(new_registry.calls()[0].1.name, "read_calendar");

    // The opening request already advertised the new tool set.
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools, vec![calendar_tool()]);
    assert_eq!(machine.state().current_tool_set(), &replacement_tool_set());
}
