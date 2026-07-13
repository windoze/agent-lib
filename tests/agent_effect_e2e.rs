//! End-to-end acceptance: attended parent + headless child are one graph.
//!
//! This is the M6-2 acceptance example (migration doc §1 / §4.4 / §7). It proves,
//! against the public crate API and fully offline fakes, that "attended" and
//! "unattended/headless" are not two agent modes but two *wirings of the same
//! graph*: the identical child subagent spec resolves an approval **in place**
//! when its own scope carries an interaction backend, and **pops the very same
//! approval to its attended parent** when its scope omits one — with no change to
//! the child itself.
//!
//! The child is a real [`DefaultAgentMachine`] driven by a fake
//! [`LlmClient`]/[`ToolRegistry`] through a tool round-trip guarded by a
//! require-approval policy. The parent is a small scripted machine that emits a
//! `NeedTool` and a `NeedSubagent` in one turn, so the run exercises a tool and a
//! subagent together. The reference [`drain`] driver + [`DrivingSubagentHandler`]
//! provide the mechanism.
//!
//! Coverage (one focused `#[tokio::test]` each):
//!
//! 1. `attended_parent_serves_headless_child_via_pop` — a headless child's
//!    approval pops to the attended parent's policy backend and is granted, the
//!    guarded tool then runs, and the child's token charges aggregate onto the
//!    parent's shared budget ledger (pop routing + hierarchy + budget).
//! 2. `same_child_spec_attended_resolves_in_place` — the *same* child spec, given
//!    an interaction backend on its own scope, resolves the approval locally with
//!    the same committed conversation (run mode = scope wiring).
//! 3. `batch_requirements_are_fulfilled_concurrently` — a single step's batch of
//!    tool requirements is fulfilled concurrently, not serially (decision B).
//! 4. `parent_cancel_propagates_and_abandons_child` — a cancelled parent context
//!    propagates into the derived child, which abandons its first requirement
//!    (never-resume) without performing any IO.

use agent_lib::{
    agent::{
        AgentError, AgentId, AgentInput, AgentMachine, AgentSpec, AgentSpecRef, AgentState,
        ApprovalDecision, ApprovalRequirement, ApprovalResponse, BudgetLimits, DefaultAgentMachine,
        DrivingSubagentHandler, HandlerScope, Interaction, InteractionHandler, InteractionKind,
        InteractionResponse, LlmHandler, LlmStepMode, LoopCursor, LoopCursorKind, LoopDoneReason,
        LoopPolicy, ModelRef, NoApprovalPolicy, Requirement, RequirementError, RequirementId,
        RequirementIds, RequirementKind, RequirementKindTag, RequirementResult, RunContext, RunId,
        ScopePop, SpawnedChild, StepId, StepInput, StepOutcome, SubagentHandler, SubagentOutput,
        SubagentSpawner, ToolApprovalPolicy, ToolExecutionIds, ToolFailurePolicy, ToolHandler,
        ToolRegistry, ToolRegistryHandler, ToolRuntimeError, ToolSetId, ToolSetRef, TraceNodeId,
        TurnDone, WorktreeRef, drain,
    },
    client::{Capability, ChatRequest, ClientError, LlmClient, Response},
    conversation::{
        Conversation, ConversationConfig, ConversationId, MessageId, ToolCallId, TurnId,
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::{Tool, ToolCall, ToolResponse, ToolStatus},
        usage::Usage,
    },
    stream::StreamEvent,
};
use async_trait::async_trait;
use futures::{StreamExt, stream};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeSet, VecDeque},
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

// ----- deterministic offline id source -----

/// Mints globally unique UUID-shaped ids from one shared counter.
///
/// Cloning shares the same `AtomicU64`, so every id handed out across the parent
/// machine, every child machine, and the sub-agent trace nodes is distinct. That
/// uniqueness matters because the driver records each requirement and sub-agent
/// under a trace node keyed by its id, and duplicate trace-node ids are rejected.
#[derive(Clone, Debug)]
struct SeqIds {
    counter: Arc<AtomicU64>,
}

impl SeqIds {
    fn new() -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(1)),
        }
    }

    fn raw(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        format!("018f0d9c-7b6a-7c12-8f31-{n:012x}")
    }

    fn mint_requirement_id(&self) -> RequirementId {
        RequirementId::parse_str(&self.raw()).expect("valid requirement id")
    }

    fn mint_run_id(&self) -> RunId {
        self.raw().parse().expect("valid run id")
    }

    fn mint_trace_node(&self) -> TraceNodeId {
        TraceNodeId::new(self.raw())
    }

    fn mint_tool_call_id(&self) -> ToolCallId {
        self.raw().parse().expect("valid tool call id")
    }

    fn mint_message_id(&self) -> MessageId {
        self.raw().parse().expect("valid message id")
    }

    fn mint_step_id(&self) -> StepId {
        self.raw().parse().expect("valid step id")
    }

    fn mint_turn_id(&self) -> TurnId {
        self.raw().parse().expect("valid turn id")
    }
}

impl RequirementIds for SeqIds {
    fn next_requirement_id(
        &self,
        _kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        Ok(self.mint_requirement_id())
    }
}

impl ToolExecutionIds for SeqIds {
    fn tool_call_id(&self, _call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        Ok(self.mint_tool_call_id())
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        _call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        Ok(self.mint_message_id())
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        Ok(self.mint_message_id())
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        Ok(self.mint_step_id())
    }
}

// ----- offline fakes -----

/// Fake [`LlmClient`] that returns scripted `chat` responses in order.
#[derive(Debug)]
struct FakeClient {
    capability: Capability,
    chat_results: Mutex<VecDeque<Result<Response, ClientError>>>,
    requests: Mutex<usize>,
}

impl FakeClient {
    fn with_chats(results: Vec<Result<Response, ClientError>>) -> Self {
        Self {
            capability: Capability::default(),
            chat_results: Mutex::new(VecDeque::from(results)),
            requests: Mutex::new(0),
        }
    }

    fn request_count(&self) -> usize {
        *self.requests.lock().expect("requests mutex")
    }
}

#[async_trait]
impl LlmClient for FakeClient {
    fn capability(&self) -> &Capability {
        &self.capability
    }

    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
        *self.requests.lock().expect("requests mutex") += 1;
        self.chat_results
            .lock()
            .expect("chat results mutex")
            .pop_front()
            .expect("a scripted chat result is available")
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError>
    {
        Ok(stream::iter(Vec::<Result<StreamEvent, ClientError>>::new()).boxed())
    }
}

/// Fake [`ToolRegistry`] that returns scripted execution results in order.
#[derive(Debug)]
struct FakeToolRegistry {
    declarations: Vec<Tool>,
    results: Mutex<VecDeque<Result<ToolResponse, ToolRuntimeError>>>,
    calls: Mutex<Vec<(ToolCallId, ToolCall)>>,
}

impl FakeToolRegistry {
    fn new(results: Vec<Result<ToolResponse, ToolRuntimeError>>) -> Self {
        Self {
            declarations: vec![weather_tool()],
            results: Mutex::new(VecDeque::from(results)),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().expect("tool calls mutex").len()
    }
}

#[async_trait]
impl ToolRegistry for FakeToolRegistry {
    fn declarations(&self) -> Vec<Tool> {
        self.declarations.clone()
    }

    async fn execute(
        &self,
        call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        self.calls
            .lock()
            .expect("tool calls mutex")
            .push((call_id, call));
        self.results
            .lock()
            .expect("tool results mutex")
            .pop_front()
            .expect("a scripted tool result is available")
    }
}

/// Approval policy that requires human approval for every tool call.
#[derive(Debug)]
struct RequireApprovalPolicy {
    reason: Option<String>,
}

impl RequireApprovalPolicy {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
        }
    }
}

impl ToolApprovalPolicy for RequireApprovalPolicy {
    fn approval_requirement(&self, _call_id: ToolCallId, _call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::required(self.reason.clone())
    }
}

// ----- effect handlers (host-supplied) -----

/// An interaction backend that approves every approval and counts how many it
/// served. This is the unattended *policy* interaction backend: which tool calls
/// even reach it was already decided upstream by the machine's approval policy.
#[derive(Default)]
struct CountingApproveInteraction {
    served: Arc<AtomicUsize>,
}

#[async_trait]
impl InteractionHandler for CountingApproveInteraction {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        self.served.fetch_add(1, Ordering::SeqCst);
        let InteractionKind::Approval { call_id, .. } = request.kind() else {
            panic!("the policy backend only answers approvals");
        };
        RequirementResult::Interaction(InteractionResponse::Approval(ApprovalResponse::new(
            request.step_id(),
            *call_id,
            ApprovalDecision::Approve,
            None,
        )))
    }
}

/// An LLM handler that runs a fake client and charges the response usage against
/// the run context, so a child's model consumption lands on the shared ledger.
struct ChargingLlmHandler {
    client: Arc<dyn LlmClient>,
    charged: Arc<AtomicU64>,
}

#[async_trait]
impl LlmHandler for ChargingLlmHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        let mut request = request.clone();
        request.stream = matches!(mode, LlmStepMode::Streaming);
        let result = self.client.chat(request).await;
        if let Ok(response) = &result {
            let tokens = u64::from(response.usage.input) + u64::from(response.usage.output);
            self.charged.fetch_add(tokens, Ordering::SeqCst);
            ctx.charge_tokens(tokens)
                .expect("charge child usage on the shared ledger");
        }
        RequirementResult::Llm(result)
    }
}

/// A tool handler that returns a fixed result and counts invocations. Used for
/// the parent's own tool step.
#[derive(Default)]
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
        RequirementResult::Tool(Ok(tool_response(&call.id, "noted", ToolStatus::Ok)))
    }
}

/// A tool handler that records the peak number of concurrently in-flight calls,
/// then defers to a fake registry. If a batch of requirements were fulfilled
/// serially the peak would stay at 1; concurrent fulfillment drives it higher.
struct ConcurrentToolHandler {
    registry: Arc<FakeToolRegistry>,
    in_flight: Arc<AtomicUsize>,
    peak_in_flight: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolHandler for ConcurrentToolHandler {
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);
        // Yield so a co-scheduled sibling future can also enter before any of
        // them completes; on a current-thread runtime this is what lets the
        // concurrent batch overlap deterministically.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        let result = self.registry.execute(call_id, call.clone()).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        RequirementResult::Tool(result)
    }
}

// ----- scopes -----

/// The attended parent's drain layer: it serves tool steps, resolves approvals
/// through its policy interaction backend, and derives subagents. It carries no
/// LLM handler because the scripted parent machine never asks for one.
struct ParentScope {
    tool: CountingToolHandler,
    interaction: CountingApproveInteraction,
    subagent: DrivingSubagentHandler,
}

impl HandlerScope for ParentScope {
    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&self.tool)
    }

    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        Some(&self.interaction)
    }

    fn subagent(&self) -> Option<&dyn SubagentHandler> {
        Some(&self.subagent)
    }
}

/// The child's own drain layer. It always serves LLM and tool steps; whether it
/// carries an interaction backend is the *only* difference between an attended
/// child (serves its own approvals) and a headless child (pops them to the
/// parent). Same child spec, different scope wiring.
struct ChildScope {
    llm: ChargingLlmHandler,
    tool: ToolRegistryHandler,
    interaction: Option<CountingApproveInteraction>,
}

impl HandlerScope for ChildScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        Some(&self.llm)
    }

    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&self.tool)
    }

    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        self.interaction
            .as_ref()
            .map(|handler| handler as &dyn InteractionHandler)
    }
}

/// An empty scope that handles nothing: used as the outer pop target when a
/// child's requirements should never actually pop.
struct EmptyScope;

impl HandlerScope for EmptyScope {}

// ----- scripted parent machine -----

/// A minimal parent machine that, on its opening turn, emits one `NeedTool`
/// (its own) and one `NeedSubagent` in a single batch, then completes once both
/// are resumed. Real subagent-emitting machines are a future layer; this double
/// exercises the driver + subagent handler against a live child.
struct ParentBatchMachine {
    cursor: LoopCursor,
    ids: SeqIds,
    parent_call_id: ToolCallId,
    parent_call: ToolCall,
    spec_ref: AgentSpecRef,
    brief: Interaction,
    outstanding: BTreeSet<RequirementId>,
}

impl ParentBatchMachine {
    fn new(ids: SeqIds, spec_ref: AgentSpecRef, brief: Interaction) -> Self {
        Self {
            cursor: LoopCursor::Idle,
            parent_call_id: ids.mint_tool_call_id(),
            parent_call: ToolCall {
                id: "parent-note".to_owned(),
                name: "note".to_owned(),
                input: json!({ "text": "record progress" }),
            },
            ids,
            spec_ref,
            brief,
            outstanding: BTreeSet::new(),
        }
    }
}

impl AgentMachine for ParentBatchMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        match input {
            StepInput::External(_) => {
                let batch = vec![
                    Requirement::at_root(
                        self.ids.mint_requirement_id(),
                        RequirementKind::NeedTool {
                            call_id: self.parent_call_id,
                            call: self.parent_call.clone(),
                        },
                    ),
                    Requirement::at_root(
                        self.ids.mint_requirement_id(),
                        RequirementKind::NeedSubagent {
                            spec_ref: self.spec_ref,
                            brief: self.brief.clone(),
                            result_schema: None,
                        },
                    ),
                ];
                self.outstanding = batch.iter().map(|requirement| requirement.id).collect();
                self.cursor = LoopCursor::streaming_step(self.ids.mint_step_id(), None);
                StepOutcome::new(Vec::new(), batch, true)
            }
            StepInput::Resume(resolution) => {
                self.outstanding.remove(&resolution.id);
                if self.outstanding.is_empty() {
                    self.cursor = LoopCursor::done(LoopDoneReason::Completed);
                }
                StepOutcome::new(Vec::new(), Vec::new(), true)
            }
            StepInput::Abandon(_) => {
                self.cursor = LoopCursor::done(LoopDoneReason::Completed);
                StepOutcome::default()
            }
        }
    }

    fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }
}

// ----- subagent spawner -----

type BuildChild = Box<dyn Fn() -> SpawnedChild + Send + Sync>;

/// Turns each `NeedSubagent` into a live child by invoking a stored builder and
/// minting deterministic child ids from the shared sequence.
struct ChildSpawner {
    ids: SeqIds,
    build: BuildChild,
    summary: String,
}

impl SubagentSpawner for ChildSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        Ok((self.ids.mint_run_id(), self.ids.mint_trace_node()))
    }

    fn spawn(
        &self,
        _spec_ref: &AgentSpecRef,
        _brief: &Interaction,
        _result_schema: Option<&Value>,
    ) -> Result<SpawnedChild, AgentError> {
        Ok((self.build)())
    }

    fn summarize(&self, _done: &TurnDone) -> SubagentOutput {
        SubagentOutput {
            summary: self.summary.clone(),
        }
    }
}

// ----- payload / builder helpers -----

fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("non-zero test value")
}

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890a1"
        .parse()
        .expect("agent id")
}

fn tool_set_id() -> ToolSetId {
    "018f0d9c-7b6a-7c12-8f31-1234567890a2"
        .parse()
        .expect("tool set id")
}

fn conversation_id() -> ConversationId {
    "018f0d9c-7b6a-7c12-8f31-1234567890a3"
        .parse()
        .expect("conversation id")
}

fn weather_tool() -> Tool {
    Tool {
        name: "get_weather".to_owned(),
        description: "Look up weather for a city.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    }
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_owned(),
        extra: Map::new(),
    }
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![text_block(text)],
    }
}

fn usage(input: u32, output: u32) -> Usage {
    Usage {
        input,
        output,
        ..Usage::default()
    }
}

fn assistant_response(text: &str, usage: Usage) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![text_block(text)],
        },
        usage,
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

fn tool_use_response(calls: Vec<(&str, &str, Value)>, usage: Usage) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: calls
                .into_iter()
                .map(|(id, name, input)| ContentBlock::ToolUse {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    input,
                    extra: Map::new(),
                })
                .collect(),
        },
        usage,
        stop_reason: StopReason::normalize("tool_use"),
        extra: Map::new(),
    }
}

fn tool_response(provider_call_id: &str, text: &str, status: ToolStatus) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_call_id.to_owned(),
        content: vec![text_block(text)],
        status,
        extra: Map::new(),
    }
}

fn child_spec(max_parallel_tools: u32) -> AgentSpec {
    AgentSpec::new(
        agent_id(),
        WorktreeRef::new("/repo/agent-lib"),
        Some("Child agent system.".to_owned()),
        ToolSetRef::new(tool_set_id(), vec![weather_tool()]),
        ModelRef::new("gpt-5.5", nz(512), Some(0.1), None),
        LoopPolicy::new(
            nz(8),
            nz(max_parallel_tools),
            ToolFailurePolicy::ReturnErrorToModel,
        ),
    )
}

fn child_state(max_parallel_tools: u32) -> AgentState {
    AgentState::new(
        child_spec(max_parallel_tools),
        Conversation::new(
            conversation_id(),
            ConversationConfig::new(Some("Child conversation system.".to_owned())),
        ),
    )
}

/// Builds the child agent machine: a real [`DefaultAgentMachine`] that runs a
/// guarded weather tool round-trip. `approval` decides whether the machine even
/// asks for approval; the *scope* (not the machine) decides who answers it.
fn build_child_machine(
    ids: &SeqIds,
    max_parallel_tools: u32,
    approval: Arc<dyn ToolApprovalPolicy>,
) -> DefaultAgentMachine {
    DefaultAgentMachine::new(
        child_state(max_parallel_tools),
        LlmStepMode::NonStreaming,
        Arc::new(ids.clone()),
    )
    .with_tool_execution_ids(Arc::new(ids.clone()))
    .with_approval_policy(approval)
}

fn child_opening(ids: &SeqIds) -> AgentInput {
    AgentInput::user_message(
        ids.mint_turn_id(),
        ids.mint_message_id(),
        user_message("Use get_weather for Shanghai, then answer from it."),
        ids.mint_message_id(),
        ids.mint_step_id(),
    )
    .expect("valid child opening input")
}

fn root_context(ids: &SeqIds) -> RunContext {
    RunContext::new_root(
        ids.mint_run_id(),
        BudgetLimits::unbounded(),
        TraceNodeId::new("root"),
    )
}

/// The scripted responses of a single guarded weather round-trip: request the
/// tool, then answer from its result. Total usage is 7 + 11 = 18 tokens.
fn approval_round_trip_chats() -> Vec<Result<Response, ClientError>> {
    vec![
        Ok(tool_use_response(
            vec![("call-weather", "get_weather", json!({ "city": "Shanghai" }))],
            usage(5, 2),
        )),
        Ok(assistant_response("sunny, per get_weather", usage(7, 4))),
    ]
}

const APPROVAL_ROUND_TRIP_TOKENS: u64 = 18;

fn assert_text(message: &Message, expected: &str) {
    assert_eq!(message.content.len(), 1, "one content block");
    let ContentBlock::Text { text, .. } = &message.content[0] else {
        panic!("expected a text block");
    };
    assert_eq!(text, expected);
}

// ----- tests -----

/// A headless child's approval pops to the attended parent's policy backend and
/// is granted; the guarded tool then runs, and the child's token charges show up
/// on the parent's shared budget ledger. One turn, one tool, one subagent.
#[tokio::test]
async fn attended_parent_serves_headless_child_via_pop() {
    let ids = SeqIds::new();

    // The child: a real machine that requires approval for its weather call, with
    // scripted offline client + registry captured for post-run assertions.
    let child_client = Arc::new(FakeClient::with_chats(approval_round_trip_chats()));
    let child_registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let child_charged = Arc::new(AtomicU64::new(0));

    let build_ids = ids.clone();
    let build_client: Arc<dyn LlmClient> = child_client.clone();
    let build_registry = child_registry.clone();
    let build_charged = child_charged.clone();
    let build: BuildChild = Box::new(move || SpawnedChild {
        machine: Box::new(build_child_machine(
            &build_ids,
            1,
            Arc::new(RequireApprovalPolicy::new("human approval required")),
        )),
        // Headless child scope: LLM + tool, *no* interaction backend, so the
        // approval pops outward to the attended parent.
        scope: Box::new(ChildScope {
            llm: ChargingLlmHandler {
                client: build_client.clone(),
                charged: build_charged.clone(),
            },
            tool: ToolRegistryHandler::new(build_registry.clone()),
            interaction: None,
        }),
        opening: child_opening(&build_ids),
    });

    let spawner = Arc::new(ChildSpawner {
        ids: ids.clone(),
        build,
        summary: "child looked up the weather".to_owned(),
    });

    let parent_tool_calls = Arc::new(AtomicUsize::new(0));
    let parent_served = Arc::new(AtomicUsize::new(0));
    let parent_scope = ParentScope {
        tool: CountingToolHandler {
            calls: parent_tool_calls.clone(),
        },
        interaction: CountingApproveInteraction {
            served: parent_served.clone(),
        },
        subagent: DrivingSubagentHandler::new(spawner, 4),
    };

    let spec_ref = AgentSpecRef(agent_id());
    let brief = Interaction::question(ids.mint_step_id(), "look up the weather".to_owned());
    let mut parent = ParentBatchMachine::new(ids.clone(), spec_ref, brief);
    let ctx = root_context(&ids);

    let done = drain(
        &mut parent,
        AgentInput::user_message(
            ids.mint_turn_id(),
            ids.mint_message_id(),
            user_message("delegate the weather lookup"),
            ids.mint_message_id(),
            ids.mint_step_id(),
        )
        .expect("valid parent input"),
        &parent_scope,
        None,
        &ctx,
    )
    .await
    .expect("parent turn drains to completion");

    // The whole turn closed on the parent.
    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert!(matches!(parent.cursor(), LoopCursor::Done(_)));

    // The parent served its own tool step exactly once.
    assert_eq!(parent_tool_calls.load(Ordering::SeqCst), 1);

    // The child's approval popped to the attended parent, which answered it once;
    // only because it was granted did the guarded weather tool run in the child.
    assert_eq!(parent_served.load(Ordering::SeqCst), 1);
    assert_eq!(child_registry.call_count(), 1);
    assert_eq!(child_client.request_count(), 2);

    // Budget aggregation: the child's token charges (18) land on the parent's
    // shared ledger via the derived child context.
    assert_eq!(
        child_charged.load(Ordering::SeqCst),
        APPROVAL_ROUND_TRIP_TOKENS
    );
    assert_eq!(
        ctx.budget().snapshot().used().tokens(),
        APPROVAL_ROUND_TRIP_TOKENS
    );
}

/// The *same* child spec, given an interaction backend on its own scope, resolves
/// the approval in place — no parent, no pop — and commits the same conversation.
/// This is the "run mode = scope wiring" half of the acceptance.
#[tokio::test]
async fn same_child_spec_attended_resolves_in_place() {
    let ids = SeqIds::new();

    let child_client = Arc::new(FakeClient::with_chats(approval_round_trip_chats()));
    let child_registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let child_served = Arc::new(AtomicUsize::new(0));
    let child_charged = Arc::new(AtomicU64::new(0));

    // Identical child machine to the headless case; only the scope changes.
    let mut child = build_child_machine(
        &ids,
        1,
        Arc::new(RequireApprovalPolicy::new("human approval required")),
    );
    let client: Arc<dyn LlmClient> = child_client.clone();
    let attended_scope = ChildScope {
        llm: ChargingLlmHandler {
            client,
            charged: child_charged.clone(),
        },
        tool: ToolRegistryHandler::new(child_registry.clone()),
        interaction: Some(CountingApproveInteraction {
            served: child_served.clone(),
        }),
    };
    let ctx = root_context(&ids);

    let done = drain(&mut child, child_opening(&ids), &attended_scope, None, &ctx)
        .await
        .expect("attended child turn drains to completion");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);

    // Served locally, exactly once, and the guarded tool ran.
    assert_eq!(child_served.load(Ordering::SeqCst), 1);
    assert_eq!(child_registry.call_count(), 1);

    // The committed conversation: user, assistant tool-use, tool result, answer.
    let conversation = child.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let messages = conversation.turns()[0].messages();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].payload().role, Role::User);
    assert_eq!(messages[1].payload().role, Role::Assistant);
    assert_eq!(messages[2].payload().role, Role::Tool);
    assert_text(messages[3].payload(), "sunny, per get_weather");
}

/// A single step's batch of tool requirements is fulfilled concurrently: the peak
/// number of in-flight tool calls exceeds one. Serial fulfillment would pin it at
/// one. This is migration decision B, observed end to end on a real machine.
#[tokio::test]
async fn batch_requirements_are_fulfilled_concurrently() {
    let ids = SeqIds::new();

    // The model asks for two tool calls at once; no approval, so both become a
    // concurrent NeedTool batch.
    let client = Arc::new(FakeClient::with_chats(vec![
        Ok(tool_use_response(
            vec![
                ("call-a", "get_weather", json!({ "city": "Shanghai" })),
                ("call-b", "get_weather", json!({ "city": "Osaka" })),
            ],
            usage(6, 3),
        )),
        Ok(assistant_response("both looked up", usage(4, 2))),
    ]));
    let registry = Arc::new(FakeToolRegistry::new(vec![
        Ok(tool_response("call-a", "Sunny", ToolStatus::Ok)),
        Ok(tool_response("call-b", "Cloudy", ToolStatus::Ok)),
    ]));

    let mut machine = build_child_machine(&ids, 2, Arc::new(NoApprovalPolicy));

    let peak_in_flight = Arc::new(AtomicUsize::new(0));
    let llm_client: Arc<dyn LlmClient> = client.clone();
    // Drive through a scope whose tool handler records peak concurrency.
    let observing_scope = ObservingScope {
        llm: ChargingLlmHandler {
            client: llm_client,
            charged: Arc::new(AtomicU64::new(0)),
        },
        tool: ConcurrentToolHandler {
            registry: registry.clone(),
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak_in_flight: peak_in_flight.clone(),
        },
    };
    let ctx = root_context(&ids);

    let done = drain(
        &mut machine,
        child_opening(&ids),
        &observing_scope,
        None,
        &ctx,
    )
    .await
    .expect("parallel tool turn drains to completion");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(registry.call_count(), 2);
    assert_eq!(
        peak_in_flight.load(Ordering::SeqCst),
        2,
        "the two-call batch was fulfilled concurrently, not serially"
    );
}

/// Scope carrying the concurrency-observing tool handler.
struct ObservingScope {
    llm: ChargingLlmHandler,
    tool: ConcurrentToolHandler,
}

impl HandlerScope for ObservingScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        Some(&self.llm)
    }

    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&self.tool)
    }
}

/// A cancelled parent context propagates into the derived child: the child drain
/// abandons its first requirement (never-resume) without performing any IO, and
/// nothing is charged to the shared budget.
#[tokio::test]
async fn parent_cancel_propagates_and_abandons_child() {
    let ids = SeqIds::new();

    let child_client = Arc::new(FakeClient::with_chats(approval_round_trip_chats()));
    let child_registry = Arc::new(FakeToolRegistry::new(vec![Ok(tool_response(
        "call-weather",
        "Sunny",
        ToolStatus::Ok,
    ))]));
    let child_charged = Arc::new(AtomicU64::new(0));

    let build_ids = ids.clone();
    let build_client: Arc<dyn LlmClient> = child_client.clone();
    let build_registry = child_registry.clone();
    let build_charged = child_charged.clone();
    let build: BuildChild = Box::new(move || SpawnedChild {
        machine: Box::new(build_child_machine(
            &build_ids,
            1,
            Arc::new(RequireApprovalPolicy::new("human approval required")),
        )),
        scope: Box::new(ChildScope {
            llm: ChargingLlmHandler {
                client: build_client.clone(),
                charged: build_charged.clone(),
            },
            tool: ToolRegistryHandler::new(build_registry.clone()),
            interaction: None,
        }),
        opening: child_opening(&build_ids),
    });

    let spawner = Arc::new(ChildSpawner {
        ids: ids.clone(),
        build,
        summary: "child was cancelled".to_owned(),
    });
    let handler = DrivingSubagentHandler::new(spawner, 4);

    let ctx = root_context(&ids);
    ctx.cancellation().cancel();

    let outer_scope = EmptyScope;
    let mut outer = ScopePop::new(&outer_scope, None);

    let result = handler
        .fulfill(
            &AgentSpecRef(agent_id()),
            &Interaction::question(ids.mint_step_id(), "look up the weather".to_owned()),
            None,
            &mut outer,
            &ctx,
        )
        .await;

    // The child turn closed through its never-resume path.
    assert!(matches!(result, RequirementResult::Subagent(Ok(_))));

    // No IO happened: the guarded tool never ran, the model was never called, and
    // nothing was charged to the shared ledger.
    assert_eq!(child_registry.call_count(), 0);
    assert_eq!(child_client.request_count(), 0);
    assert_eq!(child_charged.load(Ordering::SeqCst), 0);
    assert_eq!(ctx.budget().snapshot().used().tokens(), 0);
}
