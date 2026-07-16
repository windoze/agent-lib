//! Focused tests for the reference subagent handler (M5-2).
//!
//! Each test exercises exactly one of the four hierarchy guarantees the
//! [`DrivingSubagentHandler`](super::DrivingSubagentHandler) is responsible for
//! (migration doc §7.2 / §7.3):
//!
//! 1. **Scope enforcement / pop from outer.** An attended parent scope serves a
//!    headless child's `NeedInteraction` because the child pops it to the outer
//!    layer instead of re-entering the handler.
//! 2. **Depth guard.** A context already at `max_depth` is refused with a
//!    classified [`AgentError::SubagentDepthExceeded`].
//! 3. **Cancel propagation.** A cancelled parent context makes the child drain
//!    abandon the child's first requirement (never-resume) without fulfilling
//!    it.
//! 4. **Budget inheritance.** Child token charges land on the parent's shared
//!    budget ledger because the child context is derived, not fresh.
//!
//! The machines and scopes here are deliberately minimal doubles: they emit a
//! scripted requirement batch and record how they were driven, so the tests
//! observe the handler's own wiring (derivation, nested drain, guards) rather
//! than any concrete machine's internals.

use super::{DrivingSubagentHandler, SpawnedChild, SubagentSpawner};
use crate::{
    agent::{
        AgentError, AgentInput, AgentMachine, BudgetLimits, HandlerScope, InteractionHandler,
        LlmHandler, LlmStepMode, LoopCursor, LoopCursorKind, LoopDoneReason, Requirement,
        RequirementId, RunContext, RunId, ScopePop, StepInput, StepOutcome, SubagentHandler,
        SubagentOutput, TraceNodeId,
        id::AgentId,
        interaction::{Interaction, InteractionKind, InteractionResponse},
        requirement::{AgentSpecRef, RequirementKind, RequirementKindTag, RequirementResult},
    },
    client::{ChatRequest, Response},
    conversation::{MessageId, TurnId},
    model::{
        content::ContentBlock,
        message::{Message, Role},
    },
};
use async_trait::async_trait;
use serde_json::{Map, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// ----- id / payload helpers -----

fn run_id(suffix: &str) -> RunId {
    format!("018f0d9c-7b6a-7c12-8f31-1234567890{suffix}")
        .parse()
        .expect("valid run id")
}

fn step_id() -> crate::agent::StepId {
    "018f0d9c-7b6a-7c12-8f31-1234567890e9"
        .parse()
        .expect("step id")
}

fn requirement_id(n: u8) -> RequirementId {
    RequirementId::parse_str(&format!("018f0d9c-7b6a-7c12-8f31-1234567890{n:02x}"))
        .expect("requirement id")
}

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890a1"
        .parse()
        .expect("agent id")
}

fn spec_ref() -> AgentSpecRef {
    AgentSpecRef(agent_id())
}

fn brief() -> Interaction {
    Interaction::question(step_id(), "handle this brief".to_owned())
}

fn root_context() -> RunContext {
    RunContext::new_root(
        run_id("a0"),
        BudgetLimits::unbounded(),
        TraceNodeId::new("root"),
    )
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

fn interaction_requirement(n: u8) -> Requirement {
    Requirement::at_root(
        requirement_id(n),
        RequirementKind::NeedInteraction {
            request: Interaction::question(step_id(), "child needs a human".to_owned()),
        },
    )
}

fn llm_requirement(n: u8) -> Requirement {
    Requirement::at_root(
        requirement_id(n),
        RequirementKind::NeedLlm {
            request: chat_request(),
            mode: LlmStepMode::NonStreaming,
        },
    )
}

fn subagent_requirement(n: u8) -> Requirement {
    Requirement::at_root(
        requirement_id(n),
        RequirementKind::NeedSubagent {
            spec_ref: spec_ref(),
            brief: brief(),
            result_schema: None,
        },
    )
}

// ----- scripted machine double -----

/// Shared, post-run-observable record of how a [`ScriptMachine`] was driven.
#[derive(Default)]
struct MachineLog {
    resumed: AtomicUsize,
    abandoned: AtomicUsize,
    last_resume_tag: Mutex<Option<RequirementKindTag>>,
}

/// A machine that emits a fixed requirement batch on its external input, then
/// completes once every requirement is resumed. It records resume / abandon
/// counts so a test can observe whether the child was fulfilled or abandoned.
struct ScriptMachine {
    cursor: LoopCursor,
    batch: Vec<Requirement>,
    outstanding: BTreeSet<RequirementId>,
    log: Arc<MachineLog>,
    idle_on_abandon: bool,
}

impl ScriptMachine {
    fn new(batch: Vec<Requirement>, log: Arc<MachineLog>, idle_on_abandon: bool) -> Self {
        Self {
            cursor: LoopCursor::default(),
            batch,
            outstanding: BTreeSet::new(),
            log,
            idle_on_abandon,
        }
    }
}

impl AgentMachine for ScriptMachine {
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
                self.log.resumed.fetch_add(1, Ordering::SeqCst);
                *self.log.last_resume_tag.lock().expect("resume tag") =
                    Some(resolution.result.tag());
                self.outstanding.remove(&resolution.id);
                if self.outstanding.is_empty() {
                    self.cursor = LoopCursor::done(LoopDoneReason::Completed);
                }
                StepOutcome::new(Vec::new(), Vec::new(), true)
            }
            StepInput::Abandon(_) => {
                self.log.abandoned.fetch_add(1, Ordering::SeqCst);
                if self.idle_on_abandon {
                    self.cursor = LoopCursor::Idle;
                }
                StepOutcome::default()
            }
        }
    }

    fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }
}

// ----- handler doubles -----

/// Counts fulfillments and returns a fixed complete response.
#[derive(Default)]
struct CountingLlmHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LlmHandler for CountingLlmHandler {
    async fn fulfill(
        &self,
        _request: &ChatRequest,
        _mode: LlmStepMode,
        _ctx: &RunContext,
    ) -> RequirementResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        RequirementResult::Llm(Ok(response()))
    }
}

/// Charges a fixed token count against the context, then returns a response.
struct ChargingLlmHandler {
    tokens: u64,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LlmHandler for ChargingLlmHandler {
    async fn fulfill(
        &self,
        _request: &ChatRequest,
        _mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ctx.charge_tokens(self.tokens)
            .expect("charge against shared ledger");
        RequirementResult::Llm(Ok(response()))
    }
}

/// Counts fulfillments and answers any question interaction.
#[derive(Default)]
struct CountingInteractionHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl InteractionHandler for CountingInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = match request.kind() {
            InteractionKind::Question { .. } => InteractionResponse::answer("ok".to_owned()),
            InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
            InteractionKind::Approval { .. } => {
                panic!("test interactions are questions, never approvals")
            }
            InteractionKind::Permission { .. } => {
                panic!("test interactions are questions, never permissions")
            }
        };
        RequirementResult::Interaction(response)
    }
}

/// A scope whose handler families are wired à la carte per test.
#[derive(Default)]
struct MockScope {
    llm_counting: Option<Arc<CountingLlmHandler>>,
    llm_charging: Option<Arc<ChargingLlmHandler>>,
    interaction: Option<Arc<CountingInteractionHandler>>,
    subagent: Option<DrivingSubagentHandler>,
}

impl HandlerScope for MockScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        if let Some(handler) = self.llm_charging.as_deref() {
            return Some(handler as &dyn LlmHandler);
        }
        self.llm_counting
            .as_deref()
            .map(|handler| handler as &dyn LlmHandler)
    }

    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        self.interaction
            .as_deref()
            .map(|handler| handler as &dyn InteractionHandler)
    }

    fn subagent(&self) -> Option<&dyn SubagentHandler> {
        self.subagent
            .as_ref()
            .map(|handler| handler as &dyn SubagentHandler)
    }
}

// ----- spawner double -----

type BuildChild = Box<dyn Fn() -> SpawnedChild + Send + Sync>;

/// A [`SubagentSpawner`] that mints deterministic ids and defers child
/// construction to a stored closure, counting how often each hook is reached.
struct MockSpawner {
    child_run: RunId,
    trace_seq: AtomicUsize,
    build: BuildChild,
    ids_calls: Arc<AtomicUsize>,
    spawn_calls: Arc<AtomicUsize>,
    summary: String,
}

impl SubagentSpawner for MockSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        self.ids_calls.fetch_add(1, Ordering::SeqCst);
        let n = self.trace_seq.fetch_add(1, Ordering::SeqCst);
        Ok((self.child_run, TraceNodeId::new(format!("child-sub-{n}"))))
    }

    fn spawn(
        &self,
        _spec_ref: &AgentSpecRef,
        _brief: &Interaction,
        _result_schema: Option<&serde_json::Value>,
    ) -> Result<SpawnedChild, AgentError> {
        self.spawn_calls.fetch_add(1, Ordering::SeqCst);
        Ok((self.build)())
    }

    fn summarize(&self, _done: &super::TurnDone) -> SubagentOutput {
        SubagentOutput {
            summary: self.summary.clone(),
        }
    }
}

// ----- tests -----

/// §7.3: a headless child's `NeedInteraction` pops past the subagent handler to
/// the attended parent scope, which serves it exactly once; both machines
/// complete and the parent is resumed with a `Subagent` result.
#[tokio::test]
async fn attended_parent_serves_headless_child_interaction_via_pop() {
    let child_log = Arc::new(MachineLog::default());
    let ids_calls = Arc::new(AtomicUsize::new(0));
    let spawn_calls = Arc::new(AtomicUsize::new(0));

    let build_child_log = child_log.clone();
    let build: BuildChild = Box::new(move || SpawnedChild {
        // Child emits a NeedInteraction its own (headless) scope cannot serve.
        machine: Box::new(ScriptMachine::new(
            vec![interaction_requirement(0x11)],
            build_child_log.clone(),
            false,
        )),
        scope: Box::new(MockScope::default()),
        opening: external_input(),
    });

    let spawner = Arc::new(MockSpawner {
        child_run: run_id("b1"),
        trace_seq: AtomicUsize::new(0),
        build,
        ids_calls: ids_calls.clone(),
        spawn_calls: spawn_calls.clone(),
        summary: "child summary".to_owned(),
    });

    // The attended parent scope handles both the subagent and the interaction.
    let parent_interaction = Arc::new(CountingInteractionHandler::default());
    let parent_scope = MockScope {
        interaction: Some(parent_interaction.clone()),
        subagent: Some(DrivingSubagentHandler::new(spawner, 4)),
        ..MockScope::default()
    };

    let parent_log = Arc::new(MachineLog::default());
    let mut parent_machine =
        ScriptMachine::new(vec![subagent_requirement(0x21)], parent_log.clone(), false);
    let ctx = root_context();

    let done = super::drain(
        &mut parent_machine,
        external_input(),
        &parent_scope,
        None,
        &ctx,
    )
    .await
    .expect("parent drain completes");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    // The child's interaction popped to the parent and was served exactly once.
    assert_eq!(parent_interaction.calls.load(Ordering::SeqCst), 1);
    // The child ran to completion (one resume: the popped interaction result).
    assert_eq!(child_log.resumed.load(Ordering::SeqCst), 1);
    assert_eq!(
        *child_log.last_resume_tag.lock().expect("child resume tag"),
        Some(RequirementKindTag::Interaction)
    );
    // The parent was resumed with the driven subagent's output.
    assert_eq!(
        *parent_log
            .last_resume_tag
            .lock()
            .expect("parent resume tag"),
        Some(RequirementKindTag::Subagent)
    );
    // The handler was actually exercised (derived + spawned once).
    assert_eq!(ids_calls.load(Ordering::SeqCst), 1);
    assert_eq!(spawn_calls.load(Ordering::SeqCst), 1);
}

/// §7.2: a context already at `max_depth` is refused before any derivation or
/// spawn, with a classified [`AgentError::SubagentDepthExceeded`].
#[tokio::test]
async fn depth_guard_refuses_at_limit_without_spawning() {
    let ids_calls = Arc::new(AtomicUsize::new(0));
    let spawn_calls = Arc::new(AtomicUsize::new(0));
    let build: BuildChild = Box::new(|| panic!("spawn must not run once the depth guard trips"));

    let spawner = Arc::new(MockSpawner {
        child_run: run_id("b2"),
        trace_seq: AtomicUsize::new(0),
        build,
        ids_calls: ids_calls.clone(),
        spawn_calls: spawn_calls.clone(),
        summary: "unused".to_owned(),
    });
    let handler = DrivingSubagentHandler::new(spawner, 1);

    // A depth-1 context invoked against a max_depth of 1 must be refused.
    let root = root_context();
    let deep_ctx = root
        .derive_child(run_id("b3"), TraceNodeId::new("depth-1"))
        .expect("derive depth-1 context");
    assert_eq!(deep_ctx.depth(), 1);

    let empty = MockScope::default();
    let mut outer = ScopePop::new(&empty, None);

    let result = handler
        .fulfill(&spec_ref(), &brief(), None, &mut outer, &deep_ctx)
        .await;

    match result {
        RequirementResult::Subagent(Err(AgentError::SubagentDepthExceeded { limit, depth })) => {
            assert_eq!(limit, 1);
            assert_eq!(depth, 1);
        }
        other => panic!("expected a SubagentDepthExceeded result, got {other:?}"),
    }
    // The guard ran before any derivation or spawn.
    assert_eq!(ids_calls.load(Ordering::SeqCst), 0);
    assert_eq!(spawn_calls.load(Ordering::SeqCst), 0);
}

/// §7: a cancelled parent context propagates to the derived child, so the child
/// drain abandons (never-resumes) the child's first requirement without ever
/// invoking a child handler.
#[tokio::test]
async fn parent_cancel_propagates_and_abandons_child() {
    let child_log = Arc::new(MachineLog::default());
    let child_llm_calls = Arc::new(AtomicUsize::new(0));

    let build_child_log = child_log.clone();
    let build_llm_calls = child_llm_calls.clone();
    let build: BuildChild = Box::new(move || SpawnedChild {
        machine: Box::new(ScriptMachine::new(
            vec![llm_requirement(0x31)],
            build_child_log.clone(),
            true,
        )),
        scope: Box::new(MockScope {
            llm_counting: Some(Arc::new(CountingLlmHandler {
                calls: build_llm_calls.clone(),
            })),
            ..MockScope::default()
        }),
        opening: external_input(),
    });

    let spawner = Arc::new(MockSpawner {
        child_run: run_id("b4"),
        trace_seq: AtomicUsize::new(0),
        build,
        ids_calls: Arc::new(AtomicUsize::new(0)),
        spawn_calls: Arc::new(AtomicUsize::new(0)),
        summary: "abandoned child".to_owned(),
    });
    let handler = DrivingSubagentHandler::new(spawner, 4);

    // Cancel the parent context before fulfilling; derivation inherits it.
    let ctx = root_context();
    ctx.cancellation().cancel();

    let empty = MockScope::default();
    let mut outer = ScopePop::new(&empty, None);

    let result = handler
        .fulfill(&spec_ref(), &brief(), None, &mut outer, &ctx)
        .await;

    // The turn closed (drain returned Ok) via the child's never-resume path.
    assert!(matches!(result, RequirementResult::Subagent(Ok(_))));
    // The child's first requirement was abandoned, never fulfilled or resumed.
    assert_eq!(child_log.abandoned.load(Ordering::SeqCst), 1);
    assert_eq!(child_log.resumed.load(Ordering::SeqCst), 0);
    assert_eq!(child_llm_calls.load(Ordering::SeqCst), 0);
}

/// A child's token charge lands on the parent's shared budget ledger, proving
/// the child context is derived (budget ↕) rather than created fresh.
#[tokio::test]
async fn child_token_charge_counts_against_parent_budget() {
    const CHILD_TOKENS: u64 = 42;

    let child_log = Arc::new(MachineLog::default());
    let child_llm_calls = Arc::new(AtomicUsize::new(0));

    let build_child_log = child_log.clone();
    let build_llm_calls = child_llm_calls.clone();
    let build: BuildChild = Box::new(move || SpawnedChild {
        machine: Box::new(ScriptMachine::new(
            vec![llm_requirement(0x41)],
            build_child_log.clone(),
            false,
        )),
        scope: Box::new(MockScope {
            llm_charging: Some(Arc::new(ChargingLlmHandler {
                tokens: CHILD_TOKENS,
                calls: build_llm_calls.clone(),
            })),
            ..MockScope::default()
        }),
        opening: external_input(),
    });

    let spawner = Arc::new(MockSpawner {
        child_run: run_id("b5"),
        trace_seq: AtomicUsize::new(0),
        build,
        ids_calls: Arc::new(AtomicUsize::new(0)),
        spawn_calls: Arc::new(AtomicUsize::new(0)),
        summary: "child charged tokens".to_owned(),
    });
    let handler = DrivingSubagentHandler::new(spawner, 4);

    let ctx = root_context();
    assert_eq!(ctx.budget().snapshot().used().tokens(), 0);

    let empty = MockScope::default();
    let mut outer = ScopePop::new(&empty, None);

    let result = handler
        .fulfill(&spec_ref(), &brief(), None, &mut outer, &ctx)
        .await;

    assert!(matches!(result, RequirementResult::Subagent(Ok(_))));
    // The child fulfilled its one LLM requirement and completed.
    assert_eq!(child_llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(child_log.resumed.load(Ordering::SeqCst), 1);
    // The child's charge is visible on the parent context's shared ledger.
    assert_eq!(ctx.budget().snapshot().used().tokens(), CHILD_TOKENS);
}
