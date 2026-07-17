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
//!   [`FacadeApproval`] and emits a [`RunEvent::ApprovalRequested`] labelled with
//!   the pending tool name before delegating the approval.
//!
//! When the drive finishes the terminal [`RunOutput`] is assembled exactly as
//! [`Agent::run_full`](super::Agent::run_full) assembles it, then yielded as one
//! [`RunEvent::Done`]. A failure aborts the in-flight turn inside the machine and
//! is surfaced as an `Err` stream item, leaving the agent's committed history
//! unchanged.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt};

use crate::agent::interaction::{Interaction, InteractionKind};
use crate::agent::{
    AgentError, AgentInput, BudgetLimits, HandlerScope, InteractionHandler, LlmHandler,
    LlmStepMode, LoopCursor, RequirementResult, RunContext, ToolHandler, ToolRegistry,
    ToolRegistryHandler, drain,
};
use crate::client::{ChatRequest, ClientError, LlmClient, Response};
use crate::conversation::ToolCallId;
use crate::facade::approval::FacadeApproval;
use crate::facade::delegate::{DelegationRecorder, DelegationToolHandler, new_delegation_recorder};
use crate::facade::error::FacadeError;
use crate::facade::run::{
    ApprovalRequest, DelegationStatus, Reply, RunEvent, RunOutput, ToolTrace, UsageSummary,
};
use crate::facade::tool::{FacadeToolRegistry, ToolContextParts};
use crate::model::message::Message;
use crate::model::tool::ToolCall;
use crate::stream::accumulator::{Accumulator, AccumulatorError};
use crate::stream::{Delta, StreamEvent};

use super::{
    Agent, classify_error, collect_traces, drive_dispatcher_routed, drive_rules_routed,
    final_turn_summary, user_message_text,
};

/// A shared sink the tapping handlers push live [`RunEvent`]s into while the
/// drive future runs, drained in order by [`AgentRunStream::poll_next`].
type EventSink = Arc<Mutex<VecDeque<RunEvent>>>;

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

/// Opens one streamed agent turn over `agent`, returning an [`AgentRunStream`].
///
/// The run-scoped context, tool registry, and user input are built eagerly so a
/// registry-build or input-validation failure surfaces from the returned
/// `Result` rather than from the first poll. The machine drive itself is deferred
/// into the stream's future.
pub(super) fn start(
    agent: &mut Agent,
    message: Message,
) -> Result<AgentRunStream<'_>, FacadeError> {
    // Rules-routed delegation short-circuits the supervisor loop: if the task
    // text matches a routing rule, the whole turn is handed to the matched
    // delegate and no LLM step is taken (`docs/facade-api.md` §13.2). A
    // non-matching task falls through to the normal machine drive below.
    if agent.delegation.is_rules_routed() {
        let task = user_message_text(&message);
        if let Some(delegate_name) = agent.delegation.route_task(&task).map(str::to_owned) {
            return start_rules_routed(agent, delegate_name, task);
        }
    }

    // Dispatcher-routed delegation runs the whole task through the facade
    // cheap→verify→strong loop with no supervisor LLM step (§13.3).
    if agent.delegation.is_dispatcher_routed() {
        let task = user_message_text(&message);
        return start_dispatcher_routed(agent, task);
    }

    let run_id = agent.ids.run_id();
    let ctx = RunContext::new_root(
        run_id,
        BudgetLimits::unbounded(),
        agent.ids.trace_root("agent-run"),
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
    let sink: EventSink = Arc::new(Mutex::new(VecDeque::new()));
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
            inner: agent.approval.clone(),
            sink: sink.clone(),
        },
    };

    let machine = &mut agent.machine;
    let future = Box::pin(async move {
        let done = drain(machine, agent_input, &scope, None, &ctx).await?;
        let collected = collect_traces(done.notifications(), &recorder);
        // A denied external delegate surfaces as a run-level error, matching
        // `run_full` (§9.2). Retention of external session facts is not possible
        // on the streaming path (the future holds `&mut machine` for the stream's
        // lifetime), so a snapshot is taken between runs via `run_full`.
        if collected.external_approval_denied {
            return Err(FacadeError::ApprovalDenied);
        }
        match done.cursor() {
            LoopCursor::Done(_) => {
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
                    events: collected.events,
                })
            }
            LoopCursor::Error(error) => Err(classify_error(error.message())),
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
) -> Result<AgentRunStream<'_>, FacadeError> {
    let run_id = agent.ids.run_id();
    let ctx = RunContext::new_root(
        run_id,
        BudgetLimits::unbounded(),
        agent.ids.trace_root("agent-run"),
    );
    let recorder = new_delegation_recorder();
    let handler = agent.build_delegation_handler(run_id, &ctx, recorder.clone())?;
    let target = agent.resolve_rules_target(&delegate_name)?;
    let ids = agent.ids.clone();
    let sink: EventSink = Arc::new(Mutex::new(VecDeque::new()));
    let sink_for_future = sink.clone();
    let future = Box::pin(async move {
        let drive = drive_rules_routed(&handler, &recorder, &ids, &target, task, &ctx).await?;
        for event in &drive.output.events {
            emit(&sink_for_future, event.clone());
        }
        Ok(drive.output)
    });

    Ok(AgentRunStream {
        future,
        sink,
        output: None,
        state: DriveState::Driving,
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
) -> Result<AgentRunStream<'_>, FacadeError> {
    let config = agent
        .delegation
        .dispatcher_config()
        .cloned()
        .ok_or_else(|| {
            FacadeError::InvalidState("dispatcher config missing on a dispatcher stream".to_owned())
        })?;
    let run_id = agent.ids.run_id();
    let ctx = RunContext::new_root(
        run_id,
        BudgetLimits::unbounded(),
        agent.ids.trace_root("agent-run"),
    );
    let recorder = new_delegation_recorder();
    let handler = agent.build_delegation_handler(run_id, &ctx, recorder.clone())?;
    let targets = agent.resolve_dispatcher_targets(&config)?;
    let ids = agent.ids.clone();
    let sink: EventSink = Arc::new(Mutex::new(VecDeque::new()));
    let sink_for_future = sink.clone();
    let future = Box::pin(async move {
        let drive =
            drive_dispatcher_routed(&handler, &recorder, &ids, &config, &targets, task, &ctx)
                .await?;
        for event in &drive.output.events {
            emit(&sink_for_future, event.clone());
        }
        Ok(drive.output)
    });

    Ok(AgentRunStream {
        future,
        sink,
        output: None,
        state: DriveState::Driving,
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
    /// Returns the next event, or `None` once the stream is exhausted.
    ///
    /// This is an inherent convenience equivalent to
    /// [`StreamExt::next`](futures::StreamExt::next); it lets callers write
    /// `stream.next().await` without importing [`futures::StreamExt`].
    pub async fn next(&mut self) -> Option<Result<RunEvent, FacadeError>> {
        StreamExt::next(self).await
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
        _ctx: &RunContext,
    ) -> RequirementResult {
        let mut request = request.clone();
        request.stream = true;
        let result = match self.client.chat_stream(request).await {
            Ok(stream) => self.fold(stream).await,
            Err(error) => Err(error),
        };
        RequirementResult::Llm(result)
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

/// Fulfills a `NeedInteraction` (approval) by delegating to the shared
/// [`FacadeApproval`], emitting a live [`RunEvent::ApprovalRequested`] first.
///
/// An [`Approval`](InteractionKind::Approval) interaction only carries the
/// framework [`ToolCallId`], so the tool name is recovered from the pending
/// decision the policy already recorded (peeked *before* delegating, because the
/// delegate consumes and removes that pending entry).
struct TapInteractionHandler {
    inner: Arc<FacadeApproval>,
    sink: EventSink,
}

#[async_trait]
impl InteractionHandler for TapInteractionHandler {
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult {
        if let InteractionKind::Approval { call_id, .. } = request.kind() {
            let tool_name = self.inner.pending_tool_name(*call_id).unwrap_or_default();
            emit(
                &self.sink,
                RunEvent::ApprovalRequested(ApprovalRequest { tool_name }),
            );
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
