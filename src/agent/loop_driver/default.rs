//! Default LLM loop driver with Conversation pending integration.
//!
//! The driver wires [`crate::client::LlmClient`] calls into the checked
//! [`crate::conversation::Conversation`] pending boundary. It can complete
//! text-only turns directly, or execute provider-neutral tool calls through a
//! runtime [`crate::agent::ToolRegistry`] before asking the model for the next
//! assistant response.

use super::{AgentEventStream, AgentFeedGuard, AgentLoop, BoxAgentEventStream};
use crate::{
    agent::{
        AgentError, AgentEvent, AgentInput, AgentOutcome, AgentState, DeclaredOnlyToolRegistry,
        LoopCursor, NoToolExecutionIds, PivotMessage, RunContext, StepBoundary, StepId,
        ToolCallFinished, ToolCallStarted, ToolExecutionIds, ToolFailurePolicy, ToolRegistry,
        ToolRuntimeError, TraceNodeId, state::CancelRecoveryReason,
    },
    client::{ChatRequest, ClientError, LlmClient, Response},
    conversation::{
        AssistantFinish, CancelDisposition, MessageId, ToolCallId, ToolCallMapping, TurnMeta,
    },
    model::{
        content::ContentBlock,
        tool::{ToolCall, ToolResponse},
    },
    stream::StreamEvent,
};
use async_trait::async_trait;
use futures::{StreamExt, future::join_all, stream};
use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

type SharedAgentState = Arc<Mutex<AgentState>>;
type SharedToolRegistry = Arc<dyn ToolRegistry>;
type SharedToolExecutionIds = Arc<dyn ToolExecutionIds>;

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

/// Default Agent loop for LLM and tool-use steps.
///
/// The loop owns the data half of the Agent in a shared, private mutex so a
/// returned streaming event sequence can keep mutating the single active
/// Conversation after `feed` returns. Public inspection stays read-only through
/// [`inspect_state`](Self::inspect_state).
pub struct DefaultAgentLoop {
    runtime: LoopRuntime,
    mode: LlmStepMode,
    guard: AgentFeedGuard,
}

impl DefaultAgentLoop {
    /// Creates a default loop driver.
    ///
    /// Static tool declarations from [`crate::agent::AgentSpec`] are preserved
    /// in outgoing requests, but no tool can execute unless
    /// [`with_tool_registry`](Self::with_tool_registry) is used.
    #[must_use]
    pub fn new(
        client: Arc<dyn LlmClient>,
        state: AgentState,
        context: RunContext,
        mode: LlmStepMode,
    ) -> Self {
        let declarations = state.spec().initial_tools().tools().to_vec();
        Self::with_tool_registry(
            client,
            state,
            context,
            mode,
            Arc::new(DeclaredOnlyToolRegistry::new(declarations)),
            Arc::new(NoToolExecutionIds),
        )
    }

    /// Creates a loop driver with executable tool runtime handles.
    ///
    /// The registry and id source are live runtime objects and are deliberately
    /// kept out of [`AgentState`] serde data.
    #[must_use]
    pub fn with_tool_registry(
        client: Arc<dyn LlmClient>,
        state: AgentState,
        context: RunContext,
        mode: LlmStepMode,
        tool_registry: SharedToolRegistry,
        tool_ids: SharedToolExecutionIds,
    ) -> Self {
        Self {
            runtime: LoopRuntime {
                client,
                state: Arc::new(Mutex::new(state)),
                context,
                tool_registry,
                tool_ids,
            },
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
        let state = lock_agent_state(&self.runtime.state)?;
        Ok(inspect(&state))
    }

    /// Unwraps the owned Agent state when no feed stream still holds it.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::FeedInProgress`] if an active stream still owns a
    /// shared state handle, or [`AgentError::Other`] if the mutex was poisoned.
    pub fn into_state(self) -> Result<AgentState, AgentError> {
        let state = Arc::try_unwrap(self.runtime.state).map_err(|_| AgentError::FeedInProgress)?;
        state
            .into_inner()
            .map_err(|_| AgentError::Other("agent state mutex poisoned".to_owned()))
    }

    async fn feed_non_streaming(
        &self,
        input: AgentInput,
    ) -> Result<BoxAgentEventStream, AgentError> {
        let prepared = self
            .runtime
            .prepare_user_turn(input, self.mode.request_stream_flag())?;
        let events = match self.runtime.run_non_streaming_segment(prepared).await {
            Ok(events) => events,
            Err(error) => {
                self.runtime.abort_pending_and_idle()?;
                return Err(error);
            }
        };

        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }

    async fn feed_streaming(&self, input: AgentInput) -> Result<BoxAgentEventStream, AgentError> {
        let prepared = self
            .runtime
            .prepare_user_turn(input, self.mode.request_stream_flag())?;
        let source = match self.runtime.open_streaming_call(&prepared).await {
            Ok(source) => source,
            Err(error) => {
                self.runtime.abort_pending_and_idle()?;
                return Err(error);
            }
        };
        if let Err(error) = self.runtime.start_streaming_assistant() {
            self.runtime.abort_pending_and_idle()?;
            return Err(error);
        }

        Ok(stream::unfold(
            StreamingSegment::new(self.runtime.clone(), source, prepared),
            StreamingSegment::next_event,
        )
        .boxed())
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
        let mut state = lock_agent_state(&self.runtime.state)?;
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

#[derive(Clone)]
struct LoopRuntime {
    client: Arc<dyn LlmClient>,
    state: SharedAgentState,
    context: RunContext,
    tool_registry: SharedToolRegistry,
    tool_ids: SharedToolExecutionIds,
}

impl LoopRuntime {
    fn prepare_user_turn(
        &self,
        input: AgentInput,
        stream: bool,
    ) -> Result<PreparedAssistantCall, AgentError> {
        let user = match input {
            AgentInput::UserMessage(user) => user,
            AgentInput::Resume(_) => {
                return Err(AgentError::Other(
                    "resume feed input is not supported by the default LLM driver".to_owned(),
                ));
            }
        };

        {
            let mut state = lock_agent_state(&self.state)?;
            if let Err(error) = state.conversation_mut().begin_turn(
                user.turn_id(),
                user.message_id(),
                user.message().clone(),
            ) {
                state.transition_cursor(LoopCursor::Idle)?;
                return Err(AgentError::Conversation(error));
            }
        }

        self.prepare_assistant_call(user.step_id(), user.assistant_message_id(), stream)
    }

    fn prepare_assistant_call(
        &self,
        step_id: StepId,
        assistant_message_id: MessageId,
        stream: bool,
    ) -> Result<PreparedAssistantCall, AgentError> {
        self.context.check_cancelled()?;

        let request = {
            let mut state = lock_agent_state(&self.state)?;
            state.transition_cursor(LoopCursor::streaming_step(step_id))?;
            build_chat_request(&state, self.tool_registry.as_ref(), stream)
        };

        if let Err(error) = self.context.charge_step() {
            self.abort_pending_and_idle()?;
            return Err(AgentError::RunContext(error));
        }

        let trace_node_id = match self
            .context
            .trace()
            .record_step(TraceNodeId::new(step_id.to_string()), step_id)
        {
            Ok(record) => Some(record.id().clone()),
            Err(error) => {
                self.abort_pending_and_idle()?;
                return Err(AgentError::RunContext(error.into()));
            }
        };

        Ok(PreparedAssistantCall {
            request,
            step_id,
            assistant_message_id,
            trace_node_id,
        })
    }

    async fn run_non_streaming_segment(
        &self,
        mut prepared: PreparedAssistantCall,
    ) -> Result<Vec<AgentEvent>, AgentError> {
        let mut events = Vec::new();
        let mut assistant_steps_started = 1_u32;

        loop {
            let response = self
                .client
                .chat(prepared.request.clone())
                .await
                .map_err(AgentError::Client)?;
            match self.finish_complete_response(response, &prepared)? {
                AssistantStepOutcome::Final(final_events) => {
                    events.extend(final_events);
                    return Ok(events);
                }
                AssistantStepOutcome::ToolCalls(invocations) => {
                    events.extend(self.run_tool_batch(&prepared, invocations).await?);
                    self.ensure_can_start_next_step(assistant_steps_started)?;
                    assistant_steps_started += 1;
                    prepared = self.prepare_next_assistant_call(false)?;
                }
            }
        }
    }

    async fn open_streaming_call(
        &self,
        prepared: &PreparedAssistantCall,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>, AgentError>
    {
        self.client
            .chat_stream(prepared.request.clone())
            .await
            .map_err(AgentError::Client)
    }

    fn start_streaming_assistant(&self) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        state
            .conversation_mut()
            .start_assistant()
            .map_err(AgentError::Conversation)
    }

    fn push_stream_event(&self, event: StreamEvent) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        state
            .conversation_mut()
            .push_assistant_event(event)
            .map_err(AgentError::Conversation)
    }

    fn finish_complete_response(
        &self,
        response: Response,
        prepared: &PreparedAssistantCall,
    ) -> Result<AssistantStepOutcome, AgentError> {
        {
            let mut state = lock_agent_state(&self.state)?;
            state
                .conversation_mut()
                .start_assistant_response(response)
                .map_err(AgentError::Conversation)?;
        }
        self.finish_current_assistant(prepared)
    }

    fn finish_current_assistant(
        &self,
        prepared: &PreparedAssistantCall,
    ) -> Result<AssistantStepOutcome, AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        let finish = state
            .conversation_mut()
            .finish_assistant(prepared.assistant_message_id)?;

        match finish {
            AssistantFinish::ReadyToCommit => {
                state
                    .conversation_mut()
                    .commit_pending(TurnMeta::default())?;
                let boundary = state.conversation().head();
                state.transition_cursor(LoopCursor::Idle)?;

                Ok(AssistantStepOutcome::Final(vec![
                    AgentEvent::StepBoundary(StepBoundary::new(
                        prepared.step_id,
                        boundary,
                        prepared.trace_node_id.clone(),
                    )),
                    AgentEvent::Done(AgentOutcome::Completed),
                ]))
            }
            AssistantFinish::RequiresToolCallMappings => {
                let tool_calls = extract_last_tool_calls(&state)?;
                let mut call_ids = Vec::with_capacity(tool_calls.len());
                let mut mappings = Vec::with_capacity(tool_calls.len());
                for call in &tool_calls {
                    let call_id = self.tool_ids.tool_call_id(call)?;
                    call_ids.push(call_id);
                    mappings.push(ToolCallMapping::new(call.id.clone(), call_id));
                }

                state.conversation_mut().register_tool_calls(mappings)?;

                let mut invocations = Vec::with_capacity(tool_calls.len());
                for (call_id, call) in call_ids.iter().copied().zip(tool_calls) {
                    let result_message_id = self.tool_ids.tool_result_message_id(call_id, &call)?;
                    invocations.push(ToolInvocation {
                        call_id,
                        result_message_id,
                        call,
                    });
                }

                state.transition_cursor(LoopCursor::awaiting_tool(prepared.step_id, call_ids)?)?;
                Ok(AssistantStepOutcome::ToolCalls(invocations))
            }
        }
    }

    async fn run_tool_batch(
        &self,
        prepared: &PreparedAssistantCall,
        invocations: Vec<ToolInvocation>,
    ) -> Result<Vec<AgentEvent>, AgentError> {
        if invocations.is_empty() {
            return Err(AgentError::Tool(ToolRuntimeError::InvalidRegistry {
                message: "assistant requested no tool calls after tool-use finish".to_owned(),
            }));
        }

        let max_parallel = self.max_parallel_tools()? as usize;
        let mut events = Vec::new();
        let mut results = Vec::with_capacity(invocations.len());

        for chunk in invocations.chunks(max_parallel.max(1)) {
            let mut prepared_tools = Vec::with_capacity(chunk.len());
            for invocation in chunk {
                let prepared_tool = self.prepare_tool_execution(prepared, invocation.clone())?;
                events.push(prepared_tool.started_event.clone());
                prepared_tools.push(prepared_tool);
            }

            let executed = if prepared_tools.len() == 1 {
                vec![self.execute_prepared_tool(prepared_tools.remove(0)).await]
            } else {
                join_all(
                    prepared_tools
                        .into_iter()
                        .map(|tool| self.execute_prepared_tool(tool)),
                )
                .await
            };

            for record in executed {
                let record = record?;
                events.push(record.finished_event);
                results.push((record.result_message_id, record.response));
            }
        }

        self.append_tool_results(results)?;
        Ok(events)
    }

    fn prepare_tool_execution(
        &self,
        prepared: &PreparedAssistantCall,
        invocation: ToolInvocation,
    ) -> Result<PreparedToolExecution, AgentError> {
        let trace_node_id = self.record_tool_trace(prepared, &invocation)?;
        let started_event = AgentEvent::ToolCallStarted(ToolCallStarted::new(
            prepared.step_id,
            invocation.call_id,
            invocation.call.clone(),
            trace_node_id.clone(),
        ));

        Ok(PreparedToolExecution {
            step_id: prepared.step_id,
            invocation,
            trace_node_id,
            started_event,
        })
    }

    async fn execute_prepared_tool(
        &self,
        prepared: PreparedToolExecution,
    ) -> Result<ToolExecutionRecord, AgentError> {
        self.context.check_cancelled()?;
        let invocation = prepared.invocation;
        let response = match self
            .tool_registry
            .execute(invocation.call_id, invocation.call.clone())
            .await
        {
            Ok(response) => response,
            Err(error) => match self.tool_failure_policy()? {
                ToolFailurePolicy::ReturnErrorToModel => {
                    error.to_tool_response(invocation.call.id.clone())
                }
                ToolFailurePolicy::StopRun => return Err(AgentError::Tool(error)),
            },
        };

        let finished_event = AgentEvent::ToolCallFinished(ToolCallFinished::new(
            prepared.step_id,
            invocation.call_id,
            response.clone(),
            prepared.trace_node_id,
        ));

        Ok(ToolExecutionRecord {
            result_message_id: invocation.result_message_id,
            response,
            finished_event,
        })
    }

    fn append_tool_results(
        &self,
        results: Vec<(MessageId, ToolResponse)>,
    ) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        for (message_id, response) in results {
            state
                .conversation_mut()
                .append_tool_response(message_id, response)?;
        }
        Ok(())
    }

    fn prepare_next_assistant_call(
        &self,
        stream: bool,
    ) -> Result<PreparedAssistantCall, AgentError> {
        let next_step_id = self.tool_ids.next_step_id()?;
        let next_assistant_message_id = self.tool_ids.next_assistant_message_id()?;
        self.prepare_assistant_call(next_step_id, next_assistant_message_id, stream)
    }

    fn ensure_can_start_next_step(&self, assistant_steps_started: u32) -> Result<(), AgentError> {
        let max_steps = {
            let state = lock_agent_state(&self.state)?;
            state.spec().loop_policy().max_steps().get()
        };
        if assistant_steps_started >= max_steps {
            return Err(AgentError::Other(format!(
                "agent loop step limit {max_steps} reached before final assistant response"
            )));
        }
        Ok(())
    }

    fn max_parallel_tools(&self) -> Result<u32, AgentError> {
        let state = lock_agent_state(&self.state)?;
        Ok(state.spec().loop_policy().max_parallel_tools().get())
    }

    fn tool_failure_policy(&self) -> Result<ToolFailurePolicy, AgentError> {
        let state = lock_agent_state(&self.state)?;
        Ok(state.spec().loop_policy().tool_failure_policy())
    }

    fn record_tool_trace(
        &self,
        prepared: &PreparedAssistantCall,
        invocation: &ToolInvocation,
    ) -> Result<Option<TraceNodeId>, AgentError> {
        let Some(step_trace_node_id) = prepared.trace_node_id.as_ref() else {
            return Ok(None);
        };
        let trace_node_id =
            TraceNodeId::new(format!("{}:tool:{}", prepared.step_id, invocation.call_id));
        let step_trace = self
            .context
            .trace()
            .with_parent(step_trace_node_id.clone())
            .map_err(|error| AgentError::RunContext(error.into()))?;
        let record = step_trace
            .record_tool(trace_node_id, invocation.call.name.clone())
            .map_err(|error| AgentError::RunContext(error.into()))?;
        Ok(Some(record.id().clone()))
    }

    fn abort_pending_and_idle(&self) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        abort_pending_and_idle(&mut state)
    }
}

struct StreamingSegment {
    runtime: LoopRuntime,
    source: futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>,
    current: PreparedAssistantCall,
    queued: VecDeque<Result<AgentEvent, AgentError>>,
    done: bool,
    assistant_steps_started: u32,
}

impl StreamingSegment {
    fn new(
        runtime: LoopRuntime,
        source: futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>,
        current: PreparedAssistantCall,
    ) -> Self {
        Self {
            runtime,
            source,
            current,
            queued: VecDeque::new(),
            done: false,
            assistant_steps_started: 1,
        }
    }

    async fn next_event(mut self) -> Option<(Result<AgentEvent, AgentError>, StreamingSegment)> {
        loop {
            if let Some(event) = self.queued.pop_front() {
                return Some((event, self));
            }
            if self.done {
                return None;
            }

            match self.source.next().await {
                Some(Ok(event)) => {
                    if let Err(error) = self.runtime.push_stream_event(event.clone()) {
                        return self.fail(error);
                    }
                    return Some((Ok(AgentEvent::Llm(event)), self));
                }
                Some(Err(error)) => return self.fail(AgentError::Client(error)),
                None => match self.runtime.finish_current_assistant(&self.current) {
                    Ok(AssistantStepOutcome::Final(events)) => {
                        self.queued.extend(events.into_iter().map(Ok));
                        self.done = true;
                    }
                    Ok(AssistantStepOutcome::ToolCalls(invocations)) => {
                        match self
                            .runtime
                            .run_tool_batch(&self.current, invocations)
                            .await
                        {
                            Ok(events) => self.queued.extend(events.into_iter().map(Ok)),
                            Err(error) => return self.fail(error),
                        }
                        if let Err(error) = self
                            .runtime
                            .ensure_can_start_next_step(self.assistant_steps_started)
                        {
                            return self.fail(error);
                        }
                        self.assistant_steps_started += 1;
                        let next = match self.runtime.prepare_next_assistant_call(true) {
                            Ok(next) => next,
                            Err(error) => return self.fail(error),
                        };
                        let source = match self.runtime.open_streaming_call(&next).await {
                            Ok(source) => source,
                            Err(error) => return self.fail(error),
                        };
                        if let Err(error) = self.runtime.start_streaming_assistant() {
                            return self.fail(error);
                        }
                        self.current = next;
                        self.source = source;
                    }
                    Err(error) => return self.fail(error),
                },
            }
        }
    }

    fn fail(mut self, error: AgentError) -> Option<(Result<AgentEvent, AgentError>, Self)> {
        self.done = true;
        let error = match self.runtime.abort_pending_and_idle() {
            Ok(()) => error,
            Err(cleanup_error) => cleanup_error,
        };
        Some((Err(error), self))
    }
}

#[derive(Clone)]
struct PreparedAssistantCall {
    request: ChatRequest,
    step_id: StepId,
    assistant_message_id: MessageId,
    trace_node_id: Option<TraceNodeId>,
}

enum AssistantStepOutcome {
    Final(Vec<AgentEvent>),
    ToolCalls(Vec<ToolInvocation>),
}

#[derive(Clone)]
struct ToolInvocation {
    call_id: ToolCallId,
    result_message_id: MessageId,
    call: ToolCall,
}

struct PreparedToolExecution {
    step_id: StepId,
    invocation: ToolInvocation,
    trace_node_id: Option<TraceNodeId>,
    started_event: AgentEvent,
}

struct ToolExecutionRecord {
    result_message_id: MessageId,
    response: ToolResponse,
    finished_event: AgentEvent,
}

fn build_chat_request(
    state: &AgentState,
    tool_registry: &dyn ToolRegistry,
    stream: bool,
) -> ChatRequest {
    let effective = state.conversation().effective_view();
    let (system, mut messages) = effective.into_parts();
    if let Some(pending) = state.conversation().pending_context() {
        messages.extend(pending.into_messages());
    }
    let model = state.spec().model();

    ChatRequest {
        model: model.model().to_owned(),
        messages,
        tools: tool_registry.declarations(),
        system: system.or_else(|| state.spec().system_prompt().map(ToOwned::to_owned)),
        max_tokens: model.max_tokens().get(),
        temperature: model.temperature(),
        stream,
        provider_extras: model.provider_extras().cloned(),
    }
}

fn extract_last_tool_calls(state: &AgentState) -> Result<Vec<ToolCall>, AgentError> {
    let pending = state
        .conversation()
        .pending()
        .ok_or_else(|| AgentError::Other("tool-use finish left no pending turn".to_owned()))?;
    let message = pending
        .messages()
        .last()
        .ok_or_else(|| AgentError::Other("tool-use finish left no assistant message".to_owned()))?;

    let calls = message
        .payload()
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            ContentBlock::Text { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Thinking { .. } => None,
        })
        .collect::<Vec<_>>();

    if calls.is_empty() {
        return Err(AgentError::Other(
            "assistant finish required tool mappings but no tool-use blocks were found".to_owned(),
        ));
    }
    Ok(calls)
}

fn abort_pending_and_idle(state: &mut AgentState) -> Result<(), AgentError> {
    if state.conversation().pending().is_some() {
        state
            .conversation_mut()
            .cancel_pending(CancelDisposition::DiscardTurn)?;
    }

    match state.loop_cursor() {
        LoopCursor::StreamingStep(_) | LoopCursor::CancelRecovery(_) => {
            state.transition_cursor(LoopCursor::Idle)?;
        }
        LoopCursor::AwaitingTool(_) | LoopCursor::AwaitingApproval(_) => {
            state.transition_cursor(LoopCursor::cancel_recovery(
                None,
                CancelRecoveryReason::ToolInterrupted,
            ))?;
            state.transition_cursor(LoopCursor::Idle)?;
        }
        LoopCursor::Idle | LoopCursor::Done(_) | LoopCursor::Error(_) => {}
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
