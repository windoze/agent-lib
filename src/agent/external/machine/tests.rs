//! Unit coverage for [`ExternalAgentMachine`](super::ExternalAgentMachine)'s
//! pure step transitions.
//!
//! These construct a machine directly (no driver) and assert the cursor,
//! emitted requirements, and committed Conversation after each hop. The
//! end-to-end drain coverage (`external_agent_start_to_completed` /
//! `external_agent_start_to_failed`) lives in the workspace integration suite,
//! which exercises the same paths through the reference driver and the scripted
//! external session handler.

mod config;
mod interaction;
mod lifecycle;
mod observations;
mod reconfig;
mod spawn_agent;
mod subagent;
mod tools;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Map;

use super::ExternalAgentMachine;
use crate::agent::collab::SPAWN_AGENT;
use crate::agent::{
    AgentError, AgentId, AgentInput, AgentMachine, AgentSpecRef, Interaction, InteractionResponse,
    LoopCursorKind, Notification, PermissionCategory, PermissionRequest, PermissionResponse,
    PermissionRisk, PivotMessage, PivotSource, RequirementError, RequirementId, RequirementIds,
    RequirementKind, RequirementKindTag, RequirementResolution, RequirementResult, StepId,
    StepInput, StepOutcome, SubagentOutput, ToolExecutionIds, ToolRuntimeError, ToolSetId,
    ToolWaitRequirements,
    external::{
        ExternalAgentCursor, ExternalAgentError, ExternalAgentEvent, ExternalAgentMachineConfig,
        ExternalAgentOutput, ExternalAgentSpec, ExternalAgentState, ExternalArtifactKind,
        ExternalArtifactRef, ExternalCapability, ExternalObservedEvent, ExternalPermissionMode,
        ExternalReconfigOutcome, ExternalReconfigTiming, ExternalRuntimeKind, ExternalSessionInput,
        ExternalSessionPolicy, ExternalSessionRef, ExternalSessionRequest, ExternalSessionResult,
        ExternalStreamPolicy, ExternalSubagentOutput, ExternalSubagentRequest,
        ExternalSubagentRequestId, ExternalToolBatchId, ExternalToolCall,
        ExternalToolFailurePolicy, WorktreeIsolation,
    },
    spec::{ToolSetRef, WorktreeRef},
};
use crate::conversation::{
    Conversation, ConversationConfig, ConversationId, MessageId, ToolCallId, TurnId,
};
use crate::model::{
    content::ContentBlock,
    message::{Message, Role},
    tool::{Tool, ToolCall, ToolResponse, ToolStatus},
};

/// Deterministic requirement-id source: hands out distinct ids per call.
#[derive(Debug, Default)]
struct SeqRequirementIds {
    next: AtomicU64,
}

impl RequirementIds for SeqRequirementIds {
    fn next_requirement_id(
        &self,
        _kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let id = format!("018f0d9c-7b6a-7c12-8f31-20000000{n:04x}");
        Ok(RequirementId::parse_str(&id).expect("valid requirement id"))
    }
}

/// Deterministic tool-call-id source: hands out distinct framework
/// [`ToolCallId`]s per call. The external machine only ever needs
/// [`tool_call_id`](ToolExecutionIds::tool_call_id) — the runtime, not the host,
/// registers the tool result and continues the assistant step — so the remaining
/// trait methods are never exercised and report an unavailable id if called.
#[derive(Debug, Default)]
struct SeqToolIds {
    next: AtomicU64,
}

impl ToolExecutionIds for SeqToolIds {
    fn tool_call_id(&self, _call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let id = format!("018f0d9c-7b6a-7c12-8f31-30000000{n:04x}");
        Ok(id.parse().expect("valid tool call id"))
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        _call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        Err(ToolRuntimeError::IdUnavailable {
            purpose: "tool result message (unused by the external machine)".to_owned(),
        })
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        Err(ToolRuntimeError::IdUnavailable {
            purpose: "assistant continuation message (unused by the external machine)".to_owned(),
        })
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        Err(ToolRuntimeError::IdUnavailable {
            purpose: "assistant continuation step (unused by the external machine)".to_owned(),
        })
    }
}

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f0"
        .parse()
        .expect("agent id")
}

fn tool_set_id() -> ToolSetId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f1"
        .parse()
        .expect("tool set id")
}

fn tool(name: &str) -> Tool {
    Tool {
        name: name.to_owned(),
        description: format!("Tool {name}."),
        input_schema: serde_json::json!({ "type": "object" }),
    }
}

fn spec() -> ExternalAgentSpec {
    spec_with_max_turns(Some(8))
}

fn spec_with_max_turns(max_turns: Option<u32>) -> ExternalAgentSpec {
    ExternalAgentSpec::new(
        agent_id(),
        ExternalRuntimeKind::ClaudeCode,
        WorktreeRef::new("/repo/agent-lib"),
        None,
        ToolSetRef::new(tool_set_id(), vec![tool("apply_patch")]),
        ExternalSessionPolicy {
            permission_mode: ExternalPermissionMode::AcceptEdits,
            isolation: WorktreeIsolation::EphemeralGitWorktree,
            max_turns,
            stream_events: ExternalStreamPolicy::Buffered,
        },
    )
}

fn empty_conversation() -> Conversation {
    let conversation_id: ConversationId = "018f0d9c-7b6a-7c12-8f31-1234567890fa"
        .parse()
        .expect("conversation id");
    Conversation::new(
        conversation_id,
        ConversationConfig::new(Some("Drive the external agent.".to_owned())),
    )
}

fn machine() -> ExternalAgentMachine {
    ExternalAgentMachine::new(
        ExternalAgentState::new(spec(), empty_conversation()),
        Arc::new(SeqRequirementIds::default()),
    )
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
            extra: Map::new(),
        }],
    }
}

fn user_input(text: &str) -> AgentInput {
    user_input_seq(text, 0)
}

fn user_input_seq(text: &str, seq: u8) -> AgentInput {
    let turn_id: TurnId = format!("018f0d9c-7b6a-7c12-8f31-b000000000{seq:02x}")
        .parse()
        .expect("turn id");
    let message_id: MessageId = format!("018f0d9c-7b6a-7c12-8f31-b100000000{seq:02x}")
        .parse()
        .expect("user message id");
    let assistant_message_id: MessageId = format!("018f0d9c-7b6a-7c12-8f31-b200000000{seq:02x}")
        .parse()
        .expect("assistant message id");
    let step_id: StepId = format!("018f0d9c-7b6a-7c12-8f31-b300000000{seq:02x}")
        .parse()
        .expect("step id");
    AgentInput::user_message(
        turn_id,
        message_id,
        user_message(text),
        assistant_message_id,
        step_id,
    )
    .expect("user input is Role::User")
}

fn session_ref() -> ExternalSessionRef {
    ExternalSessionRef {
        runtime: ExternalRuntimeKind::ClaudeCode,
        session_id: Some("sess-1".to_owned()),
        transcript_ref: None,
        resume_token: Some("resume-1".to_owned()),
        last_event_seq: Some(3),
    }
}

fn output(summary: &str) -> ExternalAgentOutput {
    ExternalAgentOutput {
        summary: summary.to_owned(),
        artifacts: Vec::new(),
        usage: None,
        cost_micros: None,
    }
}

fn completed_result() -> ExternalSessionResult {
    ExternalSessionResult::Completed {
        session: session_ref(),
        output: output("refactor complete"),
        observations: Vec::new(),
    }
}

fn failed_result() -> ExternalSessionResult {
    ExternalSessionResult::Failed {
        session: Some(session_ref()),
        error: ExternalAgentError::LimitExceeded {
            limit: "max_turns=8".to_owned(),
        },
        observations: Vec::new(),
    }
}

fn paused_step_id() -> StepId {
    "018f0d9c-7b6a-7c12-8f31-1234567890e1"
        .parse()
        .expect("paused step id")
}

fn paused_result(action_id: &str) -> ExternalSessionResult {
    ExternalSessionResult::PausedForInteraction {
        session: session_ref(),
        action_id: action_id.to_owned(),
        request: Interaction::question(
            paused_step_id(),
            "Allow the external agent to run `cargo test`?".to_owned(),
        ),
        observations: Vec::new(),
    }
}

/// A machine wired with a deterministic [`SeqToolIds`] source, ready to bridge a
/// runtime tool-call pause into `NeedTool` requirements.
fn machine_with_tool_ids() -> ExternalAgentMachine {
    ExternalAgentMachine::new(
        ExternalAgentState::new(spec(), empty_conversation()),
        Arc::new(SeqRequirementIds::default()),
    )
    .with_tool_execution_ids(Arc::new(SeqToolIds::default()))
}

/// One runtime tool call carrying a provider correlation id and a name.
fn external_tool_call(provider_call_id: &str, name: &str) -> ExternalToolCall {
    ExternalToolCall {
        provider_call_id: provider_call_id.to_owned(),
        name: name.to_owned(),
        input: serde_json::json!({ "path": "src/lib.rs" }),
        raw: None,
    }
}

/// A `PausedForToolCalls` result carrying `calls` under `batch_id`.
fn paused_for_tools(batch_id: &str, calls: Vec<ExternalToolCall>) -> ExternalSessionResult {
    ExternalSessionResult::PausedForToolCalls {
        session: session_ref(),
        batch_id: ExternalToolBatchId::new(batch_id),
        calls,
        observations: Vec::new(),
    }
}

/// One runtime subagent spawn request tagged with `request_id`, reusing the
/// fixture agent as the child spec and a question brief.
fn subagent_request(request_id: &str) -> ExternalSubagentRequest {
    ExternalSubagentRequest {
        request_id: ExternalSubagentRequestId::new(request_id),
        spec_ref: AgentSpecRef(agent_id()),
        brief: Interaction::question(paused_step_id(), "Investigate the flaky test.".to_owned()),
        result_schema: Some(serde_json::json!({ "type": "object" })),
        raw: None,
    }
}

/// A `PausedForSubagent` result carrying `request` under the fixture session.
fn paused_for_subagent(request: ExternalSubagentRequest) -> ExternalSessionResult {
    ExternalSessionResult::PausedForSubagent {
        session: session_ref(),
        request,
        observations: Vec::new(),
    }
}

/// Resumable session facts reporting a specific `last_event_seq`, used to
/// exercise observation dedup on resume (design §5.5).
fn session_ref_seq(seq: u64) -> ExternalSessionRef {
    ExternalSessionRef {
        runtime: ExternalRuntimeKind::ClaudeCode,
        session_id: Some("sess-1".to_owned()),
        transcript_ref: None,
        resume_token: Some("resume-1".to_owned()),
        last_event_seq: Some(seq),
    }
}

/// A distinct, ordered batch of buffered observations.
fn observation_batch(tag: &str) -> Vec<ExternalAgentEvent> {
    vec![
        ExternalAgentEvent::SessionStarted {
            session_id: Some("sess-1".to_owned()),
        },
        ExternalAgentEvent::TextDelta {
            text: format!("delta-{tag}"),
        },
        ExternalAgentEvent::SessionCompleted,
    ]
}

/// A `Completed` result whose output carries `artifacts`.
fn completed_with_artifacts(artifacts: Vec<ExternalArtifactRef>) -> ExternalSessionResult {
    ExternalSessionResult::Completed {
        session: session_ref(),
        output: ExternalAgentOutput {
            summary: "refactor complete".to_owned(),
            artifacts,
            usage: None,
            cost_micros: None,
        },
        observations: Vec::new(),
    }
}

/// A representative set of redacted artifact references: a patch and a test
/// result, each carrying only a summary plus opaque path/reference handles.
fn sample_artifacts() -> Vec<ExternalArtifactRef> {
    vec![
        ExternalArtifactRef {
            kind: ExternalArtifactKind::Patch,
            summary: "tighten parser error recovery".to_owned(),
            path: Some("src/parser.rs".to_owned()),
            reference: Some("blob://diff-1".to_owned()),
        },
        ExternalArtifactRef {
            kind: ExternalArtifactKind::TestResult,
            summary: "cargo test: 12 passed".to_owned(),
            path: None,
            reference: Some("blob://test-log-1".to_owned()),
        },
    ]
}

/// Wraps a batch of raw events into sequenced observations whose seqs start at
/// `start` and increase by one, mirroring how a runtime adapter tags a
/// contiguous run of stream events.
fn sequenced(start: u64, events: Vec<ExternalAgentEvent>) -> Vec<ExternalObservedEvent> {
    events
        .into_iter()
        .enumerate()
        .map(|(offset, event)| ExternalObservedEvent::new(start + offset as u64, event))
        .collect()
}

/// A `Completed` result carrying sequenced `observations` and reporting `seq` as
/// the last consumed event sequence.
fn completed_with(seq: u64, observations: Vec<ExternalObservedEvent>) -> ExternalSessionResult {
    ExternalSessionResult::Completed {
        session: session_ref_seq(seq),
        output: output("refactor complete"),
        observations,
    }
}

/// A `PausedForInteraction` result carrying sequenced `observations` and
/// reporting `seq`.
fn paused_with(
    action_id: &str,
    seq: u64,
    observations: Vec<ExternalObservedEvent>,
) -> ExternalSessionResult {
    ExternalSessionResult::PausedForInteraction {
        session: session_ref_seq(seq),
        action_id: action_id.to_owned(),
        request: Interaction::question(
            paused_step_id(),
            "Allow the external agent to run `cargo test`?".to_owned(),
        ),
        observations,
    }
}

/// Extracts the [`ExternalAgentEvent`]s from a batch of external-agent
/// notifications, asserting each is a `Notification::ExternalAgent`.
fn external_events(notifications: &[Notification]) -> Vec<ExternalAgentEvent> {
    notifications
        .iter()
        .map(|notification| match notification {
            Notification::ExternalAgent(event) => event.clone(),
            other => panic!("expected a Notification::ExternalAgent, got {other:?}"),
        })
        .collect()
}

fn interaction_resolution(id: RequirementId, answer: &str) -> RequirementResolution {
    RequirementResolution::new(
        id,
        RequirementResult::Interaction(InteractionResponse::answer(answer.to_owned())),
    )
}

/// The [`PermissionRequest`] a [`permission_paused_result`] asks the host to
/// resolve, keyed by `action_id`.
fn permission_request(action_id: &str) -> PermissionRequest {
    PermissionRequest::new(
        action_id.to_owned(),
        agent_id(),
        PermissionCategory::Shell,
        "run `cargo test`".to_owned(),
        serde_json::json!({ "command": "cargo test" }),
        PermissionRisk::Medium,
        Some("verify the refactor".to_owned()),
    )
}

/// A `PausedForInteraction` result modelling a permission prompt keyed by
/// `action_id`.
fn permission_paused_result(action_id: &str) -> ExternalSessionResult {
    ExternalSessionResult::PausedForInteraction {
        session: session_ref(),
        action_id: action_id.to_owned(),
        request: Interaction::permission(paused_step_id(), permission_request(action_id)),
        observations: Vec::new(),
    }
}

/// A `PausedForInteraction` result modelling a fixed-option choice prompt.
fn choice_paused_result(action_id: &str, options: Vec<String>) -> ExternalSessionResult {
    ExternalSessionResult::PausedForInteraction {
        session: session_ref(),
        action_id: action_id.to_owned(),
        request: Interaction::choice(paused_step_id(), "Pick a branch.".to_owned(), options),
        observations: Vec::new(),
    }
}

/// A resolution carrying an arbitrary [`InteractionResponse`] for requirement
/// `id`.
fn response_resolution(id: RequirementId, response: InteractionResponse) -> RequirementResolution {
    RequirementResolution::new(id, RequirementResult::Interaction(response))
}

/// Unwraps the `RespondInteraction` a resumed session was fed, asserting it
/// echoes `action_id`, and returns the response it carried.
fn respond_interaction(
    outcome: &StepOutcome,
    action_id: &str,
) -> (RequirementId, InteractionResponse) {
    assert_eq!(
        outcome.requirements.len(),
        1,
        "a valid interaction response relays exactly one RespondInteraction"
    );
    let requirement = &outcome.requirements[0];
    match &requirement.kind {
        RequirementKind::NeedExternalSession { request } => match &request.input {
            ExternalSessionInput::RespondInteraction {
                action_id: echoed,
                response,
            } => {
                assert_eq!(echoed, action_id, "the pause's action_id is echoed back");
                (requirement.id, response.clone())
            }
            other => panic!("resume must feed a RespondInteraction, got {other:?}"),
        },
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }
}

fn external_resolution(id: RequirementId, result: ExternalSessionResult) -> RequirementResolution {
    RequirementResolution::new(id, RequirementResult::ExternalSession(Box::new(result)))
}

/// A successful host [`ToolResponse`] answering `provider_call_id` with `text`.
fn tool_response(provider_call_id: &str, text: &str) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_call_id.to_owned(),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
            extra: Map::new(),
        }],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

/// A resolved `NeedTool` requirement carrying a successful host tool result.
fn tool_resolution(id: RequirementId, provider_call_id: &str, text: &str) -> RequirementResolution {
    RequirementResolution::new(
        id,
        RequirementResult::Tool(Ok(tool_response(provider_call_id, text))),
    )
}

/// A resolved `NeedTool` requirement carrying a runtime execution failure.
fn tool_error_resolution(id: RequirementId, error: ToolRuntimeError) -> RequirementResolution {
    RequirementResolution::new(id, RequirementResult::Tool(Err(error)))
}

/// A resolved `NeedSubagent` requirement carrying a successful child output.
fn subagent_resolution(id: RequirementId, summary: &str) -> RequirementResolution {
    RequirementResolution::new(
        id,
        RequirementResult::Subagent(Ok(SubagentOutput {
            summary: summary.to_owned(),
        })),
    )
}

/// A resolved `NeedSubagent` requirement carrying a subagent-drive failure.
fn subagent_error_resolution(id: RequirementId, error: AgentError) -> RequirementResolution {
    RequirementResolution::new(id, RequirementResult::Subagent(Err(error)))
}

// ----- shared cross-topic drivers and assertions ------------------------------

/// Drives a machine to a two-call tool pause (`call-a`, `call-b` under
/// `batch-7`) and returns it alongside the per-call requirement ids, in call
/// order, so a resume can target each call's `NeedTool`.
fn pause_on_two_tools() -> (ExternalAgentMachine, Vec<RequirementId>) {
    let mut machine = machine_with_tool_ids();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let calls = vec![
        external_tool_call("call-a", "apply_patch"),
        external_tool_call("call-b", "run_tests"),
    ];
    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_for_tools("batch-7", calls),
    )));

    // The requirements are emitted one per call, in call order.
    let requirement_ids: Vec<RequirementId> = paused.requirements.iter().map(|r| r.id).collect();
    assert_eq!(requirement_ids.len(), 2);
    assert_eq!(machine.cursor().kind(), LoopCursorKind::AwaitingTool);
    (machine, requirement_ids)
}

/// Asserts `outcome` carries exactly one `RespondToolResults` external-session
/// requirement whose batch id and result ordering match the paused batch.
fn assert_responds_with_batch(outcome: &StepOutcome, expected_order: &[&str]) {
    assert_eq!(outcome.requirements.len(), 1);
    match &outcome.requirements[0].kind {
        RequirementKind::NeedExternalSession { request } => match &request.input {
            ExternalSessionInput::RespondToolResults { batch_id, results } => {
                assert_eq!(batch_id.as_str(), "batch-7");
                let order: Vec<&str> = results
                    .iter()
                    .map(|result| result.provider_call_id.as_str())
                    .collect();
                assert_eq!(order, expected_order);
            }
            other => panic!("expected a RespondToolResults input, got {other:?}"),
        },
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }
}

/// One `spawn_agent` runtime tool call reusing the fixture agent as the child
/// spec and `brief` as the task brief (design §8.3).
fn spawn_agent_call(provider_call_id: &str, brief: &str) -> ExternalToolCall {
    ExternalToolCall {
        provider_call_id: provider_call_id.to_owned(),
        name: SPAWN_AGENT.to_owned(),
        input: serde_json::json!({
            "spec": agent_id().to_string(),
            "brief": brief,
            "result_schema": { "type": "object" },
        }),
        raw: None,
    }
}

/// A malformed `spawn_agent` call: the required `brief` argument is missing, so
/// [`SpawnAgentRequest::parse`](crate::agent::collab::SpawnAgentRequest::parse)
/// rejects it and the machine must return a runtime-visible error (design §8.4).
fn malformed_spawn_agent_call(provider_call_id: &str) -> ExternalToolCall {
    ExternalToolCall {
        provider_call_id: provider_call_id.to_owned(),
        name: SPAWN_AGENT.to_owned(),
        input: serde_json::json!({ "spec": agent_id().to_string() }),
        raw: None,
    }
}

/// Reads the `NeedExternalSession` request a step emitted, panicking on any
/// other requirement shape.
fn need_session_request(outcome: &StepOutcome) -> &ExternalSessionRequest {
    match &outcome.requirements[0].kind {
        RequirementKind::NeedExternalSession { request } => request,
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }
}
