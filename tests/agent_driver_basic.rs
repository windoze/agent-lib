//! Core Rust suite: reference-driver routing basics (milestone 6, M6-3).
//!
//! Fast, offline regressions over the reference [`drain`] driver's routing,
//! written against a [`ScriptMachine`] double (which emits a fixed requirement
//! batch and routes results purely by id) layered over explicit [`TestScope`]s.
//! Using the double keeps the focus on the *driver* — local fulfilment, pop
//! routing, the unhandled-requirement boundary, and the return-path family check
//! — without any `DefaultAgentMachine` folding. One `#[tokio::test]` per
//! invariant:
//!
//! - local handler — a requirement the emitting scope can serve is fulfilled in
//!   place and fed back.
//! - pop to parent — a requirement the (headless) child scope cannot serve pops
//!   to the attended parent layer and is answered there.
//! - top unhandled — a requirement no layer serves surfaces as a classified
//!   [`AgentError::UnhandledRequirement`], never silently dropped.
//! - misaligned result — a handler returning the wrong result family trips the
//!   driver's [`RequirementKind::accepts`](agent_lib::agent::RequirementKind::accepts)
//!   check and fails the turn.
//!
//! Run in isolation with `cargo test --test agent_driver_basic`.

use std::sync::Arc;

use agent_testkit::prelude::*;

use agent_lib::agent::{
    AgentError, AgentErrorKind, LoopCursorKind, RequirementKindTag, RequirementResult, ScopePop,
    drain,
};
use serde_json::json;

/// A requirement the emitting scope can serve is fulfilled locally and fed back:
/// the turn completes, the tool ran once, and the machine recorded the resume.
#[tokio::test]
async fn local_handler_resolves_in_place() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let req_id = ids.requirement_id();
    let mut machine = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            req_id,
            RequirementKind::NeedTool {
                call_id: ids.tool_call_id(),
                call: tool_call("call-a", "note", json!({ "text": "record" })),
            },
        ))
        .done_after_all_resumed()
        .label("local")
        .build();

    let tool = ScriptedToolHandler::from_steps([ToolStep::ok("call-a", "noted")]);
    let tool_log = Arc::clone(tool.log());
    let scope = TestScope::builder().tool(Arc::new(tool)).build();

    let done = drain(&mut machine, user_input(&ids, "go"), &scope, None, &ctx)
        .await
        .expect("the local tool requirement drains to completion");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(tool_log.len(), 1, "the emitting scope served the tool");
    assert_eq!(machine.resume_tags(), vec![RequirementKindTag::Tool]);
    assert_eq!(machine.resume_order(), vec![req_id]);
}

/// A requirement the headless child scope cannot serve pops to the attended
/// parent layer, which answers it; the child's unrelated tool slot is untouched.
#[tokio::test]
async fn interaction_pops_to_the_parent_scope() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let req_id = ids.requirement_id();
    let mut machine = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            req_id,
            RequirementKind::NeedInteraction {
                request: Interaction::question(ids.step_id(), "confirm?".to_owned()),
            },
        ))
        .done_after_all_resumed()
        .label("child")
        .build();

    // The child (inner) scope is headless: it serves tools but no interaction.
    let inner_tool = ScriptedToolHandler::from_steps([ToolStep::ok("unused", "x")]);
    let inner_tool_log = Arc::clone(inner_tool.log());
    let inner = TestScope::builder().tool(Arc::new(inner_tool)).build();

    // The parent (outer) scope answers the popped interaction.
    let outer_interaction = ScriptedInteractionHandler::approve_all();
    let outer_log = Arc::clone(outer_interaction.log());
    let outer = TestScope::builder()
        .attended(Arc::new(outer_interaction))
        .build();
    let mut parent = ScopePop::new(&outer, None);

    let done = drain(
        &mut machine,
        user_input(&ids, "go"),
        &inner,
        Some(&mut parent),
        &ctx,
    )
    .await
    .expect("the popped interaction is answered by the parent");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(outer_log.len(), 1, "the interaction popped to the parent");
    assert_eq!(inner_tool_log.len(), 0, "the child tool was never touched");
    assert_eq!(machine.resume_tags(), vec![RequirementKindTag::Interaction]);
}

/// A requirement no scope layer can serve surfaces as a classified
/// [`AgentError::UnhandledRequirement`] naming the family — never dropped or
/// auto-fulfilled.
#[tokio::test]
async fn top_scope_without_handler_is_unhandled_requirement() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let mut machine = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            ids.requirement_id(),
            RequirementKind::NeedInteraction {
                request: Interaction::question(ids.step_id(), "confirm?".to_owned()),
            },
        ))
        .done_after_all_resumed()
        .label("headless")
        .build();

    // Headless top scope with no parent: the interaction has nowhere to go.
    let scope = TestScope::empty();

    let error = drain(&mut machine, user_input(&ids, "go"), &scope, None, &ctx)
        .await
        .expect_err("a headless top scope cannot fulfil the interaction");

    assert_eq!(error.kind(), AgentErrorKind::UnhandledRequirement);
    match error {
        AgentError::UnhandledRequirement { kind, .. } => {
            assert_eq!(kind, RequirementKindTag::Interaction);
        }
        other => panic!("expected UnhandledRequirement, got {other:?}"),
    }
}

/// A handler that returns a result of the wrong family trips the driver's
/// return-path family check, failing the turn with a misalignment diagnostic.
#[tokio::test]
async fn misaligned_result_fails_the_turn() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let req_id = ids.requirement_id();
    let mut machine = ScriptMachine::builder()
        .requirement(Requirement::at_root(
            req_id,
            RequirementKind::NeedTool {
                call_id: ids.tool_call_id(),
                call: tool_call("call-a", "note", json!({ "text": "record" })),
            },
        ))
        .done_after_all_resumed()
        .label("misaligned")
        .build();

    // The tool slot answers with an *LLM* result: the driver must reject it.
    let misaligned = MisalignedHandler::returning(RequirementResult::Llm(Ok(assistant_text(
        "nope",
        usage(1, 1),
    ))));
    let scope = TestScope::builder().tool(Arc::new(misaligned)).build();

    let error = drain(&mut machine, user_input(&ids, "go"), &scope, None, &ctx)
        .await
        .expect_err("a misaligned handler result must fail the turn");

    match error {
        AgentError::Other(message) => assert!(
            message.contains("misaligned"),
            "expected a misalignment diagnostic, got: {message}"
        ),
        other => panic!("expected AgentError::Other, got {other:?}"),
    }
    // The stray resume was never applied, so the machine did not complete.
    assert_ne!(machine.cursor().kind(), LoopCursorKind::Done);
}
