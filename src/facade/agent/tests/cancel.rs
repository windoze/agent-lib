//! Cancellation, timeout, and drop-cleanup tests for the [`Agent`] facade,
//! split out of `tests.rs` (M1-2).

use super::*;

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

/// Flags when the future holding it is dropped, proving a blocked tool
/// execution future was detached by the cancelled drive (M3-3).
struct ToolDropProbe(Arc<AtomicUsize>);

impl Drop for ToolDropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A `get_weather` tool that records it started, then blocks forever; its
/// execution future carries a [`ToolDropProbe`] so a test can observe the
/// cancelled drive detaching it.
fn blocking_weather_tool(started: Arc<AtomicUsize>, dropped: Arc<AtomicUsize>) -> Tool {
    Tool::function_with_schema(
        "get_weather",
        "Look up the current weather for a city.",
        json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
        move |_ctx: ToolContext, _args: Value| {
            let started = started.clone();
            let dropped = dropped.clone();
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                let _probe = ToolDropProbe(dropped);
                std::future::pending::<Result<String, Infallible>>().await
            }
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

/// Cancelling a non-streaming run that is blocked inside a never-returning
/// tool pre-empts the batch wait (M3-3): the tool future is detached, the turn
/// ends cancelled within seconds, and the agent runs again with no residue.
#[tokio::test]
async fn cancelling_a_run_blocked_in_a_tool_detaches_it_and_leaves_agent_runnable() {
    let client = ScriptedClient::new(vec![tool_use_response(), text_response("recovered.")]);
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("test-model")
        .tool(blocking_weather_tool(started.clone(), dropped.clone()))
        .approval(Approval::auto_allow())
        .build()
        .expect("build agent");
    let cancel = CancelHandle::new();

    let run = agent.run_full_with_cancel("weather?", cancel.clone());
    let canceller = {
        let started = started.clone();
        let cancel = cancel.clone();
        async move {
            // Cancel only once the tool future is genuinely in flight, so the
            // test cannot pass through the pre-batch cancel check; the bound
            // keeps a regression from hanging the suite.
            for _ in 0..10_000 {
                if started.load(Ordering::SeqCst) > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            cancel.cancel();
        }
    };
    let (result, ()) = tokio::join!(
        async move { tokio::time::timeout(std::time::Duration::from_secs(30), run).await },
        canceller,
    );
    let error = result
        .expect("a pre-empted run settles within seconds, never hangs")
        .expect_err("the run should stop through the cancel handle");
    assert!(
        matches!(&error, FacadeError::Agent(agent) if agent.to_string().contains("cancelled")),
        "cancelled run should surface an agent cancellation diagnostic, got {error:?}"
    );
    assert_eq!(
        started.load(Ordering::SeqCst),
        1,
        "the blocked tool actually started before the cancel"
    );
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "the blocked tool future was detached after the unwind grace"
    );

    // A tool-phase cancel closes the dangling tool_use with a synthesized
    // `Cancelled` result and keeps the coherent pending turn parked at `Idle`;
    // the next run supersedes it, so the recovery turn carries no residue.
    let reply = agent.run("again").await.expect("run after cancel");
    assert_eq!(reply.text(), "recovered.");
    let message_lens: Vec<usize> = client
        .requests()
        .iter()
        .map(|request| request.messages.len())
        .collect();
    assert_eq!(
        message_lens,
        vec![1, 1],
        "the recovery turn carried only its own user message; the cancelled turn left no residue"
    );
}

/// Cancelling a stream that is blocked inside a never-returning tool pre-empts
/// the batch wait exactly like the non-streaming path (M3-3): the tool future
/// is detached, the stream fails terminally within seconds, and the agent runs
/// again with no residue.
#[tokio::test]
async fn cancelling_a_stream_blocked_in_a_tool_detaches_it_and_leaves_agent_runnable() {
    let client = DropTestClient::new(tool_stream(), false, text_response("recovered."));
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("test-model")
        .tool(blocking_weather_tool(started.clone(), dropped.clone()))
        .approval(Approval::auto_allow())
        .build()
        .expect("build agent");

    {
        let mut stream = agent.stream("weather?").await.expect("open stream");
        // Drain the events buffered up to the tool call, stopping once the
        // drive parks inside the never-returning tool.
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
            "the streamed turn reached the tool call before the cancel"
        );

        stream.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_secs(30), stream.next())
            .await
            .expect("a pre-empted stream settles within seconds, never hangs")
            .expect("stream yields a terminal cancellation")
            .expect_err("cancelled stream should fail terminally");
        assert!(
            matches!(&error, FacadeError::Agent(agent) if agent.to_string().contains("cancelled")),
            "cancelled stream should surface an agent cancellation diagnostic, got {error:?}"
        );
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "the blocked tool future was detached after the unwind grace"
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
