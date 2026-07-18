//! Read-only assertions over a scripted external session handler's call log.
//!
//! [`assert_external_calls`] wraps the [`ExternalAgentCallLog`] a
//! [`ScriptedExternalSessionHandler`](crate::external::ScriptedExternalSessionHandler)
//! exposes through its `log()` accessor. It reuses the family-neutral checks from
//! [`assert_calls`](crate::assertions::assert_calls) (call count, completion
//! count, completion order) and adds the external-session-specific *summaries*
//! design §12 asks for: the [`ExternalSessionInput`] kind of each recorded
//! request and the [`ExternalSessionResult`] kind of each recorded result, both
//! in dispatch order.

use agent_lib::agent::RequirementResult;
use agent_lib::agent::external::{ExternalSessionInput, ExternalSessionResult};

use crate::assertions::assert_calls;
use crate::external::ExternalAgentCallLog;

/// A dispatch-order summary of one recorded [`ExternalSessionInput`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalInputKind {
    /// The request began a fresh session
    /// ([`ExternalSessionInput::Start`]).
    Start,
    /// The request continued an existing session
    /// ([`ExternalSessionInput::Continue`]).
    Continue,
    /// The request fed a resolved interaction back
    /// ([`ExternalSessionInput::RespondInteraction`]).
    RespondInteraction,
    /// The request fed host tool-execution results back
    /// ([`ExternalSessionInput::RespondToolResults`]).
    RespondToolResults,
    /// The request fed a host subagent result back
    /// ([`ExternalSessionInput::RespondSubagent`]).
    RespondSubagent,
    /// The request shut the session down
    /// ([`ExternalSessionInput::Shutdown`]).
    Shutdown,
}

/// A dispatch-order summary of one recorded [`ExternalSessionResult`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalResultKind {
    /// The session completed a step
    /// ([`ExternalSessionResult::Completed`]).
    Completed,
    /// The session paused awaiting an interaction
    /// ([`ExternalSessionResult::PausedForInteraction`]).
    PausedForInteraction,
    /// The session paused awaiting host tool execution
    /// ([`ExternalSessionResult::PausedForToolCalls`]).
    PausedForToolCalls,
    /// The session paused awaiting a host subagent
    /// ([`ExternalSessionResult::PausedForSubagent`]).
    PausedForSubagent,
    /// The session failed ([`ExternalSessionResult::Failed`]).
    Failed,
}

/// Summarises an [`ExternalSessionInput`] as its [`ExternalInputKind`].
fn input_kind(input: &ExternalSessionInput) -> ExternalInputKind {
    match input {
        ExternalSessionInput::Start { .. } => ExternalInputKind::Start,
        ExternalSessionInput::Continue { .. } => ExternalInputKind::Continue,
        ExternalSessionInput::RespondInteraction { .. } => ExternalInputKind::RespondInteraction,
        ExternalSessionInput::RespondToolResults { .. } => ExternalInputKind::RespondToolResults,
        ExternalSessionInput::RespondSubagent { .. } => ExternalInputKind::RespondSubagent,
        ExternalSessionInput::Shutdown => ExternalInputKind::Shutdown,
    }
}

/// Summarises an [`ExternalSessionResult`] as its [`ExternalResultKind`].
fn result_kind(result: &ExternalSessionResult) -> ExternalResultKind {
    match result {
        ExternalSessionResult::Completed { .. } => ExternalResultKind::Completed,
        ExternalSessionResult::PausedForInteraction { .. } => {
            ExternalResultKind::PausedForInteraction
        }
        ExternalSessionResult::PausedForToolCalls { .. } => ExternalResultKind::PausedForToolCalls,
        ExternalSessionResult::PausedForSubagent { .. } => ExternalResultKind::PausedForSubagent,
        ExternalSessionResult::Failed { .. } => ExternalResultKind::Failed,
    }
}

/// Starts a fluent, read-only assertion over an external session handler's call
/// log.
#[must_use]
pub fn assert_external_calls(log: &ExternalAgentCallLog) -> ExternalAgentCallAssertions<'_> {
    ExternalAgentCallAssertions { log }
}

/// A fluent, read-only assertion builder over an [`ExternalAgentCallLog`].
///
/// The count/completion checks delegate to
/// [`assert_calls`](crate::assertions::assert_calls); the
/// [`input_kinds`](Self::input_kinds) and [`result_kinds`](Self::result_kinds)
/// checks add the external-session request/result summaries.
pub struct ExternalAgentCallAssertions<'a> {
    log: &'a ExternalAgentCallLog,
}

impl<'a> Clone for ExternalAgentCallAssertions<'a> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a> Copy for ExternalAgentCallAssertions<'a> {}

impl<'a> ExternalAgentCallAssertions<'a> {
    /// Asserts the number of calls that began (dispatch count).
    pub fn count(self, expected: usize) -> Self {
        assert_calls(self.log).count(expected);
        self
    }

    /// Asserts the number of calls that completed.
    pub fn completed(self, expected: usize) -> Self {
        assert_calls(self.log).completed(expected);
        self
    }

    /// Asserts every begun call has completed.
    pub fn all_completed(self) -> Self {
        assert_calls(self.log).all_completed();
        self
    }

    /// Asserts the completion order (dispatch index -> completion index).
    pub fn completion_order(self, expected: &[usize]) -> Self {
        assert_calls(self.log).completion_order(expected);
        self
    }

    /// Asserts the [`ExternalInputKind`] of each recorded request, in dispatch
    /// order.
    pub fn input_kinds(self, expected: &[ExternalInputKind]) -> Self {
        let actual = self.log.with_records(|records| {
            records
                .iter()
                .map(|record| input_kind(&record.request.input))
                .collect::<Vec<_>>()
        });
        assert!(
            actual == expected,
            "expected request input kinds {expected:?}, found {actual:?}"
        );
        self
    }

    /// Asserts the [`ExternalResultKind`] of each recorded result, in dispatch
    /// order. Requires every recorded call to have completed with an
    /// [`RequirementResult::ExternalSession`] result.
    pub fn result_kinds(self, expected: &[ExternalResultKind]) -> Self {
        let actual = self.log.with_records(|records| {
            records
                .iter()
                .map(|record| match &record.result {
                    Some(RequirementResult::ExternalSession(boxed)) => result_kind(boxed.as_ref()),
                    Some(other) => panic!(
                        "call dispatched at {} completed with a non-external result: {other:?}",
                        record.call_index
                    ),
                    None => panic!(
                        "call dispatched at {} has not completed yet",
                        record.call_index
                    ),
                })
                .collect::<Vec<_>>()
        });
        assert!(
            actual == expected,
            "expected result kinds {expected:?}, found {actual:?}"
        );
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{ExternalInputKind, ExternalResultKind, assert_external_calls};
    use crate::external::ExternalAgentCallLog;
    use agent_lib::agent::RequirementResult;
    use agent_lib::agent::external::{
        ExternalAgentError, ExternalRuntimeKind, ExternalSessionInput, ExternalSessionPolicy,
        ExternalSessionRef, ExternalSessionRequest, ExternalSessionResult,
    };
    use agent_lib::agent::{
        ExternalPermissionMode, ExternalStreamPolicy, WorktreeIsolation, WorktreeRef,
    };

    fn request(input: ExternalSessionInput) -> ExternalSessionRequest {
        let ids = crate::ids::SeqIds::new();
        ExternalSessionRequest {
            agent_id: ids.agent_id(),
            runtime: ExternalRuntimeKind::ClaudeCode,
            worktree: WorktreeRef::new("/repo/agent-lib"),
            session: None,
            input,
            tools: Vec::new(),
            policy: ExternalSessionPolicy {
                permission_mode: ExternalPermissionMode::Prompt,
                isolation: WorktreeIsolation::Shared,
                max_turns: None,
                stream_events: ExternalStreamPolicy::Buffered,
            },
        }
    }

    fn completed_result() -> RequirementResult {
        RequirementResult::ExternalSession(Box::new(ExternalSessionResult::Failed {
            session: None,
            error: ExternalAgentError::Runtime {
                code: None,
                message: "boom".to_owned(),
                runtime_output: None,
            },
            observations: Vec::new(),
        }))
    }

    fn session_completed() -> RequirementResult {
        RequirementResult::ExternalSession(Box::new(ExternalSessionResult::Completed {
            session: ExternalSessionRef {
                runtime: ExternalRuntimeKind::ClaudeCode,
                session_id: None,
                transcript_ref: None,
                resume_token: None,
                last_event_seq: None,
            },
            output: agent_lib::agent::external::ExternalAgentOutput {
                summary: "done".to_owned(),
                artifacts: Vec::new(),
                usage: None,
                cost_micros: None,
            },
            observations: Vec::new(),
        }))
    }

    #[test]
    fn summaries_track_request_and_result_kinds() {
        let log: ExternalAgentCallLog = ExternalAgentCallLog::new();
        log.record(
            request(ExternalSessionInput::Start {
                prompt: "go".to_owned(),
            }),
            session_completed(),
        );
        log.record(request(ExternalSessionInput::Shutdown), completed_result());

        assert_external_calls(&log)
            .count(2)
            .all_completed()
            .completion_order(&[0, 1])
            .input_kinds(&[ExternalInputKind::Start, ExternalInputKind::Shutdown])
            .result_kinds(&[ExternalResultKind::Completed, ExternalResultKind::Failed]);
    }

    #[test]
    fn wrong_input_kinds_panic() {
        let log: ExternalAgentCallLog = ExternalAgentCallLog::new();
        log.record(
            request(ExternalSessionInput::Start {
                prompt: "go".to_owned(),
            }),
            session_completed(),
        );

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_external_calls(&log).input_kinds(&[ExternalInputKind::Continue]);
        }))
        .expect_err("a wrong input-kind summary must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected request input kinds"),
            "message names the mismatch: {message}"
        );
    }
}
