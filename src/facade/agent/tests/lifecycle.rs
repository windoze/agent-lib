//! Streaming vs non-streaming lifecycle parity tests for the [`Agent`] facade,
//! split out of `tests.rs`.

use super::*;

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
