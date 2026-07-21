use super::*;

#[tokio::test]
async fn model_routed_external_delegation_records_trace_artifacts_and_usage() {
    use crate::facade::ManagedExternalAgent;

    let client = RoutingClient::new(vec![route(
        "SUPERVISOR",
        vec![
            tool_call_response(
                "del-1",
                "ask_coder",
                json!({ "task": "refactor the parser" }),
            ),
            text_response("Final: the coder finished."),
        ],
    )]);

    let handler = completed_external_handler(
        "refactor complete",
        "src/parser.rs",
        Usage {
            input: 13,
            output: 9,
            ..Usage::default()
        },
    );
    let coder = ManagedExternalAgent::claude_code()
        .session_handler(Arc::new(handler))
        .build()
        .expect("managed external agent builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("coder", coder)
        .build()
        .expect("agent builds");

    let output = agent.run_full("Please refactor the parser.").await.unwrap();

    // The supervisor advanced past the delegation to its final message.
    assert_eq!(output.reply.text(), "Final: the coder finished.");

    // The external session summary was folded back as the tool result.
    assert!(
        tool_result_texts(&agent)
            .iter()
            .any(|text| text == "refactor complete"),
        "the external summary is folded back as the tool result"
    );

    // Exactly one delegation trace, attributed to the external delegate,
    // completed, carrying the runtime-reported usage.
    assert_eq!(output.delegations.len(), 1);
    let trace = &output.delegations[0];
    assert_eq!(trace.delegate, "coder");
    assert_eq!(trace.status, DelegationStatus::Completed);
    assert_eq!(trace.usage.input, 13);
    assert_eq!(trace.usage.output, 9);

    // External usage is attributed to the external slice, not the subagent or
    // supervisor slices (§17.3).
    assert_eq!(output.usage.external.input, 13);
    assert_eq!(output.usage.external.output, 9);
    assert_eq!(output.usage.subagents.input, 0);
    assert_eq!(output.usage.subagents.output, 0);

    // The reported artifact surfaces on the run output, projected to its
    // locating path.
    assert_eq!(output.artifacts.len(), 1);
    assert_eq!(output.artifacts[0].path, "src/parser.rs");

    // The delegation is not double-counted as an ordinary tool call.
    assert!(
        output.tool_calls.is_empty(),
        "an external delegation is not an ordinary tool call"
    );
    assert!(
        !output
            .events
            .iter()
            .any(|event| matches!(event, RunEvent::ToolStarted(_))),
        "no ordinary tool events for a delegation"
    );

    // The event order brackets the delegation with Started, then the
    // artifact, then Finished.
    let started = output
        .events
        .iter()
        .position(|event| matches!(event, RunEvent::DelegationStarted(_)))
        .expect("a DelegationStarted event");
    let artifact = output
        .events
        .iter()
        .position(|event| matches!(event, RunEvent::DelegationArtifact(_)))
        .expect("a DelegationArtifact event");
    let finished = output
        .events
        .iter()
        .position(|event| matches!(event, RunEvent::DelegationFinished(_)))
        .expect("a DelegationFinished event");
    assert!(
        started < artifact,
        "DelegationStarted precedes the artifact"
    );
    assert!(
        artifact < finished,
        "the artifact precedes DelegationFinished"
    );
}

#[tokio::test]
async fn external_collab_observations_bridge_into_provisioned_primitives() {
    // §14 末段: an external delegate's send_message / plan_update /
    // blackboard_post observations reflect into the facade's provisioned
    // collab substrate. An explicit `Collaboration` provisions all three
    // primitives (a lone external delegate would otherwise only get
    // artifacts), so the bridge has somewhere to write.
    use crate::facade::{Collaboration, ManagedExternalAgent};

    let recipient = AgentId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890c4").expect("agent id");
    let client = RoutingClient::new(vec![route(
        "SUPERVISOR",
        vec![
            tool_call_response(
                "del-1",
                "ask_coder",
                json!({ "task": "refactor the parser" }),
            ),
            text_response("Final: the coder finished."),
        ],
    )]);

    let coder = ManagedExternalAgent::claude_code()
        .session_handler(Arc::new(collab_external_handler(
            "refactor complete",
            recipient,
        )))
        .build()
        .expect("managed external agent builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("coder", coder)
        .collaboration(
            Collaboration::new()
                .plan()
                .blackboard()
                .mailbox()
                .artifacts(),
        )
        .build()
        .expect("agent builds");

    agent.run_full("Please refactor the parser.").await.unwrap();

    // send_message → the shared mailbox, attributed to the delegate.
    let mailbox = agent.mailbox().expect("mailbox provisioned");
    let inbox = mailbox.inbox(&recipient.to_string());
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "coder");
    assert_eq!(inbox[0].text, "please review the parser change");

    // plan_update → the shared plan, reconciled to the reported status and
    // owned by the delegate.
    let plan = agent.plan().expect("plan provisioned");
    let snapshot = plan.snapshot();
    let task = snapshot.tasks.get("parser").expect("task reflected");
    assert_eq!(task.status, crate::agent::collab::TaskStatus::Completed);
    assert_eq!(task.owner.as_deref(), Some("coder"));

    // blackboard_post → the shared blackboard channel it named.
    let blackboard = agent.blackboard().expect("blackboard provisioned");
    let posts = blackboard.snapshot("status");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].sender, "coder");
    assert_eq!(posts[0].text, "parser done");
}

#[tokio::test]
async fn external_delegate_is_advertised_as_an_ask_tool() {
    use crate::facade::ManagedExternalAgent;

    let handler = completed_external_handler(
        "done",
        "src/lib.rs",
        Usage {
            input: 1,
            output: 1,
            ..Usage::default()
        },
    );
    let coder = ManagedExternalAgent::claude_code()
        .session_handler(Arc::new(handler))
        .build()
        .expect("managed external agent builds");

    let agent = AgentBuilder::default()
        .client(RoutingClient::new(vec![route(
            "SUPERVISOR",
            vec![text_response("nothing to do")],
        )]))
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .external_agent("coder", coder)
        .build()
        .expect("agent builds");

    // The delegate is registered and exposed as its own `ask_coder` tool.
    assert_eq!(agent.external_agents().len(), 1);
    assert_eq!(agent.external_agents()[0].name(), "coder");
    assert!(
        agent
            .state()
            .spec()
            .initial_tools()
            .tools()
            .iter()
            .any(|tool| tool.name == "ask_coder"),
        "the external delegate mints an `ask_coder` delegation tool"
    );
}

#[tokio::test]
async fn external_delegation_without_session_handler_fails_the_delegation() {
    use crate::facade::ManagedExternalAgent;

    let client = RoutingClient::new(vec![route(
        "SUPERVISOR",
        vec![
            tool_call_response(
                "del-1",
                "ask_coder",
                json!({ "task": "refactor the parser" }),
            ),
            text_response("Final: gave up on the coder."),
        ],
    )]);

    // No session handler is attached, so the delegation cannot be driven.
    let coder = ManagedExternalAgent::claude_code()
        .build()
        .expect("managed external agent builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("coder", coder)
        .build()
        .expect("agent builds");

    let output = agent.run_full("Please refactor the parser.").await.unwrap();

    // The supervisor still reached its final message after the failed tool.
    assert_eq!(output.reply.text(), "Final: gave up on the coder.");

    // The delegation is recorded as failed, with no artifacts.
    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].delegate, "coder");
    assert_eq!(output.delegations[0].status, DelegationStatus::Failed);
    assert!(
        output.artifacts.is_empty(),
        "a failed drive yields no artifacts"
    );

    // A failed delegation emits Failed, never Finished.
    assert!(
        output
            .events
            .iter()
            .any(|event| matches!(event, RunEvent::DelegationFailed(_))),
        "a failed external delegation emits DelegationFailed"
    );
    assert!(
        !output
            .events
            .iter()
            .any(|event| matches!(event, RunEvent::DelegationFinished(_))),
        "a failed external delegation does not emit DelegationFinished"
    );
}

#[tokio::test]
async fn external_delegation_denied_by_auto_deny_surfaces_approval_denied() {
    let mut agent = AgentBuilder::default()
        .client(external_supervisor_client())
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(ApprovalPolicy::default().tool("ask_coder", Approval::auto_deny()))
        .external_agent("coder", completed_coder())
        .build()
        .expect("agent builds");

    let error = agent
        .run_full("Please refactor.")
        .await
        .expect_err("an auto-denied external delegate fails the run");
    assert!(
        matches!(error, crate::facade::FacadeError::ApprovalDenied),
        "auto_deny on the external start tool surfaces ApprovalDenied, got {error:?}"
    );

    // The denied external agent never drove a session, so no summary was
    // folded back as a tool result.
    assert!(
        !tool_result_texts(&agent)
            .iter()
            .any(|text| text == "refactor complete"),
        "a denied external delegate is not driven"
    );
}

#[tokio::test]
async fn external_delegation_denied_headless_when_ask_external_agents_has_no_handler() {
    // `ask_external_agents` with an auto-allow default and no `ask` handler is
    // a headless run: the external start is denied rather than left blocking.
    let mut agent = AgentBuilder::default()
        .client(external_supervisor_client())
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(ApprovalPolicy::from(Approval::auto_allow()).ask_external_agents())
        .external_agent("coder", completed_coder())
        .build()
        .expect("agent builds");

    let error = agent
        .run_full("Please refactor.")
        .await
        .expect_err("a headless ask_external_agents run denies the external start");
    assert!(
        matches!(error, crate::facade::FacadeError::ApprovalDenied),
        "headless ask_external_agents surfaces ApprovalDenied, got {error:?}"
    );
}

#[tokio::test]
async fn external_start_ask_external_agents_routes_to_parent_handler() {
    let parent_handler = Arc::new(RecordingParentInteractionHandler::new(
        ApprovalDecision::Approve,
    ));
    let mut agent = AgentBuilder::default()
        .client(external_supervisor_client())
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(ApprovalPolicy::from(Approval::auto_allow()).ask_external_agents())
        .interaction_handler(parent_handler.clone())
        .external_agent("coder", completed_coder())
        .build()
        .expect("agent builds");

    let output = agent.run_full("Please refactor.").await.unwrap();

    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].delegate, "coder");
    assert_eq!(output.delegations[0].status, DelegationStatus::Completed);
    assert!(
        tool_result_texts(&agent)
            .iter()
            .any(|text| text == "refactor complete"),
        "an async-approved external delegate is driven"
    );

    let seen = parent_handler.seen();
    assert_eq!(seen.len(), 1, "the parent handler receives the start ask");
    let origin = seen[0]
        .origin()
        .expect("external-start approval carries delegate attribution");
    assert_eq!(origin.delegate, "coder");
    assert_eq!(origin.depth, 1);
    let InteractionKind::Approval { requirement, .. } = seen[0].kind() else {
        panic!("external-start approval uses the approval interaction family");
    };
    assert!(
        requirement
            .reason()
            .is_some_and(|reason| reason.contains("managed external agent `coder`")),
        "the approval reason identifies the delegate start"
    );
}

#[tokio::test]
async fn external_start_ask_tool_denied_by_parent_handler_surfaces_approval_denied() {
    let parent_handler = Arc::new(RecordingParentInteractionHandler::new(
        ApprovalDecision::Deny,
    ));
    let mut agent = AgentBuilder::default()
        .client(external_supervisor_client())
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(ApprovalPolicy::default().ask_tool("ask_coder"))
        .interaction_handler(parent_handler.clone())
        .external_agent("coder", completed_coder())
        .build()
        .expect("agent builds");

    let error = agent
        .run_full("Please refactor.")
        .await
        .expect_err("the parent handler denies the external start");
    assert!(
        matches!(error, crate::facade::FacadeError::ApprovalDenied),
        "async-denied external start surfaces ApprovalDenied, got {error:?}"
    );
    assert_eq!(
        parent_handler.seen().len(),
        1,
        "per-tool ask_tool routes the start ask to the parent handler"
    );
    assert!(
        !tool_result_texts(&agent)
            .iter()
            .any(|text| text == "refactor complete"),
        "a denied external delegate is not driven"
    );
}

#[tokio::test]
async fn external_start_ask_family_mismatched_answer_surfaces_approval_denied() {
    // M3-R (C9): a parent handler that answers the external-start approval
    // with a wrong-family response must not start the delegate — the
    // mismatched answer is treated as a denial.
    let parent_handler = Arc::new(MismatchedFamilyParentHandler::new());
    let mut agent = AgentBuilder::default()
        .client(external_supervisor_client())
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(ApprovalPolicy::from(Approval::auto_allow()).ask_external_agents())
        .interaction_handler(parent_handler.clone())
        .external_agent("coder", completed_coder())
        .build()
        .expect("agent builds");

    let error = agent
        .run_full("Please refactor.")
        .await
        .expect_err("a wrong-family answer to the start ask denies the start");
    assert!(
        matches!(error, crate::facade::FacadeError::ApprovalDenied),
        "family-mismatched start answer surfaces ApprovalDenied, got {error:?}"
    );
    assert_eq!(
        parent_handler.seen().len(),
        1,
        "the parent handler still received the start ask"
    );
    assert!(
        !tool_result_texts(&agent)
            .iter()
            .any(|text| text == "refactor complete"),
        "a mismatched-answer external delegate is not driven"
    );
}

#[tokio::test]
async fn external_delegation_approved_by_ask_handler_runs_to_completion() {
    let approved = Arc::new(AtomicBool::new(false));
    let approved_probe = approved.clone();
    let policy = ApprovalPolicy::default().tool(
        "ask_coder",
        Approval::ask(move |request| {
            if request.tool_name == "ask_coder" {
                approved_probe.store(true, Ordering::SeqCst);
                ApprovalDecision::Approve
            } else {
                ApprovalDecision::Deny
            }
        }),
    );

    let mut agent = AgentBuilder::default()
        .client(external_supervisor_client())
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(policy)
        .external_agent("coder", completed_coder())
        .build()
        .expect("agent builds");

    let output = agent.run_full("Please refactor.").await.unwrap();

    assert!(
        approved.load(Ordering::SeqCst),
        "the external start consulted the ask handler"
    );
    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].delegate, "coder");
    assert_eq!(output.delegations[0].status, DelegationStatus::Completed);
    assert!(
        tool_result_texts(&agent)
            .iter()
            .any(|text| text == "refactor complete"),
        "an approved external delegate is driven and folds its summary back"
    );
}

#[tokio::test]
async fn driven_external_snapshot_is_data_only_with_session_facts() {
    use crate::agent::external::ExternalRuntimeKind;
    use crate::facade::ExternalDelegateStatus;

    let mut agent = AgentBuilder::default()
        .client(external_supervisor_client())
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("coder", completed_coder())
        .build()
        .expect("agent builds");

    agent.run_full("Please refactor.").await.unwrap();

    let snapshot = agent.snapshot().expect("snapshot at a committed point");
    assert_eq!(snapshot.external_delegates.len(), 1);
    let delegate = &snapshot.external_delegates[0];
    assert_eq!(delegate.name, "coder");
    assert_eq!(delegate.runtime, ExternalRuntimeKind::ClaudeCode);
    assert_eq!(delegate.status, ExternalDelegateStatus::Completed);

    // The resumable session facts are captured as data (session id + resume
    // token), and the reported artifact surfaces on the snapshot.
    let session = delegate.session.as_ref().expect("a captured session ref");
    assert_eq!(session.session_id.as_deref(), Some("sess-1"));
    assert_eq!(session.resume_token.as_deref(), Some("resume-1"));
    assert_eq!(delegate.artifacts.len(), 1);
    assert_eq!(delegate.artifacts[0].path, "src/parser.rs");

    // The snapshot serializes to data only — no runtime handle or closure
    // leaks into the persisted form — and round-trips exactly.
    let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
    assert!(
        !json.contains("session_handler") && !json.contains("handler"),
        "no runtime session handler leaks into the snapshot"
    );
    let restored: crate::facade::AgentSnapshot =
        serde_json::from_str(&json).expect("deserialize snapshot");
    assert_eq!(restored, snapshot);
}

#[tokio::test]
async fn restore_external_mark_interrupted_marks_the_delegate_interrupted() {
    use crate::facade::ExternalDelegateStatus;

    let mut agent = AgentBuilder::default()
        .client(external_supervisor_client())
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("coder", completed_coder())
        .build()
        .expect("agent builds");
    agent.run_full("Please refactor.").await.unwrap();
    let snapshot = agent.snapshot().expect("snapshot at a committed point");

    // The default restore policy marks the delegate interrupted without
    // touching any external runtime.
    let restored = Agent::restore()
        .snapshot(snapshot)
        .client(external_supervisor_client())
        .build()
        .expect("restore rebuilds the agent");

    // The restored agent re-advertises the external delegate, and a
    // re-snapshot reports its reconciled interrupted status with the recorded
    // session preserved.
    assert_eq!(restored.external_agents().len(), 1);
    assert_eq!(restored.external_agents()[0].name(), "coder");
    let resnapshot = restored.snapshot().expect("re-snapshot the restored agent");
    assert_eq!(resnapshot.external_delegates.len(), 1);
    assert_eq!(
        resnapshot.external_delegates[0].status,
        ExternalDelegateStatus::Interrupted
    );
    assert_eq!(
        resnapshot.external_delegates[0]
            .session
            .as_ref()
            .and_then(|session| session.session_id.as_deref()),
        Some("sess-1"),
        "MarkInterrupted preserves the recorded session facts"
    );
}

#[tokio::test]
async fn restore_external_attach_or_fail_errors_when_unattachable() {
    use crate::facade::RestoreExternal;

    let mut agent = AgentBuilder::default()
        .client(external_supervisor_client())
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("coder", completed_coder())
        .build()
        .expect("agent builds");
    agent.run_full("Please refactor.").await.unwrap();
    let snapshot = agent.snapshot().expect("snapshot at a committed point");

    // AttachOrFail with no re-registered runtime (hence no session handler to
    // attach with) is an explicit, non-silent failure.
    let error = Agent::restore()
        .snapshot(snapshot)
        .client(external_supervisor_client())
        .restore_external(RestoreExternal::AttachOrFail)
        .build()
        .expect_err("attach_or_fail without a re-registered runtime fails");
    assert!(
        matches!(error, crate::facade::FacadeError::InvalidState(_)),
        "an unattachable AttachOrFail restore fails explicitly, got {error:?}"
    );
}
