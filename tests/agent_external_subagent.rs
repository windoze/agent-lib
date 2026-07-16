//! Core Rust suite: external-agent subagent bridge (milestone 3, M3-1).
//!
//! Fast, offline regressions over the
//! `PausedForSubagent -> NeedSubagent -> RespondSubagent` path of an
//! [`ExternalAgentMachine`](agent_lib::agent::ExternalAgentMachine), driven to
//! the end of one turn through the testkit
//! [`DrainHarness`](agent_testkit::prelude::DrainHarness). A scripted
//! [`ScriptedExternalSessionHandler`](agent_testkit::prelude::ScriptedExternalSessionHandler)
//! first pauses the session to spawn a subagent and then completes it, while a
//! reference [`DrivingSubagentHandler`](agent_lib::agent::DrivingSubagentHandler)
//! — built from the kit's [`ScriptedSubagentSpawner`] — drives the child to
//! completion under its own subagent machinery. Each `#[tokio::test]` proves one
//! invariant:
//!
//! - pause → drive child → respond → completed — a paused session reifies one
//!   `NeedSubagent`; once the child machine completes, the machine feeds a
//!   `RespondSubagent` echoing the same `request_id` back into the session, which
//!   then completes and settles on `Done`.
//! - child interaction pops to the outer parent — a headless child's
//!   `NeedInteraction` pops past the subagent handler to the parent scope's
//!   interaction backend, without re-entering the subagent handler.
//!
//! Run in isolation with `cargo test --test agent_external_subagent`, or filter
//! the driving regressions with `cargo test driving_subagent`.

use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::{LoopCursorKind, RequirementKindTag};

/// Builds a child `NeedInteraction` requirement from the shared id sequence.
fn interaction_requirement(ids: &SeqIds, prompt: &str) -> Requirement {
    Requirement::at_root(
        ids.requirement_id(),
        RequirementKind::NeedInteraction {
            request: Interaction::question(ids.step_id(), prompt.to_owned()),
        },
    )
}

/// A paused session's `PausedForSubagent` becomes one `NeedSubagent`, which a
/// [`DrivingSubagentHandler`] fulfils by driving an (attended) child machine to
/// completion; the machine then relays a `RespondSubagent` echoing the same
/// `request_id`, and the session completes.
#[tokio::test]
async fn external_agent_driving_subagent_fulfills_child() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    // Child: emits one NeedInteraction its own (attended) scope answers in
    // place, so it never pops to the parent and runs to completion.
    let child_machine = ScriptMachine::builder()
        .requirement(interaction_requirement(&ids, "child needs a human"))
        .done_after_all_resumed()
        .label("child")
        .build();
    let child_log = Arc::clone(child_machine.log());
    let child_interaction = Arc::new(ScriptedInteractionHandler::fixed(
        InteractionDecision::Answer("done".to_owned()),
    ));
    let child = SpawnedChildBuilder::new()
        .machine(child_machine)
        .scope(attended_child_scope(child_interaction).build())
        .opening(user_input(&ids, "open child"))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("child summary")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(4);

    let external = ScriptedExternalSessionHandler::from_steps([
        ExternalSessionStep::result(fixture.subagent_pause("spawn-1")),
        ExternalSessionStep::result(fixture.completed()),
    ]);
    let external_log = Arc::clone(external.log());

    // The local scope serves both the external session and the bridged
    // NeedSubagent through the driving handler.
    let scope = TestScope::builder()
        .external(Arc::new(external))
        .subagent(Arc::new(handler))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("investigate the flaky test")
        .await
        .expect("the pause→drive-child→respond→completed advance drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    // The session is advanced twice: a Start that pauses for the subagent, then
    // a RespondSubagent that completes it.
    assert_external_calls(&external_log)
        .count(2)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start, ExternalInputKind::RespondSubagent])
        .result_kinds(&[
            ExternalResultKind::PausedForSubagent,
            ExternalResultKind::Completed,
        ]);

    // The child ran to completion (one resume: its own answered interaction).
    assert_eq!(
        child_log.resume_tags(),
        vec![RequirementKindTag::Interaction]
    );
    // The handler derived + spawned + summarized exactly one child.
    assert_eq!(spawner.ids_calls(), 1);
    assert_eq!(spawner.spawn_calls(), 1);
    assert_eq!(spawner.summarize_calls(), 1);
}

/// A headless child's `NeedInteraction` pops past the subagent handler to the
/// parent scope's interaction backend, which serves it exactly once — the
/// subagent handler is not re-entered — and both machines complete.
#[tokio::test]
async fn external_agent_driving_subagent_pops_child_interaction_to_outer() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    // Child: emits one NeedInteraction its own (headless) scope cannot serve.
    let child_machine = ScriptMachine::builder()
        .requirement(interaction_requirement(&ids, "child needs a human"))
        .done_after_all_resumed()
        .label("child")
        .build();
    let child_log = Arc::clone(child_machine.log());
    let child = SpawnedChildBuilder::new()
        .machine(child_machine)
        .scope(headless_child_scope().build())
        .opening(user_input(&ids, "open child"))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("child summary")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(4);

    let external = ScriptedExternalSessionHandler::from_steps([
        ExternalSessionStep::result(fixture.subagent_pause("spawn-1")),
        ExternalSessionStep::result(fixture.completed()),
    ]);
    let external_log = Arc::clone(external.log());

    // The parent scope serves the external session, the bridged NeedSubagent,
    // and the child's popped NeedInteraction — the latter without re-entering
    // the subagent handler.
    let parent_interaction = Arc::new(ScriptedInteractionHandler::fixed(
        InteractionDecision::Answer("ok".to_owned()),
    ));
    let parent_interaction_log = Arc::clone(parent_interaction.log());
    let scope = TestScope::builder()
        .external(Arc::new(external))
        .subagent(Arc::new(handler))
        .attended(parent_interaction)
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("investigate the flaky test")
        .await
        .expect("the child interaction pops to the parent and the turn completes");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    assert_external_calls(&external_log)
        .count(2)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start, ExternalInputKind::RespondSubagent])
        .result_kinds(&[
            ExternalResultKind::PausedForSubagent,
            ExternalResultKind::Completed,
        ]);

    // The child's interaction popped to the parent and was served exactly once.
    assert_calls(&parent_interaction_log)
        .count(1)
        .all_completed();
    // The child ran to completion (one resume: the popped interaction result).
    assert_eq!(
        child_log.resume_tags(),
        vec![RequirementKindTag::Interaction]
    );
    // The handler spawned exactly one child and was not re-entered for the
    // popped interaction.
    assert_eq!(spawner.spawn_calls(), 1);
    assert_eq!(spawner.summarize_calls(), 1);
}
