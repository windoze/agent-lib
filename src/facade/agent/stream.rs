//! Incremental [`AgentRunStream`] backing [`Agent::stream`].
//!
//! [`Agent::stream`](super::Agent::stream) is the tool-using, approval-gated
//! analog of [`ChatSession::stream`](crate::facade::ChatSession::stream). Where
//! the chat stream folds a bare client stream into a
//! [`Conversation`](crate::conversation::Conversation), the agent stream drives
//! the full [`DefaultAgentMachine`](crate::agent::DefaultAgentMachine) loop
//! ([`drain`]) while a set of *tapping* handlers forward live
//! [`RunEvent`]s into a shared sink as the drive reaches them:
//!
//! - [`StreamingTapHandler`] fulfills every `NeedLlm` by driving
//!   [`chat_stream`](crate::client::LlmClient::chat_stream) and folding the
//!   deltas back into the same [`Response`](crate::client::Response) the machine
//!   consumes, emitting a [`RunEvent::TextDelta`] per text delta. This is why the
//!   machine can stay in
//!   [`NonStreaming`](crate::agent::LlmStepMode::NonStreaming) mode and still
//!   surface incremental text — no new effect family is introduced.
//! - [`TapToolHandler`] wraps the reference
//!   [`ToolRegistryHandler`] and brackets each execution with
//!   [`RunEvent::ToolStarted`] / [`RunEvent::ToolFinished`].
//! - [`TapInteractionHandler`] wraps the shared
//!   [`FacadeApproval`] and emits a [`RunEvent::ApprovalRequested`] enriched with
//!   the pending tool name, `call_id`, reason, and a redacted input summary
//!   before delegating the approval.
//!
//! When the drive finishes the terminal [`RunOutput`] is assembled exactly as
//! [`Agent::run_full`](super::Agent::run_full) assembles it, then yielded as one
//! [`RunEvent::Done`]. A failure aborts the in-flight turn inside the machine and
//! is surfaced as an `Err` stream item, leaving the agent's committed history
//! unchanged.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt};

use crate::agent::drive::{
    Resolved, budget_precheck_exhausted, charge_resolution_budget, fulfill_batch, is_terminal,
    record_requirement, record_requirement_resolution,
};
use crate::agent::interaction::{Interaction, InteractionKind};
use crate::agent::{
    AgentError, AgentInput, AgentMachine, DefaultAgentMachine, HandlerScope, InteractionHandler,
    LlmHandler, LlmStepMode, LoopCursor, LoopDoneReason, Notification, PivotMessage, PivotSource,
    Requirement, RequirementDisposition, RequirementKind, RequirementResult, RunContext, StepInput,
    StepOutcome, ToolHandler, ToolRegistry, ToolRegistryHandler, TurnDone,
};
use crate::client::{ChatRequest, ClientError, LlmClient, Response};
use crate::conversation::ToolCallId;
use crate::facade::approval::{FacadeApproval, enriched_approval_request};
use crate::facade::delegate::{DelegationRecorder, DelegationToolHandler, new_delegation_recorder};
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::{
    DelegationStatus, IntoUserMessage, Reply, RunEvent, RunOutput, ToolTrace, UsageSummary,
};
use crate::facade::tool::{FacadeToolRegistry, ToolContextParts};
use crate::model::message::Message;
use crate::model::tool::ToolCall;
use crate::stream::accumulator::{Accumulator, AccumulatorError};
use crate::stream::{Delta, StreamEvent};

use super::{
    Agent, ApprovalRecorder, CancelHandle, abandon_in_flight_turn, classify_error, collect_traces,
    drive_dispatcher_routed, drive_rules_routed, final_turn_summary, user_message_text,
    weave_approval_events,
};

/// A shared sink the tapping handlers push live [`RunEvent`]s into while the
/// drive future runs, drained in order by [`AgentRunStream::poll_next`].
type EventSink = Arc<Mutex<VecDeque<RunEvent>>>;

/// Shared run controls that can be used while an [`AgentRunStream`] is live.
#[derive(Clone, Debug)]
struct StreamControl {
    cancel: CancelHandle,
    pivots: Arc<Mutex<VecDeque<PivotMessage>>>,
    pivot_window: Arc<AtomicBool>,
}

impl StreamControl {
    fn new(cancel: CancelHandle) -> Self {
        Self {
            cancel,
            pivots: Arc::new(Mutex::new(VecDeque::new())),
            pivot_window: Arc::new(AtomicBool::new(false)),
        }
    }

    fn open_pivot_window(&self) {
        self.pivot_window.store(true, Ordering::SeqCst);
    }

    fn close_pivot_window(&self) {
        self.pivot_window.store(false, Ordering::SeqCst);
    }

    fn enqueue_pivot(&self, pivot: PivotMessage) -> Result<(), FacadeError> {
        if !self.pivot_window.load(Ordering::SeqCst) {
            return Err(FacadeError::InvalidState(
                "pivot injection is only accepted at a streamed step boundary".to_owned(),
            ));
        }
        let mut pivots = self.pivots.lock().expect("stream pivot queue poisoned");
        if !pivots.is_empty() {
            return Err(FacadeError::InvalidState(
                "a pivot is already queued for the current streamed step boundary".to_owned(),
            ));
        }
        pivots.push_back(pivot);
        Ok(())
    }

    fn pop_pivot(&self) -> Option<PivotMessage> {
        self.pivots
            .lock()
            .expect("stream pivot queue poisoned")
            .pop_front()
    }
}

/// Pushes one event onto the shared sink.
fn emit(sink: &EventSink, event: RunEvent) {
    sink.lock()
        .expect("stream event sink poisoned")
        .push_back(event);
}

/// Pops the next buffered event from the shared sink, if any.
fn pop(sink: &EventSink) -> Option<RunEvent> {
    sink.lock().expect("stream event sink poisoned").pop_front()
}

fn pending_contains_llm(pending: &[Requirement]) -> bool {
    pending
        .iter()
        .any(|requirement| matches!(requirement.kind, RequirementKind::NeedLlm { .. }))
}

fn outcome_opens_pivot_window(outcome: &StepOutcome) -> bool {
    outcome
        .notifications
        .iter()
        .any(|notification| matches!(notification, Notification::StepBoundary(_)))
        && pending_contains_llm(&outcome.requirements)
}

fn apply_queued_pivot(
    machine: &MachineCell<'_>,
    control: &StreamControl,
    pending: &mut Vec<Requirement>,
    notifications: &mut Vec<Notification>,
) -> Result<(), AgentError> {
    let Some(pivot) = control.pop_pivot() else {
        return Ok(());
    };

    let mut guard = machine.borrow_mut();
    let mut outcome = guard.step(StepInput::External(AgentInput::pivot(pivot)));
    notifications.append(&mut outcome.notifications);
    if let Some(reason) = outcome.rejected.take() {
        abandon_in_flight_turn(&mut guard);
        return Err(AgentError::Other(format!(
            "stream pivot injection was rejected: {reason:?}"
        )));
    }
    if !outcome.requirements.is_empty() {
        *pending = outcome.requirements;
    }
    Ok(())
}

/// A shared, interior-mutable handle to the agent's held machine.
///
/// The drive future and the [`AgentRunStream`]'s [`Drop`] guard both hold a clone.
/// [`drive_streamed`] borrows the cell only for the synchronous
/// [`AgentMachine::step`](crate::agent::AgentMachine::step) calls and releases it
/// before every `await`, so a drop that lands while the drive is parked can
/// [`try_borrow_mut`](RefCell::try_borrow_mut) the same machine to abandon the
/// stranded turn (see [`AgentRunStream::abandon`]).
type MachineCell<'a> = Rc<RefCell<&'a mut DefaultAgentMachine>>;

/// Drives the held machine from a fresh `input` to the end of one streamed turn.
///
/// This mirrors [`drain`](crate::agent::drain)'s reference loop exactly (fulfill a
/// batch of requirements through `scope`, resume the machine per resolution, record
/// each on the trace, and repeat until a terminal cursor), but reaches the machine
/// through a shared [`MachineCell`] instead of an exclusive `&mut`. The cell is
/// borrowed only across the synchronous `step` calls and dropped before
/// [`fulfill_batch`] is awaited, which is what lets the stream's [`Drop`] abandon a
/// parked drive without racing the future. Because the loop is otherwise identical
/// to `drain`, the resulting [`TurnDone`] matches what
/// [`Agent::run_full`](super::Agent::run_full) would produce for the same turn.
async fn drive_streamed(
    machine: &MachineCell<'_>,
    input: AgentInput,
    scope: &dyn HandlerScope,
    ctx: &RunContext,
    control: &StreamControl,
) -> Result<TurnDone, AgentError> {
    let mut notifications = Vec::new();
    let mut cancelled = false;
    let mut pivot_window_allowed = false;

    let mut pending = {
        let mut guard = machine.borrow_mut();
        let mut outcome = guard.step(StepInput::External(input));
        notifications.append(&mut outcome.notifications);
        outcome.requirements
    };

    loop {
        if pending.is_empty() {
            if is_terminal(machine.borrow().cursor()) {
                break;
            }
            let kind = machine.borrow().cursor().kind();
            return Err(AgentError::Other(format!(
                "machine quiesced without a terminal cursor or outstanding requirement \
                 (cursor: {kind:?})"
            )));
        }

        // Cancellation is a downward "should stop" signal (migration doc §7): settle
        // *every* outstanding requirement of the batch as a never-resume (traced,
        // then abandoned) and stop driving — mirroring `drain` (M4-5). The
        // streaming drop path abandons synchronously instead (see
        // [`AgentRunStream::abandon`]).
        if ctx.is_cancelled() {
            for requirement in &pending {
                record_requirement(ctx, requirement, 0, RequirementDisposition::NeverResumed);
                let mut guard = machine.borrow_mut();
                let mut outcome = guard.step(StepInput::Abandon(requirement.id));
                notifications.append(&mut outcome.notifications);
            }
            cancelled = true;
            break;
        }

        if budget_precheck_exhausted(ctx, &pending) {
            for requirement in &pending {
                record_requirement(ctx, requirement, 0, RequirementDisposition::NeverResumed);
            }
            let mut guard = machine.borrow_mut();
            let mut outcome = guard.interrupt_budget_exhausted();
            notifications.append(&mut outcome.notifications);
            break;
        }

        if pivot_window_allowed && pending_contains_llm(&pending) {
            control.open_pivot_window();
            tokio::task::yield_now().await;
            control.close_pivot_window();

            if ctx.is_cancelled() {
                for requirement in &pending {
                    record_requirement(ctx, requirement, 0, RequirementDisposition::NeverResumed);
                    let mut guard = machine.borrow_mut();
                    let mut outcome = guard.step(StepInput::Abandon(requirement.id));
                    notifications.append(&mut outcome.notifications);
                }
                cancelled = true;
                break;
            }

            apply_queued_pivot(machine, control, &mut pending, &mut notifications)?;
            pivot_window_allowed = false;
        }

        let resolutions = fulfill_batch(&pending, scope, None, ctx).await?;

        // Re-check cancellation between the batch settling and feeding the
        // resolutions back, exactly like `drain`: a cancel that landed while
        // the batch was in flight stops here; the fulfilled results are never
        // fed back and are traced as never-resumed.
        if ctx.is_cancelled() {
            for Resolved { resolution, .. } in &resolutions {
                record_requirement_resolution(
                    ctx,
                    resolution,
                    0,
                    RequirementDisposition::NeverResumed,
                );
                let mut guard = machine.borrow_mut();
                let mut outcome = guard.step(StepInput::Abandon(resolution.id));
                notifications.append(&mut outcome.notifications);
            }
            cancelled = true;
            break;
        }

        pending = Vec::new();
        let mut resolutions = resolutions.into_iter();
        while let Some(resolved) = resolutions.next() {
            if charge_resolution_budget(ctx, &resolved.resolution).is_err() {
                record_requirement_resolution(
                    ctx,
                    &resolved.resolution,
                    0,
                    RequirementDisposition::NeverResumed,
                );
                for remaining in resolutions {
                    record_requirement_resolution(
                        ctx,
                        &remaining.resolution,
                        0,
                        RequirementDisposition::NeverResumed,
                    );
                }
                let mut guard = machine.borrow_mut();
                let mut outcome = guard.interrupt_budget_exhausted();
                notifications.append(&mut outcome.notifications);
                break;
            }
            let Resolved {
                resolution,
                resolved_at_scope,
            } = resolved;
            record_requirement_resolution(
                ctx,
                &resolution,
                resolved_at_scope,
                RequirementDisposition::Resumed,
            );
            let mut guard = machine.borrow_mut();
            let mut outcome = guard.step(StepInput::Resume(resolution));
            let opens_pivot_window = outcome_opens_pivot_window(&outcome);
            notifications.append(&mut outcome.notifications);
            pending.extend(outcome.requirements);
            pivot_window_allowed |= opens_pivot_window;
        }
    }

    let cursor = machine.borrow().cursor().clone();
    Ok(TurnDone::new(notifications, cursor).with_cancelled(cancelled))
}

/// Opens one streamed agent turn over `agent`, returning an [`AgentRunStream`].
///
/// The run-scoped context, tool registry, and user input are built eagerly so a
/// registry-build or input-validation failure surfaces from the returned
/// `Result` rather than from the first poll. The machine drive itself is deferred
/// into the stream's future.
pub(super) fn start(
    agent: &mut Agent,
    message: Message,
    cancel: CancelHandle,
) -> Result<AgentRunStream<'_>, FacadeError> {
    // Rules-routed delegation short-circuits the supervisor loop: if the task
    // text matches a routing rule, the whole turn is handed to the matched
    // delegate and no LLM step is taken (`docs/facade-api.md` §13.2). A
    // non-matching task falls through to the normal machine drive below.
    if agent.delegation.is_rules_routed() {
        let task = user_message_text(&message);
        if let Some(delegate_name) = agent.delegation.route_task(&task).map(str::to_owned) {
            return start_rules_routed(agent, delegate_name, task, cancel);
        }
    }

    // Dispatcher-routed delegation runs the whole task through the facade
    // cheap→verify→strong loop with no supervisor LLM step (§13.3).
    if agent.delegation.is_dispatcher_routed() {
        let task = user_message_text(&message);
        return start_dispatcher_routed(agent, task, cancel);
    }

    let run_id = agent.ids.run_id();
    let ctx = RunContext::new_root_with_cancellation(
        run_id,
        agent.budget,
        agent.ids.trace_root("agent-run"),
        cancel.token(),
    );

    // The registry and scope are per-run: a tool must observe this turn's run id,
    // worktree, cancellation, and trace handle (mirrors `Agent::run_full`).
    let context = ToolContextParts {
        run_id,
        agent_id: agent.machine.state().spec().id(),
        worktree: agent.machine.state().spec().worktree().clone(),
        cancel: ctx.cancellation().clone(),
        trace: ctx.trace().clone(),
    };
    let registry = FacadeToolRegistry::new(
        agent.tools.clone(),
        agent.custom_registry.clone(),
        agent.extra_declarations.clone(),
        context,
    )?;
    let registry: Arc<dyn ToolRegistry> = Arc::new(registry);

    let agent_input = AgentInput::user_message(
        agent.ids.turn_id(),
        agent.ids.message_id(),
        message,
        agent.ids.message_id(),
        agent.ids.step_id(),
    )?;

    let recorder = new_delegation_recorder();
    let approvals: ApprovalRecorder = Arc::new(Mutex::new(Vec::new()));
    let sink: EventSink = Arc::new(Mutex::new(VecDeque::new()));
    let control = StreamControl::new(cancel);
    let scope = FacadeStreamScope {
        llm: StreamingTapHandler {
            client: agent.client.clone(),
            sink: sink.clone(),
        },
        tool: TapToolHandler {
            inner: DelegationToolHandler::new(
                ToolRegistryHandler::new(registry),
                agent.delegation_route(),
                agent.client.clone(),
                agent.supervisor_model(),
                agent.ids.clone(),
                recorder.clone(),
                agent.approval.clone(),
                agent.collab_bridge(),
            ),
            recorder: recorder.clone(),
            sink: sink.clone(),
        },
        interaction: TapInteractionHandler {
            approval: agent.approval.clone(),
            inner: agent.interaction_handler(),
            recorder: approvals.clone(),
            sink: sink.clone(),
        },
    };

    // Share the held machine so the drive future and the stream's `Drop` both reach
    // it: the future steps it through `drive_streamed`, and an early drop abandons
    // any stranded turn synchronously (see `AgentRunStream::abandon`).
    let stream_ids = agent.ids.clone();
    let machine: MachineCell = Rc::new(RefCell::new(&mut agent.machine));
    let machine_for_future = machine.clone();
    let control_for_future = control.clone();
    let future = Box::pin(async move {
        let done = drive_streamed(
            &machine_for_future,
            agent_input,
            &scope,
            &ctx,
            &control_for_future,
        )
        .await?;
        let collected = collect_traces(done.notifications(), &recorder);
        let recorded_approvals = approvals
            .lock()
            .expect("approval recorder poisoned")
            .clone();
        // A denied external delegate surfaces as a run-level error, matching
        // `run_full` (§9.2). Retention of external session facts is not possible
        // on the streaming path (the future holds the machine for the stream's
        // lifetime), so a snapshot is taken between runs via `run_full`.
        if collected.external_approval_denied {
            return Err(FacadeError::ApprovalDenied);
        }
        match done.cursor() {
            // A cancelled drain rests on the machine's post-cancel rest state
            // (`Idle`), not a terminal cursor (M4-5): surface an honest cancel
            // error instead of the misleading "non-terminal cursor" one. A
            // dedicated facade-level cancellation surface lands with the cancel
            // entry points in M5-4.
            cursor if done.cancelled() => Err(FacadeError::Agent(AgentError::Other(format!(
                "agent run cancelled (cursor: {:?})",
                cursor.kind()
            )))),
            // A per-turn step-limit stop is a normal terminal on the machine
            // (M4-4); the facade surfaces it as its structured limit error,
            // matching `run_full`.
            LoopCursor::Done(done_cursor)
                if done_cursor.reason() == LoopDoneReason::StepLimitReached =>
            {
                Err(FacadeError::LoopLimitExceeded)
            }
            LoopCursor::Done(done_cursor)
                if done_cursor.reason() == LoopDoneReason::BudgetExhausted =>
            {
                Err(FacadeError::BudgetExhausted)
            }
            LoopCursor::Done(_) => {
                let machine = machine_for_future.borrow();
                let (text, usage, stop_reason) = final_turn_summary(machine.state().conversation());
                let mut usage_summary = UsageSummary::from_supervisor(usage.clone());
                usage_summary.add_subagent(collected.subagent_usage);
                usage_summary.add_external(collected.external_usage);
                Ok(RunOutput {
                    reply: Reply::from_parts(text, Some(usage), stop_reason),
                    response: None,
                    usage: usage_summary,
                    tool_calls: collected.tool_calls,
                    delegations: collected.delegations,
                    artifacts: collected.artifacts,
                    events: weave_approval_events(collected.events, recorded_approvals),
                })
            }
            LoopCursor::Error(error) => Err(classify_error(error)),
            other => Err(FacadeError::Agent(AgentError::Other(format!(
                "agent run ended on a non-terminal cursor ({:?})",
                other.kind()
            )))),
        }
    });

    Ok(AgentRunStream {
        future,
        sink,
        output: None,
        state: DriveState::Driving,
        machine,
        control,
        ids: stream_ids,
    })
}

/// Opens a streamed *rules-routed* turn: the whole task is handed to
/// `delegate_name` with no supervisor LLM step (`docs/facade-api.md` §13.2).
///
/// The delegate is driven eagerly-in-future via [`drive_rules_routed`]; its
/// bracketing [`RunEvent`]s (`DelegationStarted`, per-artifact
/// `DelegationArtifact`, then `DelegationFinished` or `DelegationFailed`) are
/// replayed into the shared sink so a caller streaming the turn observes the
/// same events as a model-routed delegation, then the terminal [`RunOutput`] is
/// yielded as `Done`.
///
/// As on the normal streaming path, an external delegate's session facts are not
/// retained here (the drive owns its inputs and does not touch `agent`); a
/// snapshot is taken between runs via [`Agent::run_full`](super::Agent::run_full).
fn start_rules_routed(
    agent: &mut Agent,
    delegate_name: String,
    task: String,
    cancel: CancelHandle,
) -> Result<AgentRunStream<'_>, FacadeError> {
    let run_id = agent.ids.run_id();
    let ctx = RunContext::new_root_with_cancellation(
        run_id,
        agent.budget,
        agent.ids.trace_root("agent-run"),
        cancel.token(),
    );
    let recorder = new_delegation_recorder();
    let handler = agent.build_delegation_handler(run_id, &ctx, recorder.clone())?;
    let target = agent.resolve_rules_target(&delegate_name)?;
    let ids = agent.ids.clone();
    let sink: EventSink = Arc::new(Mutex::new(VecDeque::new()));
    let control = StreamControl::new(cancel);
    let sink_for_future = sink.clone();
    let future = Box::pin(async move {
        let drive = drive_rules_routed(&handler, &recorder, &ids, &target, task, &ctx).await?;
        for event in &drive.output.events {
            emit(&sink_for_future, event.clone());
        }
        Ok(drive.output)
    });

    // The routed drive never steps the held machine, so its cursor stays `Idle` and
    // the stream's `Drop` finds no stranded turn to abandon; the cell is held only to
    // keep the `AgentRunStream` shape uniform across start paths.
    let stream_ids = agent.ids.clone();
    let machine: MachineCell = Rc::new(RefCell::new(&mut agent.machine));
    Ok(AgentRunStream {
        future,
        sink,
        output: None,
        state: DriveState::Driving,
        machine,
        control,
        ids: stream_ids,
    })
}

/// Opens a streamed *dispatcher-routed* turn: the whole task runs through the
/// facade cheap→verify→strong escalation loop with no supervisor LLM step
/// (`docs/facade-api.md` §13.3).
///
/// The loop is driven eagerly-in-future via [`drive_dispatcher_routed`]; its
/// ordered [`RunEvent`]s — each worker's `DelegationStarted` / per-artifact
/// `DelegationArtifact` / `DelegationFinished` or `DelegationFailed`, the
/// verifier's own bracketing events, and a `RunEvent::Escalated` at each
/// upgrade — are replayed into the shared sink so a caller streaming the turn
/// observes the same sequence, then the terminal [`RunOutput`] is yielded as
/// `Done`.
///
/// As on the other streaming paths, an external delegate's session facts are not
/// retained here (the drive owns its inputs and does not touch `agent`); a
/// snapshot is taken between runs via [`Agent::run_full`](super::Agent::run_full).
fn start_dispatcher_routed(
    agent: &mut Agent,
    task: String,
    cancel: CancelHandle,
) -> Result<AgentRunStream<'_>, FacadeError> {
    let config = agent
        .delegation
        .dispatcher_config()
        .cloned()
        .ok_or_else(|| {
            FacadeError::InvalidState("dispatcher config missing on a dispatcher stream".to_owned())
        })?;
    let run_id = agent.ids.run_id();
    let ctx = RunContext::new_root_with_cancellation(
        run_id,
        agent.budget,
        agent.ids.trace_root("agent-run"),
        cancel.token(),
    );
    let recorder = new_delegation_recorder();
    let handler = agent.build_delegation_handler(run_id, &ctx, recorder.clone())?;
    let targets = agent.resolve_dispatcher_targets(&config)?;
    let evaluator = agent.delegation.dispatcher_evaluator_hook().cloned();
    let verifier = agent.delegation.dispatcher_verifier_hook().cloned();
    let ids = agent.ids.clone();
    let sink: EventSink = Arc::new(Mutex::new(VecDeque::new()));
    let control = StreamControl::new(cancel);
    let sink_for_future = sink.clone();
    let future = Box::pin(async move {
        let drive = drive_dispatcher_routed(
            &handler, &recorder, &ids, &config, &targets, task, &ctx, evaluator, verifier,
        )
        .await?;
        for event in &drive.output.events {
            emit(&sink_for_future, event.clone());
        }
        Ok(drive.output)
    });

    // As with the rules-routed path, the dispatcher drive never steps the held
    // machine, so `Drop` finds no stranded turn; the cell keeps the shape uniform.
    let stream_ids = agent.ids.clone();
    let machine: MachineCell = Rc::new(RefCell::new(&mut agent.machine));
    Ok(AgentRunStream {
        future,
        sink,
        output: None,
        state: DriveState::Driving,
        machine,
        control,
        ids: stream_ids,
    })
}

/// The streaming counterpart to [`Agent::run_full`](super::Agent::run_full).
///
/// `AgentRunStream` implements [`futures::Stream`] with
/// `Item = Result<RunEvent, FacadeError>` and also offers an inherent
/// [`next`](AgentRunStream::next) convenience so callers need not import
/// [`futures::StreamExt`]. It forwards each live
/// [`RunEvent::TextDelta`] / [`RunEvent::ToolStarted`] /
/// [`RunEvent::ToolFinished`] / [`RunEvent::ApprovalRequested`] in the order the
/// drive reaches it, then ends with exactly one [`RunEvent::Done`] carrying the
/// complete [`RunOutput`]. On failure it yields a single `Err` and then ends.
///
/// The turn is committed to the agent's [`Conversation`](crate::conversation::Conversation)
/// only when the drive reaches its terminal `Done`. If the stream is dropped before
/// then — including before it is ever polled — its [`Drop`] implementation abandons
/// the in-flight turn through the machine's sans-io never-resume input, so the agent
/// is left at a consistent point where the next
/// [`run`](super::Agent::run) or [`stream`](super::Agent::stream) can continue.
///
/// [`Agent::stream`]: super::Agent::stream
pub struct AgentRunStream<'a> {
    /// The deferred machine drive; resolves to the terminal [`RunOutput`].
    future: Pin<Box<dyn Future<Output = Result<RunOutput, FacadeError>> + 'a>>,
    /// Live events pushed by the tapping handlers, drained ahead of the future.
    sink: EventSink,
    /// The terminal output, held between the drive completing and `Done` being
    /// yielded so any trailing live events drain first.
    output: Option<RunOutput>,
    /// Lifecycle of the fold-and-finish drive.
    state: DriveState,
    /// Shared handle to the agent's held machine, cloned by the drive future.
    ///
    /// The [`Drop`] guard uses it to abandon a stranded turn when the stream is
    /// dropped before the drive reaches a terminal cursor.
    machine: MachineCell<'a>,
    /// Shared cancellation and pivot controls for this live stream.
    control: StreamControl,
    /// Identity source used to stamp facade-created pivot messages.
    ids: FacadeIds,
}

/// Lifecycle of an [`AgentRunStream`]'s drive.
#[derive(Debug, PartialEq, Eq)]
enum DriveState {
    /// The machine drive is still running; live events may still be produced.
    Driving,
    /// The drive finished successfully; trailing live events drain, then `Done`.
    Draining,
    /// The terminal `Done` (or an error) was produced; nothing more is yielded.
    Done,
}

impl AgentRunStream<'_> {
    /// Returns a clone of this stream's cooperative cancellation handle.
    #[must_use]
    pub fn cancel_handle(&self) -> CancelHandle {
        self.control.cancel.clone()
    }

    /// Requests cooperative cancellation of this streamed run.
    pub fn cancel(&self) {
        self.control.cancel.cancel();
    }

    /// Queues a human-authored pivot for the currently open streamed step boundary.
    ///
    /// The pivot is accepted only during the short boundary window after a tool
    /// phase has closed and before the next LLM request is fulfilled. The drive
    /// consumes the queued pivot on the next poll and re-renders that LLM request
    /// through the lower-level machine pivot path.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::InvalidState`] when the stream is not currently at
    /// a streamed pivot boundary, or [`FacadeError::Agent`] when the supplied
    /// message is not a user-role message.
    pub fn interject(&mut self, input: impl IntoUserMessage) -> Result<(), FacadeError> {
        let pivot = PivotMessage::new(
            self.ids.message_id(),
            input.into_user_message(),
            PivotSource::Human,
        )
        .map_err(|error| FacadeError::Agent(AgentError::State(error)))?;
        self.interject_pivot(pivot)
    }

    /// Queues a fully specified pivot message for the current streamed boundary.
    ///
    /// Use this when the host needs a non-human [`PivotSource`] or externally
    /// allocated message id. Most callers should use [`interject`](Self::interject).
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::InvalidState`] when the stream is not currently at
    /// a streamed pivot boundary or another pivot is already queued for it.
    pub fn interject_pivot(&mut self, pivot: PivotMessage) -> Result<(), FacadeError> {
        if self.state != DriveState::Driving {
            return Err(FacadeError::InvalidState(
                "cannot inject a pivot after the streamed run has finished".to_owned(),
            ));
        }
        self.control.enqueue_pivot(pivot)
    }

    /// Returns the next event, or `None` once the stream is exhausted.
    ///
    /// This is an inherent convenience equivalent to
    /// [`StreamExt::next`](futures::StreamExt::next); it lets callers write
    /// `stream.next().await` without importing [`futures::StreamExt`].
    pub async fn next(&mut self) -> Option<Result<RunEvent, FacadeError>> {
        StreamExt::next(self).await
    }

    /// Abandons any in-flight turn left open when the drive is not terminal.
    ///
    /// This is the single cleanup path shared by the [`Drop`] guard. It is
    /// idempotent: once the drive is terminal — because it yielded a `Done`,
    /// surfaced an error, or was already abandoned — `state` is [`DriveState::Done`]
    /// and the call is a no-op that never rolls back an already-committed turn.
    ///
    /// When the drive is *not* terminal the turn is closed through the machine's
    /// sans-io never-resume input ([`StepInput::Abandon`]): the loop cursor still
    /// carries the outstanding requirement id(s) even though the parked drive future
    /// no longer holds the machine, so feeding `Abandon` for the first of them
    /// discards (or, for a tool phase, cancels) the pending turn and settles the
    /// cursor back to a feedable `Idle`. A cursor with no outstanding requirement
    /// (never stepped, or already terminal) yields nothing to abandon.
    fn abandon(&mut self) {
        if self.state == DriveState::Done {
            return;
        }
        self.state = DriveState::Done;

        // The drive future releases its machine borrow before every `await`, so a
        // drop that lands while it is parked can take the machine here. `try` keeps
        // the guard defensive: a failed borrow simply skips cleanup rather than
        // panicking inside `drop`.
        let Ok(mut guard) = self.machine.try_borrow_mut() else {
            return;
        };
        abandon_in_flight_turn(&mut guard);
    }
}

impl Stream for AgentRunStream<'_> {
    type Item = Result<RunEvent, FacadeError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // `Pin<Box<..>>` and every other field is `Unpin`, so unstructured access
        // is sound.
        let this = self.get_mut();

        loop {
            // Live events always take priority so the stream stays in true drive
            // order and any events buffered before a park are handed out.
            if let Some(event) = pop(&this.sink) {
                return Poll::Ready(Some(Ok(event)));
            }

            match this.state {
                DriveState::Driving => match this.future.as_mut().poll(cx) {
                    Poll::Ready(Ok(output)) => {
                        this.output = Some(output);
                        this.state = DriveState::Draining;
                        // Loop back to drain any events the final step produced
                        // before yielding the terminal `Done`.
                    }
                    Poll::Ready(Err(error)) => {
                        this.state = DriveState::Done;
                        return Poll::Ready(Some(Err(error)));
                    }
                    Poll::Pending => {
                        // The tapping handlers may have pushed events just before
                        // the drive parked; hand one out instead of stalling.
                        if let Some(event) = pop(&this.sink) {
                            return Poll::Ready(Some(Ok(event)));
                        }
                        return Poll::Pending;
                    }
                },
                DriveState::Draining => {
                    this.state = DriveState::Done;
                    let output = this.output.take().expect("terminal output present");
                    return Poll::Ready(Some(Ok(RunEvent::Done(Box::new(output)))));
                }
                DriveState::Done => return Poll::Ready(None),
            }
        }
    }
}

impl Drop for AgentRunStream<'_> {
    /// Abandons any in-flight turn left open when the stream is dropped early.
    ///
    /// [`Agent::stream`](super::Agent::stream) commits its turn only when the drive
    /// reaches a terminal `Done`; a caller that drops the stream before then would
    /// otherwise strand the machine's pending turn, breaking the next `run` or
    /// `stream`. `abandon` closes it through the machine's sans-io
    /// never-resume input and is idempotent, so a stream dropped after a committed
    /// `Done` or an error is left untouched.
    fn drop(&mut self) {
        self.abandon();
    }
}

/// A [`HandlerScope`] whose LLM, tool, and interaction handlers all tap live
/// events into the shared sink while fulfilling their requirement.
struct FacadeStreamScope {
    llm: StreamingTapHandler,
    tool: TapToolHandler,
    interaction: TapInteractionHandler,
}

impl HandlerScope for FacadeStreamScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        Some(&self.llm)
    }

    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&self.tool)
    }

    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        Some(&self.interaction)
    }
}

/// Fulfills a `NeedLlm` by streaming the client and folding it back to a
/// [`Response`], emitting a [`RunEvent::TextDelta`] per text delta.
///
/// Unlike the reference [`LlmClientHandler`](crate::agent::LlmClientHandler),
/// this always drives [`chat_stream`](LlmClient::chat_stream) regardless of the
/// requested [`LlmStepMode`], because the incremental deltas are the whole point
/// of the stream. The folded [`Response`] is identical to what the non-streaming
/// path would return for the same generation, so the machine loop is unaffected.
struct StreamingTapHandler {
    client: Arc<dyn LlmClient>,
    sink: EventSink,
}

impl StreamingTapHandler {
    /// Folds a client event stream into a complete [`Response`], forwarding each
    /// text delta as a live [`RunEvent::TextDelta`].
    async fn fold(
        &self,
        mut stream: BoxStream<'static, Result<StreamEvent, ClientError>>,
    ) -> Result<Response, ClientError> {
        let mut accumulator = Accumulator::new();
        while let Some(item) = stream.next().await {
            let event = item?;
            if let StreamEvent::BlockDelta {
                delta: Delta::Text(text),
                ..
            } = &event
            {
                emit(&self.sink, RunEvent::TextDelta(text.clone()));
            }
            accumulator.push(event).map_err(client_error)?;
        }
        accumulator.finish().map_err(client_error)
    }
}

#[async_trait]
impl LlmHandler for StreamingTapHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        _mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        let call = async {
            let mut request = request.clone();
            request.stream = true;
            match self.client.chat_stream(request).await {
                Ok(stream) => self.fold(stream).await,
                Err(error) => Err(error),
            }
        };
        // Same cancellation wiring as the reference `LlmClientHandler` (M4-5):
        // a cancelled run drops the in-flight stream instead of waiting it
        // out; the placeholder error is discarded by the driver's post-batch
        // cancel re-check, which settles the requirement as a never-resume.
        tokio::select! {
            biased;
            _ = ctx.cancellation().cancelled() => RequirementResult::Llm(Err(
                ClientError::Other("llm call interrupted: run context cancelled".to_owned()),
            )),
            result = call => RequirementResult::Llm(result),
        }
    }
}

/// Fulfills a `NeedTool` by delegating to the run-scoped
/// [`DelegationToolHandler`], bracketing an ordinary tool call with live
/// [`RunEvent::ToolStarted`] / [`RunEvent::ToolFinished`] and a delegation call
/// with [`RunEvent::DelegationStarted`] / [`RunEvent::DelegationFinished`] (or
/// [`RunEvent::DelegationFailed`]).
///
/// A delegation drives its child synchronously inside `fulfill`, so both live
/// delegation events are emitted once the child settles, carrying the trace the
/// handler recorded (its final status and child usage).
struct TapToolHandler {
    inner: DelegationToolHandler,
    recorder: DelegationRecorder,
    sink: EventSink,
}

#[async_trait]
impl ToolHandler for TapToolHandler {
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        ctx: &RunContext,
    ) -> RequirementResult {
        if self.inner.is_delegation(&call.name) {
            let result = self.inner.fulfill(call_id, call, ctx).await;
            if let Some(record) = self
                .recorder
                .lock()
                .expect("delegation recorder poisoned")
                .get(&call_id.to_string())
                .cloned()
            {
                let trace = record.trace;
                emit(&self.sink, RunEvent::DelegationStarted(trace.clone()));
                match trace.status {
                    DelegationStatus::Completed => {
                        for artifact in record.artifacts {
                            emit(&self.sink, RunEvent::DelegationArtifact(artifact));
                        }
                        emit(&self.sink, RunEvent::DelegationFinished(trace));
                    }
                    DelegationStatus::Failed => {
                        emit(&self.sink, RunEvent::DelegationFailed(trace));
                    }
                }
            }
            return result;
        }

        let trace = ToolTrace {
            name: call.name.clone(),
            call_id: call_id.to_string(),
        };
        emit(&self.sink, RunEvent::ToolStarted(trace.clone()));
        let result = self.inner.fulfill(call_id, call, ctx).await;
        emit(&self.sink, RunEvent::ToolFinished(trace));
        result
    }
}

/// Fulfills a `NeedInteraction` (approval) by delegating to the resolved
/// [`InteractionHandler`], emitting a live [`RunEvent::ApprovalRequested`] first
/// and recording the same request for the terminal [`RunOutput::events`].
///
/// The delegate `inner` is the host-injected handler when one was supplied to
/// [`AgentBuilder::interaction_handler`](crate::facade::AgentBuilder::interaction_handler),
/// otherwise the shared [`FacadeApproval`] fallback. The enriched request (tool
/// name plus a redacted input summary) is always recovered from `approval` — the
/// machine gate stays [`FacadeApproval`] and records the pending decision
/// regardless of which handler answers, so the emit is populated even under an
/// injected handler — while the `call_id` and `reason` are taken from the
/// machine-carried interaction. The pending entry is peeked *before* delegating,
/// because the fallback handler consumes and removes it.
struct TapInteractionHandler {
    approval: Arc<FacadeApproval>,
    inner: Arc<dyn InteractionHandler>,
    recorder: ApprovalRecorder,
    sink: EventSink,
}

#[async_trait]
impl InteractionHandler for TapInteractionHandler {
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult {
        if let InteractionKind::Approval {
            call_id,
            requirement,
        } = request.kind()
        {
            // The enriched request (tool name + redacted input summary) is
            // recovered from the pending decision the policy recorded; the
            // `call_id` and `reason` are re-bound from the machine-carried
            // interaction so the emit reflects exactly what the machine paused
            // on, even under an injected handler.
            let approval_request = enriched_approval_request(&self.approval, *call_id, requirement);
            self.recorder
                .lock()
                .expect("approval recorder poisoned")
                .push(approval_request.clone());
            emit(&self.sink, RunEvent::ApprovalRequested(approval_request));
        }
        self.inner.fulfill(request, ctx).await
    }
}

/// Maps an [`AccumulatorError`] into the [`ClientError`] the `NeedLlm` result
/// family carries.
///
/// A provider-emitted stream error already carries a [`ClientError`]; every other
/// accumulator failure is a stream-protocol violation reported as
/// [`ClientError::Protocol`].
fn client_error(error: AccumulatorError) -> ClientError {
    match error {
        AccumulatorError::Stream(client) => client,
        other => ClientError::Protocol(other.to_string()),
    }
}
