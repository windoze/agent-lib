use super::*;

#[tokio::test]
async fn snapshot_carries_delegates_and_restore_can_delegate_again() {
    let client = RoutingClient::new(vec![
        route(
            "SUPERVISOR",
            vec![
                tool_call_response("d1", "ask_reviewer", json!({ "task": "review the diff" })),
                text_response("Final: first pass done."),
            ],
        ),
        route("REVIEWER", vec![text_response("review: LGTM")]),
    ]);

    let reviewer = Agent::worker()
        .description("Strict reviewer.")
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

    agent.run_full("Please review.").await.unwrap();

    // The snapshot carries the delegate as a data-only recipe and the
    // model-routed delegation mode.
    let snapshot = agent.snapshot().expect("snapshot at a committed point");
    assert_eq!(snapshot.delegates.len(), 1);
    assert_eq!(snapshot.delegates[0].name, "reviewer");
    assert_eq!(snapshot.delegates[0].description, "Strict reviewer.");
    assert!(snapshot.delegates[0].inherit_model);
    assert!(snapshot.pending_delegations.is_empty());
    assert_eq!(snapshot.delegation, Delegation::model_routed());

    // A restored agent re-advertises the delegate and can delegate again.
    let restore_client = RoutingClient::new(vec![
        route(
            "SUPERVISOR",
            vec![
                tool_call_response("d2", "ask_reviewer", json!({ "task": "review again" })),
                text_response("Final: second pass done."),
            ],
        ),
        route("REVIEWER", vec![text_response("review: still LGTM")]),
    ]);
    let restore_reviewer = Agent::worker()
        .system("You are the REVIEWER.")
        .approval(Approval::auto_allow())
        .build()
        .expect("worker builds");
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restore_client)
        .approval(Approval::auto_allow())
        .subagent("reviewer", restore_reviewer)
        .build()
        .expect("restore agent");

    let output = restored.run_full("Review once more.").await.unwrap();
    assert_eq!(output.reply.text(), "Final: second pass done.");
    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].delegate, "reviewer");
    assert!(
        tool_result_texts(&restored)
            .iter()
            .any(|text| text == "review: still LGTM"),
        "the restored agent drove its re-registered delegate"
    );
}

#[tokio::test]
async fn snapshot_does_not_persist_the_task_brief_in_delegation_data() {
    // A distinctive brief only the supervising model routes through the
    // delegation tool call.
    const BRIEF: &str = "SECRET_TASK_BRIEF_9f2a";
    let client = RoutingClient::new(vec![
        route(
            "SUPERVISOR",
            vec![
                tool_call_response("d1", "ask_reviewer", json!({ "task": BRIEF })),
                text_response("Final: done."),
            ],
        ),
        route("REVIEWER", vec![text_response("review: LGTM")]),
    ]);

    let reviewer = Agent::worker()
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

    agent
        .run_full("Delegate with a secret brief.")
        .await
        .unwrap();
    let snapshot = agent.snapshot().expect("snapshot");

    // The delegation-specific persistence (delegate recipes + in-flight
    // delegations) never carries the runtime task brief (R5): delegates hold
    // only static spec, and no child is left in flight.
    let delegates_json = serde_json::to_string(&snapshot.delegates).expect("serialize delegates");
    assert!(
        !delegates_json.contains(BRIEF),
        "delegate recipes must not carry the runtime task brief"
    );
    let pending_json =
        serde_json::to_string(&snapshot.pending_delegations).expect("serialize pending");
    assert!(
        !pending_json.contains(BRIEF),
        "pending-delegation persistence must not carry the runtime task brief"
    );
    assert!(snapshot.pending_delegations.is_empty());
}

#[tokio::test]
async fn delegation_snapshot_round_trips_and_rebuilds_child_conversation() {
    // Drive a standalone child agent to produce a committed child
    // conversation, then round-trip it through a `DelegationSnapshot` and
    // rebuild the child's live conversation from it (§15.2).
    let child_client = RoutingClient::new(vec![route(
        "REVIEWER",
        vec![text_response("review: child ran")],
    )]);
    let mut child = AgentBuilder::default()
        .client(child_client)
        .model("child-model")
        .system("You are the REVIEWER.")
        .approval(Approval::auto_allow())
        .build()
        .expect("child agent builds");
    child.run_full("child task").await.unwrap();
    let turns_before = child.conversation().turns().len();
    assert!(turns_before > 0);

    let pending =
        DelegationSnapshot::capture("reviewer", child.conversation()).expect("capture pending");
    assert_eq!(pending.delegate, "reviewer");

    // Serde round-trip preserves the pending delegation exactly.
    let json = serde_json::to_string(&pending).expect("serialize pending");
    let restored: DelegationSnapshot = serde_json::from_str(&json).expect("deserialize pending");
    assert_eq!(restored, pending);

    // Restore rebuilds the child's live conversation with its committed turns.
    let rebuilt = restored
        .restore_conversation()
        .expect("rebuild child conversation");
    assert_eq!(rebuilt.turns().len(), turns_before);
}
