use super::*;

// ---- dispatcher-routed delegation (`docs/facade-api.md` §13.3) ----

#[test]
fn dispatcher_builder_sets_config_and_advertises_no_tools() {
    let routing = Delegation::dispatcher()
        .primary("cheap-coder")
        .verify_with("verifier")
        .escalate_to("strong-coder")
        .max_attempts(3);
    assert!(routing.is_dispatcher_routed());

    let config = routing
        .dispatcher_config()
        .expect("dispatcher config present");
    assert_eq!(config.primary(), "cheap-coder");
    assert_eq!(config.verifier(), Some("verifier"));
    assert_eq!(config.escalate_to(), Some("strong-coder"));
    assert_eq!(config.max_attempts(), 3);

    // No delegate is ever advertised to the supervising model (§13.3).
    assert!(
        routing.declarations(&[], &[]).is_empty(),
        "dispatcher-routed delegation exposes no delegate to the model"
    );
    assert!(routing.external_tool_names(&[]).is_empty());
}

#[test]
fn dispatcher_max_attempts_is_clamped_to_at_least_one() {
    let routing = Delegation::dispatcher().primary("cheap").max_attempts(0);
    assert_eq!(
        routing.dispatcher_config().expect("config").max_attempts(),
        1,
        "max_attempts clamps up to 1 so the primary always runs once"
    );
}

#[test]
fn dispatcher_builder_switches_a_non_dispatcher_delegation() {
    // Chaining a dispatcher setter onto the default model-routed delegation
    // flips it into dispatcher mode, starting from a fresh config.
    let routing = Delegation::model_routed().primary("cheap");
    assert!(routing.is_dispatcher_routed());
    assert_eq!(
        routing.dispatcher_config().expect("config").primary(),
        "cheap"
    );
}

#[test]
fn unknown_dispatcher_delegate_is_detected_for_build_validation() {
    let routing = Delegation::dispatcher()
        .primary("cheap")
        .escalate_to("ghost");
    // `cheap` is unregistered too, but the primary is reported first.
    assert_eq!(
        routing
            .first_unknown_dispatcher_delegate(&[], &[])
            .as_deref(),
        Some("cheap")
    );
}

#[tokio::test]
async fn dispatcher_empty_primary_is_rejected_at_build() {
    let error = AgentBuilder::default()
        .client(RoutingClient::new(vec![route(
            "SUPERVISOR",
            vec![text_response("x")],
        )]))
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .subagent("strong", dispatch_worker("You are the STRONG worker."))
        .delegation(Delegation::dispatcher().escalate_to("strong"))
        .build()
        .expect_err("an empty primary is rejected");
    assert!(
        matches!(error, crate::facade::FacadeError::Config(_)),
        "a dispatcher with no primary fails to build, got {error:?}"
    );
}

#[tokio::test]
async fn dispatcher_unknown_delegate_is_rejected_at_build() {
    let error = AgentBuilder::default()
        .client(RoutingClient::new(vec![route(
            "SUPERVISOR",
            vec![text_response("x")],
        )]))
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .delegation(Delegation::dispatcher().primary("ghost"))
        .build()
        .expect_err("an unregistered delegate is rejected");
    assert!(
        matches!(error, crate::facade::FacadeError::Config(_)),
        "a dispatcher naming an unregistered delegate fails to build, got {error:?}"
    );
}

#[tokio::test]
async fn dispatcher_escalates_when_primary_fails_then_strong_succeeds() {
    use crate::facade::ManagedExternalAgent;

    // Only the strong worker takes an LLM step; the primary is a managed
    // external agent with no session handler, so its delegation fails outright
    // and the loop escalates without ever consulting a verifier.
    let client = RoutingClient::new(vec![route(
        "STRONG",
        vec![text_response("strong solution complete")],
    )]);

    let cheap = ManagedExternalAgent::claude_code()
        .build()
        .expect("managed external agent builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("cheap", cheap)
        .subagent("strong", dispatch_worker("You are the STRONG worker."))
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .escalate_to("strong")
                .max_attempts(2),
        )
        .build()
        .expect("agent builds");

    let output = agent
        .run_full("Please implement the feature.")
        .await
        .unwrap();

    // The final reply is the escalation target's summary, not the failed
    // primary's error.
    assert_eq!(output.reply.text(), "strong solution complete");

    // The escalation path (primary → strong) is captured as an event.
    assert_eq!(
        escalation_edge(&output),
        Some(("cheap".to_owned(), "strong".to_owned())),
        "an Escalated event records the primary → strong hand-off"
    );

    // Both attempts are recorded: the failed primary and the successful strong
    // worker, in order.
    assert_eq!(output.delegations.len(), 2);
    assert_eq!(output.delegations[0].delegate, "cheap");
    assert_eq!(output.delegations[0].status, DelegationStatus::Failed);
    assert_eq!(output.delegations[1].delegate, "strong");
    assert_eq!(output.delegations[1].status, DelegationStatus::Completed);

    // The failed primary emits DelegationFailed; the strong worker Finished.
    assert!(
        output
            .events
            .iter()
            .any(|event| matches!(event, RunEvent::DelegationFailed(_))),
        "the failed primary emits DelegationFailed"
    );

    // The supervisor took no LLM step.
    assert_eq!(output.usage.supervisor.input, 0);
    assert_eq!(output.usage.supervisor.output, 0);
    assert!(
        agent.conversation().turns().is_empty(),
        "a dispatcher-routed turn does not commit to the supervisor conversation"
    );
}

#[tokio::test]
async fn dispatcher_verifier_rejection_escalates_to_strong() {
    // The verifier rejects the primary's output (call 1) then approves the
    // strong worker's output (call 2), driving exactly one escalation.
    let client = RoutingClient::new(vec![
        route("CHEAP", vec![text_response("cheap attempt at the task")]),
        route(
            "VERIFIER",
            vec![
                text_response("ESCALATE: this is insufficient"),
                text_response("approved, this looks good"),
            ],
        ),
        route("STRONG", vec![text_response("strong result delivered")]),
    ]);

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("cheap", dispatch_worker("You are the CHEAP worker."))
        .subagent("verifier", dispatch_worker("You are the VERIFIER."))
        .subagent("strong", dispatch_worker("You are the STRONG worker."))
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .verify_with("verifier")
                .escalate_to("strong")
                .max_attempts(2),
        )
        .build()
        .expect("agent builds");

    let output = agent.run_full("Please solve the problem.").await.unwrap();

    // The final reply is the strong worker's summary, never the verifier's.
    assert_eq!(output.reply.text(), "strong result delivered");
    assert_eq!(
        escalation_edge(&output),
        Some(("cheap".to_owned(), "strong".to_owned()))
    );

    // Four delegations run: cheap, verifier (reject), strong, verifier (pass).
    let names: Vec<&str> = output
        .delegations
        .iter()
        .map(|trace| trace.delegate.as_str())
        .collect();
    assert_eq!(names, ["cheap", "verifier", "strong", "verifier"]);
    assert!(
        output
            .delegations
            .iter()
            .all(|trace| trace.status == DelegationStatus::Completed),
        "every worker and verifier delegation completed cleanly"
    );

    // Child usage is attributed to the subagent slice, not the supervisor.
    assert_eq!(output.usage.supervisor.input, 0);
    assert!(output.usage.subagents.input > 0);
}

#[tokio::test]
async fn dispatcher_verifier_pass_does_not_escalate() {
    // The verifier approves the primary on the first pass, so the strong
    // worker — though configured — never runs.
    let client = RoutingClient::new(vec![
        route("CHEAP", vec![text_response("cheap solved it cleanly")]),
        route("VERIFIER", vec![text_response("approved, looks good")]),
        route("STRONG", vec![text_response("UNUSED strong output")]),
    ]);

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("cheap", dispatch_worker("You are the CHEAP worker."))
        .subagent("verifier", dispatch_worker("You are the VERIFIER."))
        .subagent("strong", dispatch_worker("You are the STRONG worker."))
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .verify_with("verifier")
                .escalate_to("strong")
                .max_attempts(2),
        )
        .build()
        .expect("agent builds");

    let output = agent.run_full("Please solve the problem.").await.unwrap();

    // The primary's summary is the whole reply; no escalation happened.
    assert_eq!(output.reply.text(), "cheap solved it cleanly");
    assert!(
        escalation_edge(&output).is_none(),
        "a passing verifier produces no Escalated event"
    );

    // Only the primary and its verifier ran; the strong worker did not.
    let names: Vec<&str> = output
        .delegations
        .iter()
        .map(|trace| trace.delegate.as_str())
        .collect();
    assert_eq!(names, ["cheap", "verifier"]);
}

#[tokio::test]
async fn dispatcher_respects_max_attempts_of_one() {
    use crate::facade::ManagedExternalAgent;

    // A single attempt runs the primary once and never escalates, even though
    // the primary fails and an escalation target is configured.
    let client = RoutingClient::new(vec![route(
        "STRONG",
        vec![text_response("UNUSED strong output")],
    )]);

    let cheap = ManagedExternalAgent::claude_code()
        .build()
        .expect("managed external agent builds");

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("cheap", cheap)
        .subagent("strong", dispatch_worker("You are the STRONG worker."))
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .escalate_to("strong")
                .max_attempts(1),
        )
        .build()
        .expect("agent builds");

    let output = agent
        .run_full("Please implement the feature.")
        .await
        .unwrap();

    assert!(
        escalation_edge(&output).is_none(),
        "max_attempts(1) never escalates"
    );
    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].delegate, "cheap");
    assert_eq!(output.delegations[0].status, DelegationStatus::Failed);
}

#[tokio::test]
async fn dispatcher_stream_yields_escalated_then_done() {
    let client = RoutingClient::new(vec![
        route("CHEAP", vec![text_response("cheap attempt at the task")]),
        route(
            "VERIFIER",
            vec![
                text_response("ESCALATE: this is insufficient"),
                text_response("approved, this looks good"),
            ],
        ),
        route("STRONG", vec![text_response("strong result delivered")]),
    ]);

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("cheap", dispatch_worker("You are the CHEAP worker."))
        .subagent("verifier", dispatch_worker("You are the VERIFIER."))
        .subagent("strong", dispatch_worker("You are the STRONG worker."))
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .verify_with("verifier")
                .escalate_to("strong")
                .max_attempts(2),
        )
        .build()
        .expect("agent builds");

    let mut stream = agent
        .stream("Please solve the problem.")
        .await
        .expect("stream starts");
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("stream item is ok"));
    }

    assert!(
        events
            .iter()
            .any(|event| matches!(event, RunEvent::Escalated(_))),
        "the dispatcher stream surfaces an Escalated event"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RunEvent::DelegationStarted(_))),
        "the dispatcher stream surfaces a DelegationStarted event"
    );
    let done = events
        .iter()
        .find_map(|event| match event {
            RunEvent::Done(output) => Some(output),
            _ => None,
        })
        .expect("the stream ends with a Done event");
    assert_eq!(done.reply.text(), "strong result delivered");
    assert_eq!(
        escalation_edge(done),
        Some(("cheap".to_owned(), "strong".to_owned()))
    );
}

// -----------------------------------------------------------------------
// AI-decision injection seams (milestone M7-5, docs/facade-api.md §19)
// -----------------------------------------------------------------------

#[tokio::test]
async fn dispatcher_injected_verifier_forces_escalation() {
    use crate::agent::{EscalationTrigger, ScriptedVerifier, Verifier};

    // No verifier delegate is configured, so by default a clean primary run
    // is accepted and never escalates. Injecting a Verifier that always
    // rejects (the AI-verification seam, §19) overrides that default verdict
    // and forces exactly one escalation to the strong worker.
    let client = RoutingClient::new(vec![
        route("CHEAP", vec![text_response("cheap attempt at the task")]),
        route("STRONG", vec![text_response("strong result delivered")]),
    ]);

    let verifier: Arc<dyn Verifier + Send + Sync> = Arc::new(ScriptedVerifier::rejecting(
        EscalationTrigger::ReviewRejected,
    ));

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .subagent("cheap", dispatch_worker("You are the CHEAP worker."))
        .subagent("strong", dispatch_worker("You are the STRONG worker."))
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .escalate_to("strong")
                .max_attempts(2)
                .dispatcher_verifier(verifier),
        )
        .build()
        .expect("agent builds");

    let output = agent.run_full("Please solve the problem.").await.unwrap();

    // The injected verifier rejected the clean primary, so the loop escalated
    // to the strong worker and returned its output.
    assert_eq!(output.reply.text(), "strong result delivered");
    assert_eq!(
        escalation_edge(&output),
        Some(("cheap".to_owned(), "strong".to_owned()))
    );
    let names: Vec<&str> = output
        .delegations
        .iter()
        .map(|trace| trace.delegate.as_str())
        .collect();
    assert_eq!(names, ["cheap", "strong"]);
}

#[tokio::test]
async fn dispatcher_injected_evaluator_declines_escalation() {
    use crate::agent::{ScriptedTaskEvaluator, TaskEvaluator};
    use crate::facade::ManagedExternalAgent;

    // The primary is a managed external agent with no session handler, so it
    // fails its delegation — which by default escalates to the configured
    // strong worker (cf. `dispatcher_escalates_when_primary_fails...`).
    // Injecting a TaskEvaluator that declines (returns `None`, the AI-routing
    // seam, §19) suppresses that escalation entirely.
    let client = RoutingClient::new(vec![route(
        "STRONG",
        vec![text_response("UNUSED strong output")],
    )]);

    let cheap = ManagedExternalAgent::claude_code()
        .build()
        .expect("managed external agent builds");

    let evaluator: Arc<dyn TaskEvaluator + Send + Sync> =
        Arc::new(ScriptedTaskEvaluator::new(|_, _| None));

    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the SUPERVISOR.")
        .approval(Approval::auto_allow())
        .external_agent("cheap", cheap)
        .subagent("strong", dispatch_worker("You are the STRONG worker."))
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .escalate_to("strong")
                .max_attempts(2)
                .dispatcher_evaluator(evaluator),
        )
        .build()
        .expect("agent builds");

    let output = agent
        .run_full("Please implement the feature.")
        .await
        .unwrap();

    assert!(
        escalation_edge(&output).is_none(),
        "the injected evaluator declined, so no escalation occurred"
    );
    assert_eq!(output.delegations.len(), 1);
    assert_eq!(output.delegations[0].delegate, "cheap");
    assert_eq!(output.delegations[0].status, DelegationStatus::Failed);
}

#[test]
fn dispatcher_injection_hooks_stored_and_serde_drops_them() {
    use crate::agent::{
        EscalationTrigger, ScriptedTaskEvaluator, ScriptedVerifier, TaskEvaluator, Verifier,
        WorkerProfileRef,
    };

    let evaluator: Arc<dyn TaskEvaluator + Send + Sync> = Arc::new(ScriptedTaskEvaluator::always(
        WorkerProfileRef::new("strong"),
    ));
    let verifier: Arc<dyn Verifier + Send + Sync> = Arc::new(ScriptedVerifier::rejecting(
        EscalationTrigger::ReviewRejected,
    ));

    // The builder switches to dispatcher mode and stores both runtime hooks.
    let with_hooks = Delegation::dispatcher()
        .primary("cheap")
        .escalate_to("strong")
        .dispatcher_evaluator(evaluator)
        .dispatcher_verifier(verifier);
    assert!(with_hooks.is_dispatcher_routed());
    assert!(with_hooks.dispatcher_evaluator_hook().is_some());
    assert!(with_hooks.dispatcher_verifier_hook().is_some());

    // The same config without hooks: neither hook is present, but the two are
    // config-equal because the injected handlers are runtime-only identity.
    let without_hooks = Delegation::dispatcher()
        .primary("cheap")
        .escalate_to("strong");
    assert!(without_hooks.dispatcher_evaluator_hook().is_none());
    assert!(without_hooks.dispatcher_verifier_hook().is_none());
    assert_eq!(
        with_hooks, without_hooks,
        "injected runtime hooks do not change config identity"
    );

    // Snapshotting (a serde round-trip) drops the runtime hooks (§15.2), so a
    // restored delegation falls back to the built-in defaults.
    let json = serde_json::to_string(&with_hooks).expect("serializes");
    let restored: Delegation = serde_json::from_str(&json).expect("deserializes");
    assert!(restored.dispatcher_evaluator_hook().is_none());
    assert!(restored.dispatcher_verifier_hook().is_none());
}
