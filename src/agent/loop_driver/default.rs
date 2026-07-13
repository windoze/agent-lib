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
        AgentError, AgentEvent, AgentInput, AgentOutcome, AgentState, ApprovalDecision,
        ApprovalError, ApprovalRequest, ApprovalRequirement, ApprovalResponse,
        DeclaredOnlyToolRegistry, DeclaredOnlyToolRegistryResolver, LoopCursor, NoApprovalPolicy,
        NoToolExecutionIds, PivotMessage, ReconfigRequest, RunContext, StaticToolRegistryResolver,
        StepBoundary, StepId, ToolApprovalPolicy, ToolCallFinished, ToolCallStarted,
        ToolExecutionIds, ToolFailurePolicy, ToolRegistry, ToolRegistryResolver, ToolRuntimeError,
        TraceNodeId,
        state::{CancelRecoveryReason, PivotSource, QueuedPivot, ReconfigApplication},
    },
    client::{ChatRequest, ClientError, LlmClient, Response},
    conversation::{
        AssistantFinish, CancelDisposition, CancelledToolResult, MessageId, MessageMeta,
        ToolCallId, ToolCallMapping, TurnId, TurnMeta,
    },
    model::{
        content::ContentBlock,
        message::Message,
        tool::{ToolCall, ToolResponse, ToolStatus},
    },
    stream::StreamEvent,
};
use async_trait::async_trait;
use futures::{StreamExt, future::join_all, stream};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{sync::oneshot, time::sleep};

type SharedAgentState = Arc<Mutex<AgentState>>;
type SharedToolRegistry = Arc<dyn ToolRegistry>;
type SharedToolRegistrySlot = Arc<Mutex<SharedToolRegistry>>;
type SharedToolRegistryResolver = Arc<dyn ToolRegistryResolver>;
type SharedToolExecutionIds = Arc<dyn ToolExecutionIds>;
type SharedApprovalPolicy = Arc<dyn ToolApprovalPolicy>;
type SharedApprovalWaiters = Arc<Mutex<ApprovalWaiters>>;

const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// LLM transport mode used by [`DefaultAgentLoop`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmStepMode {
    /// Use [`LlmClient::chat`] and fold a complete response into Conversation.
    NonStreaming,
    /// Use [`LlmClient::chat_stream`] and emit each [`StreamEvent`] as it arrives.
    Streaming,
}

impl LlmStepMode {
    pub(crate) const fn request_stream_flag(self) -> bool {
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
        let resolver: SharedToolRegistryResolver = Arc::new(DeclaredOnlyToolRegistryResolver);
        Self::with_tool_registry_resolver(
            client,
            state,
            context,
            mode,
            Arc::new(DeclaredOnlyToolRegistry::new(declarations)),
            Arc::new(NoToolExecutionIds),
            resolver,
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
        let resolver: SharedToolRegistryResolver = Arc::new(StaticToolRegistryResolver::single(
            state.current_tool_set().id(),
            Arc::clone(&tool_registry),
        ));
        Self::with_tool_registry_resolver(
            client,
            state,
            context,
            mode,
            tool_registry,
            tool_ids,
            resolver,
        )
    }

    /// Creates a loop driver with a registry and a resolver for future tool
    /// set replacements.
    #[must_use]
    pub fn with_tool_registry_resolver(
        client: Arc<dyn LlmClient>,
        state: AgentState,
        context: RunContext,
        mode: LlmStepMode,
        tool_registry: SharedToolRegistry,
        tool_ids: SharedToolExecutionIds,
        tool_registry_resolver: SharedToolRegistryResolver,
    ) -> Self {
        Self {
            runtime: LoopRuntime {
                client,
                state: Arc::new(Mutex::new(state)),
                context,
                tool_registry: Arc::new(Mutex::new(tool_registry)),
                tool_registry_resolver,
                tool_ids,
                approval_policy: Arc::new(NoApprovalPolicy),
                approval_waiters: Arc::new(Mutex::new(ApprovalWaiters::default())),
            },
            mode,
            guard: AgentFeedGuard::new(),
        }
    }

    /// Returns a loop driver that pauses tool execution according to `policy`.
    ///
    /// The approval policy is a live runtime object. Pending responder handles
    /// remain in the loop runtime and are addressed with
    /// [`ApprovalResponse`] through [`AgentLoop::respond_approval`].
    #[must_use]
    pub fn with_approval_policy(mut self, policy: SharedApprovalPolicy) -> Self {
        self.runtime.approval_policy = policy;
        self
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
        Ok(stream::unfold(
            NonStreamingSegment::new(self.runtime.clone(), prepared),
            NonStreamingSegment::next_event,
        )
        .boxed())
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

    fn reconfigure(&self, request: ReconfigRequest) -> Result<(), AgentError> {
        self.runtime.queue_reconfig(request)
    }

    fn respond_approval(&self, response: ApprovalResponse) -> Result<(), AgentError> {
        self.runtime.submit_approval_response(response)
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
    tool_registry: SharedToolRegistrySlot,
    tool_registry_resolver: SharedToolRegistryResolver,
    tool_ids: SharedToolExecutionIds,
    approval_policy: SharedApprovalPolicy,
    approval_waiters: SharedApprovalWaiters,
}

impl LoopRuntime {
    fn queue_reconfig(&self, request: ReconfigRequest) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        let application = state.plan_reconfig_with(&request)?;
        self.resolve_reconfig_registry(&state, &application)?;
        state.queue_prevalidated_reconfig(request);
        Ok(())
    }

    fn apply_queued_reconfigs_before_turn(&self, state: &mut AgentState) -> Result<(), AgentError> {
        let Some(application) = state.queued_reconfig_application()? else {
            return Ok(());
        };
        let replacement = self.resolve_reconfig_registry(state, &application)?;
        state.apply_reconfig_application(application);
        if let Some(registry) = replacement {
            self.replace_tool_registry(registry)?;
        }
        Ok(())
    }

    fn prepare_queued_reconfig_application(
        &self,
        state: &AgentState,
    ) -> Result<Option<PreparedReconfigApplication>, AgentError> {
        let Some(application) = state.queued_reconfig_application()? else {
            return Ok(None);
        };
        let registry = self.resolve_reconfig_registry(state, &application)?;
        Ok(Some(PreparedReconfigApplication {
            records: reconfig_records(application.requests()),
            application,
            registry,
        }))
    }

    fn resolve_reconfig_registry(
        &self,
        state: &AgentState,
        application: &ReconfigApplication,
    ) -> Result<Option<SharedToolRegistry>, AgentError> {
        if application.current_tool_set() == state.current_tool_set() {
            return Ok(None);
        }

        let registry = self
            .tool_registry_resolver
            .resolve_tool_set(application.current_tool_set())?;
        let declarations = registry.declarations();
        if declarations != application.current_tool_set().tools() {
            return Err(AgentError::Tool(ToolRuntimeError::InvalidRegistry {
                message: format!(
                    "registry declarations for tool set {} do not match requested ToolSetRef",
                    application.current_tool_set().id()
                ),
            }));
        }
        Ok(Some(registry))
    }

    fn replace_tool_registry(&self, registry: SharedToolRegistry) -> Result<(), AgentError> {
        let mut active = self
            .tool_registry
            .lock()
            .map_err(|_| AgentError::Other("tool registry mutex poisoned".to_owned()))?;
        *active = registry;
        Ok(())
    }

    fn active_tool_registry(&self) -> Result<SharedToolRegistry, AgentError> {
        self.tool_registry
            .lock()
            .map(|registry| Arc::clone(&registry))
            .map_err(|_| AgentError::Other("tool registry mutex poisoned".to_owned()))
    }

    fn submit_approval_response(&self, response: ApprovalResponse) -> Result<(), AgentError> {
        let mut waiters = self
            .approval_waiters
            .lock()
            .map_err(|_| AgentError::Other("approval waiter mutex poisoned".to_owned()))?;
        waiters.respond(response)
    }

    fn register_approval_waiter(
        &self,
        request: &ApprovalRequest,
    ) -> Result<oneshot::Receiver<ApprovalResponse>, AgentError> {
        let (sender, receiver) = oneshot::channel();
        let mut waiters = self
            .approval_waiters
            .lock()
            .map_err(|_| AgentError::Other("approval waiter mutex poisoned".to_owned()))?;
        waiters.insert(request.step_id(), request.call_id(), sender)?;
        Ok(receiver)
    }

    fn clear_approval_waiter(
        &self,
        step_id: StepId,
        call_id: ToolCallId,
    ) -> Result<(), AgentError> {
        let mut waiters = self
            .approval_waiters
            .lock()
            .map_err(|_| AgentError::Other("approval waiter mutex poisoned".to_owned()))?;
        waiters.remove(step_id, call_id);
        Ok(())
    }

    fn approval_requirement(&self, call_id: ToolCallId, call: &ToolCall) -> ApprovalRequirement {
        self.approval_policy.approval_requirement(call_id, call)
    }

    fn prepare_user_turn(
        &self,
        input: AgentInput,
        stream: bool,
    ) -> Result<PreparedAssistantCall, AgentError> {
        #[allow(deprecated)]
        let initial = match input {
            AgentInput::UserMessage(user) => {
                let state = lock_agent_state(&self.state)?;
                if !state.queued_pivots().is_empty() {
                    return Err(AgentError::QueuedPivotPending);
                }
                InitialUserTurn {
                    turn_id: user.turn_id(),
                    message_id: user.message_id(),
                    message: user.message().clone(),
                    assistant_message_id: user.assistant_message_id(),
                    step_id: user.step_id(),
                    queued_pivot: false,
                }
            }
            AgentInput::QueuedPivotTurn(input) => {
                let state = lock_agent_state(&self.state)?;
                let pivot = state
                    .queued_pivots()
                    .first()
                    .cloned()
                    .ok_or(AgentError::NoQueuedPivot)?;
                InitialUserTurn {
                    turn_id: input.turn_id(),
                    message_id: pivot.message_id(),
                    message: pivot.message().clone(),
                    assistant_message_id: input.assistant_message_id(),
                    step_id: input.step_id(),
                    queued_pivot: true,
                }
            }
            AgentInput::Resume(input) => {
                {
                    let state = lock_agent_state(&self.state)?;
                    if state.conversation().pending_context().is_none() {
                        return Err(AgentError::Other(
                            "resume feed input requires a pending conversation context".to_owned(),
                        ));
                    }
                }
                let assistant_message_id = self.tool_ids.next_assistant_message_id()?;
                return self.prepare_assistant_call(input.step_id(), assistant_message_id, stream);
            }
            AgentInput::Pivot(_) => {
                return Err(AgentError::Other(
                    "the legacy default loop does not support direct pivot injection; \
                     queue the pivot and feed AgentInput::QueuedPivotTurn instead"
                        .to_owned(),
                ));
            }
        };

        {
            let mut state = lock_agent_state(&self.state)?;
            self.apply_queued_reconfigs_before_turn(&mut state)?;
            if let Err(error) = state.conversation_mut().begin_turn(
                initial.turn_id,
                initial.message_id,
                initial.message.clone(),
            ) {
                state.transition_cursor(LoopCursor::Idle)?;
                return Err(AgentError::Conversation(error));
            }
            if initial.queued_pivot {
                let removed = state
                    .dequeue_pivot()
                    .expect("queued pivot was peeked before begin_turn");
                debug_assert_eq!(removed.message_id(), initial.message_id);
            }
        }

        self.prepare_assistant_call(initial.step_id, initial.assistant_message_id, stream)
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
            state.transition_cursor(LoopCursor::streaming_step(step_id, None))?;
            let tool_registry = self.active_tool_registry()?;
            crate::agent::request::build_chat_request(&state, tool_registry.declarations(), stream)
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

    async fn chat_with_cancel(&self, request: ChatRequest) -> Result<Response, AgentError> {
        let mut call = Box::pin(self.client.chat(request));
        loop {
            tokio::select! {
                response = &mut call => return response.map_err(AgentError::Client),
                () = sleep(CANCEL_POLL_INTERVAL) => self.context.check_cancelled()?,
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

    async fn next_stream_event(
        &self,
        source: &mut futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>,
    ) -> Result<Option<StreamEvent>, AgentError> {
        loop {
            tokio::select! {
                event = source.next() => {
                    return event.transpose().map_err(AgentError::Client);
                }
                () = sleep(CANCEL_POLL_INTERVAL) => self.context.check_cancelled()?,
            }
        }
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
                let prepared_reconfig = self.prepare_queued_reconfig_application(&state)?;
                state
                    .conversation_mut()
                    .commit_pending(TurnMeta::default())?;
                let boundary = state.conversation().head();
                let mut metadata = deferred_pivot_metadata(&state);
                if let Some(prepared_reconfig) = prepared_reconfig {
                    state.apply_reconfig_application(prepared_reconfig.application);
                    if let Some(registry) = prepared_reconfig.registry {
                        self.replace_tool_registry(registry)?;
                    }
                    merge_metadata(&mut metadata, reconfig_metadata(prepared_reconfig.records));
                }
                state.transition_cursor(LoopCursor::Idle)?;

                Ok(AssistantStepOutcome::Final(vec![
                    AgentEvent::StepBoundary(StepBoundary::with_metadata(
                        prepared.step_id,
                        boundary,
                        prepared.trace_node_id.clone(),
                        metadata,
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

                state.transition_cursor(LoopCursor::awaiting_tool(
                    prepared.step_id,
                    call_ids,
                    None,
                )?)?;
                Ok(AssistantStepOutcome::ToolCalls(invocations))
            }
        }
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
        let invocation = prepared.invocation;
        let tool_registry = self.active_tool_registry()?;
        let mut execution =
            Box::pin(tool_registry.execute(invocation.call_id, invocation.call.clone()));
        let response = loop {
            tokio::select! {
                result = &mut execution => {
                    break match result {
                        Ok(response) => response,
                        Err(error) => match self.tool_failure_policy()? {
                            ToolFailurePolicy::ReturnErrorToModel => {
                                error.to_tool_response(invocation.call.id.clone())
                            }
                            ToolFailurePolicy::StopRun => return Err(AgentError::Tool(error)),
                        },
                    };
                },
                () = sleep(CANCEL_POLL_INTERVAL) => self.context.check_cancelled()?,
            }
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

    fn append_tool_result(
        &self,
        message_id: MessageId,
        response: ToolResponse,
    ) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        state
            .conversation_mut()
            .append_tool_response(message_id, response)?;
        Ok(())
    }

    fn restore_awaiting_tool_cursor(
        &self,
        step_id: StepId,
        call_ids: Vec<ToolCallId>,
    ) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        state.transition_cursor(LoopCursor::awaiting_tool(step_id, call_ids, None)?)?;
        Ok(())
    }

    fn apply_pivots_at_pending_step_boundary(
        &self,
        prepared: &PreparedAssistantCall,
    ) -> Result<AgentEvent, AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        let boundary = state.conversation().head();
        let mut records = Vec::new();

        while let Some(pivot) = state.dequeue_pivot() {
            let record = match state.conversation_mut().inject_user_message(
                boundary,
                pivot.message_id(),
                pivot.message().clone(),
                pivot_message_meta(&pivot),
            ) {
                Ok(()) => pivot_record(&pivot, "applied", "pending_turn", None),
                Err(error) => {
                    pivot_record(&pivot, "rejected", "pending_turn", Some(error.to_string()))
                }
            };
            records.push(record);
        }

        Ok(AgentEvent::StepBoundary(StepBoundary::with_metadata(
            prepared.step_id,
            boundary,
            prepared.trace_node_id.clone(),
            pivot_metadata(records),
        )))
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
            state.current_loop_policy().max_steps().get()
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
        Ok(state.current_loop_policy().max_parallel_tools().get())
    }

    fn tool_failure_policy(&self) -> Result<ToolFailurePolicy, AgentError> {
        let state = lock_agent_state(&self.state)?;
        Ok(state.current_loop_policy().tool_failure_policy())
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

    fn cancel_pending_discard_and_done(
        &self,
        step_id: StepId,
        reason: CancelRecoveryReason,
    ) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        if state.conversation().pending().is_some() {
            state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn)?;
        }
        state.transition_cursor(LoopCursor::cancel_recovery(Some(step_id), reason))?;
        state.transition_cursor(LoopCursor::Idle)?;
        Ok(())
    }

    fn cancel_pending_resume_and_done(
        &self,
        step_id: StepId,
        cancelled_results: Vec<CancelledToolResult>,
    ) -> Result<(), AgentError> {
        let mut state = lock_agent_state(&self.state)?;
        state
            .conversation_mut()
            .cancel_pending(CancelDisposition::ResumeTurn { cancelled_results })?;
        state.transition_cursor(LoopCursor::cancel_recovery(
            Some(step_id),
            CancelRecoveryReason::Cancelled,
        ))?;
        state.transition_cursor(LoopCursor::Idle)?;
        Ok(())
    }
}

#[derive(Default)]
struct ApprovalWaiters {
    pending: BTreeMap<(StepId, ToolCallId), oneshot::Sender<ApprovalResponse>>,
}

impl ApprovalWaiters {
    fn insert(
        &mut self,
        step_id: StepId,
        call_id: ToolCallId,
        sender: oneshot::Sender<ApprovalResponse>,
    ) -> Result<(), AgentError> {
        if self.pending.insert((step_id, call_id), sender).is_some() {
            return Err(ApprovalError::DuplicatePending { call_id }.into());
        }
        Ok(())
    }

    fn respond(&mut self, response: ApprovalResponse) -> Result<(), AgentError> {
        let step_id = response.step_id();
        let call_id = response.call_id();
        let sender = self
            .pending
            .remove(&(step_id, call_id))
            .ok_or(ApprovalError::NoPending { step_id, call_id })?;
        sender
            .send(response)
            .map_err(|_| ApprovalError::ResponderClosed { call_id }.into())
    }

    fn remove(&mut self, step_id: StepId, call_id: ToolCallId) {
        self.pending.remove(&(step_id, call_id));
    }
}

struct ToolBatchSegment {
    runtime: LoopRuntime,
    prepared: PreparedAssistantCall,
    pending: VecDeque<ToolInvocation>,
    queued: VecDeque<AgentEvent>,
    waiting: Option<PendingApproval>,
    boundary_emitted: bool,
}

impl ToolBatchSegment {
    fn new(
        runtime: LoopRuntime,
        prepared: PreparedAssistantCall,
        invocations: Vec<ToolInvocation>,
    ) -> Result<Self, AgentError> {
        if invocations.is_empty() {
            return Err(AgentError::Tool(ToolRuntimeError::InvalidRegistry {
                message: "assistant requested no tool calls after tool-use finish".to_owned(),
            }));
        }

        Ok(Self {
            runtime,
            prepared,
            pending: VecDeque::from(invocations),
            queued: VecDeque::new(),
            waiting: None,
            boundary_emitted: false,
        })
    }

    async fn next_event(&mut self) -> Result<ToolBatchPoll, AgentError> {
        loop {
            if let Some(event) = self.queued.pop_front() {
                return Ok(ToolBatchPoll::Event(event));
            }

            if self.boundary_emitted {
                return Ok(ToolBatchPoll::Complete);
            }

            if self.waiting.is_some() {
                match self.resolve_pending_approval().await? {
                    ToolBatchPoll::Event(event) => return Ok(ToolBatchPoll::Event(event)),
                    ToolBatchPoll::Cancelled => return Ok(ToolBatchPoll::Cancelled),
                    ToolBatchPoll::Complete => continue,
                }
            }

            if self.pending.is_empty() {
                let boundary = self
                    .runtime
                    .apply_pivots_at_pending_step_boundary(&self.prepared)?;
                self.queued.push_back(boundary);
                self.boundary_emitted = true;
                continue;
            }

            if let Some(poll) = self.process_next_ready_tools().await? {
                return Ok(poll);
            }
        }
    }

    async fn process_next_ready_tools(&mut self) -> Result<Option<ToolBatchPoll>, AgentError> {
        let first = self
            .pending
            .front()
            .expect("pending is checked before processing");
        if let ApprovalRequirement::RequireApproval { reason } = self
            .runtime
            .approval_requirement(first.call_id, &first.call)
        {
            let invocation = self.pending.pop_front().expect("pending tool exists");
            let prepared_tool = self
                .runtime
                .prepare_tool_execution(&self.prepared, invocation)?;
            let request = ApprovalRequest::with_reason(
                prepared_tool.step_id,
                prepared_tool.invocation.call_id,
                prepared_tool.invocation.call.clone(),
                reason,
                prepared_tool.trace_node_id.clone(),
            );
            let receiver = self.runtime.register_approval_waiter(&request)?;
            {
                let mut state = lock_agent_state(&self.runtime.state)?;
                state.transition_cursor(LoopCursor::awaiting_approval(
                    prepared_tool.step_id,
                    prepared_tool.invocation.call_id,
                    None,
                ))?;
            }
            self.waiting = Some(PendingApproval {
                prepared_tool,
                receiver,
            });
            self.queued.push_back(AgentEvent::AwaitingApproval(request));
            return Ok(None);
        }

        let max_parallel = self.runtime.max_parallel_tools()? as usize;
        let mut prepared_tools = Vec::new();
        while prepared_tools.len() < max_parallel.max(1) {
            let Some(next) = self.pending.front() else {
                break;
            };
            if matches!(
                self.runtime.approval_requirement(next.call_id, &next.call),
                ApprovalRequirement::RequireApproval { .. }
            ) {
                break;
            }
            let invocation = self.pending.pop_front().expect("pending tool exists");
            let prepared_tool = self
                .runtime
                .prepare_tool_execution(&self.prepared, invocation)?;
            self.queued.push_back(prepared_tool.started_event.clone());
            prepared_tools.push(prepared_tool);
        }

        if prepared_tools.is_empty() {
            return Ok(None);
        }

        let cancelled_results = self.cancelled_results_for(
            prepared_tools
                .iter()
                .map(|tool| &tool.invocation)
                .chain(self.pending.iter()),
        );
        let executed = if prepared_tools.len() == 1 {
            vec![
                self.runtime
                    .execute_prepared_tool(prepared_tools.remove(0))
                    .await,
            ]
        } else {
            join_all(
                prepared_tools
                    .into_iter()
                    .map(|tool| self.runtime.execute_prepared_tool(tool)),
            )
            .await
        };

        for record in &executed {
            if let Err(error) = record {
                if error.kind() == crate::agent::AgentErrorKind::Cancelled {
                    self.runtime
                        .cancel_pending_resume_and_done(self.prepared.step_id, cancelled_results)?;
                    return Ok(Some(ToolBatchPoll::Cancelled));
                }
                return Err(error.clone());
            }
        }

        for record in executed {
            let record = record.expect("errors were handled before appending tool results");
            self.runtime
                .append_tool_result(record.result_message_id, record.response.clone())?;
            self.queued.push_back(record.finished_event);
        }

        Ok(None)
    }

    async fn resolve_pending_approval(&mut self) -> Result<ToolBatchPoll, AgentError> {
        let mut pending = self
            .waiting
            .take()
            .expect("waiting approval is checked before resolving");
        let response = match wait_for_approval(
            &self.runtime,
            pending.prepared_tool.step_id,
            pending.prepared_tool.invocation.call_id,
            &mut pending.receiver,
        )
        .await
        {
            Ok(response) => response,
            Err(error) if error.kind() == crate::agent::AgentErrorKind::Cancelled => {
                let cancelled_results = self.cancelled_results_for(
                    std::iter::once(&pending.prepared_tool.invocation).chain(self.pending.iter()),
                );
                self.runtime
                    .cancel_pending_resume_and_done(self.prepared.step_id, cancelled_results)?;
                return Ok(ToolBatchPoll::Cancelled);
            }
            Err(error) => return Err(error),
        };

        self.runtime.restore_awaiting_tool_cursor(
            pending.prepared_tool.step_id,
            vec![pending.prepared_tool.invocation.call_id],
        )?;

        match response.decision() {
            ApprovalDecision::Approve => {
                let cancelled_results = self.cancelled_results_for(
                    std::iter::once(&pending.prepared_tool.invocation).chain(self.pending.iter()),
                );
                self.queued
                    .push_back(pending.prepared_tool.started_event.clone());
                match self
                    .runtime
                    .execute_prepared_tool(pending.prepared_tool)
                    .await
                {
                    Ok(record) => {
                        self.runtime.append_tool_result(
                            record.result_message_id,
                            record.response.clone(),
                        )?;
                        self.queued.push_back(record.finished_event);
                        Ok(ToolBatchPoll::Complete)
                    }
                    Err(error) if error.kind() == crate::agent::AgentErrorKind::Cancelled => {
                        self.runtime.cancel_pending_resume_and_done(
                            self.prepared.step_id,
                            cancelled_results,
                        )?;
                        Ok(ToolBatchPoll::Cancelled)
                    }
                    Err(error) => Err(error),
                }
            }
            ApprovalDecision::Deny | ApprovalDecision::Timeout | ApprovalDecision::Cancel => {
                let response = approval_response_for_decision(
                    &pending.prepared_tool.invocation.call,
                    response.decision(),
                    response.message(),
                );
                let result_message_id = pending.prepared_tool.invocation.result_message_id;
                self.runtime
                    .append_tool_result(result_message_id, response.clone())?;
                Ok(ToolBatchPoll::Event(AgentEvent::ToolCallFinished(
                    ToolCallFinished::new(
                        pending.prepared_tool.step_id,
                        pending.prepared_tool.invocation.call_id,
                        response,
                        pending.prepared_tool.trace_node_id,
                    ),
                )))
            }
        }
    }

    fn cancelled_results_for<'a>(
        &self,
        invocations: impl Iterator<Item = &'a ToolInvocation>,
    ) -> Vec<CancelledToolResult> {
        invocations
            .map(|invocation| {
                CancelledToolResult::new(
                    invocation.call.id.clone(),
                    invocation.call_id,
                    invocation.result_message_id,
                )
            })
            .collect()
    }
}

enum ToolBatchPoll {
    Event(AgentEvent),
    Complete,
    Cancelled,
}

struct PendingApproval {
    prepared_tool: PreparedToolExecution,
    receiver: oneshot::Receiver<ApprovalResponse>,
}

struct NonStreamingSegment {
    runtime: LoopRuntime,
    current: PreparedAssistantCall,
    queued: VecDeque<Result<AgentEvent, AgentError>>,
    done: bool,
    assistant_steps_started: u32,
    tool_batch: Option<ToolBatchSegment>,
}

impl NonStreamingSegment {
    fn new(runtime: LoopRuntime, current: PreparedAssistantCall) -> Self {
        Self {
            runtime,
            current,
            queued: VecDeque::new(),
            done: false,
            assistant_steps_started: 1,
            tool_batch: None,
        }
    }

    async fn next_event(mut self) -> Option<(Result<AgentEvent, AgentError>, NonStreamingSegment)> {
        loop {
            if let Some(event) = self.queued.pop_front() {
                return Some((event, self));
            }
            if self.done {
                return None;
            }

            if let Some(batch) = self.tool_batch.as_mut() {
                match batch.next_event().await {
                    Ok(ToolBatchPoll::Event(event)) => return Some((Ok(event), self)),
                    Ok(ToolBatchPoll::Complete) => {
                        self.tool_batch = None;
                        if let Err(error) = self
                            .runtime
                            .ensure_can_start_next_step(self.assistant_steps_started)
                        {
                            return self.fail(error);
                        }
                        self.assistant_steps_started += 1;
                        self.current = match self.runtime.prepare_next_assistant_call(false) {
                            Ok(next) => next,
                            Err(error) => return self.fail(error),
                        };
                    }
                    Ok(ToolBatchPoll::Cancelled) => return self.cancelled(),
                    Err(error) => return self.fail(error),
                }
                continue;
            }

            let response = match self
                .runtime
                .chat_with_cancel(self.current.request.clone())
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    return if error.kind() == crate::agent::AgentErrorKind::Cancelled {
                        self.cancel_active_llm()
                    } else {
                        self.fail(error)
                    };
                }
            };

            match self
                .runtime
                .finish_complete_response(response, &self.current)
            {
                Ok(AssistantStepOutcome::Final(events)) => {
                    self.queued.extend(events.into_iter().map(Ok));
                    self.done = true;
                }
                Ok(AssistantStepOutcome::ToolCalls(invocations)) => {
                    self.tool_batch = match ToolBatchSegment::new(
                        self.runtime.clone(),
                        self.current.clone(),
                        invocations,
                    ) {
                        Ok(batch) => Some(batch),
                        Err(error) => return self.fail(error),
                    };
                }
                Err(error) => return self.fail(error),
            }
        }
    }

    fn cancel_active_llm(self) -> Option<(Result<AgentEvent, AgentError>, Self)> {
        match self.runtime.cancel_pending_discard_and_done(
            self.current.step_id,
            CancelRecoveryReason::LlmInterrupted,
        ) {
            Ok(()) => self.cancelled(),
            Err(error) => self.fail(error),
        }
    }

    fn cancelled(mut self) -> Option<(Result<AgentEvent, AgentError>, Self)> {
        self.done = true;
        Some((Ok(AgentEvent::Done(AgentOutcome::Cancelled)), self))
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

struct StreamingSegment {
    runtime: LoopRuntime,
    source: futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>,
    current: PreparedAssistantCall,
    queued: VecDeque<Result<AgentEvent, AgentError>>,
    done: bool,
    assistant_steps_started: u32,
    tool_batch: Option<ToolBatchSegment>,
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
            tool_batch: None,
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

            if let Some(batch) = self.tool_batch.as_mut() {
                match batch.next_event().await {
                    Ok(ToolBatchPoll::Event(event)) => return Some((Ok(event), self)),
                    Ok(ToolBatchPoll::Complete) => {
                        self.tool_batch = None;
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
                    Ok(ToolBatchPoll::Cancelled) => return self.cancelled(),
                    Err(error) => return self.fail(error),
                }
                continue;
            }

            match self.runtime.next_stream_event(&mut self.source).await {
                Ok(Some(event)) => {
                    if let Err(error) = self.runtime.push_stream_event(event.clone()) {
                        return self.fail(error);
                    }
                    return Some((Ok(AgentEvent::Llm(event)), self));
                }
                Ok(None) => match self.runtime.finish_current_assistant(&self.current) {
                    Ok(AssistantStepOutcome::Final(events)) => {
                        self.queued.extend(events.into_iter().map(Ok));
                        self.done = true;
                    }
                    Ok(AssistantStepOutcome::ToolCalls(invocations)) => {
                        self.tool_batch = match ToolBatchSegment::new(
                            self.runtime.clone(),
                            self.current.clone(),
                            invocations,
                        ) {
                            Ok(batch) => Some(batch),
                            Err(error) => return self.fail(error),
                        };
                    }
                    Err(error) => return self.fail(error),
                },
                Err(error) if error.kind() == crate::agent::AgentErrorKind::Cancelled => {
                    return self.cancel_active_llm();
                }
                Err(error) => return self.fail(error),
            }
        }
    }

    fn cancel_active_llm(self) -> Option<(Result<AgentEvent, AgentError>, Self)> {
        match self.runtime.cancel_pending_discard_and_done(
            self.current.step_id,
            CancelRecoveryReason::LlmInterrupted,
        ) {
            Ok(()) => self.cancelled(),
            Err(error) => self.fail(error),
        }
    }

    fn cancelled(mut self) -> Option<(Result<AgentEvent, AgentError>, Self)> {
        self.done = true;
        Some((Ok(AgentEvent::Done(AgentOutcome::Cancelled)), self))
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

struct InitialUserTurn {
    turn_id: TurnId,
    message_id: MessageId,
    message: Message,
    assistant_message_id: MessageId,
    step_id: StepId,
    queued_pivot: bool,
}

struct PreparedReconfigApplication {
    application: ReconfigApplication,
    registry: Option<SharedToolRegistry>,
    records: Vec<Value>,
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

async fn wait_for_approval(
    runtime: &LoopRuntime,
    step_id: StepId,
    call_id: ToolCallId,
    receiver: &mut oneshot::Receiver<ApprovalResponse>,
) -> Result<ApprovalResponse, AgentError> {
    loop {
        tokio::select! {
            response = &mut *receiver => {
                return response.map_err(|_| ApprovalError::ResponderClosed { call_id }.into());
            }
            () = sleep(CANCEL_POLL_INTERVAL) => {
                if runtime.context.is_cancelled() {
                    runtime.clear_approval_waiter(step_id, call_id)?;
                    return Err(AgentError::RunContext(crate::agent::RunContextError::Cancelled));
                }
            }
        }
    }
}

fn approval_response_for_decision(
    call: &ToolCall,
    decision: ApprovalDecision,
    message: Option<&str>,
) -> ToolResponse {
    let (status, default_text) = match decision {
        ApprovalDecision::Approve => unreachable!("approve executes the tool"),
        ApprovalDecision::Deny => (
            ToolStatus::Denied,
            "Tool execution was denied before it started.",
        ),
        ApprovalDecision::Timeout => (
            ToolStatus::Denied,
            "Tool execution approval timed out before the tool started.",
        ),
        ApprovalDecision::Cancel => (
            ToolStatus::Cancelled,
            "Tool execution was cancelled before it started.",
        ),
    };

    ToolResponse {
        tool_call_id: call.id.clone(),
        content: vec![ContentBlock::Text {
            text: message.unwrap_or(default_text).to_owned(),
            extra: Map::new(),
        }],
        status,
        extra: Map::new(),
    }
}

fn deferred_pivot_metadata(state: &AgentState) -> Map<String, Value> {
    pivot_metadata(
        state
            .queued_pivots()
            .iter()
            .map(|pivot| pivot_record(pivot, "deferred", "next_turn", None))
            .collect(),
    )
}

fn pivot_metadata(records: Vec<Value>) -> Map<String, Value> {
    let mut metadata = Map::new();
    if !records.is_empty() {
        metadata.insert("pivots".to_owned(), Value::Array(records));
    }
    metadata
}

fn reconfig_metadata(records: Vec<Value>) -> Map<String, Value> {
    let mut metadata = Map::new();
    if !records.is_empty() {
        metadata.insert("reconfigs".to_owned(), Value::Array(records));
    }
    metadata
}

fn merge_metadata(target: &mut Map<String, Value>, source: Map<String, Value>) {
    for (key, value) in source {
        target.insert(key, value);
    }
}

fn pivot_record(
    pivot: &QueuedPivot,
    status: &'static str,
    target: &'static str,
    error: Option<String>,
) -> Value {
    let mut record = Map::new();
    record.insert("status".to_owned(), Value::String(status.to_owned()));
    record.insert("target".to_owned(), Value::String(target.to_owned()));
    record.insert(
        "message_id".to_owned(),
        serde_json::to_value(pivot.message_id()).expect("message id serializes"),
    );
    record.insert(
        "source".to_owned(),
        serde_json::to_value(pivot.source()).expect("pivot source serializes"),
    );
    if let Some(error) = error {
        record.insert("error".to_owned(), Value::String(error));
    }
    Value::Object(record)
}

fn reconfig_records(requests: &[ReconfigRequest]) -> Vec<Value> {
    requests
        .iter()
        .map(|request| reconfig_record(request, "applied"))
        .collect()
}

fn reconfig_record(request: &ReconfigRequest, status: &'static str) -> Value {
    let mut record = Map::new();
    record.insert("status".to_owned(), Value::String(status.to_owned()));
    match request {
        ReconfigRequest::ActivateSkill { skill_id } => {
            record.insert(
                "kind".to_owned(),
                Value::String("activate_skill".to_owned()),
            );
            record.insert(
                "skill_id".to_owned(),
                serde_json::to_value(skill_id).expect("skill id serializes"),
            );
        }
        ReconfigRequest::DeactivateSkill { skill_id } => {
            record.insert(
                "kind".to_owned(),
                Value::String("deactivate_skill".to_owned()),
            );
            record.insert(
                "skill_id".to_owned(),
                serde_json::to_value(skill_id).expect("skill id serializes"),
            );
        }
        ReconfigRequest::ReplaceActiveSkills { skill_ids } => {
            record.insert(
                "kind".to_owned(),
                Value::String("replace_active_skills".to_owned()),
            );
            record.insert(
                "skill_ids".to_owned(),
                serde_json::to_value(skill_ids).expect("skill ids serialize"),
            );
        }
        ReconfigRequest::SetSystemPromptOverlay {
            expected_version, ..
        } => {
            record.insert(
                "kind".to_owned(),
                Value::String("set_system_prompt_overlay".to_owned()),
            );
            record.insert(
                "expected_version".to_owned(),
                Value::from(*expected_version),
            );
        }
        ReconfigRequest::ReplaceToolSet { tool_set } => {
            record.insert(
                "kind".to_owned(),
                Value::String("replace_tool_set".to_owned()),
            );
            record.insert(
                "tool_set_id".to_owned(),
                serde_json::to_value(tool_set.id()).expect("tool set id serializes"),
            );
        }
        ReconfigRequest::PatchToolSet { patch } => {
            record.insert(
                "kind".to_owned(),
                Value::String("patch_tool_set".to_owned()),
            );
            record.insert(
                "tool_set_id".to_owned(),
                serde_json::to_value(patch.resulting_tool_set_id())
                    .expect("tool set id serializes"),
            );
        }
        ReconfigRequest::SetModel { model } => {
            record.insert("kind".to_owned(), Value::String("set_model".to_owned()));
            record.insert("model".to_owned(), Value::String(model.model().to_owned()));
        }
        ReconfigRequest::SetLoopPolicy { .. } => {
            record.insert(
                "kind".to_owned(),
                Value::String("set_loop_policy".to_owned()),
            );
        }
    }
    Value::Object(record)
}

fn pivot_message_meta(pivot: &QueuedPivot) -> MessageMeta {
    let mut extra = Map::new();
    extra.insert(
        "pivot_source".to_owned(),
        serde_json::to_value(pivot.source()).expect("pivot source serializes"),
    );
    MessageMeta::new(Some(pivot_source_label(pivot.source())), extra)
}

fn pivot_source_label(source: &PivotSource) -> String {
    match source {
        PivotSource::Human => "pivot:human".to_owned(),
        PivotSource::Coordinator { agent_id } => format!("pivot:coordinator:{agent_id}"),
        PivotSource::Skill { skill_id } => format!("pivot:skill:{skill_id}"),
        PivotSource::Host { label } => format!("pivot:host:{label}"),
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
