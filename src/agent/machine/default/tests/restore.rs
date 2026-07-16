//! Scratch-restore tests for [`DefaultAgentMachine`](super::super::DefaultAgentMachine).
//!
//! These exercise
//! [`rebuild_scratch_from_state`](super::super::DefaultAgentMachine::rebuild_scratch_from_state)
//! (M2-3, effect-refine doc §3.4): the mid-turn [`TurnScratch`] is intentionally
//! not serialized, so a machine handed a persisted [`AgentState`] re-derives the
//! scratch from the durable Conversation pending transaction and reconfiguration
//! queue. Each test drives a live machine to a parked cursor, drops its scratch
//! by taking [`into_state`](super::super::DefaultAgentMachine::into_state),
//! rebuilds a fresh machine's scratch from that state, and asserts the rebuilt
//! scratch is aligned to the cursor phase — plus a full resume round-trip for the
//! only fully reconstructable park, the during-turn `AwaitingReconfig` boundary.

use super::super::{PendingReconfig, TurnScratch};
use super::*;
use crate::agent::{
    ApprovalRequirement, NoApprovalPolicy, ReconfigRequest, ToolApprovalPolicy, ToolExecutionIds,
    ToolRuntimeError, ToolSetId,
};
use crate::conversation::ToolCallId;
use crate::model::tool::{Tool, ToolCall, ToolResponse, ToolStatus};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

// Disjoint id bases so restored ids never collide with the base fixtures.
const REQUIREMENT_BASE: u128 = 0x1000_0000;
const TOOL_CALL_BASE: u128 = 0x1100_0000;
const RESULT_MESSAGE_BASE: u128 = 0x1200_0000;
const CONTINUATION_MESSAGE_BASE: u128 = 0x1300_0000;
const CONTINUATION_STEP_BASE: u128 = 0x1400_0000;

/// Requirement id source handing out distinct ids from a fixed pool.
#[derive(Debug)]
struct RestoreRequirementIds {
    ids: Vec<RequirementId>,
    cursor: AtomicUsize,
}

impl RestoreRequirementIds {
    fn new() -> Self {
        Self {
            ids: (0..32u128)
                .map(|index| RequirementId::new(Uuid::from_u128(REQUIREMENT_BASE + index)))
                .collect(),
            cursor: AtomicUsize::new(0),
        }
    }
}

impl RequirementIds for RestoreRequirementIds {
    fn next_requirement_id(
        &self,
        kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        let index = self.cursor.fetch_add(1, Ordering::SeqCst);
        self.ids
            .get(index)
            .copied()
            .ok_or(RequirementError::IdUnavailable { kind: kind_tag })
    }
}

/// Host id source for a tool phase, drawing ids from fixed pools in call order.
#[derive(Debug)]
struct RestoreToolIds {
    tool_call_ids: Vec<ToolCallId>,
    result_message_ids: Vec<MessageId>,
    assistant_message_ids: Vec<MessageId>,
    step_ids: Vec<StepId>,
    tool_call_cursor: AtomicUsize,
    result_cursor: AtomicUsize,
    assistant_cursor: AtomicUsize,
    step_cursor: AtomicUsize,
}

impl RestoreToolIds {
    fn new() -> Self {
        Self {
            tool_call_ids: (0..8u128)
                .map(|index| ToolCallId::new(Uuid::from_u128(TOOL_CALL_BASE + index)))
                .collect(),
            result_message_ids: (0..8u128)
                .map(|index| MessageId::new(Uuid::from_u128(RESULT_MESSAGE_BASE + index)))
                .collect(),
            assistant_message_ids: (0..8u128)
                .map(|index| MessageId::new(Uuid::from_u128(CONTINUATION_MESSAGE_BASE + index)))
                .collect(),
            step_ids: (0..8u128)
                .map(|index| StepId::new(Uuid::from_u128(CONTINUATION_STEP_BASE + index)))
                .collect(),
            tool_call_cursor: AtomicUsize::new(0),
            result_cursor: AtomicUsize::new(0),
            assistant_cursor: AtomicUsize::new(0),
            step_cursor: AtomicUsize::new(0),
        }
    }
}

impl ToolExecutionIds for RestoreToolIds {
    fn tool_call_id(&self, call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        let index = self.tool_call_cursor.fetch_add(1, Ordering::SeqCst);
        self.tool_call_ids
            .get(index)
            .copied()
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: format!("tool call `{}`", call.id),
            })
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        let index = self.result_cursor.fetch_add(1, Ordering::SeqCst);
        self.result_message_ids
            .get(index)
            .copied()
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: format!("tool result `{}`", call.id),
            })
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        let index = self.assistant_cursor.fetch_add(1, Ordering::SeqCst);
        self.assistant_message_ids
            .get(index)
            .copied()
            .ok_or(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation message".to_owned(),
            })
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        let index = self.step_cursor.fetch_add(1, Ordering::SeqCst);
        self.step_ids
            .get(index)
            .copied()
            .ok_or(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation step".to_owned(),
            })
    }
}

/// Approval policy that pauses for every tool call (to reach `AwaitingApproval`).
#[derive(Debug)]
struct AlwaysApprove;

impl ToolApprovalPolicy for AlwaysApprove {
    fn approval_requirement(&self, _call_id: ToolCallId, _call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::RequireApproval { reason: None }
    }
}

fn calendar_tool() -> Tool {
    Tool {
        name: "read_calendar".to_owned(),
        description: "Read calendar availability.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": { "day": { "type": "string" } },
            "required": ["day"]
        }),
    }
}

fn replacement_tool_set() -> ToolSetRef {
    let id: ToolSetId = "018f0d9c-7b6a-7c12-8f31-1234567890f1"
        .parse()
        .expect("replacement tool set id");
    ToolSetRef::new(id, vec![calendar_tool()])
}

/// A successful tool response for the given provider call id.
fn tool_ok(provider_id: &str, text: &str) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_id.to_owned(),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
            extra: Map::new(),
        }],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

/// Builds a machine over the default spec wired with a scripted id source and the
/// given approval policy, ready to drive a tool turn.
fn tool_machine(policy: Arc<dyn ToolApprovalPolicy>) -> DefaultAgentMachine {
    DefaultAgentMachine::new(
        state(),
        LlmStepMode::NonStreaming,
        Arc::new(RestoreRequirementIds::new()),
    )
    .with_tool_execution_ids(Arc::new(RestoreToolIds::new()))
    .with_approval_policy(policy)
}

/// Builds a machine over a persisted `state` with no scratch, mirroring a host
/// that reconstructs a machine from a serialized [`AgentState`].
fn restored_machine(state: AgentState) -> DefaultAgentMachine {
    DefaultAgentMachine::new(
        state,
        LlmStepMode::NonStreaming,
        Arc::new(RestoreRequirementIds::new()),
    )
    .with_tool_execution_ids(Arc::new(RestoreToolIds::new()))
}

/// Drives `driven` to its parked cursor, drops its live scratch by taking the
/// state, and asserts a fresh machine rebuilds a cursor-aligned scratch from it.
fn assert_rebuild_aligns_scratch(driven: DefaultAgentMachine) {
    let cursor_kind = driven.cursor().kind();
    let state = driven.into_state();

    let mut restored = restored_machine(state);
    // A freshly constructed machine starts with no scratch, regardless of cursor.
    assert!(matches!(restored.scratch, TurnScratch::None));

    restored
        .rebuild_scratch_from_state()
        .expect("rebuild scratch from state");

    assert!(
        restored.scratch.matches_cursor(restored.cursor()),
        "rebuilt scratch must be aligned to the {cursor_kind:?} cursor phase",
    );
}

/// Idle rests with no scratch: rebuild keeps it `None`.
#[test]
fn rebuild_at_idle_yields_no_scratch() {
    assert_rebuild_aligns_scratch(machine(LlmStepMode::NonStreaming));
}

/// A committed text turn settles at `Done`: rebuild keeps the scratch `None`.
#[test]
fn rebuild_at_done_yields_no_scratch() {
    let mut machine = machine(LlmStepMode::NonStreaming);
    let id = park_on_need_llm(&mut machine);
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        id,
        RequirementResult::Llm(Ok(text_response("hi"))),
    )));
    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    assert_rebuild_aligns_scratch(machine);
}

/// A tool batch parks on `AwaitingTool`: rebuild reconstructs the in-flight phase
/// marker (anchored on the frozen tool-use assistant), aligned to the cursor.
#[test]
fn rebuild_at_awaiting_tool_aligns_in_flight_scratch() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        llm_id,
        RequirementResult::Llm(Ok(tool_use_response())),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    assert!(!outcome.requirements.is_empty());
    assert_rebuild_aligns_scratch(machine);
}

/// A call needing approval parks on `AwaitingApproval`: rebuild reconstructs the
/// in-flight phase marker aligned to the cursor.
#[test]
fn rebuild_at_awaiting_approval_aligns_in_flight_scratch() {
    let mut machine = tool_machine(Arc::new(AlwaysApprove));
    let llm_id = park_on_need_llm(&mut machine);
    let _ = machine.step(StepInput::resume(RequirementResolution::new(
        llm_id,
        RequirementResult::Llm(Ok(tool_use_response())),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingApproval);
    assert_rebuild_aligns_scratch(machine);
}

/// A continuation LLM step after a tool result parks on `StreamingStep` with a
/// frozen assistant to anchor: rebuild reconstructs the in-flight phase marker.
#[test]
fn rebuild_at_continuation_streaming_step_aligns_in_flight_scratch() {
    let mut machine = tool_machine(Arc::new(NoApprovalPolicy));
    let llm_id = park_on_need_llm(&mut machine);
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        llm_id,
        RequirementResult::Llm(Ok(tool_use_response())),
    )));
    let RequirementKind::NeedTool { .. } = &outcome.requirements[0].kind else {
        panic!("expected a NeedTool requirement");
    };
    let tool_id = outcome.requirements[0].id;

    // Folding the tool result asks the model to continue: a fresh `StreamingStep`
    // whose outstanding assistant is not yet frozen, but the prior tool-use is.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_id,
        RequirementResult::Tool(Ok(tool_ok("call-weather", "Sunny"))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(!outcome.requirements.is_empty());
    assert_rebuild_aligns_scratch(machine);
}

/// The during-turn `AwaitingReconfig` boundary is the fully reconstructable park:
/// a machine rebuilt from the persisted queue reconstructs the deferred
/// [`PendingReconfig::Commit`], and a `Reconfig(Ok)` resume commits the turn
/// exactly as the original machine would have — a complete restore round-trip.
#[test]
fn rebuild_at_awaiting_reconfig_round_trips_the_commit() {
    let replacement = replacement_tool_set();

    // Drive a first machine to the deferred reconfiguration boundary.
    let mut driven = DefaultAgentMachine::new(
        state(),
        LlmStepMode::NonStreaming,
        Arc::new(RestoreRequirementIds::new()),
    );
    let outcome = driven.step(StepInput::external(user_input()));
    let llm_id = outcome.requirements[0].id;
    driven
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("Use calendar context.".to_owned()),
            0,
        ))
        .expect("system overlay reconfig queued");
    driven
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("tool set reconfig queued");
    let outcome = driven.step(StepInput::resume(RequirementResolution::new(
        llm_id,
        RequirementResult::Llm(Ok(text_response("first"))),
    )));
    assert_eq!(driven.cursor().kind(), LoopCursorKind::AwaitingReconfig);
    assert_eq!(outcome.requirements.len(), 1);

    // Drop the live scratch and rebuild a fresh machine from the persisted state.
    let state = driven.into_state();
    let mut restored = restored_machine(state);
    assert!(matches!(restored.scratch, TurnScratch::None));

    restored
        .rebuild_scratch_from_state()
        .expect("rebuild scratch from state");

    // The reconstructed scratch is the deferred during-turn commit, aligned to
    // the cursor and re-rendering the same boundary records from the queue.
    match &restored.scratch {
        TurnScratch::Reconfig(PendingReconfig::Commit { records, .. }) => {
            assert_eq!(records.len(), 2);
        }
        other => panic!("expected a reconstructed commit reconfig, got {other:?}"),
    }
    assert!(restored.scratch.matches_cursor(restored.cursor()));
    assert!(!restored.state().queued_reconfigs().is_empty());
    assert_ne!(restored.state().current_tool_set(), &replacement);

    // Resuming the registry effect commits the deferred turn and applies both
    // queued reconfigurations, just like the un-restored machine would.
    let reconfig_id = restored.cursor().pending_requirement_ids()[0];
    let outcome = restored.step(StepInput::resume(RequirementResolution::new(
        reconfig_id,
        RequirementResult::Reconfig(Ok(())),
    )));
    assert!(outcome.is_quiescent());
    assert_eq!(restored.cursor().kind(), LoopCursorKind::Done);

    let Notification::StepBoundary(boundary) = &outcome.notifications[0] else {
        panic!("expected a step-boundary notification");
    };
    let records = boundary
        .metadata()
        .get("reconfigs")
        .and_then(Value::as_array)
        .expect("reconfig metadata records");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["kind"], json!("set_system_prompt_overlay"));
    assert_eq!(records[1]["kind"], json!("replace_tool_set"));

    assert!(restored.state().queued_reconfigs().is_empty());
    assert_eq!(
        restored.state().system_prompt_overlay(),
        Some("Use calendar context.")
    );
    assert_eq!(restored.state().current_tool_set(), &replacement);
}
