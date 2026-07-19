//! End-to-end acceptance: attended parent + headless child are one graph.
//!
//! This is the migration acceptance example (migration doc §1 / §4.4 / §7). It
//! proves, against the public crate API and fully offline effect fakes, that
//! "attended" and "unattended/headless" are not two agent modes but two *wirings
//! of the same graph*: the identical child subagent spec resolves an approval
//! **in place** when its own scope carries an interaction backend, and **pops the
//! very same approval to its attended parent** when its scope omits one — with no
//! change to the child itself.
//!
//! Since milestone 6 this suite is built entirely on [`agent_testkit`]: the child
//! is a real [`DefaultAgentMachine`] driven by scripted effect handlers
//! ([`ScriptedLlmHandler`], [`ScriptedToolRegistry`]) through a tool round-trip
//! guarded by a require-approval policy. The parent is a [`ScriptMachine`] that
//! emits a `NeedTool` and a `NeedSubagent` in one turn, so the run exercises a
//! tool and a subagent together. The reference [`drain`] driver +
//! [`ScriptedSubagentSpawner`] / [`DrivingSubagentHandler`] provide the mechanism,
//! and every observation is read back from a scripted handler's [`CallLog`]
//! rather than a bespoke counter.
//!
//! Coverage (one focused `#[tokio::test]` each):
//!
//! 1. `attended_parent_serves_headless_child_via_pop` — a headless child's
//!    approval pops to the attended parent's policy backend and is granted, the
//!    guarded tool then runs, and the child's token charges aggregate onto the
//!    parent's shared budget ledger (pop routing + hierarchy + budget).
//! 2. `same_child_spec_attended_resolves_in_place` — the *same* child spec, given
//!    an interaction backend on its own scope, resolves the approval locally with
//!    the same committed conversation (run mode = scope wiring).
//! 3. `batch_requirements_are_fulfilled_concurrently` — a single step's batch of
//!    tool requirements is fulfilled concurrently, not serially (decision B).
//! 4. `parent_cancel_propagates_and_abandons_child` — a cancelled parent context
//!    propagates into the derived child, which abandons its first requirement
//!    (never-resume) without performing any IO.

use agent_testkit::prelude::*;

use agent_lib::agent::{
    AgentInput, ApprovalRequirement, LlmHandler, LoopCursorKind, RequirementResult, RunContext,
    ScopePop, SubagentHandler, ToolApprovalPolicy, ToolRegistryHandler, drain,
};
use agent_lib::client::ChatRequest;
use agent_lib::conversation::ToolCallId;
use agent_lib::model::{
    content::ContentBlock,
    message::{Message, Role},
    tool::ToolCall,
};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

// ----- host handlers kept local to the acceptance -----

/// An [`LlmHandler`] that delegates to an inner scripted handler and records the
/// response usage it observed.
///
/// Driver-level budget charging happens in `drain` before the response is
/// resumed into the child machine. This wrapper only gives the assertion a
/// handler-side log of what the scripted child consumed.
struct ObservingLlmHandler {
    inner: Arc<dyn LlmHandler>,
    charged: Arc<AtomicU64>,
}

impl ObservingLlmHandler {
    fn new(inner: Arc<dyn LlmHandler>, charged: Arc<AtomicU64>) -> Self {
        Self { inner, charged }
    }
}

#[async_trait]
impl LlmHandler for ObservingLlmHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        let result = self.inner.fulfill(request, mode, ctx).await;
        if let RequirementResult::Llm(Ok(response)) = &result {
            let tokens = u64::from(response.usage.input) + u64::from(response.usage.output);
            self.charged.fetch_add(tokens, Ordering::SeqCst);
        }
        result
    }
}

/// Approval policy that requires human approval for every tool call.
///
/// `agent-testkit` deliberately does not ship a require-approval policy — the
/// approval *policy* is a spec-level decision, not a mockable effect boundary —
/// so the acceptance carries this minimal one to force the child's `NeedTool`
/// through a `NeedInteraction`.
#[derive(Debug)]
struct RequireApprovalPolicy {
    reason: Option<String>,
}

impl RequireApprovalPolicy {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
        }
    }
}

impl ToolApprovalPolicy for RequireApprovalPolicy {
    fn approval_requirement(&self, _call_id: ToolCallId, _call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::required(self.reason.clone())
    }
}

// ----- shared fixtures -----

/// Total usage of the scripted guarded weather round-trip: (5 + 2) + (7 + 4).
const APPROVAL_ROUND_TRIP_TOKENS: u64 = 18;

/// The opening user turn every child machine starts from.
fn child_opening(ids: &SeqIds) -> AgentInput {
    user_input(ids, "Use get_weather for Shanghai, then answer from it.")
}

/// The child machine: a real [`DefaultAgentMachine`] that runs a guarded weather
/// tool round-trip. The machine decides whether it asks for approval; the *scope*
/// (not the machine) decides who answers it.
fn require_approval_child(ids: &SeqIds) -> DefaultAgentMachine {
    default_machine(
        ids,
        agent_state(ids, agent_spec_with_tools(ids, vec![weather_tool()])),
    )
    .with_approval_policy(Arc::new(RequireApprovalPolicy::new(
        "human approval required",
    )))
}

/// Scripts a guarded weather round-trip: request the tool, then answer from its
/// result. Total usage is 7 + 11 = 18 tokens ([`APPROVAL_ROUND_TRIP_TOKENS`]).
fn approval_round_trip_llm() -> ScriptedLlmHandler {
    ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "call-weather",
            "get_weather",
            json!({ "city": "Shanghai" }),
        )])
        .with_usage(usage(5, 2)),
        LlmStep::response(assistant_text("sunny, per get_weather", usage(7, 4))),
    ])
}

/// A registry that declares `get_weather` and answers the guarded call once.
fn weather_registry() -> ScriptedToolRegistry {
    ScriptedToolRegistry::from_steps(
        vec![weather_tool()],
        [ToolStep::ok("call-weather", "Sunny")],
    )
}

/// Asserts a message carries exactly one text block equal to `expected`.
fn assert_text(message: &Message, expected: &str) {
    assert_eq!(message.content.len(), 1, "one content block");
    let ContentBlock::Text { text, .. } = &message.content[0] else {
        panic!("expected a text block");
    };
    assert_eq!(text, expected);
}

// ----- tests -----

/// A headless child's approval pops to the attended parent's policy backend and
/// is granted; the guarded tool then runs, and the child's token charges show up
/// on the parent's shared budget ledger. One turn, one tool, one subagent.
#[tokio::test]
async fn attended_parent_serves_headless_child_via_pop() {
    let ids = SeqIds::new();

    // The child: a real machine that requires approval for its weather call,
    // wired to scripted offline effect handlers captured for post-run assertions.
    let child_llm = approval_round_trip_llm();
    let child_llm_log = Arc::clone(child_llm.log());
    let child_charged = Arc::new(AtomicU64::new(0));
    let charging = ObservingLlmHandler::new(Arc::new(child_llm), Arc::clone(&child_charged));

    let child_registry = weather_registry();
    let child_registry_log = Arc::clone(child_registry.log());

    // Headless child scope: LLM + tool, *no* interaction backend, so the approval
    // pops outward to the attended parent.
    let child = SpawnedChildBuilder::new()
        .machine(require_approval_child(&ids))
        .scope(
            headless_child_scope()
                .llm(Arc::new(charging))
                .tool(Arc::new(ToolRegistryHandler::new(Arc::new(child_registry))))
                .build(),
        )
        .opening(child_opening(&ids))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("child looked up the weather")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(4);

    // The attended parent serves its own tool step, resolves the popped approval,
    // and derives the subagent.
    let parent_tool = ScriptedToolHandler::from_steps([ToolStep::ok("parent-note", "noted")]);
    let parent_tool_log = Arc::clone(parent_tool.log());
    let parent_interaction = ScriptedInteractionHandler::approve_all();
    let parent_interaction_log = Arc::clone(parent_interaction.log());
    let parent_scope = parent_scope_with_subagent(handler)
        .attended(Arc::new(parent_interaction))
        .tool(Arc::new(parent_tool))
        .build();

    // The parent emits one NeedTool (its own) and one NeedSubagent in a single
    // batch, then completes once both are resumed.
    let spec_ref = AgentSpecRef(ids.agent_id());
    let brief = Interaction::question(ids.step_id(), "look up the weather".to_owned());
    let mut parent = ScriptMachine::builder()
        .requirements([
            Requirement::at_root(
                ids.requirement_id(),
                RequirementKind::NeedTool {
                    call_id: ids.tool_call_id(),
                    call: tool_call("parent-note", "note", json!({ "text": "record progress" })),
                },
            ),
            Requirement::at_root(
                ids.requirement_id(),
                RequirementKind::NeedSubagent {
                    spec_ref,
                    brief,
                    result_schema: None,
                },
            ),
        ])
        .done_after_all_resumed()
        .label("parent")
        .build();
    let ctx = root_context(&ids);

    let done = drain(
        &mut parent,
        user_input(&ids, "delegate the weather lookup"),
        &parent_scope,
        None,
        &ctx,
    )
    .await
    .expect("parent turn drains to completion");

    // The whole turn closed on the parent.
    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);

    // The parent served its own tool step exactly once.
    assert_eq!(parent_tool_log.len(), 1);

    // The child's approval popped to the attended parent, which answered it once;
    // only because it was granted did the guarded weather tool run in the child.
    assert_eq!(parent_interaction_log.len(), 1);
    assert_eq!(child_registry_log.len(), 1);
    assert_eq!(child_llm_log.len(), 2);

    // Budget aggregation: the child's token charges (18) land on the parent's
    // shared ledger via the derived child context.
    assert_eq!(
        child_charged.load(Ordering::SeqCst),
        APPROVAL_ROUND_TRIP_TOKENS
    );
    assert_eq!(
        ctx.budget().snapshot().used().tokens(),
        APPROVAL_ROUND_TRIP_TOKENS
    );
}

/// The *same* child spec, given an interaction backend on its own scope, resolves
/// the approval in place — no parent, no pop — and commits the same conversation.
/// This is the "run mode = scope wiring" half of the acceptance.
#[tokio::test]
async fn same_child_spec_attended_resolves_in_place() {
    let ids = SeqIds::new();

    let child_llm = approval_round_trip_llm();
    let child_registry = weather_registry();
    let child_registry_log = Arc::clone(child_registry.log());
    let child_interaction = ScriptedInteractionHandler::approve_all();
    let child_interaction_log = Arc::clone(child_interaction.log());

    // Identical child machine to the headless case; only the scope changes.
    let mut child = require_approval_child(&ids);
    let attended_scope = attended_child_scope(Arc::new(child_interaction))
        .llm(Arc::new(child_llm))
        .tool(Arc::new(ToolRegistryHandler::new(Arc::new(child_registry))))
        .build();
    let ctx = root_context(&ids);

    let done = drain(&mut child, child_opening(&ids), &attended_scope, None, &ctx)
        .await
        .expect("attended child turn drains to completion");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);

    // Served locally, exactly once, and the guarded tool ran.
    assert_eq!(child_interaction_log.len(), 1);
    assert_eq!(child_registry_log.len(), 1);

    // The committed conversation: user, assistant tool-use, tool result, answer.
    let conversation = child.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let messages = conversation.turns()[0].messages();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].payload().role, Role::User);
    assert_eq!(messages[1].payload().role, Role::Assistant);
    assert_eq!(messages[2].payload().role, Role::Tool);
    assert_text(messages[3].payload(), "sunny, per get_weather");
}

/// A single step's batch of tool requirements is fulfilled concurrently: the peak
/// number of in-flight tool calls exceeds one. Serial fulfillment would pin it at
/// one. This is migration decision B, observed end to end on a real machine.
#[tokio::test]
async fn batch_requirements_are_fulfilled_concurrently() {
    let ids = SeqIds::new();

    // The model asks for two tool calls at once; no approval (the default
    // policy), so both become a concurrent NeedTool batch.
    let llm = ScriptedLlmHandler::from_steps([
        LlmStep::tool_use(vec![
            tool_call("call-a", "get_weather", json!({ "city": "Shanghai" })),
            tool_call("call-b", "get_weather", json!({ "city": "Osaka" })),
        ])
        .with_usage(usage(6, 3)),
        LlmStep::text("both looked up").with_usage(usage(4, 2)),
    ]);

    // A scripted tool handler completes each call in a single poll; the injected
    // delay opens a window in which co-scheduled siblings overlap, and the wrapper
    // records the peak in-flight count. Serial fulfillment would keep it at 1.
    let tool = ScriptedToolHandler::from_steps([
        ToolStep::ok("call-a", "Sunny"),
        ToolStep::ok("call-b", "Cloudy"),
    ]);
    let tool_log = Arc::clone(tool.log());
    let delaying = Arc::new(DelayingToolHandler::with_delay(tool, Delay::yields(2)));
    let peak = Arc::clone(&delaying);

    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    );
    let scope = TestScope::builder()
        .llm(Arc::new(llm))
        .tool(delaying)
        .build();
    let ctx = root_context(&ids);

    let done = drain(&mut machine, child_opening(&ids), &scope, None, &ctx)
        .await
        .expect("parallel tool turn drains to completion");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(tool_log.len(), 2);
    assert_eq!(
        peak.peak_concurrency(),
        2,
        "the two-call batch was fulfilled concurrently, not serially"
    );
}

/// A cancelled parent context propagates into the derived child: the child drain
/// abandons its first requirement (never-resume) without performing any IO, and
/// nothing is charged to the shared budget.
#[tokio::test]
async fn parent_cancel_propagates_and_abandons_child() {
    let ids = SeqIds::new();

    let child_llm = approval_round_trip_llm();
    let child_llm_log = Arc::clone(child_llm.log());
    let child_charged = Arc::new(AtomicU64::new(0));
    let charging = ObservingLlmHandler::new(Arc::new(child_llm), Arc::clone(&child_charged));

    let child_registry = weather_registry();
    let child_registry_log = Arc::clone(child_registry.log());

    let child = SpawnedChildBuilder::new()
        .machine(require_approval_child(&ids))
        .scope(
            headless_child_scope()
                .llm(Arc::new(charging))
                .tool(Arc::new(ToolRegistryHandler::new(Arc::new(child_registry))))
                .build(),
        )
        .opening(child_opening(&ids))
        .build();

    let spawner = Arc::new(
        ScriptedSubagentSpawner::builder(ids.clone())
            .child(child)
            .summary("child was cancelled")
            .build(),
    );
    let handler = Arc::clone(&spawner).into_handler(4);

    let ctx = root_context(&ids);
    ctx.cancellation().cancel();

    let outer_scope = TestScope::empty();
    let mut outer = ScopePop::new(&outer_scope, None);

    let result = handler
        .fulfill(
            &AgentSpecRef(ids.agent_id()),
            &Interaction::question(ids.step_id(), "look up the weather".to_owned()),
            None,
            &mut outer,
            &ctx,
        )
        .await;

    // The child turn closed through its never-resume path.
    assert!(matches!(result, RequirementResult::Subagent(Ok(_))));

    // No IO happened: the guarded tool never ran, the model was never called, and
    // nothing was charged to the shared ledger.
    assert_eq!(child_registry_log.len(), 0);
    assert_eq!(child_llm_log.len(), 0);
    assert_eq!(child_charged.load(Ordering::SeqCst), 0);
    assert_eq!(ctx.budget().snapshot().used().tokens(), 0);
}
