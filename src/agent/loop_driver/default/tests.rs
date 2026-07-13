use super::{DefaultAgentLoop, LlmStepMode};
use crate::{
    agent::{
        AgentEvent, AgentInput, AgentLoop, AgentSpec, BudgetLimits, LoopCursorKind, LoopPolicy,
        ModelRef, RunContext, RunId, StepId, ToolFailurePolicy, ToolSetId, ToolSetRef, TraceNodeId,
        WorktreeRef,
    },
    client::{Capability, ChatRequest, ClientError, LlmClient, Response},
    conversation::{Conversation, ConversationConfig, ConversationId, MessageId, TurnId},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        usage::Usage,
    },
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use async_trait::async_trait;
use futures::{StreamExt, stream};
use serde_json::Map;
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
        Self {
            capability: Capability::default(),
            chat_results: Mutex::new(VecDeque::from([result])),
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

fn context() -> RunContext {
    RunContext::new_root(
        run_id(),
        BudgetLimits::unbounded(),
        TraceNodeId::new("root"),
    )
}

fn state() -> crate::agent::AgentState {
    crate::agent::AgentState::new(
        spec(),
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
