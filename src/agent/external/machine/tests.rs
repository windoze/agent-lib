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
    AgentId, AgentInput, AgentMachine, Interaction, InteractionResponse, LoopCursorKind,
    Notification, PivotMessage, PivotSource, RequirementError, RequirementId, RequirementIds,
    RequirementKind, RequirementKindTag, RequirementResolution, RequirementResult, StepId,
    StepInput, ToolSetId, ToolWaitRequirements,
    external::{
        ExternalAgentCursor, ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput,
        ExternalAgentSpec, ExternalAgentState, ExternalArtifactKind, ExternalArtifactRef,
        ExternalObservedEvent, ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionInput,
        ExternalSessionPolicy, ExternalSessionRef, ExternalSessionResult, ExternalStreamPolicy,
        ExternalToolBatchId, WorktreeIsolation,
    },
    spec::{ToolSetRef, WorktreeRef},
};
use crate::conversation::{
    Conversation, ConversationConfig, ConversationId, MessageId, ToolCallId, TurnId,
};
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
fn external_agent_abandon_settles_and_flags_cleanup() {
    let mut machine = machine();
    machine.step(StepInput::external(user_input("refactor the parser")));
    assert!(machine.state().conversation().pending().is_some());
    // Opening the turn parked the machine on AwaitingSession, so a live runtime
    // session may exist and abandon must flag it for the handle layer.
    assert!(!machine.state().cleanup_required());

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890c9"
        .parse()
        .expect("stray requirement id");
    let outcome = machine.step(StepInput::abandon(stray));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    // Never-resume abandon settles to a feedable Idle without emitting Shutdown.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(machine.state().conversation().pending().is_none());
    // The orphaned session is flagged for the handle layer to force-close (§6.4).
    assert!(machine.state().cleanup_required());
}

#[test]
fn external_agent_abandon_while_awaiting_interaction_flags_cleanup() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    // Drive to a pause so the cursor parks on AwaitingInteraction with a live
    // session behind it.
    machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_result("act-42"),
    )));
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(machine.state().conversation().pending().is_some());
    assert!(!machine.state().cleanup_required());

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890ca"
        .parse()
        .expect("stray requirement id");
    let outcome = machine.step(StepInput::abandon(stray));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(machine.state().conversation().pending().is_none());
    assert!(machine.state().cleanup_required());
}

#[test]
fn external_agent_abandon_when_idle_does_not_flag_cleanup() {
    // Abandoning a machine that never opened a session has nothing to sweep.
    let mut machine = machine();
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890cb"
        .parse()
        .expect("stray requirement id");
    let outcome = machine.step(StepInput::abandon(stray));

    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);
    assert!(!machine.state().cleanup_required());
}

#[test]
fn external_pause_emits_interaction_and_parks_on_awaiting_interaction() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_result("act-42"),
    )));

    assert!(paused.is_quiescent());
    assert!(paused.notifications.is_empty());
    assert_eq!(paused.requirements.len(), 1);

    // The pause reifies exactly one NeedInteraction carrying the runtime's
    // clarification, and no external session requirement.
    let interaction_requirement = &paused.requirements[0];
    assert_eq!(
        interaction_requirement.tag(),
        RequirementKindTag::Interaction
    );
    match &interaction_requirement.kind {
        RequirementKind::NeedInteraction { request } => {
            assert_eq!(request.step_id(), paused_step_id());
        }
        other => panic!("expected a NeedInteraction requirement, got {other:?}"),
    }

    // The resumable session facts reported at the pause are recorded, and the
    // in-flight turn stays open across the pause.
    assert_eq!(machine.state().session(), Some(&session_ref()));
    assert!(machine.state().conversation().pending().is_some());

    // The driver-facing cursor is a non-terminal streaming step stuck on the
    // interaction requirement.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![interaction_requirement.id]
    );
}

#[test]
fn external_interaction_resume_responds_with_the_paused_action_id() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let session_requirement_id = opened.requirements[0].id;

    let paused = machine.step(StepInput::resume(external_resolution(
        session_requirement_id,
        paused_result("act-42"),
    )));
    let interaction_requirement_id = paused.requirements[0].id;

    let responded = machine.step(StepInput::resume(interaction_resolution(
        interaction_requirement_id,
        "yes, run the tests",
    )));

    assert!(responded.is_quiescent());
    assert!(responded.notifications.is_empty());
    assert_eq!(responded.requirements.len(), 1);

    // The resolved interaction re-enters the session as a RespondInteraction that
    // echoes the exact action id the pause carried and reuses the established
    // session facts.
    let requirement = &responded.requirements[0];
    match &requirement.kind {
        RequirementKind::NeedExternalSession { request } => {
            assert_eq!(request.session.as_ref(), Some(&session_ref()));
            match &request.input {
                ExternalSessionInput::RespondInteraction {
                    action_id,
                    response,
                } => {
                    assert_eq!(action_id, "act-42");
                    assert_eq!(
                        response,
                        &InteractionResponse::answer("yes, run the tests".to_owned())
                    );
                }
                other => panic!("resume must feed a RespondInteraction, got {other:?}"),
            }
        }
        other => panic!("expected a NeedExternalSession requirement, got {other:?}"),
    }

    // The machine is back on AwaitingSession, stuck on the fresh external
    // requirement, with the turn still open.
    assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        machine.cursor().pending_requirement_ids(),
        vec![requirement.id]
    );
    assert!(machine.state().conversation().pending().is_some());
}

#[test]
fn external_pause_then_respond_then_complete_commits_the_turn() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));

    let paused = machine.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        paused_result("act-7"),
    )));
    let responded = machine.step(StepInput::resume(interaction_resolution(
        paused.requirements[0].id,
        "go ahead",
    )));
    let completed = machine.step(StepInput::resume(external_resolution(
        responded.requirements[0].id,
        completed_result(),
    )));

    assert!(completed.is_quiescent());
    assert!(completed.requirements.is_empty());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

    // The whole pause↔respond loop folds into a single committed turn.
    let conversation = machine.state().conversation();
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
}

#[test]
fn external_interaction_resume_rejecting_a_non_interaction_result_fails() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let paused = machine.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        paused_result("act-42"),
    )));
    let interaction_requirement_id = paused.requirements[0].id;

    // A wrong-family result for an outstanding NeedInteraction settles on Error.
    let outcome = machine.step(StepInput::resume(external_resolution(
        interaction_requirement_id,
        completed_result(),
    )));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn external_interaction_resume_targeting_the_wrong_requirement_fails() {
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    machine.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        paused_result("act-42"),
    )));

    let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890ca"
        .parse()
        .expect("stray requirement id");
    let outcome = machine.step(StepInput::resume(interaction_resolution(stray, "hi")));

    assert!(outcome.is_quiescent());
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
}

#[test]
fn external_agent_emits_observation_notifications() {
    // A Completed decision point replays its buffered observations, in order, as
    // `Notification::ExternalAgent` events on the resuming step (design §5.5).
    let mut direct = machine();
    let opened = direct.step(StepInput::external(user_input("refactor the parser")));
    let batch = observation_batch("done");
    let completed = direct.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        completed_with(3, sequenced(1, batch.clone())),
    )));

    assert!(completed.is_quiescent());
    assert!(completed.requirements.is_empty());
    assert_eq!(direct.cursor().kind(), LoopCursorKind::Done);
    // Exactly the buffered observations, preserving order and count.
    assert_eq!(external_events(&completed.notifications), batch);

    // The machine records `last_event_seq` in its retained session facts and
    // dedups observations *per event* on resume: a replayed decision point whose
    // events all fall at or below the consumed sequence emits nothing, and an
    // overlapping batch straddling the boundary replays only its unseen suffix
    // (design §5.5).
    let mut looped = machine();
    let opened = looped.step(StepInput::external(user_input("refactor the parser")));

    // First pause buffers seqs 1..=3 with no prior consumed sequence, so all
    // three events are emitted and seq 3 becomes the consumed high-water mark.
    let first_batch = observation_batch("first");
    let first_pause = looped.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        paused_with("act-1", 3, sequenced(1, first_batch.clone())),
    )));
    assert_eq!(external_events(&first_pause.notifications), first_batch);

    // Answer the interaction so the turn loops back to AwaitingSession.
    let responded = looped.step(StepInput::resume(interaction_resolution(
        first_pause.requirements[0].id,
        "go ahead",
    )));

    // A replayed pause reporting the same events (seqs 1..=3) is a duplicate:
    // every event is at or below the consumed sequence, so nothing is re-emitted.
    let replay_pause = looped.step(StepInput::resume(external_resolution(
        responded.requirements[0].id,
        paused_with("act-1", 3, sequenced(1, observation_batch("first"))),
    )));
    assert!(
        replay_pause.notifications.is_empty(),
        "observations at or below the consumed sequence must not be replayed"
    );

    // Answer again; the next pause overlaps the consumed boundary: seqs 3..=5
    // against a consumed mark of 3. Only the strictly-greater suffix (seqs 4 and
    // 5) is replayed, proving dedup is per event rather than per batch.
    let responded_again = looped.step(StepInput::resume(interaction_resolution(
        replay_pause.requirements[0].id,
        "go ahead",
    )));
    let overlap_batch = observation_batch("overlap");
    let overlap_pause = looped.step(StepInput::resume(external_resolution(
        responded_again.requirements[0].id,
        paused_with("act-1", 5, sequenced(3, overlap_batch.clone())),
    )));
    assert_eq!(
        external_events(&overlap_pause.notifications),
        overlap_batch[1..].to_vec(),
        "only observations beyond the consumed sequence are replayed"
    );

    // A final Completed reporting a fresh sequence (seqs 6..=8) beyond the
    // consumed one (5) replays its new observations in full.
    let responded_final = looped.step(StepInput::resume(interaction_resolution(
        overlap_pause.requirements[0].id,
        "go ahead",
    )));
    let final_batch = observation_batch("final");
    let final_completed = looped.step(StepInput::resume(external_resolution(
        responded_final.requirements[0].id,
        completed_with(8, sequenced(6, final_batch.clone())),
    )));

    assert_eq!(looped.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(external_events(&final_completed.notifications), final_batch);
}

#[test]
fn external_agent_records_artifacts() {
    // A completed session folds `ExternalAgentOutput.artifacts` into the retained
    // trace on `ExternalAgentState`, preserving order (design §11).
    let mut direct = machine();
    assert!(
        direct.state().artifacts().is_empty(),
        "a fresh machine records no artifacts"
    );

    let opened = direct.step(StepInput::external(user_input("refactor the parser")));
    let artifacts = sample_artifacts();
    let completed = direct.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        completed_with_artifacts(artifacts.clone()),
    )));

    assert!(completed.is_quiescent());
    assert_eq!(direct.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(direct.state().artifacts(), artifacts.as_slice());

    // Only redacted references are recorded — a kind, an untrusted summary, and
    // opaque path/reference handles — never inline artifact content (§12).
    for artifact in direct.state().artifacts() {
        if let Some(reference) = artifact.reference.as_deref() {
            assert!(
                reference.starts_with("blob://"),
                "reference must be an opaque handle, not inline content: {reference}"
            );
        }
    }

    // The recorded references survive the state persistence boundary unchanged.
    let encoded = serde_json::to_value(direct.state()).expect("serialize state");
    let decoded: ExternalAgentState = serde_json::from_value(encoded).expect("deserialize state");
    assert_eq!(decoded.artifacts(), artifacts.as_slice());
}

#[test]
fn external_agent_records_no_artifacts_when_output_reports_none() {
    // A completion with an empty artifact list leaves the recorded trace empty and
    // keeps the artifacts field absent from the persisted state (backward-compatible
    // snapshot shape).
    let mut direct = machine();
    let opened = direct.step(StepInput::external(user_input("refactor the parser")));
    direct.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        completed_with_artifacts(Vec::new()),
    )));

    assert!(direct.state().artifacts().is_empty());
    let encoded = serde_json::to_value(direct.state()).expect("serialize state");
    assert!(
        encoded.get("artifacts").is_none(),
        "an empty artifact list is skipped in the snapshot"
    );
}

#[test]
fn awaiting_tool_cursor_restores_without_a_terminal_view() {
    // A machine restored while a session is parked on a tool batch keeps the
    // resumable requirement addressing on its serializable cursor, but the
    // non-serialized batch scratch and driver-facing streaming view cannot be
    // rebuilt from state alone. `initial_loop_cursor` must therefore surface a
    // non-terminal `Idle` view (never a false `Done`/`Error`) so the driver does
    // not mistake a mid-flight batch for a finished turn. Faithfully rehydrating
    // the streaming/tool-wait view is the "恢复 mid-turn scratch" follow-up
    // tracked in PLAN.md.
    let batch_id = ExternalToolBatchId::new("batch-91");
    let requirement: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890cf"
        .parse()
        .expect("requirement id");
    let call_id: ToolCallId = "018f0d9c-7b6a-7c12-8f31-1234567890ce"
        .parse()
        .expect("tool call id");
    let requirements = ToolWaitRequirements::root({
        let mut ids = std::collections::BTreeMap::new();
        ids.insert(call_id, requirement);
        ids
    });

    let mut state = ExternalAgentState::new(spec(), empty_conversation());
    state.set_cursor(ExternalAgentCursor::AwaitingTool {
        batch_id: batch_id.clone(),
        requirements: requirements.clone(),
    });

    // Persist and restore the state to prove the resumable addressing survives
    // the snapshot boundary while the volatile scratch does not.
    let encoded = serde_json::to_value(&state).expect("serialize state");
    assert_eq!(
        encoded["cursor"]["state"],
        serde_json::json!("awaiting_tool")
    );
    let decoded: ExternalAgentState = serde_json::from_value(encoded).expect("deserialize state");
    assert_eq!(
        decoded.cursor(),
        &ExternalAgentCursor::AwaitingTool {
            batch_id,
            requirements: requirements.clone(),
        }
    );
    assert_eq!(decoded.cursor().requirements(), Some(&requirements));

    let restored = ExternalAgentMachine::new(decoded, Arc::new(SeqRequirementIds::default()));

    // Degraded driver-facing view: non-terminal `Idle`, not a false terminal.
    let kind = restored.cursor().kind();
    assert_eq!(kind, LoopCursorKind::Idle);
    assert_ne!(kind, LoopCursorKind::Done);
    assert_ne!(kind, LoopCursorKind::Error);
    // The streaming view is not rebuilt, so the driver-facing cursor reports no
    // pending requirements; the outstanding ids remain recoverable from the
    // serializable external cursor above.
    assert!(restored.cursor().pending_requirement_ids().is_empty());
}
