//! Streaming [`Agent::stream`] tests for the [`Agent`] facade, split out of
//! `tests.rs`.

use super::*;

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
async fn reconfigure_replace_tool_set_updates_streaming_registry() {
    let usage = Usage {
        input: 11,
        output: 7,
        ..Usage::default()
    };
    let client = StreamingScriptedClient::new(vec![
        tool_stream_for("read_calendar", "call-calendar", "{\"day\":\"Monday\"}"),
        text_stream(&["calendar checked"], usage),
    ]);
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let calendar_calls = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with_tools(
        client.clone(),
        vec![
            counting_weather_tool(weather_calls.clone()),
            counting_calendar_tool(calendar_calls.clone()),
        ],
        Approval::auto_allow(),
    );
    let replacement = ToolSetRef::new(reconfig_tool_set_id(1), vec![calendar_tool_decl()]);

    agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("replace-tool-set reconfig is accepted");

    let events = drain_agent_stream(&mut agent, "Use the current tool set.").await;

    assert_eq!(streamed_text(&events), "calendar checked");
    assert_eq!(agent.state().current_tool_set(), &replacement);
    assert_eq!(calendar_calls.load(Ordering::SeqCst), 1);
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);
    let requests = client.chat_requests();
    assert_eq!(tool_names(&requests[0].tools), vec!["read_calendar"]);
    assert_eq!(terminal_output(&events).tool_calls[0].name, "read_calendar");
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
