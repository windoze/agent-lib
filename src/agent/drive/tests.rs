use super::{
    ExternalSessionHandler, HandlerScope, InteractionHandler, LlmHandler, Pop, ReconfigHandler,
    ScopePop, SubagentHandler, ToolHandler, drain, fulfill_batch, fulfill_with_scope,
    scope_handles,
};
use crate::{
    agent::{
        AgentError, AgentErrorKind, AgentId, AgentInput, AgentMachine, AgentSpec, AgentState,
        ApprovalDecision, ApprovalRequirement, ApprovalResponse, BudgetLimits, DefaultAgentMachine,
        LlmStepMode, LoopCursor, LoopCursorKind, LoopDoneReason, LoopPolicy, ModelRef, Requirement,
        RequirementDisposition, RequirementError, RequirementId, RequirementIds, RunContext, RunId,
        StepInput, StepOutcome, SubagentOutput, ToolApprovalPolicy, ToolFailurePolicy, ToolSetId,
        ToolSetRef, TraceNodeId, TraceNodeKind,
        external::{
            ExternalAgentOutput, ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionInput,
            ExternalSessionPolicy, ExternalSessionRef, ExternalSessionRequest,
            ExternalSessionResult, ExternalStreamPolicy, WorktreeIsolation,
        },
        interaction::{Interaction, InteractionKind, InteractionResponse},
        requirement::{AgentSpecRef, RequirementKind, RequirementKindTag, RequirementResult},
        spec::WorktreeRef,
        tool::{ToolRegistry, ToolRuntimeError},
    },
    client::{Capability, ChatRequest, ClientError, LlmClient, Response},
    conversation::{
        Conversation, ConversationConfig, ConversationId, MessageId, ToolCallId, TurnId,
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        tool::{Tool, ToolCall, ToolResponse, ToolStatus},
    },
    stream::{
        StreamEvent,
        accumulator::{CollectError, collect},
    },
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn nz(value: u32) -> std::num::NonZeroU32 {
    std::num::NonZeroU32::new(value).expect("non-zero fixture value")
}

fn run_context() -> RunContext {
    run_context_with_budget(BudgetLimits::default())
}

fn run_context_with_budget(limits: BudgetLimits) -> RunContext {
    let run_id: RunId = "018f0d9c-7b6a-7c12-8f31-1234567890a1"
        .parse()
        .expect("run id");
    RunContext::new_root(run_id, limits, TraceNodeId::new("root"))
}

fn step_id() -> crate::agent::StepId {
    "018f0d9c-7b6a-7c12-8f31-1234567890e9"
        .parse()
        .expect("step id")
}

fn tool_call_id() -> ToolCallId {
    "018f0d9c-7b6a-7c12-8f31-1234567890c1"
        .parse()
        .expect("tool call id")
}

fn chat_request() -> ChatRequest {
    ChatRequest {
        model: "test-model".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
        system: None,
        max_tokens: 16,
        temperature: None,
        stream: false,
        provider_extras: None,
    }
}

fn tool_call() -> ToolCall {
    ToolCall {
        id: "call-weather".to_owned(),
        name: "get_weather".to_owned(),
        input: json!({ "city": "Shanghai" }),
        extra: Map::new(),
    }
}

fn response() -> Response {
    serde_json::from_value(json!({
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": "hi" }]
        },
        "usage": { "input": 1, "output": 1 },
        "stop_reason": { "value": "end_turn", "raw": "end_turn" }
    }))
    .expect("response")
}

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890d1"
        .parse()
        .expect("agent id")
}

fn conversation_id() -> ConversationId {
    "018f0d9c-7b6a-7c12-8f31-1234567890d2"
        .parse()
        .expect("conversation id")
}

fn tool_set_id() -> ToolSetId {
    "018f0d9c-7b6a-7c12-8f31-1234567890d3"
        .parse()
        .expect("tool set id")
}

#[derive(Debug)]
struct FixedRequirementIds(RequirementId);

impl RequirementIds for FixedRequirementIds {
    fn next_requirement_id(
        &self,
        _kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        Ok(self.0)
    }
}

fn default_machine() -> DefaultAgentMachine {
    let spec = AgentSpec::new(
        agent_id(),
        WorktreeRef::new("/repo/agent-lib"),
        None,
        ToolSetRef::new(tool_set_id(), Vec::new()),
        ModelRef::new("gpt-5.5", nz(512), Some(0.1), None),
        LoopPolicy::new(nz(8), nz(1), ToolFailurePolicy::ReturnErrorToModel),
    );
    let state = AgentState::new(
        spec,
        Conversation::new(conversation_id(), ConversationConfig::default()),
    );
    DefaultAgentMachine::new(
        state,
        LlmStepMode::NonStreaming,
        Arc::new(FixedRequirementIds(requirement_id_n(8))),
    )
}

fn external_session_request() -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent_id(),
        runtime: ExternalRuntimeKind::ClaudeCode,
        worktree: WorktreeRef::new("/repo/agent-lib"),
        session_dir: None,
        session: None,
        input: ExternalSessionInput::Start {
            prompt: "Refactor the parser.".to_owned(),
        },
        tools: Vec::new(),
        policy: ExternalSessionPolicy {
            permission_mode: ExternalPermissionMode::Prompt,
            isolation: WorktreeIsolation::Shared,
            max_turns: Some(8),
            stream_events: ExternalStreamPolicy::Buffered,
        },
    }
}

fn external_session_result() -> ExternalSessionResult {
    ExternalSessionResult::Completed {
        session: ExternalSessionRef {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: Some("sess-1".to_owned()),
            transcript_ref: None,
            resume_token: None,
            last_event_seq: Some(3),
        },
        output: ExternalAgentOutput {
            summary: "refactor complete".to_owned(),
            artifacts: Vec::new(),
            usage: None,
            cost_micros: None,
        },
        observations: Vec::new(),
    }
}

/// Minimal [`LlmClient`] that always returns a fixed complete response.
#[derive(Debug)]
struct FakeClient;

#[async_trait]
impl LlmClient for FakeClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
        Ok(response())
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        Err(ClientError::Other(
            "streaming not used in fixture".to_owned(),
        ))
    }
}

/// Minimal [`ToolRegistry`] that echoes an `Ok` response for any call.
#[derive(Debug)]
struct FakeRegistry;

#[async_trait]
impl ToolRegistry for FakeRegistry {
    fn declarations(&self) -> Vec<Tool> {
        Vec::new()
    }

    async fn execute(
        &self,
        _call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        Ok(ToolResponse {
            tool_call_id: call.id,
            content: Vec::new(),
            status: ToolStatus::Ok,
            extra: Map::new(),
        })
    }
}

/// Wraps an [`LlmClient`] into an [`LlmHandler`].
struct LlmClientHandler {
    client: Arc<dyn LlmClient>,
}

#[async_trait]
impl LlmHandler for LlmClientHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let mut request = request.clone();
        let result = match mode {
            LlmStepMode::NonStreaming => {
                request.stream = false;
                self.client.chat(request).await
            }
            LlmStepMode::Streaming => {
                request.stream = true;
                match self.client.chat_stream(request).await {
                    Ok(stream) => collect(stream).await.map_err(|error| match error {
                        CollectError::Stream(err) => err,
                        CollectError::Accumulator(err) => ClientError::Protocol(err.to_string()),
                    }),
                    Err(err) => Err(err),
                }
            }
        };
        RequirementResult::Llm(result)
    }
}

/// Wraps a [`ToolRegistry`] into a [`ToolHandler`].
#[derive(Debug)]
struct ToolRegistryHandler {
    registry: Arc<dyn ToolRegistry>,
}

#[async_trait]
impl ToolHandler for ToolRegistryHandler {
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::Tool(self.registry.execute(call_id, call.clone()).await)
    }
}

/// Wraps a [`ToolApprovalPolicy`] into an unattended [`InteractionHandler`].
#[derive(Debug)]
struct PolicyInteractionHandler {
    policy: Arc<dyn ToolApprovalPolicy>,
    call: ToolCall,
}

#[async_trait]
impl InteractionHandler for PolicyInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        let response = match request.kind() {
            InteractionKind::Approval { call_id, .. } => {
                let decision = match self.policy.approval_requirement(*call_id, &self.call) {
                    ApprovalRequirement::AutoApprove => ApprovalDecision::Approve,
                    ApprovalRequirement::RequireApproval { .. } => ApprovalDecision::Deny,
                };
                InteractionResponse::Approval(ApprovalResponse::new(
                    request.step_id(),
                    *call_id,
                    decision,
                    None,
                ))
            }
            InteractionKind::Question { .. } => InteractionResponse::answer("ok".to_owned()),
            InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
            InteractionKind::Permission { .. } => {
                panic!("test interactions are approvals, never permissions")
            }
        };
        RequirementResult::Interaction(response)
    }
}

/// Scope with no overrides: every accessor keeps the `None` default.
struct EmptyScope;

impl HandlerScope for EmptyScope {}

/// Scope wiring the three implemented handler families (no subagent yet).
struct WrappedScope {
    llm: LlmClientHandler,
    tool: ToolRegistryHandler,
    interaction: PolicyInteractionHandler,
}

impl HandlerScope for WrappedScope {
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

fn wrapped_scope() -> WrappedScope {
    WrappedScope {
        llm: LlmClientHandler {
            client: Arc::new(FakeClient),
        },
        tool: ToolRegistryHandler {
            registry: Arc::new(FakeRegistry),
        },
        interaction: PolicyInteractionHandler {
            policy: Arc::new(crate::agent::NoApprovalPolicy),
            call: tool_call(),
        },
    }
}

#[test]
fn empty_scope_handles_no_requirement_family() {
    let scope = EmptyScope;
    assert!(scope.llm().is_none());
    assert!(scope.tool().is_none());
    assert!(scope.interaction().is_none());
    assert!(scope.subagent().is_none());
}

#[test]
fn wrapped_scope_exposes_implemented_families_only() {
    let scope = wrapped_scope();
    assert!(scope.llm().is_some());
    assert!(scope.tool().is_some());
    assert!(scope.interaction().is_some());
    // SubagentHandler stays unimplemented until M5.
    assert!(scope.subagent().is_none());
}

#[tokio::test]
async fn llm_handler_result_is_accepted_by_its_requirement() {
    let scope = wrapped_scope();
    let ctx = run_context();
    let request = chat_request();
    let mode = LlmStepMode::NonStreaming;

    let result = scope
        .llm()
        .expect("llm handler")
        .fulfill(&request, mode, &ctx)
        .await;

    assert!(matches!(result, RequirementResult::Llm(Ok(_))));
    let kind = RequirementKind::NeedLlm { request, mode };
    kind.accepts(&result).expect("llm result aligns with kind");
}

#[tokio::test]
async fn tool_handler_result_is_accepted_by_its_requirement() {
    let scope = wrapped_scope();
    let ctx = run_context();
    let call = tool_call();
    let call_id = tool_call_id();

    let result = scope
        .tool()
        .expect("tool handler")
        .fulfill(call_id, &call, &ctx)
        .await;

    assert!(matches!(result, RequirementResult::Tool(Ok(_))));
    let kind = RequirementKind::NeedTool { call_id, call };
    kind.accepts(&result).expect("tool result aligns with kind");
}

#[tokio::test]
async fn interaction_handler_result_is_accepted_by_its_requirement() {
    let scope = wrapped_scope();
    let ctx = run_context();
    let request =
        Interaction::approval(step_id(), tool_call_id(), ApprovalRequirement::AutoApprove);

    let result = scope
        .interaction()
        .expect("interaction handler")
        .fulfill(&request, &ctx)
        .await;

    assert!(matches!(result, RequirementResult::Interaction(_)));
    let kind = RequirementKind::NeedInteraction { request };
    kind.accepts(&result)
        .expect("interaction result aligns with kind");
}

// ----- M3-2: drain + pop routing fixtures and tests -----

fn requirement_id_n(n: u8) -> RequirementId {
    RequirementId::parse_str(&format!("018f0d9c-7b6a-7c12-8f31-1234567890{n:02x}"))
        .expect("requirement id")
}

fn tool_call_id_n(n: u8) -> ToolCallId {
    format!("018f0d9c-7b6a-7c12-8f31-123456789{n:03x}")
        .parse()
        .expect("tool call id")
}

fn ok_tool_response(call: &ToolCall) -> ToolResponse {
    ToolResponse {
        tool_call_id: call.id.clone(),
        content: Vec::new(),
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

/// A `NeedTool` requirement carrying an optional `delay` (yield count) so a
/// concurrent batch can be forced to complete out of emission order.
fn tool_requirement(n: u8, delay: u64) -> Requirement {
    Requirement::at_root(
        requirement_id_n(n),
        RequirementKind::NeedTool {
            call_id: tool_call_id_n(n),
            call: ToolCall {
                id: format!("call-{n}"),
                name: "get_weather".to_owned(),
                input: json!({ "delay": delay }),
                extra: Map::new(),
            },
        },
    )
}

fn llm_requirement(n: u8) -> Requirement {
    Requirement::at_root(
        requirement_id_n(n),
        RequirementKind::NeedLlm {
            request: chat_request(),
            mode: LlmStepMode::NonStreaming,
        },
    )
}

fn interaction_requirement(n: u8) -> Requirement {
    Requirement::at_root(
        requirement_id_n(n),
        RequirementKind::NeedInteraction {
            request: Interaction::approval(
                step_id(),
                tool_call_id_n(n),
                ApprovalRequirement::AutoApprove,
            ),
        },
    )
}

fn external_requirement(n: u8) -> Requirement {
    Requirement::at_root(
        requirement_id_n(n),
        RequirementKind::NeedExternalSession {
            request: external_session_request(),
        },
    )
}

/// A minimal machine that emits a fixed requirement batch on the external
/// input, then completes once every requirement in the batch is resumed.
///
/// It routes results by id (so an out-of-order batch resume is fine) and
/// records the resume order for assertions.
struct BatchMachine {
    cursor: LoopCursor,
    batch: Vec<Requirement>,
    outstanding: BTreeSet<RequirementId>,
    resume_order: Vec<RequirementId>,
    resume_tags: Vec<RequirementKindTag>,
}

impl BatchMachine {
    fn new(batch: Vec<Requirement>) -> Self {
        Self {
            cursor: LoopCursor::default(),
            batch,
            outstanding: BTreeSet::new(),
            resume_order: Vec::new(),
            resume_tags: Vec::new(),
        }
    }
}

impl AgentMachine for BatchMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        match input {
            StepInput::External(_) => {
                self.outstanding = self
                    .batch
                    .iter()
                    .map(|requirement| requirement.id)
                    .collect();
                self.cursor = LoopCursor::streaming_step(step_id(), None);
                StepOutcome::new(Vec::new(), self.batch.clone(), true)
            }
            StepInput::Resume(resolution) => {
                self.resume_order.push(resolution.id);
                self.resume_tags.push(resolution.result.tag());
                self.outstanding.remove(&resolution.id);
                if self.outstanding.is_empty() {
                    self.cursor = LoopCursor::done(LoopDoneReason::Completed);
                }
                StepOutcome::new(Vec::new(), Vec::new(), true)
            }
            StepInput::Abandon(_) => StepOutcome::default(),
        }
    }

    fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }

    fn interrupt_budget_exhausted(&mut self) -> StepOutcome {
        self.outstanding.clear();
        self.cursor = LoopCursor::done(LoopDoneReason::BudgetExhausted);
        StepOutcome::new(Vec::new(), Vec::new(), true)
    }
}

fn external_input() -> AgentInput {
    let turn_id: TurnId = "018f0d9c-7b6a-7c12-8f31-1234567890f2"
        .parse()
        .expect("turn id");
    let message_id: MessageId = "018f0d9c-7b6a-7c12-8f31-1234567890f3"
        .parse()
        .expect("message id");
    let assistant_message_id: MessageId = "018f0d9c-7b6a-7c12-8f31-1234567890f6"
        .parse()
        .expect("assistant message id");
    let message = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "hello".to_owned(),
            extra: Map::new(),
        }],
    };
    AgentInput::user_message(
        turn_id,
        message_id,
        message,
        assistant_message_id,
        step_id(),
    )
    .expect("user input")
}

/// Counts fulfillments and echoes an `Ok` tool response.
#[derive(Clone, Default)]
struct CountingToolHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolHandler for CountingToolHandler {
    async fn fulfill(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        RequirementResult::Tool(Ok(ok_tool_response(call)))
    }
}

/// Counts fulfillments and approves any approval interaction.
#[derive(Clone, Default)]
struct CountingInteractionHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl InteractionHandler for CountingInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = match request.kind() {
            InteractionKind::Approval { call_id, .. } => InteractionResponse::Approval(
                ApprovalResponse::new(request.step_id(), *call_id, ApprovalDecision::Approve, None),
            ),
            InteractionKind::Question { .. } => InteractionResponse::answer("ok".to_owned()),
            InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
            InteractionKind::Permission { .. } => {
                panic!("test interactions are approvals, never permissions")
            }
        };
        RequirementResult::Interaction(response)
    }
}

/// Counts fulfillments and returns a fixed `Completed` external result.
#[derive(Clone, Default)]
struct CountingExternalSessionHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ExternalSessionHandler for CountingExternalSessionHandler {
    async fn fulfill(
        &self,
        _request: &ExternalSessionRequest,
        _ctx: &RunContext,
    ) -> RequirementResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        RequirementResult::ExternalSession(Box::new(external_session_result()))
    }
}

#[tokio::test]
async fn external_session_handler_default_cleanup_agent_is_a_no_op() {
    // A handler that owns no live runtime state needs no override: the
    // default sweep reports nothing and touches nothing (M3-2).
    let handler = CountingExternalSessionHandler::default();
    let agent_id: AgentId = "018f0d9c-7b6a-7c12-8f31-1234567890f0"
        .parse()
        .expect("agent id");
    assert!(handler.cleanup_agent(agent_id).await.is_empty());
}

/// Records the completion order of a concurrent tool batch, delaying each
/// fulfillment by the `delay` yield count carried in the tool call input.
#[derive(Clone, Default)]
struct DelayToolHandler {
    completed: Arc<std::sync::Mutex<Vec<ToolCallId>>>,
}

#[async_trait]
impl ToolHandler for DelayToolHandler {
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let delay = call.input.get("delay").and_then(Value::as_u64).unwrap_or(0);
        for _ in 0..delay {
            tokio::task::yield_now().await;
        }
        self.completed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(call_id);
        RequirementResult::Tool(Ok(ok_tool_response(call)))
    }
}

/// A flexible scope whose handlers are wired à la carte per test.
#[derive(Default)]
struct TestScope {
    tool: Option<CountingToolHandler>,
    interaction: Option<CountingInteractionHandler>,
    delay_tool: Option<DelayToolHandler>,
    cancelling_tool: Option<CancellingToolHandler>,
    blocking_tool: Option<BlockingToolHandler>,
    blocking_interaction: Option<BlockingInteractionHandler>,
    external: Option<CountingExternalSessionHandler>,
}

impl HandlerScope for TestScope {
    fn tool(&self) -> Option<&dyn ToolHandler> {
        if let Some(handler) = self.blocking_tool.as_ref() {
            return Some(handler as &dyn ToolHandler);
        }
        if let Some(handler) = self.delay_tool.as_ref() {
            return Some(handler as &dyn ToolHandler);
        }
        if let Some(handler) = self.cancelling_tool.as_ref() {
            return Some(handler as &dyn ToolHandler);
        }
        self.tool
            .as_ref()
            .map(|handler| handler as &dyn ToolHandler)
    }

    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        if let Some(handler) = self.blocking_interaction.as_ref() {
            return Some(handler as &dyn InteractionHandler);
        }
        self.interaction
            .as_ref()
            .map(|handler| handler as &dyn InteractionHandler)
    }

    fn external(&self) -> Option<&dyn ExternalSessionHandler> {
        self.external
            .as_ref()
            .map(|handler| handler as &dyn ExternalSessionHandler)
    }
}

#[tokio::test]
async fn drain_fulfills_locally_without_popping() {
    let tool = CountingToolHandler::default();
    let scope = TestScope {
        tool: Some(tool.clone()),
        ..TestScope::default()
    };
    let mut machine = BatchMachine::new(vec![tool_requirement(1, 0), tool_requirement(2, 0)]);
    let ctx = run_context();

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("drain completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    assert_eq!(tool.calls.load(Ordering::SeqCst), 2);
    assert_eq!(machine.resume_order.len(), 2);
    assert!(
        machine
            .resume_tags
            .iter()
            .all(|tag| *tag == RequirementKindTag::Tool)
    );
}

#[tokio::test]
async fn drain_pops_to_parent_when_local_scope_lacks_handler() {
    // Inner layer handles tools only; the outer layer handles interaction.
    let inner_tool = CountingToolHandler::default();
    let inner = TestScope {
        tool: Some(inner_tool.clone()),
        ..TestScope::default()
    };
    let outer_interaction = CountingInteractionHandler::default();
    let outer = TestScope {
        interaction: Some(outer_interaction.clone()),
        ..TestScope::default()
    };
    let mut parent = ScopePop::new(&outer, None);
    let mut machine = BatchMachine::new(vec![interaction_requirement(3)]);
    let ctx = run_context();

    let done = drain(
        &mut machine,
        external_input(),
        &inner,
        Some(&mut parent),
        &ctx,
    )
    .await
    .expect("drain completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    // The interaction popped to the outer layer; the inner tool was untouched.
    assert_eq!(outer_interaction.calls.load(Ordering::SeqCst), 1);
    assert_eq!(inner_tool.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn drain_top_scope_without_handler_is_unhandled_requirement() {
    let scope = TestScope::default();
    let mut machine = BatchMachine::new(vec![interaction_requirement(4)]);
    let ctx = run_context();

    let error = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect_err("top scope cannot fulfill the interaction");

    assert_eq!(error.kind(), AgentErrorKind::UnhandledRequirement);
    match error {
        AgentError::UnhandledRequirement { kind, origin } => {
            assert_eq!(kind, RequirementKindTag::Interaction);
            assert!(origin.is_root());
        }
        other => panic!("expected UnhandledRequirement, got {other:?}"),
    }
}

#[tokio::test]
async fn pop_starts_from_outer_scope_skipping_the_emitter() {
    // §7.3: a requirement the emitting (inner) layer cannot fulfill pops to
    // the outer layer; it is resolved there and never re-enters the inner
    // scope. Modeled as a headless inner drain (no interaction handler)
    // whose interaction request is served by the attended outer layer.
    let inner_tool = CountingToolHandler::default();
    let inner = TestScope {
        tool: Some(inner_tool.clone()),
        ..TestScope::default()
    };
    let outer_interaction = CountingInteractionHandler::default();
    let outer_tool = CountingToolHandler::default();
    let outer = TestScope {
        tool: Some(outer_tool.clone()),
        interaction: Some(outer_interaction.clone()),
        ..TestScope::default()
    };
    let mut parent = ScopePop::new(&outer, None);
    let mut machine = BatchMachine::new(vec![interaction_requirement(5)]);
    let ctx = run_context();

    let done = drain(
        &mut machine,
        external_input(),
        &inner,
        Some(&mut parent),
        &ctx,
    )
    .await
    .expect("drain completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    // Resolved once, by the outer interaction handler.
    assert_eq!(outer_interaction.calls.load(Ordering::SeqCst), 1);
    // Neither the inner nor the outer tool handler was reached: the popped
    // interaction did not loop back through any tool handler.
    assert_eq!(inner_tool.calls.load(Ordering::SeqCst), 0);
    assert_eq!(outer_tool.calls.load(Ordering::SeqCst), 0);
}

// ----- M2-3: external-session handler dispatch -----

#[tokio::test]
async fn external_session_handler_result_is_accepted_by_its_requirement() {
    let scope = TestScope {
        external: Some(CountingExternalSessionHandler::default()),
        ..TestScope::default()
    };
    let ctx = run_context();
    let request = external_session_request();

    let result = scope
        .external()
        .expect("external handler")
        .fulfill(&request, &ctx)
        .await;

    assert!(matches!(
        result,
        RequirementResult::ExternalSession(ref boxed)
            if matches!(**boxed, ExternalSessionResult::Completed { .. })
    ));
    let kind = RequirementKind::NeedExternalSession { request };
    kind.accepts(&result)
        .expect("external result aligns with kind");
}

#[tokio::test]
async fn external_session_handler_drain_fulfills_locally() {
    let external = CountingExternalSessionHandler::default();
    let scope = TestScope {
        external: Some(external.clone()),
        ..TestScope::default()
    };
    let mut machine = BatchMachine::new(vec![external_requirement(1)]);
    let ctx = run_context();

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("drain completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    assert_eq!(external.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        machine.resume_tags,
        vec![RequirementKindTag::ExternalSession]
    );
}

#[tokio::test]
async fn external_session_handler_default_scope_pops_to_outer() {
    // The inner layer offers no external handler; the requirement pops to the
    // outer layer that does — resolved there, never re-entering the inner
    // scope (the inner tool handler stays untouched).
    let inner_tool = CountingToolHandler::default();
    let inner = TestScope {
        tool: Some(inner_tool.clone()),
        ..TestScope::default()
    };
    let outer_external = CountingExternalSessionHandler::default();
    let outer = TestScope {
        external: Some(outer_external.clone()),
        ..TestScope::default()
    };
    let mut parent = ScopePop::new(&outer, None);
    let mut machine = BatchMachine::new(vec![external_requirement(2)]);
    let ctx = run_context();

    let done = drain(
        &mut machine,
        external_input(),
        &inner,
        Some(&mut parent),
        &ctx,
    )
    .await
    .expect("drain completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    assert_eq!(outer_external.calls.load(Ordering::SeqCst), 1);
    assert_eq!(inner_tool.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn drain_resolves_a_concurrent_batch_out_of_order() {
    let delay_tool = DelayToolHandler::default();
    let scope = TestScope {
        delay_tool: Some(delay_tool.clone()),
        ..TestScope::default()
    };
    // Emission order is [1, 2, 3]; delays force completion order [3, 2, 1].
    let batch = vec![
        tool_requirement(1, 2),
        tool_requirement(2, 1),
        tool_requirement(3, 0),
    ];
    let mut machine = BatchMachine::new(batch);
    let ctx = run_context();

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("drain completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));

    // Every requirement was fulfilled exactly once, regardless of order.
    let completed = delay_tool
        .completed
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let completed_set: BTreeSet<ToolCallId> = completed.iter().copied().collect();
    let expected_set: BTreeSet<ToolCallId> =
        [tool_call_id_n(1), tool_call_id_n(2), tool_call_id_n(3)]
            .into_iter()
            .collect();
    assert_eq!(completed_set, expected_set);

    // The batch was fulfilled concurrently and completed out of emission
    // order, and the machine resumed each result in completion order.
    assert_eq!(
        *completed,
        vec![tool_call_id_n(3), tool_call_id_n(2), tool_call_id_n(1)]
    );
    assert_eq!(
        machine.resume_order,
        vec![
            requirement_id_n(3),
            requirement_id_n(2),
            requirement_id_n(1)
        ]
    );

    // Terminal state is reached regardless of the reordering.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
}

#[tokio::test]
async fn drain_charges_steps_and_usage_for_successful_llm_responses() {
    let scope = wrapped_scope();
    let mut machine = BatchMachine::new(vec![llm_requirement(5)]);
    let ctx = run_context_with_budget(BudgetLimits::new(Some(2), Some(10), None, None));

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("budgeted drain completes");

    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    assert_eq!(machine.resume_order, vec![requirement_id_n(5)]);
    let budget = ctx.budget().snapshot();
    assert_eq!(budget.used().steps(), 1);
    assert_eq!(budget.used().tokens(), 2);
}

#[tokio::test]
async fn drain_stops_on_usage_budget_exhaustion_without_resuming_the_response() {
    let scope = wrapped_scope();
    let mut machine = BatchMachine::new(vec![llm_requirement(6)]);
    let ctx = run_context_with_budget(BudgetLimits::new(Some(4), Some(1), None, None));

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("budget exhaustion is a terminal drain outcome");

    match done.cursor() {
        LoopCursor::Done(done) => assert_eq!(done.reason(), LoopDoneReason::BudgetExhausted),
        other => panic!("expected budget exhausted terminal cursor, got {other:?}"),
    }
    assert!(!done.cancelled(), "budget exhaustion is not cancellation");
    assert!(machine.resume_order.is_empty());

    let budget = ctx.budget().snapshot();
    assert_eq!(budget.used().steps(), 1);
    assert_eq!(
        budget.used().tokens(),
        0,
        "the over-limit usage charge is rejected atomically"
    );
    assert_eq!(
        requirement_trace(&ctx, requirement_id_n(6), RequirementKindTag::Llm),
        (0, RequirementDisposition::NeverResumed)
    );
}

#[tokio::test]
async fn drain_budget_exhaustion_discards_the_default_machine_uncommitted_turn() {
    let scope = wrapped_scope();
    let mut machine = default_machine();
    let ctx = run_context_with_budget(BudgetLimits::new(Some(4), Some(1), None, None));

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("budget exhaustion is a terminal drain outcome");

    match done.cursor() {
        LoopCursor::Done(done) => assert_eq!(done.reason(), LoopDoneReason::BudgetExhausted),
        other => panic!("expected budget exhausted terminal cursor, got {other:?}"),
    }
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(
        conversation.turns().len(),
        0,
        "the over-budget response never commits the user turn"
    );
}

// ----- M5-3: trace records resolved-by-scope and disposition -----

/// Extracts the `(resolved_at_scope, disposition)` of the trace node whose id
/// is the string form of `id`, asserting it is a `Requirement` node whose
/// `kind_tag` matches `expected_tag`.
fn requirement_trace(
    ctx: &RunContext,
    id: RequirementId,
    expected_tag: RequirementKindTag,
) -> (u32, RequirementDisposition) {
    let records = ctx.trace().records();
    let record = records
        .iter()
        .find(|record| record.id().as_str() == id.to_string())
        .expect("a trace node was recorded for the requirement");
    match record.kind() {
        TraceNodeKind::Requirement {
            kind_tag,
            resolved_at_scope,
            disposition,
        } => {
            assert_eq!(kind_tag, expected_tag);
            (resolved_at_scope, disposition)
        }
        other => panic!("expected a requirement trace node, got {other:?}"),
    }
}

#[tokio::test]
async fn drain_records_resolved_at_scope_for_local_and_popped_requirements() {
    // Inner layer handles tools locally; interaction must pop to the outer
    // layer. One batch exercises both a hop-0 (local) and a hop-1 (popped)
    // resolution.
    let inner_tool = CountingToolHandler::default();
    let inner = TestScope {
        tool: Some(inner_tool.clone()),
        ..TestScope::default()
    };
    let outer_interaction = CountingInteractionHandler::default();
    let outer = TestScope {
        interaction: Some(outer_interaction.clone()),
        ..TestScope::default()
    };
    let mut parent = ScopePop::new(&outer, None);
    let mut machine = BatchMachine::new(vec![tool_requirement(1, 0), interaction_requirement(2)]);
    let ctx = run_context();

    let done = drain(
        &mut machine,
        external_input(),
        &inner,
        Some(&mut parent),
        &ctx,
    )
    .await
    .expect("drain completes");
    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    assert!(!done.cancelled(), "a natural end is not marked cancelled");

    // The tool was settled in place by the emitting (inner) scope: hop 0.
    assert_eq!(
        requirement_trace(&ctx, requirement_id_n(1), RequirementKindTag::Tool),
        (0, RequirementDisposition::Resumed)
    );
    // The interaction popped one layer out to the attended parent: hop 1.
    assert_eq!(
        requirement_trace(&ctx, requirement_id_n(2), RequirementKindTag::Interaction),
        (1, RequirementDisposition::Resumed)
    );
}

#[tokio::test]
async fn drain_records_never_resumed_disposition_on_cancel() {
    let tool = CountingToolHandler::default();
    let scope = TestScope {
        tool: Some(tool.clone()),
        ..TestScope::default()
    };
    let mut machine = BatchMachine::new(vec![tool_requirement(7, 0)]);
    let ctx = run_context();
    // A cancelled context abandons the batch's first requirement instead of
    // fulfilling it: a never-resume that must still be traced.
    ctx.cancellation().cancel();

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("cancelled drain closes the turn");

    // The cancel outcome is distinguishable from a natural end (M4-5).
    assert!(done.cancelled());
    // The requirement was never fulfilled by the handler.
    assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
    // The never-resume is recorded, settled at the performing layer (hop 0).
    assert_eq!(
        requirement_trace(&ctx, requirement_id_n(7), RequirementKindTag::Tool),
        (0, RequirementDisposition::NeverResumed)
    );
}

/// Fulfills a tool call successfully but cancels the run context while the
/// batch is in flight, so the driver's post-batch re-check observes it.
#[derive(Clone, Default)]
struct CancellingToolHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolHandler for CancellingToolHandler {
    async fn fulfill(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
        ctx: &RunContext,
    ) -> RequirementResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ctx.cancellation().cancel();
        RequirementResult::Tool(Ok(ok_tool_response(call)))
    }
}

#[tokio::test]
async fn drain_records_never_resumed_for_every_outstanding_requirement_on_cancel() {
    // A batch of three differently-kinded requirements, all locally
    // fulfillable: cancelling must settle *every* one on the trace, not
    // just the first (M4-5).
    let tool = CountingToolHandler::default();
    let interaction = CountingInteractionHandler::default();
    let external = CountingExternalSessionHandler::default();
    let scope = TestScope {
        tool: Some(tool.clone()),
        interaction: Some(interaction.clone()),
        external: Some(external.clone()),
        ..TestScope::default()
    };
    let mut machine = BatchMachine::new(vec![
        tool_requirement(1, 0),
        interaction_requirement(2),
        external_requirement(3),
    ]);
    let ctx = run_context();
    ctx.cancellation().cancel();

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("cancelled drain closes the turn");

    assert!(done.cancelled());
    // No handler ever ran.
    assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
    assert_eq!(interaction.calls.load(Ordering::SeqCst), 0);
    assert_eq!(external.calls.load(Ordering::SeqCst), 0);
    // Every outstanding requirement has its own never-resumed trace node.
    for (n, tag) in [
        (1, RequirementKindTag::Tool),
        (2, RequirementKindTag::Interaction),
        (3, RequirementKindTag::ExternalSession),
    ] {
        assert_eq!(
            requirement_trace(&ctx, requirement_id_n(n), tag),
            (0, RequirementDisposition::NeverResumed)
        );
    }
}

#[tokio::test]
async fn drain_rechecks_cancellation_after_fulfill_and_never_resumes_the_settled_batch() {
    // The cancel lands *while the batch is in flight*: the handler
    // completes successfully but flags the token. The post-batch re-check
    // must stop the drive before the resolution is resumed (M4-5).
    let tool = CancellingToolHandler::default();
    let scope = TestScope {
        cancelling_tool: Some(tool.clone()),
        ..TestScope::default()
    };
    let mut machine = BatchMachine::new(vec![tool_requirement(4, 0)]);
    let ctx = run_context();

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("cancel-after-fulfill drain closes the turn");

    assert!(done.cancelled());
    // The handler ran (the batch was fulfilled) ...
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    // ... but the resolution was never fed back to the machine.
    assert!(machine.resume_order.is_empty());
    // And the fulfilled-but-discarded requirement is traced never-resumed,
    // not resumed.
    assert_eq!(
        requirement_trace(&ctx, requirement_id_n(4), RequirementKindTag::Tool),
        (0, RequirementDisposition::NeverResumed)
    );
}

// ----- M3-3: cancel pre-empts a blocked batch -----

/// Flags when the future holding it is dropped, proving a blocked fulfill
/// future was detached by the cancelled drive.
struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A tool handler whose fulfill future never resolves: it records that it
/// started, then parks forever unless the future is dropped.
#[derive(Clone, Default)]
struct BlockingToolHandler {
    started: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolHandler for BlockingToolHandler {
    async fn fulfill(
        &self,
        _call_id: ToolCallId,
        _call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        self.started.fetch_add(1, Ordering::SeqCst);
        let _probe = DropProbe(self.dropped.clone());
        std::future::pending::<()>().await;
        unreachable!("the blocked tool future never completes")
    }
}

/// An interaction handler whose fulfill future never resolves, mirroring
/// [`BlockingToolHandler`] for the interaction family.
#[derive(Clone, Default)]
struct BlockingInteractionHandler {
    started: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

#[async_trait]
impl InteractionHandler for BlockingInteractionHandler {
    async fn fulfill(&self, _request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        self.started.fetch_add(1, Ordering::SeqCst);
        let _probe = DropProbe(self.dropped.clone());
        std::future::pending::<()>().await;
        unreachable!("the blocked interaction future never completes")
    }
}

#[tokio::test]
async fn drain_preempts_a_blocked_tool_and_interaction_batch_on_cancel() {
    // A batch whose tool and interaction handlers both block forever:
    // cancel must pre-empt the batch wait, detach both futures, and close
    // the turn as cancelled within seconds (M3-3).
    let tool = BlockingToolHandler::default();
    let interaction = BlockingInteractionHandler::default();
    let scope = TestScope {
        blocking_tool: Some(tool.clone()),
        blocking_interaction: Some(interaction.clone()),
        ..TestScope::default()
    };
    let mut machine = BatchMachine::new(vec![tool_requirement(9, 0), interaction_requirement(10)]);
    let ctx = run_context();
    let token = ctx.cancellation().clone();

    let canceller = {
        let tool = tool.clone();
        let interaction = interaction.clone();
        async move {
            // Cancel only once both fulfill futures are genuinely in
            // flight, so the test cannot pass through the pre-batch check.
            while tool.started.load(Ordering::SeqCst) == 0
                || interaction.started.load(Ordering::SeqCst) == 0
            {
                tokio::task::yield_now().await;
            }
            token.cancel();
        }
    };
    let drive = drain(&mut machine, external_input(), &scope, None, &ctx);
    let (result, ()) = tokio::join!(
        async move { tokio::time::timeout(std::time::Duration::from_secs(30), drive).await },
        canceller,
    );
    let done = result
        .expect("a pre-empted drain settles within seconds, never hangs")
        .expect("cancelled drain closes the turn");

    assert!(done.cancelled());
    // Both blocked futures were dropped (detached), never resumed.
    assert_eq!(tool.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(interaction.dropped.load(Ordering::SeqCst), 1);
    assert!(machine.resume_order.is_empty());
    // Every outstanding requirement is traced never-resumed at hop 0.
    for (n, tag) in [
        (9, RequirementKindTag::Tool),
        (10, RequirementKindTag::Interaction),
    ] {
        assert_eq!(
            requirement_trace(&ctx, requirement_id_n(n), tag),
            (0, RequirementDisposition::NeverResumed)
        );
    }

    // The machine still accepts a fresh turn: a second drain with
    // cooperative handlers runs to a normal completion.
    let cooperative = TestScope {
        tool: Some(CountingToolHandler::default()),
        interaction: Some(CountingInteractionHandler::default()),
        ..TestScope::default()
    };
    let next = drain(
        &mut machine,
        external_input(),
        &cooperative,
        None,
        &run_context(),
    )
    .await
    .expect("the machine drives another turn after a pre-empted one");
    assert!(!next.cancelled());
    assert!(matches!(next.cursor(), LoopCursor::Done(_)));
}

// ----- M4-3: pivot re-emission vs trace node id dedup (H-STATE-4) -----

/// A machine that re-emits its outstanding requirement under the *same* id
/// once — mirroring the default machine's pivot path, which re-renders the
/// LLM request without minting a new requirement id — then completes on the
/// second resume.
struct PivotReemitMachine {
    cursor: LoopCursor,
    requirement: Requirement,
    reemitted: bool,
    resumes: usize,
}

impl PivotReemitMachine {
    fn new(requirement: Requirement) -> Self {
        Self {
            cursor: LoopCursor::default(),
            requirement,
            reemitted: false,
            resumes: 0,
        }
    }
}

impl AgentMachine for PivotReemitMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        match input {
            StepInput::External(_) => {
                self.cursor = LoopCursor::streaming_step(step_id(), None);
                StepOutcome::new(Vec::new(), vec![self.requirement.clone()], true)
            }
            StepInput::Resume(_) => {
                self.resumes += 1;
                if self.reemitted {
                    self.cursor = LoopCursor::done(LoopDoneReason::Completed);
                    StepOutcome::new(Vec::new(), Vec::new(), true)
                } else {
                    // Pivot: re-emit the outstanding requirement under the
                    // same id so it is fulfilled a second time.
                    self.reemitted = true;
                    StepOutcome::new(Vec::new(), vec![self.requirement.clone()], true)
                }
            }
            StepInput::Abandon(_) => StepOutcome::default(),
        }
    }

    fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }
}

#[tokio::test]
async fn drain_records_pivot_reemission_under_a_derived_trace_id() {
    let tool = CountingToolHandler::default();
    let scope = TestScope {
        tool: Some(tool.clone()),
        ..TestScope::default()
    };
    let mut machine = PivotReemitMachine::new(tool_requirement(5, 0));
    let ctx = run_context();

    let done = drain(&mut machine, external_input(), &scope, None, &ctx)
        .await
        .expect("a pivot re-emission must not kill the drain");

    // The turn ran to completion and the re-emitted requirement really was
    // fulfilled twice.
    assert!(matches!(done.cursor(), LoopCursor::Done(_)));
    assert_eq!(machine.resumes, 2);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 2);

    // The first settle is recorded under the plain requirement id.
    assert_eq!(
        requirement_trace(&ctx, requirement_id_n(5), RequirementKindTag::Tool),
        (0, RequirementDisposition::Resumed)
    );
    // The re-emitted settle is kept on the trace under the derived
    // `<id>#attempt-2` node id instead of failing the drain.
    let derived_id = format!("{}#attempt-2", requirement_id_n(5));
    let records = ctx.trace().records();
    let derived = records
        .iter()
        .find(|record| record.id().as_str() == derived_id)
        .expect("the re-emitted settle is recorded under a derived node id");
    match derived.kind() {
        TraceNodeKind::Requirement {
            kind_tag,
            resolved_at_scope,
            disposition,
        } => {
            assert_eq!(kind_tag, RequirementKindTag::Tool);
            assert_eq!(
                (resolved_at_scope, disposition),
                (0, RequirementDisposition::Resumed)
            );
        }
        other => panic!("expected a requirement trace node, got {other:?}"),
    }
}

// --- Generated drive fan-out routing (design points 5–7) -----------------
//
// `scope_handles` and `fulfill_with_scope` are generated from the effect
// manifest. These ZST handlers let one scope satisfy all six families so the
// fan-out can be exercised per family: each returns a fixed, matching-family
// result. `EqSubagent` is never invoked because `Subagent` is `needs_outer`
// (`fulfill_with_scope` shorts to `None`), so its body asserts unreachability.

struct EqLlm;
#[async_trait]
impl LlmHandler for EqLlm {
    async fn fulfill(
        &self,
        _request: &ChatRequest,
        _mode: LlmStepMode,
        _ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::Llm(Ok(response()))
    }
}

struct EqTool;
#[async_trait]
impl ToolHandler for EqTool {
    async fn fulfill(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::Tool(Ok(ok_tool_response(call)))
    }
}

struct EqInteraction;
#[async_trait]
impl InteractionHandler for EqInteraction {
    async fn fulfill(&self, _request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        RequirementResult::Interaction(InteractionResponse::Answer("ok".to_owned()))
    }
}

struct EqSubagent;
#[async_trait]
impl SubagentHandler for EqSubagent {
    async fn fulfill(
        &self,
        _spec_ref: &AgentSpecRef,
        _brief: &Interaction,
        _result_schema: Option<&Value>,
        _outer: &mut dyn Pop,
        _ctx: &RunContext,
    ) -> RequirementResult {
        unreachable!("Subagent is needs_outer; fulfill_with_scope never calls its handler")
    }
}

struct EqReconfig;
#[async_trait]
impl ReconfigHandler for EqReconfig {
    async fn fulfill(&self, _tool_set: &ToolSetRef, _ctx: &RunContext) -> RequirementResult {
        RequirementResult::Reconfig(Ok(()))
    }
}

struct EqExternal;
#[async_trait]
impl ExternalSessionHandler for EqExternal {
    async fn fulfill(
        &self,
        _request: &ExternalSessionRequest,
        _ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::ExternalSession(Box::new(external_session_result()))
    }
}

static EQ_LLM: EqLlm = EqLlm;
static EQ_TOOL: EqTool = EqTool;
static EQ_INTERACTION: EqInteraction = EqInteraction;
static EQ_SUBAGENT: EqSubagent = EqSubagent;
static EQ_RECONFIG: EqReconfig = EqReconfig;
static EQ_EXTERNAL: EqExternal = EqExternal;

/// A scope offering a handler for every family, so the fan-out can be
/// exercised for each requirement kind.
struct EqScope;

impl HandlerScope for EqScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        Some(&EQ_LLM)
    }
    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&EQ_TOOL)
    }
    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        Some(&EQ_INTERACTION)
    }
    fn subagent(&self) -> Option<&dyn SubagentHandler> {
        Some(&EQ_SUBAGENT)
    }
    fn reconfig(&self) -> Option<&dyn ReconfigHandler> {
        Some(&EQ_RECONFIG)
    }
    fn external(&self) -> Option<&dyn ExternalSessionHandler> {
        Some(&EQ_EXTERNAL)
    }
}

/// Builds a representative requirement kind for `tag`.
fn kind_of(tag: RequirementKindTag) -> RequirementKind {
    match tag {
        RequirementKindTag::Llm => RequirementKind::NeedLlm {
            request: chat_request(),
            mode: LlmStepMode::NonStreaming,
        },
        RequirementKindTag::Tool => RequirementKind::NeedTool {
            call_id: tool_call_id_n(1),
            call: tool_call(),
        },
        RequirementKindTag::Interaction => RequirementKind::NeedInteraction {
            request: Interaction::approval(
                step_id(),
                tool_call_id_n(1),
                ApprovalRequirement::AutoApprove,
            ),
        },
        RequirementKindTag::Subagent => RequirementKind::NeedSubagent {
            spec_ref: AgentSpecRef(agent_id()),
            brief: Interaction::approval(
                step_id(),
                tool_call_id_n(1),
                ApprovalRequirement::AutoApprove,
            ),
            result_schema: None,
        },
        RequirementKindTag::Reconfig => RequirementKind::NeedReconfigRegistry {
            tool_set: eq_tool_set(),
        },
        RequirementKindTag::ExternalSession => RequirementKind::NeedExternalSession {
            request: external_session_request(),
        },
    }
}

/// A minimal tool-set reference for a `NeedReconfigRegistry` requirement.
fn eq_tool_set() -> ToolSetRef {
    let id: ToolSetId = "018f0d9c-7b6a-7c12-8f31-1234567890f2"
        .parse()
        .expect("tool set id");
    ToolSetRef::new(id, Vec::new())
}

#[tokio::test]
async fn fan_out_routes_every_family_consistently() {
    let scope = EqScope;
    let ctx = run_context();
    let tags = [
        RequirementKindTag::Llm,
        RequirementKindTag::Tool,
        RequirementKindTag::Interaction,
        RequirementKindTag::Subagent,
        RequirementKindTag::Reconfig,
        RequirementKindTag::ExternalSession,
    ];

    for tag in tags {
        // `EqScope` offers a handler for every family, so the routing
        // predicate holds for all six.
        assert!(scope_handles(&scope, tag), "scope must handle {tag}");

        let result = fulfill_with_scope(&kind_of(tag), &scope, &ctx).await;
        if tag == RequirementKindTag::Subagent {
            // `Subagent` is `needs_outer`: it is routed out (`None`) rather
            // than fulfilled in place, even though the scope offers a handler.
            assert!(
                result.is_none(),
                "Subagent must route outward, not fulfill in place"
            );
        } else {
            // The `scope_handles`/`fulfill_with_scope` consistency invariant
            // that `fulfill_batch` relies on (its `expect` after a positive
            // `scope_handles`): a handled family fulfills in place with a
            // result of the matching family.
            let result = result.expect("handled family must fulfill in place");
            assert_eq!(
                result.tag(),
                tag,
                "fulfilled result family must match the requirement family"
            );
        }
    }
}

/// A subagent handler that drives a child to a real summary, unlike the
/// `unreachable!` [`EqSubagent`] stub, so a mixed batch can be fulfilled to
/// completion.
struct RealSubagent;
#[async_trait]
impl SubagentHandler for RealSubagent {
    async fn fulfill(
        &self,
        _spec_ref: &AgentSpecRef,
        _brief: &Interaction,
        _result_schema: Option<&Value>,
        _outer: &mut dyn Pop,
        _ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::Subagent(Ok(SubagentOutput {
            summary: "child summary".to_owned(),
        }))
    }
}

static REAL_SUBAGENT: RealSubagent = RealSubagent;

/// A scope offering tool and subagent handlers so a mixed batch can be
/// fulfilled: the tool joins the concurrent local set while the subagent must
/// be resolved serially.
struct MixedToolSubagentScope;

impl HandlerScope for MixedToolSubagentScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        None
    }
    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&EQ_TOOL)
    }
    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        None
    }
    fn subagent(&self) -> Option<&dyn SubagentHandler> {
        Some(&REAL_SUBAGENT)
    }
    fn reconfig(&self) -> Option<&dyn ReconfigHandler> {
        None
    }
    fn external(&self) -> Option<&dyn ExternalSessionHandler> {
        None
    }
}

fn subagent_requirement(n: u8) -> Requirement {
    Requirement::at_root(requirement_id_n(n), kind_of(RequirementKindTag::Subagent))
}

#[tokio::test]
async fn mixed_tool_and_subagent_batch_routes_subagent_serially() {
    // A mixed batch of an ordinary tool and a subagent (as a `spawn_agent`
    // bridge produces): the scope handles both families.
    let scope = MixedToolSubagentScope;
    let ctx = run_context();
    let batch = vec![tool_requirement(1, 0), subagent_requirement(2)];

    // `fulfill_batch` must not panic: a subagent is `needs_outer`, so if it
    // had joined the concurrent local set, `fulfill_with_scope` would short to
    // `None` and the `.expect` after `scope_handles` would panic. Completing
    // successfully proves the subagent was routed serially instead.
    let resolutions = fulfill_batch(&batch, &scope, None, &ctx)
        .await
        .expect("mixed batch resolves");

    assert_eq!(resolutions.len(), 2);

    // Each requirement is answered with a result of its own family: the tool
    // in place (concurrent set), the subagent through the serial subagent
    // handler.
    let tool = resolutions
        .iter()
        .find(|resolved| resolved.resolution.id == requirement_id_n(1))
        .expect("tool resolution present");
    assert_eq!(tool.resolution.result.tag(), RequirementKindTag::Tool);
    assert_eq!(tool.resolved_at_scope, 0);

    let subagent = resolutions
        .iter()
        .find(|resolved| resolved.resolution.id == requirement_id_n(2))
        .expect("subagent resolution present");
    assert_eq!(
        subagent.resolution.result.tag(),
        RequirementKindTag::Subagent
    );
    // The emitting scope owns the subagent handler, so it settles in place
    // (zero pop hops) rather than being popped outward.
    assert_eq!(subagent.resolved_at_scope, 0);
    match &subagent.resolution.result {
        RequirementResult::Subagent(Ok(output)) => {
            assert_eq!(output.summary, "child summary");
        }
        other => panic!("expected a Subagent(Ok) result, got {other:?}"),
    }
}
