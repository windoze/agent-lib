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

use agent_lib::agent::PlanId;

use complex_support::plan_blackboard::{
    MockPlanBlackboardStore, StoreError, TaskState, TaskStatus, detect_cycle,
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
