//! Unit coverage for the complex-test support layer (milestone 1, M1-1).
//!
//! These tests pin the invariants of the mock plan/blackboard vertical feature
//! before the higher-level complex flow/subagent/cancel suites depend on it:
//! dependency-graph validation, atomic dependency-blocked claims,
//! `claim_first_available` skip rules, and append-only blackboard offsets.
//!
//! Run in isolation with `cargo test --test agent_complex_support`.

#[path = "complex_support/mod.rs"]
mod complex_support;

use std::collections::BTreeMap;
use std::sync::Arc;

use agent_lib::agent::{
    ApprovalRequirement, RequirementResult, RunContext, ToolApprovalPolicy, ToolHandler,
    ToolRuntimeError,
};
use agent_lib::model::content::ContentBlock;
use agent_lib::model::tool::{ToolResponse, ToolStatus};

use agent_lib::agent::PlanId;

use agent_testkit::fixtures::{root_context, tool_call};
use agent_testkit::ids::SeqIds;

use complex_support::plan_blackboard::{
    MockPlanBlackboardStore, StoreError, TaskState, TaskStatus, detect_cycle,
};
use complex_support::tools::{
    BLACKBOARD_POST, ComplexToolHandler, DANGEROUS_WRITE, PLAN_ADD_TASK, PLAN_CLAIM, PLAN_CREATE,
    RequireDangerousWriteApprovalPolicy, SAFE_READ,
};

/// Fixed plan id so store construction stays deterministic and offline.
fn plan_id() -> PlanId {
    PlanId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a6").expect("valid plan id")
}

/// Builds a freshly created, empty plan/blackboard store.
fn store() -> MockPlanBlackboardStore {
    let store = MockPlanBlackboardStore::new(plan_id());
    store.create_plan();
    store
}

#[test]
fn plan_dependencies_reject_unknown_self_and_cycles() {
    let store = store();

    // A DAG built from back-references is accepted.
    assert_eq!(store.add_task("design", Vec::<String>::new()), Ok(1));
    assert_eq!(store.add_task("implement", ["design"]), Ok(2));

    // Referencing a task that does not exist is rejected without mutating.
    assert_eq!(
        store.add_task("ship", ["missing"]),
        Err(StoreError::UnknownTask("missing".to_owned())),
    );
    // A self-dependency is rejected.
    assert_eq!(
        store.add_task("loop", ["loop"]),
        Err(StoreError::SelfDependency("loop".to_owned())),
    );

    // Neither failed add mutated the plan: version and membership are intact.
    let plan = store.plan_snapshot();
    assert_eq!(plan.version, 2, "failed adds must not bump version");
    assert_eq!(
        plan.task_order,
        vec!["design".to_owned(), "implement".to_owned()]
    );
    assert!(!plan.tasks.contains_key("ship"));
    assert!(!plan.tasks.contains_key("loop"));

    // The real (accepted) graph is acyclic.
    assert!(detect_cycle(&plan.tasks).is_none());

    // The shared cycle detector catches a genuine multi-node cycle.
    let mut cyclic: BTreeMap<String, TaskState> = BTreeMap::new();
    cyclic.insert("a".to_owned(), TaskState::todo(vec!["b".to_owned()]));
    cyclic.insert("b".to_owned(), TaskState::todo(vec!["a".to_owned()]));
    let cycle = detect_cycle(&cyclic).expect("a<->b is a cycle");
    assert!(cycle.contains(&"a".to_owned()) && cycle.contains(&"b".to_owned()));
    assert_eq!(
        cycle.first(),
        cycle.last(),
        "a cycle path closes back on its start: {cycle:?}",
    );
}

#[test]
fn claim_rejects_unfinished_dependencies_atomically() {
    let store = store();
    store
        .add_task("design", Vec::<String>::new())
        .expect("add design");
    store
        .add_task("implement", ["design"])
        .expect("add implement");
    assert_eq!(store.version(), 2);

    // `implement` depends on the still-unfinished `design`, so the claim is
    // dependency-blocked and must change nothing.
    let blocked = store.claim("implement", "worker", 2);
    assert_eq!(
        blocked,
        Err(StoreError::DependencyBlocked {
            task: "implement".to_owned(),
            unfinished: vec!["design".to_owned()],
        }),
    );
    let plan = store.plan_snapshot();
    assert_eq!(plan.version, 2, "blocked claim must not bump version");
    let implement = &plan.tasks["implement"];
    assert_eq!(implement.status, TaskStatus::Todo, "status untouched");
    assert_eq!(implement.owner, None, "owner untouched");

    // Completing the dependency unblocks the claim.
    assert_eq!(store.claim("design", "worker", 2), Ok(3));
    assert_eq!(
        store.update_status("design", "worker", TaskStatus::Completed, 3),
        Ok(4),
    );
    assert_eq!(store.claim("implement", "worker", 4), Ok(5));

    let plan = store.plan_snapshot();
    assert_eq!(plan.tasks["implement"].status, TaskStatus::InProgress);
    assert_eq!(plan.tasks["implement"].owner.as_deref(), Some("worker"));
}

#[test]
fn claim_first_available_skips_blocked_and_claimed_items() {
    let store = store();
    // Stable order: finished, owned, blocked (on owned), free.
    store
        .add_task("finished", Vec::<String>::new())
        .expect("add finished");
    store
        .add_task("owned", Vec::<String>::new())
        .expect("add owned");
    store.add_task("blocked", ["owned"]).expect("add blocked");
    store
        .add_task("free", Vec::<String>::new())
        .expect("add free");

    // Drive `finished` to Completed and `owned` to a live claim.
    store.claim("finished", "w", 4).expect("claim finished");
    store
        .update_status("finished", "w", TaskStatus::Completed, 5)
        .expect("complete finished");
    store.claim("owned", "w2", 6).expect("claim owned");
    assert_eq!(store.version(), 7);

    // The scan must skip completed (`finished`), claimed (`owned`), and
    // dependency-blocked (`blocked`, waiting on the in-progress `owned`) items
    // and land on `free`.
    let claimed = store.claim_first_available("picker", 7);
    assert_eq!(claimed, Ok(("free".to_owned(), 8)));

    let plan = store.plan_snapshot();
    assert_eq!(plan.tasks["free"].status, TaskStatus::InProgress);
    assert_eq!(plan.tasks["free"].owner.as_deref(), Some("picker"));
    // The skipped `blocked` task was not claimed.
    assert_eq!(plan.tasks["blocked"].status, TaskStatus::Todo);
    assert_eq!(plan.tasks["blocked"].owner, None);
    assert_eq!(plan.tasks["finished"].status, TaskStatus::Completed);
    assert_eq!(plan.tasks["owned"].owner.as_deref(), Some("w2"));

    // With nothing left to claim, the entry reports NoAvailableItem.
    assert_eq!(
        store.claim_first_available("picker", 8),
        Err(StoreError::NoAvailableItem),
    );
    assert_eq!(store.version(), 8, "a failed scan must not bump version");
}

#[test]
fn blackboard_is_append_only_and_offsets_are_monotonic() {
    let store = store();

    assert_eq!(store.post("parent", "start processing"), 0);
    assert_eq!(store.post("child", "working"), 1);
    assert_eq!(store.post("parent", "changed strategy after pivot"), 2);

    let board = store.board_snapshot();
    assert_eq!(board.len(), 3);
    let offsets: Vec<u64> = board.iter().map(|message| message.offset).collect();
    assert_eq!(offsets, vec![0, 1, 2]);
    assert!(
        offsets.windows(2).all(|pair| pair[0] < pair[1]),
        "offsets must be strictly monotonic: {offsets:?}",
    );
    assert_eq!(board[0].sender, "parent");
    assert_eq!(board[1].sender, "child");
    assert_eq!(board[2].text, "changed strategy after pivot");

    // Cursor reads return the tail at and beyond the offset.
    assert_eq!(store.read_from(0).len(), 3);
    let from_one = store.read_from(1);
    assert_eq!(from_one.len(), 2);
    assert_eq!(from_one[0].offset, 1);
    assert_eq!(store.read_from(2).len(), 1);
    assert!(store.read_from(3).is_empty());

    // Appending more never rewrites earlier messages; offsets keep climbing.
    assert_eq!(store.post("child", "done"), 3);
    let board = store.board_snapshot();
    assert_eq!(board.len(), 4);
    assert_eq!(board[0].text, "start processing", "history is immutable");
    assert_eq!(board[3].offset, 3);
}

// ----- M1-2: complex tool adapter + approval policy ------------------------

/// Fixed framework-side tool call id source for adapter tests.
fn adapter_ids() -> SeqIds {
    SeqIds::new()
}

/// Concatenates the text of every text block in a tool response.
fn response_text(response: &ToolResponse) -> String {
    response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Drives one tool call through the handler and unwraps its tool-family result.
async fn call_tool(
    handler: &ComplexToolHandler,
    ids: &SeqIds,
    ctx: &RunContext,
    name: &str,
    input: serde_json::Value,
) -> Result<ToolResponse, ToolRuntimeError> {
    let call = tool_call("provider-call", name, input);
    match handler.fulfill(ids.tool_call_id(), &call, ctx).await {
        RequirementResult::Tool(inner) => inner,
        other => panic!("tool handler must return a Tool result, got {other:?}"),
    }
}

#[tokio::test]
async fn plan_tools_return_model_visible_errors() {
    let ids = adapter_ids();
    let ctx = root_context(&ids);
    let handler = ComplexToolHandler::new(Arc::new(MockPlanBlackboardStore::new(plan_id())));

    // Build a small plan through the tools themselves.
    let created = call_tool(&handler, &ids, &ctx, PLAN_CREATE, serde_json::json!({}))
        .await
        .expect("plan_create is a known tool");
    assert_eq!(created.status, ToolStatus::Ok);

    let added = call_tool(
        &handler,
        &ids,
        &ctx,
        PLAN_ADD_TASK,
        serde_json::json!({ "id": "design" }),
    )
    .await
    .expect("plan_add_task is a known tool");
    assert_eq!(added.status, ToolStatus::Ok);

    call_tool(
        &handler,
        &ids,
        &ctx,
        PLAN_ADD_TASK,
        serde_json::json!({ "id": "implement", "depends_on": ["design"] }),
    )
    .await
    .expect("plan_add_task is a known tool");

    // A store error (dependency-blocked claim) folds into a model-visible error
    // tool result, not a panic and not a runtime error.
    let blocked = call_tool(
        &handler,
        &ids,
        &ctx,
        PLAN_CLAIM,
        serde_json::json!({ "task": "implement", "owner": "worker", "expected_version": 2 }),
    )
    .await
    .expect("plan_claim is a known tool");
    assert_eq!(blocked.status, ToolStatus::Error);
    assert!(
        response_text(&blocked).contains("blocked by unfinished dependencies"),
        "claim error must surface the store message: {}",
        response_text(&blocked),
    );

    // An unknown-dependency add is likewise a model-visible error.
    let unknown_dep = call_tool(
        &handler,
        &ids,
        &ctx,
        PLAN_ADD_TASK,
        serde_json::json!({ "id": "ship", "depends_on": ["ghost"] }),
    )
    .await
    .expect("plan_add_task is a known tool");
    assert_eq!(unknown_dep.status, ToolStatus::Error);
    assert!(response_text(&unknown_dep).contains("unknown task `ghost`"));

    // A missing required argument is a model-visible error, never a panic.
    let bad_args = call_tool(
        &handler,
        &ids,
        &ctx,
        PLAN_CLAIM,
        serde_json::json!({ "owner": "worker", "expected_version": 2 }),
    )
    .await
    .expect("plan_claim is a known tool");
    assert_eq!(bad_args.status, ToolStatus::Error);
    assert!(response_text(&bad_args).contains("argument `task`"));

    // An unknown tool is the one hard failure: a tool-family runtime error.
    let unknown = call_tool(&handler, &ids, &ctx, "not_a_tool", serde_json::json!({})).await;
    assert_eq!(
        unknown,
        Err(ToolRuntimeError::UnknownTool {
            name: "not_a_tool".to_owned(),
        }),
    );
}

#[tokio::test]
async fn dangerous_write_requires_approval_and_safe_tools_do_not() {
    let ids = adapter_ids();
    let policy = RequireDangerousWriteApprovalPolicy;

    // The dangerous tool requires approval and carries a stable reason.
    let dangerous = tool_call(
        "c-danger",
        DANGEROUS_WRITE,
        serde_json::json!({ "text": "rm" }),
    );
    let requirement = policy.approval_requirement(ids.tool_call_id(), &dangerous);
    let ApprovalRequirement::RequireApproval { reason } = &requirement else {
        panic!("dangerous_write must require approval, got {requirement:?}");
    };
    assert_eq!(
        reason.as_deref(),
        Some("`dangerous_write` requires human approval")
    );

    // Every other tool auto-approves.
    for name in [
        SAFE_READ,
        PLAN_CREATE,
        PLAN_CLAIM,
        BLACKBOARD_POST,
        PLAN_ADD_TASK,
    ] {
        let call = tool_call("c-safe", name, serde_json::json!({}));
        assert_eq!(
            policy.approval_requirement(ids.tool_call_id(), &call),
            ApprovalRequirement::AutoApprove,
            "{name} must auto-approve",
        );
    }
}

#[tokio::test]
async fn dangerous_write_call_log_counts_executions() {
    let ids = adapter_ids();
    let ctx = root_context(&ids);
    let store = Arc::new(MockPlanBlackboardStore::new(plan_id()));
    let handler = ComplexToolHandler::new(Arc::clone(&store));

    // Two dangerous writes and one safe read all execute (approval is enforced
    // by the machine, not the handler; a handler call means the tool ran).
    for text in ["first", "second"] {
        let response = call_tool(
            &handler,
            &ids,
            &ctx,
            DANGEROUS_WRITE,
            serde_json::json!({ "text": text }),
        )
        .await
        .expect("dangerous_write is a known tool");
        assert_eq!(response.status, ToolStatus::Ok);
    }
    call_tool(&handler, &ids, &ctx, SAFE_READ, serde_json::json!({}))
        .await
        .expect("safe_read is a known tool");

    // The call log counts executions per tool and preserves the inputs.
    assert_eq!(handler.execution_count(DANGEROUS_WRITE), 2);
    assert_eq!(handler.execution_count(SAFE_READ), 1);
    let inputs: Vec<String> = handler
        .calls_named(DANGEROUS_WRITE)
        .iter()
        .map(|call| {
            call.input
                .get("text")
                .and_then(serde_json::Value::as_str)
                .expect("dangerous_write records its text input")
                .to_owned()
        })
        .collect();
    assert_eq!(inputs, vec!["first".to_owned(), "second".to_owned()]);

    // Each approved dangerous write left a visible blackboard side effect.
    let board = store.board_snapshot();
    assert_eq!(board.len(), 2, "both dangerous writes posted to the board");
    assert!(
        board
            .iter()
            .all(|message| message.sender == DANGEROUS_WRITE)
    );
    assert_eq!(board[0].text, "first");
    assert_eq!(board[1].text, "second");
}
