//! In-memory mock plan/blackboard vertical feature for complex agent tests.
//!
//! Production plan/blackboard APIs are not implemented yet; the agent layer only
//! exposes [`PlanId`]/`BlackboardId` identities. This module provides a minimal,
//! offline store whose semantics track [`docs/agent-layer.md`](../../docs/agent-layer.md)
//! §6.2 (plan) and §6.4 (blackboard) closely enough to pin the invariants the
//! complex mock tests care about:
//!
//! - plan tasks carry a stable id, status, optional owner, and a `depends_on`
//!   edge set that must reference known tasks, never self-depend, and never form
//!   a cycle;
//! - claiming a task is a CAS against the plan `version` plus an atomic check of
//!   owner, status, and dependency completion — a dependency-blocked claim
//!   changes nothing;
//! - `claim_first_available` scans the stable task order and claims the first
//!   task that is unclaimed, unfinished, and dependency-satisfied, or reports
//!   [`StoreError::NoAvailableItem`];
//! - the blackboard is append-only with monotonically increasing offsets.
//!
//! Every operation — success or failure — is appended to an [`StoreOp`] log so a
//! failing complex test can print the exact sequence that led to a mismatch.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use agent_lib::agent::PlanId;

/// Lifecycle status of a single plan task.
///
/// Mirrors the states named in `docs/agent-layer.md` §6.2: unclaimed
/// ([`Todo`](TaskStatus::Todo)), claimed/running
/// ([`InProgress`](TaskStatus::InProgress)), finished
/// ([`Completed`](TaskStatus::Completed)), and the two off-ramp states
/// ([`Blocked`](TaskStatus::Blocked) / [`Cancelled`](TaskStatus::Cancelled)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    /// Not yet claimed; eligible for claiming once dependencies complete.
    Todo,
    /// Claimed by an owner and being worked on.
    InProgress,
    /// Finished; satisfies dependents and can no longer be claimed.
    Completed,
    /// Parked because it cannot currently proceed.
    Blocked,
    /// Abandoned; will not be completed.
    Cancelled,
}

impl TaskStatus {
    /// Returns the lowercase wire label used in operation logs and errors.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Todo => "todo",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    /// Returns whether a status transition is a legal `update_status` move.
    ///
    /// Terminal states ([`Completed`](TaskStatus::Completed) /
    /// [`Cancelled`](TaskStatus::Cancelled)) never transition further, a task
    /// only reaches [`Completed`](TaskStatus::Completed) from
    /// [`InProgress`](TaskStatus::InProgress), and re-declaring the same status
    /// is a no-op that is allowed for idempotent updates.
    #[must_use]
    pub fn can_transition_to(self, next: TaskStatus) -> bool {
        use TaskStatus::{Blocked, Cancelled, Completed, InProgress, Todo};

        if self == next {
            return true;
        }
        match self {
            Todo => matches!(next, InProgress | Blocked | Cancelled),
            InProgress => matches!(next, Completed | Blocked | Cancelled),
            Blocked => matches!(next, Todo | InProgress | Cancelled),
            Completed | Cancelled => false,
        }
    }
}

/// State of a single plan task: its status, optional owner, and dependencies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskState {
    /// Current lifecycle status.
    pub status: TaskStatus,
    /// Owner that has claimed the task, if any.
    pub owner: Option<String>,
    /// Ids of the tasks that must be [`Completed`](TaskStatus::Completed) before
    /// this task can be claimed.
    pub depends_on: Vec<String>,
}

impl TaskState {
    /// Builds a fresh, unclaimed [`Todo`](TaskStatus::Todo) task with the given
    /// dependency edges.
    #[must_use]
    pub fn todo(depends_on: Vec<String>) -> Self {
        Self {
            status: TaskStatus::Todo,
            owner: None,
            depends_on,
        }
    }
}

/// The mutable plan board: an ordered, versioned set of tasks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanState {
    /// Identity of the plan.
    pub id: PlanId,
    /// Monotonically increasing version; every successful mutation bumps it.
    pub version: u64,
    /// Stable creation/display order used by `claim_first_available`.
    pub task_order: Vec<String>,
    /// Task states keyed by task id.
    pub tasks: BTreeMap<String, TaskState>,
}

impl PlanState {
    /// Creates an empty plan at version `0`.
    #[must_use]
    pub fn empty(id: PlanId) -> Self {
        Self {
            id,
            version: 0,
            task_order: Vec::new(),
            tasks: BTreeMap::new(),
        }
    }

    /// Returns whether every id in `depends_on` is [`Completed`](TaskStatus::Completed).
    fn dependencies_satisfied(&self, depends_on: &[String]) -> bool {
        depends_on.iter().all(|dep| {
            self.tasks
                .get(dep)
                .is_some_and(|task| task.status == TaskStatus::Completed)
        })
    }

    /// Returns the dependency ids that are not yet completed, in declared order.
    fn unfinished_dependencies(&self, depends_on: &[String]) -> Vec<String> {
        depends_on
            .iter()
            .filter(|dep| {
                self.tasks
                    .get(*dep)
                    .is_none_or(|task| task.status != TaskStatus::Completed)
            })
            .cloned()
            .collect()
    }
}

/// A single append-only blackboard message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardMessage {
    /// Zero-based, monotonically increasing position in the message log.
    pub offset: u64,
    /// Author label of the message.
    pub sender: String,
    /// Message body.
    pub text: String,
}

/// A recorded store operation and its outcome.
///
/// The `outcome` carries a human-readable summary on success and the
/// model-visible error text on failure, so a printed [`StoreOp`] log reads as a
/// linear transcript of what the mock store did and why any step failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreOp {
    /// Which logical operation ran.
    pub kind: OpKind,
    /// `Ok(summary)` when the operation succeeded, `Err(message)` otherwise.
    pub outcome: Result<String, String>,
}

/// The logical operations the store records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpKind {
    /// `create_plan`.
    CreatePlan,
    /// `add_task(id, depends_on)`.
    AddTask {
        /// Task id being added.
        id: String,
        /// Declared dependency edges.
        depends_on: Vec<String>,
    },
    /// `claim(task, owner, expected_version)`.
    Claim {
        /// Task the caller tried to claim.
        task: String,
        /// Owner attempting the claim.
        owner: String,
        /// Plan version the caller expected.
        expected_version: u64,
    },
    /// `claim_first_available(owner, expected_version)`.
    ClaimFirst {
        /// Owner attempting the claim.
        owner: String,
        /// Plan version the caller expected.
        expected_version: u64,
    },
    /// `update_status(task, owner, status, expected_version)`.
    UpdateStatus {
        /// Task being updated.
        task: String,
        /// Owner performing the update.
        owner: String,
        /// Requested next status.
        status: TaskStatus,
        /// Plan version the caller expected.
        expected_version: u64,
    },
    /// `post(sender, text)`.
    Post {
        /// Author of the posted message.
        sender: String,
        /// Message body.
        text: String,
    },
    /// `read_from(offset)`.
    Read {
        /// Cursor the caller read from.
        from: u64,
    },
}

/// Errors returned by the mock plan/blackboard store.
///
/// Every variant renders (via [`Display`](std::fmt::Display)) to a compact,
/// model-visible string so the M1-2 tool adapter can surface it as tool-error
/// text without leaking Rust types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// A dependency (or claim/update target) referenced an unknown task id.
    UnknownTask(String),
    /// A task declared itself as one of its own dependencies.
    SelfDependency(String),
    /// Adding a task would close a dependency cycle through the listed path.
    DependencyCycle(Vec<String>),
    /// A task with the given id already exists.
    DuplicateTask(String),
    /// The plan version did not match the caller's `expected_version`.
    VersionConflict {
        /// Version the caller expected.
        expected: u64,
        /// Version the plan actually held.
        actual: u64,
    },
    /// The task is already claimed by a different owner.
    AlreadyClaimed {
        /// Task in question.
        task: String,
        /// Current owner.
        owner: String,
    },
    /// The claim/update cannot proceed because dependencies are unfinished.
    DependencyBlocked {
        /// Task the caller targeted.
        task: String,
        /// Dependency ids that are not yet completed.
        unfinished: Vec<String>,
    },
    /// An update targeted a task the caller does not own.
    NotOwner {
        /// Task in question.
        task: String,
        /// Owner the caller presented.
        expected: String,
        /// Owner the plan actually recorded, if any.
        actual: Option<String>,
    },
    /// The requested status transition is not legal.
    InvalidTransition {
        /// Task in question.
        task: String,
        /// Current status.
        from: TaskStatus,
        /// Requested status.
        to: TaskStatus,
    },
    /// No task is currently available to claim.
    NoAvailableItem,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::UnknownTask(id) => write!(formatter, "unknown task `{id}`"),
            StoreError::SelfDependency(id) => {
                write!(formatter, "task `{id}` cannot depend on itself")
            }
            StoreError::DependencyCycle(path) => {
                write!(formatter, "dependency cycle: {}", path.join(" -> "))
            }
            StoreError::DuplicateTask(id) => write!(formatter, "task `{id}` already exists"),
            StoreError::VersionConflict { expected, actual } => write!(
                formatter,
                "version conflict: expected {expected}, found {actual}"
            ),
            StoreError::AlreadyClaimed { task, owner } => {
                write!(formatter, "task `{task}` already claimed by `{owner}`")
            }
            StoreError::DependencyBlocked { task, unfinished } => write!(
                formatter,
                "task `{task}` blocked by unfinished dependencies [{}]",
                unfinished.join(", ")
            ),
            StoreError::NotOwner {
                task,
                expected,
                actual,
            } => write!(
                formatter,
                "task `{task}` is owned by {}, not `{expected}`",
                actual
                    .as_deref()
                    .map_or_else(|| "nobody".to_owned(), |owner| format!("`{owner}`"))
            ),
            StoreError::InvalidTransition { task, from, to } => write!(
                formatter,
                "task `{task}` cannot move from {} to {}",
                from.label(),
                to.label()
            ),
            StoreError::NoAvailableItem => {
                write!(formatter, "no available task to claim")
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// Detects a dependency cycle in `tasks` and returns one offending path.
///
/// The returned path lists the ids around the cycle, ending with the id it
/// closes back onto (for example `["a", "b", "a"]`). Returns `None` when the
/// `depends_on` edges form a DAG.
///
/// The mock store builds its graph by only allowing edges to already-known
/// tasks, so `add_task` alone cannot form a multi-node cycle; this shared
/// detector still runs inside `add_task` as a defensive check and is unit-tested
/// directly against hand-built cyclic graphs.
#[must_use]
pub fn detect_cycle(tasks: &BTreeMap<String, TaskState>) -> Option<Vec<String>> {
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = Vec::new();
    let mut on_stack: BTreeSet<String> = BTreeSet::new();

    for root in tasks.keys() {
        if visited.contains(root) {
            continue;
        }
        if let Some(cycle) = visit(root, tasks, &mut visited, &mut stack, &mut on_stack) {
            return Some(cycle);
        }
    }
    None
}

/// Depth-first helper for [`detect_cycle`].
fn visit(
    node: &str,
    tasks: &BTreeMap<String, TaskState>,
    visited: &mut BTreeSet<String>,
    stack: &mut Vec<String>,
    on_stack: &mut BTreeSet<String>,
) -> Option<Vec<String>> {
    visited.insert(node.to_owned());
    stack.push(node.to_owned());
    on_stack.insert(node.to_owned());

    if let Some(task) = tasks.get(node) {
        for dep in &task.depends_on {
            if on_stack.contains(dep) {
                let start = stack.iter().position(|id| id == dep).unwrap_or(0);
                let mut cycle = stack[start..].to_vec();
                cycle.push(dep.clone());
                return Some(cycle);
            }
            if !visited.contains(dep)
                && let Some(cycle) = visit(dep, tasks, visited, stack, on_stack)
            {
                return Some(cycle);
            }
        }
    }

    stack.pop();
    on_stack.remove(node);
    None
}

/// In-memory plan + blackboard store used by the complex mock tests.
///
/// See the [module docs](self) for the modeled semantics. All state lives behind
/// `Mutex`es so the store can be shared (`Arc`) across a parent agent and its
/// subagents without changing the observable single-writer semantics of any one
/// operation.
#[derive(Debug)]
pub struct MockPlanBlackboardStore {
    /// The mutable plan board.
    plan: Mutex<PlanState>,
    /// The append-only blackboard message log.
    board: Mutex<Vec<BoardMessage>>,
    /// Ordered log of every operation, success or failure.
    ops: Mutex<Vec<StoreOp>>,
}

impl MockPlanBlackboardStore {
    /// Creates a store holding an empty plan (version `0`) for `plan_id` and an
    /// empty blackboard.
    #[must_use]
    pub fn new(plan_id: PlanId) -> Self {
        Self {
            plan: Mutex::new(PlanState::empty(plan_id)),
            board: Mutex::new(Vec::new()),
            ops: Mutex::new(Vec::new()),
        }
    }

    /// Locks the plan, panicking with the op log on poison.
    fn plan(&self) -> std::sync::MutexGuard<'_, PlanState> {
        self.plan
            .lock()
            .unwrap_or_else(|_| panic!("plan mutex poisoned; ops:\n{}", self.ops_summary()))
    }

    /// Locks the blackboard, panicking with the op log on poison.
    fn board(&self) -> std::sync::MutexGuard<'_, Vec<BoardMessage>> {
        self.board
            .lock()
            .unwrap_or_else(|_| panic!("board mutex poisoned; ops:\n{}", self.ops_summary()))
    }

    /// Locks the op log, panicking on poison.
    fn ops_guard(&self) -> std::sync::MutexGuard<'_, Vec<StoreOp>> {
        self.ops.lock().expect("ops mutex poisoned")
    }

    /// Records one operation and its outcome.
    fn record(&self, kind: OpKind, outcome: Result<String, String>) {
        self.ops_guard().push(StoreOp { kind, outcome });
    }

    /// Records `result` under `kind` and returns it unchanged, converting the
    /// `Ok`/`Err` payload into the logged summary/message.
    fn record_result<T>(
        &self,
        kind: OpKind,
        summary: impl FnOnce(&T) -> String,
        result: Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let outcome = match &result {
            Ok(value) => Ok(summary(value)),
            Err(error) => Err(error.to_string()),
        };
        self.record(kind, outcome);
        result
    }

    /// Returns a snapshot of the plan state.
    #[must_use]
    pub fn plan_snapshot(&self) -> PlanState {
        self.plan().clone()
    }

    /// Returns the current plan version.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.plan().version
    }

    /// Returns a snapshot of the operation log.
    #[must_use]
    pub fn ops(&self) -> Vec<StoreOp> {
        self.ops_guard().clone()
    }

    /// Renders the operation log as a numbered, human-readable transcript.
    ///
    /// Assertion helpers embed this in panic messages so a failing complex test
    /// shows exactly which store operations ran and how each resolved.
    #[must_use]
    pub fn ops_summary(&self) -> String {
        let ops = self.ops_guard();
        if ops.is_empty() {
            return "  <no store operations recorded>".to_owned();
        }
        ops.iter()
            .enumerate()
            .map(|(index, op)| {
                let status = match &op.outcome {
                    Ok(summary) => format!("ok: {summary}"),
                    Err(message) => format!("err: {message}"),
                };
                format!("  {index:>3}. {:?} -> {status}", op.kind)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ----- plan operations -------------------------------------------------

    /// Initializes the plan to version `0` with no tasks and records the op.
    ///
    /// Returns the plan id and its (zero) version, mirroring a `plan_create`
    /// tool call.
    pub fn create_plan(&self) -> (PlanId, u64) {
        let id = {
            let mut plan = self.plan();
            plan.version = 0;
            plan.task_order.clear();
            plan.tasks.clear();
            plan.id
        };
        self.record(OpKind::CreatePlan, Ok(format!("plan {id} v0")));
        (id, 0)
    }

    /// Adds a new task with the given dependency edges.
    ///
    /// Validates that the id is fresh, that no dependency is the task itself,
    /// that every dependency references a known task, and (defensively) that the
    /// resulting graph stays acyclic. On success the task is appended to the
    /// stable order and the plan version is incremented.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::DuplicateTask`], [`StoreError::SelfDependency`],
    /// [`StoreError::UnknownTask`], or [`StoreError::DependencyCycle`] without
    /// mutating the plan.
    pub fn add_task(
        &self,
        id: impl Into<String>,
        depends_on: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<u64, StoreError> {
        let id = id.into();
        let depends_on: Vec<String> = depends_on.into_iter().map(Into::into).collect();
        let kind = OpKind::AddTask {
            id: id.clone(),
            depends_on: depends_on.clone(),
        };

        let result = (|| {
            let mut plan = self.plan();
            if plan.tasks.contains_key(&id) {
                return Err(StoreError::DuplicateTask(id.clone()));
            }
            if depends_on.iter().any(|dep| dep == &id) {
                return Err(StoreError::SelfDependency(id.clone()));
            }
            if let Some(unknown) = depends_on.iter().find(|dep| !plan.tasks.contains_key(*dep)) {
                return Err(StoreError::UnknownTask(unknown.clone()));
            }

            let mut candidate = plan.tasks.clone();
            candidate.insert(id.clone(), TaskState::todo(depends_on.clone()));
            if let Some(cycle) = detect_cycle(&candidate) {
                return Err(StoreError::DependencyCycle(cycle));
            }

            plan.tasks
                .insert(id.clone(), TaskState::todo(depends_on.clone()));
            plan.task_order.push(id.clone());
            plan.version += 1;
            Ok(plan.version)
        })();

        self.record_result(
            kind,
            |version| format!("task `{id}` added at v{version}"),
            result,
        )
    }

    /// Claims `task_id` for `owner` under an optimistic version check.
    ///
    /// The claim only proceeds when the plan is at `expected_version`, the task
    /// is unclaimed (or already owned by `owner`), the task is claimable, and
    /// every dependency is completed. Any failed check leaves owner, status, and
    /// version untouched. On success the task moves to
    /// [`InProgress`](TaskStatus::InProgress) and the version is incremented.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::VersionConflict`], [`StoreError::UnknownTask`],
    /// [`StoreError::AlreadyClaimed`], [`StoreError::InvalidTransition`], or
    /// [`StoreError::DependencyBlocked`].
    pub fn claim(
        &self,
        task_id: impl Into<String>,
        owner: impl Into<String>,
        expected_version: u64,
    ) -> Result<u64, StoreError> {
        let task_id = task_id.into();
        let owner = owner.into();
        let kind = OpKind::Claim {
            task: task_id.clone(),
            owner: owner.clone(),
            expected_version,
        };

        let result = (|| {
            let mut plan = self.plan();
            if plan.version != expected_version {
                return Err(StoreError::VersionConflict {
                    expected: expected_version,
                    actual: plan.version,
                });
            }
            let current = plan
                .tasks
                .get(&task_id)
                .ok_or_else(|| StoreError::UnknownTask(task_id.clone()))?;
            if let Some(existing) = &current.owner
                && existing != &owner
            {
                return Err(StoreError::AlreadyClaimed {
                    task: task_id.clone(),
                    owner: existing.clone(),
                });
            }
            if !current.status.can_transition_to(TaskStatus::InProgress) {
                return Err(StoreError::InvalidTransition {
                    task: task_id.clone(),
                    from: current.status,
                    to: TaskStatus::InProgress,
                });
            }
            let depends_on = current.depends_on.clone();
            let unfinished = plan.unfinished_dependencies(&depends_on);
            if !unfinished.is_empty() {
                return Err(StoreError::DependencyBlocked {
                    task: task_id.clone(),
                    unfinished,
                });
            }

            let task = plan
                .tasks
                .get_mut(&task_id)
                .expect("task presence checked above");
            task.owner = Some(owner.clone());
            task.status = TaskStatus::InProgress;
            plan.version += 1;
            Ok(plan.version)
        })();

        self.record_result(
            kind,
            |version| format!("`{task_id}` claimed by `{owner}` at v{version}"),
            result,
        )
    }

    /// Claims the first available task in stable order for `owner`.
    ///
    /// Scans `task_order`, skipping completed tasks, tasks already owned, and
    /// tasks whose dependencies are unfinished, and atomically claims the first
    /// [`Todo`](TaskStatus::Todo) task that is dependency-satisfied. Reports the
    /// claimed task id and the new version.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::VersionConflict`] when the plan moved, or
    /// [`StoreError::NoAvailableItem`] when nothing is claimable.
    pub fn claim_first_available(
        &self,
        owner: impl Into<String>,
        expected_version: u64,
    ) -> Result<(String, u64), StoreError> {
        let owner = owner.into();
        let kind = OpKind::ClaimFirst {
            owner: owner.clone(),
            expected_version,
        };

        let result = (|| {
            let mut plan = self.plan();
            if plan.version != expected_version {
                return Err(StoreError::VersionConflict {
                    expected: expected_version,
                    actual: plan.version,
                });
            }

            let choice = plan.task_order.iter().find(|id| {
                plan.tasks.get(*id).is_some_and(|task| {
                    task.status == TaskStatus::Todo
                        && task.owner.is_none()
                        && plan.dependencies_satisfied(&task.depends_on)
                })
            });
            let Some(task_id) = choice.cloned() else {
                return Err(StoreError::NoAvailableItem);
            };

            let task = plan
                .tasks
                .get_mut(&task_id)
                .expect("scanned task must exist");
            task.owner = Some(owner.clone());
            task.status = TaskStatus::InProgress;
            plan.version += 1;
            Ok((task_id, plan.version))
        })();

        self.record_result(
            kind,
            |(task_id, version)| format!("`{task_id}` claimed by `{owner}` at v{version}"),
            result,
        )
    }

    /// Updates the status of a task the caller owns.
    ///
    /// Requires the plan to be at `expected_version`, the task to be owned by
    /// `owner`, and the requested transition to be legal. On success the status
    /// changes and the version is incremented.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::VersionConflict`], [`StoreError::UnknownTask`],
    /// [`StoreError::NotOwner`], or [`StoreError::InvalidTransition`].
    pub fn update_status(
        &self,
        task_id: impl Into<String>,
        owner: impl Into<String>,
        status: TaskStatus,
        expected_version: u64,
    ) -> Result<u64, StoreError> {
        let task_id = task_id.into();
        let owner = owner.into();
        let kind = OpKind::UpdateStatus {
            task: task_id.clone(),
            owner: owner.clone(),
            status,
            expected_version,
        };

        let result = (|| {
            let mut plan = self.plan();
            if plan.version != expected_version {
                return Err(StoreError::VersionConflict {
                    expected: expected_version,
                    actual: plan.version,
                });
            }
            let current = plan
                .tasks
                .get(&task_id)
                .ok_or_else(|| StoreError::UnknownTask(task_id.clone()))?;
            if current.owner.as_deref() != Some(owner.as_str()) {
                return Err(StoreError::NotOwner {
                    task: task_id.clone(),
                    expected: owner.clone(),
                    actual: current.owner.clone(),
                });
            }
            if !current.status.can_transition_to(status) {
                return Err(StoreError::InvalidTransition {
                    task: task_id.clone(),
                    from: current.status,
                    to: status,
                });
            }

            let task = plan
                .tasks
                .get_mut(&task_id)
                .expect("task presence checked above");
            task.status = status;
            plan.version += 1;
            Ok(plan.version)
        })();

        self.record_result(
            kind,
            |version| format!("`{task_id}` -> {} at v{version}", status.label()),
            result,
        )
    }

    // ----- blackboard operations ------------------------------------------

    /// Appends a message to the blackboard and returns its offset.
    ///
    /// Offsets start at `0` and increase by one per message; the blackboard is
    /// append-only and exposes no delete or update path.
    pub fn post(&self, sender: impl Into<String>, text: impl Into<String>) -> u64 {
        let sender = sender.into();
        let text = text.into();
        let offset = {
            let mut board = self.board();
            let offset = board.len() as u64;
            board.push(BoardMessage {
                offset,
                sender: sender.clone(),
                text: text.clone(),
            });
            offset
        };
        self.record(
            OpKind::Post { sender, text },
            Ok(format!("offset {offset}")),
        );
        offset
    }

    /// Reads all messages at `offset` and beyond, in order.
    #[must_use]
    pub fn read_from(&self, offset: u64) -> Vec<BoardMessage> {
        let messages: Vec<BoardMessage> = self
            .board()
            .iter()
            .filter(|message| message.offset >= offset)
            .cloned()
            .collect();
        self.record(
            OpKind::Read { from: offset },
            Ok(format!("{} message(s)", messages.len())),
        );
        messages
    }

    /// Returns a snapshot of every blackboard message.
    #[must_use]
    pub fn board_snapshot(&self) -> Vec<BoardMessage> {
        self.board().clone()
    }
}
