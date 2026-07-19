//! Turn-boundary reconfiguration tests for
//! [`DefaultAgentMachine`](super::super::DefaultAgentMachine).
//!
//! These cover the turn-boundary reconfig behavior on the sans-io machine
//! (the reconfig integration coverage that predates the machine). A
//! reconfiguration queued mid-turn is deferred to the turn boundary; when it
//! changes the active tool set the machine parks on a
//! [`RequirementKind::NeedReconfigRegistry`] effect and only commits once a
//! [`RequirementResult::Reconfig`] confirms the driver swapped the registry.
//! The machine is single-turn (it settles at [`LoopCursorKind::Done`]), so the
//! "next request changes" observation lives in the reference-driver tests;
//! here we assert the applied state (`current_tool_set`, overlay, queue) and
//! the boundary metadata that a following request would read.

use super::*;
use crate::agent::{
    AgentErrorKind, DeclaredOnlyToolRegistry, ReconfigRequest, StaticToolRegistryResolver,
    ToolExecutionIds, ToolRuntimeError, ToolSetId,
};
use crate::conversation::ToolCallId;
use crate::model::tool::{Tool, ToolCall, ToolResponse, ToolStatus};
use serde_json::Value;
use std::sync::Mutex;
use uuid::Uuid;

// Disjoint id bases so no two host-supplied ids collide within one test.
const REQUIREMENT_BASE: u128 = 0x0A00_0000;
const TOOL_CALL_BASE: u128 = 0x0B00_0000;
const RESULT_MESSAGE_BASE: u128 = 0x0C00_0000;
const CONTINUATION_MESSAGE_BASE: u128 = 0x0D00_0000;
const CONTINUATION_STEP_BASE: u128 = 0x0E00_0000;

/// Requirement id source handing out distinct ids from a fixed pool.
#[derive(Debug)]
struct ScriptedRequirementIds {
    ids: Vec<RequirementId>,
    cursor: Mutex<usize>,
}

impl ScriptedRequirementIds {
    fn new() -> Self {
        Self {
            ids: (0..32u128)
                .map(|index| RequirementId::new(Uuid::from_u128(REQUIREMENT_BASE + index)))
                .collect(),
            cursor: Mutex::new(0),
        }
    }
}

impl RequirementIds for ScriptedRequirementIds {
    fn next_requirement_id(
        &self,
        kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        let mut cursor = self.cursor.lock().expect("requirement id cursor");
        let index = *cursor;
        *cursor += 1;
        self.ids
            .get(index)
            .copied()
            .ok_or(RequirementError::IdUnavailable { kind: kind_tag })
    }
}

/// Host id source for a tool phase, drawing ids from fixed pools in call order.
#[derive(Debug)]
struct ScriptedToolIds {
    tool_call_ids: Mutex<Vec<ToolCallId>>,
    result_message_ids: Mutex<Vec<MessageId>>,
    assistant_message_ids: Mutex<Vec<MessageId>>,
    step_ids: Mutex<Vec<StepId>>,
}

impl ScriptedToolIds {
    fn new() -> Self {
        Self {
            tool_call_ids: Mutex::new(
                (0..4u128)
                    .rev()
                    .map(|index| ToolCallId::new(Uuid::from_u128(TOOL_CALL_BASE + index)))
                    .collect(),
            ),
            result_message_ids: Mutex::new(
                (0..4u128)
                    .rev()
                    .map(|index| MessageId::new(Uuid::from_u128(RESULT_MESSAGE_BASE + index)))
                    .collect(),
            ),
            assistant_message_ids: Mutex::new(
                (0..4u128)
                    .rev()
                    .map(|index| MessageId::new(Uuid::from_u128(CONTINUATION_MESSAGE_BASE + index)))
                    .collect(),
            ),
            step_ids: Mutex::new(
                (0..4u128)
                    .rev()
                    .map(|index| StepId::new(Uuid::from_u128(CONTINUATION_STEP_BASE + index)))
                    .collect(),
            ),
        }
    }
}

impl ToolExecutionIds for ScriptedToolIds {
    fn tool_call_id(&self, call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        self.tool_call_ids
            .lock()
            .expect("tool call id pool")
            .pop()
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: format!("tool call `{}`", call.id),
            })
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        self.result_message_ids
            .lock()
            .expect("tool result id pool")
            .pop()
            .ok_or_else(|| ToolRuntimeError::IdUnavailable {
                purpose: format!("tool result `{}`", call.id),
            })
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        self.assistant_message_ids
            .lock()
            .expect("assistant id pool")
            .pop()
            .ok_or(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation message".to_owned(),
            })
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        self.step_ids
            .lock()
            .expect("step id pool")
            .pop()
            .ok_or(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation step".to_owned(),
            })
    }
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

fn replacement_tool_set_id() -> ToolSetId {
    "018f0d9c-7b6a-7c12-8f31-1234567890c1"
        .parse()
        .expect("replacement tool set id")
}

fn replacement_tool_set() -> ToolSetRef {
    ToolSetRef::new(replacement_tool_set_id(), vec![calendar_tool()])
}

fn spec_with_weather() -> AgentSpec {
    AgentSpec::new(
        agent_id(),
        WorktreeRef::new("/repo/agent-lib"),
        Some("Spec fallback system.".to_owned()),
        ToolSetRef::new(tool_set_id(), vec![weather_tool()]),
        ModelRef::new("gpt-5.5", nz(512), Some(0.1), None),
        LoopPolicy::new(nz(8), nz(1), ToolFailurePolicy::ReturnErrorToModel),
    )
}

fn state_with_weather() -> AgentState {
    AgentState::new(
        spec_with_weather(),
        crate::conversation::Conversation::new(
            conversation_id(),
            crate::conversation::ConversationConfig::new(Some("Conversation system.".to_owned())),
        ),
    )
}

fn reconfig_machine(state: AgentState) -> DefaultAgentMachine {
    DefaultAgentMachine::new(
        state,
        LlmStepMode::NonStreaming,
        Arc::new(ScriptedRequirementIds::new()),
    )
    .with_tool_execution_ids(Arc::new(ScriptedToolIds::new()))
}

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

fn need_llm_request(outcome: &StepOutcome, index: usize) -> &crate::client::ChatRequest {
    let RequirementKind::NeedLlm { request, .. } = &outcome.requirements[index].kind else {
        panic!(
            "expected a NeedLlm requirement, got {:?}",
            outcome.requirements[index].kind
        );
    };
    request
}

fn reconfig_records(outcome: &StepOutcome) -> Vec<Value> {
    let Notification::StepBoundary(boundary) = &outcome.notifications[0] else {
        panic!("expected a step-boundary notification");
    };
    boundary
        .metadata()
        .get("reconfigs")
        .and_then(Value::as_array)
        .cloned()
        .expect("reconfig metadata records")
}

type StepOutcome = crate::agent::StepOutcome;

/// A reconfiguration queued *during* a text turn is deferred to the turn
/// boundary. Because it changes the active tool set, the commit is held behind
/// a `NeedReconfigRegistry` effect; the confirming `Reconfig(Ok)` applies both
/// queued requests, commits the turn with `reconfigs` metadata, and leaves the
/// state advertising the new tool set + overlay a following request would read.
#[test]
fn reconfig_queued_during_text_turn_defers_commit_behind_registry_effect() {
    let mut machine = reconfig_machine(state());
    let replacement = replacement_tool_set();

    // Turn opens with the spec's (empty) tool set and no overlay.
    let outcome = machine.step(StepInput::external(user_input()));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    let llm_id = outcome.requirements[0].id;
    let request = need_llm_request(&outcome, 0);
    assert!(request.tools.is_empty());
    assert_eq!(request.system.as_deref(), Some("Conversation system."));

    // Two reconfigurations queued mid-turn (between steps).
    machine
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("Use calendar context.".to_owned()),
            0,
        ))
        .expect("system overlay reconfig queued");
    machine
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("tool set reconfig queued");

    // Folding the text response defers the commit behind a registry effect.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        llm_id,
        RequirementResult::Llm(Ok(text_response("first"))),
    )));
    assert!(outcome.is_quiescent());
    assert!(outcome.notifications.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingReconfig);
    assert_eq!(outcome.requirements.len(), 1);
    let RequirementKind::NeedReconfigRegistry { tool_set } = &outcome.requirements[0].kind else {
        panic!(
            "expected a NeedReconfigRegistry requirement, got {:?}",
            outcome.requirements[0].kind
        );
    };
    assert_eq!(tool_set, &replacement);
    let reconfig_id = outcome.requirements[0].id;
    // Nothing is applied yet: the reconfiguration is still queued.
    assert!(!machine.state().queued_reconfigs().is_empty());
    assert_ne!(machine.state().current_tool_set(), &replacement);

    // The driver confirms the swap; the deferred turn commits with metadata.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        reconfig_id,
        RequirementResult::Reconfig(Ok(())),
    )));
    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

    let records = reconfig_records(&outcome);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["status"], json!("applied"));
    assert_eq!(records[0]["kind"], json!("set_system_prompt_overlay"));
    assert_eq!(records[1]["kind"], json!("replace_tool_set"));

    // The applied state now advertises the new tool set + overlay.
    assert!(machine.state().queued_reconfigs().is_empty());
    assert_eq!(
        machine.state().system_prompt_overlay(),
        Some("Use calendar context.")
    );
    assert_eq!(machine.state().system_prompt_overlay_version(), 1);
    assert_eq!(machine.state().current_tool_set(), &replacement);
}

/// Abandoning the registry requirement of a during-turn reconfiguration no
/// longer discards the folded text turn (M4-4): the pending turn sits at
/// `ReadyToCommit`, where `ResumeTurn` is not a legal closure, so the machine
/// commits the text turn to preserve it, settles back to `Idle`, and leaves the
/// abandoned reconfiguration queued for the next turn boundary.
#[test]
fn abandon_during_turn_reconfig_commits_the_folded_text_turn() {
    let mut machine = reconfig_machine(state());
    let replacement = replacement_tool_set();

    let outcome = machine.step(StepInput::external(user_input()));
    let llm_id = outcome.requirements[0].id;
    machine
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("tool set reconfig queued");

    // The folded text response parks the commit behind a registry effect.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        llm_id,
        RequirementResult::Llm(Ok(text_response("first"))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingReconfig);
    assert_eq!(outcome.requirements.len(), 1);
    let reconfig_id = outcome.requirements[0].id;

    // Abandoning the registry requirement preserves the text turn by committing
    // it, and the machine settles back to a feedable Idle.
    let outcome = machine.step(StepInput::abandon(reconfig_id));
    assert!(outcome.is_quiescent());
    assert!(!outcome.is_rejected());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);

    // The folded text response is committed, not discarded.
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 2);
    assert_text(turn.messages()[0].payload(), "hello");
    assert_text(turn.messages()[1].payload(), "first");

    // The abandoned reconfiguration was never applied: it stays queued and the
    // tool set is unchanged.
    assert!(!machine.state().queued_reconfigs().is_empty());
    assert_ne!(machine.state().current_tool_set(), &replacement);

    // The next turn re-raises the deferred reconfiguration at its boundary.
    let outcome = machine.step(StepInput::external(second_user_input()));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingReconfig);
    assert_eq!(outcome.requirements.len(), 1);
    let RequirementKind::NeedReconfigRegistry { tool_set } = &outcome.requirements[0].kind else {
        panic!("the queued reconfiguration must re-emit its registry effect");
    };
    assert_eq!(tool_set, &replacement);
}

/// A reconfiguration queued while a tool turn is in flight does not disturb the
/// current turn: the post-tool assistant continuation still renders the old
/// tool set. The change only lands at the turn boundary, again behind the
/// registry effect, leaving the state on the new tool set for the next turn.
#[test]
fn reconfig_during_tool_turn_keeps_current_turn_tools() {
    let mut machine = reconfig_machine(state_with_weather());
    let replacement = replacement_tool_set();

    // Turn opens advertising the weather tool.
    let outcome = machine.step(StepInput::external(user_input()));
    let llm_id = outcome.requirements[0].id;
    assert_eq!(need_llm_request(&outcome, 0).tools, vec![weather_tool()]);

    // The model asks for a tool; the machine opens a tool phase.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        llm_id,
        RequirementResult::Llm(Ok(tool_use_response())),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    let RequirementKind::NeedTool { .. } = &outcome.requirements[0].kind else {
        panic!("expected a NeedTool requirement");
    };
    let tool_id = outcome.requirements[0].id;

    // The tool set is reconfigured *during* the pending tool turn.
    machine
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("replacement queued during pending turn");

    // The tool result folds back and the machine asks the model to continue.
    // The continuation still renders the *current* turn's (old) tool set.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        tool_id,
        RequirementResult::Tool(Ok(tool_ok("call-weather", "Sunny"))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    let continuation_id = outcome.requirements[0].id;
    assert_eq!(
        need_llm_request(&outcome, 0).tools,
        vec![weather_tool()],
        "the assistant continuation in the same turn keeps the old tool set"
    );

    // The final text response reaches the turn boundary: the queued tool-set
    // change parks on the registry effect before committing.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        continuation_id,
        RequirementResult::Llm(Ok(text_response("used old registry"))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingReconfig);
    let RequirementKind::NeedReconfigRegistry { tool_set } = &outcome.requirements[0].kind else {
        panic!("expected a NeedReconfigRegistry requirement");
    };
    assert_eq!(tool_set, &replacement);
    let reconfig_id = outcome.requirements[0].id;

    // The driver confirms the swap; the turn commits and the new tool set lands.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        reconfig_id,
        RequirementResult::Reconfig(Ok(())),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    let records = reconfig_records(&outcome);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["kind"], json!("replace_tool_set"));
    assert_eq!(machine.state().current_tool_set(), &replacement);
}

/// H-STATE-5 / M4-2: while the machine is parked on `AwaitingReconfig`, a new
/// reconfiguration is rejected instead of silently dropped by the resume's
/// queue clear. The parked queue stays exactly the set the parked application
/// was planned from, and the rejected request can be resubmitted once the
/// outstanding requirement resolves.
#[test]
fn reconfigure_during_awaiting_reconfig_is_rejected_and_can_be_retried() {
    let mut machine = reconfig_machine(state());
    let replacement = replacement_tool_set();

    // A tool-set change queued mid-turn parks the commit behind the registry
    // effect.
    let outcome = machine.step(StepInput::external(user_input()));
    let llm_id = outcome.requirements[0].id;
    machine
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("tool set reconfig queued");
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        llm_id,
        RequirementResult::Llm(Ok(text_response("first"))),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingReconfig);
    let reconfig_id = outcome.requirements[0].id;

    // A reconfigure arriving during the park is rejected, never queued.
    let rejected = machine
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("late overlay".to_owned()),
            0,
        ))
        .expect_err("reconfigure during AwaitingReconfig is rejected");
    assert_eq!(rejected.kind(), AgentErrorKind::AgentState);
    assert!(matches!(
        rejected,
        crate::agent::AgentError::State(
            crate::agent::AgentStateError::ReconfigWhileAwaitingRegistry
        )
    ));
    // The parked queue is untouched: still exactly the planned set.
    assert_eq!(machine.state().queued_reconfigs().len(), 1);

    // Resuming applies the parked application and clears the queue.
    let outcome = machine.step(StepInput::resume(RequirementResolution::new(
        reconfig_id,
        RequirementResult::Reconfig(Ok(())),
    )));
    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    assert!(machine.state().queued_reconfigs().is_empty());
    assert_eq!(machine.state().current_tool_set(), &replacement);

    // The rejected request can be resubmitted once the park resolves.
    machine
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("late overlay".to_owned()),
            0,
        ))
        .expect("rejected reconfig can be resubmitted after resume");
    assert_eq!(machine.state().queued_reconfigs().len(), 1);
}

/// Conflicting reconfigurations are rejected eagerly and atomically at queue
/// time, leaving the queue and state untouched. A duplicate skill and a stale
/// overlay version fail as [`AgentErrorKind::AgentState`]; an unresolvable tool
/// set fails as [`AgentErrorKind::Tool`].
#[test]
fn conflicting_reconfig_requests_are_rejected_atomically() {
    let active_skill = "018f0d9c-7b6a-7c12-8f31-1234567890d1"
        .parse()
        .expect("active skill id");
    let mut initial_state = state();
    initial_state
        .replace_active_skills(vec![active_skill])
        .expect("active skill set");
    let mut machine = reconfig_machine(initial_state);

    // Re-activating an already active skill is rejected; the queue is untouched.
    let duplicate = machine
        .reconfigure(ReconfigRequest::ActivateSkill {
            skill_id: active_skill,
        })
        .expect_err("duplicate skill activation is rejected");
    assert_eq!(duplicate.kind(), AgentErrorKind::AgentState);
    assert_eq!(machine.state().active_skills(), &[active_skill]);
    assert!(machine.state().queued_reconfigs().is_empty());

    // A first overlay queues; a second against the stale version is rejected.
    machine
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("first overlay".to_owned()),
            0,
        ))
        .expect("first overlay queued");
    let stale = machine
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("stale overlay".to_owned()),
            0,
        ))
        .expect_err("stale overlay version is rejected");
    assert_eq!(stale.kind(), AgentErrorKind::AgentState);
    assert_eq!(machine.state().queued_reconfigs().len(), 1);
    assert_eq!(machine.state().system_prompt_overlay(), None);
    assert_eq!(machine.state().system_prompt_overlay_version(), 0);

    // A strict resolver knows only the initial set, so an unknown replacement
    // set is rejected as a tool error and never reaches the queue.
    let strict_registry: Arc<dyn crate::agent::ToolRegistry> =
        Arc::new(DeclaredOnlyToolRegistry::new(Vec::new()));
    let mut strict_machine = reconfig_machine(state()).with_tool_registry_resolver(Arc::new(
        StaticToolRegistryResolver::single(tool_set_id(), strict_registry),
    ));
    let unknown = strict_machine
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement_tool_set(),
        })
        .expect_err("unknown tool set is rejected");
    assert_eq!(unknown.kind(), AgentErrorKind::Tool);
    assert!(strict_machine.state().queued_reconfigs().is_empty());
    assert_eq!(
        strict_machine.state().current_tool_set().id(),
        tool_set_id()
    );
}
