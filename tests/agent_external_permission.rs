//! Core Rust suite: external-agent permission interaction (milestone 4, M4-3).
//!
//! Fast, offline regressions over the external-agent permission path: a
//! [`ScriptedExternalSessionHandler`](agent_testkit::prelude::ScriptedExternalSessionHandler)
//! pauses a session with an
//! [`InteractionKind::Permission`](agent_lib::agent::InteractionKind::Permission)
//! request (via [`ExternalAgentFixture::permission_pause`]), a
//! [`ScriptedInteractionHandler`](agent_testkit::prelude::ScriptedInteractionHandler)
//! resolves it with a permission decision, and the
//! [`ExternalAgentMachine`](agent_lib::agent::ExternalAgentMachine) feeds the
//! resolved [`InteractionResponse::Permission`] back into the session as a
//! [`RespondInteraction`](agent_lib::agent::ExternalSessionInput::RespondInteraction).
//! Each `#[tokio::test]` proves the machine relays the *correct* decision to the
//! runtime:
//!
//! - approve flow — an approved permission reaches the runtime as a
//!   [`PermissionDecision::Approve`](agent_lib::agent::PermissionDecision::Approve)
//!   echoing the request's `action_id`, and the session then completes.
//! - deny flow — a denied permission reaches the runtime as a
//!   [`PermissionDecision::Deny`](agent_lib::agent::PermissionDecision::Deny)
//!   carrying its rationale, and the session still settles cleanly.
//!
//! Run in isolation with `cargo test --test agent_external_permission`, or filter
//! with `cargo test external_agent_permission`.

use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::{
    ExternalSessionInput, InteractionResponse, LoopCursorKind, PermissionResponse,
};

/// Unwraps the `RespondInteraction` a paused external session was resumed with,
/// asserting it echoes `action_id`, and returns the response it carried.
fn respond_interaction_response(
    request: &agent_lib::agent::ExternalSessionRequest,
    action_id: &str,
) -> InteractionResponse {
    match &request.input {
        ExternalSessionInput::RespondInteraction {
            action_id: echoed,
            response,
        } => {
            assert_eq!(echoed, action_id, "the pause's action_id is echoed back");
            response.clone()
        }
        other => panic!("resume must feed a RespondInteraction, got {other:?}"),
    }
}

/// An approved permission reaches the runtime as a permission approve response
/// echoing the request's `action_id`; the session then completes.
#[tokio::test]
async fn external_agent_permission_approve_flow() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let external = ScriptedExternalSessionHandler::from_steps([
        ExternalSessionStep::result(fixture.permission_pause()),
        ExternalSessionStep::result(fixture.completed()),
    ]);
    let external_log = Arc::clone(external.log());

    let interaction = ScriptedInteractionHandler::sequence([InteractionDecision::Approve]);
    let interaction_log = Arc::clone(interaction.log());

    let scope = TestScope::builder()
        .external(Arc::new(external))
        .interaction(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the permission approve advance drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    // The session is advanced twice: a Start that pauses for permission, then a
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

    // The machine relays a permission *approve* keyed to the paused action.
    let expected = InteractionResponse::Permission(PermissionResponse::approve("act-1".to_owned()));
    let external_records = external_log.records();
    assert_eq!(
        respond_interaction_response(&external_records[1].request, "act-1"),
        expected
    );

    // The interaction handler resolved exactly one request with that response.
    let interaction_records = interaction_log.records();
    assert_eq!(interaction_records.len(), 1);
    assert_eq!(interaction_records[0].result.as_ref(), Some(&expected));

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// A denied permission reaches the runtime as a permission deny response
/// carrying its rationale; the session still settles cleanly.
#[tokio::test]
async fn external_agent_permission_deny_flow() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let external = ScriptedExternalSessionHandler::from_steps([
        ExternalSessionStep::result(fixture.permission_pause()),
        ExternalSessionStep::result(fixture.completed()),
    ]);
    let external_log = Arc::clone(external.log());

    let interaction = ScriptedInteractionHandler::sequence([InteractionDecision::Deny(Some(
        "shell is blocked by policy".to_owned(),
    ))]);
    let interaction_log = Arc::clone(interaction.log());

    let scope = TestScope::builder()
        .external(Arc::new(external))
        .interaction(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the permission deny advance drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

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

    // The machine relays a permission *deny* keyed to the paused action, with the
    // policy rationale preserved inline.
    let expected = InteractionResponse::Permission(PermissionResponse::deny(
        "act-1".to_owned(),
        Some("shell is blocked by policy".to_owned()),
    ));
    let external_records = external_log.records();
    assert_eq!(
        respond_interaction_response(&external_records[1].request, "act-1"),
        expected
    );

    let interaction_records = interaction_log.records();
    assert_eq!(interaction_records.len(), 1);
    assert_eq!(interaction_records[0].result.as_ref(), Some(&expected));
}
