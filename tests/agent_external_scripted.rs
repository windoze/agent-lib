//! Core Rust suite: scripted external *runtime* adapter managed loop (M5-2).
//!
//! Where the other `agent_external_*` suites drive the machine through the
//! short-circuiting
//! [`ScriptedExternalSessionHandler`](agent_testkit::prelude::ScriptedExternalSessionHandler)
//! — which returns a pre-built
//! [`ExternalSessionResult`](agent_lib::agent::ExternalSessionResult) without
//! touching the runtime layer — this suite exercises the milestone-5 runtime
//! abstraction end to end: a
//! [`ScriptedExternalRuntimeAdapter`](agent_testkit::prelude::ScriptedExternalRuntimeAdapter)
//! advances a live
//! [`ExternalRuntimeSession`](agent_lib::agent::external::ExternalRuntimeSession)
//! through a script of decision points, an
//! [`ExternalSessionRegistry`](agent_lib::agent::external::ExternalSessionRegistry)
//! owns the live handle across turns, and a registry-backed
//! [`ScriptedRuntimeExternalSessionHandler`](agent_testkit::prelude::ScriptedRuntimeExternalSessionHandler)
//! folds each advance into the machine. Every test drains one turn through the
//! reference [`DrainHarness`](agent_testkit::prelude::DrainHarness), offline.
//!
//! Each `#[tokio::test]` proves one managed-loop path settles on `Done`:
//!
//! - start → completed — a first user message starts a fresh live session that
//!   completes in one advance, mirroring its observations to the live sink.
//! - tool batch round-trip — a `PausedForToolCalls` decision point becomes host
//!   `NeedTool`s, whose results relay back as one `RespondToolResults` that
//!   reattaches the same live session and completes it.
//! - interaction round-trip — a `PausedForInteraction` becomes a host
//!   `NeedInteraction`, whose resolution relays back as a `RespondInteraction`
//!   that completes the session.
//! - subagent round-trip — a `PausedForSubagent` becomes a host `NeedSubagent`,
//!   whose driven child relays back as a `RespondSubagent` that completes the
//!   session.
//! - mixed tool + subagent — one live session pauses first for a host tool batch
//!   and then for a host subagent across three advances; both bridges reattach
//!   the same live handle (no restart) before the session completes.
//!
//! Run in isolation with `cargo test --test agent_external_scripted`, or filter
//! with `cargo test scripted_external`.

use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::{LoopCursorKind, RequirementKindTag};

/// A first user message starts a fresh live session that completes in one
/// advance; the machine settles on `Done` and the live sink mirrors both
/// buffered observations on one monotonic `seq` line.
#[tokio::test]
async fn scripted_external_start_to_completed() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let handler = ScriptedRuntimeBuilder::new()
        .advance(
            ScriptedAdvance::completed(fixture.output("refactor complete"))
                .expecting(ExternalInputKind::Start)
                .emitting([fixture.command_finished_event(), fixture.file_patch_event()]),
        )
        .build();
    let external_log = Arc::clone(handler.log());
    let sink = Arc::clone(handler.sink());
    let start_log = handler.start_log().clone();
    let scope = TestScope::builder().external(Arc::new(handler)).build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the scripted start→completed advance drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    // One fresh session started; one advance, keyed Start → Completed.
    assert_eq!(start_log.len(), 1);
    assert_external_calls(&external_log)
        .count(1)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start])
        .result_kinds(&[ExternalResultKind::Completed]);

    // The live sink saw both observations on one monotonic seq line.
    assert_eq!(sink.seqs(), vec![0, 1]);

    let machine = harness.into_machine();
    assert!(
        machine.state().session().is_some(),
        "a completed advance records the resumable session facts"
    );
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// A `PausedForToolCalls` decision point bridges to host `NeedTool`s; their
/// results relay back as one `RespondToolResults` that reattaches the same live
/// session (no restart) and completes it.
#[tokio::test]
async fn scripted_external_tool_batch_round_trip() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine_with_tool_ids();

    let handler = ScriptedRuntimeBuilder::new()
        .advance(
            ScriptedAdvance::paused_for_tool_calls(
                fixture.tool_batch_id(),
                vec![
                    fixture.tool_call("call-a", "apply_patch"),
                    fixture.tool_call("call-b", "run_tests"),
                ],
            )
            .expecting(ExternalInputKind::Start),
        )
        .advance(
            ScriptedAdvance::completed(fixture.output("refactor complete"))
                .expecting(ExternalInputKind::RespondToolResults),
        )
        .build();
    let external_log = Arc::clone(handler.log());
    let start_log = handler.start_log().clone();

    let tool = ScriptedToolHandler::from_steps([
        ToolStep::ok("call-a", "patch applied"),
        ToolStep::ok("call-b", "1 passed"),
    ]);
    let tool_log = Arc::clone(tool.log());

    let scope = TestScope::builder()
        .external(Arc::new(handler))
        .tool(Arc::new(tool))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the tool-batch round-trip drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    // A single fresh session serviced both advances: the reattach never restarts.
    assert_eq!(start_log.len(), 1);
    assert_external_calls(&external_log)
        .count(2)
        .all_completed()
        .input_kinds(&[
            ExternalInputKind::Start,
            ExternalInputKind::RespondToolResults,
        ])
        .result_kinds(&[
            ExternalResultKind::PausedForToolCalls,
            ExternalResultKind::Completed,
        ]);

    // Both bridged tool calls were executed exactly once.
    assert_eq!(tool_log.records().len(), 2);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// A `PausedForInteraction` decision point bridges to a host `NeedInteraction`;
/// its resolution relays back as a `RespondInteraction` that completes the
/// session.
#[tokio::test]
async fn scripted_external_interaction_round_trip() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    let handler = ScriptedRuntimeBuilder::new()
        .advance(
            ScriptedAdvance::paused_for_interaction(
                "act-1",
                Interaction::permission(ids.step_id(), fixture.permission_request()),
            )
            .expecting(ExternalInputKind::Start)
            .emitting([fixture.permission_requested_event("act-1", "run `cargo test`")]),
        )
        .advance(
            ScriptedAdvance::completed(fixture.output("refactor complete"))
                .expecting(ExternalInputKind::RespondInteraction),
        )
        .build();
    let external_log = Arc::clone(handler.log());
    let start_log = handler.start_log().clone();

    let interaction = ScriptedInteractionHandler::sequence([InteractionDecision::Approve]);
    let interaction_log = Arc::clone(interaction.log());

    let scope = TestScope::builder()
        .external(Arc::new(handler))
        .interaction(Arc::new(interaction))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser")
        .await
        .expect("the interaction round-trip drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    assert_eq!(start_log.len(), 1);
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

    // The host resolved exactly one interaction.
    assert_eq!(interaction_log.records().len(), 1);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// A `PausedForSubagent` decision point bridges to a host `NeedSubagent`; its
/// driven child relays back as a `RespondSubagent` that completes the session.
#[tokio::test]
async fn scripted_external_subagent_round_trip() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine();

    // Child: emits one NeedInteraction its own attended scope answers in place,
    // so it never pops to the parent and runs to completion.
    let child_machine = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            ids.requirement_id(),
            RequirementKind::NeedInteraction {
                request: Interaction::question(ids.step_id(), "child needs a human".to_owned()),
            },
        ))
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
    let subagent = Arc::clone(&spawner).into_handler(4);

    let handler = ScriptedRuntimeBuilder::new()
        .advance(
            ScriptedAdvance::paused_for_subagent(fixture.subagent_request("spawn-1"))
                .expecting(ExternalInputKind::Start),
        )
        .advance(
            ScriptedAdvance::completed(fixture.output("refactor complete"))
                .expecting(ExternalInputKind::RespondSubagent),
        )
        .build();
    let external_log = Arc::clone(handler.log());
    let start_log = handler.start_log().clone();

    let scope = TestScope::builder()
        .external(Arc::new(handler))
        .subagent(Arc::new(subagent))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("investigate the flaky test")
        .await
        .expect("the subagent round-trip drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    assert_eq!(start_log.len(), 1);
    assert_external_calls(&external_log)
        .count(2)
        .all_completed()
        .input_kinds(&[ExternalInputKind::Start, ExternalInputKind::RespondSubagent])
        .result_kinds(&[
            ExternalResultKind::PausedForSubagent,
            ExternalResultKind::Completed,
        ]);

    // The child ran to completion (one resume: its own answered interaction), and
    // the handler drove exactly one child.
    assert_eq!(
        child_log.resume_tags(),
        vec![RequirementKindTag::Interaction]
    );
    assert_eq!(spawner.spawn_calls(), 1);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}

/// One live session pauses first for a host tool batch and then for a host
/// subagent before completing. Both bridges reattach the same live handle across
/// three advances (`Start → PausedForToolCalls`, `RespondToolResults →
/// PausedForSubagent`, `RespondSubagent → Completed`) without ever restarting the
/// session, proving the milestone-5 registry keeps one live handle across
/// interleaved tool and subagent phases.
#[tokio::test]
async fn scripted_external_mixed_tool_and_subagent_round_trip() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let fixture = ExternalAgentFixture::new(&ids);
    let machine = fixture.machine_with_tool_ids();

    // Child: emits one NeedInteraction its own attended scope answers in place,
    // so it never pops to the parent and runs to completion.
    let child_machine = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            ids.requirement_id(),
            RequirementKind::NeedInteraction {
                request: Interaction::question(ids.step_id(), "child needs a human".to_owned()),
            },
        ))
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
    let subagent = Arc::clone(&spawner).into_handler(4);

    let handler = ScriptedRuntimeBuilder::new()
        .advance(
            ScriptedAdvance::paused_for_tool_calls(
                fixture.tool_batch_id(),
                vec![fixture.tool_call("call-a", "apply_patch")],
            )
            .expecting(ExternalInputKind::Start)
            .emitting([fixture.command_finished_event()]),
        )
        .advance(
            ScriptedAdvance::paused_for_subagent(fixture.subagent_request("spawn-1"))
                .expecting(ExternalInputKind::RespondToolResults),
        )
        .advance(
            ScriptedAdvance::completed(fixture.output("refactor complete"))
                .expecting(ExternalInputKind::RespondSubagent),
        )
        .build();
    let external_log = Arc::clone(handler.log());
    let start_log = handler.start_log().clone();

    let tool = ScriptedToolHandler::from_steps([ToolStep::ok("call-a", "patch applied")]);
    let tool_log = Arc::clone(tool.log());

    let scope = TestScope::builder()
        .external(Arc::new(handler))
        .tool(Arc::new(tool))
        .subagent(Arc::new(subagent))
        .build();

    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("refactor the parser and investigate the flaky test")
        .await
        .expect("the mixed tool + subagent round-trip drains to completion");

    assert_eq!(observed.final_cursor().kind(), LoopCursorKind::Done);

    // A single fresh session serviced all three advances: neither the tool nor
    // the subagent reattach restarts the live handle.
    assert_eq!(start_log.len(), 1);
    assert_external_calls(&external_log)
        .count(3)
        .all_completed()
        .input_kinds(&[
            ExternalInputKind::Start,
            ExternalInputKind::RespondToolResults,
            ExternalInputKind::RespondSubagent,
        ])
        .result_kinds(&[
            ExternalResultKind::PausedForToolCalls,
            ExternalResultKind::PausedForSubagent,
            ExternalResultKind::Completed,
        ]);

    // The bridged tool call ran once, and the handler drove exactly one child.
    assert_eq!(tool_log.records().len(), 1);
    assert_eq!(
        child_log.resume_tags(),
        vec![RequirementKindTag::Interaction]
    );
    assert_eq!(spawner.spawn_calls(), 1);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text("refactor complete");
}
