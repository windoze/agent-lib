//! Snapshot / restore tests for the [`Agent`] facade, split out of `tests.rs`.

use super::*;

#[tokio::test]
async fn snapshot_then_restore_continues_history() {
    let client = ScriptedClient::new(vec![text_response("First."), text_response("Second.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );

    let first = agent.run("one").await.unwrap();
    assert_eq!(first.text(), "First.");
    assert_eq!(agent.conversation().turns().len(), 1);

    let snapshot = agent.snapshot().expect("snapshot at a committed point");

    // Restore against a fresh client and re-injected tool.
    let restore_client = ScriptedClient::new(vec![text_response("Second.")]);
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restore_client)
        .tool(counting_weather_tool(Arc::new(AtomicUsize::new(0))))
        .approval(Approval::auto_allow())
        .build()
        .expect("restore agent");

    assert_eq!(
        restored.conversation().turns().len(),
        1,
        "restore preserves the first committed turn"
    );

    let second = restored.run("two").await.unwrap();
    assert_eq!(second.text(), "Second.");
    assert_eq!(
        restored.conversation().turns().len(),
        2,
        "a run after restore appends to the restored history"
    );
}

#[tokio::test]
async fn restore_builder_provider_extras_reach_restored_request() {
    let base_client = ScriptedClient::new(vec![text_response("First.")]);
    let mut agent = AgentBuilder::default()
        .client(base_client)
        .model("claude-test")
        .build()
        .expect("build agent");
    agent.run("one").await.expect("first run");
    let snapshot = agent.snapshot().expect("snapshot at a committed point");

    let restore_client = ScriptedClient::new(vec![text_response("Second.")]);
    let extras = provider_extras(ProviderId::Anthropic);
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restore_client.clone())
        .provider_extras(extras.clone())
        .build()
        .expect("restore agent");

    restored.run("two").await.expect("restored run");

    assert_eq!(restore_client.requests()[0].provider_extras, Some(extras));
}

#[test]
fn restore_builder_rejects_provider_extras_for_different_provider() {
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("x")]))
        .model("gpt-test")
        .build()
        .expect("build agent");
    let snapshot = agent.snapshot().expect("snapshot");

    let error = Agent::restore()
        .snapshot(snapshot)
        .provider(provider_config(ProviderId::OpenAiResp))
        .provider_extras(provider_extras(ProviderId::Anthropic))
        .build()
        .expect_err("provider mismatch is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("provider_extras"));
}

#[tokio::test]
async fn snapshot_round_trips_through_json() {
    let client = ScriptedClient::new(vec![text_response("Hello.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    agent.run("hi").await.unwrap();

    let snapshot = agent.snapshot().expect("snapshot");
    let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
    let restored: AgentSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");

    assert_eq!(restored, snapshot, "snapshot survives a JSON round trip");
    assert!(
        snapshot.delegates.is_empty()
            && snapshot.pending_delegations.is_empty()
            && snapshot.artifacts.is_empty(),
        "reserved slices are empty on the base agent path"
    );
    assert!(
        snapshot.mailbox.is_none() && snapshot.blackboard.is_none() && snapshot.plan.is_none(),
        "reserved options are absent on the base agent path"
    );
}

#[tokio::test]
async fn into_parts_exposes_usable_state() {
    let client = ScriptedClient::new(vec![text_response("Hello.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    agent.run("hi").await.unwrap();

    let parts = agent.into_parts();
    assert_eq!(
        parts.state.conversation().turns().len(),
        1,
        "the handed-out state owns the committed history"
    );
    assert_eq!(parts.tools.len(), 1);
    assert_eq!(parts.tools[0].name(), "get_weather");
}

#[test]
fn restore_requires_a_snapshot() {
    let error = Agent::restore()
        .client(ScriptedClient::new(vec![text_response("x")]))
        .build()
        .unwrap_err();
    assert!(
        matches!(error, FacadeError::Config(_)),
        "restore without a snapshot is a config error, got {error:?}"
    );
}

#[tokio::test]
async fn restore_requires_a_client_or_provider() {
    let client = ScriptedClient::new(vec![text_response("Hi.")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    agent.run("hi").await.unwrap();
    let snapshot = agent.snapshot().expect("snapshot");

    let error = Agent::restore().snapshot(snapshot).build().unwrap_err();
    assert!(
        matches!(error, FacadeError::Config(_)),
        "restore without a client or provider is a config error, got {error:?}"
    );
}
