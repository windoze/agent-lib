//! Reconfigure x delegation/snapshot tests for the [`Agent`] facade, split out
//! of `tests.rs`: delegation-tool merges on tool-set reconfigs and
//! snapshot/restore of applied and queued reconfigs.

use super::*;

fn weather_tool_decl() -> crate::model::tool::Tool {
    counting_weather_tool(Arc::new(AtomicUsize::new(0))).declaration()
}

#[tokio::test]
async fn snapshot_restore_preserves_applied_model_and_tool_set_reconfig() {
    let client = ScriptedClient::new(vec![text_response("reconfigured")]);
    let calendar_calls = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with_tools(
        client,
        vec![
            counting_weather_tool(Arc::new(AtomicUsize::new(0))),
            counting_calendar_tool(calendar_calls.clone()),
        ],
        Approval::auto_allow(),
    );
    let model = reconfig_model("test-model-v2");
    let replacement = ToolSetRef::new(reconfig_tool_set_id(1), vec![calendar_tool_decl()]);
    agent
        .reconfigure(ReconfigRequest::SetModel {
            model: model.clone(),
        })
        .expect("set-model reconfig is accepted");
    agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("replace-tool-set reconfig is accepted");

    agent
        .run("apply the reconfig.")
        .await
        .expect("reconfig applies at the run's turn boundary");
    assert_eq!(agent.state().current_model(), &model);
    assert_eq!(agent.state().current_tool_set(), &replacement);

    let snapshot = agent
        .snapshot()
        .expect("snapshot captures the applied reconfig");
    let restored_client = ScriptedClient::new(vec![text_response("restored")]);
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restored_client.clone())
        .tool(counting_calendar_tool(calendar_calls.clone()))
        .build()
        .expect("restore with the reconfigured tool surface");

    assert_eq!(restored.state().current_model(), &model);
    assert_eq!(restored.state().current_tool_set(), &replacement);

    let reply = restored
        .run("continue after restore.")
        .await
        .expect("restored agent runs on the reconfigured model and tool set");
    assert_eq!(reply.text(), "restored");
    assert_eq!(calendar_calls.load(Ordering::SeqCst), 0);
    let requests = restored_client.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "test-model-v2");
    assert_eq!(requests[0].max_tokens, 321);
    assert_eq!(requests[0].temperature, Some(0.25));
    assert_eq!(tool_names(&requests[0].tools), vec!["read_calendar"]);
}

#[tokio::test]
async fn snapshot_captures_queued_unapplied_reconfigs_for_restore() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let model = reconfig_model("test-model-v2");
    agent
        .reconfigure(ReconfigRequest::SetModel {
            model: model.clone(),
        })
        .expect("set-model reconfig is accepted");
    assert_eq!(agent.state().queued_reconfigs().len(), 1);
    assert_ne!(agent.state().current_model(), &model);

    let snapshot = agent
        .snapshot()
        .expect("snapshot captures the queued reconfig");
    // Round-trip through JSON, since persistence is the snapshot's purpose.
    let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
    let snapshot: AgentSnapshot = serde_json::from_str(&json).expect("snapshot deserializes");

    let restored_client = ScriptedClient::new(vec![text_response("applied after restore")]);
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restored_client.clone())
        .tool(counting_weather_tool(Arc::new(AtomicUsize::new(0))))
        .build()
        .expect("restore replans the captured queue");

    assert_eq!(restored.state().queued_reconfigs().len(), 1);
    assert_ne!(restored.state().current_model(), &model);

    let reply = restored
        .run("apply the queued reconfig.")
        .await
        .expect("queued reconfig applies at the restored agent's next turn boundary");
    assert_eq!(reply.text(), "applied after restore");
    assert_eq!(restored.state().current_model(), &model);
    assert!(restored.state().queued_reconfigs().is_empty());
    assert_eq!(restored_client.requests()[0].model, "test-model-v2");
}

/// M3-R (F4b): a queued (never applied) tool-set reconfig survives snapshot +
/// restore and applies at the restored agent's first turn boundary — the
/// swapped registry both advertises and executes the new tool.
#[tokio::test]
async fn restore_applies_queued_tool_set_reconfig_at_first_turn_boundary() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let calendar_calls = Arc::new(AtomicUsize::new(0));
    // Both tools sit in the facade registry so the calendar-only replacement
    // passes admission; the current set starts as the weather-only subset…
    let mut agent = agent_with_tools(
        client,
        vec![
            counting_weather_tool(weather_calls.clone()),
            counting_calendar_tool(calendar_calls.clone()),
        ],
        Approval::auto_allow(),
    );
    let weather_only = ToolSetRef::new(
        agent.state().current_tool_set().id(),
        vec![weather_tool_decl()],
    );
    agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: weather_only,
        })
        .expect("weather-only reconfig is accepted");
    agent
        .run("narrow to weather.")
        .await
        .expect("the narrowing reconfig applies");
    assert_eq!(
        tool_names(agent.state().current_tool_set().tools()),
        vec!["get_weather"]
    );

    // …then queue (but never apply) a swap to the calendar-only set.
    let replacement = ToolSetRef::new(reconfig_tool_set_id(1), vec![calendar_tool_decl()]);
    agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("replace-tool-set reconfig is accepted");
    assert_eq!(agent.state().queued_reconfigs().len(), 1);
    assert_eq!(
        tool_names(agent.state().current_tool_set().tools()),
        vec!["get_weather"],
        "the queued reconfig has not applied yet"
    );

    // Snapshot BEFORE any run, round-tripping through JSON since persistence
    // is the snapshot's purpose.
    let snapshot = agent
        .snapshot()
        .expect("snapshot captures the queued reconfig");
    let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
    let snapshot: AgentSnapshot = serde_json::from_str(&json).expect("snapshot deserializes");

    // Restore with a surface covering both the current set (`get_weather`)
    // and the queued set (`read_calendar`).
    let restored_client = ScriptedClient::new(vec![
        tool_use_response_for(
            "read_calendar",
            "call-calendar",
            json!({ "day": "Tuesday" }),
        ),
        text_response("restored calendar checked"),
    ]);
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restored_client.clone())
        .tool(counting_weather_tool(weather_calls.clone()))
        .tool(counting_calendar_tool(calendar_calls.clone()))
        .build()
        .expect("restore with a covering surface");
    assert_eq!(restored.state().queued_reconfigs().len(), 1);
    assert_eq!(
        tool_names(restored.state().current_tool_set().tools()),
        vec!["get_weather"],
        "the restored agent still sits on the pre-reconfig tool set"
    );

    let output = restored
        .run_full("Check the calendar.")
        .await
        .expect("the queued reconfig applies at the first turn boundary");

    assert_eq!(output.reply.text(), "restored calendar checked");
    assert_eq!(restored.state().current_tool_set(), &replacement);
    assert!(restored.state().queued_reconfigs().is_empty());
    // The swapped registry advertised the new tool on the very first request…
    let requests = restored_client.requests();
    assert_eq!(tool_names(&requests[0].tools), vec!["read_calendar"]);
    // …and executed it; the retired weather tool never ran.
    assert_eq!(calendar_calls.load(Ordering::SeqCst), 1);
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn restore_rejects_a_surface_missing_current_tool_set_tools() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let snapshot = agent.snapshot().expect("snapshot");

    let error = Agent::restore()
        .snapshot(snapshot.clone())
        .client(ScriptedClient::new(vec![text_response("unused")]))
        .build()
        .expect_err("restore without the advertised tools fails explicitly");
    assert!(
        matches!(error, FacadeError::InvalidState(ref message) if message.contains("current tool set") && message.contains("get_weather")),
        "unexpected error: {error:?}"
    );

    // The same snapshot restores cleanly once the surface covers the set.
    Agent::restore()
        .snapshot(snapshot)
        .client(ScriptedClient::new(vec![text_response("unused")]))
        .tool(counting_weather_tool(Arc::new(AtomicUsize::new(0))))
        .build()
        .expect("restore with the full surface succeeds");
}

#[tokio::test]
async fn restore_rejects_a_surface_missing_a_queued_reconfig_tool_set() {
    let client = ScriptedClient::new(vec![text_response("shrunk")]);
    let mut agent = agent_with_tools(
        client,
        vec![
            counting_weather_tool(Arc::new(AtomicUsize::new(0))),
            counting_calendar_tool(Arc::new(AtomicUsize::new(0))),
        ],
        Approval::auto_allow(),
    );
    let shrink = ToolSetPatch::new(
        agent.state().current_tool_set().id(),
        reconfig_tool_set_id(1),
        vec!["get_weather".to_owned()],
        Vec::new(),
    )
    .expect("valid tool-set patch");
    agent
        .reconfigure(ReconfigRequest::PatchToolSet { patch: shrink })
        .expect("patch-tool-set reconfig is accepted");
    agent
        .run("apply the shrink.")
        .await
        .expect("shrink applies at the run's turn boundary");
    assert_eq!(
        tool_names(agent.state().current_tool_set().tools()),
        vec!["read_calendar"]
    );

    // Queue (but never apply) a corrective switch to the weather-only set.
    let queued = ToolSetRef::new(reconfig_tool_set_id(2), vec![weather_tool_decl()]);
    agent
        .reconfigure(ReconfigRequest::ReplaceToolSet { tool_set: queued })
        .expect("replace-tool-set reconfig is accepted");
    let snapshot = agent
        .snapshot()
        .expect("snapshot captures the queued reconfig");

    // The re-injected surface covers the current set (`read_calendar`) but not
    // the set the queued corrective reconfig would apply (`get_weather`), so
    // the restore fails instead of stranding the agent on its first run.
    let error = Agent::restore()
        .snapshot(snapshot.clone())
        .client(ScriptedClient::new(vec![text_response("unused")]))
        .tool(counting_calendar_tool(Arc::new(AtomicUsize::new(0))))
        .build()
        .expect_err("restore whose surface misses the queued tool set fails");
    assert!(
        matches!(error, FacadeError::InvalidState(ref message) if message.contains("queued reconfig") && message.contains("get_weather")),
        "unexpected error: {error:?}"
    );

    // A surface covering both the current and the queued set restores cleanly.
    Agent::restore()
        .snapshot(snapshot)
        .client(ScriptedClient::new(vec![text_response("unused")]))
        .tool(counting_calendar_tool(Arc::new(AtomicUsize::new(0))))
        .tool(counting_weather_tool(Arc::new(AtomicUsize::new(0))))
        .build()
        .expect("restore with the full surface succeeds");
}
/// Builds a supervisor with two scripted model-routed delegates, `reviewer`
/// and `researcher`, exercising the facade's delegation-declaration merge on
/// tool-set reconfigs.
fn delegating_agent(client: Arc<dyn LlmClient>) -> Agent {
    let reviewer = Agent::worker()
        .description("Strict code reviewer.")
        .system("You review code.")
        .build()
        .expect("worker builds");
    let researcher = Agent::worker()
        .description("Diligent researcher.")
        .system("You research prior art.")
        .build()
        .expect("worker builds");
    AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the supervisor.")
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .build()
        .expect("agent builds")
}

/// B1 hardening: `ReplaceToolSet` covers only the non-delegation surface. The
/// facade re-derives the `ask_<name>` declarations from the registered
/// delegates and merges them into the queued set, so a caller replacement
/// naming only plugin tools keeps every delegate advertised and routable
/// (previously the verbatim replacement silently dropped them and the model's
/// `ask_*` calls degraded to `UnknownTool`).
#[tokio::test]
async fn reconfigure_replace_tool_set_preserves_delegation_tools() {
    let client = ScriptedClient::new(vec![
        tool_use_response_for(
            "ask_reviewer",
            "del-1",
            json!({ "task": "review the diff" }),
        ),
        text_response("the diff looks good"),
        text_response("reviewer reported"),
    ]);
    let reviewer = Agent::worker()
        .description("Strict code reviewer.")
        .system("You review code.")
        .build()
        .expect("worker builds");
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("supervisor-model")
        .system("You are the supervisor.")
        .tool(counting_weather_tool(weather_calls.clone()))
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .build()
        .expect("agent builds");
    // The caller's replacement names only its own (plugin) tools — it cannot
    // mint the facade-internal `ask_reviewer` declaration.
    let replacement = ToolSetRef::new(reconfig_tool_set_id(1), vec![weather_tool_decl()]);

    agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement,
        })
        .expect("replace-tool-set reconfig is accepted");

    let output = agent
        .run_full("Review the diff.")
        .await
        .expect("run succeeds");

    assert_eq!(output.reply.text(), "reviewer reported");
    // The applied set is the caller's tools plus the re-synthesized delegation
    // declarations, in that order.
    assert_eq!(
        tool_names(agent.state().current_tool_set().tools()),
        vec!["get_weather", "ask_reviewer"]
    );
    // `ask_reviewer` stays advertised on the first post-reconfig request…
    assert_eq!(
        tool_names(&client.requests()[0].tools),
        vec!["get_weather", "ask_reviewer"]
    );
    // …and the delegation still routes and drives to completion.
    assert_eq!(output.delegations.len(), 1);
    let signature = lifecycle_signature(&output.events);
    assert_eq!(
        signature
            .iter()
            .filter(|event| event.contains("delegate=reviewer"))
            .collect::<Vec<_>>(),
        vec![
            "DelegationStarted{delegate=reviewer,status=Completed}",
            "DelegationFinished{delegate=reviewer,status=Completed}",
        ],
        "the merged delegation tool still drives: {signature:?}"
    );
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);
}

/// B1 hardening: a caller-supplied replacement set must not declare a name
/// that collides with a synthesized delegation declaration — delegation tools
/// are derived state, and admitting a caller-minted twin would shadow the
/// facade's own declaration.
#[test]
fn reconfigure_replace_tool_set_conflicting_delegation_declaration_is_rejected() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = delegating_agent(client);
    let conflicting = ToolSetRef::new(
        reconfig_tool_set_id(1),
        vec![
            weather_tool_decl(),
            crate::model::tool::Tool {
                name: "ask_reviewer".to_owned(),
                description: "caller-minted twin of the delegation tool".to_owned(),
                input_schema: json!({ "type": "object" }),
            },
        ],
    );

    let error = agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: conflicting,
        })
        .expect_err("a replacement colliding with a delegation tool is rejected");

    assert!(
        matches!(error, FacadeError::Config(ref message) if message.contains("ask_reviewer") && message.contains("synthesized")),
        "unexpected error: {error:?}"
    );
    assert!(agent.state().queued_reconfigs().is_empty());
}

/// B1 hardening: a patch cannot remove a delegation tool. Re-synthesizing over
/// the removal would silently ignore the caller's edit, so the facade rejects
/// it explicitly instead — delegation declarations are derived from the
/// registered delegates, never patch-managed.
#[test]
fn reconfigure_patch_tool_set_removing_a_delegation_tool_is_rejected() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = delegating_agent(client);
    let patch = ToolSetPatch::new(
        agent.state().current_tool_set().id(),
        reconfig_tool_set_id(2),
        vec!["ask_reviewer".to_owned()],
        Vec::new(),
    )
    .expect("valid tool-set patch");

    let error = agent
        .reconfigure(ReconfigRequest::PatchToolSet { patch })
        .expect_err("removing a delegation tool through a patch is rejected");

    assert!(
        matches!(error, FacadeError::Config(ref message) if message.contains("ask_reviewer") && message.contains("derived")),
        "unexpected error: {error:?}"
    );
    assert!(agent.state().queued_reconfigs().is_empty());
    // The rejected admission changed nothing: both delegation tools remain.
    assert_eq!(
        tool_names(agent.state().current_tool_set().tools()),
        vec!["ask_reviewer", "ask_researcher"]
    );
}

/// B1 hardening: a patch cannot shadow a delegation tool through
/// add-or-replace either — the synthesized declaration is the single authority
/// for that name.
#[test]
fn reconfigure_patch_tool_set_shadowing_a_delegation_tool_is_rejected() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = delegating_agent(client);
    let patch = ToolSetPatch::new(
        agent.state().current_tool_set().id(),
        reconfig_tool_set_id(2),
        Vec::new(),
        vec![crate::model::tool::Tool {
            name: "ask_reviewer".to_owned(),
            description: "caller-minted twin of the delegation tool".to_owned(),
            input_schema: json!({ "type": "object" }),
        }],
    )
    .expect("valid tool-set patch");

    let error = agent
        .reconfigure(ReconfigRequest::PatchToolSet { patch })
        .expect_err("shadowing a delegation tool through a patch is rejected");

    assert!(
        matches!(error, FacadeError::Config(ref message) if message.contains("ask_reviewer") && message.contains("synthesized")),
        "unexpected error: {error:?}"
    );
    assert!(agent.state().queued_reconfigs().is_empty());
}

/// B1 hardening, SingleTool mirror: the unified delegation tool is likewise
/// re-derived from the registered delegates and merged, so a replacement
/// naming only the typed tools keeps it advertised and routable.
#[tokio::test]
async fn reconfigure_replace_tool_set_preserves_the_unified_delegation_tool() {
    let client = ScriptedClient::new(vec![
        tool_use_response_for(
            "delegate",
            "del-1",
            json!({ "agent": "researcher", "task": "find prior art" }),
        ),
        text_response("found three papers"),
        text_response("researcher reported"),
    ]);
    let reviewer = Agent::worker()
        .description("Strict code reviewer.")
        .system("You review code.")
        .build()
        .expect("worker builds");
    let researcher = Agent::worker()
        .description("Diligent researcher.")
        .system("You research prior art.")
        .build()
        .expect("worker builds");
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let mut agent = AgentBuilder::default()
        .client(client.clone())
        .model("supervisor-model")
        .system("You are the supervisor.")
        .tool(counting_weather_tool(weather_calls.clone()))
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .delegation(Delegation::single_tool("delegate"))
        .build()
        .expect("agent builds");
    let replacement = ToolSetRef::new(reconfig_tool_set_id(1), vec![weather_tool_decl()]);

    agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement,
        })
        .expect("replace-tool-set reconfig is accepted");

    let output = agent
        .run_full("Delegate the research.")
        .await
        .expect("run succeeds");

    assert_eq!(output.reply.text(), "researcher reported");
    // The unified delegation tool is re-synthesized into the applied set.
    assert_eq!(
        tool_names(agent.state().current_tool_set().tools()),
        vec!["get_weather", "delegate"]
    );
    assert_eq!(
        tool_names(&client.requests()[0].tools),
        vec!["get_weather", "delegate"]
    );
    // The unified tool still routes to the named delegate and drives it.
    assert_eq!(output.delegations.len(), 1);
    let signature = lifecycle_signature(&output.events);
    assert!(
        signature
            .iter()
            .any(|event| event.starts_with("DelegationStarted{delegate=researcher")),
        "the unified delegation tool still drives: {signature:?}"
    );
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);
}

/// B1 hardening, snapshot roundtrip: a reconfig-merged tool set (caller tools
/// + re-synthesized delegation declarations) serializes and restores
/// consistently — the restored agent advertises the same surface and the
/// delegation still routes.
#[tokio::test]
async fn snapshot_restore_preserves_reconfig_merged_delegation_tools() {
    let client = ScriptedClient::new(vec![text_response("reconfigured")]);
    let reviewer = Agent::worker()
        .description("Strict code reviewer.")
        .system("You review code.")
        .build()
        .expect("worker builds");
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("supervisor-model")
        .system("You are the supervisor.")
        .tool(counting_weather_tool(weather_calls.clone()))
        .approval(Approval::auto_allow())
        .subagent("reviewer", reviewer)
        .build()
        .expect("agent builds");
    let replacement = ToolSetRef::new(reconfig_tool_set_id(1), vec![weather_tool_decl()]);
    agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement,
        })
        .expect("replace-tool-set reconfig is accepted");
    agent
        .run("apply the reconfig.")
        .await
        .expect("reconfig applies at the run's turn boundary");
    assert_eq!(
        tool_names(agent.state().current_tool_set().tools()),
        vec!["get_weather", "ask_reviewer"]
    );

    // Round-trip through JSON, since persistence is the snapshot's purpose.
    let snapshot = agent.snapshot().expect("snapshot captures the merged set");
    let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
    let snapshot: AgentSnapshot = serde_json::from_str(&json).expect("snapshot deserializes");

    let restored_client = ScriptedClient::new(vec![
        tool_use_response_for(
            "ask_reviewer",
            "del-1",
            json!({ "task": "review the diff" }),
        ),
        text_response("the diff looks good"),
        text_response("reviewer reported"),
    ]);
    let restored_reviewer = Agent::worker()
        .description("Strict code reviewer.")
        .system("You review code.")
        .build()
        .expect("worker builds");
    let mut restored = Agent::restore()
        .snapshot(snapshot)
        .client(restored_client.clone())
        .tool(counting_weather_tool(weather_calls.clone()))
        .subagent("reviewer", restored_reviewer)
        .build()
        .expect("restore with the reconfigured tool surface");

    assert_eq!(
        tool_names(restored.state().current_tool_set().tools()),
        vec!["get_weather", "ask_reviewer"],
        "the restored surface matches the reconfig-merged set"
    );

    let output = restored
        .run_full("Review the diff.")
        .await
        .expect("restored agent runs on the merged tool set");
    assert_eq!(output.reply.text(), "reviewer reported");
    assert_eq!(
        tool_names(&restored_client.requests()[0].tools),
        vec!["get_weather", "ask_reviewer"]
    );
    assert_eq!(output.delegations.len(), 1);
}
