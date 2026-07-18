//! Offline tests for [`RegistryExternalSessionHandler`](super::RegistryExternalSessionHandler).
//!
//! A scripted in-crate [`ExternalRuntimeAdapter`]/[`ExternalRuntimeSession`]
//! double drives the handler through the whole managed loop without a live CLI:
//! start → pause for interaction → resume the same live handle → complete →
//! force-close. A second double proves a launch failure folds into a
//! family-aligned [`ExternalSessionResult::Failed`] rather than the wrong
//! requirement family.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::agent::drive::ExternalSessionHandler;
use crate::agent::external::{
    ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput, ExternalEventSink,
    ExternalObservedEvent, ExternalPermissionMode, ExternalRuntimeAdapter,
    ExternalRuntimeCapabilities, ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionInput,
    ExternalSessionPolicy, ExternalSessionRef, ExternalSessionRequest, ExternalSessionResult,
    ExternalSessionShutdown, ExternalStreamPolicy, RuntimeDecisionPoint, WorktreeIsolation,
};
use crate::agent::spec::WorktreeRef;
use crate::agent::{
    AgentId, BudgetLimits, Interaction, InteractionResponse, RequirementResult, RunContext, RunId,
    StepId, TraceNodeId,
};

use super::{ExternalSessionRegistry, RegistryExternalSessionHandler};

const SESSION_ID: &str = "script-sess-1";

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f0"
        .parse()
        .expect("agent id")
}

fn run_context() -> RunContext {
    let run_id: RunId = "018f0d9c-7b6a-7c12-8f31-1234567890e0"
        .parse()
        .expect("run id");
    let trace_root = TraceNodeId::new("external-runtime-handler-root");
    RunContext::new_root(run_id, BudgetLimits::unbounded(), trace_root)
}

fn policy() -> ExternalSessionPolicy {
    // Shared isolation: handler tests exercise the fulfill/advance contract, not
    // worktree preparation (covered by the registry tests), and Shared prepares
    // without touching a real git binary.
    ExternalSessionPolicy {
        permission_mode: ExternalPermissionMode::Prompt,
        isolation: WorktreeIsolation::Shared,
        max_turns: Some(8),
        stream_events: ExternalStreamPolicy::Buffered,
    }
}

fn session_ref(session_id: &str) -> ExternalSessionRef {
    ExternalSessionRef {
        runtime: ExternalRuntimeKind::ClaudeCode,
        session_id: Some(session_id.to_owned()),
        transcript_ref: None,
        resume_token: None,
        last_event_seq: None,
    }
}

fn start_request(agent: AgentId) -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent,
        runtime: ExternalRuntimeKind::ClaudeCode,
        worktree: WorktreeRef::new("/repo/agent-lib"),
        session_dir: None,
        session: None,
        input: ExternalSessionInput::Start {
            prompt: "do the thing".to_owned(),
        },
        tools: Vec::new(),
        policy: policy(),
    }
}

fn respond_request(agent: AgentId) -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent,
        runtime: ExternalRuntimeKind::ClaudeCode,
        worktree: WorktreeRef::new("/repo/agent-lib"),
        session_dir: None,
        session: Some(session_ref(SESSION_ID)),
        input: ExternalSessionInput::RespondInteraction {
            action_id: "action-1".to_owned(),
            response: InteractionResponse::answer("approved".to_owned()),
        },
        tools: Vec::new(),
        policy: policy(),
    }
}

fn expect_external(result: RequirementResult) -> Box<ExternalSessionResult> {
    match result {
        RequirementResult::ExternalSession(result) => result,
        other => panic!("expected an external-session result, got {other:?}"),
    }
}

/// A scripted session: the first advance pauses for an interaction, the second
/// completes. It mirrors one observation to the live sink on the first advance
/// so the sink-forwarding path is exercised, and counts shutdowns so a test can
/// assert force-close reached the live IO.
struct ScriptSession {
    advances: usize,
    shutdowns: Arc<AtomicUsize>,
    sink: Option<Arc<dyn ExternalEventSink>>,
}

#[async_trait]
impl ExternalRuntimeSession for ScriptSession {
    fn session_ref(&self) -> ExternalSessionRef {
        session_ref(SESSION_ID)
    }

    async fn advance(
        &mut self,
        _input: &ExternalSessionInput,
        ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        let step = self.advances;
        self.advances += 1;
        if step == 0 {
            let observed = ExternalObservedEvent::new(
                1,
                ExternalAgentEvent::TextDelta {
                    text: "thinking".to_owned(),
                },
            );
            if let Some(sink) = &self.sink {
                sink.emit(&observed);
            }
            Ok(RuntimeDecisionPoint::PausedForInteraction {
                session: session_ref(SESSION_ID),
                action_id: "action-1".to_owned(),
                request: Interaction::question(
                    StepId::new(*ctx.run_id().as_uuid()),
                    "approve the edit?".to_owned(),
                ),
                observations: vec![observed],
            })
        } else {
            Ok(RuntimeDecisionPoint::Completed {
                session: session_ref(SESSION_ID),
                output: ExternalAgentOutput {
                    summary: "done".to_owned(),
                    artifacts: Vec::new(),
                    usage: None,
                    cost_micros: None,
                },
                observations: Vec::new(),
            })
        }
    }

    async fn shutdown(&mut self) -> ExternalSessionShutdown {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        ExternalSessionShutdown::Graceful
    }
}

/// A scripted adapter that hands out one [`ScriptSession`] per start, or fails
/// the launch when `fail_start` is set.
struct ScriptAdapter {
    starts: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    fail_start: bool,
}

impl ScriptAdapter {
    fn new() -> Self {
        Self {
            starts: Arc::new(AtomicUsize::new(0)),
            shutdowns: Arc::new(AtomicUsize::new(0)),
            fail_start: false,
        }
    }

    fn failing() -> Self {
        Self {
            fail_start: true,
            ..Self::new()
        }
    }
}

#[async_trait]
impl ExternalRuntimeAdapter for ScriptAdapter {
    fn kind(&self) -> ExternalRuntimeKind {
        ExternalRuntimeKind::ClaudeCode
    }

    fn capabilities(&self) -> ExternalRuntimeCapabilities {
        // Resume stays off: the follow-up turn reattaches the same live handle,
        // never exercising the adapter's cross-process resume path.
        ExternalRuntimeCapabilities::none(ExternalRuntimeKind::ClaudeCode)
    }

    async fn start(
        &self,
        _request: &ExternalSessionRequest,
        _ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
        if self.fail_start {
            return Err(ExternalAgentError::Launch {
                runtime: ExternalRuntimeKind::ClaudeCode,
                detail: "scripted launch failure".to_owned(),
            });
        }
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(ScriptSession {
            advances: 0,
            shutdowns: Arc::clone(&self.shutdowns),
            sink,
        }))
    }

    async fn resume(
        &self,
        _session: &ExternalSessionRef,
        _request: &ExternalSessionRequest,
        _ctx: &RunContext,
        _sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
        Err(ExternalAgentError::Protocol {
            detail: "scripted adapter does not resume".to_owned(),
        })
    }
}

/// A collecting sink recording every observation the handler mirrored.
#[derive(Default)]
struct CollectingSink {
    events: Mutex<Vec<ExternalObservedEvent>>,
}

impl ExternalEventSink for CollectingSink {
    fn emit(&self, event: &ExternalObservedEvent) {
        self.events.lock().expect("sink mutex").push(event.clone());
    }
}

#[tokio::test]
async fn advances_through_pause_resume_then_force_closes() {
    let adapter = Arc::new(ScriptAdapter::new());
    let starts = Arc::clone(&adapter.starts);
    let shutdowns = Arc::clone(&adapter.shutdowns);
    let sink = Arc::new(CollectingSink::default());
    let registry = Arc::new(ExternalSessionRegistry::new(adapter));
    let handler = RegistryExternalSessionHandler::with_sink(
        Arc::clone(&registry),
        Arc::clone(&sink) as Arc<dyn ExternalEventSink>,
    );
    let ctx = run_context();
    let agent = agent_id();

    // 1. Start advances to the first decision point: a pause for interaction,
    //    registering exactly one live session and mirroring its observation.
    let paused = expect_external(handler.fulfill(&start_request(agent), &ctx).await);
    match paused.as_ref() {
        ExternalSessionResult::PausedForInteraction { observations, .. } => {
            assert_eq!(observations.len(), 1);
        }
        other => panic!("expected pause-for-interaction, got {other:?}"),
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(registry.live_len(), 1);
    assert_eq!(sink.events.lock().expect("sink mutex").len(), 1);

    // 2. The resolved interaction reattaches the *same* live handle (no second
    //    start) and advances it to completion.
    let completed = expect_external(handler.fulfill(&respond_request(agent), &ctx).await);
    match completed.as_ref() {
        ExternalSessionResult::Completed { output, .. } => {
            assert_eq!(output.summary, "done");
        }
        other => panic!("expected completed, got {other:?}"),
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(registry.live_len(), 1);

    // 3. The machine emits no shutdown effect; the host force-closes through the
    //    registry accessor, which reaches the live IO and deregisters it.
    let dispositions = handler.registry().cleanup_agent(agent).await;
    assert_eq!(dispositions, vec![ExternalSessionShutdown::Graceful]);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(registry.live_len(), 0);
}

#[tokio::test]
async fn launch_failure_folds_to_failed_result() {
    let registry = Arc::new(ExternalSessionRegistry::new(Arc::new(
        ScriptAdapter::failing(),
    )));
    let handler = RegistryExternalSessionHandler::new(Arc::clone(&registry));
    let ctx = run_context();

    let result = expect_external(handler.fulfill(&start_request(agent_id()), &ctx).await);
    assert!(
        matches!(result.as_ref(), ExternalSessionResult::Failed { .. }),
        "a launch failure must fold into a family-aligned Failed result, got {result:?}"
    );
    // Nothing was registered, so there is no live session to leak.
    assert_eq!(registry.live_len(), 0);
}
