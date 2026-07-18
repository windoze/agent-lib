//! First-class plan board vertical feature (design `agent-layer.md` §6.2).
//!
//! A [`Plan`] is "办公室里的计划板": a public, inspectable task board that only
//! records *what tasks exist, what state each is in, and who claimed which*. It
//! deliberately holds **no executor** — advancing work is the host's / agent
//! loop's job (design §6.2 "plan 自己不推进任何东西"). The invariants it *does*
//! enforce are the ones a plain append-only [`Blackboard`](super::Blackboard)
//! cannot give:
//!
//! - tasks carry a stable id, a [`TaskStatus`], an optional owner, and a
//!   `depends_on` edge set that must reference known tasks, never self-depend,
//!   and never close a cycle;
//! - [`claim`](Plan::claim) is an optimistic CAS against the plan
//!   [`version`](Plan::version) **plus** an atomic check that the task is
//!   unclaimed / claimable and that every dependency is
//!   [`Completed`](TaskStatus::Completed) — a dependency-blocked claim changes
//!   nothing (design §6.2 "认领需要原子性与依赖检查");
//! - [`claim_first_available`](Plan::claim_first_available) scans the stable
//!   creation order and claims the first task that is unclaimed, unfinished, and
//!   dependency-satisfied, or reports [`PlanError::NoAvailableItem`] (design §6.2
//!   claim-first 入口).
//!
//! The live [`Plan`] holds its mutable board behind a `Mutex` so it can be shared
//! (`Arc`) across a coordinator and its workers while keeping every single
//! operation a single-writer transaction. The serde-friendly *data* is exposed
//! through [`PlanSnapshot`] / [`TaskSnapshot`]; the live handle itself is not a
//! serde type (design §5 API-first: data + live API are separate).

use crate::agent::id::PlanId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use thiserror::Error;

/// Lifecycle status of a single plan task (design §6.2).
///
/// The states are the ones named in `agent-layer.md` §6.2: unclaimed
/// ([`Todo`](Self::Todo)), claimed/running ([`InProgress`](Self::InProgress)),
/// finished ([`Completed`](Self::Completed)), and the two off-ramps
/// ([`Blocked`](Self::Blocked) / [`Cancelled`](Self::Cancelled)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    /// Returns the lowercase wire label used in tool output and errors.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parses a lowercase wire label (as produced by [`label`](Self::label))
    /// back into a [`TaskStatus`], returning `None` for an unknown label.
    ///
    /// The [`plan_update`](super::tools::PLAN_UPDATE) tool adapter uses this to
    /// turn a model-supplied `status` string into a typed status.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "todo" => Some(Self::Todo),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "blocked" => Some(Self::Blocked),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Returns whether a status transition is a legal
    /// [`update_status`](Plan::update_status) move.
    ///
    /// Terminal states ([`Completed`](Self::Completed) /
    /// [`Cancelled`](Self::Cancelled)) never transition further, a task only
    /// reaches [`Completed`](Self::Completed) from
    /// [`InProgress`](Self::InProgress), and re-declaring the same status is an
    /// allowed idempotent no-op.
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Todo => matches!(next, Self::InProgress | Self::Blocked | Self::Cancelled),
            Self::InProgress => matches!(next, Self::Completed | Self::Blocked | Self::Cancelled),
            Self::Blocked => matches!(next, Self::Todo | Self::InProgress | Self::Cancelled),
            Self::Completed | Self::Cancelled => false,
        }
    }
}

/// Serde-friendly snapshot of a single plan task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    /// Current lifecycle status.
    pub status: TaskStatus,
    /// Owner that has claimed the task, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Ids of the tasks that must be [`Completed`](TaskStatus::Completed) before
    /// this task can be claimed.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl TaskSnapshot {
    /// Builds a fresh, unclaimed [`Todo`](TaskStatus::Todo) task.
    fn todo(depends_on: Vec<String>) -> Self {
        Self {
            status: TaskStatus::Todo,
            owner: None,
            depends_on,
        }
    }
}

/// Serde-friendly snapshot of the whole plan board.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSnapshot {
    /// Identity of the plan.
    pub id: PlanId,
    /// Monotonically increasing version; every successful mutation bumps it.
    pub version: u64,
    /// Stable creation/display order used by
    /// [`claim_first_available`](Plan::claim_first_available).
    pub task_order: Vec<String>,
    /// Task states keyed by task id.
    pub tasks: BTreeMap<String, TaskSnapshot>,
}

impl PlanSnapshot {
    /// Returns whether every id in `depends_on` is
    /// [`Completed`](TaskStatus::Completed).
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

/// Classified failure from a [`Plan`] operation (design §6.2 分类错误).
///
/// Every variant renders through [`Display`](std::fmt::Display) to a compact,
/// model-visible string so a tool adapter can surface it as tool-error text
/// without leaking Rust types.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PlanError {
    /// A dependency (or claim/update target) referenced an unknown task id.
    #[error("unknown task `{0}`")]
    UnknownTask(String),
    /// A task declared itself as one of its own dependencies.
    #[error("task `{0}` cannot depend on itself")]
    SelfDependency(String),
    /// Adding a task would close a dependency cycle through the listed path.
    #[error("dependency cycle: {}", .0.join(" -> "))]
    DependencyCycle(Vec<String>),
    /// A task with the given id already exists.
    #[error("task `{0}` already exists")]
    DuplicateTask(String),
    /// The plan version did not match the caller's `expected_version`.
    #[error("version conflict: expected {expected}, found {actual}")]
    VersionConflict {
        /// Version the caller expected.
        expected: u64,
        /// Version the plan actually held.
        actual: u64,
    },
    /// The task is already claimed by a different owner.
    #[error("task `{task}` already claimed by `{owner}`")]
    AlreadyClaimed {
        /// Task in question.
        task: String,
        /// Current owner.
        owner: String,
    },
    /// The claim/update cannot proceed because dependencies are unfinished.
    #[error("task `{task}` blocked by unfinished dependencies [{}]", .unfinished.join(", "))]
    DependencyBlocked {
        /// Task the caller targeted.
        task: String,
        /// Dependency ids that are not yet completed.
        unfinished: Vec<String>,
    },
    /// An update targeted a task the caller does not own.
    #[error("task `{task}` is not owned by `{expected}`")]
    NotOwner {
        /// Task in question.
        task: String,
        /// Owner the caller presented.
        expected: String,
        /// Owner the plan actually recorded, if any.
        actual: Option<String>,
    },
    /// The requested status transition is not legal.
    #[error("task `{task}` cannot move from {} to {}", .from.label(), .to.label())]
    InvalidTransition {
        /// Task in question.
        task: String,
        /// Current status.
        from: TaskStatus,
        /// Requested status.
        to: TaskStatus,
    },
    /// No task is currently available to claim.
    #[error("no available task to claim")]
    NoAvailableItem,
}

/// Detects a dependency cycle in `tasks` and returns one offending path.
///
/// The returned path lists the ids around the cycle, ending with the id it
/// closes back onto (for example `["a", "b", "a"]`). Returns `None` when the
/// `depends_on` edges form a DAG.
#[must_use]
fn detect_cycle(tasks: &BTreeMap<String, TaskSnapshot>) -> Option<Vec<String>> {
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
    tasks: &BTreeMap<String, TaskSnapshot>,
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

/// A live, shareable plan board (design §6.2).
///
/// The mutable [`PlanSnapshot`] lives behind a `Mutex` so a plan can be wrapped
/// in an `Arc` and shared between a coordinator agent and its workers without
/// changing the single-writer semantics of any one operation. Read the
/// serde-friendly state with [`snapshot`](Self::snapshot); mutating operations
/// return the plan's new [`version`](Self::version) so a caller can chain further
/// CAS operations.
#[derive(Debug)]
pub struct Plan {
    board: Mutex<PlanSnapshot>,
}

impl Plan {
    /// Creates an empty plan (version `0`) for `id`.
    #[must_use]
    pub fn new(id: PlanId) -> Self {
        Self {
            board: Mutex::new(PlanSnapshot {
                id,
                version: 0,
                task_order: Vec::new(),
                tasks: BTreeMap::new(),
            }),
        }
    }

    /// Rebuilds a plan board from a data-only [`PlanSnapshot`].
    ///
    /// The [`PlanSnapshot`] already carries the full board state — the plan
    /// [`id`](PlanSnapshot::id), the [`version`](PlanSnapshot::version) counter,
    /// the stable [`task_order`](PlanSnapshot::task_order), and every
    /// [`TaskSnapshot`] — so restore is a direct rehydration: the resumed plan
    /// keeps its version so a subsequent CAS [`claim`](Self::claim) still needs
    /// the caller's `expected_version` to match.
    #[must_use]
    pub fn from_snapshot(snapshot: PlanSnapshot) -> Self {
        Self {
            board: Mutex::new(snapshot),
        }
    }

    /// Locks the board, recovering the guard even if a prior holder panicked.
    ///
    /// A poisoned lock only means some earlier operation panicked mid-way; the
    /// board's own invariants are still upheld by each transaction, so recovering
    /// the guard keeps the shared plan usable rather than cascading the panic to
    /// every other agent holding the `Arc`.
    fn board(&self) -> std::sync::MutexGuard<'_, PlanSnapshot> {
        self.board
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Returns the plan identity.
    #[must_use]
    pub fn id(&self) -> PlanId {
        self.board().id
    }

    /// Returns the current plan version.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.board().version
    }

    /// Returns a snapshot of the plan state.
    #[must_use]
    pub fn snapshot(&self) -> PlanSnapshot {
        self.board().clone()
    }

    /// Adds a new task with the given dependency edges and returns the new
    /// version.
    ///
    /// Validates that the id is fresh, that no dependency is the task itself,
    /// that every dependency references a known task, and (defensively) that the
    /// resulting graph stays acyclic. On success the task is appended to the
    /// stable order and the plan version is incremented.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::DuplicateTask`], [`PlanError::SelfDependency`],
    /// [`PlanError::UnknownTask`], or [`PlanError::DependencyCycle`] without
    /// mutating the plan.
    pub fn add_task(
        &self,
        id: impl Into<String>,
        depends_on: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<u64, PlanError> {
        let id = id.into();
        let depends_on: Vec<String> = depends_on.into_iter().map(Into::into).collect();
        let mut plan = self.board();

        if plan.tasks.contains_key(&id) {
            return Err(PlanError::DuplicateTask(id));
        }
        if depends_on.iter().any(|dep| dep == &id) {
            return Err(PlanError::SelfDependency(id));
        }
        if let Some(unknown) = depends_on.iter().find(|dep| !plan.tasks.contains_key(*dep)) {
            return Err(PlanError::UnknownTask(unknown.clone()));
        }

        let mut candidate = plan.tasks.clone();
        candidate.insert(id.clone(), TaskSnapshot::todo(depends_on.clone()));
        if let Some(cycle) = detect_cycle(&candidate) {
            return Err(PlanError::DependencyCycle(cycle));
        }

        plan.tasks
            .insert(id.clone(), TaskSnapshot::todo(depends_on));
        plan.task_order.push(id);
        plan.version += 1;
        Ok(plan.version)
    }

    /// Claims `task_id` for `owner` under an optimistic version check.
    ///
    /// The claim only proceeds when the plan is at `expected_version`, the task
    /// is unclaimed (or already owned by `owner`), the task is claimable, and
    /// every dependency is completed. Any failed check leaves owner, status, and
    /// version untouched (design §6.2). On success the task moves to
    /// [`InProgress`](TaskStatus::InProgress) and the version is incremented.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::VersionConflict`], [`PlanError::UnknownTask`],
    /// [`PlanError::AlreadyClaimed`], [`PlanError::InvalidTransition`], or
    /// [`PlanError::DependencyBlocked`].
    pub fn claim(
        &self,
        task_id: impl Into<String>,
        owner: impl Into<String>,
        expected_version: u64,
    ) -> Result<u64, PlanError> {
        let task_id = task_id.into();
        let owner = owner.into();
        let mut plan = self.board();

        if plan.version != expected_version {
            return Err(PlanError::VersionConflict {
                expected: expected_version,
                actual: plan.version,
            });
        }
        let current = plan
            .tasks
            .get(&task_id)
            .ok_or_else(|| PlanError::UnknownTask(task_id.clone()))?;
        if let Some(existing) = &current.owner
            && existing != &owner
        {
            return Err(PlanError::AlreadyClaimed {
                task: task_id,
                owner: existing.clone(),
            });
        }
        if !current.status.can_transition_to(TaskStatus::InProgress) {
            return Err(PlanError::InvalidTransition {
                task: task_id,
                from: current.status,
                to: TaskStatus::InProgress,
            });
        }
        let depends_on = current.depends_on.clone();
        let unfinished = plan.unfinished_dependencies(&depends_on);
        if !unfinished.is_empty() {
            return Err(PlanError::DependencyBlocked {
                task: task_id,
                unfinished,
            });
        }

        let task = plan
            .tasks
            .get_mut(&task_id)
            .expect("task presence checked above");
        task.owner = Some(owner);
        task.status = TaskStatus::InProgress;
        plan.version += 1;
        Ok(plan.version)
    }

    /// Claims the first available task in stable order for `owner`.
    ///
    /// Scans `task_order`, skipping completed / claimed / dependency-blocked
    /// tasks, and atomically claims the first [`Todo`](TaskStatus::Todo) task
    /// whose dependencies are satisfied. Returns the claimed task id and the new
    /// version.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::VersionConflict`] when the plan moved, or
    /// [`PlanError::NoAvailableItem`] when nothing is claimable.
    pub fn claim_first_available(
        &self,
        owner: impl Into<String>,
        expected_version: u64,
    ) -> Result<(String, u64), PlanError> {
        let owner = owner.into();
        let mut plan = self.board();

        if plan.version != expected_version {
            return Err(PlanError::VersionConflict {
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
            return Err(PlanError::NoAvailableItem);
        };

        let task = plan
            .tasks
            .get_mut(&task_id)
            .expect("scanned task must exist");
        task.owner = Some(owner);
        task.status = TaskStatus::InProgress;
        plan.version += 1;
        Ok((task_id, plan.version))
    }

    /// Updates the status of a task the caller owns and returns the new version.
    ///
    /// Requires the plan to be at `expected_version`, the task to be owned by
    /// `owner`, and the requested transition to be legal. On success the status
    /// changes and the version is incremented.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::VersionConflict`], [`PlanError::UnknownTask`],
    /// [`PlanError::NotOwner`], or [`PlanError::InvalidTransition`].
    pub fn update_status(
        &self,
        task_id: impl Into<String>,
        owner: impl Into<String>,
        status: TaskStatus,
        expected_version: u64,
    ) -> Result<u64, PlanError> {
        let task_id = task_id.into();
        let owner = owner.into();
        let mut plan = self.board();

        if plan.version != expected_version {
            return Err(PlanError::VersionConflict {
                expected: expected_version,
                actual: plan.version,
            });
        }
        let current = plan
            .tasks
            .get(&task_id)
            .ok_or_else(|| PlanError::UnknownTask(task_id.clone()))?;
        if current.owner.as_deref() != Some(owner.as_str()) {
            return Err(PlanError::NotOwner {
                task: task_id,
                expected: owner,
                actual: current.owner.clone(),
            });
        }
        if !current.status.can_transition_to(status) {
            return Err(PlanError::InvalidTransition {
                task: task_id,
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
    }
}
