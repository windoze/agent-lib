//! [`AgentBuilder`] validation tests for the [`Agent`] facade, split out of
//! `tests.rs`.

use super::*;

#[tokio::test]
async fn builder_provider_extras_reach_supervisor_request() {
    let client = ScriptedClient::new(vec![text_response("done")]);
    let extras = provider_extras(ProviderId::Anthropic);
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("claude-test")
        .provider_extras(extras.clone())
        .build()
        .expect("build agent");

    agent.run("hello").await.expect("run succeeds");

    assert_eq!(client.requests()[0].provider_extras, Some(extras));
}

#[tokio::test]
async fn builder_budget_limits_supervisor_run_and_leaves_agent_usable() {
    let client = ScriptedClient::new(vec![
        text_response_with_usage("too expensive", 11, 7),
        text_response_with_usage("recovered", 0, 0),
    ]);
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .budget(BudgetLimits::new(None, Some(10), None, None))
        .build()
        .expect("build agent");

    let error = agent.run("exceed the token budget").await.unwrap_err();
    assert!(
        matches!(error, FacadeError::BudgetExhausted),
        "token overrun maps to a structured facade budget error, got {error:?}"
    );
    agent
        .snapshot()
        .expect("budget failure leaves state snapshot-able");

    let reply = agent
        .run("second run gets a fresh budget ledger")
        .await
        .expect("subsequent low-usage run succeeds");
    assert_eq!(reply.text(), "recovered");
}

#[test]
fn builder_rejects_provider_extras_for_different_provider() {
    let error = AgentBuilder::default()
        .provider(provider_config(ProviderId::OpenAiResp))
        .model("gpt-test")
        .provider_extras(provider_extras(ProviderId::Anthropic))
        .build()
        .expect_err("provider mismatch is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("provider_extras"));
}

#[test]
fn builder_rejects_blank_model() {
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("done")]))
        .model("\t  ")
        .build()
        .expect_err("blank model is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("model"));
}

#[test]
fn builder_rejects_non_finite_temperature() {
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("done")]))
        .model("test-model")
        .temperature(f32::INFINITY)
        .build()
        .expect_err("non-finite temperature is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("temperature"));
}

#[test]
fn builder_rejects_blank_delegation_tool_name() {
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("done")]))
        .model("test-model")
        .delegation(Delegation::single_tool(" "))
        .build()
        .expect_err("blank delegation tool name is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("tool name"));
}

#[test]
fn builder_rejects_empty_rules_delegation() {
    // A rules-routed delegation with no rules can never route and exposes no
    // delegate tools, so registered subagents would be silently unreachable.
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("done")]))
        .model("test-model")
        .delegation(Delegation::rules())
        .build()
        .expect_err("rules delegation with no rules is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("at least one rule"), "{message}");
}

#[test]
fn builder_rejects_invalid_rules_routing_entries() {
    for (delegation, expected) in [
        (
            Delegation::rules().when_task_contains(Vec::<String>::new(), "coder"),
            "keywords",
        ),
        (
            Delegation::rules().when_task_contains(["fix", "  "], "coder"),
            "keyword",
        ),
        (
            Delegation::rules().when_task_contains(["fix"], " "),
            "delegate",
        ),
    ] {
        let error = AgentBuilder::default()
            .client(ScriptedClient::new(vec![text_response("done")]))
            .model("test-model")
            .delegation(delegation)
            .build()
            .expect_err("invalid rules entry is rejected");

        let FacadeError::Config(message) = error else {
            panic!("expected config error")
        };
        assert!(message.contains(expected), "{message}");
    }
}
#[test]
fn build_rejects_missing_model() {
    let executions = Arc::new(AtomicUsize::new(0));
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .tool(counting_weather_tool(executions))
        .build()
        .unwrap_err();
    assert!(matches!(error, FacadeError::Config(_)));
}

#[test]
fn build_rejects_duplicate_tool_names() {
    let a = counting_weather_tool(Arc::new(AtomicUsize::new(0)));
    let b = counting_weather_tool(Arc::new(AtomicUsize::new(0)));
    let error = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .tool(a)
        .tool(b)
        .build()
        .unwrap_err();
    assert!(
        matches!(error, FacadeError::DuplicateTool { name } if name == "get_weather"),
        "duplicate tool names are rejected at build",
    );
}
