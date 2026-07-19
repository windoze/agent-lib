//! End-to-end coverage for the `spawn_agent` bridge adapter (M6-3).
//!
//! The other two required M6-3 validations (a dependency-blocked plan claim and
//! append-only monotonic blackboard offsets) are exercised as library unit tests
//! in `src/agent/collab/tests.rs`. This integration test pins the third: the
//! `spawn_agent` tool call is a *translation*, not an inline tool. A host parses
//! the call into a [`SpawnAgentRequest`], converts it to a
//! [`RequirementKind::NeedSubagent`], and the existing
//! [`DrivingSubagentHandler`] derives and drives the child under the parent's
//! [`RunContext`] — no new orchestration runtime, reusing the milestone-5
//! subagent path (design `external-agent.md` §3.4 / `agent-layer.md` §6.3).
//!
//! The child machine and its scope are deliberately minimal doubles: the child
//! completes on its opening input with no requirements, so the test observes the
//! adapter → requirement → subagent-handler wiring rather than any concrete
//! machine's internals. Every test name contains `tool_adapter` so the milestone
//! selector `cargo test tool_adapter` runs it.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Map, Value, json};

use agent_lib::agent::id::{AgentId, StepId};
use agent_lib::agent::requirement::AgentSpecRef;
use agent_lib::agent::{
    AgentError, AgentInput, AgentMachine, BudgetLimits, HandlerScope, InteractionKind, LoopCursor,
    LoopCursorKind, LoopDoneReason, RequirementKind, RequirementResult, RunContext, RunId,
    ScopePop, StepInput, StepOutcome, SubagentHandler, SubagentOutput, TraceNodeId,
    collab::{SPAWN_AGENT, SpawnAgentRequest},
};
use agent_lib::agent::{DrivingSubagentHandler, SpawnedChild, SubagentSpawner, TurnDone};
use agent_lib::conversation::{MessageId, TurnId};
use agent_lib::model::content::ContentBlock;
use agent_lib::model::message::{Message, Role};
use agent_lib::model::tool::ToolCall;
use uuid::Uuid;

// ----- deterministic id helpers --------------------------------------------

fn run_id() -> RunId {
    RunId::new(Uuid::from_u128(0x6003_A001))
}

fn child_run_id() -> RunId {
    RunId::new(Uuid::from_u128(0x6003_A002))
}

fn spec_id() -> AgentId {
    AgentId::new(Uuid::from_u128(0x6003_A100))
}

fn step_id() -> StepId {
    StepId::new(Uuid::from_u128(0x6003_A200))
}

fn root_context() -> RunContext {
    RunContext::new_root(
        run_id(),
        BudgetLimits::unbounded(),
        TraceNodeId::new("root"),
    )
}

/// A trivial opening input for the child turn.
fn child_opening() -> AgentInput {
    let turn_id = TurnId::new(Uuid::from_u128(0x6003_A300));
    let message_id = MessageId::new(Uuid::from_u128(0x6003_A301));
    let assistant_message_id = MessageId::new(Uuid::from_u128(0x6003_A302));
    let message = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "start".to_owned(),
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
    .expect("valid child opening input")
}

// ----- minimal child machine + scope ---------------------------------------

/// A child machine that completes on its opening input, emitting no
/// requirements. It records that it was stepped so the test can confirm the
/// handler actually drove it.
struct ImmediateChildMachine {
    cursor: LoopCursor,
    steps: Arc<AtomicUsize>,
}

impl ImmediateChildMachine {
    fn new(steps: Arc<AtomicUsize>) -> Self {
        Self {
            cursor: LoopCursor::default(),
            steps,
        }
    }
}

impl AgentMachine for ImmediateChildMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        self.steps.fetch_add(1, Ordering::SeqCst);
        if let StepInput::External(_) = input {
            self.cursor = LoopCursor::done(LoopDoneReason::Completed);
        }
        StepOutcome::new(Vec::new(), Vec::new(), true)
    }

    fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }
}

/// An empty handler scope: the child needs no effect handlers because it emits
/// no requirements.
#[derive(Default)]
struct EmptyScope;

impl HandlerScope for EmptyScope {}

// ----- spawner double ------------------------------------------------------

/// What the [`SubagentSpawner`] was asked to spawn, captured for assertions.
#[derive(Default)]
struct SpawnRecord {
    spec: Mutex<Option<AgentId>>,
    step_id: Mutex<Option<StepId>>,
    prompt: Mutex<Option<String>>,
    schema: Mutex<Option<Value>>,
}

/// A [`SubagentSpawner`] that returns an [`ImmediateChildMachine`] and records
/// the spec/brief/schema the requirement carried into it.
struct RecordingSpawner {
    record: Arc<SpawnRecord>,
    child_steps: Arc<AtomicUsize>,
    summary: String,
}

impl SubagentSpawner for RecordingSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        Ok((child_run_id(), TraceNodeId::new("child")))
    }

    fn spawn(
        &self,
        spec_ref: &AgentSpecRef,
        brief: &agent_lib::agent::interaction::Interaction,
        result_schema: Option<&Value>,
    ) -> Result<SpawnedChild, AgentError> {
        *self.record.spec.lock().unwrap() = Some(spec_ref.0);
        *self.record.step_id.lock().unwrap() = Some(brief.step_id());
        if let InteractionKind::Question { prompt } = brief.kind() {
            *self.record.prompt.lock().unwrap() = Some(prompt.clone());
        }
        *self.record.schema.lock().unwrap() = result_schema.cloned();
        Ok(SpawnedChild {
            machine: Box::new(ImmediateChildMachine::new(self.child_steps.clone())),
            scope: Box::new(EmptyScope),
            opening: child_opening(),
        })
    }

    fn summarize(&self, _done: &TurnDone) -> SubagentOutput {
        SubagentOutput {
            summary: self.summary.clone(),
        }
    }
}

// ----- test ----------------------------------------------------------------

/// A `spawn_agent` tool call is translated to a `NeedSubagent` requirement whose
/// payload the reference [`DrivingSubagentHandler`] then derives and drives to
/// completion — the full "adapter produces NeedSubagent and derives via
/// SubagentHandler" path.
#[tokio::test]
async fn tool_adapter_spawn_agent_derives_child_via_subagent_handler() {
    // 1. The model emits a `spawn_agent` tool call.
    let schema = json!({ "type": "object", "properties": { "verdict": { "type": "string" } } });
    let call = ToolCall {
        id: "provider-call-7".to_owned(),
        name: SPAWN_AGENT.to_owned(),
        input: json!({
            "spec": spec_id().to_string(),
            "brief": "review the proposed patch",
            "result_schema": schema,
        }),
        extra: Map::new(),
    };

    // 2. The host translates it into a structured request, then a requirement.
    let request = SpawnAgentRequest::parse(&call).expect("parse spawn_agent call");
    let requirement = request.into_requirement_kind(step_id());
    let RequirementKind::NeedSubagent {
        spec_ref,
        brief,
        result_schema,
    } = requirement
    else {
        panic!("spawn_agent must translate to NeedSubagent");
    };
    assert_eq!(spec_ref.0, spec_id());

    // 3. The reference subagent handler derives and drives the child under the
    //    parent context, exactly as it would for any `NeedSubagent`.
    let record = Arc::new(SpawnRecord::default());
    let child_steps = Arc::new(AtomicUsize::new(0));
    let spawner = Arc::new(RecordingSpawner {
        record: record.clone(),
        child_steps: child_steps.clone(),
        summary: "child reviewed the patch".to_owned(),
    });
    let handler = DrivingSubagentHandler::new(spawner, 4);

    let scope = EmptyScope;
    let mut outer = ScopePop::new(&scope, None);
    let ctx = root_context();

    let result = handler
        .fulfill(&spec_ref, &brief, result_schema.as_ref(), &mut outer, &ctx)
        .await;

    // 4. The child ran to completion and its output flowed back.
    match result {
        RequirementResult::Subagent(Ok(output)) => {
            assert_eq!(output.summary, "child reviewed the patch");
        }
        other => panic!("expected Subagent(Ok(..)), got {other:?}"),
    }
    assert!(
        child_steps.load(Ordering::SeqCst) >= 1,
        "the child machine should have been driven at least once"
    );

    // 5. The requirement payload the adapter built reached the spawner intact:
    //    the child spec, the brief's owning step, its prompt, and the schema.
    assert_eq!(*record.spec.lock().unwrap(), Some(spec_id()));
    assert_eq!(*record.step_id.lock().unwrap(), Some(step_id()));
    assert_eq!(
        record.prompt.lock().unwrap().as_deref(),
        Some("review the proposed patch")
    );
    assert_eq!(*record.schema.lock().unwrap(), Some(schema));
}

/// The derived child completes with a `Done` cursor, confirming the handler
/// drove a full child turn rather than leaving it parked.
#[tokio::test]
async fn tool_adapter_spawn_agent_child_reaches_done_cursor() {
    let request = SpawnAgentRequest::new(AgentSpecRef(spec_id()), "do the thing", None);
    let RequirementKind::NeedSubagent {
        spec_ref, brief, ..
    } = request.into_requirement_kind(step_id())
    else {
        panic!("expected NeedSubagent");
    };

    // Verify the child machine independently reaches Done on its opening input.
    let mut machine = ImmediateChildMachine::new(Arc::new(AtomicUsize::new(0)));
    let outcome = machine.step(StepInput::External(child_opening()));
    assert!(outcome.is_quiescent());
    assert!(!outcome.has_requirements());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

    // And that driving it through the handler yields a subagent output.
    let spawner = Arc::new(RecordingSpawner {
        record: Arc::new(SpawnRecord::default()),
        child_steps: Arc::new(AtomicUsize::new(0)),
        summary: "done".to_owned(),
    });
    let handler = DrivingSubagentHandler::new(spawner, 4);
    let scope = EmptyScope;
    let mut outer = ScopePop::new(&scope, None);
    let ctx = root_context();

    let result = handler
        .fulfill(&spec_ref, &brief, None, &mut outer, &ctx)
        .await;
    assert!(matches!(result, RequirementResult::Subagent(Ok(_))));
}
