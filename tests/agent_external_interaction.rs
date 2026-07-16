//! Core Rust suite: external-agent two-stage interaction (milestone 3, M3-3).
//!
//! Fast, offline regressions over the pause→respond path of an
//! [`ExternalAgentMachine`](agent_lib::agent::ExternalAgentMachine), driven to
//! the end of one turn through the testkit
//! [`DrainHarness`](agent_testkit::prelude::DrainHarness). A scripted
//! [`ScriptedExternalSessionHandler`](agent_testkit::prelude::ScriptedExternalSessionHandler)
//! first pauses the session for an interaction and then completes it, while a
//! [`ScriptedInteractionHandler`](agent_testkit::prelude::ScriptedInteractionHandler)
//! resolves the clarification. Each `#[tokio::test]` proves one invariant:
//!
//! - pause → respond → completed — a paused session reifies one
//!   `NeedInteraction`; once resolved, the machine feeds a `RespondInteraction`
//!   back into the session, which then completes and settles on `Done`.
//! - interaction routing — when the local scope has no interaction handler, the
//!   `NeedInteraction` is served by a wrapped outer layer rather than surfacing
//!   as an unhandled requirement.
//!
//! Run in isolation with `cargo test --test agent_external_interaction`, or
//! filter the pause-path regressions with `cargo test external_agent_pause`.

use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::LoopCursorKind;

/// A paused session emits one interaction; resolving it feeds a
/// `RespondInteraction` back into the session, which then completes.
#[tokio::test]
async fn external_agent_pause_resume_interaction() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let external = ScriptedExternalSessionHandler::from_steps([
        ExternalSessionStep::result(fixture.permission_pause()),
        ExternalSessionStep::result(fixture.completed()),
    ]);
    let external_log = Arc::clone(external.log());

    let interaction = ScriptedInteractionHandler::sequence([InteractionDecision::Answer(
        "yes, proceed".to_owned(),
    )]);
    let interaction_log = Arc::clone(interaction.log());

    let scope = TestScope::builder()
        .external(Arc::new(external))
        .interaction(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the pause→respond→completed advance drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    // The session is advanced twice: a Start that pauses, then a
    // RespondInteraction that completes it.
    assert_external_calls(&external_log)
        .count(2)
        .all_completed()
        .input_kinds(&[
            ExternalInputKind::Start,
            ExternalInputKind::RespondInteraction,
        ])
        .result_kinds(&[
            ExternalResultKind::PausedForInteraction,
            ExternalResultKind::Completed,
        ]);

    // Exactly one interaction was resolved between the two session steps.
    assert_calls(&interaction_log).count(1).all_completed();

    let machine = harness.into_machine();
    assert!(
        machine.state().session().is_some(),
        "the pause records the resumable session facts before the interaction"
    );
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// When the local scope lacks an interaction handler, the machine's
/// `NeedInteraction` is served by a wrapped outer layer rather than surfacing as
/// an unhandled requirement.
#[tokio::test]
async fn external_agent_pause_pops_interaction_to_outer_scope() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let external = ScriptedExternalSessionHandler::from_steps([
        ExternalSessionStep::result(fixture.permission_pause()),
        ExternalSessionStep::result(fixture.completed()),
    ]);
    let external_log = Arc::clone(external.log());

    let interaction =
        ScriptedInteractionHandler::sequence([InteractionDecision::Answer("approved".to_owned())]);
    let interaction_log = Arc::clone(interaction.log());

    // The outer layer serves interaction; the local layer serves only the
    // external session, so the machine's NeedInteraction pops outward.
    let outer = TestScope::builder()
        .interaction(Arc::new(interaction))
        .build();
    let scope = TestScope::builder()
        .external(Arc::new(external))
        .wrapping(Arc::new(outer))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the interaction pops to the outer scope and the turn completes");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);
    assert_calls(&interaction_log).count(1).all_completed();
    assert_external_calls(&external_log)
        .count(2)
        .all_completed()
        .input_kinds(&[
            ExternalInputKind::Start,
            ExternalInputKind::RespondInteraction,
        ])
        .result_kinds(&[
            ExternalResultKind::PausedForInteraction,
            ExternalResultKind::Completed,
        ]);
}
