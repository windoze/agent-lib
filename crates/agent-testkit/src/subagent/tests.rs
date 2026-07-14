//! Focused tests for the M5-3 subagent spawner and scope helpers.
//!
//! Each test drives the reference [`DrivingSubagentHandler`](agent_lib::agent::DrivingSubagentHandler)
//! over a [`ScriptedSubagentSpawner`] built from the kit's helpers, exercising
//! one of the four hierarchy guarantees the handler owns (migration doc §7.2 /
//! §7.3):
//!
//! 1. **Scope enforcement / pop from outer.** A headless child's
//!    `NeedInteraction` pops past the subagent handler to the attended parent
//!    scope, which serves it in place of the child's own (absent) backend.
//! 2. **Depth guard.** A context already at `max_depth` is refused with a
//!    classified [`SubagentDepthExceeded`](agent_lib::agent::AgentError::SubagentDepthExceeded),
//!    reaching neither `child_ids` nor `spawn`.
//! 3. **Cancel propagation.** A cancelled parent makes the child drain abandon
//!    the child's first requirement (never-resume) without invoking a child
//!    handler.
//! 4. **Budget inheritance.** A child's token charge lands on the parent's
//!    shared ledger because the child context is derived, not fresh.
//!
//! A fifth test covers [`attended_child_scope`]: an attended child resolves its
//! own interaction in place, so it never pops to the parent.

use super::{
    ScriptedSubagentSpawner, SpawnedChildBuilder, attended_child_scope, headless_child_scope,
    parent_scope_with_subagent,
};
use crate::concurrency::PanicOnCall;
use crate::fixtures::{assistant_text, root_context, usage, user_input};
use crate::handlers::{InteractionDecision, ScriptedInteractionHandler};
use crate::ids::SeqIds;
use crate::machine::ScriptMachine;
use crate::scope::TestScope;
use agent_lib::agent::{
    AgentError, AgentSpecRef, Interaction, LlmHandler, LlmStepMode, LoopCursorKind, Requirement,
    RequirementKind, RequirementKindTag, RequirementResult, RunContext, ScopePop, SubagentHandler,
    drain,
};
use agent_lib::client::ChatRequest;
use async_trait::async_trait;
use std::sync::Arc;

// ----- requirement helpers -----

fn interaction_requirement(ids: &SeqIds, prompt: &str) -> Requirement {
    Requirement::at_root(
        ids.requirement_id(),
        RequirementKind::NeedInteraction {
            request: Interaction::question(ids.step_id(), prompt.to_owned()),
        },
    )
}

fn chat_request() -> ChatRequest {
    ChatRequest {
        model: "test-model".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
        system: None,
        max_tokens: 16,
        temperature: None,
        stream: false,
        provider_extras: None,
    }
}

fn llm_requirement(ids: &SeqIds) -> Requirement {
    Requirement::at_root(
        ids.requirement_id(),
        RequirementKind::NeedLlm {
            request: chat_request(),
            mode: LlmStepMode::NonStreaming,
        },
    )
}

fn subagent_requirement(ids: &SeqIds, spec_ref: AgentSpecRef, brief: Interaction) -> Requirement {
    Requirement::at_root(
        ids.requirement_id(),
        RequirementKind::NeedSubagent {
            spec_ref,
            brief,
            result_schema: None,
        },
    )
}

/// An [`LlmHandler`] that charges a fixed token count against the run context
/// before returning a complete response, proving the child ledger is shared.
struct ChargingLlmHandler {
    tokens: u64,
}

#[async_trait]
impl LlmHandler for ChargingLlmHandler {
    async fn fulfill(
        &self,
        _request: &ChatRequest,
        _mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        ctx.charge_tokens(self.tokens)
            .expect("charge against the shared parent ledger");
        RequirementResult::Llm(Ok(assistant_text("hi", usage(1, 1))))
    }
}

// ----- tests -----

/// §7.3: a headless child's `NeedInteraction` pops past the subagent handler to
/// the attended parent scope, which serves it exactly once; both machines
/// complete and the parent is resumed with a `Subagent` result.
#[tokio::test]
async fn attended_parent_serves_headless_child_interaction_via_pop() {
    let ids = SeqIds::new();

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

    // The attended parent scope serves both the subagent and the popped
    // interaction the child's headless scope could not.
    let parent_interaction = Arc::new(ScriptedInteractionHandler::fixed(
        InteractionDecision::Answer("ok".to_owned()),
    ));
    let parent_interaction_log = Arc::clone(parent_interaction.log());
    let parent_scope = parent_scope_with_subagent(handler)
        .attended(parent_interaction)
        .build();

    let spec_ref = AgentSpecRef(ids.agent_id());
    let brief = Interaction::question(ids.step_id(), "delegate".to_owned());
    let mut parent = ScriptMachine::builder()
        .requirement(subagent_requirement(&ids, spec_ref, brief))
        .done_after_all_resumed()
        .label("parent")
        .build();
    let parent_log = Arc::clone(parent.log());
    let ctx = root_context(&ids);

    let done = drain(
        &mut parent,
        user_input(&ids, "go"),
        &parent_scope,
        None,
        &ctx,
    )
    .await
    .expect("parent drain completes");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    // The child's interaction popped to the parent and was served exactly once.
    assert_eq!(parent_interaction_log.len(), 1);
    // The child ran to completion (one resume: the popped interaction result).
    assert_eq!(
        child_log.resume_tags(),
        vec![RequirementKindTag::Interaction]
    );
    // The parent was resumed with the driven subagent's output.
    assert_eq!(parent_log.resume_tags(), vec![RequirementKindTag::Subagent]);
    // The handler derived + spawned + summarized exactly one child.
    assert_eq!(spawner.ids_calls(), 1);
    assert_eq!(spawner.spawn_calls(), 1);
    assert_eq!(spawner.summarize_calls(), 1);
}

/// §7.2: a context already at `max_depth` is refused before any derivation or
/// spawn, with a classified [`AgentError::SubagentDepthExceeded`].
#[tokio::test]
async fn depth_guard_refuses_at_limit_without_spawning() {
    let ids = SeqIds::new();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child_factory(|| panic!("spawn must not run once the depth guard trips"))
            .summary("unused")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(1);

    // A depth-1 context invoked against a max_depth of 1 must be refused.
    let root = root_context(&ids);
    let deep_ctx = root
        .derive_child(ids.run_id(), ids.trace_node("depth-1"))
        .expect("derive depth-1 context");
    assert_eq!(deep_ctx.depth(), 1);

    let empty = TestScope::empty();
    let mut outer = ScopePop::new(&empty, None);

    let result = handler
        .fulfill(
            &AgentSpecRef(ids.agent_id()),
            &Interaction::question(ids.step_id(), "brief".to_owned()),
            None,
            &mut outer,
            &deep_ctx,
        )
        .await;

    match result {
        RequirementResult::Subagent(Err(AgentError::SubagentDepthExceeded { limit, depth })) => {
            assert_eq!(limit, 1);
            assert_eq!(depth, 1);
        }
        other => panic!("expected a SubagentDepthExceeded result, got {other:?}"),
    }
    // The guard ran before any derivation or spawn.
    assert_eq!(spawner.ids_calls(), 0);
    assert_eq!(spawner.spawn_calls(), 0);
    assert_eq!(spawner.summarize_calls(), 0);
}

/// §7: a cancelled parent context propagates to the derived child, so the child
/// drain abandons (never-resumes) the child's first requirement without ever
/// invoking a child handler.
#[tokio::test]
async fn parent_cancel_propagates_and_abandons_child() {
    let ids = SeqIds::new();

    let child_machine = ScriptMachine::builder()
        .requirement(llm_requirement(&ids))
        .idle_on_abandon()
        .done_after_all_resumed()
        .label("child")
        .build();
    let child_log = Arc::clone(child_machine.log());
    // A PanicOnCall llm proves the child's LLM never runs: if the abandon path
    // ever dispatched it, the test would panic.
    let child = SpawnedChildBuilder::new()
        .machine(child_machine)
        .scope(
            headless_child_scope()
                .llm(Arc::new(PanicOnCall::with_message(
                    "child llm must not run",
                )))
                .build(),
        )
        .opening(user_input(&ids, "open child"))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("abandoned child")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(4);

    // Cancel the parent context before fulfilling; derivation inherits it.
    let ctx = root_context(&ids);
    ctx.cancellation().cancel();

    let empty = TestScope::empty();
    let mut outer = ScopePop::new(&empty, None);

    let result = handler
        .fulfill(
            &AgentSpecRef(ids.agent_id()),
            &Interaction::question(ids.step_id(), "brief".to_owned()),
            None,
            &mut outer,
            &ctx,
        )
        .await;

    // The turn closed (drain returned Ok) via the child's never-resume path.
    assert!(matches!(result, RequirementResult::Subagent(Ok(_))));
    // The child's first requirement was abandoned, never fulfilled or resumed.
    assert_eq!(child_log.abandon_count(), 1);
    assert_eq!(child_log.resume_count(), 0);
    // The child was derived + spawned, then abandoned (so it was summarized).
    assert_eq!(spawner.spawn_calls(), 1);
}

/// A child's token charge lands on the parent's shared budget ledger, proving
/// the child context is derived (budget ↕) rather than created fresh.
#[tokio::test]
async fn child_token_charge_counts_against_parent_budget() {
    const CHILD_TOKENS: u64 = 42;

    let ids = SeqIds::new();

    let child_machine = ScriptMachine::builder()
        .requirement(llm_requirement(&ids))
        .done_after_all_resumed()
        .label("child")
        .build();
    let child_log = Arc::clone(child_machine.log());
    let child = SpawnedChildBuilder::new()
        .machine(child_machine)
        .scope(
            headless_child_scope()
                .llm(Arc::new(ChargingLlmHandler {
                    tokens: CHILD_TOKENS,
                }))
                .build(),
        )
        .opening(user_input(&ids, "open child"))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("child charged tokens")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(4);

    let ctx = root_context(&ids);
    assert_eq!(ctx.budget().snapshot().used().tokens(), 0);

    let empty = TestScope::empty();
    let mut outer = ScopePop::new(&empty, None);

    let result = handler
        .fulfill(
            &AgentSpecRef(ids.agent_id()),
            &Interaction::question(ids.step_id(), "brief".to_owned()),
            None,
            &mut outer,
            &ctx,
        )
        .await;

    assert!(matches!(result, RequirementResult::Subagent(Ok(_))));
    // The child fulfilled its one LLM requirement and completed.
    assert_eq!(child_log.resume_tags(), vec![RequirementKindTag::Llm]);
    // The child's charge is visible on the parent context's shared ledger.
    assert_eq!(ctx.budget().snapshot().used().tokens(), CHILD_TOKENS);
}

/// An attended child answers its own `NeedInteraction` in place, so it never
/// pops to the parent: the parent's interaction backend (a `PanicOnCall`) is
/// therefore never invoked.
#[tokio::test]
async fn attended_child_resolves_its_interaction_in_place() {
    let ids = SeqIds::new();

    let child_interaction = Arc::new(ScriptedInteractionHandler::fixed(
        InteractionDecision::Answer("child self-serves".to_owned()),
    ));
    let child_interaction_log = Arc::clone(child_interaction.log());
    let child_machine = ScriptMachine::builder()
        .requirement(interaction_requirement(&ids, "child needs a human"))
        .done_after_all_resumed()
        .label("child")
        .build();
    let child_log = Arc::clone(child_machine.log());
    let child = SpawnedChildBuilder::new()
        .machine(child_machine)
        .scope(attended_child_scope(child_interaction).build())
        .opening(user_input(&ids, "open child"))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("attended child")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(4);

    // The parent serves the subagent; its interaction backend must never fire,
    // because an attended child resolves its own interaction in place.
    let parent_scope = parent_scope_with_subagent(handler)
        .attended(Arc::new(PanicOnCall::with_message(
            "parent must not serve an attended child's interaction",
        )))
        .build();

    let spec_ref = AgentSpecRef(ids.agent_id());
    let brief = Interaction::question(ids.step_id(), "delegate".to_owned());
    let mut parent = ScriptMachine::builder()
        .requirement(subagent_requirement(&ids, spec_ref, brief))
        .done_after_all_resumed()
        .label("parent")
        .build();
    let ctx = root_context(&ids);

    let done = drain(
        &mut parent,
        user_input(&ids, "go"),
        &parent_scope,
        None,
        &ctx,
    )
    .await
    .expect("parent drain completes");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    // The child served its own interaction; the parent's backend never fired.
    assert_eq!(child_interaction_log.len(), 1);
    assert_eq!(
        child_log.resume_tags(),
        vec![RequirementKindTag::Interaction]
    );
}
