//! Behavioural coverage: the reference driver drains a machine through a turn.
//!
//! Each test drives a [`DefaultAgentMachine`](agent_lib::agent::DefaultAgentMachine)
//! through [`drive_turn`]/[`drain`] over a single
//! [`ReferenceScope`] and asserts the committed
//! [`Conversation`](agent_lib::conversation::Conversation) and the drained
//! [`Notification`] sequence. These are the canonical turn-level coverage for the
//! sans-io path (migration doc §10, stage 2); the `*_matches_default_loop` names
//! preserve the behaviour the removed self-driving loop used to guarantee.
//!
//! ## Migrated to `agent-testkit` (milestone 6)
//!
//! This suite used to live as an in-crate unit test module
//! (`src/agent/drive/reference/tests.rs`) with a full set of hand-written fakes.
//! It now lives here, at the integration-test layer, and is built on
//! [`agent_testkit`]. The move is required, not cosmetic: `agent-testkit`
//! depends on `agent-lib` and is wired back in as a dev-dependency, so a unit
//! test compiled *inside* `agent-lib` sees two distinct instances of the crate
//! (the test-cfg build and the plain build the kit links) and testkit-produced
//! types will not unify with `crate::` types. An integration test links a single
//! plain `agent-lib`, so the kit's fixtures, scripted handlers, and scopes drop
//! straight in (the same seam milestone 6 established for `agent_effect_e2e`).
//!
//! Ids come from [`SeqIds`], payloads from the kit fixtures, the tool backend
//! from [`ScriptedToolRegistry`], cancellation from [`CancelOnCall`]/[`PanicOnCall`],
//! and the deny dispositions from [`ScriptedInteractionHandler`]. The only local
//! scaffolding retained is deliberately reference-specific rather than a mockable
//! effect boundary:
//!
//! - [`ScriptedLlmClient`]: `ReferenceScope` wraps a real
//!   [`LlmClient`](agent_lib::client::LlmClient), and the kit intentionally never
//!   mocks one (it scripts the `LlmHandler` seam instead). This thin adapter
//!   drives a testkit [`Script`] of [`LlmStep`]s and records every request in a
//!   testkit [`LlmCallLog`], so the script and call log still come from the kit.
//! - [`RequireApprovalPolicy`]: an approval *policy* is a spec-level decision, not
//!   an effect the kit mocks, so a require-approval policy is kept local (as in
//!   the `agent_effect_e2e` migration).
//! - `assert_text` / `assert_tool_result`: single-block payload assertions.

use agent_testkit::prelude::*;

use agent_lib::agent::ApprovalInteractionHandler;
use agent_lib::agent::{
    AgentError, AgentErrorKind, ApprovalRequirement, LoopCursor, LoopCursorKind, Notification,
    ReconfigRequest, ReferenceScope, RequirementKindTag, RequirementResult,
    StaticToolRegistryResolver, ToolApprovalPolicy, ToolSetRef, drain, drive_turn,
};
use agent_lib::client::{Capability, ChatRequest, ClientError, LlmClient, Response};
use agent_lib::conversation::ToolCallId;
use agent_lib::model::{
    content::ContentBlock,
    message::{Message, Role},
    tool::{ToolCall, ToolStatus},
};
use agent_lib::stream::StreamEvent;
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use serde_json::json;
use std::sync::Arc;

// ----- retained, reference-specific scaffolding (not mockable effects) -----

/// A minimal [`LlmClient`] backed by a testkit [`Script`] of [`LlmStep`]s.
///
/// `ReferenceScope` requires an [`LlmClient`], which the kit does not provide (it
/// scripts the `LlmHandler` seam directly). This adapter keeps the reference
/// `LlmClientHandler` wrapping under test while sourcing both its scripted
/// responses and its observable call log from the kit.
struct ScriptedLlmClient {
    capability: Capability,
    script: Arc<Script<LlmStep>>,
    log: Arc<LlmCallLog>,
}

impl ScriptedLlmClient {
    fn from_steps(steps: impl IntoIterator<Item = LlmStep>) -> Self {
        Self {
            capability: Capability::default(),
            script: Arc::new(Script::new(steps)),
            log: Arc::new(CallLog::new()),
        }
    }

    fn request_count(&self) -> usize {
        self.log.len()
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.log.requests()
    }
}

#[async_trait]
impl LlmClient for ScriptedLlmClient {
    fn capability(&self) -> &Capability {
        &self.capability
    }

    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        let ticket = self.log.begin(request);
        let result = match self.script.next_step() {
            Ok(step) => step.into_result(),
            Err(error) => RequirementResult::Llm(Err(ClientError::Other(error.to_string()))),
        };
        self.log.complete(ticket, result.clone());
        match result {
            RequirementResult::Llm(outcome) => outcome,
            other => unreachable!("llm script yielded a non-llm result: {other:?}"),
        }
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        Ok(stream::iter(Vec::<Result<StreamEvent, ClientError>>::new()).boxed())
    }
}

/// Approval policy that requires approval for every tool call.
///
/// Approval *policy* is a spec-level decision rather than an effect boundary the
/// kit mocks, so it stays local (mirroring the `agent_effect_e2e` migration).
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

// ----- payload assertion helpers -----

fn assert_text(message: &Message, expected: &str) {
    assert_eq!(message.content.len(), 1);
    let ContentBlock::Text { text, .. } = &message.content[0] else {
        panic!("expected text content");
    };
    assert_eq!(text, expected);
}

fn assert_tool_result(message: &Message, expected_call_id: &str, expected_status: ToolStatus) {
    assert_eq!(message.role, Role::Tool);
    assert_eq!(message.content.len(), 1);
    let ContentBlock::ToolResult {
        tool_use_id,
        status,
        ..
    } = &message.content[0]
    else {
        panic!("expected tool result content");
    };
    assert_eq!(tool_use_id, expected_call_id);
    assert_eq!(*status, expected_status);
}

/// Builds a registry declaring `get_weather` with no scripted executions, for
/// turns whose tool is never reached (text-only, denied, idle reconfig).
fn idle_registry() -> Arc<ScriptedToolRegistry> {
    Arc::new(ScriptedToolRegistry::from_steps(
        vec![weather_tool()],
        Vec::<ToolStep>::new(),
    ))
}

// ----- equivalence tests -----

#[tokio::test]
async fn reference_text_only_matches_default_loop() {
    let ids = SeqIds::new();
    let client = Arc::new(ScriptedLlmClient::from_steps([
        LlmStep::text("hi").with_usage(usage(3, 5))
    ]));
    let registry = idle_registry();
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    );
    let scope = ReferenceScope::new(client.clone(), registry.clone());
    let ctx = root_context(&ids);

    let done = drive_turn(&mut machine, user_input(&ids, "hello"), &scope, &ctx)
        .await
        .expect("reference driver completes the text turn");

    // Terminal state: one committed turn, no pending, cursor Done.
    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    assert_eq!(machine.state().loop_cursor().kind(), LoopCursorKind::Done);
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 2);
    assert_text(turn.messages()[0].payload(), "hello");
    assert_text(turn.messages()[1].payload(), "hi");
    assert_eq!(turn.meta().usage(), &usage(3, 5));
    assert_eq!(conversation.version(), 1);

    // Notification sequence: exactly one StepBoundary (turn_count 1).
    let notifications = done.notifications();
    assert_eq!(notifications.len(), 1);
    let Notification::StepBoundary(boundary) = &notifications[0] else {
        panic!("only event is the step boundary");
    };
    assert_eq!(boundary.boundary().turn_count(), 1);

    assert_eq!(client.request_count(), 1);
    assert!(registry.log().is_empty());
}

/// A committed turn settles the cursor at `Done`, yet the same machine must
/// accept a follow-up user turn. Regression coverage for the multi-turn reuse
/// gap: feeding a second `UserMessage` into a machine whose cursor was left at
/// `Done` used to be rejected (no `Done -> Idle` transition), so the driver
/// returned the stale conversation without issuing a new LLM call. Both turns
/// must now commit distinct assistant replies and both must hit the client.
#[tokio::test]
async fn reference_consecutive_turns_reuse_the_same_machine() {
    let ids = SeqIds::new();
    let client = Arc::new(ScriptedLlmClient::from_steps([
        LlmStep::text("first reply").with_usage(usage(3, 5)),
        LlmStep::text("second reply").with_usage(usage(7, 11)),
    ]));
    let registry = idle_registry();
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    );
    let scope = ReferenceScope::new(client.clone(), registry.clone());

    let ctx = root_context(&ids);
    let done = drive_turn(&mut machine, user_input(&ids, "hello"), &scope, &ctx)
        .await
        .expect("first turn commits");
    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    assert_eq!(machine.state().loop_cursor().kind(), LoopCursorKind::Done);

    // Second turn on the *same* machine, whose cursor is currently `Done`.
    let ctx2 = root_context(&ids);
    let done2 = drive_turn(&mut machine, user_input(&ids, "again"), &scope, &ctx2)
        .await
        .expect("second turn commits on the reused machine");
    assert!(matches!(done2.cursor(), LoopCursor::Done(_)));

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 2);
    assert_text(
        conversation.turns()[0].messages()[1].payload(),
        "first reply",
    );
    assert_text(
        conversation.turns()[1].messages()[1].payload(),
        "second reply",
    );

    // The second turn genuinely called the client instead of replaying the
    // first response.
    assert_eq!(client.request_count(), 2);
}

#[tokio::test]
async fn reference_single_tool_matches_default_loop() {
    let ids = SeqIds::new();
    let client = Arc::new(ScriptedLlmClient::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "call-weather",
            "get_weather",
            json!({ "city": "Shanghai" }),
        )])
        .with_usage(usage(5, 2)),
        LlmStep::text("sunny in Shanghai").with_usage(usage(7, 4)),
    ]));
    let registry = Arc::new(ScriptedToolRegistry::from_steps(
        vec![weather_tool()],
        [ToolStep::ok("call-weather", "Sunny")],
    ));
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    );
    let scope = ReferenceScope::new(client.clone(), registry.clone());
    let ctx = root_context(&ids);

    let done = drive_turn(&mut machine, user_input(&ids, "hello"), &scope, &ctx)
        .await
        .expect("reference driver completes the tool turn");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 4);
    assert_text(turn.messages()[0].payload(), "hello");
    assert_eq!(turn.messages()[1].payload().role, Role::Assistant);
    assert_tool_result(turn.messages()[2].payload(), "call-weather", ToolStatus::Ok);
    assert_text(turn.messages()[3].payload(), "sunny in Shanghai");
    // The single pairing links the tool call to its committed result message.
    assert_eq!(turn.pairings().len(), 1);
    assert_eq!(turn.pairings()[0].result_msg(), turn.messages()[2].id());

    // ToolCallStarted, ToolCallFinished, tool StepBoundary, final StepBoundary.
    let notifications = done.notifications();
    assert_eq!(notifications.len(), 4);
    assert!(matches!(notifications[0], Notification::ToolCallStarted(_)));
    let Notification::ToolCallFinished(finished) = &notifications[1] else {
        panic!("second notification finishes the tool");
    };
    assert_eq!(finished.response().status, ToolStatus::Ok);
    let Notification::StepBoundary(tool_boundary) = &notifications[2] else {
        panic!("third notification is the tool step boundary");
    };
    assert_eq!(tool_boundary.boundary().turn_count(), 0);
    assert!(tool_boundary.metadata().is_empty());
    let Notification::StepBoundary(final_boundary) = &notifications[3] else {
        panic!("fourth notification is the final step boundary");
    };
    assert_eq!(final_boundary.boundary().turn_count(), 1);

    assert_eq!(client.request_count(), 2);
    assert_eq!(registry.log().len(), 1);
    assert_eq!(registry.log().requests()[0].name, "get_weather");
}

#[tokio::test]
async fn reference_parallel_tools_matches_default_loop() {
    let ids = SeqIds::new();
    let client = Arc::new(ScriptedLlmClient::from_steps([
        LlmStep::tool_use(vec![
            tool_call("call-a", "get_weather", json!({ "city": "Shanghai" })),
            tool_call("call-b", "get_weather", json!({ "city": "Tokyo" })),
        ])
        .with_usage(usage(8, 3)),
        LlmStep::text("both checked").with_usage(usage(9, 5)),
    ]));
    let registry = Arc::new(ScriptedToolRegistry::from_steps(
        vec![weather_tool()],
        [
            ToolStep::ok("call-a", "Sunny"),
            ToolStep::ok("call-b", "Rain"),
        ],
    ));
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    );
    let scope = ReferenceScope::new(client, registry.clone());
    let ctx = root_context(&ids);

    let done = drive_turn(&mut machine, user_input(&ids, "hello"), &scope, &ctx)
        .await
        .expect("reference driver completes the parallel tool turn");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    // Both tools start before either finishes, then two finishes.
    let notifications = done.notifications();
    assert!(matches!(notifications[0], Notification::ToolCallStarted(_)));
    assert!(matches!(notifications[1], Notification::ToolCallStarted(_)));
    assert!(matches!(
        notifications[2],
        Notification::ToolCallFinished(_)
    ));
    assert!(matches!(
        notifications[3],
        Notification::ToolCallFinished(_)
    ));

    let conversation = machine.state().conversation();
    let turn = &conversation.turns()[0];
    // Two pairings, each linked to one of the two committed tool result messages.
    assert_eq!(turn.pairings().len(), 2);
    let result_msgs: Vec<_> = turn.pairings().iter().map(|p| p.result_msg()).collect();
    assert!(result_msgs.contains(&turn.messages()[2].id()));
    assert!(result_msgs.contains(&turn.messages()[3].id()));
    assert_eq!(registry.log().len(), 2);
}

#[tokio::test]
async fn reference_tool_failure_self_heal_matches_default_loop() {
    let ids = SeqIds::new();
    let client = Arc::new(ScriptedLlmClient::from_steps([
        LlmStep::tool_use(vec![
            tool_call("call-denied", "get_weather", json!({ "city": "Private" })),
            tool_call("call-error", "get_weather", json!({ "city": "Nowhere" })),
        ])
        .with_usage(usage(8, 3)),
        LlmStep::text("I recovered from tool results").with_usage(usage(9, 5)),
    ]));
    let registry = Arc::new(ScriptedToolRegistry::from_steps(
        vec![weather_tool()],
        [
            ToolStep::response(tool_response(
                "call-denied",
                "policy denied",
                ToolStatus::Denied,
            )),
            ToolStep::runtime_error(agent_lib::agent::ToolRuntimeError::ExecutionFailed {
                tool_name: "get_weather".to_owned(),
                message: "backend unavailable".to_owned(),
            }),
        ],
    ));
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    );
    let scope = ReferenceScope::new(client, registry);
    let ctx = root_context(&ids);

    let done = drive_turn(&mut machine, user_input(&ids, "hello"), &scope, &ctx)
        .await
        .expect("tool failures are returned to the model");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    let finished_statuses = done
        .notifications()
        .iter()
        .filter_map(|event| match event {
            Notification::ToolCallFinished(finished) => Some(finished.response().status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        finished_statuses,
        vec![ToolStatus::Denied, ToolStatus::Error]
    );

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    let turn = &conversation.turns()[0];
    assert_tool_result(
        turn.messages()[2].payload(),
        "call-denied",
        ToolStatus::Denied,
    );
    assert_tool_result(
        turn.messages()[3].payload(),
        "call-error",
        ToolStatus::Error,
    );
    assert_text(
        turn.messages()[4].payload(),
        "I recovered from tool results",
    );
}

#[tokio::test]
async fn reference_approval_approve_matches_default_loop() {
    let ids = SeqIds::new();
    let client = Arc::new(ScriptedLlmClient::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "call-weather",
            "get_weather",
            json!({ "city": "Shanghai" }),
        )])
        .with_usage(usage(5, 2)),
        LlmStep::text("approved result used").with_usage(usage(7, 4)),
    ]));
    let registry = Arc::new(ScriptedToolRegistry::from_steps(
        vec![weather_tool()],
        [ToolStep::ok("call-weather", "Sunny")],
    ));
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    )
    .with_approval_policy(Arc::new(RequireApprovalPolicy::new(
        "human approval required",
    )));
    // The reference `ApprovalInteractionHandler` is the driver's own attended
    // backend (not a fake), so the approve path exercises the real component.
    let scope = ReferenceScope::new(client, registry.clone())
        .with_interaction(ApprovalInteractionHandler::approve());
    let ctx = root_context(&ids);

    let done = drive_turn(&mut machine, user_input(&ids, "hello"), &scope, &ctx)
        .await
        .expect("approved tool turn completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    // The approved call starts, finishes Ok, then two boundaries close the turn.
    let notifications = done.notifications();
    assert!(matches!(notifications[0], Notification::ToolCallStarted(_)));
    let Notification::ToolCallFinished(finished) = &notifications[1] else {
        panic!("approved tool finishes");
    };
    assert_eq!(finished.response().status, ToolStatus::Ok);
    assert!(matches!(notifications[2], Notification::StepBoundary(_)));
    assert!(matches!(notifications[3], Notification::StepBoundary(_)));

    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    let turn = &conversation.turns()[0];
    assert_tool_result(turn.messages()[2].payload(), "call-weather", ToolStatus::Ok);
    assert_text(turn.messages()[3].payload(), "approved result used");
    assert_eq!(registry.log().len(), 1);
}

#[tokio::test]
async fn reference_headless_scope_surfaces_unhandled_approval() {
    // Run mode = scope wiring (migration doc §4.4 / §6): the *same* machine the
    // approve test drives to completion under an attended scope instead surfaces a
    // classified `UnhandledRequirement` under a headless top-level scope with no
    // interaction backend — never a silent skip or hang, and the guarded tool
    // never runs.
    let ids = SeqIds::new();
    let client = Arc::new(ScriptedLlmClient::from_steps([LlmStep::tool_use(vec![
        tool_call("call-weather", "get_weather", json!({ "city": "Shanghai" })),
    ])
    .with_usage(usage(5, 2))]));
    let registry = Arc::new(ScriptedToolRegistry::from_steps(
        vec![weather_tool()],
        [ToolStep::ok("call-weather", "Sunny")],
    ));
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    )
    .with_approval_policy(Arc::new(RequireApprovalPolicy::new(
        "human approval required",
    )));
    // Headless: identical wiring to the approve test, minus the interaction backend.
    let scope = ReferenceScope::new(client, registry.clone());
    let ctx = root_context(&ids);

    let error = drive_turn(&mut machine, user_input(&ids, "hello"), &scope, &ctx)
        .await
        .expect_err("a headless top scope cannot fulfill the approval");

    assert_eq!(error.kind(), AgentErrorKind::UnhandledRequirement);
    match error {
        AgentError::UnhandledRequirement { kind, .. } => {
            assert_eq!(kind, RequirementKindTag::Interaction);
        }
        other => panic!("expected UnhandledRequirement, got {other:?}"),
    }
    // The approval was neither auto-granted nor skipped: the guarded tool never ran.
    assert!(registry.log().is_empty());
}

#[tokio::test]
async fn reference_approval_deny_matches_default_loop() {
    let ids = SeqIds::new();
    let client = Arc::new(ScriptedLlmClient::from_steps([
        LlmStep::tool_use(vec![
            tool_call("call-deny", "get_weather", json!({ "city": "Private" })),
            tool_call("call-timeout", "get_weather", json!({ "city": "Slow" })),
            tool_call("call-cancel", "get_weather", json!({ "city": "Cancelled" })),
        ])
        .with_usage(usage(8, 3)),
        LlmStep::text("handled approval decisions").with_usage(usage(9, 5)),
    ]));
    let registry = idle_registry();
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    )
    .with_approval_policy(Arc::new(RequireApprovalPolicy::new("approval required")));

    // A per-call disposition over the concurrent approval batch, in emission
    // order: deny, timeout, cancel. The reference `ReferenceScope` only takes the
    // fixed-disposition `ApprovalInteractionHandler`, so its llm/tool wiring is
    // reused through `TestScope::wrapping` while a scripted interaction backend
    // overrides the attended seam.
    let reference = Arc::new(ReferenceScope::new(client, registry.clone()));
    let interaction = Arc::new(ScriptedInteractionHandler::sequence([
        InteractionDecision::Deny(Some("denied by policy".to_owned())),
        InteractionDecision::Timeout(Some("approval timed out".to_owned())),
        InteractionDecision::Cancel(Some("cancelled by approver".to_owned())),
    ]));
    let scope = TestScope::builder()
        .wrapping(reference)
        .attended(interaction)
        .build();
    let ctx = root_context(&ids);

    let done = drain(&mut machine, user_input(&ids, "hello"), &scope, None, &ctx)
        .await
        .expect("loop recovers after denials");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    let finished_statuses = done
        .notifications()
        .iter()
        .filter_map(|event| match event {
            Notification::ToolCallFinished(finished) => Some(finished.response().status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        finished_statuses,
        vec![
            ToolStatus::Denied,
            ToolStatus::Denied,
            ToolStatus::Cancelled
        ]
    );

    let conversation = machine.state().conversation();
    let turn = &conversation.turns()[0];
    assert_tool_result(
        turn.messages()[2].payload(),
        "call-deny",
        ToolStatus::Denied,
    );
    assert_tool_result(
        turn.messages()[3].payload(),
        "call-timeout",
        ToolStatus::Denied,
    );
    assert_tool_result(
        turn.messages()[4].payload(),
        "call-cancel",
        ToolStatus::Cancelled,
    );
    assert_text(turn.messages()[5].payload(), "handled approval decisions");
    assert!(registry.log().is_empty());
}

// ----- cancellation -----

#[tokio::test]
async fn reference_cancel_during_tool_wait_abandons_turn() {
    let ids = SeqIds::new();
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    );
    // The tool handler cancels the context while the tool batch is in flight:
    // the call executes (an in-flight side effect cannot be un-run), but the
    // driver's post-batch re-check (M4-5) discards its resolution and settles
    // the requirement as a never-resume instead of resuming the machine with it.
    let scope = TestScope::builder()
        .llm(Arc::new(ScriptedLlmHandler::from_steps([
            LlmStep::tool_use(vec![tool_call(
                "call-weather",
                "get_weather",
                json!({ "city": "Shanghai" }),
            )])
            .with_usage(usage(5, 2)),
        ])))
        .tool(Arc::new(CancelOnCall::before(
            ScriptedToolHandler::from_steps([ToolStep::ok("call-weather", "sunny")]),
        )))
        .build();
    let ctx = root_context(&ids);

    let done = drain(&mut machine, user_input(&ids, "hello"), &scope, None, &ctx)
        .await
        .expect("a cancelled turn drains to a rest state");

    // The cancel outcome is distinguishable from a natural end (M4-5).
    assert!(done.cancelled());
    // Never-resume: the fulfilled tool batch is abandoned, the cursor settles to
    // a feedable Idle, and the pending turn is coherent (its tool_use closed by
    // a synthesized cancelled result) with nothing committed to history.
    assert!(matches!(done.cursor(), LoopCursor::Idle));
    assert_eq!(machine.state().loop_cursor().kind(), LoopCursorKind::Idle);
    let conversation = machine.state().conversation();
    let pending = conversation
        .pending()
        .expect("cancellation leaves a coherent pending turn");
    assert_eq!(pending.open_calls().count(), 0);
    assert_eq!(pending.tool_calls().len(), 1);
    assert!(conversation.turns().is_empty());

    // The turn never resumes: the tool's real result was discarded on the
    // never-resume path, so no tool-finished or step boundary was emitted.
    assert!(done.notifications().iter().all(|event| !matches!(
        event,
        Notification::ToolCallFinished(_) | Notification::StepBoundary(_)
    )));
}

#[tokio::test]
async fn reference_new_turn_after_cancel_starts_fresh() {
    let ids = SeqIds::new();
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    );
    let cancel_scope = TestScope::builder()
        .llm(Arc::new(CancelOnCall::before(
            ScriptedLlmHandler::from_steps([LlmStep::tool_use(vec![tool_call(
                "call-weather",
                "get_weather",
                json!({ "city": "Shanghai" }),
            )])
            .with_usage(usage(5, 2))]),
        )))
        .tool(Arc::new(PanicOnCall::new()))
        .build();
    let ctx = root_context(&ids);
    let _ = drain(
        &mut machine,
        user_input(&ids, "hello"),
        &cancel_scope,
        None,
        &ctx,
    )
    .await
    .expect("first turn is cancelled");
    assert!(matches!(machine.state().loop_cursor(), LoopCursor::Idle));

    // A fresh, uncancelled turn discards the interrupted pending and completes.
    let client = Arc::new(ScriptedLlmClient::from_steps([LlmStep::text(
        "hello again",
    )
    .with_usage(usage(3, 5))]));
    let scope = ReferenceScope::new(client, idle_registry());
    let fresh_ctx = root_context(&ids);

    let done = drive_turn(
        &mut machine,
        user_input(&ids, "hello again"),
        &scope,
        &fresh_ctx,
    )
    .await
    .expect("the follow-up turn completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 2);
    assert_text(turn.messages()[1].payload(), "hello again");
}

// ----- turn-boundary reconfiguration -----

/// A reconfiguration queued while the machine is idle is applied at the start of
/// the next turn: the driver resolves and installs the new registry through the
/// `ReconfigRegistryHandler`, and the opening request already reflects the new
/// tool set and system-prompt overlay. A start-of-turn application writes no
/// `reconfigs` boundary metadata (only a during-turn change does).
#[tokio::test]
async fn reference_idle_queued_reconfig_applies_at_next_turn_start() {
    let ids = SeqIds::new();
    let client = Arc::new(ScriptedLlmClient::from_steps([
        LlmStep::text("done").with_usage(usage(3, 5))
    ]));
    let registry = idle_registry();
    let mut machine = default_machine(
        &ids,
        agent_state(&ids, agent_spec_with_tools(&ids, vec![weather_tool()])),
    );
    let replacement = ToolSetRef::new(ids.tool_set_id(), vec![calendar_tool()]);

    // Queue the reconfiguration while idle, before the turn opens.
    machine
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("Use calendar context.".to_owned()),
            0,
        ))
        .expect("system overlay reconfig queued");
    machine
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("tool set reconfig queued");

    let scope = ReferenceScope::new(client.clone(), registry);
    let ctx = root_context(&ids);
    let done = drive_turn(&mut machine, user_input(&ids, "hello"), &scope, &ctx)
        .await
        .expect("the reconfigured turn completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    // The opening request already advertises the new tool set + overlay.
    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].tools, vec![calendar_tool()]);
    assert_eq!(
        requests[0].system.as_deref(),
        Some("Test conversation system.\n\nUse calendar context.")
    );

    // A start-of-turn application carries no reconfig boundary metadata.
    let boundaries: Vec<_> = done
        .notifications()
        .iter()
        .filter_map(|event| match event {
            Notification::StepBoundary(boundary) => Some(boundary),
            _ => None,
        })
        .collect();
    assert_eq!(boundaries.len(), 1);
    assert!(boundaries[0].metadata().get("reconfigs").is_none());

    // State reflects the applied reconfiguration for subsequent turns.
    assert!(machine.state().queued_reconfigs().is_empty());
    assert_eq!(
        machine.state().system_prompt_overlay(),
        Some("Use calendar context.")
    );
    assert_eq!(machine.state().current_tool_set(), &replacement);
}

/// A tool-set reconfiguration swaps the *executable* registry end-to-end: the
/// reference driver resolves the queued set through a
/// [`StaticToolRegistryResolver`], installs the resolved registry into the shared
/// slot, and the ensuing tool call runs against the new registry while the old
/// one is never touched.
#[tokio::test]
async fn reference_reconfig_swaps_executable_registry_end_to_end() {
    let ids = SeqIds::new();
    let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
    let initial_tool_set_id = spec.initial_tools().id();
    let client = Arc::new(ScriptedLlmClient::from_steps([
        LlmStep::tool_use(vec![tool_call(
            "call-cal",
            "get_calendar",
            json!({ "date": "Monday" }),
        )])
        .with_usage(usage(5, 2)),
        LlmStep::text("checked calendar").with_usage(usage(3, 5)),
    ]));
    let old_registry = Arc::new(ScriptedToolRegistry::from_steps(
        vec![weather_tool()],
        [ToolStep::ok("call-weather", "Sunny")],
    ));
    let new_registry = Arc::new(ScriptedToolRegistry::from_steps(
        vec![calendar_tool()],
        [ToolStep::ok("call-cal", "Free all day")],
    ));
    let old_log = Arc::clone(old_registry.log());
    let new_log = Arc::clone(new_registry.log());

    let replacement = ToolSetRef::new(ids.tool_set_id(), vec![calendar_tool()]);
    let mut resolver = StaticToolRegistryResolver::new();
    resolver
        .insert(initial_tool_set_id, old_registry.clone())
        .expect("initial registry inserted");
    resolver
        .insert(replacement.id(), new_registry.clone())
        .expect("replacement registry inserted");

    let mut machine = default_machine(&ids, agent_state(&ids, spec));
    machine
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("tool set reconfig queued while idle");

    let scope = ReferenceScope::new(client.clone(), old_registry.clone())
        .with_tool_registry_resolver(Arc::new(resolver));
    let ctx = root_context(&ids);
    let done = drive_turn(&mut machine, user_input(&ids, "hello"), &scope, &ctx)
        .await
        .expect("the reconfigured tool turn completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    // The swapped-in registry executed the call; the old registry never did.
    assert_eq!(new_log.len(), 1);
    assert!(old_log.is_empty());
    assert_eq!(new_log.requests()[0].name, "get_calendar");

    // The opening request already advertised the new tool set.
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools, vec![calendar_tool()]);
    assert_eq!(machine.state().current_tool_set(), &replacement);
}
