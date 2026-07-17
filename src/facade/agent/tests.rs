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
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Map, Value, json};

use super::{Agent, AgentBuilder, AgentSnapshot};
use crate::client::{Capability, ChatRequest, ClientError, LlmClient, Response};
use crate::facade::approval::Approval;
use crate::facade::collab::Collaboration;
use crate::facade::delegate::Delegation;
use crate::facade::error::FacadeError;
use crate::facade::run::RunEvent;
use crate::facade::tool::{Tool, ToolContext};
use crate::model::content::ContentBlock;
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

// -- Streaming, snapshot/restore, and escape-hatch tests (M2-4) --------------

/// A scripted client whose `chat_stream` replays a per-step normalized event
/// sequence (repeating the last once exhausted) and counts how many streams it
/// served, so a tool round trip can script one stream per LLM step offline.
#[derive(Debug)]
struct StreamingScriptedClient {
    scripts: Vec<Vec<StreamEvent>>,
    calls: Mutex<usize>,
}

impl StreamingScriptedClient {
    fn new(scripts: Vec<Vec<StreamEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts,
            calls: Mutex::new(0),
        })
    }

    fn call_count(&self) -> usize {
        *self.calls.lock().expect("calls mutex")
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
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
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
    let id = BlockId::new("tool-1");
    vec![
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "call-1".to_owned(),
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

/// Drives an [`Agent::stream`] to exhaustion, returning every yielded event.
async fn drain_agent_stream(agent: &mut Agent, input: &str) -> Vec<RunEvent> {
    let mut stream = agent.stream(input).await.expect("open stream");
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("stream item is ok"));
    }
    events
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
        RunEvent::ApprovalRequested(request) => Some(request.tool_name.clone()),
        _ => None,
    });
    assert_eq!(
        approval.as_deref(),
        Some("get_weather"),
        "an ApprovalRequested event names the pending tool, got {events:?}"
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
