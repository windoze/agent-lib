//! Delegate registration, `into_parts`, and collaboration-topology tests for
//! the [`Agent`] facade, split out of `tests.rs`.

use super::*;

#[test]
fn registered_subagents_appear_in_the_delegate_table() {
    let reviewer = Agent::worker()
        .model("reviewer-model")
        .system("You review code.")
        .build()
        .expect("worker builds");
    let researcher = Agent::worker()
        .system("You research.")
        .build()
        .expect("worker builds");

    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .build()
        .expect("build agent");

    let names: Vec<&str> = agent.subagents().iter().map(|s| s.name()).collect();
    assert_eq!(names, ["reviewer", "researcher"]);
    // The explicit-model worker keeps its model; the default worker inherits.
    assert!(!agent.subagents()[0].inherits_model());
    assert_eq!(
        agent.subagents()[0].spec().model().model(),
        "reviewer-model"
    );
    assert!(agent.subagents()[1].inherits_model());
}

#[tokio::test]
async fn rules_routed_delegate_output_has_no_supervisor_reply_usage() {
    let client = ScriptedClient::new(vec![text_response_with_usage("delegated", 3, 4)]);
    let reviewer = Agent::worker().system("review").build().expect("worker");
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .subagent("reviewer", reviewer)
        .delegation(Delegation::rules().when_task_contains(["route"], "reviewer"))
        .build()
        .expect("build agent");

    let output = agent.run_full("please route this").await.expect("run");

    assert_eq!(output.reply.text(), "delegated");
    assert_eq!(output.reply.usage(), None);
    assert_eq!(output.usage.supervisor, Usage::default());
    assert_eq!(output.usage.subagents.input, 3);
    assert_eq!(output.usage.subagents.output, 4);
}

#[test]
fn into_parts_carries_registered_delegates() {
    let reviewer = Agent::worker()
        .system("You review code.")
        .build()
        .expect("worker builds");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("reviewer", reviewer)
        .build()
        .expect("build agent");

    let parts = agent.into_parts();
    assert_eq!(parts.delegates.len(), 1);
    assert_eq!(parts.delegates[0].name(), "reviewer");
}

#[test]
fn into_parts_carries_the_injected_interaction_handler() {
    // An injected async interaction handler is a live runtime handle distinct
    // from the approval bridge; `into_parts` must hand it back rather than drop
    // it (§19).
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .interaction_handler(Arc::new(FixedInteractionHandler {
            decision: ApprovalDecision::Approve,
        }))
        .build()
        .expect("build agent");

    let parts = agent.into_parts();
    assert!(
        parts.interaction_handler.is_some(),
        "the injected interaction handler survives into_parts"
    );
    // A base agent with no delegation drives no external runtime, so its
    // retained external session facts are empty.
    assert!(parts.retained_external_sessions.is_empty());
}

#[test]
fn into_parts_without_a_handler_leaves_the_slot_empty() {
    // Without an injected handler the agent falls back to the approval bridge,
    // so the dedicated interaction-handler slot is `None` (the fallback is still
    // reachable through `parts.approval`).
    let agent = agent_with(
        ScriptedClient::new(vec![text_response("hi")]),
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );

    let parts = agent.into_parts();
    assert!(
        parts.interaction_handler.is_none(),
        "no injected handler means the slot is empty"
    );
}

#[test]
fn into_parts_carries_live_collaboration_state() {
    // §14: a dispatcher / verifier loop provisions plan + blackboard + mailbox.
    // `into_parts` must surface both the resolved config and the live, shared
    // primitives so a caller can keep messaging through them.
    let cheap = Agent::worker().system("cheap").build().expect("worker");
    let checker = Agent::worker().system("checker").build().expect("worker");
    let strong = Agent::worker().system("strong").build().expect("worker");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("cheap", cheap)
        .subagent("checker", checker)
        .subagent("strong", strong)
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .verify_with("checker")
                .escalate_to("strong"),
        )
        .build()
        .expect("build agent");

    let parts = agent.into_parts();
    assert!(
        parts.collaboration.plan_enabled()
            && parts.collaboration.blackboard_enabled()
            && parts.collaboration.mailbox_enabled(),
        "the resolved collaboration config is handed out verbatim"
    );

    let mailbox = parts.mailbox.expect("mailbox handed out");
    mailbox.send("cheap", "checker", "verify claim 3");
    let inbox = mailbox.inbox("checker");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].text, "verify claim 3");

    assert!(
        parts.blackboard.is_some() && parts.plan.is_some(),
        "the live blackboard and plan handles are handed out too"
    );
}

#[test]
fn into_parts_carries_registered_external_delegates() {
    // A managed external delegate is registered as a data-first recipe; §14 also
    // enables the artifact store for it. `into_parts` must keep both the
    // delegate and its resolved collaboration flags.
    let coder = crate::facade::external::ManagedExternalAgent::claude_code()
        .build()
        .expect("external agent builds");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .external_agent("coder", coder)
        .build()
        .expect("build agent");

    let parts = agent.into_parts();
    let names: Vec<&str> = parts.external_agents.iter().map(|d| d.name()).collect();
    assert_eq!(names, ["coder"], "the external delegate is not dropped");
    assert!(
        parts.collaboration.artifacts_enabled(),
        "a managed external delegate enables the artifact store (§14)"
    );
    // No delegation has driven the runtime yet, so no session facts are retained.
    assert!(parts.retained_external_sessions.is_empty());
}

#[test]
fn base_agent_enables_no_collaboration() {
    // No delegate → §14 provisions no collaboration substrate.
    let agent = agent_with(
        ScriptedClient::new(vec![text_response("hi")]),
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    assert!(!agent.collaboration().any());
    assert!(agent.mailbox().is_none());
    assert!(agent.blackboard().is_none());
    assert!(agent.plan().is_none());
}

#[test]
fn two_subagents_auto_enable_a_shared_mailbox() {
    // §14: multiple delegates auto-enable a mailbox (only) — the shared inbox two
    // delegates can message through.
    let reviewer = Agent::worker()
        .system("You review code.")
        .build()
        .expect("worker builds");
    let researcher = Agent::worker()
        .system("You research topics.")
        .build()
        .expect("worker builds");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .build()
        .expect("build agent");

    assert!(agent.collaboration().mailbox_enabled());
    assert!(!agent.collaboration().plan_enabled());
    assert!(!agent.collaboration().blackboard_enabled());

    let mailbox = agent.mailbox().expect("mailbox provisioned");
    mailbox.send("reviewer", "researcher", "need sources for claim 3");
    let inbox = mailbox.inbox("researcher");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "reviewer");
    assert_eq!(inbox[0].text, "need sources for claim 3");
    assert!(agent.plan().is_none() && agent.blackboard().is_none());
}

#[test]
fn dispatcher_topology_enables_plan_blackboard_and_mailbox() {
    // §14: a dispatcher / verifier loop enables plan + blackboard + mailbox.
    let cheap = Agent::worker().system("cheap").build().expect("worker");
    let checker = Agent::worker().system("checker").build().expect("worker");
    let strong = Agent::worker().system("strong").build().expect("worker");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("cheap", cheap)
        .subagent("checker", checker)
        .subagent("strong", strong)
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .verify_with("checker")
                .escalate_to("strong"),
        )
        .build()
        .expect("build agent");

    let collab = agent.collaboration();
    assert!(collab.plan_enabled() && collab.blackboard_enabled() && collab.mailbox_enabled());
    assert!(agent.plan().is_some());
    assert!(agent.blackboard().is_some());
    assert!(agent.mailbox().is_some());
}

#[test]
fn explicit_collaboration_overrides_topology() {
    // An explicit `Collaboration` replaces the derived default in full: a
    // multi-delegate topology would derive a mailbox, but the explicit plan-only
    // config suppresses it.
    let reviewer = Agent::worker().system("r").build().expect("worker");
    let researcher = Agent::worker().system("s").build().expect("worker");
    let agent = AgentBuilder::default()
        .client(ScriptedClient::new(vec![text_response("hi")]))
        .model("test-model")
        .subagent("reviewer", reviewer)
        .subagent("researcher", researcher)
        .collaboration(Collaboration::new().plan())
        .build()
        .expect("build agent");

    assert!(agent.collaboration().plan_enabled());
    assert!(!agent.collaboration().mailbox_enabled());
    assert!(agent.plan().is_some());
    assert!(agent.mailbox().is_none());
}
