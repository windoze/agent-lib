//! Run-level context shared by Agent loops, tools, and child agents.
//!
//! [`RunContext`] deliberately contains live handles and therefore is not a
//! serde data type. Persist the data returned by [`BudgetHandle::snapshot`] and
//! [`TraceHandle::records`] instead of trying to serialize the context itself.
//!
//! ```compile_fail
//! use agent_lib::agent::{BudgetLimits, RunContext, RunId, TraceNodeId};
//!
//! let run_id: RunId = "018f0d9c-7b6a-7c12-8f31-1234567890c1"
//!     .parse()
//!     .unwrap();
//! let context = RunContext::new_root(
//!     run_id,
//!     BudgetLimits::default(),
//!     TraceNodeId::new("root"),
//! );
//!
//! let _encoded = serde_json::to_string(&context).unwrap();
//! ```

mod budget;
mod cancel;
mod trace;

#[cfg(test)]
mod tests;

use crate::{agent::id::RunId, model::usage::Usage};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

pub use budget::{
    BudgetCharge, BudgetDimension, BudgetError, BudgetHandle, BudgetLimits, BudgetSnapshot,
    BudgetUsage,
};
pub use cancel::CancellationToken;
pub use trace::{
    RequirementDisposition, TraceError, TraceHandle, TraceNodeId, TraceNodeKind, TraceRecord,
};

/// Live run context passed through Agent loop, tool, and child-agent calls.
///
/// The context owns three field-private runtime handles: cancellation, shared
/// budget accounting, and trace recording. Child contexts must be created with
/// [`RunContext::derive_child`] so they inherit the parent cancellation chain,
/// shared budget, and trace parent.
#[derive(Clone, Debug)]
pub struct RunContext {
    run_id: RunId,
    cancellation: CancellationToken,
    budget: BudgetHandle,
    trace: TraceHandle,
    depth: u32,
}

impl RunContext {
    /// Creates a root run context from caller-supplied identity and limits.
    #[must_use]
    pub fn new_root(
        run_id: RunId,
        budget_limits: BudgetLimits,
        trace_root_id: TraceNodeId,
    ) -> Self {
        Self {
            run_id,
            cancellation: CancellationToken::new(),
            budget: BudgetHandle::new(budget_limits),
            trace: TraceHandle::new_root(trace_root_id, run_id),
            depth: 0,
        }
    }

    /// Returns the externally supplied run identity.
    #[must_use]
    pub const fn run_id(&self) -> RunId {
        self.run_id
    }

    /// Returns the cancellation handle for this run.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Returns the shared budget handle for this run.
    #[must_use]
    pub const fn budget(&self) -> &BudgetHandle {
        &self.budget
    }

    /// Returns the trace handle scoped to this context's current trace parent.
    #[must_use]
    pub const fn trace(&self) -> &TraceHandle {
        &self.trace
    }

    /// Returns the subagent nesting depth of this context.
    ///
    /// A root context created by [`RunContext::new_root`] has depth `0`; each
    /// [`RunContext::derive_child`] adds one. A subagent handler uses this to
    /// enforce a maximum hierarchy depth (migration doc §7.2 / `agent-layer.md`
    /// §6.3): the guard belongs in the one handler that deepens the scope chain,
    /// not scattered elsewhere.
    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// Returns whether this context has been cancelled by itself or an ancestor.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Fails with [`RunContextError::Cancelled`] if cancellation has propagated.
    ///
    /// # Errors
    ///
    /// Returns [`RunContextError::Cancelled`] when this context or one of its
    /// ancestors has been cancelled.
    pub fn check_cancelled(&self) -> Result<(), RunContextError> {
        if self.is_cancelled() {
            Err(RunContextError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Charges one logical Agent step against the shared budget.
    ///
    /// # Errors
    ///
    /// Returns [`RunContextError::Budget`] when the step limit would be
    /// exceeded or the internal counter would overflow.
    pub fn charge_step(&self) -> Result<BudgetSnapshot, RunContextError> {
        self.budget.charge_step().map_err(RunContextError::from)
    }

    /// Charges a raw token count against the shared budget.
    ///
    /// # Errors
    ///
    /// Returns [`RunContextError::Budget`] when the token limit would be
    /// exceeded or the internal counter would overflow.
    pub fn charge_tokens(&self, tokens: u64) -> Result<BudgetSnapshot, RunContextError> {
        self.budget
            .charge_tokens(tokens)
            .map_err(RunContextError::from)
    }

    /// Charges normalized model usage against the shared token budget.
    ///
    /// # Errors
    ///
    /// Returns [`RunContextError::Budget`] when the token limit would be
    /// exceeded or the internal counter would overflow.
    pub fn charge_usage(&self, usage: &Usage) -> Result<BudgetSnapshot, RunContextError> {
        self.budget
            .charge_usage(usage)
            .map_err(RunContextError::from)
    }

    /// Charges provider or host cost, in micro-units, against the shared budget.
    ///
    /// # Errors
    ///
    /// Returns [`RunContextError::Budget`] when the cost limit would be
    /// exceeded or the internal counter would overflow.
    pub fn charge_cost_micros(&self, cost_micros: u64) -> Result<BudgetSnapshot, RunContextError> {
        self.budget
            .charge_cost_micros(cost_micros)
            .map_err(RunContextError::from)
    }

    /// Checks externally measured wall-clock elapsed time against the budget.
    ///
    /// The context does not read the system clock; callers pass elapsed time so
    /// tests and restored runs can keep time injection deterministic.
    ///
    /// # Errors
    ///
    /// Returns [`RunContextError::Budget`] when `elapsed` is greater than the
    /// configured wall-clock limit.
    pub fn check_wall_clock(&self, elapsed: Duration) -> Result<(), RunContextError> {
        self.budget
            .check_wall_clock(elapsed)
            .map_err(RunContextError::from)
    }

    /// Derives a child context that inherits cancel, budget, and trace parentage.
    ///
    /// The child shares the same budget ledger, so child charges consume the
    /// parent run's limits. Parent cancellation propagates through the derived
    /// cancellation token. A sub-agent trace node is recorded and becomes the
    /// parent for records created through the child context. The child's
    /// [`depth`](Self::depth) is one greater than this context's.
    ///
    /// # Errors
    ///
    /// Returns [`RunContextError::Trace`] if `trace_node_id` duplicates an
    /// existing trace node or if the parent trace chain is inconsistent.
    pub fn derive_child(
        &self,
        child_run_id: RunId,
        trace_node_id: TraceNodeId,
    ) -> Result<Self, RunContextError> {
        let sub_agent = self
            .trace
            .record_sub_agent(trace_node_id, child_run_id)
            .map_err(RunContextError::from)?;

        Ok(Self {
            run_id: child_run_id,
            cancellation: self.cancellation.derive_child(),
            budget: self.budget.clone(),
            trace: self
                .trace
                .with_parent(sub_agent.id().clone())
                .map_err(RunContextError::from)?,
            depth: self.depth.saturating_add(1),
        })
    }
}

/// Run-context failure classification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(rename_all = "snake_case")]
pub enum RunContextError {
    /// The run was cancelled through this context or an ancestor context.
    #[error("run was cancelled")]
    Cancelled,
    /// A budget operation failed.
    #[error(transparent)]
    Budget(#[from] BudgetError),
    /// A trace operation failed.
    #[error(transparent)]
    Trace(#[from] TraceError),
}
