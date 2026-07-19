//! Offline unit tests for the [`Agent`](super::Agent) facade.
//!
//! Every test is fully offline: a scripted [`ScriptedClient`] returns a fixed
//! sequence of [`Response`]s (repeating the last one once exhausted) and a typed
//! tool records how many times it actually executed, so no network, credential,
//! or CLI is involved and each test finishes well under a second.

use std::convert::Infallible;
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
    AgentError, ApprovalResponse, BudgetLimits, ErrorCursor, ErrorCursorKind, Interaction,
    InteractionHandler, InteractionKind, InteractionResponse, RequirementResult, RunContext,
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
use crate::model::usage::Usage;
use crate::stream::{BlockId, BlockKind, Delta, StreamEvent};

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

/// Builds an assistant response that asks to call `get_weather`, carrying the
/// given provider-assigned call id.
fn tool_use_response_with_id(id: &str) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.to_owned(),
                name: "get_weather".to_owned(),
                input: json!({ "city": "Shanghai" }),
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

#[tokio::test]
async fn run_completes_a_tool_round_trip() {
    let client = ScriptedClient::new(vec![tool_use_response(), text_response("It is sunny.")]);
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with(
        client.clone(),
        counting_weather_tool(executions.clone()),
        Approval::auto_allow(),
    );

    let reply = agent.run("What is the weather in Shanghai?").await.unwrap();

    assert_eq!(reply.text(), "It is sunny.");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "tool ran exactly once"
    );
    assert_eq!(
        client.call_count(),
        2,
        "one tool-use step plus one final step"
    );
}

#[tokio::test]
async fn builder_provider_extras_reach_supervisor_request() {
    let client = ScriptedClient::new(vec![text_response("done")]);
    let extras = provider_extras(ProviderId::Anthropic);
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("claude-test")
        .provider_extras(extras.clone())
        .build()
        .expect("build agent");

    agent.run("hello").await.expect("run succeeds");

    assert_eq!(client.requests()[0].provider_extras, Some(extras));
}

#[tokio::test]
async fn builder_budget_limits_supervisor_run_and_leaves_agent_usable() {
    let client = ScriptedClient::new(vec![
        text_response_with_usage("too expensive", 11, 7),
        text_response_with_usage("recovered", 0, 0),
    ]);
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .budget(BudgetLimits::new(None, Some(10), None, None))
        .build()
        .expect("build agent");

    let error = agent.run("exceed the token budget").await.unwrap_err();
    assert!(
        matches!(error, FacadeError::BudgetExhausted),
        "token overrun maps to a structured facade budget error, got {error:?}"
    );
    agent
        .snapshot()
        .expect("budget failure leaves state snapshot-able");

    let reply = agent
        .run("second run gets a fresh budget ledger")
        .await
        .expect("subsequent low-usage run succeeds");
    assert_eq!(reply.text(), "recovered");
}

#[test]
fn builder_rejects_provider_extras_for_different_provider() {
    let error = AgentBuilder::default()
        .provider(provider_config(ProviderId::OpenAiResp))
        .model("gpt-test")
        .provider_extras(provider_extras(ProviderId::Anthropic))
        .build()
        .expect_err("provider mismatch is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("provider_extras"));
}

#[test]
fn builder_rejects_blank_model() {
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("done")]))
        .model("\t  ")
        .build()
        .expect_err("blank model is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("model"));
}

#[test]
fn builder_rejects_non_finite_temperature() {
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("done")]))
        .model("test-model")
        .temperature(f32::INFINITY)
        .build()
        .expect_err("non-finite temperature is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("temperature"));
}

#[test]
fn builder_rejects_blank_delegation_tool_name() {
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("done")]))
        .model("test-model")
        .delegation(Delegation::single_tool(" "))
        .build()
        .expect_err("blank delegation tool name is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("tool name"));
}

#[test]
fn builder_rejects_empty_rules_delegation() {
    // A rules-routed delegation with no rules can never route and exposes no
    // delegate tools, so registered subagents would be silently unreachable.
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("done")]))
        .model("test-model")
        .delegation(Delegation::rules())
        .build()
        .expect_err("rules delegation with no rules is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("at least one rule"), "{message}");
}

#[test]
fn builder_rejects_invalid_rules_routing_entries() {
    for (delegation, expected) in [
        (
            Delegation::rules().when_task_contains(Vec::<String>::new(), "coder"),
            "keywords",
        ),
        (
            Delegation::rules().when_task_contains(["fix", "  "], "coder"),
            "keyword",
        ),
        (
            Delegation::rules().when_task_contains(["fix"], " "),
            "delegate",
        ),
    ] {
        let error = AgentBuilder::default()
            .client(ScriptedClient::new(vec![text_response("done")]))
            .model("test-model")
            .delegation(delegation)
            .build()
            .expect_err("invalid rules entry is rejected");

        let FacadeError::Config(message) = error else {
            panic!("expected config error")
        };
        assert!(message.contains(expected), "{message}");
    }
}

#[tokio::test]
async fn run_full_records_tool_calls_and_events() {
    let client = ScriptedClient::new(vec![tool_use_response(), text_response("It is sunny.")]);
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with(
        client,
        counting_weather_tool(executions),
        Approval::auto_allow(),
    );

    let output = agent.run_full("weather?").await.unwrap();

    assert_eq!(output.reply.text(), "It is sunny.");
    assert!(
        output.response.is_none(),
        "the drive folds responses, none handed back"
    );
    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].name, "get_weather");

    // The aggregate usage sums both the tool-use step and the final step.
    assert_eq!(output.usage.supervisor.input, 16);
    assert_eq!(output.usage.supervisor.output, 10);

    let started = output
        .events
        .iter()
        .filter(|event| matches!(event, RunEvent::ToolStarted(_)))
        .count();
    let finished = output
        .events
        .iter()
        .filter(|event| matches!(event, RunEvent::ToolFinished(_)))
        .count();
    assert_eq!(started, 1, "one tool-started event");
    assert_eq!(finished, 1, "one tool-finished event");

    if let Some(RunEvent::ToolFinished(trace)) = output
        .events
        .iter()
        .find(|event| matches!(event, RunEvent::ToolFinished(_)))
    {
        assert_eq!(
            trace.name, "get_weather",
            "finished trace recovers the name"
        );
    } else {
        panic!("expected a ToolFinished event");
    }
}

#[tokio::test]
async fn auto_deny_skips_tool_execution() {
    let client = ScriptedClient::new(vec![
        tool_use_response(),
        text_response("I could not run that tool."),
    ]);
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with(
        client,
        counting_weather_tool(executions.clone()),
        Approval::auto_deny(),
    );

    let reply = agent.run("weather?").await.unwrap();

    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "a denied tool never executes"
    );
    assert_eq!(reply.text(), "I could not run that tool.");
}

/// The non-streaming `run_full` records `ApprovalRequested` for an `ask`-tier
/// tool answered through the shared `FacadeApproval` fallback, and the event
/// precedes the tool lifecycle it gated with the same enriched fields the
/// streaming path emits (M2-1).
#[tokio::test]
async fn run_full_records_ask_approval_then_tool_lifecycle() {
    let client = ScriptedClient::new(vec![tool_use_response(), text_response("It is sunny.")]);
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with(
        client,
        counting_weather_tool(executions.clone()),
        Approval::ask(|_request| ApprovalDecision::Approve),
    );

    let output = agent.run_full("weather?").await.unwrap();

    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "an approved tool runs exactly once"
    );

    let approval_pos = output.events.iter().position(|event| {
        matches!(event, RunEvent::ApprovalRequested(request) if request.tool_name == "get_weather")
    });
    let started_pos = output.events.iter().position(
        |event| matches!(event, RunEvent::ToolStarted(trace) if trace.name == "get_weather"),
    );
    let finished_pos = output.events.iter().position(
        |event| matches!(event, RunEvent::ToolFinished(trace) if trace.name == "get_weather"),
    );
    let (Some(approval_pos), Some(started_pos), Some(finished_pos)) =
        (approval_pos, started_pos, finished_pos)
    else {
        panic!(
            "expected approval + tool lifecycle events, got {:?}",
            output.events
        );
    };
    assert!(
        approval_pos < started_pos && started_pos < finished_pos,
        "ApprovalRequested precedes ToolStarted precedes ToolFinished, got {:?}",
        output.events
    );

    let RunEvent::ApprovalRequested(request) = &output.events[approval_pos] else {
        unreachable!("indexed an ApprovalRequested position");
    };
    assert!(
        request.call_id.is_some(),
        "the approval carries the pending call id, got {request:?}"
    );
    assert_eq!(
        request.reason.as_deref(),
        Some("approve execution of tool `get_weather`"),
        "the approval carries the requirement reason, got {request:?}"
    );
    assert_eq!(
        request.input.as_deref(),
        Some("{\"city\":\"Shanghai\"}"),
        "the approval carries a redacted input summary, got {request:?}"
    );

    let RunEvent::ToolStarted(started) = &output.events[started_pos] else {
        unreachable!("indexed a ToolStarted position");
    };
    assert_eq!(
        Some(started.call_id.as_str()),
        request.call_id.as_deref(),
        "the approval gates the same call that started"
    );
}

/// A caller-injected handler that denies still leaves the paused approval in
/// `RunOutput.events`, and the denied tool emits no lifecycle events (M2-1).
#[tokio::test]
async fn run_full_records_approval_when_injected_handler_denies() {
    let client = ScriptedClient::new(vec![
        tool_use_response(),
        text_response("I could not run that tool."),
    ]);
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(FixedInteractionHandler {
        decision: ApprovalDecision::Deny,
    });
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .tool(counting_weather_tool(executions.clone()))
        .approval(Approval::auto_deny())
        .interaction_handler(handler)
        .build()
        .expect("build agent");

    let output = agent.run_full("weather?").await.unwrap();

    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "a denied tool never executes"
    );
    assert_eq!(output.reply.text(), "I could not run that tool.");

    let approval = output.events.iter().find_map(|event| match event {
        RunEvent::ApprovalRequested(request) => Some(request.clone()),
        _ => None,
    });
    let Some(approval) = approval else {
        panic!(
            "a denied run still records ApprovalRequested, got {:?}",
            output.events
        );
    };
    assert_eq!(
        approval.tool_name, "get_weather",
        "the approval names the denied tool"
    );
    assert!(
        approval.call_id.is_some(),
        "the approval carries the pending call id, got {approval:?}"
    );
    // A denied tool never starts, so it emits no `ToolStarted`; and since it
    // never ran, it emits no `ToolFinished` either — the non-streaming tool
    // lifecycle is now identical to the streaming path for a denied call. Only
    // the paused `ApprovalRequested` remains observable.
    assert!(
        !output
            .events
            .iter()
            .any(|event| matches!(event, RunEvent::ToolStarted(_))),
        "a denied tool never starts, got {:?}",
        output.events
    );
    assert!(
        !output
            .events
            .iter()
            .any(|event| matches!(event, RunEvent::ToolFinished(_))),
        "a denied tool never finishes, got {:?}",
        output.events
    );
}

/// A headless `ask` tier (no injected handler, no `ask` closure) is denied by
/// `FacadeApproval` without blocking, yet the paused approval is still recorded
/// into `RunOutput.events` (M2-1).
#[tokio::test]
async fn run_full_records_approval_for_headless_ask_without_handler() {
    let client = ScriptedClient::new(vec![
        tool_use_response(),
        text_response("I could not run that tool."),
    ]);
    let executions = Arc::new(AtomicUsize::new(0));
    let policy = ApprovalPolicy::new(Approval::auto_allow()).ask_tool("get_weather");
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .tool(counting_weather_tool(executions.clone()))
        .approval(policy)
        .build()
        .expect("build agent");

    let output = agent.run_full("weather?").await.unwrap();

    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "a headless-denied tool never executes"
    );

    let approval = output.events.iter().find_map(|event| match event {
        RunEvent::ApprovalRequested(request) => Some(request.clone()),
        _ => None,
    });
    let Some(approval) = approval else {
        panic!(
            "a headless ask still records ApprovalRequested, got {:?}",
            output.events
        );
    };
    assert_eq!(
        approval.tool_name, "get_weather",
        "the approval names the pending tool"
    );
    assert!(
        approval.call_id.is_some(),
        "the approval carries the pending call id, got {approval:?}"
    );
}

#[tokio::test]
async fn exceeding_the_tool_round_budget_fails() {
    // The client always asks to call the tool (with a fresh id each round), so no
    // final response is ever reached and the loop budget must stop the run.
    let client = AlwaysToolUse::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .tool(counting_weather_tool(executions))
        .approval(Approval::auto_allow())
        .max_tool_rounds(1)
        .build()
        .expect("build agent");

    let error = agent.run("loop forever").await.unwrap_err();

    assert!(
        matches!(error, FacadeError::LoopLimitExceeded),
        "an exhausted loop budget maps to LoopLimitExceeded, got {error:?}"
    );
}

#[tokio::test]
async fn exceeding_the_tool_round_budget_fails_the_stream() {
    // Same budget stop as the non-streaming path: the streamed run surfaces the
    // structured step-limit terminal as LoopLimitExceeded (M4-4 parity). The
    // client asks for the tool under a fresh call id on every step, so no final
    // response is ever reached.
    let client = AlwaysStreamingToolUse::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .tool(counting_weather_tool(executions))
        .approval(Approval::auto_allow())
        .max_tool_rounds(1)
        .build()
        .expect("build agent");

    let mut stream = agent.stream("loop forever").await.expect("open stream");
    let mut terminal = None;
    while let Some(item) = stream.next().await {
        if let Err(error) = item {
            terminal = Some(error);
            break;
        }
    }

    assert!(
        matches!(terminal, Some(FacadeError::LoopLimitExceeded)),
        "an exhausted loop budget maps to LoopLimitExceeded on the stream path, got {terminal:?}"
    );
}

#[test]
fn error_cursor_classification_uses_kind_not_message_text() {
    let limit = ErrorCursor::with_kind(
        "the human-facing wording can change",
        ErrorCursorKind::LoopLimitExceeded,
    )
    .expect("typed limit error cursor");
    assert!(matches!(
        super::classify_error(&limit),
        FacadeError::LoopLimitExceeded
    ));

    let ordinary = ErrorCursor::new("legacy loop step limit words in an unrelated error")
        .expect("ordinary error cursor");
    match super::classify_error(&ordinary) {
        FacadeError::Agent(AgentError::Other(message)) => {
            assert_eq!(message, ordinary.message());
        }
        other => panic!("ordinary error must not be classified by message text: {other:?}"),
    }
}

#[tokio::test]
async fn multiple_runs_accumulate_history() {
    let client = ScriptedClient::new(vec![text_response("First."), text_response("Second.")]);
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with(
        client,
        counting_weather_tool(executions),
        Approval::auto_allow(),
    );

    let first = agent.run("one").await.unwrap();
    assert_eq!(first.text(), "First.");
    let second = agent.run("two").await.unwrap();
    assert_eq!(second.text(), "Second.");

    // Two committed user+assistant turns remain in the shared conversation.
    assert_eq!(agent.conversation().turns().len(), 2);
}

#[test]
fn build_rejects_missing_model() {
    let executions = Arc::new(AtomicUsize::new(0));
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .tool(counting_weather_tool(executions))
        .build()
        .unwrap_err();
    assert!(matches!(error, FacadeError::Config(_)));
}

#[test]
fn build_rejects_duplicate_tool_names() {
    let a = counting_weather_tool(Arc::new(AtomicUsize::new(0)));
    let b = counting_weather_tool(Arc::new(AtomicUsize::new(0)));
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .tool(a)
        .tool(b)
        .build()
        .unwrap_err();
    assert!(
        matches!(error, FacadeError::DuplicateTool { name } if name == "get_weather"),
        "duplicate tool names are rejected at build",
    );
}

// -- Streaming, snapshot/restore, and escape-hatch tests (M2-4) --------------

/// A scripted client whose `chat_stream` replays a per-step normalized event
/// sequence (repeating the last once exhausted) and counts how many streams it
/// served, so a tool round trip can script one stream per LLM step offline.
#[derive(Debug)]
struct StreamingScriptedClient {
    scripts: Vec<Vec<StreamEvent>>,
    calls: Mutex<usize>,
    requests: Mutex<Vec<Vec<Message>>>,
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
        self.requests
            .lock()
            .expect("requests mutex")
            .push(request.messages.clone());
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
    tool_stream_with_id("call-1")
}

/// Builds a tool-use response stream asking to call `get_weather` under a
/// caller-chosen provider call id (a fresh id per step mirrors a real model).
fn tool_stream_with_id(call_id: &str) -> Vec<StreamEvent> {
    let id = BlockId::new("tool-1");
    vec![
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: call_id.to_owned(),
            },
        },
        StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Json("{\"city\":\"Shanghai\"}".to_owned()),
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

#[tokio::test]
async fn stream_text_matches_run_full() {
    let usage = Usage {
        input: 11,
        output: 7,
        ..Usage::default()
    };

    // Non-streaming reference over an equivalent response.
    let reference_client = ScriptedClient::new(vec![text_response("It is sunny.")]);
    let mut reference = agent_with(
        reference_client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let expected = reference.run_full("weather?").await.unwrap();

    // Streaming the same generation in three chunks.
    let stream_client =
        StreamingScriptedClient::new(vec![text_stream(&["It ", "is ", "sunny."], usage)]);
    let mut streamed = agent_with(
        stream_client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let events = drain_agent_stream(&mut streamed, "weather?").await;

    assert_eq!(
        streamed_text(&events),
        "It is sunny.",
        "text deltas reassemble the full assistant text"
    );
    let Some(RunEvent::Done(output)) = events.last() else {
        panic!("the stream ends with a terminal Done, got {events:?}");
    };
    assert_eq!(
        **output, expected,
        "the streamed terminal output matches run_full"
    );
}

#[tokio::test]
async fn stream_tool_round_trip_emits_tool_events() {
    let usage = Usage {
        input: 11,
        output: 7,
        ..Usage::default()
    };
    let client =
        StreamingScriptedClient::new(vec![tool_stream(), text_stream(&["It is sunny."], usage)]);
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with(
        client.clone(),
        counting_weather_tool(executions.clone()),
        Approval::auto_allow(),
    );

    let events = drain_agent_stream(&mut agent, "weather?").await;

    let started = events.iter().position(
        |event| matches!(event, RunEvent::ToolStarted(trace) if trace.name == "get_weather"),
    );
    let finished = events.iter().position(
        |event| matches!(event, RunEvent::ToolFinished(trace) if trace.name == "get_weather"),
    );
    assert!(
        matches!((started, finished), (Some(s), Some(f)) if s < f),
        "a live ToolStarted precedes the matching ToolFinished, got {events:?}"
    );

    assert_eq!(
        streamed_text(&events),
        "It is sunny.",
        "final text streams after the tool round"
    );

    let Some(RunEvent::Done(output)) = events.last() else {
        panic!("the stream ends with a terminal Done, got {events:?}");
    };
    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].name, "get_weather");
    assert_eq!(executions.load(Ordering::SeqCst), 1, "the tool ran once");
    assert_eq!(
        client.call_count(),
        2,
        "one tool-use step plus one final step"
    );
}

#[tokio::test]
async fn stream_interject_injects_a_pivot_at_the_next_step_boundary() {
    let usage = Usage {
        input: 11,
        output: 7,
        ..Usage::default()
    };
    let client = StreamingScriptedClient::new(vec![
        tool_stream(),
        text_stream(&["Pivot acknowledged."], usage),
    ]);
    let mut agent = agent_with(
        client.clone(),
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );

    let mut stream = agent.stream("weather?").await.expect("open stream");
    let mut events = Vec::new();
    loop {
        let event = stream
            .next()
            .await
            .expect("stream reaches the post-tool boundary")
            .expect("event ok");
        let finished_tool =
            matches!(&event, RunEvent::ToolFinished(trace) if trace.name == "get_weather");
        events.push(event);
        if finished_tool {
            break;
        }
    }

    stream
        .interject("Please answer with the pivot in mind.")
        .expect("tool-step boundary accepts a stream pivot");

    while let Some(item) = stream.next().await {
        events.push(item.expect("stream item is ok"));
    }

    assert_eq!(streamed_text(&events), "Pivot acknowledged.");
    let requests = client.requests();
    assert_eq!(requests.len(), 2, "tool step plus pivoted final step");
    let second_request = &requests[1];
    assert!(
        second_request.iter().any(|message| {
            message.role == Role::User
                && message_text(message).contains("Please answer with the pivot in mind.")
        }),
        "the re-rendered LLM request should include the injected pivot user message, got {second_request:?}"
    );
}

#[tokio::test]
async fn stream_reports_approval_request() {
    let usage = Usage {
        input: 11,
        output: 7,
        ..Usage::default()
    };
    let client =
        StreamingScriptedClient::new(vec![tool_stream(), text_stream(&["Denied."], usage)]);
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with(
        client,
        counting_weather_tool(executions.clone()),
        Approval::auto_deny(),
    );

    let events = drain_agent_stream(&mut agent, "weather?").await;

    let approval = events.iter().find_map(|event| match event {
        RunEvent::ApprovalRequested(request) => Some(request.clone()),
        _ => None,
    });
    let Some(approval) = approval else {
        panic!("an ApprovalRequested event is emitted, got {events:?}");
    };
    assert_eq!(
        approval.tool_name, "get_weather",
        "the ApprovalRequested event names the pending tool, got {events:?}"
    );
    assert!(
        approval.call_id.is_some(),
        "the ApprovalRequested event carries the pending call id, got {approval:?}"
    );
    assert_eq!(
        approval.reason.as_deref(),
        Some("approve execution of tool `get_weather`"),
        "the ApprovalRequested event carries the requirement reason, got {approval:?}"
    );
    assert_eq!(
        approval.input.as_deref(),
        Some("{\"city\":\"Shanghai\"}"),
        "the ApprovalRequested event carries a redacted input summary, got {approval:?}"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "a denied tool never executes"
    );
}

#[tokio::test]
async fn snapshot_then_restore_continues_history() {
    let client = ScriptedClient::new(vec![text_response("First."), text_response("Second.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );

    let first = agent.run("one").await.unwrap();
    assert_eq!(first.text(), "First.");
    assert_eq!(agent.conversation().turns().len(), 1);

    let snapshot = agent.snapshot().expect("snapshot at a committed point");

    // Restore against a fresh client and re-injected tool.
    let restore_client = ScriptedClient::new(vec![text_response("Second.")]);
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restore_client)
        .tool(counting_weather_tool(Arc::new(AtomicUsize::new(0))))
        .approval(Approval::auto_allow())
        .build()
        .expect("restore agent");

    assert_eq!(
        restored.conversation().turns().len(),
        1,
        "restore preserves the first committed turn"
    );

    let second = restored.run("two").await.unwrap();
    assert_eq!(second.text(), "Second.");
    assert_eq!(
        restored.conversation().turns().len(),
        2,
        "a run after restore appends to the restored history"
    );
}

#[tokio::test]
async fn restore_builder_provider_extras_reach_restored_request() {
    let base_client = ScriptedClient::new(vec![text_response("First.")]);
    let mut agent = AgentBuilder::default()
        .client(base_client)
        .model("claude-test")
        .build()
        .expect("build agent");
    agent.run("one").await.expect("first run");
    let snapshot = agent.snapshot().expect("snapshot at a committed point");

    let restore_client = ScriptedClient::new(vec![text_response("Second.")]);
    let extras = provider_extras(ProviderId::Anthropic);
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restore_client.clone())
        .provider_extras(extras.clone())
        .build()
        .expect("restore agent");

    restored.run("two").await.expect("restored run");

    assert_eq!(restore_client.requests()[0].provider_extras, Some(extras));
}

#[test]
fn restore_builder_rejects_provider_extras_for_different_provider() {
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("x")]))
        .model("gpt-test")
        .build()
        .expect("build agent");
    let snapshot = agent.snapshot().expect("snapshot");

    let error = Agent::restore()
        .snapshot(snapshot)
        .provider(provider_config(ProviderId::OpenAiResp))
        .provider_extras(provider_extras(ProviderId::Anthropic))
        .build()
        .expect_err("provider mismatch is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("provider_extras"));
}

#[tokio::test]
async fn snapshot_round_trips_through_json() {
    let client = ScriptedClient::new(vec![text_response("Hello.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    agent.run("hi").await.unwrap();

    let snapshot = agent.snapshot().expect("snapshot");
    let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
    let restored: AgentSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");

    assert_eq!(restored, snapshot, "snapshot survives a JSON round trip");
    assert!(
        snapshot.delegates.is_empty()
            && snapshot.pending_delegations.is_empty()
            && snapshot.artifacts.is_empty(),
        "reserved slices are empty on the base agent path"
    );
    assert!(
        snapshot.mailbox.is_none() && snapshot.blackboard.is_none() && snapshot.plan.is_none(),
        "reserved options are absent on the base agent path"
    );
}

#[tokio::test]
async fn into_parts_exposes_usable_state() {
    let client = ScriptedClient::new(vec![text_response("Hello.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    agent.run("hi").await.unwrap();

    let parts = agent.into_parts();
    assert_eq!(
        parts.state.conversation().turns().len(),
        1,
        "the handed-out state owns the committed history"
    );
    assert_eq!(parts.tools.len(), 1);
    assert_eq!(parts.tools[0].name(), "get_weather");
}

#[test]
fn restore_requires_a_snapshot() {
    let error = Agent::restore()
        .client(ScriptedClient::new(vec![text_response("x")]))
        .build()
        .unwrap_err();
    assert!(
        matches!(error, FacadeError::Config(_)),
        "restore without a snapshot is a config error, got {error:?}"
    );
}

#[tokio::test]
async fn restore_requires_a_client_or_provider() {
    let client = ScriptedClient::new(vec![text_response("Hi.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    agent.run("hi").await.unwrap();
    let snapshot = agent.snapshot().expect("snapshot");

    let error = Agent::restore().snapshot(snapshot).build().unwrap_err();
    assert!(
        matches!(error, FacadeError::Config(_)),
        "restore without a client or provider is a config error, got {error:?}"
    );
}

#[test]
fn registered_subagents_appear_in_the_delegate_table() {
    let reviewer = Agent::worker()
        .model("reviewer-model")
        .system("You review code.")
        .build()
        .expect("worker builds");
    let researcher = Agent::worker()
        .system("You research.")
        .build()
        .expect("worker builds");

    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .build()
        .expect("build agent");

    let names: Vec<&str> = agent.subagents().iter().map(|s| s.name()).collect();
    assert_eq!(names, ["reviewer", "researcher"]);
    // The explicit-model worker keeps its model; the default worker inherits.
    assert!(!agent.subagents()[0].inherits_model());
    assert_eq!(
        agent.subagents()[0].spec().model().model(),
        "reviewer-model"
    );
    assert!(agent.subagents()[1].inherits_model());
}

#[tokio::test]
async fn rules_routed_delegate_output_has_no_supervisor_reply_usage() {
    let client = ScriptedClient::new(vec![text_response_with_usage("delegated", 3, 4)]);
    let reviewer = Agent::worker().system("review").build().expect("worker");
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .subagent("reviewer", reviewer)
        .delegation(Delegation::rules().when_task_contains(["route"], "reviewer"))
        .build()
        .expect("build agent");

    let output = agent.run_full("please route this").await.expect("run");

    assert_eq!(output.reply.text(), "delegated");
    assert_eq!(output.reply.usage(), None);
    assert_eq!(output.usage.supervisor, Usage::default());
    assert_eq!(output.usage.subagents.input, 3);
    assert_eq!(output.usage.subagents.output, 4);
}

#[test]
fn into_parts_carries_registered_delegates() {
    let reviewer = Agent::worker()
        .system("You review code.")
        .build()
        .expect("worker builds");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("reviewer", reviewer)
        .build()
        .expect("build agent");

    let parts = agent.into_parts();
    assert_eq!(parts.delegates.len(), 1);
    assert_eq!(parts.delegates[0].name(), "reviewer");
}

#[test]
fn into_parts_carries_the_injected_interaction_handler() {
    // An injected async interaction handler is a live runtime handle distinct
    // from the approval bridge; `into_parts` must hand it back rather than drop
    // it (§19).
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .interaction_handler(Arc::new(FixedInteractionHandler {
            decision: ApprovalDecision::Approve,
        }))
        .build()
        .expect("build agent");

    let parts = agent.into_parts();
    assert!(
        parts.interaction_handler.is_some(),
        "the injected interaction handler survives into_parts"
    );
    // A base agent with no delegation drives no external runtime, so its
    // retained external session facts are empty.
    assert!(parts.retained_external_sessions.is_empty());
}

#[test]
fn into_parts_without_a_handler_leaves_the_slot_empty() {
    // Without an injected handler the agent falls back to the approval bridge,
    // so the dedicated interaction-handler slot is `None` (the fallback is still
    // reachable through `parts.approval`).
    let agent = agent_with(
        ScriptedClient::new(vec![text_response("hi")]),
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );

    let parts = agent.into_parts();
    assert!(
        parts.interaction_handler.is_none(),
        "no injected handler means the slot is empty"
    );
}

#[test]
fn into_parts_carries_live_collaboration_state() {
    // §14: a dispatcher / verifier loop provisions plan + blackboard + mailbox.
    // `into_parts` must surface both the resolved config and the live, shared
    // primitives so a caller can keep messaging through them.
    let cheap = Agent::worker().system("cheap").build().expect("worker");
    let checker = Agent::worker().system("checker").build().expect("worker");
    let strong = Agent::worker().system("strong").build().expect("worker");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("cheap", cheap)
        .subagent("checker", checker)
        .subagent("strong", strong)
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .verify_with("checker")
                .escalate_to("strong"),
        )
        .build()
        .expect("build agent");

    let parts = agent.into_parts();
    assert!(
        parts.collaboration.plan_enabled()
            && parts.collaboration.blackboard_enabled()
            && parts.collaboration.mailbox_enabled(),
        "the resolved collaboration config is handed out verbatim"
    );

    let mailbox = parts.mailbox.expect("mailbox handed out");
    mailbox.send("cheap", "checker", "verify claim 3");
    let inbox = mailbox.inbox("checker");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].text, "verify claim 3");

    assert!(
        parts.blackboard.is_some() && parts.plan.is_some(),
        "the live blackboard and plan handles are handed out too"
    );
}

#[test]
fn into_parts_carries_registered_external_delegates() {
    // A managed external delegate is registered as a data-first recipe; §14 also
    // enables the artifact store for it. `into_parts` must keep both the
    // delegate and its resolved collaboration flags.
    let coder = crate::facade::external::ManagedExternalAgent::claude_code()
        .build()
        .expect("external agent builds");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .external_agent("coder", coder)
        .build()
        .expect("build agent");

    let parts = agent.into_parts();
    let names: Vec<&str> = parts.external_agents.iter().map(|d| d.name()).collect();
    assert_eq!(names, ["coder"], "the external delegate is not dropped");
    assert!(
        parts.collaboration.artifacts_enabled(),
        "a managed external delegate enables the artifact store (§14)"
    );
    // No delegation has driven the runtime yet, so no session facts are retained.
    assert!(parts.retained_external_sessions.is_empty());
}

#[test]
fn base_agent_enables_no_collaboration() {
    // No delegate → §14 provisions no collaboration substrate.
    let agent = agent_with(
        ScriptedClient::new(vec![text_response("hi")]),
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    assert!(!agent.collaboration().any());
    assert!(agent.mailbox().is_none());
    assert!(agent.blackboard().is_none());
    assert!(agent.plan().is_none());
}

#[test]
fn two_subagents_auto_enable_a_shared_mailbox() {
    // §14: multiple delegates auto-enable a mailbox (only) — the shared inbox two
    // delegates can message through.
    let reviewer = Agent::worker()
        .system("You review code.")
        .build()
        .expect("worker builds");
    let researcher = Agent::worker()
        .system("You research topics.")
        .build()
        .expect("worker builds");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .build()
        .expect("build agent");

    assert!(agent.collaboration().mailbox_enabled());
    assert!(!agent.collaboration().plan_enabled());
    assert!(!agent.collaboration().blackboard_enabled());

    let mailbox = agent.mailbox().expect("mailbox provisioned");
    mailbox.send("reviewer", "researcher", "need sources for claim 3");
    let inbox = mailbox.inbox("researcher");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "reviewer");
    assert_eq!(inbox[0].text, "need sources for claim 3");
    assert!(agent.plan().is_none() && agent.blackboard().is_none());
}

#[test]
fn dispatcher_topology_enables_plan_blackboard_and_mailbox() {
    // §14: a dispatcher / verifier loop enables plan + blackboard + mailbox.
    let cheap = Agent::worker().system("cheap").build().expect("worker");
    let checker = Agent::worker().system("checker").build().expect("worker");
    let strong = Agent::worker().system("strong").build().expect("worker");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("cheap", cheap)
        .subagent("checker", checker)
        .subagent("strong", strong)
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .verify_with("checker")
                .escalate_to("strong"),
        )
        .build()
        .expect("build agent");

    let collab = agent.collaboration();
    assert!(collab.plan_enabled() && collab.blackboard_enabled() && collab.mailbox_enabled());
    assert!(agent.plan().is_some());
    assert!(agent.blackboard().is_some());
    assert!(agent.mailbox().is_some());
}

#[test]
fn explicit_collaboration_overrides_topology() {
    // An explicit `Collaboration` replaces the derived default in full: a
    // multi-delegate topology would derive a mailbox, but the explicit plan-only
    // config suppresses it.
    let reviewer = Agent::worker().system("r").build().expect("worker");
    let researcher = Agent::worker().system("s").build().expect("worker");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .collaboration(Collaboration::new().plan())
        .build()
        .expect("build agent");

    assert!(agent.collaboration().plan_enabled());
    assert!(!agent.collaboration().mailbox_enabled());
    assert!(agent.plan().is_some());
    assert!(agent.mailbox().is_none());
}

// --- Milestone 7-1: injected async interaction handler ---------------------

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

/// An async interaction handler that models a true cross-process pause: it
/// signals when `fulfill` is entered, then `await`s a test-driven channel before
/// answering with the decision the channel delivers.
struct GatedInteractionHandler {
    reached: Mutex<Option<oneshot::Sender<()>>>,
    gate: Mutex<Option<oneshot::Receiver<ApprovalDecision>>>,
}

impl GatedInteractionHandler {
    fn new() -> (
        Arc<Self>,
        oneshot::Receiver<()>,
        oneshot::Sender<ApprovalDecision>,
    ) {
        let (reached_tx, reached_rx) = oneshot::channel();
        let (gate_tx, gate_rx) = oneshot::channel();
        let handler = Arc::new(Self {
            reached: Mutex::new(Some(reached_tx)),
            gate: Mutex::new(Some(gate_rx)),
        });
        (handler, reached_rx, gate_tx)
    }
}

#[async_trait]
impl InteractionHandler for GatedInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        if let Some(reached) = self.reached.lock().expect("reached mutex").take() {
            let _ = reached.send(());
        }
        // Take the receiver out before awaiting so no lock guard is held across
        // the suspension point.
        let gate = self.gate.lock().expect("gate mutex").take();
        let decision = gate
            .expect("gate receiver is available once")
            .await
            .expect("the test delivers a decision");
        approval_response(request, decision)
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

/// The injected handler pauses the whole run until the host resolves it, and its
/// `approve` lets the gated tool execute even though the policy default denies.
#[tokio::test]
async fn injected_interaction_handler_pauses_until_approved() {
    let client = ScriptedClient::new(vec![tool_use_response(), text_response("It is sunny.")]);
    let executions = Arc::new(AtomicUsize::new(0));
    let (handler, mut reached_rx, gate_tx) = GatedInteractionHandler::new();
    // `auto_deny` makes the machine gate pause every tool call; the injected
    // handler then overrides that default and decides for itself.
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .tool(counting_weather_tool(executions.clone()))
        .approval(Approval::auto_deny())
        .interaction_handler(handler)
        .build()
        .expect("build agent");

    let mut run = Box::pin(agent.run("weather?"));

    // Drive the run until the handler is entered, asserting it never completes
    // before the interaction is resolved.
    let mut reached = false;
    for _ in 0..1000 {
        if futures::poll!(run.as_mut()).is_ready() {
            panic!("the run completed before the interaction was resolved");
        }
        if reached_rx.try_recv().is_ok() {
            reached = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(reached, "the injected interaction handler was reached");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "the gated tool has not run while the interaction is unresolved"
    );
    // Still pending: nothing has resolved the gate yet.
    assert!(
        matches!(futures::poll!(run.as_mut()), Poll::Pending),
        "the run stays paused until the host resolves the interaction"
    );

    // The host approves; only now can the run finish and the tool execute.
    gate_tx
        .send(ApprovalDecision::Approve)
        .expect("send the decision");
    let reply = run
        .await
        .expect("run completes after the interaction resolves");

    assert_eq!(reply.text(), "It is sunny.");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "an approved gated tool runs exactly once"
    );
}

/// The same injected handler denying leaves the gated tool unexecuted, matching
/// the conservative deny path but driven by the host's async decision.
#[tokio::test]
async fn injected_interaction_handler_pauses_until_denied() {
    let client = ScriptedClient::new(vec![
        tool_use_response(),
        text_response("I could not run that tool."),
    ]);
    let executions = Arc::new(AtomicUsize::new(0));
    let (handler, mut reached_rx, gate_tx) = GatedInteractionHandler::new();
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .tool(counting_weather_tool(executions.clone()))
        .approval(Approval::auto_deny())
        .interaction_handler(handler)
        .build()
        .expect("build agent");

    let mut run = Box::pin(agent.run("weather?"));

    let mut reached = false;
    for _ in 0..1000 {
        if futures::poll!(run.as_mut()).is_ready() {
            panic!("the run completed before the interaction was resolved");
        }
        if reached_rx.try_recv().is_ok() {
            reached = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(reached, "the injected interaction handler was reached");

    gate_tx
        .send(ApprovalDecision::Deny)
        .expect("send the decision");
    let reply = run
        .await
        .expect("run completes after the interaction resolves");

    assert_eq!(reply.text(), "I could not run that tool.");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "a denied gated tool never executes"
    );
}

/// The streaming path still emits `ApprovalRequested` (labelled with the pending
/// tool) and routes the decision through the injected handler, whose approve
/// overrides the policy's deny so the tool runs.
#[tokio::test]
async fn stream_routes_approval_through_injected_handler() {
    let usage = Usage {
        input: 11,
        output: 7,
        ..Usage::default()
    };
    let client =
        StreamingScriptedClient::new(vec![tool_stream(), text_stream(&["It is sunny."], usage)]);
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(FixedInteractionHandler {
        decision: ApprovalDecision::Approve,
    });
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .tool(counting_weather_tool(executions.clone()))
        .approval(Approval::auto_deny())
        .interaction_handler(handler)
        .build()
        .expect("build agent");

    let events = drain_agent_stream(&mut agent, "weather?").await;

    let approval = events.iter().find_map(|event| match event {
        RunEvent::ApprovalRequested(request) => Some(request.tool_name.clone()),
        _ => None,
    });
    assert_eq!(
        approval.as_deref(),
        Some("get_weather"),
        "the injected handler path still emits an ApprovalRequested naming the tool, got {events:?}"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "the injected approve overrides the policy deny so the tool runs"
    );
    assert_eq!(
        streamed_text(&events),
        "It is sunny.",
        "the final text streams after the approved tool round"
    );
}

// --- Milestone 7-F1: injected interaction handler on the restore path ------

/// Snapshots a committed turn, then restores with a re-injected gated handler:
/// the restored agent must pause a gated turn until the host resolves it, and an
/// `approve` lets the gated tool run even though the policy default denies —
/// symmetric with the build-path `injected_interaction_handler_pauses_until_approved`.
#[tokio::test]
async fn restored_interaction_handler_pauses_until_approved() {
    // First, commit a turn on a build-path agent so there is a snapshot to
    // restore from.
    let client = ScriptedClient::new(vec![text_response("First.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let first = agent.run("one").await.unwrap();
    assert_eq!(first.text(), "First.");
    let snapshot = agent.snapshot().expect("snapshot at a committed point");

    // Restore with a fresh scripted client, the re-injected tool, an `auto_deny`
    // policy (so every tool call pauses at the gate), and the gated handler.
    let restore_client =
        ScriptedClient::new(vec![tool_use_response(), text_response("It is sunny.")]);
    let executions = Arc::new(AtomicUsize::new(0));
    let (handler, mut reached_rx, gate_tx) = GatedInteractionHandler::new();
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restore_client)
        .tool(counting_weather_tool(executions.clone()))
        .approval(Approval::auto_deny())
        .interaction_handler(handler)
        .build()
        .expect("restore agent");

    let mut run = Box::pin(restored.run("weather?"));

    // Drive the restored run until the handler is entered, asserting it never
    // completes before the interaction is resolved.
    let mut reached = false;
    for _ in 0..1000 {
        if futures::poll!(run.as_mut()).is_ready() {
            panic!("the restored run completed before the interaction was resolved");
        }
        if reached_rx.try_recv().is_ok() {
            reached = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        reached,
        "the re-injected interaction handler was reached on the restore path"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "the gated tool has not run while the interaction is unresolved"
    );
    assert!(
        matches!(futures::poll!(run.as_mut()), Poll::Pending),
        "the restored run stays paused until the host resolves the interaction"
    );

    // The host approves; only now can the restored run finish and the tool run.
    gate_tx
        .send(ApprovalDecision::Approve)
        .expect("send the decision");
    let reply = run
        .await
        .expect("restored run completes after the interaction resolves");

    assert_eq!(reply.text(), "It is sunny.");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "an approved gated tool runs exactly once after restore"
    );
}

/// Without re-injecting a handler, a restored agent falls back to the
/// conservative synchronous `FacadeApproval`: an `auto_deny` gate leaves the
/// gated tool unexecuted, matching the pre-M7-F1 behavior.
#[tokio::test]
async fn restored_without_handler_falls_back_to_facade_approval() {
    let client = ScriptedClient::new(vec![text_response("First.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    agent.run("one").await.unwrap();
    let snapshot = agent.snapshot().expect("snapshot at a committed point");

    let restore_client = ScriptedClient::new(vec![
        tool_use_response(),
        text_response("I could not run that tool."),
    ]);
    let executions = Arc::new(AtomicUsize::new(0));
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restore_client)
        .tool(counting_weather_tool(executions.clone()))
        .approval(Approval::auto_deny())
        .build()
        .expect("restore agent");

    let reply = restored
        .run("weather?")
        .await
        .expect("restored run completes");

    assert_eq!(reply.text(), "I could not run that tool.");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "a restored agent with no injected handler denies the gated tool via FacadeApproval"
    );
}

// --- Milestone 1-2: AgentRunStream drop-time cleanup -----------------------

/// A client that scripts one streamed turn (served by `chat_stream`) and a
/// text-only recovery turn (served by `chat`).
///
/// It records the message count of every `chat` request so a test can prove that
/// a stream dropped mid-turn left no stranded turn in committed history: the
/// recovery `run` must see only its own user message.
#[derive(Debug)]
struct DropTestClient {
    /// Events the single streamed turn replays through `chat_stream`.
    stream_events: Vec<StreamEvent>,
    /// When set, `chat_stream` parks (never completes) after the scripted events,
    /// stranding the turn mid-fold so a drop has an open turn to abandon.
    park_stream: bool,
    /// The text response every recovery `chat` serves.
    chat_response: Response,
    /// The `messages.len()` recorded for each `chat` request, in order.
    chat_request_lens: Mutex<Vec<usize>>,
}

/// A non-streaming client whose first `chat` call never resolves, then every
/// later call returns a text recovery response.
#[derive(Debug)]
struct RunTimeoutClient {
    calls: AtomicUsize,
    chat_request_lens: Mutex<Vec<usize>>,
    recovery_response: Response,
}

impl RunTimeoutClient {
    fn new(recovery_response: Response) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            chat_request_lens: Mutex::new(Vec::new()),
            recovery_response,
        })
    }

    fn chat_request_lens(&self) -> Vec<usize> {
        self.chat_request_lens.lock().expect("lens mutex").clone()
    }
}

#[async_trait]
impl LlmClient for RunTimeoutClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        self.chat_request_lens
            .lock()
            .expect("lens mutex")
            .push(request.messages.len());
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            std::future::pending::<Result<Response, ClientError>>().await
        } else {
            Ok(self.recovery_response.clone())
        }
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

impl DropTestClient {
    fn new(
        stream_events: Vec<StreamEvent>,
        park_stream: bool,
        chat_response: Response,
    ) -> Arc<Self> {
        Arc::new(Self {
            stream_events,
            park_stream,
            chat_response,
            chat_request_lens: Mutex::new(Vec::new()),
        })
    }

    fn chat_request_lens(&self) -> Vec<usize> {
        self.chat_request_lens.lock().expect("lens mutex").clone()
    }
}

#[async_trait]
impl LlmClient for DropTestClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        self.chat_request_lens
            .lock()
            .expect("lens mutex")
            .push(request.messages.len());
        Ok(self.chat_response.clone())
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        let events = self.stream_events.clone();
        let base = futures::stream::iter(events.into_iter().map(Ok::<_, ClientError>));
        if self.park_stream {
            Ok(base
                .chain(futures::stream::pending::<Result<StreamEvent, ClientError>>())
                .boxed())
        } else {
            Ok(base.boxed())
        }
    }
}

/// A partial text stream head: message start, a text block start, and exactly one
/// text delta — no block stop, usage, or message stop. Chained with `pending()`
/// (via [`DropTestClient::park_stream`]) it emits one live [`RunEvent::TextDelta`]
/// and then parks the turn mid-fold.
fn partial_text_head(chunk: &str) -> Vec<StreamEvent> {
    let id = BlockId::new("text-1");
    vec![
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::Text,
        },
        StreamEvent::BlockDelta {
            id,
            delta: Delta::Text(chunk.to_owned()),
        },
    ]
}

/// An interaction handler that never resolves, used to park a streamed turn at
/// the approval gate so a test can drop the stream while a turn is still open.
#[derive(Debug)]
struct ParkingInteractionHandler;

#[async_trait]
impl InteractionHandler for ParkingInteractionHandler {
    async fn fulfill(&self, _request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        std::future::pending().await
    }
}

/// A `get_weather` tool whose execution never returns, used to park a streamed
/// turn while it is awaiting a tool result so a test can drop it mid-flight.
fn parking_weather_tool() -> Tool {
    Tool::function_with_schema(
        "get_weather",
        "Look up the current weather for a city.",
        json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
        move |_ctx: ToolContext, _args: Value| async move {
            std::future::pending::<Result<String, Infallible>>().await
        },
    )
}

/// Dropping a non-streaming `run` future through a host timeout abandons the
/// stranded LLM requirement synchronously, so the agent is immediately
/// snapshot-able and the next run starts from the previous committed history.
#[tokio::test]
async fn timing_out_non_streaming_run_discards_it_and_leaves_agent_runnable() {
    let client = RunTimeoutClient::new(text_response("recovered."));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("test-model")
        .build()
        .expect("build agent");

    let timed_out = tokio::time::timeout(
        std::time::Duration::from_millis(20),
        agent.run("this call will time out"),
    )
    .await;
    assert!(timed_out.is_err(), "the first run is deliberately parked");

    agent
        .snapshot()
        .expect("timeout guard returns the machine to a snapshot-able point");

    let reply = agent.run("again").await.expect("run after timeout");
    assert_eq!(reply.text(), "recovered.");
    assert_eq!(
        client.chat_request_lens(),
        vec![1, 1],
        "the recovery turn carried only its own user message; the timed-out turn left no residue"
    );
    agent
        .snapshot()
        .expect("recovered agent remains snapshot-able");
}

#[tokio::test]
async fn cancelling_non_streaming_run_stops_it_and_leaves_agent_runnable() {
    let client = RunTimeoutClient::new(text_response("recovered."));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("test-model")
        .build()
        .expect("build agent");
    let cancel = CancelHandle::new();
    let trigger = cancel.clone();

    let run = agent.run_full_with_cancel("cancel me", cancel.clone());
    let canceller = async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        trigger.cancel();
    };
    let (result, ()) = tokio::join!(run, canceller);

    let error = result.expect_err("the run should stop through the cancel handle");
    assert!(
        matches!(&error, FacadeError::Agent(agent) if agent.to_string().contains("cancelled")),
        "cancelled run should surface an agent cancellation diagnostic, got {error:?}"
    );
    assert!(cancel.is_cancelled());

    agent
        .snapshot()
        .expect("cancelled run returns the machine to a snapshot-able point");

    let reply = agent.run("again").await.expect("run after cancel");
    assert_eq!(reply.text(), "recovered.");
    assert_eq!(
        client.chat_request_lens(),
        vec![1, 1],
        "the recovery turn carried only its own user message; the cancelled turn left no residue"
    );
}

#[tokio::test]
async fn cancelling_stream_stops_it_and_leaves_agent_runnable() {
    let client = DropTestClient::new(partial_text_head("It "), true, text_response("recovered."));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("test-model")
        .build()
        .expect("build agent");

    {
        let mut stream = agent.stream("cancel stream").await.expect("open stream");
        let first = stream
            .next()
            .await
            .expect("a first event")
            .expect("event ok");
        assert!(
            matches!(&first, RunEvent::TextDelta(text) if text == "It "),
            "the first streamed event is the partial text delta, got {first:?}"
        );

        stream.cancel();
        let error = stream
            .next()
            .await
            .expect("stream yields a cancellation error")
            .expect_err("cancelled stream should fail terminally");
        assert!(
            matches!(&error, FacadeError::Agent(agent) if agent.to_string().contains("cancelled")),
            "cancelled stream should surface an agent cancellation diagnostic, got {error:?}"
        );
    }

    let reply = agent.run("again").await.expect("run after stream cancel");
    assert_eq!(reply.text(), "recovered.");
    assert_eq!(
        client.chat_request_lens(),
        vec![1],
        "the recovery turn carried only its own user message; the cancelled stream left no residue"
    );
}

/// Dropping a stream that was never polled leaves the machine untouched, so the
/// same agent can immediately `run` again.
#[tokio::test]
async fn dropping_never_polled_stream_leaves_agent_runnable() {
    let client = DropTestClient::new(
        text_stream(&["streamed."], Usage::default()),
        false,
        text_response("recovered."),
    );
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("test-model")
        .tool(counting_weather_tool(Arc::new(AtomicUsize::new(0))))
        .approval(Approval::auto_allow())
        .build()
        .expect("build agent");

    // Open a stream and drop it without ever polling it.
    {
        let _stream = agent.stream("weather?").await.expect("open stream");
    }

    let reply = agent.run("again").await.expect("run after early drop");
    assert_eq!(reply.text(), "recovered.");
    assert_eq!(
        client.chat_request_lens(),
        vec![1],
        "the never-polled stream touched no state; the recovery turn is clean"
    );
}

/// Dropping a stream parked mid-turn inside the LLM fold abandons the in-flight
/// turn: the agent runs again cleanly and the partially streamed turn leaves no
/// residue in committed history.
#[tokio::test]
async fn dropping_partially_streamed_run_discards_it_and_leaves_agent_runnable() {
    let client = DropTestClient::new(partial_text_head("It "), true, text_response("recovered."));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("test-model")
        .tool(counting_weather_tool(Arc::new(AtomicUsize::new(0))))
        .approval(Approval::auto_allow())
        .build()
        .expect("build agent");

    {
        let mut stream = agent.stream("weather?").await.expect("open stream");
        let first = stream
            .next()
            .await
            .expect("a first event")
            .expect("event ok");
        assert!(
            matches!(&first, RunEvent::TextDelta(text) if text == "It "),
            "the first streamed event is the partial text delta, got {first:?}"
        );
        // Drop the stream while its turn is still open (parked in the LLM fold).
    }

    let reply = agent.run("again").await.expect("run after early drop");
    assert_eq!(reply.text(), "recovered.");
    assert_eq!(
        client.chat_request_lens(),
        vec![1],
        "the recovery turn carried only its own user message; the dropped turn left no residue"
    );
}

/// Dropping a stream parked at the approval gate abandons the gated turn: no tool
/// ran, and the agent runs again cleanly with no residue.
#[tokio::test]
async fn dropping_approval_gated_stream_leaves_agent_runnable() {
    let client = DropTestClient::new(tool_stream(), false, text_response("recovered."));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("test-model")
        .tool(counting_weather_tool(executions.clone()))
        .approval(Approval::auto_deny())
        .interaction_handler(Arc::new(ParkingInteractionHandler))
        .build()
        .expect("build agent");

    {
        let mut stream = agent.stream("weather?").await.expect("open stream");
        let first = stream
            .next()
            .await
            .expect("a first event")
            .expect("event ok");
        assert!(
            matches!(
                &first,
                RunEvent::ApprovalRequested(request) if request.tool_name == "get_weather"
            ),
            "the first streamed event is the approval request, got {first:?}"
        );
        // Drop the stream while it is parked awaiting the approval decision.
    }

    let reply = agent.run("again").await.expect("run after early drop");
    assert_eq!(reply.text(), "recovered.");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "the gated tool never executed for the dropped turn"
    );
    assert_eq!(
        client.chat_request_lens(),
        vec![1],
        "the recovery turn carried only its own user message; the dropped turn left no residue"
    );
}

/// Dropping a stream parked while awaiting a tool result abandons the tool phase:
/// the agent runs again cleanly with no residue in committed history.
#[tokio::test]
async fn dropping_tool_awaiting_stream_leaves_agent_runnable() {
    let client = DropTestClient::new(tool_stream(), false, text_response("recovered."));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("test-model")
        .tool(parking_weather_tool())
        .approval(Approval::auto_allow())
        .build()
        .expect("build agent");

    {
        let mut stream = agent.stream("weather?").await.expect("open stream");
        // Drain the events buffered up to the tool call, then stop once the drive
        // parks inside the never-returning tool.
        let mut saw_tool_started = false;
        for _ in 0..1000 {
            let mut next = std::pin::pin!(stream.next());
            match futures::poll!(next.as_mut()) {
                Poll::Ready(Some(item)) => {
                    let event = item.expect("event ok");
                    if matches!(&event, RunEvent::ToolStarted(trace) if trace.name == "get_weather")
                    {
                        saw_tool_started = true;
                    }
                }
                Poll::Ready(None) => break,
                Poll::Pending => break,
            }
        }
        assert!(
            saw_tool_started,
            "the streamed turn reached the tool call before parking"
        );
        // Drop the stream while it is parked awaiting the tool result.
    }

    let reply = agent.run("again").await.expect("run after early drop");
    assert_eq!(reply.text(), "recovered.");
    assert_eq!(
        client.chat_request_lens(),
        vec![1],
        "the recovery turn carried only its own user message; the dropped turn left no residue"
    );
}

// --- Milestone 2-2: streaming vs non-streaming event-contract parity --------

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

/// Same scripted tool round trip, once via `run_full` and once via `stream`:
/// the lifecycle event sequences (tool started/finished, same call id) are
/// identical, and only the streaming path carries token `TextDelta`s.
#[tokio::test]
async fn stream_and_run_full_agree_on_plain_tool_lifecycle() {
    let non_streaming =
        ScriptedClient::new(vec![tool_use_response(), text_response("It is sunny.")]);
    let mut nf_agent = agent_with(
        non_streaming,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let run_full = nf_agent.run_full("weather?").await.unwrap();

    let streaming = StreamingScriptedClient::new(vec![
        tool_stream(),
        text_stream(&["It is sunny."], fixture_usage()),
    ]);
    let mut stream_agent = agent_with(
        streaming,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let stream_events = drain_agent_stream(&mut stream_agent, "weather?").await;
    let done_output = terminal_output(&stream_events);

    assert_eq!(
        &done_output.events, &run_full.events,
        "streaming Done.events matches the non-streaming RunOutput.events"
    );

    assert_eq!(
        lifecycle_signature(&run_full.events),
        lifecycle_signature(&stream_events),
        "streaming and non-streaming agree on the tool lifecycle sequence"
    );
    assert_eq!(
        lifecycle_signature(&run_full.events),
        vec![
            "ToolStarted{name=get_weather,call_id=00000000-0000-0000-0000-00000000000a}".to_owned(),
            "ToolFinished{name=get_weather,call_id=00000000-0000-0000-0000-00000000000a}"
                .to_owned(),
        ],
        "the shared lifecycle is exactly one bracketed tool call"
    );
    // The token-delta boundary: only the streaming path carries `TextDelta`.
    assert!(
        has_text_delta(&stream_events),
        "the streaming path carries token deltas"
    );
    assert!(
        !has_text_delta(&run_full.events),
        "the non-streaming path never fabricates token deltas"
    );
}

/// Same `ask`-tier approved tool round trip on both paths: both surface the
/// enriched `ApprovalRequested` (identical tool, call id, reason, and redacted
/// input) immediately before the tool lifecycle it gated.
#[tokio::test]
async fn stream_and_run_full_agree_on_approved_tool_lifecycle() {
    let non_streaming =
        ScriptedClient::new(vec![tool_use_response(), text_response("It is sunny.")]);
    let mut nf_agent = agent_with(
        non_streaming,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::ask(|_request| ApprovalDecision::Approve),
    );
    let run_full = nf_agent.run_full("weather?").await.unwrap();

    let streaming = StreamingScriptedClient::new(vec![
        tool_stream(),
        text_stream(&["It is sunny."], fixture_usage()),
    ]);
    let mut stream_agent = agent_with(
        streaming,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::ask(|_request| ApprovalDecision::Approve),
    );
    let stream_events = drain_agent_stream(&mut stream_agent, "weather?").await;
    let done_output = terminal_output(&stream_events);

    assert_eq!(
        &done_output.events, &run_full.events,
        "streaming Done.events matches the non-streaming RunOutput.events, including approvals"
    );
    assert!(
        done_output
            .events
            .iter()
            .any(|event| matches!(event, RunEvent::ApprovalRequested(request) if request.tool_name == "get_weather")),
        "streaming Done.events contains the approval request, got {:?}",
        done_output.events
    );

    let signature = lifecycle_signature(&run_full.events);
    assert_eq!(
        signature,
        lifecycle_signature(&stream_events),
        "streaming and non-streaming agree on the approval + tool lifecycle sequence"
    );
    assert_eq!(
        signature.first().map(String::as_str),
        Some(
            "ApprovalRequested{tool=get_weather,\
             call_id=00000000-0000-0000-0000-00000000000a,\
             reason=Some(\"approve execution of tool `get_weather`\"),\
             input=Some(\"{\\\"city\\\":\\\"Shanghai\\\"}\")}"
        ),
        "both paths surface the same enriched approval first, got {signature:?}"
    );
    assert_eq!(
        signature.len(),
        3,
        "approval, then tool started, then finished"
    );
}

/// Same auto-denied tool call on both paths: both surface the paused
/// `ApprovalRequested` and — because a denied tool never executes — neither
/// surfaces any `ToolStarted` or `ToolFinished`. This locks in the alignment
/// fixed in M2-2 (the non-streaming path no longer projects a phantom, empty
/// -named `ToolFinished` for a denied call).
#[tokio::test]
async fn stream_and_run_full_agree_on_denied_tool_lifecycle() {
    let non_streaming = ScriptedClient::new(vec![tool_use_response(), text_response("Denied.")]);
    let mut nf_agent = agent_with(
        non_streaming,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_deny(),
    );
    let run_full = nf_agent.run_full("weather?").await.unwrap();

    let streaming = StreamingScriptedClient::new(vec![
        tool_stream(),
        text_stream(&["Denied."], fixture_usage()),
    ]);
    let mut stream_agent = agent_with(
        streaming,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_deny(),
    );
    let stream_events = drain_agent_stream(&mut stream_agent, "weather?").await;
    let done_output = terminal_output(&stream_events);

    assert_eq!(
        &done_output.events, &run_full.events,
        "streaming Done.events matches the non-streaming RunOutput.events for a denied approval"
    );

    let signature = lifecycle_signature(&run_full.events);
    assert_eq!(
        signature,
        lifecycle_signature(&stream_events),
        "streaming and non-streaming agree on the denied-tool sequence"
    );
    assert_eq!(
        signature,
        vec![
            "ApprovalRequested{tool=get_weather,\
             call_id=00000000-0000-0000-0000-00000000000a,\
             reason=Some(\"approve execution of tool `get_weather`\"),\
             input=Some(\"{\\\"city\\\":\\\"Shanghai\\\"}\")}"
                .to_owned(),
        ],
        "a denied call surfaces only the approval, no tool lifecycle event"
    );
}

/// An assistant response asking to call `tool` with the given provider id and
/// JSON `input` (non-streaming supervisor scripting for delegation parity).
fn tool_call_response(id: &str, tool: &str, input: Value) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.to_owned(),
                name: tool.to_owned(),
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

/// A tool-use response stream calling `tool_name`/`call_id` with `input_json`
/// (streaming supervisor scripting for delegation parity).
fn tool_use_stream(tool_name: &str, call_id: &str, input_json: &str) -> Vec<StreamEvent> {
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
            delta: Delta::Json(input_json.to_owned()),
        },
        StreamEvent::BlockStop { id: id.clone() },
        StreamEvent::MessageStop {
            stop_reason: Normalized::from_mapped(StopReason::ToolUse, "tool_use"),
        },
    ]
}

/// One scripted route of a [`MarkerRoutingClient`]: `chat` is served from
/// `responses` and `chat_stream` from `streams`, each indexed independently and
/// repeating its last entry once exhausted.
struct MarkerRoute {
    marker: &'static str,
    responses: Vec<Response>,
    streams: Vec<Vec<StreamEvent>>,
    chat_calls: Mutex<usize>,
    stream_calls: Mutex<usize>,
}

/// A client that dispatches each request to the [`MarkerRoute`] whose marker
/// appears in the request system prompt, serving `chat` and `chat_stream`
/// independently. This lets a streaming supervisor drive a non-streaming child
/// (which is always driven through `chat`) with one client handle.
struct MarkerRoutingClient {
    routes: Vec<MarkerRoute>,
}

impl MarkerRoutingClient {
    fn new(routes: Vec<MarkerRoute>) -> Arc<Self> {
        Arc::new(Self { routes })
    }

    fn route(&self, system: Option<&str>) -> &MarkerRoute {
        let system = system.unwrap_or_default();
        self.routes
            .iter()
            .find(|route| system.contains(route.marker))
            .expect("a route matches the request system prompt")
    }
}

fn marker_route(
    marker: &'static str,
    responses: Vec<Response>,
    streams: Vec<Vec<StreamEvent>>,
) -> MarkerRoute {
    MarkerRoute {
        marker,
        responses,
        streams,
        chat_calls: Mutex::new(0),
        stream_calls: Mutex::new(0),
    }
}

#[async_trait]
impl LlmClient for MarkerRoutingClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        let route = self.route(request.system.as_deref());
        let mut calls = route.chat_calls.lock().expect("chat calls");
        let index = (*calls).min(route.responses.len() - 1);
        *calls += 1;
        Ok(route.responses[index].clone())
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        let route = self.route(request.system.as_deref());
        let mut calls = route.stream_calls.lock().expect("stream calls");
        let index = (*calls).min(route.streams.len() - 1);
        *calls += 1;
        let events = route.streams[index].clone();
        Ok(futures::stream::iter(events.into_iter().map(Ok::<_, ClientError>)).boxed())
    }
}

/// A one-subagent supervisor delegating to `reviewer`, once via `run_full` and
/// once via `stream`: both paths bracket the delegation with an identical
/// `DelegationStarted` / `DelegationFinished` pair, and only the streaming path
/// carries token `TextDelta`s.
#[tokio::test]
async fn stream_and_run_full_agree_on_delegation_lifecycle() {
    fn build_agent(client: Arc<dyn LlmClient>) -> Agent {
        let reviewer = Agent::worker()
            .description("Strict code reviewer.")
            .system("You are the REVIEWER.")
            .build()
            .expect("worker builds");
        AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("reviewer", reviewer)
            .build()
            .expect("agent builds")
    }

    // Non-streaming supervisor: both supervisor and child answer through `chat`.
    let non_streaming = MarkerRoutingClient::new(vec![
        marker_route(
            "SUPERVISOR",
            vec![
                tool_call_response(
                    "del-1",
                    "ask_reviewer",
                    json!({ "task": "review the diff" }),
                ),
                text_response("Final: the reviewer approved."),
            ],
            Vec::new(),
        ),
        marker_route(
            "REVIEWER",
            vec![text_response("LGTM: no issues found")],
            Vec::new(),
        ),
    ]);
    let mut nf_agent = build_agent(non_streaming);
    let run_full = nf_agent.run_full("Please review the diff.").await.unwrap();

    // Streaming supervisor: the supervisor answers through `chat_stream`, the
    // child is still driven through `chat`.
    let streaming = MarkerRoutingClient::new(vec![
        marker_route(
            "SUPERVISOR",
            Vec::new(),
            vec![
                tool_use_stream("ask_reviewer", "del-1", "{\"task\":\"review the diff\"}"),
                text_stream(&["Final: the reviewer approved."], fixture_usage()),
            ],
        ),
        marker_route(
            "REVIEWER",
            vec![text_response("LGTM: no issues found")],
            Vec::new(),
        ),
    ]);
    let mut stream_agent = build_agent(streaming);
    let stream_events = drain_agent_stream(&mut stream_agent, "Please review the diff.").await;

    assert_eq!(
        lifecycle_signature(&run_full.events),
        lifecycle_signature(&stream_events),
        "streaming and non-streaming agree on the delegation lifecycle sequence"
    );
    assert_eq!(
        lifecycle_signature(&run_full.events),
        vec![
            "DelegationStarted{delegate=reviewer,status=Completed}".to_owned(),
            "DelegationFinished{delegate=reviewer,status=Completed}".to_owned(),
        ],
        "the shared lifecycle is exactly one bracketed delegation"
    );
    assert!(
        has_text_delta(&stream_events),
        "the streaming path carries token deltas"
    );
    assert!(
        !has_text_delta(&run_full.events),
        "the non-streaming path never fabricates token deltas"
    );
}
