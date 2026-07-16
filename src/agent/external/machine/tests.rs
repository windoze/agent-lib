//! Unit coverage for [`ExternalAgentMachine`](super::ExternalAgentMachine)'s
//! pure step transitions.
//!
//! These construct a machine directly (no driver) and assert the cursor,
//! emitted requirements, and committed Conversation after each hop. The
//! end-to-end drain coverage (`external_agent_start_to_completed` /
//! `external_agent_start_to_failed`) lives in the workspace integration suite,
//! which exercises the same paths through the reference driver and the scripted
//! external session handler.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Map;

use super::ExternalAgentMachine;
use crate::agent::{
    AgentId, AgentInput, AgentMachine, LoopCursorKind, PivotMessage, PivotSource, RequirementError,
    RequirementId, RequirementIds, RequirementKind, RequirementKindTag, RequirementResolution,
    RequirementResult, StepId, StepInput, ToolSetId,
    external::{
        ExternalAgentError, ExternalAgentOutput, ExternalAgentSpec, ExternalAgentState,
        ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionInput, ExternalSessionPolicy,
        ExternalSessionRef, ExternalSessionResult, ExternalStreamPolicy, WorktreeIsolation,
    },
    spec::{ToolSetRef, WorktreeRef},
};
use crate::conversation::{Conversation, ConversationConfig, ConversationId, MessageId, TurnId};
use crate::model::{
    content::ContentBlock,
    message::{Message, Role},
    tool::Tool,
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
    ExternalAgentSpec::new(
        agent_id(),
        ExternalRuntimeKind::ClaudeCode,
        WorktreeRef::new("/repo/agent-lib"),
        None,
        ToolSetRef::new(tool_set_id(), vec![tool("apply_patch")]),
        ExternalSessionPolicy {
            permission_mode: ExternalPermissionMode::AcceptEdits,
            isolation: WorktreeIsolation::EphemeralGitWorktree,
            max_turns: Some(8),
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

fn external_resolution(id: RequirementId, result: ExternalSessionResult) -> RequirementResolution {
    RequirementResolution::new(id, RequirementResult::ExternalSession(Box::new(result)))
}

#[test]
fn external_user_message_blocks_on_start_session() {
    let mut machine = machine();
    let outcome = machine.step(StepInput::external(user_input("refactor the parser")));

    assert!(outcome.is_quiescent());
    assert_eq!(outcome.requirements.len(), 1);
    assert!(outcome.notifications.is_empty());

    let requirement = &outcome.requirements[0];
    match &requirement.kind {
        RequirementKind::NeedExternalSession { request } => {
            assert_eq!(request.agent_id, agent_id());
            assert_eq!(request.runtime, ExternalRuntimeKind::ClaudeCode);
            assert!(request.session.is_none());
            assert_eq!(request.tools.len(), 1);
            match &request.input {
                ExternalSessionInput::Start { prompt } => {
                    assert_eq!(prompt, "refactor the parser");
                }
                other => panic!("first advance must be a Start, got {other:?}"),
            }
        }
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }

    // The driver-facing cursor view is a non-terminal streaming step carrying the
    // outstanding requirement id.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![requirement.id]
    );

    // The Conversation opened a pending turn that is not yet committed.
    assert!(machine.state().conversation().pending().is_some());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_completed_resume_commits_and_settles_done() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let requirement_id = opened.requirements[0].id;

    let resumed = machine.step(StepInput::resume(external_resolution(
        requirement_id,
        completed_result(),
    )));

    assert!(resumed.is_quiescent());
    assert!(resumed.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

    // The session facts are recorded and the terminal output committed as the
    // turn's assistant response.
    assert_eq!(machine.state().session(), Some(&session_ref()));
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
}

#[test]
fn external_continue_reuses_the_established_session() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let first_id = opened.requirements[0].id;
    machine.step(StepInput::resume(external_resolution(
        first_id,
        completed_result(),
    )));

    // A second user message on an established session continues rather than
    // starting fresh, and carries the recorded session facts.
    let followup = machine.step(StepInput::external(user_input_seq("now add tests", 1)));
    let requirement = &followup.requirements[0];
    match &requirement.kind {
        RequirementKind::NeedExternalSession { request } => {
            assert_eq!(request.session.as_ref(), Some(&session_ref()));
            match &request.input {
                ExternalSessionInput::Continue { message } => {
                    assert_eq!(message, "now add tests");
                }
                other => panic!("second advance must be a Continue, got {other:?}"),
            }
        }
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }
}

#[test]
fn external_failed_resume_settles_error() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let requirement_id = opened.requirements[0].id;

    let resumed = machine.step(StepInput::resume(external_resolution(
        requirement_id,
        failed_result(),
    )));

    assert!(resumed.is_quiescent());
    assert!(resumed.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);

    // A failed advance still records the retained session facts but leaves no
    // committed turn — the pending turn is discarded.
    assert_eq!(machine.state().session(), Some(&session_ref()));
    assert!(machine.state().conversation().pending().is_none());
    assert_eq!(machine.state().conversation().turns().len(), 0);
}

#[test]
fn external_resume_targeting_the_wrong_requirement_fails() {
    let mut machine = machine();
    machine.step(StepInput::external(user_input("refactor the parser")));

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890c9"
        .parse()
        .expect("stray requirement id");
    let resumed = machine.step(StepInput::resume(external_resolution(
        stray,
        completed_result(),
    )));

    assert!(resumed.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn external_resume_while_idle_is_rejected() {
    let mut machine = machine();
    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890c9"
        .parse()
        .expect("stray requirement id");

    let outcome = machine.step(StepInput::resume(external_resolution(
        stray,
        completed_result(),
    )));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn external_pivot_input_is_rejected() {
    let mut machine = machine();
    let pivot = PivotMessage::new(
        "018f0d9c-7b6a-7c12-8f31-1234567890d1"
            .parse()
            .expect("pivot message id"),
        user_message("pivot"),
        PivotSource::Human,
    )
    .expect("valid pivot");

    let outcome = machine.step(StepInput::external(AgentInput::pivot(pivot)));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn external_abandon_settles_back_to_idle() {
    let mut machine = machine();
    machine.step(StepInput::external(user_input("refactor the parser")));
    assert!(machine.state().conversation().pending().is_some());

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890c9"
        .parse()
        .expect("stray requirement id");
    let outcome = machine.step(StepInput::abandon(stray));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(machine.state().conversation().pending().is_none());
}
