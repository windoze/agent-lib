//! Scripted testing components for the external-agent session effect boundary.
//!
//! Milestone 2 wired the external session effect into `agent-lib`: a
//! `NeedExternalSession` [`Requirement`](agent_lib::agent::Requirement) is
//! fulfilled by an [`ExternalSessionHandler`] that advances an external
//! coding-agent session (Claude Code / Codex / …) to its next decision point.
//! This module gives agent-layer tests the offline doubles for that boundary,
//! mirroring the scripted handlers for the other families in
//! [`crate::handlers`]:
//!
//! - [`ScriptedExternalSessionHandler`] fulfils a `NeedExternalSession` from a
//!   [`Script`] of [`ExternalSessionStep`]s, returning the preset
//!   [`ExternalSessionResult`] for each `fulfill` in dispatch order and
//!   recording every call in an observable [`ExternalAgentCallLog`]. When the
//!   script is drained under [`StrictMode::Error`](crate::script::StrictMode)
//!   the exhaustion folds into a *family-aligned*
//!   [`ExternalSessionResult::Failed`] carried inside
//!   [`RequirementResult::ExternalSession`], never a wrong-family result.
//! - [`ExternalAgentFixture`] builds the provider-neutral request/result/event
//!   shapes a test scripts against: a `Start`/`Continue`
//!   [`ExternalSessionRequest`], a permission-style
//!   [`ExternalSessionResult::PausedForInteraction`], structured
//!   [`ExternalAgentEvent`] observations
//!   ([`FilePatch`](ExternalAgentEvent::FilePatch),
//!   [`CommandFinished`](ExternalAgentEvent::CommandFinished),
//!   [`PermissionRequested`](ExternalAgentEvent::PermissionRequested)), and an
//!   [`ExternalAgentOutput`].
//!
//! Read the recorded traffic back with
//! [`assert_external_calls`](crate::assertions::assert_external_calls).
//!
//! The [`runtime`] submodule sits one layer *below* this scripted handler: it
//! exercises the milestone-5 runtime abstraction
//! ([`ExternalRuntimeAdapter`](agent_lib::agent::external::ExternalRuntimeAdapter)
//! / [`ExternalRuntimeSession`](agent_lib::agent::external::ExternalRuntimeSession)
//! / [`ExternalSessionRegistry`](agent_lib::agent::external::ExternalSessionRegistry))
//! with a [`ScriptedExternalRuntimeAdapter`]
//! that advances a live session through a script of decision points, so the full
//! managed loop (start, tool batch, interaction, subagent) can be driven offline
//! through a registry-backed handler.
//!
//! The [`cassette`] submodule sits alongside the scripted runtime: it replays a
//! *recorded* runtime session from a redacted on-disk fixture
//! ([`CassetteRuntimeExternalSessionHandler`], design §12), freezing the parser
//! output (sequenced observations + decision point) for regression coverage of
//! the real Claude/Codex/OpenCode adapters landing in later milestones.

pub mod cassette;
pub mod runtime;

pub use cassette::{
    CassetteDecision, CassetteExternalRuntimeAdapter, CassetteExternalRuntimeSession,
    CassetteFrame, CassetteInputKind, CassetteRuntimeExternalSessionHandler, CassetteRuntimeInfo,
    CassetteStream, CassetteTurn, EXTERNAL_CASSETTE_SCHEMA_VERSION, ExternalCassetteError,
    ExternalRuntimeCassette, RedactionMetadata, SecretHit, scan_secrets,
};
pub use runtime::{
    ScriptedAdvance, ScriptedExternalRuntimeAdapter, ScriptedExternalRuntimeSession,
    ScriptedRuntimeBuilder, ScriptedRuntimeExternalSessionHandler, ScriptedRuntimeStartLog,
    ScriptedSinkLog,
};

use std::sync::Arc;

use agent_lib::agent::external::{
    ExternalAgentError, ExternalAgentEvent, ExternalAgentMachine, ExternalAgentOutput,
    ExternalAgentSpec, ExternalAgentState, ExternalArtifactKind, ExternalArtifactRef,
    ExternalObservedEvent, ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionInput,
    ExternalSessionPolicy, ExternalSessionRef, ExternalSessionRequest, ExternalSessionResult,
    ExternalSessionShutdown, ExternalStreamPolicy, ExternalSubagentRequest,
    ExternalSubagentRequestId, ExternalToolBatchId, ExternalToolCall, PreparedWorktree,
    WorktreeCleanupOutcome, WorktreeError, WorktreeIsolation, WorktreeManager,
};
use agent_lib::agent::{
    AgentId, AgentSpecRef, ExternalSessionHandler, Interaction, PermissionCategory,
    PermissionRequest, PermissionRisk, RequirementKindTag, RequirementResult, RunContext,
    ToolSetRef, WorktreeRef,
};
use agent_lib::conversation::{Conversation, ConversationConfig};
use async_trait::async_trait;

use crate::ids::SeqIds;

/// A [`WorktreeManager`] that performs no filesystem work.
///
/// Scripted and cassette drives never touch a real worktree: their requests
/// name fixture paths that need no git backing, so the registry's isolation
/// wiring (M2-7) is satisfied by returning the requested base unchanged from
/// `prepare` and treating `cleanup` as a recorded no-op. The registry's real
/// [`GitWorktreeManager`](agent_lib::agent::external::GitWorktreeManager)
/// behavior is covered by agent-lib's own registry/worktree tests.
#[derive(Debug, Default)]
pub struct PassThroughWorktreeManager;

#[async_trait]
impl WorktreeManager for PassThroughWorktreeManager {
    async fn prepare(
        &self,
        agent_id: AgentId,
        base: &WorktreeRef,
        isolation: WorktreeIsolation,
    ) -> Result<PreparedWorktree, WorktreeError> {
        Ok(
            PreparedWorktree::new(agent_id, isolation, base.clone(), false)
                .with_base_repo(base.clone()),
        )
    }

    async fn cleanup(
        &self,
        prepared: PreparedWorktree,
        disposition: ExternalSessionShutdown,
    ) -> Result<WorktreeCleanupOutcome, WorktreeError> {
        Ok(WorktreeCleanupOutcome::new(
            prepared.isolation(),
            prepared.worktree().clone(),
            false,
            disposition.leaves_residual_side_effects(),
        ))
    }
}
use crate::script::{CallLog, Script, ScriptStep};

/// The observable call log of a scripted external session handler.
///
/// Records, per fulfilled `NeedExternalSession`, the call's dispatch index, the
/// [`ExternalSessionRequest`] it was handed, the [`RequirementResult`] it
/// returned, and the completion order — the four facts design §12 asks an
/// external-agent call log to keep. Assert over it with
/// [`assert_external_calls`](crate::assertions::assert_external_calls).
pub type ExternalAgentCallLog = CallLog<ExternalSessionRequest, RequirementResult>;

/// One scripted external session decision-point result (a
/// [`RequirementResult::ExternalSession`] payload).
///
/// A [`ScriptedExternalSessionHandler`] pops one step per fulfilled
/// `NeedExternalSession` and returns its [`ExternalSessionResult`]. Build steps
/// from an [`ExternalAgentFixture`] (for the common
/// [`completed`](ExternalAgentFixture::completed) /
/// [`permission_pause`](ExternalAgentFixture::permission_pause) /
/// [`failed`](ExternalAgentFixture::failed) shapes) or from an explicit
/// [`ExternalSessionResult`] via [`ExternalSessionStep::result`].
#[derive(Clone, Debug)]
pub struct ExternalSessionStep {
    result: ExternalSessionResult,
}

impl ExternalSessionStep {
    /// Scripts an explicit decision-point [`ExternalSessionResult`].
    #[must_use]
    pub fn result(result: ExternalSessionResult) -> Self {
        Self { result }
    }
}

impl ScriptStep for ExternalSessionStep {
    const FAMILY: RequirementKindTag = RequirementKindTag::ExternalSession;

    fn into_result(self) -> RequirementResult {
        RequirementResult::ExternalSession(Box::new(self.result))
    }
}

/// Fulfils a `NeedExternalSession` from a [`Script`] of [`ExternalSessionStep`]s.
///
/// Every call is recorded in an observable [`ExternalAgentCallLog`]. The handler
/// advances no real runtime: it simply returns the next scripted
/// [`ExternalSessionResult`] in dispatch order, exactly as a real handler would
/// report the decision point it advanced the session to. When the script is
/// drained under [`StrictMode::Error`](crate::script::StrictMode) the exhaustion
/// folds into an [`ExternalSessionResult::Failed`] carrying an
/// [`ExternalAgentError::Runtime`], keeping the failure inside the
/// [`RequirementResult::ExternalSession`] family rather than surfacing a
/// wrong-family result.
pub struct ScriptedExternalSessionHandler {
    script: Arc<Script<ExternalSessionStep>>,
    log: Arc<ExternalAgentCallLog>,
}

impl ScriptedExternalSessionHandler {
    /// Wraps a shared `script`, tracking calls in a fresh log.
    #[must_use]
    pub fn new(script: Arc<Script<ExternalSessionStep>>) -> Self {
        Self {
            script,
            log: Arc::new(CallLog::new()),
        }
    }

    /// Builds a handler over a fresh script of `steps`.
    #[must_use]
    pub fn from_steps(steps: impl IntoIterator<Item = ExternalSessionStep>) -> Self {
        Self::new(Arc::new(Script::new(steps)))
    }

    /// Returns the shared script this handler consumes.
    #[must_use]
    pub fn script(&self) -> &Arc<Script<ExternalSessionStep>> {
        &self.script
    }

    /// Returns the shared call log recording every fulfilled call.
    #[must_use]
    pub fn log(&self) -> &Arc<ExternalAgentCallLog> {
        &self.log
    }
}

#[async_trait]
impl ExternalSessionHandler for ScriptedExternalSessionHandler {
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let ticket = self.log.begin(request.clone());
        let result = match self.script.next_step() {
            Ok(step) => step.into_result(),
            Err(error) => {
                RequirementResult::ExternalSession(Box::new(ExternalSessionResult::Failed {
                    session: None,
                    error: ExternalAgentError::Runtime {
                        code: None,
                        message: error.to_string(),
                        runtime_output: None,
                    },
                    observations: Vec::new(),
                }))
            }
        };
        self.log.complete(ticket, result.clone());
        result
    }
}

/// Builds provider-neutral external-agent effect shapes for tests.
///
/// The fixture draws identity-bearing ids ([`AgentId`],
/// [`StepId`](agent_lib::agent::StepId)) from a [`SeqIds`] handle so a whole test
/// tree stays deterministic and globally unique, matching the rest of
/// [`crate::fixtures`]. It constructs only the *data* an
/// [`ExternalSessionRequest`]/[`ExternalSessionResult`] carries; it never
/// launches a runtime.
///
/// The permission-style pause is modelled with an
/// [`Interaction::permission`] paired with a matching
/// [`PermissionRequested`](ExternalAgentEvent::PermissionRequested) observation,
/// so the runtime's `action_id` flows through the neutral
/// [`InteractionKind::Permission`](agent_lib::agent::InteractionKind::Permission)
/// request and is echoed back verbatim in the resolving
/// [`RespondInteraction`](ExternalSessionInput::RespondInteraction).
#[derive(Clone)]
pub struct ExternalAgentFixture {
    ids: SeqIds,
}

impl ExternalAgentFixture {
    /// Creates a fixture drawing ids from the same tree as `ids`.
    #[must_use]
    pub fn new(ids: &SeqIds) -> Self {
        Self { ids: ids.clone() }
    }

    /// The static policy applied to the fixture's sessions.
    #[must_use]
    pub fn policy(&self) -> ExternalSessionPolicy {
        ExternalSessionPolicy {
            permission_mode: ExternalPermissionMode::Prompt,
            isolation: WorktreeIsolation::EphemeralGitWorktree,
            max_turns: Some(8),
            stream_events: ExternalStreamPolicy::Buffered,
        }
    }

    /// A data-only [`ExternalAgentSpec`] over a Claude Code runtime with no
    /// initial tools, matching the request shapes this fixture scripts against.
    ///
    /// The runtime, worktree, empty tool set, and [`policy`](Self::policy) line
    /// up with [`start_request`](Self::start_request) /
    /// [`continue_request`](Self::continue_request), so a machine built from this
    /// spec reifies the same provider-neutral request family the scripted handler
    /// answers.
    #[must_use]
    pub fn spec(&self) -> ExternalAgentSpec {
        ExternalAgentSpec::new(
            self.ids.agent_id(),
            ExternalRuntimeKind::ClaudeCode,
            WorktreeRef::new("/repo/agent-lib"),
            None,
            ToolSetRef::new(self.ids.tool_set_id(), Vec::new()),
            self.policy(),
        )
    }

    /// Wraps [`spec`](Self::spec) in fresh [`ExternalAgentState`] over one active
    /// Conversation, ready for an [`ExternalAgentMachine`] to drive.
    #[must_use]
    pub fn agent_state(&self) -> ExternalAgentState {
        ExternalAgentState::new(
            self.spec(),
            Conversation::new(
                self.ids.conversation_id(),
                ConversationConfig::new(Some("Drive the external agent.".to_owned())),
            ),
        )
    }

    /// Builds an [`ExternalAgentMachine`] over [`agent_state`](Self::agent_state).
    ///
    /// The machine mints its `NeedExternalSession` requirement ids from the same
    /// deterministic [`SeqIds`] tree as the rest of the fixtures, so a
    /// [`DrainHarness`](crate::harness::DrainHarness) sharing that tree keeps
    /// every fabricated id globally unique.
    #[must_use]
    pub fn machine(&self) -> ExternalAgentMachine {
        ExternalAgentMachine::new(self.agent_state(), Arc::new(self.ids.clone()))
    }

    /// Builds an [`ExternalAgentMachine`] wired with a deterministic tool-execution
    /// id source, ready to bridge a runtime tool-call pause into `NeedTool`
    /// requirements.
    ///
    /// Identical to [`machine`](Self::machine) but additionally injects the shared
    /// [`SeqIds`] tree as the machine's
    /// [`ToolExecutionIds`](agent_lib::agent::ToolExecutionIds) source, so a
    /// `PausedForToolCalls` decision point can mint host tool-call ids while
    /// staying globally unique across the fixture tree.
    #[must_use]
    pub fn machine_with_tool_ids(&self) -> ExternalAgentMachine {
        ExternalAgentMachine::new(self.agent_state(), Arc::new(self.ids.clone()))
            .with_tool_execution_ids(Arc::new(self.ids.clone()))
    }

    /// A `Start` [`ExternalSessionRequest`] carrying `prompt` and no prior
    /// session.
    #[must_use]
    pub fn start_request(&self, prompt: &str) -> ExternalSessionRequest {
        ExternalSessionRequest {
            agent_id: self.ids.agent_id(),
            runtime: ExternalRuntimeKind::ClaudeCode,
            worktree: WorktreeRef::new("/repo/agent-lib"),
            session_dir: None,
            session: None,
            input: ExternalSessionInput::Start {
                prompt: prompt.to_owned(),
            },
            tools: Vec::new(),
            policy: self.policy(),
        }
    }

    /// A `Continue` [`ExternalSessionRequest`] resuming
    /// [`session_ref`](Self::session_ref) with `message`.
    #[must_use]
    pub fn continue_request(&self, message: &str) -> ExternalSessionRequest {
        ExternalSessionRequest {
            agent_id: self.ids.agent_id(),
            runtime: ExternalRuntimeKind::ClaudeCode,
            worktree: WorktreeRef::new("/repo/agent-lib"),
            session_dir: None,
            session: Some(self.session_ref()),
            input: ExternalSessionInput::Continue {
                message: message.to_owned(),
            },
            tools: Vec::new(),
            policy: self.policy(),
        }
    }

    /// Resumable facts for the fixture's Claude Code session.
    #[must_use]
    pub fn session_ref(&self) -> ExternalSessionRef {
        ExternalSessionRef {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: Some("sess-1".to_owned()),
            transcript_ref: None,
            resume_token: Some("resume-1".to_owned()),
            last_event_seq: Some(3),
        }
    }

    /// A terminal [`ExternalAgentOutput`] with `summary` and one patch artifact.
    #[must_use]
    pub fn output(&self, summary: &str) -> ExternalAgentOutput {
        ExternalAgentOutput {
            summary: summary.to_owned(),
            artifacts: vec![ExternalArtifactRef {
                kind: ExternalArtifactKind::Patch,
                summary: "parser patch".to_owned(),
                path: Some("src/parser.rs".to_owned()),
                reference: Some("diff-1".to_owned()),
            }],
            usage: None,
            cost_micros: None,
        }
    }

    /// A [`FilePatch`](ExternalAgentEvent::FilePatch) observation.
    #[must_use]
    pub fn file_patch_event(&self) -> ExternalAgentEvent {
        ExternalAgentEvent::FilePatch {
            path: "src/parser.rs".to_owned(),
            summary: "tighten the token loop".to_owned(),
            diff_ref: Some("diff-1".to_owned()),
        }
    }

    /// A successful [`CommandFinished`](ExternalAgentEvent::CommandFinished)
    /// observation.
    #[must_use]
    pub fn command_finished_event(&self) -> ExternalAgentEvent {
        ExternalAgentEvent::CommandFinished {
            exit_code: Some(0),
            stdout_tail: "test result: ok. 1 passed".to_owned(),
            stderr_tail: String::new(),
        }
    }

    /// A [`PermissionRequested`](ExternalAgentEvent::PermissionRequested)
    /// observation for `action_id`.
    #[must_use]
    pub fn permission_requested_event(&self, action_id: &str, summary: &str) -> ExternalAgentEvent {
        ExternalAgentEvent::PermissionRequested {
            action_id: action_id.to_owned(),
            summary: summary.to_owned(),
        }
    }

    /// A [`Completed`](ExternalSessionResult::Completed) result carrying a
    /// command-then-patch observation trail.
    #[must_use]
    pub fn completed(&self) -> ExternalSessionResult {
        ExternalSessionResult::Completed {
            session: self.session_ref(),
            output: self.output("refactor complete"),
            observations: ExternalObservedEvent::unsequenced_for_tests(vec![
                self.command_finished_event(),
                self.file_patch_event(),
            ]),
        }
    }

    /// The [`PermissionRequest`] a [`permission_pause`](Self::permission_pause)
    /// asks the host to resolve.
    ///
    /// Its [`action_id`](PermissionRequest::action_id) is the runtime handle
    /// (`"act-1"`) the pause echoes back through a
    /// [`RespondInteraction`](ExternalSessionInput::RespondInteraction), and it
    /// matches the [`PermissionRequested`](ExternalAgentEvent::PermissionRequested)
    /// observation carried alongside the pause.
    #[must_use]
    pub fn permission_request(&self) -> PermissionRequest {
        PermissionRequest::new(
            "act-1".to_owned(),
            self.ids.agent_id(),
            PermissionCategory::Shell,
            "run `cargo test`".to_owned(),
            serde_json::json!({ "command": "cargo test" }),
            PermissionRisk::Medium,
            Some("verify the refactor".to_owned()),
        )
    }

    /// A [`PausedForInteraction`](ExternalSessionResult::PausedForInteraction)
    /// result modelling a permission prompt.
    ///
    /// The paused interaction is an [`Interaction::permission`] carrying the
    /// fixture's [`permission_request`](Self::permission_request), and the result
    /// carries the runtime's `action_id` (`"act-1"`) that a
    /// [`RespondInteraction`](ExternalSessionInput::RespondInteraction) echoes
    /// back. The matching
    /// [`PermissionRequested`](ExternalAgentEvent::PermissionRequested)
    /// observation repeats that same `action_id`.
    #[must_use]
    pub fn permission_pause(&self) -> ExternalSessionResult {
        ExternalSessionResult::PausedForInteraction {
            session: self.session_ref(),
            action_id: "act-1".to_owned(),
            request: Interaction::permission(self.ids.step_id(), self.permission_request()),
            observations: ExternalObservedEvent::unsequenced_for_tests(vec![
                self.permission_requested_event("act-1", "run `cargo test`"),
            ]),
        }
    }

    /// One runtime subagent spawn [`ExternalSubagentRequest`] tagged with
    /// `request_id`, reusing the fixture agent as the child spec.
    ///
    /// The host bridges this into a standard `NeedSubagent` (reusing its
    /// [`spec_ref`](ExternalSubagentRequest::spec_ref),
    /// [`brief`](ExternalSubagentRequest::brief), and
    /// [`result_schema`](ExternalSubagentRequest::result_schema)), drives the
    /// child under its own subagent machinery, and feeds the result back through
    /// a [`RespondSubagent`](ExternalSessionInput::RespondSubagent) echoing the
    /// same `request_id`.
    #[must_use]
    pub fn subagent_request(&self, request_id: &str) -> ExternalSubagentRequest {
        ExternalSubagentRequest {
            request_id: ExternalSubagentRequestId::new(request_id),
            spec_ref: AgentSpecRef(self.ids.agent_id()),
            brief: Interaction::question(
                self.ids.step_id(),
                "Investigate the flaky test.".to_owned(),
            ),
            result_schema: Some(serde_json::json!({ "type": "object" })),
            raw: None,
        }
    }

    /// A [`PausedForSubagent`](ExternalSessionResult::PausedForSubagent) result
    /// carrying the fixture's [`subagent_request`](Self::subagent_request) under
    /// `request_id`.
    ///
    /// The machine bridges the carried request into one `NeedSubagent`, whose
    /// child a host [`DrivingSubagentHandler`](agent_lib::agent::DrivingSubagentHandler)
    /// drives; its output is relayed back to the runtime through a
    /// [`RespondSubagent`](ExternalSessionInput::RespondSubagent).
    #[must_use]
    pub fn subagent_pause(&self, request_id: &str) -> ExternalSessionResult {
        ExternalSessionResult::PausedForSubagent {
            session: self.session_ref(),
            request: self.subagent_request(request_id),
            observations: ExternalObservedEvent::unsequenced_for_tests(vec![
                self.command_finished_event(),
            ]),
        }
    }

    /// The batch id a [`tool_call`](Self::tool_call) pause groups its calls under.
    #[must_use]
    pub fn tool_batch_id(&self) -> ExternalToolBatchId {
        ExternalToolBatchId::new("batch-1")
    }

    /// One runtime [`ExternalToolCall`] correlated by `provider_call_id` and
    /// selecting a `name`d tool over a fixed read-only input.
    ///
    /// The machine bridges this into a provider-neutral
    /// [`ToolCall`](agent_lib::model::tool::ToolCall) whose `id` is
    /// `provider_call_id`, so a scripted tool handler keys its result by the same
    /// id and the machine echoes it back in the
    /// [`RespondToolResults`](ExternalSessionInput::RespondToolResults) batch.
    #[must_use]
    pub fn tool_call(&self, provider_call_id: &str, name: &str) -> ExternalToolCall {
        ExternalToolCall {
            provider_call_id: provider_call_id.to_owned(),
            name: name.to_owned(),
            input: serde_json::json!({ "path": "src/parser.rs" }),
            raw: None,
        }
    }

    /// A [`Failed`](ExternalSessionResult::Failed) result reporting a limit
    /// breach, retaining the session facts and a trailing observation.
    #[must_use]
    pub fn failed(&self) -> ExternalSessionResult {
        ExternalSessionResult::Failed {
            session: Some(self.session_ref()),
            error: ExternalAgentError::LimitExceeded {
                limit: "max_turns=8".to_owned(),
            },
            observations: ExternalObservedEvent::unsequenced_for_tests(vec![
                self.command_finished_event(),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ExternalAgentFixture, ExternalSessionStep, ScriptedExternalSessionHandler};
    use crate::assertions::{ExternalInputKind, ExternalResultKind, assert_external_calls};
    use crate::fixtures::root_context;
    use crate::ids::SeqIds;
    use agent_lib::agent::external::{
        ExternalAgentError, ExternalAgentEvent, ExternalObservedEvent, ExternalSessionResult,
    };
    use agent_lib::agent::{ExternalSessionHandler, RequirementResult};

    /// Unwraps the boxed external result from a family-aligned requirement result.
    fn external_result(result: &RequirementResult) -> &ExternalSessionResult {
        match result {
            RequirementResult::ExternalSession(boxed) => boxed.as_ref(),
            other => panic!("expected an ExternalSession result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn returns_scripted_results_in_dispatch_order() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let fixture = ExternalAgentFixture::new(&ids);

        let handler = ScriptedExternalSessionHandler::from_steps([
            ExternalSessionStep::result(fixture.completed()),
            ExternalSessionStep::result(fixture.permission_pause()),
        ]);

        let first = handler
            .fulfill(&fixture.start_request("refactor the parser"), &ctx)
            .await;
        assert!(matches!(
            external_result(&first),
            ExternalSessionResult::Completed { .. }
        ));

        let second = handler
            .fulfill(&fixture.continue_request("keep going"), &ctx)
            .await;
        assert!(matches!(
            external_result(&second),
            ExternalSessionResult::PausedForInteraction { .. }
        ));

        assert_external_calls(handler.log())
            .count(2)
            .all_completed()
            .completion_order(&[0, 1])
            .input_kinds(&[ExternalInputKind::Start, ExternalInputKind::Continue])
            .result_kinds(&[
                ExternalResultKind::Completed,
                ExternalResultKind::PausedForInteraction,
            ]);
    }

    #[tokio::test]
    async fn completed_result_carries_structured_observations() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let fixture = ExternalAgentFixture::new(&ids);

        let handler = ScriptedExternalSessionHandler::from_steps([ExternalSessionStep::result(
            fixture.completed(),
        )]);

        let result = handler
            .fulfill(&fixture.start_request("refactor the parser"), &ctx)
            .await;

        let ExternalSessionResult::Completed { observations, .. } = external_result(&result) else {
            panic!("the scripted step returns a Completed result");
        };
        assert!(matches!(
            observations.as_slice(),
            [
                ExternalObservedEvent {
                    event: ExternalAgentEvent::CommandFinished { .. },
                    ..
                },
                ExternalObservedEvent {
                    event: ExternalAgentEvent::FilePatch { .. },
                    ..
                },
            ]
        ));
    }

    #[tokio::test]
    async fn exhausted_script_folds_into_family_aligned_failure() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let fixture = ExternalAgentFixture::new(&ids);

        // An empty script is drained on the first call.
        let handler = ScriptedExternalSessionHandler::from_steps([]);

        let result = handler
            .fulfill(&fixture.start_request("refactor the parser"), &ctx)
            .await;

        let failed = external_result(&result);
        assert!(
            matches!(
                failed,
                ExternalSessionResult::Failed {
                    error: ExternalAgentError::Runtime { .. },
                    ..
                }
            ),
            "exhaustion stays in-family as a Failed(Runtime), got {failed:?}"
        );

        assert_external_calls(handler.log())
            .count(1)
            .all_completed()
            .result_kinds(&[ExternalResultKind::Failed]);
    }
}
