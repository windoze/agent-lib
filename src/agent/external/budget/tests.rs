//! Unit tests for external usage/cost budget charging.
//!
//! Every test name is prefixed `external_budget_` so `cargo test -p agent-lib
//! external_budget` selects exactly this module. All cases are offline and each
//! runs well under a second; the async handler is driven on a current-thread
//! runtime.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use crate::agent::context::BudgetDimension;
use crate::agent::drive::ExternalSessionHandler;
use crate::agent::external::{
    ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionInput, ExternalSessionPolicy,
    ExternalSessionRef, ExternalSessionRequest, ExternalSessionResult, ExternalSessionShutdown,
    ExternalStreamPolicy, ExternalToolBatchId, WorktreeIsolation,
};
use crate::agent::requirement::RequirementResult;
use crate::agent::spec::WorktreeRef;
use crate::agent::{
    AgentId, BudgetLimits, ExternalAgentError, ExternalAgentOutput, RunContext, RunId, TraceNodeId,
    TraceNodeKind,
};
use crate::model::usage::Usage;

use super::{
    ExternalSessionSweeper, ExternalUsageCharge, ExternalUsageChargingHandler, NoSweep,
    budget_exhausted,
};

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f0"
        .parse()
        .expect("agent id")
}

fn run_id() -> RunId {
    "018f0d9c-7b6a-7c12-8f31-1234567890e0"
        .parse()
        .expect("run id")
}

fn run_context(limits: BudgetLimits) -> RunContext {
    RunContext::new_root(run_id(), limits, TraceNodeId::new("external-budget-root"))
}

fn policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        permission_mode: ExternalPermissionMode::AcceptEdits,
        isolation: WorktreeIsolation::EphemeralGitWorktree,
        max_turns: Some(8),
        stream_events: ExternalStreamPolicy::Buffered,
    }
}

fn session_ref(id: &str) -> ExternalSessionRef {
    ExternalSessionRef {
        runtime: ExternalRuntimeKind::ClaudeCode,
        session_id: Some(id.to_owned()),
        transcript_ref: None,
        resume_token: None,
        last_event_seq: None,
    }
}

fn start_request() -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent_id(),
        runtime: ExternalRuntimeKind::ClaudeCode,
        worktree: WorktreeRef::new("/repo/agent-lib"),
        session: None,
        input: ExternalSessionInput::Start {
            prompt: "do the thing".to_owned(),
        },
        tools: Vec::new(),
        policy: policy(),
    }
}

fn continue_request(session_id: &str) -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent_id(),
        runtime: ExternalRuntimeKind::ClaudeCode,
        worktree: WorktreeRef::new("/repo/agent-lib"),
        session: Some(session_ref(session_id)),
        input: ExternalSessionInput::Continue {
            message: "keep going".to_owned(),
        },
        tools: Vec::new(),
        policy: policy(),
    }
}

fn output_with(usage: Option<Usage>, cost_micros: Option<u64>) -> ExternalAgentOutput {
    ExternalAgentOutput {
        summary: "done".to_owned(),
        artifacts: Vec::new(),
        usage,
        cost_micros,
    }
}

fn usage_total(total: u32) -> Usage {
    Usage {
        total: Some(total),
        ..Usage::default()
    }
}

fn completed(session_id: &str, output: ExternalAgentOutput) -> ExternalSessionResult {
    ExternalSessionResult::Completed {
        session: session_ref(session_id),
        output,
        observations: Vec::new(),
    }
}

/// An inner handler that returns a preset result and counts fulfillments.
struct ScriptedHandler {
    result: std::sync::Mutex<Option<ExternalSessionResult>>,
    calls: AtomicUsize,
}

impl ScriptedHandler {
    fn new(result: ExternalSessionResult) -> Self {
        Self {
            result: std::sync::Mutex::new(Some(result)),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ExternalSessionHandler for ScriptedHandler {
    async fn fulfill(
        &self,
        _request: &ExternalSessionRequest,
        _ctx: &RunContext,
    ) -> RequirementResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = self
            .result
            .lock()
            .expect("scripted result")
            .take()
            .expect("scripted handler fulfilled more than once");
        RequirementResult::ExternalSession(Box::new(result))
    }
}

/// Records the sessions it was asked to sweep and reports a fixed disposition.
struct SpySweeper {
    disposition: ExternalSessionShutdown,
    swept: std::sync::Mutex<Vec<(AgentId, String)>>,
}

impl SpySweeper {
    fn new(disposition: ExternalSessionShutdown) -> Self {
        Self {
            disposition,
            swept: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn swept(&self) -> Vec<(AgentId, String)> {
        self.swept.lock().expect("swept log").clone()
    }
}

#[async_trait]
impl ExternalSessionSweeper for SpySweeper {
    async fn sweep(
        &self,
        agent_id: AgentId,
        session: &ExternalSessionRef,
    ) -> ExternalSessionShutdown {
        self.swept
            .lock()
            .expect("swept log")
            .push((agent_id, session.session_id.clone().unwrap_or_default()));
        self.disposition
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(future)
}

fn into_external(result: RequirementResult) -> ExternalSessionResult {
    match result {
        RequirementResult::ExternalSession(boxed) => *boxed,
        other => panic!("expected external-session result, got {other:?}"),
    }
}

fn usage_trace_nodes(ctx: &RunContext) -> Vec<TraceNodeKind> {
    ctx.trace()
        .records()
        .into_iter()
        .map(|record| record.kind())
        .filter(|kind| matches!(kind, TraceNodeKind::ExternalUsage { .. }))
        .collect()
}

fn shutdown_trace_nodes(ctx: &RunContext) -> Vec<TraceNodeKind> {
    ctx.trace()
        .records()
        .into_iter()
        .map(|record| record.kind())
        .filter(|kind| matches!(kind, TraceNodeKind::ExternalShutdown { .. }))
        .collect()
}

#[test]
fn external_budget_charge_reads_reported_total_without_estimating() {
    let charge =
        ExternalUsageCharge::from_output(&output_with(Some(usage_total(120)), Some(4_000)));
    assert_eq!(charge.tokens(), Some(120));
    assert_eq!(charge.cost_micros(), Some(4_000));
    assert!(!charge.is_unknown());
}

#[test]
fn external_budget_charge_falls_back_to_computed_total() {
    let usage = Usage {
        input: 10,
        output: 15,
        ..Usage::default()
    };
    let charge = ExternalUsageCharge::from_output(&output_with(Some(usage), None));
    assert_eq!(charge.tokens(), Some(25));
    assert_eq!(charge.cost_micros(), None);
}

#[test]
fn external_budget_missing_usage_is_unknown() {
    let charge = ExternalUsageCharge::from_output(&output_with(None, None));
    assert!(charge.is_unknown());
    assert_eq!(charge.tokens(), None);
    assert_eq!(charge.cost_micros(), None);
}

#[test]
fn external_budget_reported_usage_and_cost_are_charged() {
    let ctx = run_context(BudgetLimits::new(None, Some(1_000), Some(10_000), None));
    let charge =
        ExternalUsageCharge::from_output(&output_with(Some(usage_total(120)), Some(4_000)));
    charge.charge(&ctx).expect("charge within budget");

    let used = ctx.budget().snapshot();
    assert_eq!(used.used().tokens(), 120);
    assert_eq!(used.used().cost_micros(), 4_000);
}

#[test]
fn external_budget_missing_usage_charges_nothing() {
    let ctx = run_context(BudgetLimits::new(None, Some(1_000), Some(10_000), None));
    let charge = ExternalUsageCharge::from_output(&output_with(None, None));
    charge.charge(&ctx).expect("nothing to charge");

    let used = ctx.budget().snapshot();
    assert_eq!(used.used().tokens(), 0);
    assert_eq!(used.used().cost_micros(), 0);
}

#[test]
fn external_budget_partial_usage_charges_only_reported_dimension() {
    let ctx = run_context(BudgetLimits::new(None, Some(1_000), Some(10_000), None));
    let charge = ExternalUsageCharge::from_output(&output_with(Some(usage_total(50)), None));
    charge.charge(&ctx).expect("token-only charge");

    let used = ctx.budget().snapshot();
    assert_eq!(used.used().tokens(), 50);
    assert_eq!(used.used().cost_micros(), 0);
}

#[test]
fn external_budget_charge_maps_overrun_to_limit_exceeded() {
    let ctx = run_context(BudgetLimits::new(None, Some(100), None, None));
    let charge = ExternalUsageCharge::from_output(&output_with(Some(usage_total(200)), None));
    let error = charge.charge(&ctx).expect_err("token limit exceeded");
    assert!(matches!(error, ExternalAgentError::LimitExceeded { .. }));

    // The over-limit dimension is not partially applied.
    assert_eq!(ctx.budget().snapshot().used().tokens(), 0);
}

#[test]
fn external_budget_exhausted_detects_each_dimension() {
    let steps = run_context(BudgetLimits::new(Some(1), None, None, None));
    steps.charge_step().expect("consume the only step");
    assert_eq!(budget_exhausted(&steps), Some(BudgetDimension::Steps));

    let tokens = run_context(BudgetLimits::new(None, Some(10), None, None));
    tokens.charge_tokens(10).expect("consume tokens");
    assert_eq!(budget_exhausted(&tokens), Some(BudgetDimension::Tokens));

    let cost = run_context(BudgetLimits::new(None, None, Some(5), None));
    cost.charge_cost_micros(5).expect("consume cost");
    assert_eq!(budget_exhausted(&cost), Some(BudgetDimension::CostMicros));

    let unbounded = run_context(BudgetLimits::unbounded());
    assert_eq!(budget_exhausted(&unbounded), None);

    let headroom = run_context(BudgetLimits::new(None, Some(10), None, None));
    headroom.charge_tokens(9).expect("still one token left");
    assert_eq!(budget_exhausted(&headroom), None);
}

#[test]
fn external_budget_handler_charges_reported_usage_on_completion() {
    let ctx = run_context(BudgetLimits::new(None, Some(1_000), Some(10_000), None));
    let handler = ExternalUsageChargingHandler::new(ScriptedHandler::new(completed(
        "s1",
        output_with(Some(usage_total(120)), Some(4_000)),
    )));

    let result = block_on(handler.fulfill(&start_request(), &ctx));
    assert!(matches!(
        into_external(result),
        ExternalSessionResult::Completed { .. }
    ));

    let used = ctx.budget().snapshot();
    assert_eq!(used.used().tokens(), 120);
    assert_eq!(used.used().cost_micros(), 4_000);
    assert_eq!(handler.inner().calls(), 1);
}

#[test]
fn external_budget_handler_records_usage_source_in_trace() {
    let ctx = run_context(BudgetLimits::unbounded());
    let handler = ExternalUsageChargingHandler::new(ScriptedHandler::new(completed(
        "s1",
        output_with(Some(usage_total(75)), Some(2_500)),
    )));

    block_on(handler.fulfill(&start_request(), &ctx));

    assert_eq!(
        usage_trace_nodes(&ctx),
        vec![TraceNodeKind::ExternalUsage {
            tokens_charged: Some(75),
            cost_micros_charged: Some(2_500),
        }]
    );
}

#[test]
fn external_budget_handler_records_unknown_usage_source_in_trace() {
    let ctx = run_context(BudgetLimits::unbounded());
    let handler = ExternalUsageChargingHandler::new(ScriptedHandler::new(completed(
        "s1",
        output_with(None, None),
    )));

    block_on(handler.fulfill(&start_request(), &ctx));

    assert_eq!(
        usage_trace_nodes(&ctx),
        vec![TraceNodeKind::ExternalUsage {
            tokens_charged: None,
            cost_micros_charged: None,
        }]
    );
    // Nothing was charged for an unknown usage report.
    assert_eq!(ctx.budget().snapshot().used().tokens(), 0);
}

#[test]
fn external_budget_precheck_fails_before_advancing_and_sweeps_session() {
    let ctx = run_context(BudgetLimits::new(None, Some(10), None, None));
    ctx.charge_tokens(10).expect("exhaust tokens");

    let sweeper = Arc::new(SpySweeper::new(ExternalSessionShutdown::ForcedKill));
    let inner = ScriptedHandler::new(completed("s1", output_with(Some(usage_total(1)), None)));
    let handler = ExternalUsageChargingHandler::with_sweeper(inner, Arc::clone(&sweeper));

    let result = block_on(handler.fulfill(&continue_request("s1"), &ctx));
    match into_external(result) {
        ExternalSessionResult::Failed { session, error, .. } => {
            assert_eq!(session, Some(session_ref("s1")));
            assert!(matches!(error, ExternalAgentError::LimitExceeded { .. }));
        }
        other => panic!("expected failure, got {other:?}"),
    }

    // Inner handler was never advanced, and the live session was swept.
    assert_eq!(handler.inner().calls(), 0);
    assert_eq!(sweeper.swept(), vec![(agent_id(), "s1".to_owned())]);
}

#[test]
fn external_budget_precheck_without_live_session_does_not_sweep() {
    let ctx = run_context(BudgetLimits::new(None, Some(10), None, None));
    ctx.charge_tokens(10).expect("exhaust tokens");

    let sweeper = Arc::new(SpySweeper::new(ExternalSessionShutdown::ForcedKill));
    let inner = ScriptedHandler::new(completed("s1", output_with(None, None)));
    let handler = ExternalUsageChargingHandler::with_sweeper(inner, Arc::clone(&sweeper));

    let result = block_on(handler.fulfill(&start_request(), &ctx));
    match into_external(result) {
        ExternalSessionResult::Failed { session, .. } => assert_eq!(session, None),
        other => panic!("expected failure, got {other:?}"),
    }
    assert_eq!(handler.inner().calls(), 0);
    assert!(sweeper.swept().is_empty());
}

#[test]
fn external_budget_overrun_on_completion_stops_session_and_cleans_up() {
    let ctx = run_context(BudgetLimits::new(None, Some(100), None, None));
    let sweeper = Arc::new(SpySweeper::new(ExternalSessionShutdown::ForcedKill));
    let inner = ScriptedHandler::new(completed("s7", output_with(Some(usage_total(500)), None)));
    let handler = ExternalUsageChargingHandler::with_sweeper(inner, Arc::clone(&sweeper));

    let result = block_on(handler.fulfill(&continue_request("s7"), &ctx));
    match into_external(result) {
        ExternalSessionResult::Failed { session, error, .. } => {
            // Session facts are retained so the machine can audit the close.
            assert_eq!(session, Some(session_ref("s7")));
            assert!(matches!(error, ExternalAgentError::LimitExceeded { .. }));
        }
        other => panic!("expected failure, got {other:?}"),
    }

    // The session that overran the budget was force-closed.
    assert_eq!(sweeper.swept(), vec![(agent_id(), "s7".to_owned())]);

    // A ForcedKill close is recorded so the worktree is not treated as clean.
    assert_eq!(
        shutdown_trace_nodes(&ctx),
        vec![TraceNodeKind::ExternalShutdown {
            disposition: ExternalSessionShutdown::ForcedKill,
        }]
    );
}

#[test]
fn external_budget_paused_result_is_forwarded_without_charging() {
    let ctx = run_context(BudgetLimits::new(None, Some(1_000), None, None));
    let paused = ExternalSessionResult::PausedForToolCalls {
        session: session_ref("s1"),
        batch_id: ExternalToolBatchId::new("batch-1"),
        calls: Vec::new(),
        observations: Vec::new(),
    };
    let handler = ExternalUsageChargingHandler::new(ScriptedHandler::new(paused));

    let result = block_on(handler.fulfill(&start_request(), &ctx));
    assert!(matches!(
        into_external(result),
        ExternalSessionResult::PausedForToolCalls { .. }
    ));
    // A pause carries no terminal usage, so nothing is charged.
    assert_eq!(ctx.budget().snapshot().used().tokens(), 0);
}

#[test]
fn external_budget_completed_within_budget_leaves_result_intact() {
    let ctx = run_context(BudgetLimits::unbounded());
    let handler = ExternalUsageChargingHandler::<_, NoSweep>::new(ScriptedHandler::new(completed(
        "s1",
        output_with(Some(usage_total(10)), Some(20)),
    )));

    let result = block_on(handler.fulfill(&start_request(), &ctx));
    match into_external(result) {
        ExternalSessionResult::Completed { session, .. } => {
            assert_eq!(session, session_ref("s1"));
        }
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn external_budget_registry_is_a_session_sweeper() {
    // Compile-time proof that the production registry satisfies the sweeper
    // contract the charging handler depends on for cleanup.
    fn assert_sweeper<S: ExternalSessionSweeper>() {}
    assert_sweeper::<crate::agent::external::ExternalSessionRegistry>();
}
