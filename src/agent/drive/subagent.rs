//! The reference subagent handler: derive a child, drive it, enforce scope.
//!
//! `NeedSubagent` is the only requirement that *deepens* the scope chain
//! (migration doc §7.2). Fulfilling one opens another [`drain`] layer for a
//! child machine, so the handler that serves it owns the three hierarchy
//! guards that must live in exactly one place:
//!
//! - **Depth.** Each derived layer adds one to [`RunContext::depth`]; the
//!   handler refuses to deepen past a configured `max_depth`, turning an
//!   over-deep hierarchy into a classified
//!   [`AgentError::SubagentDepthExceeded`] instead of unbounded recursion
//!   (`agent-layer.md` §6.3).
//! - **Budget inheritance / cancel propagation.** The child context comes from
//!   [`RunContext::derive_child`], which shares the parent's budget ledger and
//!   derives a cancellation token from the parent chain. A cancelled parent
//!   therefore cancels the child drain, which abandons the child's first
//!   requirement and lets the child machine settle through its never-resume
//!   (`cancel_pending`) path; child consumption is charged against the parent
//!   ledger.
//! - **Scope enforcement / pop from outer.** The child drains under its own
//!   scope; whatever that scope cannot serve pops to the `outer` layer the
//!   handler is handed — the scope that emitted the `NeedSubagent` plus that
//!   scope's own parents — so a child `NeedInteraction` reaches the attended
//!   parent rather than re-entering this handler (§7.3).
//!
//! The library keeps the *policy* of building a child machine, scope, and
//! opening input behind a [`SubagentSpawner`] the host supplies; this module
//! owns only the *mechanism* that ties derivation, the nested drain, and the
//! guards together.

use super::{HandlerScope, Pop, SubagentHandler, TurnDone, drain};
use crate::agent::{
    AgentError, AgentInput, AgentMachine, RunContext, RunId, SubagentOutput, TraceNodeId,
    interaction::Interaction,
    requirement::{AgentSpecRef, RequirementResult},
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// A child machine, its drain scope, and the input that opens its turn.
///
/// A [`SubagentSpawner`] produces one of these per `NeedSubagent`. The child
/// `scope` is the child's *own* drain layer: leaving a family off (for example
/// omitting an interaction backend) makes that family pop to the `outer` layer
/// the [`DrivingSubagentHandler`] drives the child under (migration doc §6 /
/// §7.3).
pub struct SpawnedChild {
    /// The child machine to drive to the end of its opening turn.
    pub machine: Box<dyn AgentMachine + Send>,
    /// The child's own drain layer; unserved families pop to the outer layer.
    pub scope: Box<dyn HandlerScope>,
    /// The external input that opens the child's turn (the brief, folded in).
    pub opening: AgentInput,
}

/// Host policy for turning a `NeedSubagent` into a drivable child.
///
/// The handler owns the hierarchy *mechanism* (derivation, nested drain, the
/// depth / budget / cancel guards); the spawner owns the *policy* of resolving
/// a [`AgentSpecRef`] to a concrete child machine, scope, and opening input, and
/// of summarizing the finished child turn. Keeping this behind a trait lets the
/// library stay agnostic about how specs resolve to machines while still owning
/// the scope-deepening rules in one place.
pub trait SubagentSpawner: Send + Sync {
    /// Returns the child run id and sub-agent trace node id for a derivation.
    ///
    /// These feed [`RunContext::derive_child`], which records the sub-agent
    /// trace node and roots the child's shared budget / cancel chain.
    ///
    /// # Errors
    ///
    /// Returns an [`AgentError`] when ids cannot be minted for `spec_ref`.
    fn child_ids(&self, spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError>;

    /// Builds the child machine, its scope, and its opening input.
    ///
    /// # Errors
    ///
    /// Returns an [`AgentError`] when `spec_ref` cannot be resolved into a
    /// runnable child (for example an unknown spec or an invalid brief).
    fn spawn(
        &self,
        spec_ref: &AgentSpecRef,
        brief: &Interaction,
        result_schema: Option<&Value>,
    ) -> Result<SpawnedChild, AgentError>;

    /// Summarizes a finished child turn into the parent-facing output.
    fn summarize(&self, done: &TurnDone) -> SubagentOutput;
}

/// The reference [`SubagentHandler`]: derive a child, drive it, enforce depth.
///
/// Wraps a host [`SubagentSpawner`] with a `max_depth` guard. `max_depth` is the
/// greatest [`RunContext::depth`] at which a further child may still be derived:
/// a handler invoked with a context already at `max_depth` refuses with
/// [`AgentError::SubagentDepthExceeded`] before spawning anything, so
/// `max_depth == 0` forbids subagents entirely.
pub struct DrivingSubagentHandler {
    spawner: Arc<dyn SubagentSpawner>,
    max_depth: u32,
}

impl DrivingSubagentHandler {
    /// Wraps `spawner`, refusing to derive a child at depth `>= max_depth`.
    #[must_use]
    pub fn new(spawner: Arc<dyn SubagentSpawner>, max_depth: u32) -> Self {
        Self { spawner, max_depth }
    }
}

#[async_trait]
impl SubagentHandler for DrivingSubagentHandler {
    async fn fulfill(
        &self,
        spec_ref: &AgentSpecRef,
        brief: &Interaction,
        result_schema: Option<&Value>,
        outer: &mut dyn Pop,
        ctx: &RunContext,
    ) -> RequirementResult {
        // Depth guard first: refuse before minting ids or spawning, so an
        // over-deep hierarchy costs nothing and never recurses (§7.2).
        if ctx.depth() >= self.max_depth {
            return RequirementResult::Subagent(Err(AgentError::SubagentDepthExceeded {
                limit: self.max_depth,
                depth: ctx.depth(),
            }));
        }

        let (child_run_id, trace_node_id) = match self.spawner.child_ids(spec_ref) {
            Ok(ids) => ids,
            Err(error) => return RequirementResult::Subagent(Err(error)),
        };

        // The child context shares the parent budget ledger and derives its
        // cancellation from the parent chain, so budget inheritance and cancel
        // propagation come for free from `derive_child`.
        let child_ctx = match ctx.derive_child(child_run_id, trace_node_id) {
            Ok(child_ctx) => child_ctx,
            Err(error) => return RequirementResult::Subagent(Err(AgentError::from(error))),
        };

        let child = match self.spawner.spawn(spec_ref, brief, result_schema) {
            Ok(child) => child,
            Err(error) => return RequirementResult::Subagent(Err(error)),
        };
        let SpawnedChild {
            mut machine,
            scope,
            opening,
        } = child;

        // Open another drain layer for the child. Its unserved requirements pop
        // to `outer` — the emitting scope and its parents — never back into this
        // handler (§7.3). A cancelled `child_ctx` makes this drain abandon the
        // child's first requirement and settle it through the machine's
        // never-resume path.
        let result = drain(
            machine.as_mut(),
            opening,
            scope.as_ref(),
            Some(&mut *outer),
            &child_ctx,
        )
        .await;

        match result {
            Ok(done) => RequirementResult::Subagent(Ok(self.spawner.summarize(&done))),
            Err(error) => RequirementResult::Subagent(Err(error)),
        }
    }
}

#[cfg(test)]
mod tests;
