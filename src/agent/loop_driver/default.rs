//! Default text-only LLM loop driver.
//!
//! The driver wires one [`crate::client::LlmClient`] call into the checked
//! [`crate::conversation::Conversation`] pending boundary. It intentionally
//! stops at text-only assistant completion; tool-use orchestration is added by
//! the next milestone task.

use super::{AgentEventStream, AgentFeedGuard, AgentLoop, BoxAgentEventStream};
use crate::{
    agent::{
        AgentError, AgentEvent, AgentInput, AgentOutcome, AgentState, LoopCursor, PivotMessage,
        RunContext, StepBoundary, TraceNodeId,
    },
    client::{ChatRequest, ClientError, LlmClient, Response},
    conversation::{AssistantFinish, CancelDisposition, MessageId, TurnMeta},
    stream::StreamEvent,
};
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};
use std::{
    collections::VecDeque,
    fmt,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    task::{Context, Poll},
};

type SharedAgentState = Arc<Mutex<AgentState>>;

/// LLM transport mode used by [`DefaultAgentLoop`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LlmStepMode {
    /// Use [`LlmClient::chat`] and fold a complete response into Conversation.
    NonStreaming,
    /// Use [`LlmClient::chat_stream`] and emit each [`StreamEvent`] as it arrives.
    Streaming,
}

impl LlmStepMode {
    const fn request_stream_flag(self) -> bool {
        matches!(self, Self::Streaming)
    }
}

/// Default Agent loop for one text-only LLM step.
///
/// The loop owns the data half of the Agent in a shared, private mutex so a
/// returned streaming event sequence can keep mutating the single active
/// Conversation after `feed` returns. Public inspection stays read-only through
/// [`inspect_state`](Self::inspect_state).
pub struct DefaultAgentLoop {
    client: Arc<dyn LlmClient>,
    state: SharedAgentState,
    context: RunContext,
    mode: LlmStepMode,
    guard: AgentFeedGuard,
}

impl DefaultAgentLoop {
    /// Creates a default text-only loop driver.
    #[must_use]
    pub fn new(
        client: Arc<dyn LlmClient>,
        state: AgentState,
        context: RunContext,
        mode: LlmStepMode,
    ) -> Self {
        Self {
            client,
            state: Arc::new(Mutex::new(state)),
            context,
            mode,
            guard: AgentFeedGuard::new(),
        }
    }

    /// Returns the configured LLM transport mode.
    #[must_use]
    pub const fn mode(&self) -> LlmStepMode {
        self.mode
    }

    /// Returns whether a feed event stream is currently active.
    #[must_use]
    pub fn feed_in_progress(&self) -> bool {
        self.guard.is_active()
    }

    /// Inspects the Agent state without exposing mutable runtime internals.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Other`] if the internal state mutex was poisoned
    /// by a panic in another caller.
    pub fn inspect_state<R>(
        &self,
        inspect: impl FnOnce(&AgentState) -> R,
    ) -> Result<R, AgentError> {
        let state = lock_agent_state(&self.state)?;
        Ok(inspect(&state))
    }

    /// Unwraps the owned Agent state when no feed stream still holds it.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::FeedInProgress`] if an active stream still owns a
    /// shared state handle, or [`AgentError::Other`] if the mutex was poisoned.
    pub fn into_state(self) -> Result<AgentState, AgentError> {
        let state = Arc::try_unwrap(self.state).map_err(|_| AgentError::FeedInProgress)?;
        state
            .into_inner()
            .map_err(|_| AgentError::Other("agent state mutex poisoned".to_owned()))
    }

    async fn feed_non_streaming(
        &self,
        input: AgentInput,
    ) -> Result<BoxAgentEventStream, AgentError> {
        let prepared = self.prepare_step(input)?;
        let response = match self.client.chat(prepared.request).await {
            Ok(response) => response,
            Err(error) => {
                self.abort_prepared_step()?;
                return Err(AgentError::Client(error));
            }
        };

        let events = {
            let mut state = lock_agent_state(&self.state)?;
            apply_complete_response(
                &mut state,
                response,
                prepared.step_id,
                prepared.assistant_message_id,
                prepared.trace_node_id,
            )?
        };

        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }

    async fn feed_streaming(&self, input: AgentInput) -> Result<BoxAgentEventStream, AgentError> {
        let prepared = self.prepare_step(input)?;
        let source = match self.client.chat_stream(prepared.request).await {
            Ok(source) => source,
            Err(error) => {
                self.abort_prepared_step()?;
                return Err(AgentError::Client(error));
            }
        };

        {
            let mut state = lock_agent_state(&self.state)?;
            if let Err(error) = state.conversation_mut().start_assistant() {
                abort_pending_and_idle(&mut state)?;
                return Err(AgentError::Conversation(error));
            }
        }

        Ok(Box::pin(StreamingStep::new(
            Arc::clone(&self.state),
            source,
            prepared.step_id,
            prepared.assistant_message_id,
            prepared.trace_node_id,
        )))
    }

    fn prepare_step(&self, input: AgentInput) -> Result<PreparedStep, AgentError> {
        let user = match input {
            AgentInput::UserMessage(user) => user,
            AgentInput::Resume(_) => {
                return Err(AgentError::Other(
                    "resume feed input is not supported by the default text-only LLM driver"
                        .to_owned(),
                ));
            }
        };

        self.context.check_cancelled()?;

        let step_id = user.step_id();
        let assistant_message_id = user.assistant_message_id();
        let request = {
            let mut state = lock_agent_state(&self.state)?;
            state.transition_cursor(LoopCursor::streaming_step(step_id))?;
            if let Err(error) = state.conversation_mut().begin_turn(
                user.turn_id(),
                user.message_id(),
                user.message().clone(),
            ) {
                state.transition_cursor(LoopCursor::Idle)?;
                return Err(AgentError::Conversation(error));
            }
            build_chat_request(&state, self.mode.request_stream_flag())
        };

        if let Err(error) = self.context.charge_step() {
            self.abort_prepared_step()?;
            return Err(AgentError::RunContext(error));
        }

        let trace_node_id = match self
            .context
            .trace()
            .record_step(TraceNodeId::new(step_id.to_string()), step_id)
        {
            Ok(record) => Some(record.id().clone()),
            Err(error) => {
                self.abort_prepared_step()?;
                return Err(AgentError::RunContext(error.into()));
            }
        };

        Ok(PreparedStep {
            request,
            step_id,
            assistant_message_id,
            trace_node_id,
        })
    }

    fn abort_prepared_step(&self) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        abort_pending_and_idle(&mut state)
    }
}

#[async_trait]
impl AgentLoop for DefaultAgentLoop {
    async fn feed(&mut self, input: AgentInput) -> Result<AgentEventStream, AgentError> {
        let permit = self.guard.try_acquire()?;
        let stream = match self.mode {
            LlmStepMode::NonStreaming => self.feed_non_streaming(input).await,
            LlmStepMode::Streaming => self.feed_streaming(input).await,
        };
        stream.map(|stream| AgentEventStream::new(stream, permit))
    }

    fn interject(&self, message: PivotMessage) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        state.queue_pivot(message).map_err(AgentError::from)
    }
}

impl fmt::Debug for DefaultAgentLoop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DefaultAgentLoop")
            .field("mode", &self.mode)
            .field("feed_in_progress", &self.feed_in_progress())
            .finish_non_exhaustive()
    }
}

struct PreparedStep {
    request: ChatRequest,
    step_id: crate::agent::StepId,
    assistant_message_id: MessageId,
    trace_node_id: Option<TraceNodeId>,
}

struct StreamingStep {
    state: SharedAgentState,
    source: futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>,
    step_id: crate::agent::StepId,
    assistant_message_id: MessageId,
    trace_node_id: Option<TraceNodeId>,
    queued: VecDeque<Result<AgentEvent, AgentError>>,
    done: bool,
}

impl StreamingStep {
    fn new(
        state: SharedAgentState,
        source: futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>,
        step_id: crate::agent::StepId,
        assistant_message_id: MessageId,
        trace_node_id: Option<TraceNodeId>,
    ) -> Self {
        Self {
            state,
            source,
            step_id,
            assistant_message_id,
            trace_node_id,
            queued: VecDeque::new(),
            done: false,
        }
    }

    fn fail(&mut self, error: AgentError) -> Poll<Option<Result<AgentEvent, AgentError>>> {
        self.done = true;
        Poll::Ready(Some(Err(error)))
    }
}

impl Stream for StreamingStep {
    type Item = Result<AgentEvent, AgentError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(event) = this.queued.pop_front() {
            return Poll::Ready(Some(event));
        }
        if this.done {
            return Poll::Ready(None);
        }

        match this.source.poll_next_unpin(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(event))) => {
                let push = lock_agent_state(&this.state).and_then(|mut state| {
                    state
                        .conversation_mut()
                        .push_assistant_event(event.clone())
                        .map_err(AgentError::Conversation)
                        .or_else(|error| {
                            abort_pending_and_idle(&mut state)?;
                            Err(error)
                        })
                });
                match push {
                    Ok(()) => Poll::Ready(Some(Ok(AgentEvent::Llm(event)))),
                    Err(error) => this.fail(error),
                }
            }
            Poll::Ready(Some(Err(error))) => {
                let cleanup = lock_agent_state(&this.state)
                    .and_then(|mut state| abort_pending_and_idle(&mut state));
                if let Err(cleanup_error) = cleanup {
                    return this.fail(cleanup_error);
                }
                this.fail(AgentError::Client(error))
            }
            Poll::Ready(None) => {
                let finalize = lock_agent_state(&this.state).and_then(|mut state| {
                    apply_finished_stream(
                        &mut state,
                        this.step_id,
                        this.assistant_message_id,
                        this.trace_node_id.clone(),
                    )
                });
                match finalize {
                    Ok(events) => {
                        this.queued.extend(events.into_iter().map(Ok));
                        this.done = true;
                        Poll::Ready(this.queued.pop_front())
                    }
                    Err(error) => this.fail(error),
                }
            }
        }
    }
}

fn build_chat_request(state: &AgentState, stream: bool) -> ChatRequest {
    let effective = state.conversation().effective_view();
    let (system, mut messages) = effective.into_parts();
    if let Some(pending) = state.conversation().pending_context() {
        messages.extend(pending.into_messages());
    }
    let model = state.spec().model();

    ChatRequest {
        model: model.model().to_owned(),
        messages,
        tools: state.spec().initial_tools().tools().to_vec(),
        system: system.or_else(|| state.spec().system_prompt().map(ToOwned::to_owned)),
        max_tokens: model.max_tokens().get(),
        temperature: model.temperature(),
        stream,
        provider_extras: model.provider_extras().cloned(),
    }
}

fn apply_complete_response(
    state: &mut AgentState,
    response: Response,
    step_id: crate::agent::StepId,
    assistant_message_id: MessageId,
    trace_node_id: Option<TraceNodeId>,
) -> Result<Vec<AgentEvent>, AgentError> {
    if let Err(error) = state.conversation_mut().start_assistant_response(response) {
        abort_pending_and_idle(state)?;
        return Err(AgentError::Conversation(error));
    }
    apply_finished_stream(state, step_id, assistant_message_id, trace_node_id)
}

fn apply_finished_stream(
    state: &mut AgentState,
    step_id: crate::agent::StepId,
    assistant_message_id: MessageId,
    trace_node_id: Option<TraceNodeId>,
) -> Result<Vec<AgentEvent>, AgentError> {
    let result = finish_and_commit_ready_turn(state, step_id, assistant_message_id, trace_node_id);
    match result {
        Ok(events) => Ok(events),
        Err(error) => {
            abort_pending_and_idle(state)?;
            Err(error)
        }
    }
}

fn finish_and_commit_ready_turn(
    state: &mut AgentState,
    step_id: crate::agent::StepId,
    assistant_message_id: MessageId,
    trace_node_id: Option<TraceNodeId>,
) -> Result<Vec<AgentEvent>, AgentError> {
    let finish = state
        .conversation_mut()
        .finish_assistant(assistant_message_id)?;
    if finish != AssistantFinish::ReadyToCommit {
        return Err(AgentError::Other(
            "assistant response requested tool use; tool execution is implemented by M2-3"
                .to_owned(),
        ));
    }

    state
        .conversation_mut()
        .commit_pending(TurnMeta::default())?;
    let boundary = state.conversation().head();
    state.transition_cursor(LoopCursor::Idle)?;

    Ok(vec![
        AgentEvent::StepBoundary(StepBoundary::new(step_id, boundary, trace_node_id)),
        AgentEvent::Done(AgentOutcome::Completed),
    ])
}

fn abort_pending_and_idle(state: &mut AgentState) -> Result<(), AgentError> {
    if state.conversation().pending().is_some() {
        state
            .conversation_mut()
            .cancel_pending(CancelDisposition::DiscardTurn)?;
    }
    if matches!(
        state.loop_cursor(),
        LoopCursor::StreamingStep(_) | LoopCursor::CancelRecovery(_)
    ) {
        state.transition_cursor(LoopCursor::Idle)?;
    }
    Ok(())
}

fn lock_agent_state(state: &SharedAgentState) -> Result<MutexGuard<'_, AgentState>, AgentError> {
    state
        .lock()
        .map_err(|_| AgentError::Other("agent state mutex poisoned".to_owned()))
}

#[cfg(test)]
mod tests;
