use super::*;

// ---- rules-routed delegation (`docs/facade-api.md` §13.2) ----

#[test]
fn rules_route_task_first_match_wins_and_is_case_insensitive() {
    let routing = Delegation::rules()
        .when_task_contains(["review", "audit"], "reviewer")
        .when_task_contains(["fix", "compile"], "coder");
    assert!(routing.is_rules_routed());

    // The first rule whose keyword hits wins, even when a later rule would
    // also match — registration order is the routing priority.
    assert_eq!(
        routing.route_task("Please REVIEW and fix the diff"),
        Some("reviewer")
    );
    // Matching is case-insensitive substring containment.
    assert_eq!(routing.route_task("time to COMPILE it"), Some("coder"));
    // No keyword present routes nowhere (the supervisor answers instead).
    assert_eq!(routing.route_task("write documentation"), None);
}

#[test]
fn rules_mode_advertises_no_delegate_tools() {
    let routing = Delegation::rules().when_task_contains(["fix"], "coder");
    assert!(
        routing.declarations(&[], &[]).is_empty(),
        "rules-routed delegation exposes no delegate to the model"
    );
    assert!(routing.external_tool_names(&[]).is_empty());
}

#[test]
fn when_task_contains_switches_a_non_rules_delegation_to_rules() {
    // Chaining onto the default model-routed delegation flips it to rules
    // mode, starting from the single appended rule.
    let routing = Delegation::model_routed().when_task_contains(["fix"], "coder");
    assert!(routing.is_rules_routed());
    assert_eq!(routing.route_task("fix it"), Some("coder"));
}

#[test]
fn unknown_rule_delegate_is_detected_for_build_validation() {
    let routing = Delegation::rules().when_task_contains(["x"], "ghost");
    assert_eq!(
        routing.first_unknown_rule_delegate(&[], &[]).as_deref(),
        Some("ghost")
    );
}

#[tokio::test]
async fn rules_routed_task_routes_to_matching_local_subagent() {
    // No SUPERVISOR route is scripted: a rules-routed turn must not take an
    // LLM step, so the supervisor client is never asked to `chat`.
    let client = RoutingClient::new(vec![route(
        "REVIEWER",
        vec![text_response("LGTM: no issues found")],
    )]);

    let reviewer = Agent::worker()
        .description("Strict code reviewer.")
        .system("You are the REVIEWER.")
        .build()
        .expect("worker builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .delegation(Delegation::rules().when_task_contains(["review", "audit"], "reviewer"))
        .build()
        .expect("agent builds");

    // The model is never told a delegate exists: no delegation tool is
    // advertised on the supervisor spec.
    let advertised: Vec<&str> = agent
        .state()
        .spec()
        .initial_tools()
        .tools()
        .iter()
        .map(|decl| decl.name.as_str())
        .collect();
    assert!(
        !advertised.iter().any(|name| name.starts_with("ask_")),
        "rules-routed delegation advertises no delegate tool, got {advertised:?}"
    );

    let output = agent.run_full("Please review the diff.").await.unwrap();

    // With no supervisor step the delegate's summary is the whole reply.
    assert_eq!(output.reply.text(), "LGTM: no issues found");
    // The supervisor took no LLM step, so its usage slice is zero.
    assert_eq!(output.usage.supervisor.input, 0);
    assert_eq!(output.usage.supervisor.output, 0);

    // Exactly one delegation trace, attributed to the reviewer, completed.
    assert_eq!(output.delegations.len(), 1);
    let trace = &output.delegations[0];
    assert_eq!(trace.delegate, "reviewer");
    assert_eq!(trace.status, DelegationStatus::Completed);

    // Child usage is attributed to the subagent slice, not the supervisor.
    assert_eq!(output.usage.subagents.input, 11);
    assert_eq!(output.usage.subagents.output, 7);

    // The routed exchange is not folded into the supervisor conversation.
    assert!(
        agent.conversation().turns().is_empty(),
        "a rules-routed turn does not commit to the supervisor conversation"
    );

    // Bracketing events: Started then Finished, no ordinary tool events.
    let started = output
        .events
        .iter()
        .position(|event| matches!(event, RunEvent::DelegationStarted(_)))
        .expect("a DelegationStarted event");
    let finished = output
        .events
        .iter()
        .position(|event| matches!(event, RunEvent::DelegationFinished(_)))
        .expect("a DelegationFinished event");
    assert!(started < finished, "DelegationStarted precedes Finished");
    assert!(
        !output
            .events
            .iter()
            .any(|event| matches!(event, RunEvent::ToolStarted(_))),
        "no ordinary tool events for a rules-routed delegation"
    );
}

#[tokio::test]
async fn rules_routed_task_routes_to_external_delegate() {
    // The supervisor client is never asked to chat; only the external
    // delegate's scripted session runs.
    let client = RoutingClient::new(vec![route("SUPERVISOR", vec![text_response("unused")])]);

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("coder", completed_coder())
        .delegation(Delegation::rules().when_task_contains(["refactor", "fix"], "coder"))
        .build()
        .expect("agent builds");

    let output = agent.run_full("Please refactor the parser.").await.unwrap();

    // The external summary is the whole reply.
    assert_eq!(output.reply.text(), "refactor complete");

    // One external delegation trace, completed, with runtime usage on the
    // external slice.
    assert_eq!(output.delegations.len(), 1);
    let trace = &output.delegations[0];
    assert_eq!(trace.delegate, "coder");
    assert_eq!(trace.status, DelegationStatus::Completed);
    assert_eq!(output.usage.external.input, 4);
    assert_eq!(output.usage.external.output, 2);
    assert_eq!(output.usage.subagents.input, 0);

    // The reported artifact surfaces on the run output.
    assert_eq!(output.artifacts.len(), 1);
    assert_eq!(output.artifacts[0].path, "src/parser.rs");

    // The external delegate's resumable session facts are retained for a
    // later snapshot (§15.2).
    let snapshot = agent.snapshot().expect("snapshot at a committed point");
    let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
    assert!(
        json.contains("resume-1"),
        "the retained external session token is persisted in the snapshot"
    );
}

#[tokio::test]
async fn rules_routed_no_match_runs_the_supervisor_normally() {
    // A task matching no rule falls through to the ordinary supervisor drive.
    let client = RoutingClient::new(vec![route(
        "SUPERVISOR",
        vec![text_response("I answered it myself.")],
    )]);

    let reviewer = Agent::worker()
        .description("Strict code reviewer.")
        .system("You are the REVIEWER.")
        .build()
        .expect("worker builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .delegation(Delegation::rules().when_task_contains(["review", "audit"], "reviewer"))
        .build()
        .expect("agent builds");

    let output = agent.run_full("Write the documentation.").await.unwrap();

    // The supervisor answered directly; no delegation happened.
    assert_eq!(output.reply.text(), "I answered it myself.");
    assert!(
        output.delegations.is_empty(),
        "a non-matching task is not delegated"
    );
    assert_eq!(output.usage.supervisor.input, 11);
    // The supervisor turn is committed to the conversation as usual.
    assert!(!agent.conversation().turns().is_empty());
}

#[tokio::test]
async fn rules_routed_first_matching_rule_wins_across_delegates() {
    // Both rules would match "review and refactor"; the first (reviewer) wins.
    let client = RoutingClient::new(vec![
        route("REVIEWER", vec![text_response("reviewer handled it")]),
        route("CODER", vec![text_response("coder handled it")]),
    ]);

    let reviewer = Agent::worker()
        .description("Strict reviewer.")
        .system("You are the REVIEWER.")
        .build()
        .expect("worker builds");
    let coder = Agent::worker()
        .description("Focused coder.")
        .system("You are the CODER.")
        .build()
        .expect("worker builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .subagent("coder", coder)
        .delegation(
            Delegation::rules()
                .when_task_contains(["review"], "reviewer")
                .when_task_contains(["review", "refactor"], "coder"),
        )
        .build()
        .expect("agent builds");

    let output = agent
        .run_full("Please review and refactor the module.")
        .await
        .unwrap();
    assert_eq!(output.reply.text(), "reviewer handled it");
    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].delegate, "reviewer");
}

#[test]
fn rules_routed_unknown_delegate_is_rejected_at_build() {
    let client = RoutingClient::new(vec![route("SUPERVISOR", vec![text_response("x")])]);
    let error = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .delegation(Delegation::rules().when_task_contains(["fix"], "ghost"))
        .build()
        .expect_err("a rule naming an unregistered delegate is rejected");
    assert!(
        matches!(error, crate::facade::FacadeError::Config(_)),
        "an unknown rule delegate is a build-time Config error, got {error:?}"
    );
}

#[tokio::test]
async fn rules_routed_stream_yields_delegation_events_then_done() {
    let client = RoutingClient::new(vec![route(
        "REVIEWER",
        vec![text_response("streamed review done")],
    )]);

    let reviewer = Agent::worker()
        .description("Strict code reviewer.")
        .system("You are the REVIEWER.")
        .build()
        .expect("worker builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .delegation(Delegation::rules().when_task_contains(["review"], "reviewer"))
        .build()
        .expect("agent builds");

    let mut stream = agent
        .stream("Please review the diff.")
        .await
        .expect("stream starts");
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("stream item is ok"));
    }

    assert!(
        events
            .iter()
            .any(|event| matches!(event, RunEvent::DelegationStarted(_))),
        "the stream surfaces a DelegationStarted event"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RunEvent::DelegationFinished(_))),
        "the stream surfaces a DelegationFinished event"
    );
    let done = events
        .iter()
        .find_map(|event| match event {
            RunEvent::Done(output) => Some(output),
            _ => None,
        })
        .expect("the stream ends with a Done event");
    assert_eq!(done.reply.text(), "streamed review done");
    assert_eq!(done.delegations.len(), 1);
    assert_eq!(done.delegations[0].delegate, "reviewer");
}
