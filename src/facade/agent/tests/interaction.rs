//! Injected async interaction-handler tests (M7-1) for the [`Agent`] facade,
//! split out of `tests.rs`.

use super::*;

// --- Milestone 7-1: injected async interaction handler ---------------------
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
