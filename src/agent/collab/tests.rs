//! Unit tests for the collaboration primitives and their bridge tool adapters
//! (M6-3). The bridge-adapter tests are named with a `tool_adapter` prefix so the
//! milestone validation selector `cargo test tool_adapter` runs that whole slice;
//! the data-only snapshot / restore round-trips added for M3-1 are named after the
//! primitive they exercise (`mailbox_snapshot_*`, `blackboard_snapshot_*`,
//! `plan_snapshot_*`).
//!
//! The tests cover three layers:
//!
//! - the [`Plan`] / [`Blackboard`] / [`Mailbox`] primitive semantics directly
//!   (CAS + dependency-completion claims, append-only monotonic offsets,
//!   directed monotonic delivery);
//! - the [`CollabToolHandler`] adapter, which must run under the host
//!   [`RunContext`] guard, use the *injected* identity rather than a
//!   model-supplied owner/sender, and surface primitive errors as model-visible
//!   [`ToolStatus::Error`] results; and
//! - the [`SpawnAgentRequest`] translation, which turns a `spawn_agent` tool call
//!   into a [`RequirementKind::NeedSubagent`] instead of running inline.

use super::blackboard::{Blackboard, BlackboardSnapshot, DEFAULT_CHANNEL};
use super::mailbox::{MailMessage, Mailbox, MailboxSnapshot};
use super::plan::{Plan, PlanError, PlanSnapshot, TaskSnapshot, TaskStatus};
use super::tools::{
    BLACKBOARD_POST, BLACKBOARD_READ, CollabToolHandler, MAILBOX_READ, PLAN_ADD_TASK, PLAN_CLAIM,
    PLAN_CLAIM_FIRST_AVAILABLE, PLAN_READ, PLAN_UPDATE, REPORT_ARTIFACT, RUN_HOST_TOOL,
    SEND_MESSAGE, SPAWN_AGENT, SpawnAgentRequest, ToolAdapterError, bridge_tool_declarations,
    bridge_tool_set,
};
use crate::agent::{
    BudgetLimits, RunContext, RunId, TraceNodeId,
    drive::ToolHandler,
    id::{AgentId, BlackboardId, PlanId, ToolSetId},
    requirement::{RequirementKind, RequirementResult},
    tool::{ToolRegistry, ToolRuntimeError},
};
use crate::conversation::ToolCallId;
use crate::model::content::ContentBlock;
use crate::model::tool::{Tool, ToolCall, ToolResponse, ToolStatus};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

// ----- fixtures ------------------------------------------------------------

/// A root run context with an unbounded budget (the collab tools do not charge
/// tokens; the only guard they consult is cancellation).
fn ctx() -> RunContext {
    RunContext::new_root(
        RunId::new(Uuid::from_u128(0x6003_0001)),
        BudgetLimits::unbounded(),
        TraceNodeId::new("collab-root"),
    )
}

fn plan() -> Arc<Plan> {
    Arc::new(Plan::new(PlanId::new(Uuid::from_u128(0x6003_0010))))
}

fn blackboard() -> Arc<Blackboard> {
    Arc::new(Blackboard::new(BlackboardId::new(Uuid::from_u128(
        0x6003_0020,
    ))))
}

fn mailbox() -> Arc<Mailbox> {
    Arc::new(Mailbox::new())
}

/// A handler wired for `identity` over fresh, empty primitives.
fn handler(identity: &str) -> CollabToolHandler {
    CollabToolHandler::new(identity, plan(), blackboard(), mailbox())
}

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall {
        id: "provider-call-1".to_owned(),
        name: name.to_owned(),
        input,
        extra: Map::new(),
    }
}

fn framework_call_id() -> ToolCallId {
    ToolCallId::new(Uuid::from_u128(0x6003_0099))
}

/// Drives one inline tool call, asserting the handler produced a `Tool(Ok(..))`
/// result, and returns the response the model would see.
async fn run(handler: &CollabToolHandler, call: &ToolCall) -> ToolResponse {
    match handler.fulfill(framework_call_id(), call, &ctx()).await {
        RequirementResult::Tool(Ok(response)) => response,
        other => panic!("expected Tool(Ok(..)), got {other:?}"),
    }
}

/// Concatenates the text content of a tool response for assertions.
fn text_of(response: &ToolResponse) -> String {
    response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// ----- Plan primitive ------------------------------------------------------

/// A `plan_claim` on a task whose dependency is unfinished is rejected and
/// changes nothing (design §6.2 认领需要依赖检查). This is the first required
/// M6-3 validation, exercised through the tool adapter end to end.
#[tokio::test]
async fn tool_adapter_plan_claim_rejects_unfinished_dependency() {
    let plan = plan();
    // `impl` depends on `design`; `design` is never completed.
    plan.add_task("design", Vec::<String>::new())
        .expect("add design");
    plan.add_task("impl", ["design"]).expect("add impl");
    let version = plan.version();

    let handler = CollabToolHandler::new("worker-a", plan.clone(), blackboard(), mailbox());
    let response = run(
        &handler,
        &call(
            PLAN_CLAIM,
            json!({ "task": "impl", "expected_version": version }),
        ),
    )
    .await;

    assert_eq!(response.status, ToolStatus::Error);
    assert!(
        text_of(&response).contains("blocked by unfinished dependencies"),
        "unexpected error text: {}",
        text_of(&response)
    );
    // The failed claim mutated nothing: version unchanged and task still Todo,
    // unowned.
    assert_eq!(plan.version(), version);
    let snapshot = plan.snapshot();
    let task = &snapshot.tasks["impl"];
    assert_eq!(task.status, TaskStatus::Todo);
    assert_eq!(task.owner, None);
}

/// Once the dependency is completed, the same claim succeeds, takes ownership,
/// and moves the task to `InProgress` under the injected identity.
#[tokio::test]
async fn tool_adapter_plan_claim_succeeds_after_dependency_completed() {
    let plan = plan();
    plan.add_task("design", Vec::<String>::new())
        .expect("add design");
    plan.add_task("impl", ["design"]).expect("add impl");
    // Complete `design` through its own claim + update.
    let v = plan
        .claim("design", "worker-a", plan.version())
        .expect("claim design");
    plan.update_status("design", "worker-a", TaskStatus::Completed, v)
        .expect("complete design");

    let handler = CollabToolHandler::new("worker-b", plan.clone(), blackboard(), mailbox());
    let response = run(
        &handler,
        &call(
            PLAN_CLAIM,
            json!({ "task": "impl", "expected_version": plan.version() }),
        ),
    )
    .await;

    assert_eq!(response.status, ToolStatus::Ok);
    let snapshot = plan.snapshot();
    let task = &snapshot.tasks["impl"];
    assert_eq!(task.status, TaskStatus::InProgress);
    assert_eq!(task.owner.as_deref(), Some("worker-b"));
}

/// A claim under a stale `expected_version` loses the optimistic race and leaves
/// the plan untouched.
#[tokio::test]
async fn tool_adapter_plan_claim_version_conflict_changes_nothing() {
    let plan = plan();
    plan.add_task("task", Vec::<String>::new())
        .expect("add task");
    let stale = plan.version();
    // Bump the version so `stale` no longer matches.
    plan.add_task("other", Vec::<String>::new())
        .expect("add other");
    let current = plan.version();

    let result = plan.claim("task", "worker", stale);
    assert!(matches!(
        result,
        Err(PlanError::VersionConflict { expected, actual })
            if expected == stale && actual == current
    ));
    assert_eq!(plan.version(), current);
    assert_eq!(plan.snapshot().tasks["task"].owner, None);
}

/// `add_task` rejects duplicates, self-dependencies, and unknown dependencies
/// without mutating the plan (the malformed-edge class). A dependency cycle
/// cannot be formed through `add_task` — a fresh node only points at existing
/// nodes and has no incoming edges — so the acyclic invariant holds by
/// construction and the internal cycle check is purely defensive.
#[tokio::test]
async fn tool_adapter_plan_add_task_rejects_malformed_graph() {
    let plan = plan();
    plan.add_task("a", Vec::<String>::new()).expect("add a");
    let version = plan.version();

    assert!(matches!(
        plan.add_task("a", Vec::<String>::new()),
        Err(PlanError::DuplicateTask(id)) if id == "a"
    ));
    assert!(matches!(
        plan.add_task("b", ["b"]),
        Err(PlanError::SelfDependency(id)) if id == "b"
    ));
    assert!(matches!(
        plan.add_task("c", ["missing"]),
        Err(PlanError::UnknownTask(id)) if id == "missing"
    ));

    // Every rejected add left the plan untouched.
    assert_eq!(plan.version(), version);
    assert_eq!(plan.snapshot().tasks.len(), 1);
}

#[tokio::test]
async fn plan_add_task_defensively_rejects_a_restored_cycle_without_cloning_the_board() {
    let mut tasks = BTreeMap::new();
    tasks.insert(
        "a".to_owned(),
        TaskSnapshot {
            status: TaskStatus::Todo,
            owner: None,
            depends_on: vec!["b".to_owned()],
        },
    );
    tasks.insert(
        "b".to_owned(),
        TaskSnapshot {
            status: TaskStatus::Todo,
            owner: None,
            depends_on: vec!["a".to_owned()],
        },
    );
    let plan = Plan::from_snapshot(PlanSnapshot {
        id: PlanId::new(Uuid::from_u128(0x6003_00ff)),
        version: 7,
        task_order: vec!["a".to_owned(), "b".to_owned()],
        tasks,
    });

    assert!(matches!(
        plan.add_task("c", ["a"]),
        Err(PlanError::DependencyCycle(cycle)) if cycle.len() == 3
    ));
    assert_eq!(plan.version(), 7);
    assert_eq!(plan.snapshot().tasks.len(), 2);
}

/// Status transitions are enforced: only the owner may update, terminal states
/// are sticky, and illegal moves are rejected.
#[tokio::test]
async fn tool_adapter_plan_status_transitions_enforced() {
    let plan = plan();
    plan.add_task("t", Vec::<String>::new()).expect("add t");
    let v = plan.claim("t", "owner", plan.version()).expect("claim t");

    // A non-owner cannot update.
    assert!(matches!(
        plan.update_status("t", "intruder", TaskStatus::Completed, v),
        Err(PlanError::NotOwner { .. })
    ));

    // InProgress -> Todo is illegal.
    assert!(matches!(
        plan.update_status("t", "owner", TaskStatus::Todo, v),
        Err(PlanError::InvalidTransition { .. })
    ));

    // InProgress -> Completed is legal; Completed is terminal.
    let v = plan
        .update_status("t", "owner", TaskStatus::Completed, v)
        .expect("complete");
    assert!(matches!(
        plan.update_status("t", "owner", TaskStatus::InProgress, v),
        Err(PlanError::InvalidTransition { .. })
    ));
}

/// `plan_claim_first_available` skips completed / claimed / dependency-blocked
/// tasks and claims the first eligible one in stable order.
#[tokio::test]
async fn tool_adapter_plan_claim_first_available_skips_ineligible() {
    let plan = plan_with_blocked();
    let handler = CollabToolHandler::new("claimer", plan.clone(), blackboard(), mailbox());
    let response = run(
        &handler,
        &call(
            PLAN_CLAIM_FIRST_AVAILABLE,
            json!({ "expected_version": plan.version() }),
        ),
    )
    .await;

    assert_eq!(response.status, ToolStatus::Ok);
    assert!(
        text_of(&response).contains("ready"),
        "expected `ready` to be claimed, got: {}",
        text_of(&response)
    );
    assert_eq!(
        plan.snapshot().tasks["ready"].status,
        TaskStatus::InProgress
    );
    assert_eq!(plan.snapshot().tasks["blocked"].status, TaskStatus::Todo);
}

/// Builds a plan whose only claimable task is `ready`: `gate` is already claimed
/// (so it is `InProgress`, not an eligible `Todo`) and `blocked` waits on the
/// never-completed `gate`.
fn plan_with_blocked() -> Arc<Plan> {
    let plan = plan();
    plan.add_task("gate", Vec::<String>::new())
        .expect("add gate");
    plan.add_task("blocked", ["gate"]).expect("add blocked");
    plan.add_task("ready", Vec::<String>::new())
        .expect("add ready");
    plan.claim("gate", "someone", plan.version())
        .expect("claim gate");
    plan
}

// ----- Blackboard primitive ------------------------------------------------

/// Blackboard posts are append-only with per-channel zero-based, monotonic
/// offsets, and a cursored read returns exactly the tail. This is the second
/// required M6-3 validation.
#[tokio::test]
async fn tool_adapter_blackboard_post_read_append_only_monotonic() {
    let board = blackboard();

    assert_eq!(board.post(DEFAULT_CHANNEL, "a", "first"), 0);
    assert_eq!(board.post(DEFAULT_CHANNEL, "b", "second"), 1);
    assert_eq!(board.post(DEFAULT_CHANNEL, "a", "third"), 2);

    // Full history preserves order and immutable offsets.
    let all = board.read_from(DEFAULT_CHANNEL, 0);
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].offset, 0);
    assert_eq!(all[0].sender, "a");
    assert_eq!(all[0].text, "first");
    assert_eq!(all[2].offset, 2);
    assert_eq!(all[2].text, "third");

    // A cursored read returns only the tail, keeping original offsets.
    let tail = board.read_from(DEFAULT_CHANNEL, 2);
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].offset, 2);
    assert_eq!(tail[0].text, "third");

    // Reading past the end is empty, never an error.
    assert!(board.read_from(DEFAULT_CHANNEL, 3).is_empty());
    // An unknown channel reads as empty.
    assert!(board.read_from("nope", 0).is_empty());
}

/// Channels are independent namespaces: each keeps its own zero-based offset
/// sequence.
#[tokio::test]
async fn tool_adapter_blackboard_channels_are_namespaced() {
    let board = blackboard();
    assert_eq!(board.post("alpha", "s", "a0"), 0);
    assert_eq!(board.post("beta", "s", "b0"), 0);
    assert_eq!(board.post("alpha", "s", "a1"), 1);

    assert_eq!(board.read_from("alpha", 0).len(), 2);
    assert_eq!(board.read_from("beta", 0).len(), 1);
    let mut channels = board.channels_list();
    channels.sort();
    assert_eq!(channels, vec!["alpha".to_owned(), "beta".to_owned()]);
}

/// Posting through the tool adapter uses the *injected* identity as the sender,
/// never a model-supplied value.
#[tokio::test]
async fn tool_adapter_blackboard_post_uses_injected_identity_as_sender() {
    let board = blackboard();
    let handler = CollabToolHandler::new("agent-42", plan(), board.clone(), mailbox());

    let response = run(
        &handler,
        &call(BLACKBOARD_POST, json!({ "text": "status: ok" })),
    )
    .await;
    assert_eq!(response.status, ToolStatus::Ok);

    let messages = board.read_from(DEFAULT_CHANNEL, 0);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].sender, "agent-42");
    assert_eq!(messages[0].text, "status: ok");

    // A read tool call reports how many messages are visible.
    let read = run(&handler, &call(BLACKBOARD_READ, json!({ "from": 0 }))).await;
    assert!(text_of(&read).contains("read 1 message"));
}

/// `blackboard_read` returns the message **bodies** (H-STATE-6: it previously
/// counted and discarded them), each line attributed with offset and sender,
/// and a cursored read returns only the tail.
#[tokio::test]
async fn tool_adapter_blackboard_read_returns_message_bodies() {
    let board = blackboard();
    board.post(DEFAULT_CHANNEL, "alice", "first message");
    board.post(DEFAULT_CHANNEL, "bob", "second message");
    let handler = CollabToolHandler::new("reader", plan(), board.clone(), mailbox());

    let response = run(&handler, &call(BLACKBOARD_READ, json!({}))).await;
    assert_eq!(response.status, ToolStatus::Ok);
    let text = text_of(&response);
    assert!(text.contains("read 2 message(s)"), "unexpected: {text}");
    assert!(
        text.contains("#0 alice: first message"),
        "unexpected: {text}"
    );
    assert!(
        text.contains("#1 bob: second message"),
        "unexpected: {text}"
    );

    // A cursored read returns only the tail, keeping original offsets.
    let tail = run(&handler, &call(BLACKBOARD_READ, json!({ "from": 1 }))).await;
    let text = text_of(&tail);
    assert!(text.contains("read 1 message(s)"), "unexpected: {text}");
    assert!(
        text.contains("#1 bob: second message"),
        "unexpected: {text}"
    );
    assert!(!text.contains("first message"), "unexpected: {text}");
}

/// A read page is bounded: `limit` holds back further messages and reports the
/// cursor to resume from, and an over-long body is truncated.
#[tokio::test]
async fn tool_adapter_blackboard_read_paginates_and_truncates_bodies() {
    let board = blackboard();
    board.post(DEFAULT_CHANNEL, "a", "m0");
    board.post(DEFAULT_CHANNEL, "a", "m1");
    board.post(DEFAULT_CHANNEL, "a", "m2");
    let long = "x".repeat(500);
    board.post(DEFAULT_CHANNEL, "a", &long);
    let handler = CollabToolHandler::new("reader", plan(), board.clone(), mailbox());

    let page = run(&handler, &call(BLACKBOARD_READ, json!({ "limit": 2 }))).await;
    let text = text_of(&page);
    assert!(text.contains("read 2 message(s)"), "unexpected: {text}");
    assert!(text.contains("#0 a: m0"), "unexpected: {text}");
    assert!(text.contains("#1 a: m1"), "unexpected: {text}");
    assert!(!text.contains("#2 a: m2"), "unexpected: {text}");
    assert!(
        text.contains("resume with from=2"),
        "expected a resume hint, got: {text}"
    );

    // The 500-char body is truncated with a marker instead of flooding output.
    let read = run(&handler, &call(BLACKBOARD_READ, json!({ "from": 3 }))).await;
    let text = text_of(&read);
    assert!(text.contains("[truncated]"), "unexpected: {text}");
    assert!(!text.contains(&long), "body was not truncated: {text}");
}

// ----- Mailbox primitive ---------------------------------------------------

/// Mailbox delivery is directed (only the recipient's inbox grows) and every
/// message gets a mailbox-global monotonic sequence number.
#[tokio::test]
async fn tool_adapter_mailbox_directed_delivery_monotonic_seq() {
    let mail = mailbox();
    assert_eq!(mail.send("a", "b", "hi b"), 0);
    assert_eq!(mail.send("a", "c", "hi c"), 1);
    assert_eq!(mail.send("b", "a", "hi a"), 2);

    assert_eq!(mail.inbox("b").len(), 1);
    assert_eq!(mail.inbox("b")[0].from, "a");
    assert_eq!(mail.inbox("b")[0].seq, 0);
    assert_eq!(mail.inbox("c").len(), 1);
    assert_eq!(mail.inbox("a").len(), 1);
    assert_eq!(mail.inbox("a")[0].seq, 2);

    // A cursored read returns only newer mail.
    assert!(mail.read_from("a", 3).is_empty());
}

/// The `send_message` tool routes through the library mailbox from the injected
/// identity to the requested recipient.
#[tokio::test]
async fn tool_adapter_send_message_delivers_via_library_mailbox() {
    let mail = mailbox();
    let handler = CollabToolHandler::new("coordinator", plan(), blackboard(), mail.clone());

    let response = run(
        &handler,
        &call(
            SEND_MESSAGE,
            json!({ "to": "worker-1", "text": "please claim review" }),
        ),
    )
    .await;
    assert_eq!(response.status, ToolStatus::Ok);

    let inbox = mail.inbox("worker-1");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "coordinator");
    assert_eq!(inbox[0].text, "please claim review");
}

/// The `mailbox_read` tool (H-STATE-6: the mailbox previously had no read
/// bridge) returns the calling agent's own inbox messages, attributed with seq
/// and sender; the recipient identity is the injected one, so a bystander reads
/// an empty inbox, and a `from` cursor returns only newer mail.
#[tokio::test]
async fn tool_adapter_mailbox_read_returns_own_inbox_messages() {
    let mail = mailbox();
    let sender = CollabToolHandler::new("coordinator", plan(), blackboard(), mail.clone());
    run(
        &sender,
        &call(
            SEND_MESSAGE,
            json!({ "to": "worker-1", "text": "please claim review" }),
        ),
    )
    .await;
    run(
        &sender,
        &call(
            SEND_MESSAGE,
            json!({ "to": "worker-1", "text": "design landed" }),
        ),
    )
    .await;

    let reader = CollabToolHandler::new("worker-1", plan(), blackboard(), mail.clone());
    let response = run(&reader, &call(MAILBOX_READ, json!({}))).await;
    assert_eq!(response.status, ToolStatus::Ok);
    let text = text_of(&response);
    assert!(text.contains("read 2 message(s)"), "unexpected: {text}");
    assert!(
        text.contains("#0 coordinator: please claim review"),
        "unexpected: {text}"
    );
    assert!(
        text.contains("#1 coordinator: design landed"),
        "unexpected: {text}"
    );

    // A cursored read returns only newer mail.
    let tail = run(&reader, &call(MAILBOX_READ, json!({ "from": 1 }))).await;
    let text = text_of(&tail);
    assert!(text.contains("read 1 message(s)"), "unexpected: {text}");
    assert!(
        text.contains("#1 coordinator: design landed"),
        "unexpected: {text}"
    );
    assert!(!text.contains("please claim review"), "unexpected: {text}");

    // A bystander cannot read someone else's mail: the recipient identity is
    // the injected one, never a model-supplied argument.
    let bystander = CollabToolHandler::new("bystander", plan(), blackboard(), mail.clone());
    let response = run(&bystander, &call(MAILBOX_READ, json!({}))).await;
    let text = text_of(&response);
    assert!(text.contains("read 0 message(s)"), "unexpected: {text}");
    assert!(!text.contains("coordinator"), "unexpected: {text}");
}

// ----- spawn_agent translation ---------------------------------------------

/// `spawn_agent` parses into a structured request that converts to a
/// `NeedSubagent` requirement (never an inline tool). This is the first half of
/// the third required M6-3 validation; the derivation half lives in
/// `tests/agent_tool_adapter.rs`.
#[tokio::test]
async fn tool_adapter_spawn_agent_produces_need_subagent() {
    let spec_id = AgentId::new(Uuid::from_u128(0x6003_0500));
    let step_id = crate::agent::id::StepId::new(Uuid::from_u128(0x6003_0501));
    let schema = json!({ "type": "object" });
    let spawn_call = call(
        SPAWN_AGENT,
        json!({
            "spec": spec_id.to_string(),
            "brief": "review the patch",
            "result_schema": schema,
        }),
    );

    let request = SpawnAgentRequest::parse(&spawn_call).expect("parse spawn_agent");
    assert_eq!(request.spec().0, spec_id);
    assert_eq!(request.brief(), "review the patch");
    assert_eq!(request.result_schema(), Some(&schema));

    match request.into_requirement_kind(step_id) {
        RequirementKind::NeedSubagent {
            spec_ref,
            brief,
            result_schema,
        } => {
            assert_eq!(spec_ref.0, spec_id);
            assert_eq!(brief.step_id(), step_id);
            assert_eq!(result_schema, Some(schema));
        }
        other => panic!("expected NeedSubagent, got {other:?}"),
    }
}

/// The `spawn_agent` translator rejects the malformed-argument class: wrong
/// tool, missing `spec`/`brief`, a non-UUID spec, and a non-object schema.
#[tokio::test]
async fn tool_adapter_spawn_agent_parse_rejects_bad_arguments() {
    // Wrong tool.
    assert!(matches!(
        SpawnAgentRequest::parse(&call(PLAN_READ, json!({}))),
        Err(ToolAdapterError::WrongTool { .. })
    ));
    // Missing brief.
    assert!(matches!(
        SpawnAgentRequest::parse(&call(
            SPAWN_AGENT,
            json!({ "spec": AgentId::new(Uuid::from_u128(1)).to_string() })
        )),
        Err(ToolAdapterError::MissingArgument("brief"))
    ));
    // Non-UUID spec.
    assert!(matches!(
        SpawnAgentRequest::parse(&call(
            SPAWN_AGENT,
            json!({ "spec": "not-a-uuid", "brief": "x" })
        )),
        Err(ToolAdapterError::InvalidAgentId(_))
    ));
    // Non-object result_schema.
    assert!(matches!(
        SpawnAgentRequest::parse(&call(
            SPAWN_AGENT,
            json!({
                "spec": AgentId::new(Uuid::from_u128(1)).to_string(),
                "brief": "x",
                "result_schema": "nope"
            })
        )),
        Err(ToolAdapterError::InvalidArgument { argument, .. }) if argument == "result_schema"
    ));
}

/// `spawn_agent` is a scope-deepening op: the inline handler refuses to run it,
/// surfacing an execution error that tells the host to translate it instead.
#[tokio::test]
async fn tool_adapter_spawn_agent_is_not_run_inline() {
    let handler = handler("agent");
    let result = handler
        .fulfill(
            framework_call_id(),
            &call(
                SPAWN_AGENT,
                json!({ "spec": AgentId::new(Uuid::from_u128(1)).to_string(), "brief": "x" }),
            ),
            &ctx(),
        )
        .await;
    match result {
        RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed { tool_name, .. })) => {
            assert_eq!(tool_name, SPAWN_AGENT);
        }
        other => panic!("expected inline ExecutionFailed, got {other:?}"),
    }
}

// ----- adapter guards & routing --------------------------------------------

/// A cancelled run refuses further tool work before touching any primitive
/// (design §3.4 不绕过 RunContext 护栏).
#[tokio::test]
async fn tool_adapter_cancelled_context_refuses_tool() {
    let plan = plan();
    plan.add_task("t", Vec::<String>::new()).expect("add t");
    let handler = CollabToolHandler::new("worker", plan.clone(), blackboard(), mailbox());

    let ctx = ctx();
    ctx.cancellation().cancel();

    let result = handler
        .fulfill(
            framework_call_id(),
            &call(
                PLAN_CLAIM,
                json!({ "task": "t", "expected_version": plan.version() }),
            ),
            &ctx,
        )
        .await;
    assert!(matches!(
        result,
        RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed { .. }))
    ));
    // The refused claim never mutated the plan.
    assert_eq!(plan.snapshot().tasks["t"].owner, None);
}

/// An unrecognized tool name is a routing error, not a model-visible result.
#[tokio::test]
async fn tool_adapter_unknown_tool_is_rejected() {
    let handler = handler("agent");
    let result = handler
        .fulfill(
            framework_call_id(),
            &call("no_such_tool", json!({})),
            &ctx(),
        )
        .await;
    match result {
        RequirementResult::Tool(Err(ToolRuntimeError::UnknownTool { name })) => {
            assert_eq!(name, "no_such_tool");
        }
        other => panic!("expected UnknownTool, got {other:?}"),
    }
}

/// `plan_read` reports the version and per-task status labels.
#[tokio::test]
async fn tool_adapter_plan_read_reports_version_and_tasks() {
    let plan = plan();
    plan.add_task("t", Vec::<String>::new()).expect("add t");
    let handler = CollabToolHandler::new("agent", plan.clone(), blackboard(), mailbox());

    let response = run(&handler, &call(PLAN_READ, json!({}))).await;
    let text = text_of(&response);
    assert!(text.contains("plan v1"), "unexpected: {text}");
    assert!(text.contains("t=todo"), "unexpected: {text}");
}

/// `plan_read` also reports each task's owner and dependencies, so a reader
/// can see who owns what and which tasks are still blocked without extra
/// round trips.
#[tokio::test]
async fn tool_adapter_plan_read_reports_owner_and_dependencies() {
    let plan = plan();
    plan.add_task("design", Vec::<String>::new())
        .expect("add design");
    plan.add_task("impl", ["design"]).expect("add impl");
    plan.claim("design", "alice", plan.version())
        .expect("claim design");
    let handler = CollabToolHandler::new("agent", plan.clone(), blackboard(), mailbox());

    let response = run(&handler, &call(PLAN_READ, json!({}))).await;
    let text = text_of(&response);
    assert!(
        text.contains("design=in_progress@alice"),
        "unexpected: {text}"
    );
    assert!(
        text.contains("impl=todo deps:[design]"),
        "unexpected: {text}"
    );
}

/// `plan_add_task` via the tool adapter appends a task and bumps the version.
#[tokio::test]
async fn tool_adapter_plan_add_task_via_tool_appends() {
    let plan = plan();
    let handler = CollabToolHandler::new("agent", plan.clone(), blackboard(), mailbox());

    let response = run(
        &handler,
        &call(PLAN_ADD_TASK, json!({ "id": "review", "depends_on": [] })),
    )
    .await;
    assert_eq!(response.status, ToolStatus::Ok);
    assert!(plan.snapshot().tasks.contains_key("review"));
}

/// `plan_update` via the tool adapter enforces ownership: a claim (as the
/// injected identity) then a legal completion succeeds.
#[tokio::test]
async fn tool_adapter_plan_update_requires_owned_task() {
    let plan = plan();
    plan.add_task("t", Vec::<String>::new()).expect("add t");
    let handler = CollabToolHandler::new("owner", plan.clone(), blackboard(), mailbox());

    let claim = run(
        &handler,
        &call(
            PLAN_CLAIM,
            json!({ "task": "t", "expected_version": plan.version() }),
        ),
    )
    .await;
    assert_eq!(claim.status, ToolStatus::Ok);

    let update = run(
        &handler,
        &call(
            PLAN_UPDATE,
            json!({ "task": "t", "status": "completed", "expected_version": plan.version() }),
        ),
    )
    .await;
    assert_eq!(update.status, ToolStatus::Ok);
    assert_eq!(plan.snapshot().tasks["t"].status, TaskStatus::Completed);
}

// ----- report_artifact -----------------------------------------------------

/// `report_artifact` records a redaction-safe reference to the configured sink.
#[tokio::test]
async fn tool_adapter_report_artifact_records_to_sink() {
    use super::tools::RecordingArtifactSink;
    let sink = Arc::new(RecordingArtifactSink::new());
    let handler = CollabToolHandler::new("agent", plan(), blackboard(), mailbox())
        .with_artifact_sink(sink.clone());

    let response = run(
        &handler,
        &call(
            REPORT_ARTIFACT,
            json!({ "kind": "patch", "summary": "fix bug", "path": "src/lib.rs" }),
        ),
    )
    .await;
    assert_eq!(response.status, ToolStatus::Ok);

    let recorded = sink.artifacts();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].summary, "fix bug");
    assert_eq!(recorded[0].path.as_deref(), Some("src/lib.rs"));
}

// ----- run_host_tool -------------------------------------------------------

/// A minimal host [`ToolRegistry`] that echoes the inner tool name back.
#[derive(Debug)]
struct EchoRegistry;

#[async_trait]
impl ToolRegistry for EchoRegistry {
    fn declarations(&self) -> Vec<Tool> {
        Vec::new()
    }

    async fn execute(
        &self,
        _call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        Ok(ToolResponse {
            tool_call_id: call.id.clone(),
            content: vec![ContentBlock::Text {
                text: format!("echo:{}", call.name),
                extra: Map::new(),
            }],
            status: ToolStatus::Ok,
            extra: Map::new(),
        })
    }
}

/// `run_host_tool` forwards to the registered host registry and re-pairs the
/// response to the outer provider call id.
#[tokio::test]
async fn tool_adapter_run_host_tool_forwards_to_registry() {
    let handler = CollabToolHandler::new("agent", plan(), blackboard(), mailbox())
        .with_host_tools(Arc::new(EchoRegistry));

    let outer = call(RUN_HOST_TOOL, json!({ "name": "do_thing", "input": {} }));
    let response = run(&handler, &outer).await;
    assert_eq!(response.status, ToolStatus::Ok);
    assert_eq!(text_of(&response), "echo:do_thing");
    // The response is addressed to the outer provider call id, not the inner one.
    assert_eq!(response.tool_call_id, outer.id);
}

/// Without a host registry, `run_host_tool` returns a model-visible error rather
/// than failing the requirement.
#[tokio::test]
async fn tool_adapter_run_host_tool_without_registry_errors() {
    let handler = handler("agent");
    let response = run(&handler, &call(RUN_HOST_TOOL, json!({ "name": "x" }))).await;
    assert_eq!(response.status, ToolStatus::Error);
    assert!(text_of(&response).contains("no host tools"));
}

// ----- declarations & serde ------------------------------------------------

/// The bridge declarations cover every inline tool plus `spawn_agent`, and
/// `bridge_tool_set` packages them under the requested id.
#[tokio::test]
async fn tool_adapter_bridge_declarations_cover_every_tool() {
    let names: Vec<String> = bridge_tool_declarations()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    for expected in [
        SPAWN_AGENT,
        PLAN_ADD_TASK,
        PLAN_READ,
        PLAN_CLAIM,
        PLAN_CLAIM_FIRST_AVAILABLE,
        PLAN_UPDATE,
        BLACKBOARD_POST,
        BLACKBOARD_READ,
        SEND_MESSAGE,
        MAILBOX_READ,
        REPORT_ARTIFACT,
        RUN_HOST_TOOL,
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing {expected}"
        );
    }

    let set_id = ToolSetId::new(Uuid::from_u128(0x6003_0700));
    let set = bridge_tool_set(set_id);
    assert_eq!(set.id(), set_id);
    assert_eq!(set.tools().len(), names.len());
}

/// `TaskStatus` round-trips through its wire label and serde form.
#[tokio::test]
async fn tool_adapter_task_status_label_round_trip() {
    for status in [
        TaskStatus::Todo,
        TaskStatus::InProgress,
        TaskStatus::Completed,
        TaskStatus::Blocked,
        TaskStatus::Cancelled,
    ] {
        assert_eq!(TaskStatus::from_label(status.label()), Some(status));
        let json = serde_json::to_string(&status).expect("serialize status");
        let back: TaskStatus = serde_json::from_str(&json).expect("deserialize status");
        assert_eq!(back, status);
    }
    assert_eq!(TaskStatus::from_label("bogus"), None);
}

// ----- Data-only snapshot / restore round-trips (M3-1) ---------------------

/// A [`Mailbox`] snapshot is data-only serde and round-trips: after restore the
/// inboxes are identical and a fresh send continues the monotonic sequence
/// rather than reusing a delivered seq.
#[tokio::test]
async fn mailbox_snapshot_round_trip_preserves_inboxes_and_seq() {
    let mail = mailbox();
    assert_eq!(mail.send("a", "b", "hi b"), 0);
    assert_eq!(mail.send("a", "c", "hi c"), 1);
    assert_eq!(mail.send("b", "a", "hi a"), 2);

    let snapshot = mail.snapshot();
    let encoded = serde_json::to_string(&snapshot).expect("serialize mailbox snapshot");
    let decoded: MailboxSnapshot =
        serde_json::from_str(&encoded).expect("deserialize mailbox snapshot");
    assert_eq!(decoded, snapshot);

    let restored = Mailbox::from_snapshot(decoded);
    assert_eq!(restored.read_from("b", 0), mail.read_from("b", 0));
    assert_eq!(restored.read_from("c", 0), mail.read_from("c", 0));
    assert_eq!(restored.read_from("a", 0), mail.read_from("a", 0));

    // A fresh send continues the sequence at the next value, not an old one.
    assert_eq!(restored.send("c", "a", "again"), 3);
    assert_eq!(restored.inbox("a").last().expect("inbox a").seq, 3);
}

/// [`Mailbox::from_snapshot`] reconciles a stale `next_seq` up to
/// `max(seq) + 1`, so even a hand-written or older snapshot can never hand out a
/// sequence that collides with delivered mail.
#[tokio::test]
async fn mailbox_from_snapshot_reconciles_stale_next_seq() {
    let mut inboxes = BTreeMap::new();
    inboxes.insert(
        "a".to_owned(),
        vec![MailMessage {
            seq: 7,
            from: "x".to_owned(),
            to: "a".to_owned(),
            text: "hi".to_owned(),
        }],
    );
    // next_seq trails the delivered mail; restore must repair it.
    let snapshot = MailboxSnapshot {
        next_seq: 0,
        inboxes,
    };
    let mail = Mailbox::from_snapshot(snapshot);
    assert_eq!(mail.send("a", "b", "next"), 8);
}

/// A [`Blackboard`] whole-board snapshot is data-only serde and round-trips:
/// after restore the identity, channel list, and each channel's ordered content
/// match, and a fresh post continues each channel's offset.
#[tokio::test]
async fn blackboard_snapshot_all_round_trip_preserves_channels_and_offsets() {
    let board = blackboard();
    assert_eq!(board.post("alpha", "s", "a0"), 0);
    assert_eq!(board.post("beta", "s", "b0"), 0);
    assert_eq!(board.post("alpha", "s", "a1"), 1);

    let snapshot = board.snapshot_all();
    let encoded = serde_json::to_string(&snapshot).expect("serialize blackboard snapshot");
    let decoded: BlackboardSnapshot =
        serde_json::from_str(&encoded).expect("deserialize blackboard snapshot");
    assert_eq!(decoded, snapshot);

    let restored = Blackboard::from_snapshot(decoded);
    assert_eq!(restored.id(), board.id());
    let mut channels = restored.channels_list();
    channels.sort();
    assert_eq!(channels, vec!["alpha".to_owned(), "beta".to_owned()]);
    assert_eq!(restored.snapshot("alpha"), board.snapshot("alpha"));
    assert_eq!(restored.snapshot("beta"), board.snapshot("beta"));

    // A fresh post continues each channel's offset from its current length.
    assert_eq!(restored.post("alpha", "s", "a2"), 2);
    assert_eq!(restored.post("beta", "s", "b1"), 1);
}

/// A [`Plan`] snapshot is data-only serde and round-trips: after restore the id,
/// version, task order, and task states match, the retained version still guards
/// a CAS claim, and the restored plan can continue driving operations.
#[tokio::test]
async fn plan_snapshot_round_trip_preserves_state_and_resumes_operations() {
    let plan = plan();
    plan.add_task("design", Vec::<String>::new())
        .expect("add design");
    plan.add_task("impl", ["design"]).expect("add impl");
    plan.claim("design", "alice", plan.version())
        .expect("claim design");

    let snapshot = plan.snapshot();
    let encoded = serde_json::to_string(&snapshot).expect("serialize plan snapshot");
    let decoded: PlanSnapshot = serde_json::from_str(&encoded).expect("deserialize plan snapshot");
    assert_eq!(decoded, snapshot);

    let restored = Plan::from_snapshot(decoded);
    assert_eq!(restored.id(), plan.id());
    assert_eq!(restored.version(), plan.version());
    let restored_snapshot = restored.snapshot();
    assert_eq!(restored_snapshot.task_order, snapshot.task_order);
    assert_eq!(restored_snapshot.tasks, snapshot.tasks);

    // The retained version still guards a CAS claim: a stale version is rejected.
    assert!(matches!(
        restored.claim("impl", "bob", 0),
        Err(PlanError::VersionConflict { .. })
    ));

    // With design completed and the correct version, the dependent claim proceeds.
    restored
        .update_status("design", "alice", TaskStatus::Completed, restored.version())
        .expect("complete design");
    restored
        .claim("impl", "bob", restored.version())
        .expect("claim impl after restore");
    assert_eq!(
        restored.snapshot().tasks["impl"].status,
        TaskStatus::InProgress
    );
}
