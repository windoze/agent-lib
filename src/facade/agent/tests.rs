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

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::{Map, Value, json};

use super::{Agent, AgentBuilder};
use crate::client::{Capability, ChatRequest, ClientError, LlmClient, Response};
use crate::facade::approval::Approval;
use crate::facade::error::FacadeError;
use crate::facade::run::RunEvent;
use crate::facade::tool::{Tool, ToolContext};
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::normalized::StopReason;
use crate::model::usage::Usage;
use crate::stream::StreamEvent;

/// A scripted client that returns responses in order, repeating the last once
/// the script is exhausted, and counts how many `chat` calls it served.
#[derive(Debug)]
struct ScriptedClient {
    responses: Vec<Response>,
    calls: Mutex<usize>,
}

impl ScriptedClient {
    fn new(responses: Vec<Response>) -> Arc<Self> {
        Arc::new(Self {
            responses,
            calls: Mutex::new(0),
        })
    }

    fn call_count(&self) -> usize {
        *self.calls.lock().expect("calls mutex")
    }
}

#[async_trait]
impl LlmClient for ScriptedClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
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
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        },
        usage: Usage {
            input: 11,
            output: 7,
            ..Usage::default()
        },
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
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
