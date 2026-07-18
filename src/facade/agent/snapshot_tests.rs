//! Round-trip tests for the collaboration slices of an [`AgentSnapshot`] (M3-2,
//! M3-3).
//!
//! These tests assert that [`Agent::snapshot`](crate::facade::Agent::snapshot)
//! captures the *live* mailbox / blackboard / plan substrate an agent has
//! provisioned, and that [`Agent::restore`](crate::facade::Agent::restore)
//! rehydrates that content — while a disabled substrate stays absent on both
//! sides so restore never resurrects an unconfigured primitive. Restore follows
//! the §15.2 conflict rule (**snapshot content wins; topology is only a provision
//! hint for older snapshots**): a captured slice restores its substrate even when
//! the restored topology alone would leave it disabled, while a legacy snapshot
//! that predates collaboration capture decodes its missing slices to absent and
//! re-derives an empty substrate from the topology. Each test drives a realistic
//! §14 delegate topology, so the restored agent re-derives the same substrate set
//! and the snapshot supplies its contents. Every test is fully offline: a
//! [`StubClient`] is only present so the builder / restore builder have a client
//! to hold; it is never driven.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::client::{Capability, ChatRequest, ClientError, LlmClient, Response};
use crate::facade::AgentSnapshot;
use crate::facade::agent::{Agent, AgentBuilder};
use crate::facade::delegate::Delegation;
use crate::stream::StreamEvent;

/// A never-driven client, so the builder / restore builder have a runtime handle
/// to hold (a snapshot cannot carry one, §15.2).
#[derive(Debug)]
struct StubClient;

#[async_trait]
impl LlmClient for StubClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
        Err(ClientError::Other(
            "stub client is not driven in snapshot tests".to_owned(),
        ))
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        Err(ClientError::Other(
            "stub client is not driven in snapshot tests".to_owned(),
        ))
    }
}

/// Restores `snapshot` against a fresh stub client, re-injecting nothing else.
fn restore(snapshot: AgentSnapshot) -> Agent {
    Agent::restore()
        .snapshot(snapshot)
        .client(Arc::new(StubClient))
        .build()
        .expect("restore agent")
}

/// A two-delegate supervisor: §14 auto-enables a shared mailbox (only).
fn mailbox_supervisor() -> Agent {
    AgentBuilder::default()
        .client(Arc::new(StubClient))
        .model("supervisor-model")
        .subagent(
            "researcher",
            Agent::worker().system("r").build().expect("w"),
        )
        .subagent("reviewer", Agent::worker().system("v").build().expect("w"))
        .build()
        .expect("build agent")
}

/// A dispatcher supervisor: §14 enables plan + blackboard + mailbox.
fn dispatcher_supervisor() -> Agent {
    AgentBuilder::default()
        .client(Arc::new(StubClient))
        .model("supervisor-model")
        .subagent("cheap", Agent::worker().system("cheap").build().expect("w"))
        .subagent(
            "checker",
            Agent::worker().system("checker").build().expect("w"),
        )
        .subagent(
            "strong",
            Agent::worker().system("strong").build().expect("w"),
        )
        .delegation(
            Delegation::dispatcher()
                .primary("cheap")
                .verify_with("checker")
                .escalate_to("strong"),
        )
        .build()
        .expect("build agent")
}

#[test]
fn capture_and_restore_preserve_live_mailbox_contents() {
    let agent = mailbox_supervisor();
    let mailbox = agent.mailbox().expect("mailbox provisioned");
    mailbox.send("reviewer", "researcher", "need sources for claim 3");
    mailbox.send("planner", "researcher", "prioritise section 2");
    mailbox.send("reviewer", "editor", "tighten the intro");

    let snapshot = agent.snapshot().expect("snapshot at a committed point");
    let captured = snapshot.mailbox.clone().expect("mailbox captured live");
    assert_eq!(
        captured.next_seq, 3,
        "capture preserves the mailbox-global sequence cursor"
    );
    assert_eq!(
        captured.inboxes.get("researcher").map(Vec::len),
        Some(2),
        "capture preserves each recipient inbox"
    );

    let restored = restore(snapshot);
    let restored_mailbox = restored.mailbox().expect("mailbox re-provisioned");
    let researcher = restored_mailbox.read_from("researcher", 0);
    assert_eq!(
        researcher
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        vec!["need sources for claim 3", "prioritise section 2"],
        "restore keeps the researcher inbox in delivery order"
    );
    let editor = restored_mailbox.read_from("editor", 0);
    assert_eq!(editor.len(), 1, "restore keeps other inboxes too");

    // A message sent after restore continues the sequence rather than reusing an
    // old one, because restore rehydrates the cursor.
    let next_seq = restored_mailbox.send("planner", "researcher", "one more");
    assert_eq!(next_seq, 3, "restore resumes the sequence cursor");
}

#[test]
fn capture_and_restore_preserve_blackboard_channels() {
    let agent = dispatcher_supervisor();
    let blackboard = agent.blackboard().expect("blackboard provisioned");
    blackboard.post("research", "researcher", "draft outline ready");
    blackboard.post("research", "researcher", "collected 4 sources");
    blackboard.post("review", "reviewer", "found a gap in section 2");

    let snapshot = agent.snapshot().expect("snapshot at a committed point");
    let captured = snapshot
        .blackboard
        .clone()
        .expect("blackboard captured live");
    assert_eq!(
        captured.channels.len(),
        2,
        "capture preserves every channel that holds messages"
    );

    let restored = restore(snapshot);
    let restored_board = restored.blackboard().expect("blackboard re-provisioned");
    assert_eq!(
        restored_board.id(),
        blackboard.id(),
        "restore keeps the board identity"
    );
    let mut channels = restored_board.channels_list();
    channels.sort();
    assert_eq!(channels, vec!["research".to_owned(), "review".to_owned()]);
    let research = restored_board.read_from("research", 0);
    assert_eq!(
        research.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        vec!["draft outline ready", "collected 4 sources"],
        "restore keeps each channel's ordered log"
    );

    // A post after restore continues the channel offset from its current length.
    let offset = restored_board.post("research", "researcher", "final pass");
    assert_eq!(offset, 2, "restore resumes channel offsets");
}

#[test]
fn capture_and_restore_preserve_plan_state() {
    let agent = dispatcher_supervisor();
    let plan = agent.plan().expect("plan provisioned");
    plan.add_task("root", Vec::<String>::new())
        .expect("add root");
    plan.add_task("child", ["root"]).expect("add child");

    let snapshot = agent.snapshot().expect("snapshot at a committed point");
    let captured = snapshot.plan.clone().expect("plan captured live");
    assert_eq!(captured.version, 2, "capture preserves the plan version");
    assert_eq!(
        captured.task_order,
        vec!["root".to_owned(), "child".to_owned()]
    );

    let restored = restore(snapshot);
    let restored_plan = restored.plan().expect("plan re-provisioned");
    assert_eq!(
        restored_plan.id(),
        plan.id(),
        "restore keeps the plan identity"
    );
    assert_eq!(
        restored_plan.version(),
        2,
        "restore keeps the plan version so a later CAS claim still matches"
    );
    assert_eq!(
        restored_plan.snapshot().task_order,
        vec!["root".to_owned(), "child".to_owned()],
        "restore keeps the stable task order"
    );
}

#[test]
fn disabled_collaboration_leaves_snapshot_and_restore_bare() {
    // A base agent (no delegate) provisions no §14 substrate.
    let agent = AgentBuilder::default()
        .client(Arc::new(StubClient))
        .model("supervisor-model")
        .build()
        .expect("build agent");
    assert!(!agent.collaboration().any());

    let snapshot = agent.snapshot().expect("snapshot at a committed point");
    assert!(
        snapshot.mailbox.is_none() && snapshot.blackboard.is_none() && snapshot.plan.is_none(),
        "a disabled substrate is captured as absent, not empty content"
    );

    let restored = restore(snapshot);
    assert!(
        !restored.collaboration().any(),
        "restore does not enable an unconfigured substrate"
    );
    assert!(restored.mailbox().is_none());
    assert!(restored.blackboard().is_none());
    assert!(restored.plan().is_none());
}

#[test]
fn snapshot_content_overrides_disabled_restore_topology() {
    // Capture a populated mailbox from a topology that enables one...
    let supervisor = mailbox_supervisor();
    let mailbox = supervisor.mailbox().expect("mailbox provisioned");
    mailbox.send("reviewer", "researcher", "need sources for claim 3");
    mailbox.send("planner", "researcher", "prioritise section 2");
    let populated = supervisor
        .snapshot()
        .expect("snapshot")
        .mailbox
        .expect("mailbox captured live");

    // ...then graft it onto a base agent whose topology derives *no* substrate,
    // producing a snapshot whose mailbox content conflicts with its topology.
    let base = AgentBuilder::default()
        .client(Arc::new(StubClient))
        .model("supervisor-model")
        .build()
        .expect("build agent");
    let mut snapshot = base.snapshot().expect("snapshot at a committed point");
    assert!(
        snapshot.mailbox.is_none(),
        "the base topology derives no mailbox"
    );
    snapshot.mailbox = Some(populated);

    // Snapshot content wins: restore rehydrates the mailbox even though the
    // restored topology alone would leave it disabled, and the advertised config
    // is widened so `collaboration()` agrees with the live primitive.
    let restored = restore(snapshot);
    assert!(
        restored.collaboration().mailbox_enabled(),
        "a captured mailbox slice widens the effective collaboration config"
    );
    let restored_mailbox = restored.mailbox().expect("mailbox restored from snapshot");
    let researcher = restored_mailbox.read_from("researcher", 0);
    assert_eq!(
        researcher
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        vec!["need sources for claim 3", "prioritise section 2"],
        "snapshot content restores the researcher inbox in order"
    );
    // The sequence cursor is authoritative too: a post-restore send continues it
    // rather than reusing an old seq.
    let next_seq = restored_mailbox.send("planner", "researcher", "one more");
    assert_eq!(next_seq, 2, "restore resumes the mailbox sequence cursor");

    // A substrate the snapshot did not carry (and the topology did not enable)
    // stays absent — the widening only covers what the snapshot restored.
    assert!(restored.blackboard().is_none());
    assert!(restored.plan().is_none());
}

#[test]
fn legacy_snapshot_without_collaboration_fields_restores_bare() {
    // Simulate a snapshot persisted before collaboration capture existed: encode
    // a real snapshot, drop the collaboration slices, and confirm the
    // `#[serde(default)]` fields decode to empty and restore builds a bare agent.
    let agent = mailbox_supervisor();
    agent.mailbox().expect("mailbox provisioned").send(
        "reviewer",
        "researcher",
        "will be dropped from the legacy blob",
    );
    let snapshot = agent.snapshot().expect("snapshot at a committed point");

    let mut value = serde_json::to_value(&snapshot).expect("encode snapshot");
    let object = value
        .as_object_mut()
        .expect("snapshot encodes as an object");
    for legacy_absent in ["mailbox", "blackboard", "plan", "artifacts"] {
        object.remove(legacy_absent);
    }
    assert!(
        !object.contains_key("mailbox"),
        "the legacy blob omits the collaboration slices entirely"
    );

    let legacy: AgentSnapshot =
        serde_json::from_value(value).expect("legacy snapshot decodes via serde defaults");
    assert!(
        legacy.mailbox.is_none() && legacy.blackboard.is_none() && legacy.plan.is_none(),
        "missing collaboration fields default to absent"
    );
    assert!(
        legacy.artifacts.is_empty(),
        "missing artifacts defaults empty"
    );

    // Restore succeeds and re-derives an *empty* substrate from the topology: the
    // two-delegate recipe still enables a mailbox, but with no captured content.
    let restored = restore(legacy);
    let restored_mailbox = restored
        .mailbox()
        .expect("topology re-enables an empty mailbox");
    assert!(
        restored_mailbox.read_from("researcher", 0).is_empty(),
        "a legacy snapshot restores empty collaboration content"
    );
    assert!(restored.blackboard().is_none());
    assert!(restored.plan().is_none());
}
