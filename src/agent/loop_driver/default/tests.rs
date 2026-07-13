use super::{DefaultAgentLoop, LlmStepMode};
use crate::{
    agent::{
        AgentErrorKind, AgentEvent, AgentInput, AgentLoop, AgentSpec, BudgetLimits, LoopCursorKind,
        LoopPolicy, ModelRef, RunContext, RunId, StepId, ToolExecutionIds, ToolFailurePolicy,
        ToolRegistry, ToolRuntimeError, ToolSetId, ToolSetRef, TraceNodeId, WorktreeRef,
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
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use async_trait::async_trait;
use futures::{StreamExt, stream};
use serde_json::Map;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

type FakeStreamEvents = Vec<Result<StreamEvent, ClientError>>;
type FakeStreamResult = Result<FakeStreamEvents, ClientError>;

#[derive(Debug)]
struct FakeClient {
    capability: Capability,
    chat_results: Mutex<VecDeque<Result<Response, ClientError>>>,
    stream_results: Mutex<VecDeque<FakeStreamResult>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl FakeClient {
    fn with_chat(result: Result<Response, ClientError>) -> Self {
        Self::with_chats(vec![result])
    }

    fn with_chats(results: Vec<Result<Response, ClientError>>) -> Self {
        Self {
            capability: Capability::default(),
            chat_results: Mutex::new(VecDeque::from(results)),
            stream_results: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn with_stream(events: Vec<Result<StreamEvent, ClientError>>) -> Self {
        Self {
            capability: Capability::default(),
            chat_results: Mutex::new(VecDeque::new()),
            stream_results: Mutex::new(VecDeque::from([Ok(events)])),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("requests mutex").clone()
    }
}

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
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation message".to_owned(),
            })
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        self.step_ids
            .lock()
            .expect("step id mutex")
            .pop_front()
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation step".to_owned(),
            })
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
        request: ChatRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError>
    {
        self.requests.lock().expect("requests mutex").push(request);
        let events = self
            .stream_results
            .lock()
            .expect("stream results mutex")
            .pop_front()
            .expect("fake stream result")?;
        Ok(stream::iter(events).boxed())
    }
}

fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("test value is non-zero")
}

fn agent_id() -> crate::agent::AgentId {
    "018f0d9c-7b6a-7c12-8f31-123456789001"
        .parse()
        .expect("agent id")
}

fn tool_set_id() -> ToolSetId {
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
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        }),
    }
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

fn state() -> crate::agent::AgentState {
    state_with_spec(spec())
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

async fn collect_events(
    mut events: crate::agent::AgentEventStream,
) -> Result<Vec<AgentEvent>, crate::agent::AgentError> {
    let mut collected = Vec::new();
    while let Some(event) = events.next().await {
        collected.push(event?);
    }
    Ok(collected)
}

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

fn tool_loop(
    client: Arc<FakeClient>,
    registry: Arc<FakeToolRegistry>,
    ids: Arc<FakeToolIds>,
    spec: AgentSpec,
) -> DefaultAgentLoop {
    let llm: Arc<dyn LlmClient> = client;
    let tool_registry: Arc<dyn ToolRegistry> = registry;
    let tool_ids: Arc<dyn ToolExecutionIds> = ids;
    DefaultAgentLoop::with_tool_registry(
        llm,
        state_with_spec(spec),
        context(),
        LlmStepMode::NonStreaming,
        tool_registry,
        tool_ids,
    )
}

#[tokio::test]
async fn non_streaming_text_response_commits_turn_and_emits_boundary_done() {
    let response_usage = usage(3, 5);
    let client = Arc::new(FakeClient::with_chat(Ok(assistant_response(
        "hi",
        response_usage.clone(),
    ))));
    let llm: Arc<dyn LlmClient> = client.clone();
    let mut loop_impl = DefaultAgentLoop::new(llm, state(), context(), LlmStepMode::NonStreaming);

    let events = loop_impl.feed(input()).await.expect("feed starts");
    assert!(loop_impl.feed_in_progress());

    let events = collect_events(events).await.expect("events succeed");
    assert!(!loop_impl.feed_in_progress());
    assert_eq!(events.len(), 2);

    let AgentEvent::StepBoundary(boundary) = &events[0] else {
        panic!("first event is step boundary");
    };
    assert_eq!(boundary.step_id(), step_id());
    assert_eq!(boundary.boundary().turn_count(), 1);
    assert_eq!(boundary.boundary().version(), 1);
    assert_eq!(
        boundary.trace_node_id().map(TraceNodeId::as_str),
        Some(step_id().to_string().as_str())
    );
    assert_eq!(
        events[1],
        AgentEvent::Done(crate::agent::AgentOutcome::Completed)
    );

    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.loop_cursor().kind(), LoopCursorKind::Idle);
            assert!(state.conversation().pending().is_none());
            assert_eq!(state.conversation().turns().len(), 1);
            let turn = &state.conversation().turns()[0];
            assert_eq!(turn.messages().len(), 2);
            assert_text(turn.messages()[0].payload(), "hello");
            assert_text(turn.messages()[1].payload(), "hi");
            assert_eq!(turn.meta().usage(), &response_usage);
            assert_eq!(state.conversation().version(), 1);
        })
        .expect("inspect state");

    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].stream);
    assert_eq!(requests[0].model, "gpt-5.5");
    assert_eq!(requests[0].max_tokens, 512);
    assert_eq!(requests[0].temperature, Some(0.1));
    assert_eq!(requests[0].system.as_deref(), Some("Conversation system."));
    assert_eq!(requests[0].messages.len(), 1);
    assert_text(&requests[0].messages[0], "hello");
}

#[tokio::test]
async fn streaming_text_response_forwards_llm_events_and_commits_turn() {
    let text = BlockId::new("text-1");
    let stream_usage = usage(4, 6);
    let stream_events = vec![
        Ok(StreamEvent::MessageStart {
            role: Role::Assistant,
        }),
        Ok(StreamEvent::BlockStart {
            id: text.clone(),
            kind: BlockKind::Text,
        }),
        Ok(StreamEvent::BlockDelta {
            id: text.clone(),
            delta: Delta::Text("he".to_owned()),
        }),
        Ok(StreamEvent::BlockDelta {
            id: text.clone(),
            delta: Delta::Text("llo".to_owned()),
        }),
        Ok(StreamEvent::BlockStop { id: text }),
        Ok(StreamEvent::Usage(stream_usage.clone())),
        Ok(StreamEvent::MessageStop {
            stop_reason: StopReason::normalize("end_turn"),
        }),
    ];
    let client = Arc::new(FakeClient::with_stream(stream_events));
    let llm: Arc<dyn LlmClient> = client.clone();
    let mut loop_impl = DefaultAgentLoop::new(llm, state(), context(), LlmStepMode::Streaming);

    let events = collect_events(loop_impl.feed(input()).await.expect("feed starts"))
        .await
        .expect("stream succeeds");

    assert_eq!(events.len(), 9);
    assert!(matches!(
        events[0],
        AgentEvent::Llm(StreamEvent::MessageStart { .. })
    ));
    assert!(matches!(
        events[6],
        AgentEvent::Llm(StreamEvent::MessageStop { .. })
    ));
    assert!(matches!(events[7], AgentEvent::StepBoundary(_)));
    assert_eq!(
        events[8],
        AgentEvent::Done(crate::agent::AgentOutcome::Completed)
    );

    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.loop_cursor().kind(), LoopCursorKind::Idle);
            assert!(state.conversation().pending().is_none());
            let turn = &state.conversation().turns()[0];
            assert_text(turn.messages()[1].payload(), "hello");
            assert_eq!(turn.meta().usage(), &stream_usage);
        })
        .expect("inspect state");

    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].stream);
    assert_eq!(requests[0].messages.len(), 1);
}

#[tokio::test]
async fn client_error_discards_pending_without_committing() {
    let client = Arc::new(FakeClient::with_chat(Err(ClientError::Timeout)));
    let llm: Arc<dyn LlmClient> = client;
    let mut loop_impl = DefaultAgentLoop::new(llm, state(), context(), LlmStepMode::NonStreaming);

    let error = loop_impl
        .feed(input())
        .await
        .expect_err("client error should fail feed");

    assert!(matches!(
        error,
        crate::agent::AgentError::Client(ClientError::Timeout)
    ));
    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.loop_cursor().kind(), LoopCursorKind::Idle);
            assert!(state.conversation().pending().is_none());
            assert!(state.conversation().turns().is_empty());
            assert_eq!(state.conversation().version(), 0);
        })
        .expect("inspect state");
}

#[tokio::test]
async fn invalid_assistant_response_discards_pending_without_committing() {
    let invalid = Response {
        message: user_message("not an assistant"),
        usage: Usage::default(),
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    };
    let client = Arc::new(FakeClient::with_chat(Ok(invalid)));
    let llm: Arc<dyn LlmClient> = client;
    let mut loop_impl = DefaultAgentLoop::new(llm, state(), context(), LlmStepMode::NonStreaming);

    let error = loop_impl
        .feed(input())
        .await
        .expect_err("invalid assistant response should fail feed");

    assert_eq!(error.kind(), crate::agent::AgentErrorKind::Conversation);
    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.loop_cursor().kind(), LoopCursorKind::Idle);
            assert!(state.conversation().pending().is_none());
            assert!(state.conversation().turns().is_empty());
            assert_eq!(state.conversation().version(), 0);
        })
        .expect("inspect state");
}

#[tokio::test]
async fn non_streaming_single_tool_executes_result_and_commits_final_assistant() {
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
    let mut loop_impl = tool_loop(
        client.clone(),
        registry.clone(),
        ids,
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
    );

    let events = collect_events(loop_impl.feed(input()).await.expect("feed starts"))
        .await
        .expect("tool loop succeeds");

    assert_eq!(events.len(), 4);
    let AgentEvent::ToolCallStarted(started) = &events[0] else {
        panic!("first event starts tool");
    };
    assert_eq!(started.step_id(), step_id());
    assert_eq!(started.call_id(), tool_call_id_seed(100));
    assert_eq!(started.call().id, "call-weather");

    let AgentEvent::ToolCallFinished(finished) = &events[1] else {
        panic!("second event finishes tool");
    };
    assert_eq!(finished.step_id(), step_id());
    assert_eq!(finished.call_id(), tool_call_id_seed(100));
    assert_eq!(finished.response().status, ToolStatus::Ok);
    assert!(matches!(events[2], AgentEvent::StepBoundary(_)));
    assert_eq!(
        events[3],
        AgentEvent::Done(crate::agent::AgentOutcome::Completed)
    );

    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.loop_cursor().kind(), LoopCursorKind::Idle);
            assert!(state.conversation().pending().is_none());
            assert_eq!(state.conversation().turns().len(), 1);
            let turn = &state.conversation().turns()[0];
            assert_eq!(turn.messages().len(), 4);
            assert_text(turn.messages()[0].payload(), "hello");
            assert_eq!(turn.messages()[1].payload().role, Role::Assistant);
            assert_tool_result(turn.messages()[2].payload(), "call-weather", ToolStatus::Ok);
            assert_text(turn.messages()[3].payload(), "sunny in Shanghai");
            assert_eq!(turn.pairings().len(), 1);
            assert_eq!(turn.pairings()[0].call_id(), tool_call_id_seed(100));
            assert_eq!(turn.pairings()[0].result_msg(), message_id_seed(101));
        })
        .expect("inspect state");

    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools, vec![weather_tool()]);
    assert_eq!(requests[0].messages.len(), 1);
    assert_eq!(requests[1].messages.len(), 3);

    let calls = registry.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, tool_call_id_seed(100));
    assert_eq!(calls[0].1.name, "get_weather");
}

#[tokio::test]
async fn non_streaming_parallel_tools_start_before_finishing_and_commit_pairings() {
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
    let mut loop_impl = tool_loop(
        client,
        registry,
        ids,
        spec_with_tools(2, ToolFailurePolicy::ReturnErrorToModel),
    );

    let events = collect_events(loop_impl.feed(input()).await.expect("feed starts"))
        .await
        .expect("parallel tool loop succeeds");

    assert!(matches!(events[0], AgentEvent::ToolCallStarted(_)));
    assert!(matches!(events[1], AgentEvent::ToolCallStarted(_)));
    assert!(matches!(events[2], AgentEvent::ToolCallFinished(_)));
    assert!(matches!(events[3], AgentEvent::ToolCallFinished(_)));

    loop_impl
        .inspect_state(|state| {
            let turn = &state.conversation().turns()[0];
            assert_eq!(turn.pairings().len(), 2);
            assert_eq!(turn.pairings()[0].call_id(), tool_call_id_seed(200));
            assert_eq!(turn.pairings()[0].result_msg(), message_id_seed(202));
            assert_eq!(turn.pairings()[1].call_id(), tool_call_id_seed(201));
            assert_eq!(turn.pairings()[1].result_msg(), message_id_seed(203));
        })
        .expect("inspect state");
}

#[tokio::test]
async fn tool_error_and_denied_results_are_returned_to_model_for_recovery() {
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
    let mut loop_impl = tool_loop(
        client,
        registry,
        ids,
        spec_with_tools(2, ToolFailurePolicy::ReturnErrorToModel),
    );

    let events = collect_events(loop_impl.feed(input()).await.expect("feed starts"))
        .await
        .expect("tool errors are returned to model");

    let finished_statuses = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallFinished(finished) => Some(finished.response().status),
            AgentEvent::Llm(_)
            | AgentEvent::StepBoundary(_)
            | AgentEvent::ToolCallStarted(_)
            | AgentEvent::AwaitingApproval(_)
            | AgentEvent::Done(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        finished_statuses,
        vec![ToolStatus::Denied, ToolStatus::Error]
    );

    loop_impl
        .inspect_state(|state| {
            assert!(state.conversation().pending().is_none());
            let turn = &state.conversation().turns()[0];
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
        })
        .expect("inspect state");
}

#[tokio::test]
async fn duplicate_framework_tool_call_id_is_rejected_without_commit() {
    let client = Arc::new(FakeClient::with_chats(vec![Ok(tool_use_response(
        vec![
            ("call-a", "get_weather", json!({ "city": "Shanghai" })),
            ("call-b", "get_weather", json!({ "city": "Tokyo" })),
        ],
        usage(8, 3),
    ))]));
    let registry = Arc::new(FakeToolRegistry::new(Vec::new()));
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(400), tool_call_id_seed(400)],
        vec![message_id_seed(401), message_id_seed(402)],
        vec![message_id_seed(403)],
        vec![step_id_seed(404)],
    ));
    let mut loop_impl = tool_loop(
        client,
        registry,
        ids,
        spec_with_tools(2, ToolFailurePolicy::ReturnErrorToModel),
    );

    let error = loop_impl
        .feed(input())
        .await
        .expect_err("duplicate framework call id should reject feed");

    assert_eq!(error.kind(), AgentErrorKind::Conversation);
    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.loop_cursor().kind(), LoopCursorKind::Idle);
            assert!(state.conversation().pending().is_none());
            assert!(state.conversation().turns().is_empty());
            assert_eq!(state.conversation().version(), 0);
        })
        .expect("inspect state");
}

#[tokio::test]
async fn tool_response_for_unknown_provider_call_is_rejected_without_commit() {
    let client = Arc::new(FakeClient::with_chats(vec![Ok(tool_use_response(
        vec![("call-a", "get_weather", json!({ "city": "Shanghai" }))],
        usage(5, 2),
    ))]));
    let registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "unknown-call",
        "wrong result",
        ToolStatus::Ok,
    ))]));
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(500)],
        vec![message_id_seed(501)],
        vec![message_id_seed(502)],
        vec![step_id_seed(503)],
    ));
    let mut loop_impl = tool_loop(
        client,
        registry,
        ids,
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
    );

    let error = loop_impl
        .feed(input())
        .await
        .expect_err("unknown provider call result should reject feed");

    assert_eq!(error.kind(), AgentErrorKind::Conversation);
    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.loop_cursor().kind(), LoopCursorKind::Idle);
            assert!(state.conversation().pending().is_none());
            assert!(state.conversation().turns().is_empty());
            assert_eq!(state.conversation().version(), 0);
        })
        .expect("inspect state");
}
