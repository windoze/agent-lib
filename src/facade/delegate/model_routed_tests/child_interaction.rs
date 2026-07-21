use super::*;

#[tokio::test]
async fn child_approval_interaction_routes_to_parent_handler_with_origin() {
    let client = RoutingClient::new(vec![
        route(
            "SUPERVISOR",
            vec![
                tool_call_response(
                    "del-9",
                    "ask_reviewer",
                    json!({ "task": "inspect the tree" }),
                ),
                text_response("Final: done."),
            ],
        ),
        route(
            "REVIEWER",
            vec![
                tool_call_response("child-shell-1", "shell", json!({ "cmd": "ls" })),
                text_response("I could not run shell; reporting from memory."),
            ],
        ),
    ]);

    let child_sync_called = Arc::new(AtomicBool::new(false));
    let child_sync_probe = child_sync_called.clone();
    let child_approval = ApprovalPolicy::new(Approval::ask(move |request| {
        if request.tool_name == "shell" {
            child_sync_probe.store(true, Ordering::SeqCst);
        }
        ApprovalDecision::Deny
    }));
    let reviewer = Agent::worker()
        .system("You are the REVIEWER.")
        .tool_declarations(vec![shell_decl()])
        .approval(child_approval)
        .build()
        .expect("worker builds");
    let parent_handler = Arc::new(RecordingParentInteractionHandler::new(
        ApprovalDecision::Deny,
    ));

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .interaction_handler(parent_handler.clone())
        .subagent("reviewer", reviewer)
        .build()
        .expect("agent builds");

    let output = agent.run_full("Delegate an inspection.").await.unwrap();

    assert_eq!(output.reply.text(), "Final: done.");
    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].status, DelegationStatus::Completed);
    assert!(
        !child_sync_called.load(Ordering::SeqCst),
        "the child worker policy gates the call, but the parent handler answers it"
    );
    let seen = parent_handler.seen();
    assert_eq!(seen.len(), 1, "the parent handler receives the child ask");
    let origin = seen[0]
        .origin()
        .expect("child interaction carries delegate attribution");
    assert_eq!(origin.delegate, "reviewer");
    assert_eq!(origin.depth, 1);
    assert!(
        matches!(seen[0].kind(), InteractionKind::Approval { .. }),
        "the forwarded interaction remains an approval"
    );
}

#[tokio::test]
async fn cancelling_while_parent_child_interaction_handler_is_parked_does_not_hang() {
    let client = RoutingClient::new(vec![
        route(
            "SUPERVISOR",
            vec![
                tool_call_response(
                    "del-9",
                    "ask_reviewer",
                    json!({ "task": "inspect the tree" }),
                ),
                text_response("Final: should not be reached."),
            ],
        ),
        route(
            "REVIEWER",
            vec![
                tool_call_response("child-shell-1", "shell", json!({ "cmd": "ls" })),
                text_response("reviewer would continue after an answer"),
            ],
        ),
    ]);

    let reviewer = Agent::worker()
        .system("You are the REVIEWER.")
        .tool_declarations(vec![shell_decl()])
        .approval(ApprovalPolicy::new(Approval::ask(|_| {
            ApprovalDecision::Deny
        })))
        .build()
        .expect("worker builds");
    let (parent_handler, reached_rx) = ParkingParentInteractionHandler::new();
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .interaction_handler(parent_handler)
        .subagent("reviewer", reviewer)
        .build()
        .expect("agent builds");
    let cancel = CancelHandle::new();
    let trigger = cancel.clone();

    let run = agent.run_full_with_cancel("Delegate an inspection.", cancel.clone());
    let canceller = async move {
        let interaction = tokio::time::timeout(std::time::Duration::from_secs(1), reached_rx)
            .await
            .expect("parent handler should be reached before the test timeout")
            .expect("parent handler sends the interaction");
        let origin = interaction
            .origin()
            .expect("parked child interaction carries delegate attribution");
        assert_eq!(origin.delegate, "reviewer");
        assert_eq!(origin.depth, 1);
        trigger.cancel();
    };

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let (result, ()) = tokio::join!(run, canceller);
        result
    })
    .await
    .expect("cancelling a parked child interaction must not hang");

    let error = result.expect_err("the run should stop through cancellation");
    assert!(
        matches!(&error, crate::facade::FacadeError::Agent(agent) if agent.to_string().contains("cancelled")),
        "cancelled run should surface an agent cancellation diagnostic, got {error:?}"
    );
    assert!(cancel.is_cancelled());
}

#[tokio::test]
async fn child_approval_gated_tool_still_triggers_approval() {
    let client = RoutingClient::new(vec![
        route(
            "SUPERVISOR",
            vec![
                tool_call_response(
                    "del-9",
                    "ask_reviewer",
                    json!({ "task": "inspect the tree" }),
                ),
                text_response("Final: done."),
            ],
        ),
        route(
            "REVIEWER",
            vec![
                tool_call_response("child-shell-1", "shell", json!({ "cmd": "ls" })),
                text_response("I could not run shell; reporting from memory."),
            ],
        ),
    ]);

    // The child's approval handler records that it was consulted, then denies
    // so the gated tool never executes (§9.2).
    let consulted = Arc::new(AtomicBool::new(false));
    let flag = consulted.clone();
    let child_approval = ApprovalPolicy::new(Approval::ask(move |request| {
        if request.tool_name == "shell" {
            flag.store(true, Ordering::SeqCst);
        }
        ApprovalDecision::Deny
    }));

    let reviewer = Agent::worker()
        .system("You are the REVIEWER.")
        .tool_declarations(vec![shell_decl()])
        .approval(child_approval)
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

    let output = agent.run_full("Delegate an inspection.").await.unwrap();

    assert!(
        consulted.load(Ordering::SeqCst),
        "the child's approval-requiring tool still triggered approval"
    );
    assert_eq!(output.reply.text(), "Final: done.");
    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].status, DelegationStatus::Completed);
}
