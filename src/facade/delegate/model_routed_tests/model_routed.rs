use super::*;

#[tokio::test]
async fn model_routed_delegation_drives_child_and_folds_result() {
    let client = RoutingClient::new(vec![
        route(
            "SUPERVISOR",
            vec![
                tool_call_response(
                    "del-1",
                    "ask_reviewer",
                    json!({ "task": "review the diff" }),
                ),
                text_response("Final: the reviewer approved."),
            ],
        ),
        route("REVIEWER", vec![text_response("LGTM: no issues found")]),
    ]);

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
        .build()
        .expect("agent builds");

    let output = agent.run_full("Please review the diff.").await.unwrap();

    // The supervisor advanced past the delegation to its final message.
    assert_eq!(output.reply.text(), "Final: the reviewer approved.");

    // The child summary was folded back as the delegation tool result.
    assert!(
        tool_result_texts(&agent)
            .iter()
            .any(|text| text == "LGTM: no issues found"),
        "the child summary is folded back as the tool result"
    );

    // Exactly one delegation trace, attributed to the reviewer, completed,
    // carrying the child's usage.
    assert_eq!(output.delegations.len(), 1);
    let trace = &output.delegations[0];
    assert_eq!(trace.delegate, "reviewer");
    assert_eq!(trace.status, DelegationStatus::Completed);
    assert_eq!(trace.usage.input, 11);
    assert_eq!(trace.usage.output, 7);

    // Child usage is attributed to the subagent slice, not the supervisor.
    assert_eq!(output.usage.subagents.input, 11);
    assert_eq!(output.usage.subagents.output, 7);

    // The delegation is not double-counted as an ordinary tool call.
    assert!(
        output.tool_calls.is_empty(),
        "a delegation is not an ordinary tool call"
    );

    // The event order brackets the delegation with Started then Finished.
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
        "no ordinary tool events for a delegation"
    );
}

// -----------------------------------------------------------------------
// Delegation config, multi-delegate, and snapshot coverage (milestone M3-3)
// -----------------------------------------------------------------------

#[tokio::test]
async fn two_subagents_each_expose_independent_tools_and_route() {
    // The supervisor calls each delegate's own `ask_<name>` tool in turn;
    // each routes to the matching child and folds that child's summary back.
    let client = RoutingClient::new(vec![
        route(
            "SUPERVISOR",
            vec![
                tool_call_response("d1", "ask_reviewer", json!({ "task": "review" })),
                tool_call_response("d2", "ask_researcher", json!({ "task": "research" })),
                text_response("Final: both done."),
            ],
        ),
        route("REVIEWER", vec![text_response("review: LGTM")]),
        route("RESEARCHER", vec![text_response("research: found it")]),
    ]);

    let reviewer = Agent::worker()
        .description("Strict reviewer.")
        .system("You are the REVIEWER.")
        .build()
        .expect("worker builds");
    let researcher = Agent::worker()
        .description("Focused researcher.")
        .system("You are the RESEARCHER.")
        .build()
        .expect("worker builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .build()
        .expect("agent builds");

    // Both `ask_reviewer` and `ask_researcher` are advertised to the model.
    let advertised: Vec<&str> = agent
        .state()
        .spec()
        .initial_tools()
        .tools()
        .iter()
        .map(|decl| decl.name.as_str())
        .collect();
    assert!(advertised.contains(&"ask_reviewer"));
    assert!(advertised.contains(&"ask_researcher"));

    let output = agent.run_full("Do both.").await.unwrap();
    assert_eq!(output.reply.text(), "Final: both done.");

    // One trace per delegate, recorded in call order.
    assert_eq!(output.delegations.len(), 2);
    assert_eq!(output.delegations[0].delegate, "reviewer");
    assert_eq!(output.delegations[1].delegate, "researcher");
    assert!(
        output
            .delegations
            .iter()
            .all(|trace| trace.status == DelegationStatus::Completed)
    );

    // Each child's summary was folded back as its own tool result.
    let texts = tool_result_texts(&agent);
    assert!(texts.iter().any(|text| text == "review: LGTM"));
    assert!(texts.iter().any(|text| text == "research: found it"));
}

#[tokio::test]
async fn single_tool_delegation_routes_by_agent_argument() {
    // One unified `delegate(agent, task)` tool routes to the delegate named
    // by the `agent` argument.
    let client = RoutingClient::new(vec![
        route(
            "SUPERVISOR",
            vec![
                tool_call_response(
                    "d1",
                    "delegate",
                    json!({ "agent": "researcher", "task": "dig in" }),
                ),
                text_response("Final: routed."),
            ],
        ),
        route("REVIEWER", vec![text_response("review: unused")]),
        route(
            "RESEARCHER",
            vec![text_response("research: the answer is 42")],
        ),
    ]);

    let reviewer = Agent::worker()
        .system("You are the REVIEWER.")
        .build()
        .expect("worker builds");
    let researcher = Agent::worker()
        .system("You are the RESEARCHER.")
        .build()
        .expect("worker builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .delegation(Delegation::single_tool("delegate"))
        .build()
        .expect("agent builds");

    // Exactly one unified delegation tool is advertised (no `ask_*`).
    let delegation_tools: Vec<&str> = agent
        .state()
        .spec()
        .initial_tools()
        .tools()
        .iter()
        .map(|decl| decl.name.as_str())
        .filter(|name| *name == "delegate" || name.starts_with("ask_"))
        .collect();
    assert_eq!(delegation_tools, vec!["delegate"]);

    let output = agent.run_full("Route this.").await.unwrap();
    assert_eq!(output.reply.text(), "Final: routed.");

    // The call routed to the researcher, and only the researcher.
    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].delegate, "researcher");
    let texts = tool_result_texts(&agent);
    assert!(
        texts
            .iter()
            .any(|text| text == "research: the answer is 42")
    );
    assert!(texts.iter().all(|text| text != "review: unused"));
}

#[test]
fn duplicate_delegate_name_is_rejected_at_build() {
    let first = Agent::worker()
        .system("You are the REVIEWER.")
        .build()
        .expect("worker builds");
    let second = Agent::worker()
        .system("You are another REVIEWER.")
        .build()
        .expect("worker builds");

    let error = AgentBuilder::default()
        .client(RoutingClient::new(vec![route(
            "SUPERVISOR",
            vec![text_response("unused")],
        )]))
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .subagent("reviewer", first)
        .subagent("reviewer", second)
        .build()
        .expect_err("two delegates under the same name collide");

    match error {
        crate::facade::FacadeError::DuplicateTool { name } => {
            assert_eq!(name, "ask_reviewer");
        }
        other => panic!("expected a DuplicateTool error, got {other:?}"),
    }
}
