//! Non-streaming run / `run_full` tests for the [`Agent`] facade, split out of
//! `tests.rs`: tool round trips, traces, approvals, and loop budgets.

use super::*;

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
        super::super::classify_error(&limit),
        FacadeError::LoopLimitExceeded
    ));

    let ordinary = ErrorCursor::new("legacy loop step limit words in an unrelated error")
        .expect("ordinary error cursor");
    match super::super::classify_error(&ordinary) {
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
