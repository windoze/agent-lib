//! Offline unit tests for the [`Agent`](super::Agent) facade.
//!
//! Every test is fully offline: a scripted [`ScriptedClient`] returns a fixed
//! sequence of [`Response`]s (repeating the last one once exhausted) and a typed
//! tool records how many times it actually executed, so no network, credential,
//! or CLI is involved and each test finishes well under a second.

use std::convert::Infallible;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use super::{Agent, AgentBuilder, AgentSnapshot, CancelHandle};
use crate::agent::{
    AgentError, AgentInput, AgentMachine, ApprovalResponse, BudgetLimits, ErrorCursor,
    ErrorCursorKind, Interaction, InteractionHandler, InteractionKind, InteractionResponse,
    LoopCursorKind, ModelRef, ReconfigRequest, RequirementResult, RunContext, SkillId, StepInput,
    ToolSetId, ToolSetPatch, ToolSetRef,
};
use crate::client::{
    AuthScheme, Capability, ChatRequest, ClientError, EndpointConfig, LlmClient, Response,
};
use crate::facade::approval::{Approval, ApprovalDecision, ApprovalPolicy};
use crate::facade::collab::Collaboration;
use crate::facade::config::ProviderConfig;
use crate::facade::delegate::Delegation;
use crate::facade::error::FacadeError;
use crate::facade::run::{RunEvent, RunOutput};
use crate::facade::tool::{Tool, ToolContext};
use crate::model::content::ContentBlock;
use crate::model::extras::{ProviderExtras, ProviderId};
use crate::model::message::{Message, Role};
use crate::model::normalized::{Normalized, StopReason};
use crate::model::tool::ToolStatus;
use crate::model::usage::Usage;
use crate::stream::{BlockId, BlockKind, Delta, StreamEvent};

mod builder;
mod cancel;
mod delegates;
mod interaction;
mod lifecycle;
mod reconfig;
mod reconfig_delegation;
mod run;
mod snapshot;
mod stream;
/// A scripted client that returns responses in order, repeating the last once
/// the script is exhausted, and counts how many `chat` calls it served.
#[derive(Debug)]
struct ScriptedClient {
    responses: Vec<Response>,
    calls: Mutex<usize>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedClient {
    fn new(responses: Vec<Response>) -> Arc<Self> {
        Arc::new(Self {
            responses,
            calls: Mutex::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn call_count(&self) -> usize {
        *self.calls.lock().expect("calls mutex")
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("requests mutex").clone()
    }
}

#[async_trait]
impl LlmClient for ScriptedClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        self.requests.lock().expect("requests mutex").push(request);
        let mut calls = self.calls.lock().expect("calls mutex");
        let index = (*calls).min(self.responses.len() - 1);
        *calls += 1;
        Ok(self.responses[index].clone())
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        Err(ClientError::Other(
            "streaming not used in fixture".to_owned(),
        ))
    }
}

/// Builds an assistant response carrying only the given text.
fn text_response(text: &str) -> Response {
    text_response_with_usage(text, 11, 7)
}

fn text_response_with_usage(text: &str, input: u32, output: u32) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        },
        usage: Usage {
            input,
            output,
            ..Usage::default()
        },
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

fn provider_extras(provider: ProviderId) -> ProviderExtras {
    ProviderExtras {
        provider,
        fields: Map::from_iter([("reasoning".to_owned(), json!({ "effort": "high" }))]),
    }
}

fn provider_config(provider: ProviderId) -> ProviderConfig {
    ProviderConfig::custom(
        EndpointConfig {
            base_url: "https://example.invalid".to_owned(),
            auth: AuthScheme::None,
            query_params: Vec::new(),
            extra_headers: Vec::new(),
        },
        provider,
    )
}

/// Builds an assistant response that asks to call `tool_name`, carrying the
/// given provider-assigned call id and JSON input.
fn tool_use_response_for(tool_name: &str, id: &str, input: Value) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.to_owned(),
                name: tool_name.to_owned(),
                input,
                extra: Map::new(),
            }],
        },
        usage: Usage {
            input: 5,
            output: 3,
            ..Usage::default()
        },
        stop_reason: StopReason::normalize("tool_use"),
        extra: Map::new(),
    }
}

/// Builds an assistant response that asks to call `get_weather`, carrying the
/// given provider-assigned call id.
fn tool_use_response_with_id(id: &str) -> Response {
    tool_use_response_for("get_weather", id, json!({ "city": "Shanghai" }))
}

/// Builds an assistant response that asks to call `get_weather`.
fn tool_use_response() -> Response {
    tool_use_response_with_id("call-1")
}

/// A client that always asks to call the tool, minting a fresh provider call id
/// on every step (as a real model does), so a run can never reach a final
/// response and must stop on the loop budget.
#[derive(Debug)]
struct AlwaysToolUse {
    calls: Mutex<usize>,
}

impl AlwaysToolUse {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(0),
        })
    }
}

#[async_trait]
impl LlmClient for AlwaysToolUse {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
        let mut calls = self.calls.lock().expect("calls mutex");
        *calls += 1;
        Ok(tool_use_response_with_id(&format!("call-{calls}")))
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        Err(ClientError::Other(
            "streaming not used in fixture".to_owned(),
        ))
    }
}

/// A typed `get_weather` tool that records how many times it executed.
fn counting_weather_tool(counter: Arc<AtomicUsize>) -> Tool {
    Tool::function_with_schema(
        "get_weather",
        "Look up the current weather for a city.",
        json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
        move |_ctx: ToolContext, args: Value| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let city = args.get("city").and_then(Value::as_str).unwrap_or("?");
                Ok::<_, Infallible>(format!("{city}: sunny, 26C"))
            }
        },
    )
}

/// Builds an agent driven by the scripted client with the given tool and
/// approval tier.
fn agent_with(client: Arc<dyn LlmClient>, tool: Tool, approval: Approval) -> Agent {
    AgentBuilder::default()
        .client(client)
        .model("test-model")
        .system("You are a concise weather assistant.")
        .tool(tool)
        .approval(approval)
        .build()
        .expect("build agent")
}

fn agent_with_tools(client: Arc<dyn LlmClient>, tools: Vec<Tool>, approval: Approval) -> Agent {
    let mut builder = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .system("You are a concise weather assistant.")
        .approval(approval);
    for tool in tools {
        builder = builder.tool(tool);
    }
    builder.build().expect("build agent")
}

fn reconfig_model(name: &str) -> ModelRef {
    ModelRef::new(
        name,
        NonZeroU32::new(321).expect("non-zero max tokens"),
        Some(0.25),
        None,
    )
}

fn reconfig_tool_set_id(offset: u8) -> ToolSetId {
    let uuid = match offset {
        1 => "018f0d9c-7b6a-7c12-8f31-1234567890e1",
        2 => "018f0d9c-7b6a-7c12-8f31-1234567890e2",
        _ => "018f0d9c-7b6a-7c12-8f31-1234567890ef",
    };
    ToolSetId::parse_str(uuid).expect("tool set id")
}

fn reconfig_skill_id() -> SkillId {
    SkillId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890f5").expect("skill id")
}

fn calendar_tool_decl() -> crate::model::tool::Tool {
    counting_calendar_tool(Arc::new(AtomicUsize::new(0))).declaration()
}

fn counting_calendar_tool(counter: Arc<AtomicUsize>) -> Tool {
    Tool::function_with_schema(
        "read_calendar",
        "Read calendar availability.",
        json!({
            "type": "object",
            "properties": { "day": { "type": "string" } },
            "required": ["day"]
        }),
        move |_ctx: ToolContext, args: Value| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let day = args.get("day").and_then(Value::as_str).unwrap_or("?");
                Ok::<_, Infallible>(format!("{day}: free"))
            }
        },
    )
}

fn tool_names(tools: &[crate::model::tool::Tool]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name.as_str()).collect()
}

// -- Streaming, snapshot/restore, and escape-hatch tests (M2-4) --------------

/// A scripted client whose `chat_stream` replays a per-step normalized event
/// sequence (repeating the last once exhausted) and counts how many streams it
/// served, so a tool round trip can script one stream per LLM step offline.
#[derive(Debug)]
struct StreamingScriptedClient {
    scripts: Vec<Vec<StreamEvent>>,
    calls: Mutex<usize>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl StreamingScriptedClient {
    fn new(scripts: Vec<Vec<StreamEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts,
            calls: Mutex::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn call_count(&self) -> usize {
        *self.calls.lock().expect("calls mutex")
    }

    fn requests(&self) -> Vec<Vec<Message>> {
        self.requests
            .lock()
            .expect("requests mutex")
            .iter()
            .map(|request| request.messages.clone())
            .collect()
    }

    fn chat_requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("requests mutex").clone()
    }
}

#[async_trait]
impl LlmClient for StreamingScriptedClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
        Err(ClientError::Other(
            "chat not used in streaming fixture".to_owned(),
        ))
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        self.requests.lock().expect("requests mutex").push(request);
        let mut calls = self.calls.lock().expect("calls mutex");
        let index = (*calls).min(self.scripts.len() - 1);
        *calls += 1;
        let events = self.scripts[index].clone();
        Ok(futures::stream::iter(events.into_iter().map(Ok::<_, ClientError>)).boxed())
    }
}

/// The normalized end-turn stop shared by every text stream fixture.
fn end_turn() -> Normalized<StopReason> {
    Normalized::from_mapped(StopReason::EndTurn, "end_turn")
}

/// Builds a text response stream: message start, one text block streamed in
/// `chunks`, the given usage, and a normalized end-turn stop.
fn text_stream(chunks: &[&str], usage: Usage) -> Vec<StreamEvent> {
    let id = BlockId::new("text-1");
    let mut events = vec![
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::Text,
        },
    ];
    for chunk in chunks {
        events.push(StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Text((*chunk).to_owned()),
        });
    }
    events.push(StreamEvent::BlockStop { id: id.clone() });
    events.push(StreamEvent::Usage(usage));
    events.push(StreamEvent::MessageStop {
        stop_reason: end_turn(),
    });
    events
}

/// Builds a tool-use response stream that asks to call `get_weather`.
fn tool_stream() -> Vec<StreamEvent> {
    tool_stream_for("get_weather", "call-1", "{\"city\":\"Shanghai\"}")
}

/// Builds a tool-use response stream asking to call `get_weather` under a
/// caller-chosen provider call id (a fresh id per step mirrors a real model).
fn tool_stream_with_id(call_id: &str) -> Vec<StreamEvent> {
    tool_stream_for("get_weather", call_id, "{\"city\":\"Shanghai\"}")
}

fn tool_stream_for(tool_name: &str, call_id: &str, input: &str) -> Vec<StreamEvent> {
    let id = BlockId::new("tool-1");
    vec![
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: tool_name.to_owned(),
                tool_call_id: call_id.to_owned(),
            },
        },
        StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Json(input.to_owned()),
        },
        StreamEvent::BlockStop { id: id.clone() },
        StreamEvent::MessageStop {
            stop_reason: Normalized::from_mapped(StopReason::ToolUse, "tool_use"),
        },
    ]
}

/// The streaming counterpart of [`AlwaysToolUse`]: every `chat_stream` asks to
/// call the tool under a fresh provider call id, so a streamed run can never
/// reach a final response and must stop on the loop budget.
#[derive(Debug)]
struct AlwaysStreamingToolUse {
    calls: Mutex<usize>,
}

impl AlwaysStreamingToolUse {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(0),
        })
    }
}

#[async_trait]
impl LlmClient for AlwaysStreamingToolUse {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
        Err(ClientError::Other(
            "chat not used in streaming fixture".to_owned(),
        ))
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        let mut calls = self.calls.lock().expect("calls mutex");
        *calls += 1;
        let events = tool_stream_with_id(&format!("call-{calls}"));
        Ok(futures::stream::iter(events.into_iter().map(Ok::<_, ClientError>)).boxed())
    }
}

/// Drives an [`Agent::stream`] to exhaustion, returning every yielded event.
async fn drain_agent_stream(agent: &mut Agent, input: &str) -> Vec<RunEvent> {
    let mut stream = agent.stream(input).await.expect("open stream");
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("stream item is ok"));
    }
    events
}

/// Returns the terminal [`RunOutput`] carried by a fully drained stream.
fn terminal_output(events: &[RunEvent]) -> &RunOutput {
    let Some(RunEvent::Done(output)) = events.last() else {
        panic!("the stream ends with a terminal Done, got {events:?}");
    };
    output.as_ref()
}

/// The aggregated assistant text carried by every `TextDelta` in `events`.
fn streamed_text(events: &[RunEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            RunEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn messages_contain_tool_error(messages: &[Message], needle: &str) -> bool {
    messages.iter().any(|message| {
        message.content.iter().any(|block| match block {
            ContentBlock::ToolResult {
                content, status, ..
            } => {
                *status == ToolStatus::Error
                    && content.iter().any(|nested| match nested {
                        ContentBlock::Text { text, .. } => text.contains(needle),
                        _ => false,
                    })
            }
            _ => false,
        })
    })
}
/// Builds the `InteractionResponse` answering an approval `request` with a
/// caller-chosen `decision`, matching the shape `FacadeApproval` produces.
fn approval_response(request: &Interaction, decision: ApprovalDecision) -> RequirementResult {
    match request.kind() {
        InteractionKind::Approval { call_id, .. } => {
            RequirementResult::Interaction(InteractionResponse::Approval(ApprovalResponse::new(
                request.step_id(),
                *call_id,
                decision,
                None,
            )))
        }
        // The facade never emits these on the base path; answer trivially.
        _ => RequirementResult::Interaction(InteractionResponse::answer(String::new())),
    }
}
/// An async interaction handler that answers every approval with a fixed
/// decision without blocking.
struct FixedInteractionHandler {
    decision: ApprovalDecision,
}

#[async_trait]
impl InteractionHandler for FixedInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        approval_response(request, self.decision)
    }
}
/// Reduces a run's events to the ordered lifecycle subsequence that both the
/// streaming ([`Agent::stream`]) and non-streaming ([`Agent::run_full`]) paths
/// are contracted to agree on.
///
/// The streaming-only token [`RunEvent::TextDelta`]s, the terminal
/// [`RunEvent::Done`] envelope, and the raw escape hatches are dropped, leaving
/// only the approval / tool / delegation lifecycle events in a stable,
/// comparable canonical form. This is the "normalized event sequence" the parity
/// tests below compare across the two paths.
fn lifecycle_signature(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(canonical_lifecycle_event)
        .collect()
}

/// Renders a single lifecycle [`RunEvent`] into a canonical string, or `None`
/// for events that are not part of the shared lifecycle contract (token deltas,
/// the terminal `Done`, and the raw escape hatches).
fn canonical_lifecycle_event(event: &RunEvent) -> Option<String> {
    match event {
        RunEvent::TextDelta(_)
        | RunEvent::Done(_)
        | RunEvent::RawStream(_)
        | RunEvent::RawNotification(_) => None,
        RunEvent::ApprovalRequested(request) => Some(format!(
            "ApprovalRequested{{tool={},call_id={},reason={:?},input={:?}}}",
            request.tool_name,
            request.call_id.as_deref().unwrap_or("<none>"),
            request.reason,
            request.input
        )),
        RunEvent::ToolStarted(trace) => Some(format!(
            "ToolStarted{{name={},call_id={}}}",
            trace.name, trace.call_id
        )),
        RunEvent::ToolFinished(trace) => Some(format!(
            "ToolFinished{{name={},call_id={}}}",
            trace.name, trace.call_id
        )),
        RunEvent::DelegationStarted(trace) => Some(format!(
            "DelegationStarted{{delegate={},status={:?}}}",
            trace.delegate, trace.status
        )),
        RunEvent::DelegationFinished(trace) => Some(format!(
            "DelegationFinished{{delegate={},status={:?}}}",
            trace.delegate, trace.status
        )),
        RunEvent::DelegationFailed(trace) => Some(format!(
            "DelegationFailed{{delegate={},status={:?}}}",
            trace.delegate, trace.status
        )),
        RunEvent::DelegationProgress(progress) => Some(format!(
            "DelegationProgress{{delegate={},message={}}}",
            progress.delegate, progress.message
        )),
        RunEvent::DelegationMessage(message) => Some(format!(
            "DelegationMessage{{delegate={},message={}}}",
            message.delegate, message.message
        )),
        RunEvent::DelegationArtifact(artifact) => {
            Some(format!("DelegationArtifact{{path={}}}", artifact.path))
        }
        RunEvent::Escalated(trace) => {
            Some(format!("Escalated{{from={},to={}}}", trace.from, trace.to))
        }
    }
}

/// True when any event in the run is a streaming-only token delta.
fn has_text_delta(events: &[RunEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, RunEvent::TextDelta(_)))
}

/// The standard fixture usage for a streamed text step.
fn fixture_usage() -> Usage {
    Usage {
        input: 11,
        output: 7,
        ..Usage::default()
    }
}
