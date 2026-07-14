//! The primary complex agent-effect flow (milestone 2, M2-1).
//!
//! `docs/complex-tests.md` §4.1 P0-1 is the highest-value combined scenario: a
//! single turn that threads a plan dependency graph, an append-only blackboard
//! post, a dangerous-tool approval that is *granted*, a mid-turn human pivot at
//! the post-tool boundary, a second dangerous-tool approval that is *denied*, and
//! a final model answer that closes the turn.
//!
//! The turn is driven by hand through a [`StepHarness`] rather than a
//! [`DrainHarness`](agent_testkit::harness::DrainHarness): the pivot has to land
//! at a legal streaming-step boundary (right after a tool result, before the next
//! LLM step), which is only observable when the harness stops at every blocking
//! point. Every tool and approval requirement the machine emits is fulfilled at
//! the effect boundary by the milestone-1 [`ComplexToolHandler`] and a scripted
//! interaction backend, so the mock plan/blackboard store and the handler/
//! interaction logs record exactly what a real registry would.
//!
//! Alongside the combined happy-path turn, milestone 2 (M2-2) pins the two error
//! faces that flow must never silently swallow: a dependency-blocked plan claim
//! surfaces as a model-visible [`ToolStatus::Error`] without mutating the task,
//! and a denied dangerous write never reaches the tool at the effect boundary.
//!
//! Run in isolation with `cargo test --test agent_complex_flow`.

#[path = "complex_support/mod.rs"]
mod complex_support;

use std::sync::Arc;

use agent_lib::agent::{
    ApprovalDecision, InteractionHandler, InteractionResponse, LoopCursorKind, PlanId, Requirement,
    RequirementKind, RequirementResult, RunContext, ToolHandler,
};
use agent_lib::model::content::ContentBlock;
use agent_lib::model::message::{Message, Role};
use agent_lib::model::tool::{ToolResponse, ToolStatus};

use agent_testkit::assertions::{assert_conversation, assert_done};
use agent_testkit::fixtures::{assistant_text, assistant_tool_use, root_context, tool_call, usage};
use agent_testkit::handlers::{
    InteractionCallLog, InteractionDecision, ScriptedInteractionHandler, ScriptedLlmHandler,
};
use agent_testkit::harness::{DrainHarness, StepHarness, StepObservation};
use agent_testkit::ids::SeqIds;
use agent_testkit::script::LlmStep;

use complex_support::assertions::{
    assert_board_messages, assert_interaction_decisions, assert_no_task_owner,
    assert_pivot_after_tool_result, assert_task_depends_on, assert_task_status,
    assert_tool_executions, role_sequence,
};
use complex_support::plan_blackboard::{MockPlanBlackboardStore, StoreError, TaskStatus};
use complex_support::tools::{
    BLACKBOARD_POST, ComplexToolHandler, DANGEROUS_WRITE, PLAN_ADD_TASK, PLAN_CLAIM, PLAN_CREATE,
    complex_agent_machine, complex_scope, complex_tool_handler,
};

/// Fixed plan id so store construction stays deterministic and offline.
fn plan_id() -> PlanId {
    PlanId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890b2").expect("valid plan id")
}

/// The human pivot injected mid-turn, right after the first dangerous write.
const PIVOT_TEXT: &str = "先不要改文件,只给方案";

/// Fulfils a `NeedTool` requirement through the complex tool handler, mutating
/// the shared store and recording the invocation, then returns the tool-family
/// result the harness resumes with.
async fn fulfill_tool(
    handler: &ComplexToolHandler,
    ctx: &RunContext,
    requirement: &Requirement,
) -> RequirementResult {
    match &requirement.kind {
        RequirementKind::NeedTool { call_id, call } => handler.fulfill(*call_id, call, ctx).await,
        other => panic!("expected a NeedTool requirement, found {other:?}"),
    }
}

/// Fulfils a `NeedInteraction` (approval) requirement through the scripted
/// interaction backend, returning the interaction-family result.
async fn fulfill_interaction(
    interaction: &ScriptedInteractionHandler,
    ctx: &RunContext,
    requirement: &Requirement,
) -> RequirementResult {
    match &requirement.kind {
        RequirementKind::NeedInteraction { request } => interaction.fulfill(request, ctx).await,
        other => panic!("expected a NeedInteraction requirement, found {other:?}"),
    }
}

/// Concatenates every [`ContentBlock::Text`] payload of `message`.
fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Extracts the final LLM step's request messages, panicking with a family
/// diagnostic when the requirement is not a `NeedLlm`.
fn llm_request_messages(requirement: &Requirement) -> Vec<Message> {
    match &requirement.kind {
        RequirementKind::NeedLlm { request, .. } => request.messages.clone(),
        other => panic!("expected a NeedLlm requirement, found {other:?}"),
    }
}

/// Reports the approval decisions recorded on `log`, in dispatch order.
fn recorded_decisions(log: &InteractionCallLog) -> Vec<ApprovalDecision> {
    log.records()
        .into_iter()
        .map(|record| match record.result {
            Some(InteractionResponse::Approval(approval)) => approval.decision(),
            other => panic!("every complex-flow interaction is an approval, found {other:?}"),
        })
        .collect()
}

/// The P0-1 combined turn: plan dependency + blackboard + approve/deny + pivot.
///
/// Driven step by step so the pivot lands at the legal post-tool boundary. The
/// assertions pin every observable the scenario is meant to fix: the single
/// committed turn, the dependency graph and its unclaimable downstream task, the
/// monotonic blackboard side effects, the exactly-once dangerous execution, the
/// two ordered approval decisions, and the pivot text plus denied tool result in
/// the final model request.
#[tokio::test]
async fn complex_turn_combines_plan_blackboard_approval_deny_and_pivot() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);

    // The store starts empty; the model creates the plan through the tools.
    let store = Arc::new(MockPlanBlackboardStore::new(plan_id()));
    let handler = complex_tool_handler(Arc::clone(&store));

    // Only `dangerous_write` is gated; the first approval is granted, the second
    // denied.
    let interaction = ScriptedInteractionHandler::sequence([
        InteractionDecision::Approve,
        InteractionDecision::Deny(Some("keep it to a plan for now".to_owned())),
    ]);
    let interaction_log = Arc::clone(interaction.log());

    let machine = complex_agent_machine(&ids);
    let mut harness = StepHarness::with_ids(machine, ids);

    // 1. Open the turn. A fresh user turn parks on the opening NeedLlm.
    let llm_open = harness
        .user("实现功能 A")
        .single_llm()
        .expect("a fresh user turn opens on NeedLlm")
        .id;

    // 2. First model step: build the plan — create it, add `design`, then add
    //    `implement` depending on `design`. None are dangerous, so all three
    //    auto-approve into one NeedTool batch.
    let after_plan_llm = harness.resume(
        llm_open,
        RequirementResult::Llm(Ok(assistant_tool_use(
            vec![
                tool_call("c-plan-create", PLAN_CREATE, serde_json::json!({})),
                tool_call(
                    "c-add-design",
                    PLAN_ADD_TASK,
                    serde_json::json!({ "id": "design" }),
                ),
                tool_call(
                    "c-add-implement",
                    PLAN_ADD_TASK,
                    serde_json::json!({ "id": "implement", "depends_on": ["design"] }),
                ),
            ],
            usage(6, 4),
        ))),
    );

    // Fulfil the whole auto-approved batch in emission (model) order; the last
    // resume advances the machine to the next model step.
    let after_plan_tools = resume_tool_batch(&mut harness, &handler, &ctx, after_plan_llm).await;
    let plan_llm = after_plan_tools
        .single_llm()
        .expect("the drained plan-tool batch parks on the next NeedLlm")
        .id;

    // 3. Second model step: post an opening status to the blackboard (auto) and
    //    request the first dangerous write (gated). The auto post fires first as
    //    its own NeedTool batch.
    let after_second_llm = harness.resume(
        plan_llm,
        RequirementResult::Llm(Ok(assistant_tool_use(
            vec![
                tool_call(
                    "c-post-start",
                    BLACKBOARD_POST,
                    serde_json::json!({
                        "sender": "planner",
                        "text": "start processing feature A"
                    }),
                ),
                tool_call(
                    "c-danger-1",
                    DANGEROUS_WRITE,
                    serde_json::json!({ "text": "apply the risky change to file A" }),
                ),
            ],
            usage(7, 5),
        ))),
    );

    let post_start = after_second_llm
        .single_tool()
        .expect("the auto blackboard post parks on NeedTool")
        .clone();
    let after_post_start = harness.resume(
        post_start.id,
        fulfill_tool(&handler, &ctx, &post_start).await,
    );

    // The auto batch drained; the gated write now surfaces as an approval.
    let approval_one = after_post_start
        .single_interaction()
        .expect("the gated dangerous write parks on a NeedInteraction")
        .clone();
    let after_approve = harness.resume(
        approval_one.id,
        fulfill_interaction(&interaction, &ctx, &approval_one).await,
    );

    // Approval granted: the dangerous write is now a NeedTool. Run it.
    let danger_one = after_approve
        .single_tool()
        .expect("an approved dangerous write parks on NeedTool")
        .clone();
    let after_danger_one = harness.resume(
        danger_one.id,
        fulfill_tool(&handler, &ctx, &danger_one).await,
    );

    // 4. The first dangerous result closes the tool phase and parks on the next
    //    NeedLlm — the legal boundary to inject the human pivot.
    let pre_pivot_llm = after_danger_one
        .single_llm()
        .expect("the first dangerous write drains to the next NeedLlm")
        .id;
    let after_pivot = harness.pivot(PIVOT_TEXT);
    let pivot_llm = after_pivot
        .single_llm()
        .expect("the pivot re-renders the outstanding NeedLlm")
        .id;
    assert_eq!(
        pre_pivot_llm, pivot_llm,
        "a pivot re-renders the same LLM step under the same id"
    );

    // 5. Re-rendered model step: record the pivot on the blackboard (auto) and
    //    request a second dangerous write, which will be denied.
    let after_third_llm = harness.resume(
        pivot_llm,
        RequirementResult::Llm(Ok(assistant_tool_use(
            vec![
                tool_call(
                    "c-post-pivot",
                    BLACKBOARD_POST,
                    serde_json::json!({
                        "sender": "planner",
                        "text": "changed strategy after pivot: plan only, no file edits"
                    }),
                ),
                tool_call(
                    "c-danger-2",
                    DANGEROUS_WRITE,
                    serde_json::json!({ "text": "second risky change" }),
                ),
            ],
            usage(6, 4),
        ))),
    );

    let post_pivot = after_third_llm
        .single_tool()
        .expect("the post-pivot blackboard post parks on NeedTool")
        .clone();
    let after_post_pivot = harness.resume(
        post_pivot.id,
        fulfill_tool(&handler, &ctx, &post_pivot).await,
    );

    // The second gated write surfaces as an approval; deny it.
    let approval_two = after_post_pivot
        .single_interaction()
        .expect("the second dangerous write parks on a NeedInteraction")
        .clone();
    let after_deny = harness.resume(
        approval_two.id,
        fulfill_interaction(&interaction, &ctx, &approval_two).await,
    );

    // A denied approval never emits a NeedTool: the machine synthesizes a denied
    // result and drains to the final NeedLlm. Capture it before resuming so the
    // final model request is inspectable.
    let final_llm = after_deny
        .single_llm()
        .expect("a denied dangerous write drains to the final NeedLlm")
        .clone();
    let final_request = llm_request_messages(&final_llm);

    // 6. Final model answer closes the turn.
    let done = harness.resume(
        final_llm.id,
        RequirementResult::Llm(Ok(assistant_text("done, delivered the plan", usage(4, 3)))),
    );
    assert_eq!(
        done.cursor().kind(),
        LoopCursorKind::Done,
        "the final assistant text closes the turn"
    );

    // ----- assertions -------------------------------------------------------

    let machine = harness.into_machine();
    let conversation = machine.state().conversation();

    // Exactly one committed turn, nothing left pending.
    assert_eq!(
        conversation.turns().len(),
        1,
        "the whole scenario commits a single turn: {:?}",
        role_sequence(conversation, 0)
    );
    assert!(
        conversation.pending().is_none(),
        "the turn is fully committed with no pending frozen messages"
    );

    // The pivot user message lands after the first tool result, in turn order.
    assert_pivot_after_tool_result(conversation, PIVOT_TEXT);

    // Plan dependency graph: `implement` depends on `design`, and because
    // `design` never completed, `implement` cannot be claimed.
    assert_task_status(&store, "design", TaskStatus::Todo);
    assert_task_depends_on(&store, "implement", &["design"]);
    match store.claim("implement", "worker", store.version()) {
        Err(StoreError::DependencyBlocked { task, unfinished }) => {
            assert_eq!(task, "implement");
            assert_eq!(unfinished, vec!["design".to_owned()]);
        }
        other => panic!(
            "claiming `implement` before `design` completes must be dependency-blocked, got \
             {other:?}\nstore operations:\n{}",
            store.ops_summary()
        ),
    }

    // Blackboard: monotonic, non-duplicated side effects — the opening status,
    // the one approved dangerous write, and the post-pivot strategy change.
    assert_board_messages(
        &store,
        &[
            "start processing feature A",
            "apply the risky change to file A",
            "changed strategy after pivot",
        ],
    );

    // The dangerous tool executed exactly once: the granted write ran, the denied
    // one never did.
    assert_tool_executions(&handler, DANGEROUS_WRITE, 1);

    // Two approval decisions were rendered, in order: approve then deny.
    assert_interaction_decisions(&interaction_log, 2);
    assert_eq!(
        recorded_decisions(&interaction_log),
        vec![ApprovalDecision::Approve, ApprovalDecision::Deny],
        "the two approvals resolve approve-then-deny"
    );

    // The final model request carries the pivot text and the denied tool result.
    assert!(
        final_request
            .iter()
            .any(|message| message.role == Role::User && message_text(message).contains(PIVOT_TEXT)),
        "the final LLM request must include the pivot user message"
    );
    assert!(
        final_request
            .iter()
            .any(|message| message.content.iter().any(|block| matches!(
                block,
                ContentBlock::ToolResult {
                    status: ToolStatus::Denied,
                    ..
                }
            ))),
        "the final LLM request must include the denied dangerous-write tool result"
    );
}

/// Fulfils every `NeedTool` requirement emitted in `observation`, in emission
/// order, returning the observation produced by the final resume.
///
/// A tool batch only advances the machine once its last member resolves, so the
/// intermediate resumes emit no new requirements; the returned observation is the
/// one that carries whatever the machine parked on after the batch drained.
async fn resume_tool_batch(
    harness: &mut StepHarness<agent_lib::agent::DefaultAgentMachine>,
    handler: &ComplexToolHandler,
    ctx: &RunContext,
    observation: StepObservation,
) -> StepObservation {
    let batch: Vec<Requirement> = observation.requirements().to_vec();
    assert!(
        !batch.is_empty(),
        "resume_tool_batch requires at least one requirement to fulfil"
    );
    let mut last = None;
    for requirement in &batch {
        let result = fulfill_tool(handler, ctx, requirement).await;
        last = Some(harness.resume(requirement.id, result));
    }
    last.expect("the batch had at least one requirement")
}

// ----- M2-2: negative / regression cases -----------------------------------

/// Concatenates every [`ContentBlock::Text`] payload of a tool response.
fn tool_response_text(response: &ToolResponse) -> String {
    response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A dependency-blocked claim is a model-visible tool error, not a silent
/// success or a panic, and it must leave the target task untouched.
///
/// This pins the plan-dependency error face directly at the
/// [`ComplexToolHandler`] boundary: with `implement` depending on an unfinished
/// `design`, claiming `implement` returns a [`ToolStatus::Error`] response whose
/// text names the blocking dependency, and `implement` stays `Todo`, unowned,
/// with the plan version unchanged. A future handler or store change that swallowed
/// the dependency check (claiming the task anyway, or hiding the error) would trip
/// one of these assertions.
#[tokio::test]
async fn claim_dependency_block_returns_tool_error_and_does_not_mutate_task() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);

    // Build the dependency graph directly: `implement` depends on `design`, and
    // `design` is never completed.
    let store = Arc::new(MockPlanBlackboardStore::new(plan_id()));
    store.create_plan();
    store
        .add_task("design", Vec::<String>::new())
        .expect("`design` is a fresh task");
    store
        .add_task("implement", ["design"])
        .expect("`implement` depends on the known task `design`");
    let version_before = store.version();

    let handler = complex_tool_handler(Arc::clone(&store));

    // The model tries to claim `implement` while `design` is still `Todo`.
    let call = tool_call(
        "c-claim-implement",
        PLAN_CLAIM,
        serde_json::json!({
            "task": "implement",
            "owner": "worker",
            "expected_version": version_before
        }),
    );
    let response = match handler.fulfill(ids.tool_call_id(), &call, &ctx).await {
        RequirementResult::Tool(Ok(response)) => response,
        other => panic!(
            "plan_claim must fulfil to a tool response, got {other:?}\nstore operations:\n{}",
            store.ops_summary()
        ),
    };

    // The dependency block surfaces as a model-visible tool error that names the
    // unfinished dependency — it is not silently downgraded to success.
    assert_eq!(
        response.status,
        ToolStatus::Error,
        "a dependency-blocked claim must be a tool error, got {:?}\nstore operations:\n{}",
        response.status,
        store.ops_summary()
    );
    let error_text = tool_response_text(&response);
    assert!(
        error_text.contains("design"),
        "the tool error must name the blocking dependency `design`, got {error_text:?}\nstore operations:\n{}",
        store.ops_summary()
    );

    // The rejected claim left the task untouched: still `Todo`, unowned, and the
    // plan version did not move.
    assert_task_status(&store, "implement", TaskStatus::Todo);
    assert_no_task_owner(&store, "implement");
    assert_eq!(
        store.version(),
        version_before,
        "a rejected claim must not bump the plan version\nstore operations:\n{}",
        store.ops_summary()
    );
}

/// A denied dangerous write never reaches the tool at the effect boundary.
///
/// The guarded round-trip is driven end to end through a [`DrainHarness`]: the
/// model requests [`DANGEROUS_WRITE`], the approval is denied, the machine
/// synthesizes a [`ToolStatus::Denied`] result, and the model's follow-up closes
/// the turn. The regression guard is [`assert_tool_executions`] reporting zero
/// dangerous executions — the [`ComplexToolHandler`] log stays empty, proving the
/// denied write never ran and never touched the shared store.
#[tokio::test]
async fn denied_dangerous_write_does_not_execute_tool() {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);

    let store = Arc::new(MockPlanBlackboardStore::new(plan_id()));
    let handler = complex_tool_handler(Arc::clone(&store));

    let interaction =
        ScriptedInteractionHandler::deny_all(Some("keep it to a plan for now".to_owned()));
    let interaction_log = Arc::clone(interaction.log());

    // The model asks for the gated write, then answers once the (synthesized)
    // denied result returns.
    let llm = ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "c-danger",
            DANGEROUS_WRITE,
            serde_json::json!({ "text": "apply the risky change to file A" }),
        )]),
        LlmStep::text("done, delivered the plan"),
    ]);

    let scope = complex_scope(
        Arc::new(llm),
        Arc::clone(&handler) as Arc<dyn ToolHandler>,
        Some(Arc::new(interaction) as Arc<dyn InteractionHandler>),
    );

    let machine = complex_agent_machine(&ids);
    let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
    let observed = harness
        .run_user("实现功能 A")
        .await
        .expect("the denied dangerous-write turn drains to completion");

    assert_done(observed.turn_done());
    // Exactly one approval was rendered, and it denied the write.
    assert_interaction_decisions(&interaction_log, 1);
    // The dangerous tool never executed: its handler log is empty.
    assert_tool_executions(&handler, DANGEROUS_WRITE, 0);
    // The denied write left the shared store untouched.
    assert_board_messages(&store, &[]);

    let machine = harness.into_machine();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .tool_result_status("c-danger", ToolStatus::Denied)
        .last_assistant_text("done, delivered the plan");
}
