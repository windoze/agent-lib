//! Runtime adapter abstraction that fulfills an external session effect.
//!
//! [`ExternalSessionHandler`](crate::agent::ExternalSessionHandler) is the effect
//! boundary the [`ExternalAgentMachine`](super::ExternalAgentMachine) hands
//! `NeedExternalSession` work to. This module defines the layer *beneath* that
//! handler — the pieces a production handler composes so it holds no machine
//! state itself (design §11):
//!
//! - [`ExternalRuntimeAdapter`] is the per-runtime factory: it reports the
//!   runtime [`kind`](ExternalRuntimeAdapter::kind) and its
//!   [`capabilities`](ExternalRuntimeAdapter::capabilities), and starts or
//!   resumes a live [`ExternalRuntimeSession`].
//! - [`ExternalRuntimeSession`] is one live session. It advances to the next
//!   [`RuntimeDecisionPoint`] and closes with a classified
//!   [`ExternalSessionShutdown`](super::ExternalSessionShutdown).
//! - [`RuntimeDecisionPoint`] is the adapter-internal counterpart of
//!   [`ExternalSessionResult`](super::ExternalSessionResult): it carries only the
//!   *non-failure* decision points, because an adapter reports failure through a
//!   `Result`'s [`Err`] arm ([`ExternalAgentError`]) rather than a value variant.
//!   The conversion back to the serde DTO lives here.
//!
//! # Sans-io boundary
//!
//! These traits live on the **handler/driver** side, never inside the machine.
//! The machine only reifies requirements; the adapter is where real IO (spawning
//! a CLI, decoding a stream, killing a process) happens. Live session handles are
//! owned by an [`ExternalSessionRegistry`](super::ExternalSessionRegistry), never
//! serialized into [`ExternalAgentState`](super::ExternalAgentState) (design
//! §4.2, §6.4).
//!
//! # Trait object safety
//!
//! Both [`ExternalRuntimeAdapter`] and [`ExternalRuntimeSession`] are
//! object-safe: their only non-`async` methods return owned values and take no
//! generic parameters, and their `async` methods are desugared by
//! [`async_trait`] into `Pin<Box<dyn Future>>`-returning methods. That lets a
//! registry hold an adapter as `Arc<dyn ExternalRuntimeAdapter>` and a live
//! session as `Box<dyn ExternalRuntimeSession>` without naming a concrete type.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::RunContext;

use crate::agent::interaction::Interaction;

use super::{
    ExternalAgentError, ExternalAgentOutput, ExternalEventSink, ExternalObservedEvent,
    ExternalRuntimeCapabilities, ExternalRuntimeKind, ExternalSessionInput, ExternalSessionRef,
    ExternalSessionRequest, ExternalSessionResult, ExternalSessionShutdown,
    ExternalSubagentRequest, ExternalToolBatchId, ExternalToolCall,
};

/// A non-failure decision point an adapter advanced a live session to.
///
/// This is the adapter-internal mirror of
/// [`ExternalSessionResult`](super::ExternalSessionResult) with the
/// [`Failed`](super::ExternalSessionResult::Failed) arm removed: an adapter
/// signals failure by returning `Err(`[`ExternalAgentError`]`)` from
/// [`advance`](ExternalRuntimeSession::advance) instead of a value variant, so
/// the four variants here are exactly the ways a session *successfully* reached
/// its next control-flow transfer. A handler converts the whole
/// `Result<RuntimeDecisionPoint, ExternalAgentError>` into an
/// [`ExternalSessionResult`] via the [`From`] conversion on that `Result`, which
/// folds the `Err` arm into `Failed`.
///
/// Each variant carries the same `observations` buffer the effect DTO does, so
/// the sequenced [`ExternalObservedEvent`] stream survives the conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeDecisionPoint {
    /// The session produced output and needs no further input this step.
    Completed {
        /// Updated resumable session facts.
        session: ExternalSessionRef,
        /// Terminal output of the session step.
        output: ExternalAgentOutput,
        /// Events observed before completion.
        observations: Vec<ExternalObservedEvent>,
    },
    /// The session paused awaiting an interaction (approval or clarification).
    PausedForInteraction {
        /// Updated resumable session facts.
        session: ExternalSessionRef,
        /// Runtime-assigned handle for the paused action, echoed back on resume.
        action_id: String,
        /// The interaction the host must resolve.
        request: Interaction,
        /// Events observed before the pause.
        observations: Vec<ExternalObservedEvent>,
    },
    /// The session paused awaiting host execution of a batch of tool calls.
    PausedForToolCalls {
        /// Updated resumable session facts.
        session: ExternalSessionRef,
        /// Identifier the matching results echo back.
        batch_id: ExternalToolBatchId,
        /// Tool calls the host must execute this step.
        calls: Vec<ExternalToolCall>,
        /// Events observed before the pause.
        observations: Vec<ExternalObservedEvent>,
    },
    /// The session paused awaiting host execution of a subagent spawn request.
    PausedForSubagent {
        /// Updated resumable session facts.
        session: ExternalSessionRef,
        /// The subagent spawn the host must drive this step.
        request: ExternalSubagentRequest,
        /// Events observed before the pause.
        observations: Vec<ExternalObservedEvent>,
    },
}

impl RuntimeDecisionPoint {
    /// Returns the resumable session facts this decision point rests on.
    #[must_use]
    pub const fn session(&self) -> &ExternalSessionRef {
        match self {
            RuntimeDecisionPoint::Completed { session, .. }
            | RuntimeDecisionPoint::PausedForInteraction { session, .. }
            | RuntimeDecisionPoint::PausedForToolCalls { session, .. }
            | RuntimeDecisionPoint::PausedForSubagent { session, .. } => session,
        }
    }

    /// Returns the sequenced events observed before this decision point.
    #[must_use]
    pub fn observations(&self) -> &[ExternalObservedEvent] {
        match self {
            RuntimeDecisionPoint::Completed { observations, .. }
            | RuntimeDecisionPoint::PausedForInteraction { observations, .. }
            | RuntimeDecisionPoint::PausedForToolCalls { observations, .. }
            | RuntimeDecisionPoint::PausedForSubagent { observations, .. } => observations,
        }
    }

    /// Converts this non-failure decision point into the serde effect DTO.
    ///
    /// The mapping is one-to-one onto the non-[`Failed`](ExternalSessionResult::Failed)
    /// variants of [`ExternalSessionResult`]; a handler uses this (or the
    /// [`From`] on `Result`) to hand the machine a
    /// [`RequirementResult::ExternalSession`](crate::agent::RequirementResult::ExternalSession).
    #[must_use]
    pub fn into_session_result(self) -> ExternalSessionResult {
        match self {
            RuntimeDecisionPoint::Completed {
                session,
                output,
                observations,
            } => ExternalSessionResult::Completed {
                session,
                output,
                observations,
            },
            RuntimeDecisionPoint::PausedForInteraction {
                session,
                action_id,
                request,
                observations,
            } => ExternalSessionResult::PausedForInteraction {
                session,
                action_id,
                request,
                observations,
            },
            RuntimeDecisionPoint::PausedForToolCalls {
                session,
                batch_id,
                calls,
                observations,
            } => ExternalSessionResult::PausedForToolCalls {
                session,
                batch_id,
                calls,
                observations,
            },
            RuntimeDecisionPoint::PausedForSubagent {
                session,
                request,
                observations,
            } => ExternalSessionResult::PausedForSubagent {
                session,
                request,
                observations,
            },
        }
    }
}

/// Folds an adapter outcome into the serde effect DTO.
///
/// `Ok(point)` maps to the matching non-failure
/// [`ExternalSessionResult`] variant; `Err(error)` maps to
/// [`Failed`](ExternalSessionResult::Failed). The session facts an error carries
/// (a lost, unresumable, or shutdown-failed session) are lifted into the
/// `Failed` variant's optional `session` so a scheduler still learns which
/// session was affected, and `observations` is left empty because a failing
/// `Result` carries none.
impl From<Result<RuntimeDecisionPoint, ExternalAgentError>> for ExternalSessionResult {
    fn from(result: Result<RuntimeDecisionPoint, ExternalAgentError>) -> Self {
        match result {
            Ok(point) => point.into_session_result(),
            Err(error) => {
                let session = session_from_error(&error);
                ExternalSessionResult::Failed {
                    session,
                    error,
                    observations: Vec::new(),
                }
            }
        }
    }
}

/// Extracts the affected session facts from a classified adapter error, when the
/// error variant records one.
fn session_from_error(error: &ExternalAgentError) -> Option<ExternalSessionRef> {
    match error {
        ExternalAgentError::SessionLost { session, .. } => session.clone(),
        ExternalAgentError::ResumeUnavailable { session, .. }
        | ExternalAgentError::ShutdownFailed { session, .. } => Some(session.clone()),
        ExternalAgentError::Launch { .. }
        | ExternalAgentError::Protocol { .. }
        | ExternalAgentError::LimitExceeded { .. }
        | ExternalAgentError::Runtime { .. }
        | ExternalAgentError::UnsupportedCapability { .. } => None,
    }
}

/// One live external-runtime session driven to its next decision point.
///
/// A session owns whatever live IO backs it (a CLI child process, an SDK client,
/// a stream reader task). It is created by an [`ExternalRuntimeAdapter`] and held
/// by an [`ExternalSessionRegistry`](super::ExternalSessionRegistry); it never
/// appears in [`ExternalAgentState`](super::ExternalAgentState).
///
/// # Contract
///
/// - [`session_ref`](Self::session_ref) must always report a
///   [`session_id`](ExternalSessionRef::session_id) once the adapter has started
///   or resumed the session, so a registry can key the live handle by it.
/// - [`advance`](Self::advance) drives the session to its next
///   [`RuntimeDecisionPoint`] or returns a classified [`ExternalAgentError`]; it
///   must not run the session to completion in one blocking call.
/// - [`shutdown`](Self::shutdown) closes the live IO and classifies how the close
///   went as an [`ExternalSessionShutdown`].
#[async_trait]
pub trait ExternalRuntimeSession: Send {
    /// Returns the current resumable facts for this session.
    ///
    /// The returned [`ExternalSessionRef`] tracks the runtime-assigned session
    /// id, transcript/resume tokens, and the last consumed event sequence, and
    /// advances as the session does.
    fn session_ref(&self) -> ExternalSessionRef;

    /// Advances the session by `input` to its next decision point.
    ///
    /// Returns the reached [`RuntimeDecisionPoint`], or a classified
    /// [`ExternalAgentError`] when the runtime failed, the session was lost, or a
    /// required capability is unavailable.
    ///
    /// # Errors
    ///
    /// Returns an [`ExternalAgentError`] describing the failure; the caller folds
    /// it into [`ExternalSessionResult::Failed`].
    async fn advance(
        &mut self,
        input: &ExternalSessionInput,
        ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError>;

    /// Closes the live session and reports how the close went.
    ///
    /// This is the graceful path; the never-resume cancel path force-closes the
    /// session through the registry instead. The returned
    /// [`ExternalSessionShutdown`] tells a scheduler whether residual side
    /// effects may remain (design §6.4).
    async fn shutdown(&mut self) -> ExternalSessionShutdown;
}

/// A per-runtime factory that starts and resumes live sessions.
///
/// One adapter backs one [`ExternalRuntimeKind`]. It reports the managed
/// features its sessions can fulfill via [`capabilities`](Self::capabilities) so
/// a host can gate work before dispatching, and it turns an
/// [`ExternalSessionRequest`] into a live [`ExternalRuntimeSession`]. A
/// production [`ExternalSessionHandler`](crate::agent::ExternalSessionHandler)
/// composes an adapter with an [`ExternalSessionRegistry`](super::ExternalSessionRegistry)
/// and holds no machine state of its own.
///
/// # Errors
///
/// Every fallible method returns a classified [`ExternalAgentError`] (launch
/// failure, protocol drift, unresumable session, …) rather than an ad-hoc error
/// type, so a caller can map the failure uniformly.
#[async_trait]
pub trait ExternalRuntimeAdapter: Send + Sync {
    /// Returns the runtime kind this adapter drives.
    fn kind(&self) -> ExternalRuntimeKind;

    /// Returns the managed features this adapter's sessions can fulfill.
    ///
    /// The baseline is conservative
    /// ([`ExternalRuntimeCapabilities::none`](super::ExternalRuntimeCapabilities::none));
    /// an adapter flips on only what a probe confirmed (design §15).
    fn capabilities(&self) -> ExternalRuntimeCapabilities;

    /// Starts a brand-new session for `request` and returns its live handle.
    ///
    /// The returned session must expose a
    /// [`session_id`](ExternalSessionRef::session_id) through
    /// [`session_ref`](ExternalRuntimeSession::session_ref) so a registry can key
    /// the live handle. The optional `sink` receives sequenced observations live
    /// as the session decodes them; it is a lossy side channel and never blocks
    /// the continuation.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Launch`] when the runtime cannot start, or
    /// another classified variant for early protocol failures.
    async fn start(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError>;

    /// Resumes a previously-started session from its resumable facts.
    ///
    /// This is the cross-process reattach path used when no live handle is
    /// registered for `session`. The default implementation refuses with
    /// [`ExternalAgentError::ResumeUnavailable`]; an adapter whose
    /// [`capabilities`](Self::capabilities) report
    /// [`resume`](super::ExternalRuntimeCapabilities::resume) must override it.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::ResumeUnavailable`] when the session,
    /// transcript, or resume token is no longer valid (the default always does).
    async fn resume(
        &self,
        session: &ExternalSessionRef,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
        let _ = (request, ctx, sink);
        Err(ExternalAgentError::ResumeUnavailable {
            session: session.clone(),
            detail: format!(
                "{:?} runtime adapter does not implement resume",
                self.kind()
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeDecisionPoint;
    use crate::agent::ExternalAgentEvent;
    use crate::agent::external::{
        ExternalAgentError, ExternalAgentOutput, ExternalObservedEvent, ExternalRuntimeKind,
        ExternalSessionRef, ExternalSessionResult, ExternalSubagentRequest,
        ExternalSubagentRequestId, ExternalToolBatchId, ExternalToolCall,
    };
    use crate::agent::interaction::Interaction;
    use crate::agent::{AgentSpecRef, StepId};

    fn session_ref(id: &str) -> ExternalSessionRef {
        ExternalSessionRef {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: Some(id.to_owned()),
            transcript_ref: None,
            resume_token: None,
            last_event_seq: Some(3),
        }
    }

    fn observations() -> Vec<ExternalObservedEvent> {
        ExternalObservedEvent::unsequenced_for_tests(vec![ExternalAgentEvent::TextDelta {
            text: "hi".to_owned(),
        }])
    }

    fn step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-40000000ab01"
            .parse()
            .expect("step id")
    }

    fn spec_ref() -> AgentSpecRef {
        let agent_id = "018f0d9c-7b6a-7c12-8f31-40000000ab02"
            .parse()
            .expect("agent id");
        AgentSpecRef(agent_id)
    }

    #[test]
    fn completed_decision_point_maps_to_completed_result() {
        let output = ExternalAgentOutput {
            summary: "done".to_owned(),
            artifacts: Vec::new(),
            usage: None,
            cost_micros: None,
        };
        let point = RuntimeDecisionPoint::Completed {
            session: session_ref("s1"),
            output: output.clone(),
            observations: observations(),
        };
        assert_eq!(point.session(), &session_ref("s1"));
        assert_eq!(point.observations(), observations().as_slice());

        match point.into_session_result() {
            ExternalSessionResult::Completed {
                session,
                output: got,
                observations: obs,
            } => {
                assert_eq!(session, session_ref("s1"));
                assert_eq!(got, output);
                assert_eq!(obs, observations());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn paused_for_interaction_decision_point_preserves_action_id() {
        let request = Interaction::question(step_id(), "approve?".to_owned());
        let point = RuntimeDecisionPoint::PausedForInteraction {
            session: session_ref("s2"),
            action_id: "action-7".to_owned(),
            request: request.clone(),
            observations: observations(),
        };
        match point.into_session_result() {
            ExternalSessionResult::PausedForInteraction {
                session,
                action_id,
                request: got,
                observations: obs,
            } => {
                assert_eq!(session, session_ref("s2"));
                assert_eq!(action_id, "action-7");
                assert_eq!(got, request);
                assert_eq!(obs, observations());
            }
            other => panic!("expected PausedForInteraction, got {other:?}"),
        }
    }

    #[test]
    fn paused_for_tool_calls_decision_point_preserves_batch() {
        let batch_id = ExternalToolBatchId::new("batch-1");
        let calls = vec![ExternalToolCall {
            provider_call_id: "call-1".to_owned(),
            name: "apply_patch".to_owned(),
            input: serde_json::json!({ "path": "x" }),
            raw: None,
        }];
        let point = RuntimeDecisionPoint::PausedForToolCalls {
            session: session_ref("s3"),
            batch_id: batch_id.clone(),
            calls: calls.clone(),
            observations: observations(),
        };
        match point.into_session_result() {
            ExternalSessionResult::PausedForToolCalls {
                session,
                batch_id: got_batch,
                calls: got_calls,
                observations: obs,
            } => {
                assert_eq!(session, session_ref("s3"));
                assert_eq!(got_batch, batch_id);
                assert_eq!(got_calls, calls);
                assert_eq!(obs, observations());
            }
            other => panic!("expected PausedForToolCalls, got {other:?}"),
        }
    }

    #[test]
    fn paused_for_subagent_decision_point_preserves_request() {
        let request = ExternalSubagentRequest {
            request_id: ExternalSubagentRequestId::new("req-9"),
            spec_ref: spec_ref(),
            brief: Interaction::question(step_id(), "do subtask".to_owned()),
            result_schema: None,
            raw: None,
        };
        let point = RuntimeDecisionPoint::PausedForSubagent {
            session: session_ref("s4"),
            request: request.clone(),
            observations: observations(),
        };
        match point.into_session_result() {
            ExternalSessionResult::PausedForSubagent {
                session,
                request: got,
                observations: obs,
            } => {
                assert_eq!(session, session_ref("s4"));
                assert_eq!(got, request);
                assert_eq!(obs, observations());
            }
            other => panic!("expected PausedForSubagent, got {other:?}"),
        }
    }

    #[test]
    fn ok_result_folds_into_matching_session_result() {
        let output = ExternalAgentOutput {
            summary: "done".to_owned(),
            artifacts: Vec::new(),
            usage: None,
            cost_micros: None,
        };
        let result: Result<RuntimeDecisionPoint, ExternalAgentError> =
            Ok(RuntimeDecisionPoint::Completed {
                session: session_ref("s5"),
                output,
                observations: Vec::new(),
            });
        assert!(matches!(
            ExternalSessionResult::from(result),
            ExternalSessionResult::Completed { .. }
        ));
    }

    #[test]
    fn err_result_with_session_lifts_it_into_failed() {
        // ResumeUnavailable carries a non-optional session, which must surface on
        // the folded Failed variant so a scheduler learns which session failed.
        let result: Result<RuntimeDecisionPoint, ExternalAgentError> =
            Err(ExternalAgentError::ResumeUnavailable {
                session: session_ref("s6"),
                detail: "gone".to_owned(),
            });
        match ExternalSessionResult::from(result) {
            ExternalSessionResult::Failed {
                session,
                error,
                observations,
            } => {
                assert_eq!(session, Some(session_ref("s6")));
                assert!(matches!(
                    error,
                    ExternalAgentError::ResumeUnavailable { .. }
                ));
                assert!(observations.is_empty());
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn err_result_without_session_folds_to_failed_with_none() {
        // A launch failure carries no session reference, so Failed's session is
        // None while still carrying the classified error.
        let result: Result<RuntimeDecisionPoint, ExternalAgentError> =
            Err(ExternalAgentError::Launch {
                runtime: ExternalRuntimeKind::ClaudeCode,
                detail: "missing binary".to_owned(),
            });
        match ExternalSessionResult::from(result) {
            ExternalSessionResult::Failed {
                session,
                error,
                observations,
            } => {
                assert!(session.is_none());
                assert!(matches!(error, ExternalAgentError::Launch { .. }));
                assert!(observations.is_empty());
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
