use super::{DefaultAgentLoop, LlmStepMode};
use crate::{
    agent::{
        AgentErrorKind, AgentEvent, AgentInput, AgentLoop, AgentSpec, BudgetLimits, LoopCursorKind,
        LoopPolicy, ModelRef, PivotSource, QueuedPivot, ReconfigRequest, RunContext, RunId,
        StaticToolRegistryResolver, StepId, ToolExecutionIds, ToolFailurePolicy, ToolRegistry,
        ToolRegistryResolver, ToolRuntimeError, ToolSetId, ToolSetRef, TraceNodeId, WorktreeRef,
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
        Self::with_streams(vec![Ok(events)])
    }

    fn with_streams(results: Vec<FakeStreamResult>) -> Self {
        Self {
            capability: Capability::default(),
            chat_results: Mutex::new(VecDeque::new()),
            stream_results: Mutex::new(VecDeque::from(results)),
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
        Self::with_declarations(vec![weather_tool()], results)
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

fn tool_set_id_seed(seed: u64) -> ToolSetId {
    format!("018f0d9c-7b6a-7c12-8f31-{seed:012x}")
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

fn turn_id_seed(seed: u64) -> TurnId {
    format!("018f0d9c-7b6a-7c12-8f31-{seed:012x}")
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

fn skill_id_seed(seed: u64) -> crate::agent::SkillId {
    format!("018f0d9c-7b6a-7c12-8f31-{seed:012x}")
        .parse()
        .expect("skill id")
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

fn calendar_tool() -> Tool {
    Tool {
        name: "read_calendar".to_owned(),
        description: "Read calendar availability.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "day": { "type": "string" }
            },
            "required": ["day"]
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

fn input_seed(seed: u64, text: &str) -> AgentInput {
    AgentInput::user_message(
        turn_id_seed(seed),
        message_id_seed(seed + 1),
        user_message(text),
        message_id_seed(seed + 2),
        step_id_seed(seed + 3),
    )
    .expect("valid user input")
}

fn queued_pivot_turn_input(seed: u64) -> AgentInput {
    AgentInput::queued_pivot_turn(
        turn_id_seed(seed),
        message_id_seed(seed + 1),
        step_id_seed(seed + 2),
    )
}

fn pivot(seed: u64, text: &str) -> QueuedPivot {
    QueuedPivot::new(
        message_id_seed(seed),
        user_message(text),
        PivotSource::Human,
    )
    .expect("valid pivot")
}

fn tool_use_stream_events(provider_call_id: &str) -> Vec<Result<StreamEvent, ClientError>> {
    let tool = BlockId::new(format!("tool-{provider_call_id}"));
    vec![
        Ok(StreamEvent::MessageStart {
            role: Role::Assistant,
        }),
        Ok(StreamEvent::BlockStart {
            id: tool.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: provider_call_id.to_owned(),
            },
        }),
        Ok(StreamEvent::BlockDelta {
            id: tool.clone(),
            delta: Delta::Json("{\"city\":\"Shanghai\"}".to_owned()),
        }),
        Ok(StreamEvent::ToolInputAvailable {
            id: tool.clone(),
            input: json!({ "city": "Shanghai" }),
        }),
        Ok(StreamEvent::BlockStop { id: tool }),
        Ok(StreamEvent::Usage(usage(5, 2))),
        Ok(StreamEvent::MessageStop {
            stop_reason: StopReason::normalize("tool_use"),
        }),
    ]
}

fn text_stream_events(text: &str) -> Vec<Result<StreamEvent, ClientError>> {
    let block = BlockId::new(format!("text-{text}"));
    vec![
        Ok(StreamEvent::MessageStart {
            role: Role::Assistant,
        }),
        Ok(StreamEvent::BlockStart {
            id: block.clone(),
            kind: BlockKind::Text,
        }),
        Ok(StreamEvent::BlockDelta {
            id: block.clone(),
            delta: Delta::Text(text.to_owned()),
        }),
        Ok(StreamEvent::BlockStop { id: block }),
        Ok(StreamEvent::Usage(usage(7, 4))),
        Ok(StreamEvent::MessageStop {
            stop_reason: StopReason::normalize("end_turn"),
        }),
    ]
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

fn pivot_records(boundary: &crate::agent::StepBoundary) -> &[Value] {
    boundary
        .metadata()
        .get("pivots")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .expect("pivot metadata records")
}

fn reconfig_records(boundary: &crate::agent::StepBoundary) -> &[Value] {
    boundary
        .metadata()
        .get("reconfigs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .expect("reconfig metadata records")
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

fn tool_loop_with_resolver(
    client: Arc<FakeClient>,
    registry: Arc<FakeToolRegistry>,
    ids: Arc<FakeToolIds>,
    spec: AgentSpec,
    resolver: Arc<dyn ToolRegistryResolver>,
    mode: LlmStepMode,
) -> DefaultAgentLoop {
    let llm: Arc<dyn LlmClient> = client;
    let tool_registry: Arc<dyn ToolRegistry> = registry;
    let tool_ids: Arc<dyn ToolExecutionIds> = ids;
    DefaultAgentLoop::with_tool_registry_resolver(
        llm,
        state_with_spec(spec),
        context(),
        mode,
        tool_registry,
        tool_ids,
        resolver,
    )
}

fn streaming_tool_loop(
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
        LlmStepMode::Streaming,
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
async fn streaming_interject_does_not_interrupt_text_and_starts_next_pivot_turn() {
    let client = Arc::new(FakeClient::with_streams(vec![
        Ok(text_stream_events("first")),
        Ok(text_stream_events("pivot acknowledged")),
    ]));
    let llm: Arc<dyn LlmClient> = client.clone();
    let mut loop_impl = DefaultAgentLoop::new(llm, state(), context(), LlmStepMode::Streaming);

    let mut stream = loop_impl.feed(input()).await.expect("feed starts");
    let first = stream
        .next()
        .await
        .expect("first event")
        .expect("first event succeeds");
    assert!(matches!(
        first,
        AgentEvent::Llm(StreamEvent::MessageStart { .. })
    ));

    loop_impl
        .interject(pivot(900, "change direction"))
        .expect("pivot accepted");

    let mut events = vec![first];
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event succeeds"));
    }

    let AgentEvent::StepBoundary(boundary) = &events[events.len() - 2] else {
        panic!("penultimate event is final boundary");
    };
    let records = pivot_records(boundary);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["status"], json!("deferred"));
    assert_eq!(records[0]["target"], json!("next_turn"));

    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.queued_pivots().len(), 1);
            assert_eq!(state.conversation().turns().len(), 1);
            let turn = &state.conversation().turns()[0];
            assert_eq!(turn.messages().len(), 2);
            assert_text(turn.messages()[0].payload(), "hello");
            assert_text(turn.messages()[1].payload(), "first");
        })
        .expect("inspect state");

    let pivot_events = collect_events(
        loop_impl
            .feed(queued_pivot_turn_input(910))
            .await
            .expect("queued pivot feed starts"),
    )
    .await
    .expect("queued pivot turn succeeds");
    assert!(matches!(
        pivot_events[pivot_events.len() - 2],
        AgentEvent::StepBoundary(_)
    ));

    loop_impl
        .inspect_state(|state| {
            assert!(state.queued_pivots().is_empty());
            assert_eq!(state.conversation().turns().len(), 2);
            let turn = &state.conversation().turns()[1];
            assert_eq!(turn.messages().len(), 2);
            assert_text(turn.messages()[0].payload(), "change direction");
            assert_text(turn.messages()[1].payload(), "pivot acknowledged");
        })
        .expect("inspect state");

    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_text(&requests[0].messages[0], "hello");
    assert_text(
        requests[1].messages.last().expect("current pivot user"),
        "change direction",
    );
}

#[tokio::test]
async fn interject_rejects_invalid_pivot_role_without_queueing() {
    let client = Arc::new(FakeClient::with_chat(Ok(assistant_response(
        "unused",
        usage(1, 1),
    ))));
    let llm: Arc<dyn LlmClient> = client;
    let loop_impl = DefaultAgentLoop::new(llm, state(), context(), LlmStepMode::NonStreaming);
    let invalid: QueuedPivot = serde_json::from_value(json!({
        "message_id": message_id_seed(950),
        "message": {
            "role": "assistant",
            "content": []
        },
        "source": {
            "source": "human"
        }
    }))
    .expect("raw serde can construct unchecked queued pivot data");

    let error = loop_impl
        .interject(invalid)
        .expect_err("invalid pivot role is rejected");
    assert_eq!(error.kind(), AgentErrorKind::AgentState);
    loop_impl
        .inspect_state(|state| assert!(state.queued_pivots().is_empty()))
        .expect("inspect state");
}

#[tokio::test]
async fn reconfig_queued_during_text_turn_applies_at_turn_boundary_and_next_request_changes() {
    let client = Arc::new(FakeClient::with_streams(vec![
        Ok(text_stream_events("first")),
        Ok(text_stream_events("second")),
    ]));
    let llm: Arc<dyn LlmClient> = client.clone();
    let mut loop_impl = DefaultAgentLoop::new(llm, state(), context(), LlmStepMode::Streaming);
    let replacement_tools = ToolSetRef::new(tool_set_id_seed(1_400), vec![calendar_tool()]);

    let mut stream = loop_impl.feed(input()).await.expect("feed starts");
    let first = stream
        .next()
        .await
        .expect("first event")
        .expect("first event succeeds");
    assert!(matches!(
        first,
        AgentEvent::Llm(StreamEvent::MessageStart { .. })
    ));

    loop_impl
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("Use calendar context.".to_owned()),
            0,
        ))
        .expect("system overlay reconfig queued");
    loop_impl
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement_tools.clone(),
        })
        .expect("tool set reconfig queued");

    let mut events = vec![first];
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event succeeds"));
    }

    let AgentEvent::StepBoundary(boundary) = &events[events.len() - 2] else {
        panic!("penultimate event is final boundary");
    };
    let records = reconfig_records(boundary);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["status"], json!("applied"));
    assert_eq!(records[0]["kind"], json!("set_system_prompt_overlay"));
    assert_eq!(records[1]["kind"], json!("replace_tool_set"));

    loop_impl
        .inspect_state(|state| {
            assert!(state.queued_reconfigs().is_empty());
            assert_eq!(state.system_prompt_overlay(), Some("Use calendar context."));
            assert_eq!(state.system_prompt_overlay_version(), 1);
            assert_eq!(state.current_tool_set(), &replacement_tools);
        })
        .expect("inspect state");

    let second = collect_events(
        loop_impl
            .feed(input_seed(1_410, "next"))
            .await
            .expect("second feed starts"),
    )
    .await
    .expect("second feed succeeds");
    assert!(matches!(
        second[second.len() - 2],
        AgentEvent::StepBoundary(_)
    ));

    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].tools.is_empty());
    assert_eq!(requests[1].tools, vec![calendar_tool()]);
    assert_eq!(
        requests[1].system.as_deref(),
        Some("Conversation system.\n\nUse calendar context.")
    );
}

#[tokio::test]
async fn reconfig_during_tool_turn_keeps_current_turn_registry_snapshot() {
    let client = Arc::new(FakeClient::with_streams(vec![
        Ok(tool_use_stream_events("call-weather")),
        Ok(text_stream_events("used old registry")),
        Ok(text_stream_events("next turn")),
    ]));
    let old_registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let new_registry = Arc::new(FakeToolRegistry::with_declarations(
        vec![calendar_tool()],
        Vec::new(),
    ));
    let new_tool_set = ToolSetRef::new(tool_set_id_seed(1_500), vec![calendar_tool()]);
    let mut resolver = StaticToolRegistryResolver::new();
    resolver
        .insert(tool_set_id(), old_registry.clone())
        .expect("initial registry inserted");
    resolver
        .insert(new_tool_set.id(), new_registry.clone())
        .expect("replacement registry inserted");
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(1_510)],
        vec![message_id_seed(1_511)],
        vec![message_id_seed(1_512)],
        vec![step_id_seed(1_513)],
    ));
    let mut loop_impl = tool_loop_with_resolver(
        client.clone(),
        old_registry.clone(),
        ids,
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
        Arc::new(resolver),
        LlmStepMode::Streaming,
    );

    let mut stream = loop_impl.feed(input()).await.expect("feed starts");
    for _ in 0..7 {
        stream
            .next()
            .await
            .expect("tool-use stream event")
            .expect("tool-use stream succeeds");
    }
    loop_impl
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: new_tool_set.clone(),
        })
        .expect("replacement queued during pending turn");
    let events = collect_events(stream).await.expect("tool turn completes");
    assert!(matches!(events[0], AgentEvent::ToolCallStarted(_)));

    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools, vec![weather_tool()]);
    assert_eq!(
        requests[1].tools,
        vec![weather_tool()],
        "assistant continuation in the same turn keeps the old registry"
    );
    assert_eq!(old_registry.calls().len(), 1);
    assert!(new_registry.calls().is_empty());
    loop_impl
        .inspect_state(|state| assert_eq!(state.current_tool_set(), &new_tool_set))
        .expect("inspect state");

    let _next = collect_events(
        loop_impl
            .feed(input_seed(1_520, "after reconfig"))
            .await
            .expect("next feed starts"),
    )
    .await
    .expect("next turn succeeds");
    let requests = client.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].tools, vec![calendar_tool()]);
}

#[tokio::test]
async fn pivot_and_reconfig_queues_share_final_boundary_without_interfering() {
    let client = Arc::new(FakeClient::with_streams(vec![
        Ok(text_stream_events("first")),
        Ok(text_stream_events("pivot turn")),
    ]));
    let llm: Arc<dyn LlmClient> = client.clone();
    let mut loop_impl = DefaultAgentLoop::new(llm, state(), context(), LlmStepMode::Streaming);

    let mut stream = loop_impl.feed(input()).await.expect("feed starts");
    stream
        .next()
        .await
        .expect("first event")
        .expect("first event succeeds");
    loop_impl
        .interject(pivot(1_600, "queued pivot"))
        .expect("pivot queued");
    loop_impl
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("Overlay for pivot turn.".to_owned()),
            0,
        ))
        .expect("reconfig queued");

    let events = collect_events(stream).await.expect("first turn completes");
    let AgentEvent::StepBoundary(boundary) = &events[events.len() - 2] else {
        panic!("penultimate event is final boundary");
    };
    assert_eq!(pivot_records(boundary)[0]["status"], json!("deferred"));
    assert_eq!(
        reconfig_records(boundary)[0]["kind"],
        json!("set_system_prompt_overlay")
    );
    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.queued_pivots().len(), 1);
            assert!(state.queued_reconfigs().is_empty());
            assert_eq!(
                state.system_prompt_overlay(),
                Some("Overlay for pivot turn.")
            );
        })
        .expect("inspect state");

    let _pivot_events = collect_events(
        loop_impl
            .feed(queued_pivot_turn_input(1_610))
            .await
            .expect("queued pivot feed starts"),
    )
    .await
    .expect("queued pivot turn succeeds");
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_text(
        requests[1].messages.last().expect("pivot user message"),
        "queued pivot",
    );
    assert_eq!(
        requests[1].system.as_deref(),
        Some("Conversation system.\n\nOverlay for pivot turn.")
    );
}

#[tokio::test]
async fn conflicting_reconfig_requests_are_rejected_atomically() {
    let client = Arc::new(FakeClient::with_chat(Ok(assistant_response(
        "unused",
        usage(1, 1),
    ))));
    let llm: Arc<dyn LlmClient> = client.clone();
    let active_skill = skill_id_seed(1_700);
    let mut initial_state = state();
    initial_state
        .replace_active_skills(vec![active_skill])
        .expect("active skill set");
    let loop_impl = DefaultAgentLoop::new(llm, initial_state, context(), LlmStepMode::NonStreaming);

    let duplicate_skill = loop_impl
        .reconfigure(ReconfigRequest::ActivateSkill {
            skill_id: active_skill,
        })
        .expect_err("duplicate skill activation is rejected");
    assert_eq!(duplicate_skill.kind(), AgentErrorKind::AgentState);
    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.active_skills(), &[active_skill]);
            assert!(state.queued_reconfigs().is_empty());
        })
        .expect("inspect state");

    loop_impl
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("first overlay".to_owned()),
            0,
        ))
        .expect("first overlay queued");
    let stale_overlay = loop_impl
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("stale overlay".to_owned()),
            0,
        ))
        .expect_err("stale overlay version is rejected");
    assert_eq!(stale_overlay.kind(), AgentErrorKind::AgentState);
    loop_impl
        .inspect_state(|state| {
            assert_eq!(state.queued_reconfigs().len(), 1);
            assert_eq!(state.system_prompt_overlay(), None);
            assert_eq!(state.system_prompt_overlay_version(), 0);
        })
        .expect("inspect state");

    let strict_client = Arc::new(FakeClient::with_chat(Ok(assistant_response(
        "unused",
        usage(1, 1),
    ))));
    let strict_registry = Arc::new(FakeToolRegistry::new(Vec::new()));
    let strict_ids = Arc::new(FakeToolIds::new(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ));
    let strict_loop = tool_loop_with_resolver(
        strict_client,
        strict_registry.clone(),
        strict_ids,
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
        Arc::new(StaticToolRegistryResolver::single(
            tool_set_id(),
            strict_registry,
        )),
        LlmStepMode::NonStreaming,
    );
    let unknown_tool_set = ToolSetRef::new(tool_set_id_seed(1_710), vec![calendar_tool()]);
    let unknown = strict_loop
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: unknown_tool_set,
        })
        .expect_err("unknown tool set is rejected");
    assert_eq!(unknown.kind(), AgentErrorKind::Tool);
    strict_loop
        .inspect_state(|state| {
            assert!(state.queued_reconfigs().is_empty());
            assert_eq!(state.current_tool_set().id(), tool_set_id());
        })
        .expect("inspect state");
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

    assert_eq!(events.len(), 5);
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
    let AgentEvent::StepBoundary(tool_boundary) = &events[2] else {
        panic!("third event is tool-result step boundary");
    };
    assert_eq!(tool_boundary.step_id(), step_id());
    assert_eq!(tool_boundary.boundary().turn_count(), 0);
    assert!(tool_boundary.metadata().is_empty());
    let AgentEvent::StepBoundary(final_boundary) = &events[3] else {
        panic!("fourth event is final step boundary");
    };
    assert_eq!(final_boundary.boundary().turn_count(), 1);
    assert_eq!(
        events[4],
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
async fn streaming_tool_result_boundary_injects_queued_pivots_fifo_in_same_turn() {
    let client = Arc::new(FakeClient::with_streams(vec![
        Ok(tool_use_stream_events("call-weather")),
        Ok(text_stream_events("used the pivot")),
    ]));
    let registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(1_000)],
        vec![message_id_seed(1_001)],
        vec![message_id_seed(1_002)],
        vec![step_id_seed(1_003)],
    ));
    let mut loop_impl = streaming_tool_loop(
        client.clone(),
        registry,
        ids,
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
    );

    let mut stream = loop_impl.feed(input()).await.expect("feed starts");
    let mut events = Vec::new();
    for _ in 0..7 {
        events.push(
            stream
                .next()
                .await
                .expect("first stream event")
                .expect("first stream event succeeds"),
        );
    }
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Llm(StreamEvent::MessageStop { .. }))
    ));

    loop_impl
        .interject(pivot(1_100, "first pivot"))
        .expect("first pivot accepted");
    loop_impl
        .interject(pivot(1_101, "second pivot"))
        .expect("second pivot accepted");

    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event succeeds"));
    }

    let boundary = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::StepBoundary(boundary) if boundary.metadata().contains_key("pivots") => {
                Some(boundary)
            }
            _ => None,
        })
        .expect("tool boundary carries pivot metadata");
    assert_eq!(boundary.step_id(), step_id());
    assert_eq!(boundary.boundary().turn_count(), 0);
    let records = pivot_records(boundary);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["status"], json!("applied"));
    assert_eq!(records[0]["target"], json!("pending_turn"));
    assert_eq!(records[0]["message_id"], json!(message_id_seed(1_100)));
    assert_eq!(records[1]["message_id"], json!(message_id_seed(1_101)));

    loop_impl
        .inspect_state(|state| {
            assert!(state.queued_pivots().is_empty());
            let turn = &state.conversation().turns()[0];
            assert_eq!(turn.messages().len(), 6);
            assert_text(turn.messages()[0].payload(), "hello");
            assert_tool_result(turn.messages()[2].payload(), "call-weather", ToolStatus::Ok);
            assert_text(turn.messages()[3].payload(), "first pivot");
            assert_text(turn.messages()[4].payload(), "second pivot");
            assert_text(turn.messages()[5].payload(), "used the pivot");
        })
        .expect("inspect state");

    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages.len(), 5);
    assert_text(&requests[1].messages[3], "first pivot");
    assert_text(&requests[1].messages[4], "second pivot");
}

#[tokio::test]
async fn rejected_pivot_is_reported_and_dropped_without_blocking_recovery() {
    let client = Arc::new(FakeClient::with_streams(vec![
        Ok(tool_use_stream_events("call-weather")),
        Ok(text_stream_events("continued")),
    ]));
    let registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let ids = Arc::new(FakeToolIds::new(
        vec![tool_call_id_seed(1_200)],
        vec![message_id_seed(1_201)],
        vec![message_id_seed(1_202)],
        vec![step_id_seed(1_203)],
    ));
    let mut loop_impl = streaming_tool_loop(
        client,
        registry,
        ids,
        spec_with_tools(1, ToolFailurePolicy::ReturnErrorToModel),
    );

    let mut stream = loop_impl.feed(input()).await.expect("feed starts");
    for _ in 0..7 {
        stream
            .next()
            .await
            .expect("first stream event")
            .expect("first stream event succeeds");
    }

    let duplicate_id_pivot = QueuedPivot::new(
        user_message_id(),
        user_message("duplicate id"),
        PivotSource::Human,
    )
    .expect("role is valid");
    loop_impl
        .interject(duplicate_id_pivot)
        .expect("pivot accepted into runtime queue");

    let events = collect_events(stream).await.expect("loop continues");
    let boundary = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::StepBoundary(boundary) if boundary.metadata().contains_key("pivots") => {
                Some(boundary)
            }
            _ => None,
        })
        .expect("rejection is recorded at boundary");
    let records = pivot_records(boundary);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["status"], json!("rejected"));
    assert_eq!(records[0]["target"], json!("pending_turn"));
    assert!(
        records[0]["error"]
            .as_str()
            .expect("error text")
            .contains("message id")
    );

    loop_impl
        .inspect_state(|state| {
            assert!(state.queued_pivots().is_empty());
            let turn = &state.conversation().turns()[0];
            assert_eq!(turn.messages().len(), 4);
            assert_text(turn.messages()[3].payload(), "continued");
        })
        .expect("inspect state");
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
