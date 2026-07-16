//! Core Rust suite: external-agent lifecycle boundaries (milestone 3, M3-4).
//!
//! Fast, offline regressions over the two lifecycle boundaries M3-4 lands on top
//! of the basic advance path
//! ([`agent_external_basic`](super)): the never-resume **cancel/abandon** close
//! and **mounting** an [`ExternalAgentMachine`](agent_lib::agent::ExternalAgentMachine)
//! as a subagent child. Each `#[tokio::test]` drives real crate machinery through
//! the testkit rather than a bespoke harness:
//!
//! - `external_agent_abandon_settles_and_flags_cleanup` — a cancelled
//!   [`RunContext`](agent_lib::agent::RunContext) abandons the outstanding
//!   `NeedExternalSession` (never-resume, design §6.4): the machine settles back
//!   to a feedable `Idle` without emitting any `Shutdown`, the scripted runtime
//!   handler is never invoked, and the orphaned session is flagged
//!   [`cleanup_required`](agent_lib::agent::ExternalAgentState::cleanup_required)
//!   for the handle layer to force-close.
//! - `external_agent_mounts_under_nested_machine` — a parent
//!   [`ScriptMachine`](agent_testkit::prelude::ScriptMachine) emits one
//!   `NeedSubagent` whose child is an `ExternalAgentMachine`; the reference
//!   [`DrivingSubagentHandler`](agent_lib::agent::DrivingSubagentHandler) mounts
//!   it in a nested drain layer where it advances Start→Completed just like any
//!   other child machine, and the parent turn completes on the child's summary.
//!
//! Run in isolation with `cargo test --test agent_external_lifecycle`, or filter
//! the boundaries the milestone calls out with `cargo test external_agent_abandon`
//! and `cargo test external_agent_mounts`.

use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::{LoopCursorKind, drain};

/// A cancelled context abandons the outstanding session requirement: the machine
/// settles to a feedable `Idle`, never invokes the runtime handler, and flags the
/// orphaned session for handle-layer cleanup (design §6.4).
#[tokio::test]
async fn external_agent_abandon_settles_and_flags_cleanup() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    // A scripted runtime is wired in, but the cancelled drain abandons the
    // session requirement before it is ever served.
    let handler = ScriptedExternalSessionHandler::from_steps([ExternalSessionStep::result(
        fixture.completed(),
    )]);
    let log = Arc::clone(handler.log());
    let scope = TestScope::builder().external(Arc::new(handler)).build();

    // Cancel before driving: the opening step reifies one NeedExternalSession,
    // which the cancelled drain abandons (never-resume) instead of fulfilling.
    ctx.cancellation().cancel();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("a cancelled drain still closes the turn cleanly");

    // Never-resume abandon settles the machine back to a feedable Idle and emits
    // no Shutdown effect; the scripted runtime handler is never invoked.
    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Idle);
    assert_external_calls(&log).count(0);

    let machine = harness.into_machine();
    // The orphaned session is flagged for the handle layer to force-close, and
    // the dangling turn is discarded rather than committed.
    assert!(
        machine.state().cleanup_required(),
        "abandoning an outstanding session step flags handle-layer cleanup"
    );
    assert_conversation(machine.state().conversation())
        .committed_turns(0)
        .pending_none();
}

/// An `ExternalAgentMachine` mounts as a subagent child: a parent `NeedSubagent`
/// drives it through a nested drain layer where it advances Start→Completed, and
/// the parent turn completes on the child's summary.
#[tokio::test]
async fn external_agent_mounts_under_nested_machine() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);

    // The child is a real ExternalAgentMachine; its own scope serves the runtime
    // session with a single Completed step inside the nested layer.
    let child_handler = ScriptedExternalSessionHandler::from_steps([ExternalSessionStep::result(
        fixture.completed(),
    )]);
    let child_log = Arc::clone(child_handler.log());
    let child = SpawnedChildBuilder::new()
        .machine(fixture.machine())
        .scope(
            headless_child_scope()
                .external(Arc::new(child_handler))
                .build(),
        )
        .opening(user_input(&ids, "refactor the parser"))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("external child refactored the parser")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(4);
    let parent_scope = parent_scope_with_subagent(handler).build();

    // A minimal parent that delegates exactly one NeedSubagent to the external
    // child, then completes once it is resumed.
    let spec_ref = AgentSpecRef(ids.agent_id());
    let brief = Interaction::question(ids.step_id(), "refactor the parser".to_owned());
    let mut parent = ScriptMachine::builder()
        .requirements([Requirement::at_root(
            ids.requirement_id(),
            RequirementKind::NeedSubagent {
                spec_ref,
                brief,
                result_schema: None,
            },
        )])
        .done_after_all_resumed()
        .label("parent")
        .build();

    let done = drain(
        &mut parent,
        user_input(&ids, "delegate the refactor"),
        &parent_scope,
        None,
        &ctx,
    )
    .await
    .expect("the parent turn drains once the mounted external child completes");

    // The whole turn closed on the parent, and the child ran under its own layer.
    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(spawner.spawn_calls(), 1);
    assert_external_calls(&child_log)
        .count(1)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start])
        .result_kinds(&[ExternalResultKind::Completed]);
}
