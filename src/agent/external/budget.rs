//! Usage/cost budget charging for managed external sessions.
//!
//! An external runtime is a black box: it may report token usage and cost on its
//! own terms, or not at all. This module is the handler/driver-side layer that
//! folds whatever the runtime *actually reported* into the shared run budget,
//! and never fabricates an estimate when the runtime stayed silent (design §17):
//!
//! - [`ExternalUsageCharge`] snapshots the reported `usage`/`cost_micros` from an
//!   [`ExternalAgentOutput`] and applies them to a [`RunContext`]. A dimension
//!   the runtime did not report stays `None` and is left unbudgeted.
//! - [`budget_exhausted`] is the pre-advance guard: a session is not advanced
//!   when a configured budget dimension already has no headroom.
//! - [`ExternalUsageChargingHandler`] wraps any
//!   [`ExternalSessionHandler`](crate::agent::ExternalSessionHandler) to enforce
//!   both: it refuses to advance an exhausted budget, charges reported usage/cost
//!   after each completed step, records the charge in the trace as
//!   external-runtime-reported, and — when a real reported charge tips the budget
//!   over — stops the live session through an [`ExternalSessionSweeper`] and
//!   folds the step into a [`LimitExceeded`](ExternalAgentError::LimitExceeded)
//!   failure so a scheduler never keeps spending past the limit.
//!
//! Budget breaches are reported uniformly as
//! [`ExternalAgentError::LimitExceeded`], whether they are caught by the
//! pre-advance guard or while charging a completed step.

// The charge helpers return the external adapter's canonical `ExternalAgentError`,
// matching the unboxed error contract used across `adapter.rs`, `registry.rs`, and
// the runtime adapters. That enum is intentionally not boxed, so `result_large_err`
// (which only fires because these helpers have a small `Ok` type) would force a
// signature style inconsistent with the rest of the external module.
#![allow(clippy::result_large_err)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use crate::agent::context::BudgetDimension;
use crate::agent::drive::ExternalSessionHandler;
use crate::agent::requirement::RequirementResult;
use crate::agent::{AgentId, RunContext, RunContextError, TraceNodeId};

use super::{
    ExternalAgentError, ExternalAgentOutput, ExternalSessionRef, ExternalSessionRegistry,
    ExternalSessionRequest, ExternalSessionResult, ExternalSessionShutdown,
};

/// The usage and cost an external runtime reported for one session step.
///
/// Both fields are independently optional because a black-box runtime may report
/// neither, one, or both. A `None` dimension is *unknown*, not zero: this crate
/// records it as unknown and leaves it unbudgeted rather than estimating a value
/// (design §17). The captured `tokens` prefer a runtime-reported
/// [`total`](crate::model::usage::Usage::total) and otherwise fall back to the
/// normalized column sum
/// ([`total_computed`](crate::model::usage::Usage::total_computed)) — still a
/// value the runtime reported, never a word-count guess.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExternalUsageCharge {
    tokens: Option<u64>,
    cost_micros: Option<u64>,
}

impl ExternalUsageCharge {
    /// Reads the reported usage/cost from a completed session's output.
    ///
    /// No charge is applied and no estimate is synthesized: a missing
    /// [`usage`](ExternalAgentOutput::usage) or
    /// [`cost_micros`](ExternalAgentOutput::cost_micros) stays `None`.
    #[must_use]
    pub fn from_output(output: &ExternalAgentOutput) -> Self {
        let tokens = output
            .usage
            .as_ref()
            .map(|usage| u64::from(usage.total.unwrap_or_else(|| usage.total_computed())));
        Self {
            tokens,
            cost_micros: output.cost_micros,
        }
    }

    /// Returns the token count the runtime reported, if any.
    #[must_use]
    pub const fn tokens(&self) -> Option<u64> {
        self.tokens
    }

    /// Returns the cost in micro-units the runtime reported, if any.
    #[must_use]
    pub const fn cost_micros(&self) -> Option<u64> {
        self.cost_micros
    }

    /// Returns `true` when the runtime reported neither usage nor cost.
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        self.tokens.is_none() && self.cost_micros.is_none()
    }

    /// Charges the reported usage/cost against the shared run budget.
    ///
    /// Only the dimensions the runtime reported are charged; an unknown
    /// dimension is skipped entirely so no estimate reaches the ledger. The
    /// token charge is applied before the cost charge; a charge that lands keeps
    /// its counter even if a later dimension trips the limit, because the
    /// runtime genuinely consumed it.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::LimitExceeded`] when a reported charge would
    /// exceed a configured budget dimension (or overflow its counter).
    pub fn charge(&self, ctx: &RunContext) -> Result<(), ExternalAgentError> {
        if let Some(tokens) = self.tokens {
            ctx.charge_tokens(tokens).map_err(limit_exceeded)?;
        }
        if let Some(cost_micros) = self.cost_micros {
            ctx.charge_cost_micros(cost_micros)
                .map_err(limit_exceeded)?;
        }
        Ok(())
    }
}

/// Maps a run-context budget failure onto the external error taxonomy.
///
/// Every failure returned by the charge helpers here is a budget breach, so it
/// is reported uniformly as [`ExternalAgentError::LimitExceeded`] with the
/// stable, secret-free budget diagnostic as its `limit` text.
fn limit_exceeded(error: RunContextError) -> ExternalAgentError {
    ExternalAgentError::LimitExceeded {
        limit: error.to_string(),
    }
}

/// Returns the first budget dimension that already has no headroom, if any.
///
/// This is the pre-advance guard (design §17): a session is not advanced when a
/// configured count-like dimension is already at or over its limit. Unbounded
/// dimensions never count as exhausted, so a run with no limits is never
/// blocked. Dimensions are checked in a stable order (steps, tokens, cost) so the
/// reported dimension is deterministic. The wall-clock dimension is not checked
/// here because it needs a caller-supplied elapsed time.
#[must_use]
pub fn budget_exhausted(ctx: &RunContext) -> Option<BudgetDimension> {
    let snapshot = ctx.budget().snapshot();
    let limits = snapshot.limits();
    let used = snapshot.used();

    let exhausted =
        |limit: Option<u64>, used: u64| -> bool { limit.is_some_and(|limit| used >= limit) };

    if exhausted(limits.max_steps(), used.steps()) {
        Some(BudgetDimension::Steps)
    } else if exhausted(limits.max_tokens(), used.tokens()) {
        Some(BudgetDimension::Tokens)
    } else if exhausted(limits.max_cost_micros(), used.cost_micros()) {
        Some(BudgetDimension::CostMicros)
    } else {
        None
    }
}

/// Stops a live external session when the budget-charging layer must abandon it.
///
/// The charging handler owns no runtime connection, so it delegates the actual
/// force-close to the layer that does (typically an
/// [`ExternalSessionRegistry`]). Cleanup returns the resulting
/// [`ExternalSessionShutdown`] so the handler can record how the session closed
/// and whether the worktree may be dirty (design §6.4, §10).
#[async_trait]
pub trait ExternalSessionSweeper: Send + Sync {
    /// Force-closes and deregisters `session` for `agent_id`.
    async fn sweep(
        &self,
        agent_id: AgentId,
        session: &ExternalSessionRef,
    ) -> ExternalSessionShutdown;
}

#[async_trait]
impl ExternalSessionSweeper for ExternalSessionRegistry {
    async fn sweep(
        &self,
        agent_id: AgentId,
        session: &ExternalSessionRef,
    ) -> ExternalSessionShutdown {
        self.cleanup(agent_id, session).await
    }
}

#[async_trait]
impl<S: ExternalSessionSweeper + ?Sized> ExternalSessionSweeper for Arc<S> {
    async fn sweep(
        &self,
        agent_id: AgentId,
        session: &ExternalSessionRef,
    ) -> ExternalSessionShutdown {
        (**self).sweep(agent_id, session).await
    }
}

/// A sweeper for handlers that do not own session teardown.
///
/// Selecting [`NoSweep`] means the host cleans live sessions up elsewhere (its
/// own registry sweep on the classified failure). A budget breach still fails
/// loudly; there is simply nothing for the handler itself to close, so
/// [`sweep`](ExternalSessionSweeper::sweep) reports a graceful no-op close.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoSweep;

#[async_trait]
impl ExternalSessionSweeper for NoSweep {
    async fn sweep(
        &self,
        _agent_id: AgentId,
        _session: &ExternalSessionRef,
    ) -> ExternalSessionShutdown {
        ExternalSessionShutdown::Graceful
    }
}

/// Wraps an [`ExternalSessionHandler`] to charge reported usage/cost and enforce
/// the run budget.
///
/// The wrapper is transparent for everything except budget accounting:
///
/// - **Before** delegating, it fails with
///   [`LimitExceeded`](ExternalAgentError::LimitExceeded) when
///   [`budget_exhausted`] reports no headroom, so an already-spent budget never
///   starts another step.
/// - **After** a delegated step [`Completed`](ExternalSessionResult::Completed),
///   it charges the runtime-reported usage/cost, records the charge in the trace
///   as external-runtime-reported (unknown dimensions included), and passes the
///   completion through unchanged.
/// - When that reported charge tips the budget over, it stops the live session
///   through the configured [`ExternalSessionSweeper`], records the shutdown, and
///   rewrites the step into a [`Failed`](ExternalSessionResult::Failed) carrying
///   [`LimitExceeded`](ExternalAgentError::LimitExceeded) — retaining the session
///   facts so the machine can still audit the close.
///
/// Paused decision points and non-external results are forwarded verbatim: usage
/// is only knowable at completion, so there is nothing to charge mid-turn.
///
/// The `S` type parameter defaults to [`NoSweep`]; build with
/// [`with_sweeper`](Self::with_sweeper) to wire session teardown (for example an
/// [`ExternalSessionRegistry`]).
pub struct ExternalUsageChargingHandler<H, S = NoSweep> {
    inner: H,
    sweeper: S,
    trace_seq: AtomicU64,
}

impl<H> ExternalUsageChargingHandler<H, NoSweep> {
    /// Wraps `inner` with usage/cost charging and no session teardown.
    #[must_use]
    pub const fn new(inner: H) -> Self {
        Self {
            inner,
            sweeper: NoSweep,
            trace_seq: AtomicU64::new(0),
        }
    }
}

impl<H, S> ExternalUsageChargingHandler<H, S> {
    /// Wraps `inner` with usage/cost charging and a session teardown sweeper.
    #[must_use]
    pub const fn with_sweeper(inner: H, sweeper: S) -> Self {
        Self {
            inner,
            sweeper,
            trace_seq: AtomicU64::new(0),
        }
    }

    /// Returns a reference to the wrapped handler.
    #[must_use]
    pub const fn inner(&self) -> &H {
        &self.inner
    }

    /// Mints a stable, per-handler-unique trace node id.
    ///
    /// The crate mints no ids itself, so uniqueness comes from the run id plus a
    /// per-handler monotonic counter (mirroring the worktree manager's approach),
    /// keeping the id deterministic and collision-free without a clock or RNG.
    fn next_trace_id(&self, ctx: &RunContext, kind: &str) -> TraceNodeId {
        let seq = self.trace_seq.fetch_add(1, Ordering::Relaxed);
        TraceNodeId::new(format!("external-{kind}/{}/{seq}", ctx.run_id()))
    }

    /// Records a usage charge under the current trace parent (best effort).
    ///
    /// Recording is auxiliary audit: the id is unique by construction and the
    /// parent is always the context's live trace node, so the only ways this
    /// could fail cannot occur here. The result is intentionally dropped so a
    /// trace hiccup never masks the charge outcome.
    fn record_usage(&self, ctx: &RunContext, charge: &ExternalUsageCharge) {
        let id = self.next_trace_id(ctx, "usage");
        let _ = ctx
            .trace()
            .record_external_usage(id, charge.tokens(), charge.cost_micros());
    }

    /// Records a session shutdown under the current trace parent (best effort).
    fn record_shutdown(&self, ctx: &RunContext, disposition: ExternalSessionShutdown) {
        let id = self.next_trace_id(ctx, "shutdown");
        let _ = ctx.trace().record_external_shutdown(id, disposition);
    }
}

impl<H, S> ExternalUsageChargingHandler<H, S>
where
    S: ExternalSessionSweeper,
{
    /// Builds the pre-advance failure for an already-exhausted budget.
    ///
    /// Any live session named by the request is force-closed and its shutdown
    /// recorded, so an exhausted budget both stops the current session and
    /// cleans it up before failing.
    async fn fail_exhausted(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        dimension: BudgetDimension,
    ) -> RequirementResult {
        if let Some(session) = &request.session {
            let disposition = self.sweeper.sweep(request.agent_id, session).await;
            self.record_shutdown(ctx, disposition);
        }
        external_failed(
            request.session.clone(),
            ExternalAgentError::LimitExceeded {
                limit: format!("{dimension:?} budget exhausted before advancing external session"),
            },
            Vec::new(),
        )
    }
}

#[async_trait]
impl<H, S> ExternalSessionHandler for ExternalUsageChargingHandler<H, S>
where
    H: ExternalSessionHandler,
    S: ExternalSessionSweeper,
{
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> RequirementResult {
        if let Some(dimension) = budget_exhausted(ctx) {
            return self.fail_exhausted(request, ctx, dimension).await;
        }

        let result = self.inner.fulfill(request, ctx).await;
        let RequirementResult::ExternalSession(boxed) = result else {
            // A budget wrapper only understands the external-session family; any
            // other result is forwarded untouched.
            return result;
        };

        match *boxed {
            ExternalSessionResult::Completed {
                session,
                output,
                observations,
            } => {
                let charge = ExternalUsageCharge::from_output(&output);
                match charge.charge(ctx) {
                    Ok(()) => {
                        self.record_usage(ctx, &charge);
                        RequirementResult::ExternalSession(Box::new(
                            ExternalSessionResult::Completed {
                                session,
                                output,
                                observations,
                            },
                        ))
                    }
                    Err(error) => {
                        let disposition = self.sweeper.sweep(request.agent_id, &session).await;
                        self.record_usage(ctx, &charge);
                        self.record_shutdown(ctx, disposition);
                        external_failed(Some(session), error, observations)
                    }
                }
            }
            other => RequirementResult::ExternalSession(Box::new(other)),
        }
    }
}

/// Packages an [`ExternalSessionResult::Failed`] as a [`RequirementResult`].
fn external_failed(
    session: Option<ExternalSessionRef>,
    error: ExternalAgentError,
    observations: Vec<super::ExternalObservedEvent>,
) -> RequirementResult {
    RequirementResult::ExternalSession(Box::new(ExternalSessionResult::Failed {
        session,
        error,
        observations,
    }))
}

#[cfg(test)]
mod tests;
