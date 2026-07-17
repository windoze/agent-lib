//! Scripted runtime adapter for the milestone-5 external-runtime abstraction.
//!
//! Where [`ScriptedExternalSessionHandler`](super::ScriptedExternalSessionHandler)
//! short-circuits the effect boundary — it returns a pre-built
//! [`ExternalSessionResult`](agent_lib::agent::external::ExternalSessionResult)
//! without ever touching the runtime layer — this
//! module exercises the layer *beneath* the handler: the milestone-5
//! [`ExternalRuntimeAdapter`] / [`ExternalRuntimeSession`] traits and the
//! [`ExternalSessionRegistry`] that owns live handles (design §11).
//!
//! - [`ScriptedExternalRuntimeSession`] is one live session driven by a script
//!   of [`ScriptedAdvance`]s. Each [`advance`](ExternalRuntimeSession::advance)
//!   pops one scripted step, optionally asserts the
//!   [`ExternalSessionInput`] kind it was handed, mirrors the step's events to
//!   the live sink *and* buffers them as sequenced observations sharing one
//!   monotonic `seq` line, and returns the matching
//!   [`RuntimeDecisionPoint`] (or a classified [`ExternalAgentError`]).
//! - [`ScriptedExternalRuntimeAdapter`] is the factory: it hands the script to a
//!   fresh session on [`start`](ExternalRuntimeAdapter::start) and records every
//!   start request in a [`ScriptedRuntimeStartLog`].
//! - [`ScriptedRuntimeExternalSessionHandler`] composes an adapter with an
//!   [`ExternalSessionRegistry`] into a production-shaped
//!   [`ExternalSessionHandler`]: it holds no machine state, `get_or_start`s the
//!   live handle (starting the first turn, reattaching every follow-up turn),
//!   advances it one decision point, and folds the outcome into a family-aligned
//!   [`RequirementResult::ExternalSession`].
//!
//! Build the trio with [`ScriptedRuntimeBuilder`]. The result drives the whole
//! managed loop offline — start, host tool batch, interaction, and subagent
//! bridges — through a real registry, so agent-layer tests can prove the
//! milestone-5 boundary without a live CLI.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use agent_lib::agent::external::{
    ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput, ExternalEventSink,
    ExternalObservedEvent, ExternalRuntimeAdapter, ExternalRuntimeCapabilities,
    ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionInput, ExternalSessionRef,
    ExternalSessionRegistry, ExternalSessionRequest, ExternalSessionShutdown,
    ExternalSubagentRequest, ExternalToolBatchId, ExternalToolCall, RuntimeDecisionPoint,
};
use agent_lib::agent::{ExternalSessionHandler, Interaction, RequirementResult, RunContext};
use async_trait::async_trait;

use crate::assertions::ExternalInputKind;
use crate::external::ExternalAgentCallLog;
use crate::script::CallLog;

/// Summarises an [`ExternalSessionInput`] as its [`ExternalInputKind`].
///
/// Mirrors the private classifier the external-call assertions use, so a
/// scripted advance can assert the input kind it was handed inline.
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

/// The decision point (or failure) a single [`ScriptedAdvance`] resolves to.
///
/// These mirror the non-failure [`RuntimeDecisionPoint`] variants plus a
/// [`Failed`](ScriptedOutcome::Failed) arm carrying the classified error an
/// [`advance`](ExternalRuntimeSession::advance) returns as `Err`.
#[derive(Clone, Debug)]
enum ScriptedOutcome {
    /// The session step completed with terminal output.
    Completed(ExternalAgentOutput),
    /// The session paused awaiting an interaction under `action_id`.
    PausedForInteraction {
        /// Runtime handle echoed back on resume.
        action_id: String,
        /// The interaction the host must resolve.
        request: Interaction,
    },
    /// The session paused awaiting host execution of a tool-call batch.
    PausedForToolCalls {
        /// Identifier the matching results echo back.
        batch_id: ExternalToolBatchId,
        /// Tool calls the host must execute this step.
        calls: Vec<ExternalToolCall>,
    },
    /// The session paused awaiting a host-driven subagent.
    PausedForSubagent(ExternalSubagentRequest),
    /// The session failed with a classified error.
    Failed(ExternalAgentError),
}

/// One scripted advance of a [`ScriptedExternalRuntimeSession`].
///
/// A session pops one of these per [`advance`](ExternalRuntimeSession::advance).
/// It carries three optional facets:
///
/// - the [`ExternalInputKind`] the step *expects* to be advanced with, asserted
///   inline (via [`expecting`](Self::expecting)); by default no assertion is
///   made and the handler's [`ExternalAgentCallLog`] is used for sequence
///   assertions instead;
/// - the [`ExternalAgentEvent`]s the step observes (via
///   [`emitting`](Self::emitting)), which are assigned a monotonic `seq`,
///   mirrored to the live sink, and buffered into the decision point's
///   `observations`;
/// - the [`RuntimeDecisionPoint`] (or [`ExternalAgentError`]) the step resolves
///   to.
#[derive(Clone, Debug)]
pub struct ScriptedAdvance {
    expect_input: Option<ExternalInputKind>,
    events: Vec<ExternalAgentEvent>,
    outcome: ScriptedOutcome,
}

impl ScriptedAdvance {
    /// Builds a scripted advance from an outcome, with no expected input and no
    /// observed events.
    fn from_outcome(outcome: ScriptedOutcome) -> Self {
        Self {
            expect_input: None,
            events: Vec::new(),
            outcome,
        }
    }

    /// A step that completes the session step with `output`.
    #[must_use]
    pub fn completed(output: ExternalAgentOutput) -> Self {
        Self::from_outcome(ScriptedOutcome::Completed(output))
    }

    /// A step that pauses the session awaiting an interaction under `action_id`.
    #[must_use]
    pub fn paused_for_interaction(action_id: impl Into<String>, request: Interaction) -> Self {
        Self::from_outcome(ScriptedOutcome::PausedForInteraction {
            action_id: action_id.into(),
            request,
        })
    }

    /// A step that pauses the session awaiting host execution of `calls` under
    /// `batch_id`.
    #[must_use]
    pub fn paused_for_tool_calls(
        batch_id: ExternalToolBatchId,
        calls: Vec<ExternalToolCall>,
    ) -> Self {
        Self::from_outcome(ScriptedOutcome::PausedForToolCalls { batch_id, calls })
    }

    /// A step that pauses the session awaiting a host-driven subagent.
    #[must_use]
    pub fn paused_for_subagent(request: ExternalSubagentRequest) -> Self {
        Self::from_outcome(ScriptedOutcome::PausedForSubagent(request))
    }

    /// A step that fails the advance with `error`.
    ///
    /// The session returns this as `Err`; the handler folds it into an
    /// [`ExternalSessionResult::Failed`](agent_lib::agent::external::ExternalSessionResult::Failed).
    #[must_use]
    pub fn failed(error: ExternalAgentError) -> Self {
        Self::from_outcome(ScriptedOutcome::Failed(error))
    }

    /// Asserts the advance is driven with an input of `kind`.
    ///
    /// When set, [`advance`](ExternalRuntimeSession::advance) panics if the input
    /// it is handed classifies to a different [`ExternalInputKind`], so a test can
    /// pin the exact resume input the machine relays back to the runtime.
    #[must_use]
    pub fn expecting(mut self, kind: ExternalInputKind) -> Self {
        self.expect_input = Some(kind);
        self
    }

    /// Attaches the observed `events` this advance emits.
    ///
    /// Each event is assigned the session's next monotonic `seq`, mirrored to the
    /// live sink, and buffered into the resulting decision point's
    /// `observations`, so the live tail and the replay stream share one marker
    /// line (design §5.5, §10.1).
    #[must_use]
    pub fn emitting(mut self, events: impl IntoIterator<Item = ExternalAgentEvent>) -> Self {
        self.events = events.into_iter().collect();
        self
    }
}

/// A collecting [`ExternalEventSink`] recording every sequenced observation.
///
/// A [`ScriptedRuntimeExternalSessionHandler`] passes one of these to
/// [`get_or_start`](ExternalSessionRegistry::get_or_start), so a session's live
/// bypass is captured for assertions. It is a passive mirror of the buffered
/// `observations`: emitting to it never perturbs the authoritative replay stream.
#[derive(Debug, Default)]
pub struct ScriptedSinkLog {
    events: Mutex<Vec<ExternalObservedEvent>>,
}

impl ScriptedSinkLog {
    /// Returns a snapshot of every sequenced observation offered to the sink, in
    /// emission order.
    #[must_use]
    pub fn events(&self) -> Vec<ExternalObservedEvent> {
        self.events.lock().expect("sink log mutex poisoned").clone()
    }

    /// Returns the `seq` markers of every observation offered to the sink, in
    /// emission order.
    #[must_use]
    pub fn seqs(&self) -> Vec<u64> {
        self.events
            .lock()
            .expect("sink log mutex poisoned")
            .iter()
            .map(|observed| observed.seq)
            .collect()
    }

    /// Returns the number of observations offered to the sink.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.lock().expect("sink log mutex poisoned").len()
    }

    /// Returns whether the sink has seen no observation yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events
            .lock()
            .expect("sink log mutex poisoned")
            .is_empty()
    }
}

impl ExternalEventSink for ScriptedSinkLog {
    fn emit(&self, event: &ExternalObservedEvent) {
        self.events
            .lock()
            .expect("sink log mutex poisoned")
            .push(event.clone());
    }
}

/// An observable log of every request an adapter was asked to
/// [`start`](ExternalRuntimeAdapter::start).
///
/// The registry only calls the adapter's `start` on a first
/// [`Start`](ExternalSessionInput::Start); every follow-up turn reattaches to the
/// live handle, so this log records exactly the fresh-session launches, letting a
/// test assert that a reattached session was *not* restarted.
#[derive(Clone, Debug, Default)]
pub struct ScriptedRuntimeStartLog {
    requests: Arc<Mutex<Vec<ExternalSessionRequest>>>,
}

impl ScriptedRuntimeStartLog {
    fn record(&self, request: ExternalSessionRequest) {
        self.requests
            .lock()
            .expect("start log mutex poisoned")
            .push(request);
    }

    /// Returns a snapshot of every start request the adapter serviced, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<ExternalSessionRequest> {
        self.requests
            .lock()
            .expect("start log mutex poisoned")
            .clone()
    }

    /// Returns the number of fresh sessions the adapter started.
    #[must_use]
    pub fn len(&self) -> usize {
        self.requests
            .lock()
            .expect("start log mutex poisoned")
            .len()
    }

    /// Returns whether the adapter has started no session yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests
            .lock()
            .expect("start log mutex poisoned")
            .is_empty()
    }
}

/// One live scripted external-runtime session.
///
/// Holds a script of [`ScriptedAdvance`]s consumed across successive
/// [`advance`](ExternalRuntimeSession::advance) calls, a fixed runtime-assigned
/// `session_id` so the registry can key and reattach the live handle, a monotonic
/// event `seq`, and the optional live sink events are mirrored to as they are
/// buffered.
pub struct ScriptedExternalRuntimeSession {
    runtime: ExternalRuntimeKind,
    session_id: String,
    next_seq: u64,
    last_event_seq: Option<u64>,
    script: VecDeque<ScriptedAdvance>,
    sink: Option<Arc<dyn ExternalEventSink>>,
}

impl ScriptedExternalRuntimeSession {
    fn new(
        runtime: ExternalRuntimeKind,
        session_id: String,
        script: VecDeque<ScriptedAdvance>,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Self {
        Self {
            runtime,
            session_id,
            next_seq: 0,
            last_event_seq: None,
            script,
            sink,
        }
    }

    /// Assigns the next `seq` to each event, mirrors it to the live sink, and
    /// returns the buffered sequenced observations.
    fn observe(&mut self, events: Vec<ExternalAgentEvent>) -> Vec<ExternalObservedEvent> {
        let mut observations = Vec::with_capacity(events.len());
        for event in events {
            let observed = ExternalObservedEvent::new(self.next_seq, event);
            if let Some(sink) = &self.sink {
                sink.emit(&observed);
            }
            self.last_event_seq = Some(self.next_seq);
            self.next_seq += 1;
            observations.push(observed);
        }
        observations
    }
}

#[async_trait]
impl ExternalRuntimeSession for ScriptedExternalRuntimeSession {
    fn session_ref(&self) -> ExternalSessionRef {
        ExternalSessionRef {
            runtime: self.runtime.clone(),
            session_id: Some(self.session_id.clone()),
            transcript_ref: None,
            resume_token: None,
            last_event_seq: self.last_event_seq,
        }
    }

    async fn advance(
        &mut self,
        input: &ExternalSessionInput,
        _ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        let Some(step) = self.script.pop_front() else {
            return Err(ExternalAgentError::Runtime {
                code: None,
                message: "scripted external runtime session advanced past its script".to_owned(),
            });
        };

        if let Some(expected) = step.expect_input {
            let actual = input_kind(input);
            assert_eq!(
                actual, expected,
                "scripted advance expected a {expected:?} input but was driven with {actual:?}",
            );
        }

        let observations = self.observe(step.events);
        let session = self.session_ref();

        match step.outcome {
            ScriptedOutcome::Completed(output) => Ok(RuntimeDecisionPoint::Completed {
                session,
                output,
                observations,
            }),
            ScriptedOutcome::PausedForInteraction { action_id, request } => {
                Ok(RuntimeDecisionPoint::PausedForInteraction {
                    session,
                    action_id,
                    request,
                    observations,
                })
            }
            ScriptedOutcome::PausedForToolCalls { batch_id, calls } => {
                Ok(RuntimeDecisionPoint::PausedForToolCalls {
                    session,
                    batch_id,
                    calls,
                    observations,
                })
            }
            ScriptedOutcome::PausedForSubagent(request) => {
                Ok(RuntimeDecisionPoint::PausedForSubagent {
                    session,
                    request,
                    observations,
                })
            }
            ScriptedOutcome::Failed(error) => Err(error),
        }
    }

    async fn shutdown(&mut self) -> ExternalSessionShutdown {
        ExternalSessionShutdown::Graceful
    }
}

/// A per-runtime factory that hands a scripted session to each fresh
/// [`start`](ExternalRuntimeAdapter::start).
///
/// It carries one script (built by [`ScriptedRuntimeBuilder`]) handed out on the
/// first start; a second start with no remaining script fails with
/// [`ExternalAgentError::Launch`]. Every start request is recorded in a shared
/// [`ScriptedRuntimeStartLog`].
pub struct ScriptedExternalRuntimeAdapter {
    runtime: ExternalRuntimeKind,
    session_id: String,
    capabilities: ExternalRuntimeCapabilities,
    scripts: Mutex<VecDeque<VecDeque<ScriptedAdvance>>>,
    start_log: ScriptedRuntimeStartLog,
}

impl ScriptedExternalRuntimeAdapter {
    /// Returns the shared log of every start request this adapter serviced.
    #[must_use]
    pub fn start_log(&self) -> &ScriptedRuntimeStartLog {
        &self.start_log
    }
}

#[async_trait]
impl ExternalRuntimeAdapter for ScriptedExternalRuntimeAdapter {
    fn kind(&self) -> ExternalRuntimeKind {
        self.runtime.clone()
    }

    fn capabilities(&self) -> ExternalRuntimeCapabilities {
        self.capabilities.clone()
    }

    async fn start(
        &self,
        request: &ExternalSessionRequest,
        _ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
        self.start_log.record(request.clone());
        let script = self
            .scripts
            .lock()
            .expect("adapter scripts mutex poisoned")
            .pop_front()
            .ok_or_else(|| ExternalAgentError::Launch {
                runtime: self.runtime.clone(),
                detail: "scripted external runtime adapter has no script left to start".to_owned(),
            })?;
        Ok(Box::new(ScriptedExternalRuntimeSession::new(
            self.runtime.clone(),
            self.session_id.clone(),
            script,
            sink,
        )))
    }
}

/// An [`ExternalSessionHandler`] composing a scripted adapter with a real
/// [`ExternalSessionRegistry`].
///
/// This is the production-shaped handler the milestone-5 design calls for: it
/// holds *no* machine state. Every [`fulfill`](ExternalSessionHandler::fulfill)
/// resolves the live handle through the registry —
/// [`get_or_start`](ExternalSessionRegistry::get_or_start) starts the session on
/// the first [`Start`](ExternalSessionInput::Start) and reattaches to the same
/// live handle on every follow-up turn — advances it one
/// [`RuntimeDecisionPoint`], and folds the outcome into a family-aligned
/// [`RequirementResult::ExternalSession`] via the milestone-5 `From` conversion.
/// Every call is recorded in an [`ExternalAgentCallLog`] so tests can assert the
/// input/result sequence with
/// [`assert_external_calls`](crate::assertions::assert_external_calls).
pub struct ScriptedRuntimeExternalSessionHandler {
    registry: Arc<ExternalSessionRegistry>,
    sink: Arc<ScriptedSinkLog>,
    log: Arc<ExternalAgentCallLog>,
    start_log: ScriptedRuntimeStartLog,
}

impl ScriptedRuntimeExternalSessionHandler {
    /// Returns the registry that owns the handler's live sessions.
    #[must_use]
    pub fn registry(&self) -> &Arc<ExternalSessionRegistry> {
        &self.registry
    }

    /// Returns the collecting sink recording every live observation.
    #[must_use]
    pub fn sink(&self) -> &Arc<ScriptedSinkLog> {
        &self.sink
    }

    /// Returns the call log recording every fulfilled `NeedExternalSession`.
    #[must_use]
    pub fn log(&self) -> &Arc<ExternalAgentCallLog> {
        &self.log
    }

    /// Returns the log of every fresh session the adapter started.
    #[must_use]
    pub fn start_log(&self) -> &ScriptedRuntimeStartLog {
        &self.start_log
    }

    /// Resolves the live handle and advances it one decision point, folding both
    /// a `get_or_start` failure and an `advance` failure into a family-aligned
    /// [`ExternalSessionResult`].
    async fn advance(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> agent_lib::agent::external::ExternalSessionResult {
        let sink: Arc<dyn ExternalEventSink> = Arc::clone(&self.sink) as Arc<dyn ExternalEventSink>;
        let handle = match self.registry.get_or_start(request, ctx, Some(sink)).await {
            Ok(handle) => handle,
            Err(error) => return Err::<RuntimeDecisionPoint, _>(error).into(),
        };
        let mut session = handle.lock().await;
        let point = session.advance(&request.input, ctx).await;
        point.into()
    }
}

#[async_trait]
impl ExternalSessionHandler for ScriptedRuntimeExternalSessionHandler {
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> RequirementResult {
        let ticket = self.log.begin(request.clone());
        let result = RequirementResult::ExternalSession(Box::new(self.advance(request, ctx).await));
        self.log.complete(ticket, result.clone());
        result
    }
}

/// Builds a [`ScriptedRuntimeExternalSessionHandler`] over a scripted adapter and
/// a real [`ExternalSessionRegistry`].
///
/// Push one [`ScriptedAdvance`] per decision point the live session should reach,
/// in order, then [`build`](Self::build). The default runtime is
/// [`ExternalRuntimeKind::ClaudeCode`] with a permissive capability set (all
/// managed features on except `resume`, so the registry reattaches through the
/// live handle rather than the adapter's resume path), and a fixed
/// `"scripted-sess-1"` session id.
pub struct ScriptedRuntimeBuilder {
    runtime: ExternalRuntimeKind,
    session_id: String,
    capabilities: Option<ExternalRuntimeCapabilities>,
    script: VecDeque<ScriptedAdvance>,
}

impl ScriptedRuntimeBuilder {
    /// Starts a builder with the default runtime, capabilities, and session id.
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: "scripted-sess-1".to_owned(),
            capabilities: None,
            script: VecDeque::new(),
        }
    }

    /// Sets the runtime kind the adapter reports and the sessions run under.
    #[must_use]
    pub fn runtime(mut self, runtime: ExternalRuntimeKind) -> Self {
        self.runtime = runtime;
        self
    }

    /// Sets the runtime-assigned session id the started session reports.
    ///
    /// The registry keys the live handle by this id, and the machine echoes it in
    /// every follow-up request, so it must stay stable for the whole session.
    #[must_use]
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        self
    }

    /// Overrides the capability set the adapter reports.
    #[must_use]
    pub fn capabilities(mut self, capabilities: ExternalRuntimeCapabilities) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    /// Appends one scripted decision point to the session's script.
    #[must_use]
    pub fn advance(mut self, step: ScriptedAdvance) -> Self {
        self.script.push_back(step);
        self
    }

    /// Appends every scripted decision point in `steps` to the session's script.
    #[must_use]
    pub fn advances(mut self, steps: impl IntoIterator<Item = ScriptedAdvance>) -> Self {
        self.script.extend(steps);
        self
    }

    /// Builds the scripted adapter, registry, and registry-backed handler.
    #[must_use]
    pub fn build(self) -> ScriptedRuntimeExternalSessionHandler {
        let capabilities = self
            .capabilities
            .unwrap_or_else(|| permissive_capabilities(self.runtime.clone()));
        let start_log = ScriptedRuntimeStartLog::default();
        let mut scripts = VecDeque::new();
        scripts.push_back(self.script);
        let adapter = ScriptedExternalRuntimeAdapter {
            runtime: self.runtime,
            session_id: self.session_id,
            capabilities,
            scripts: Mutex::new(scripts),
            start_log: start_log.clone(),
        };
        let registry = Arc::new(ExternalSessionRegistry::new(
            Arc::new(adapter) as Arc<dyn ExternalRuntimeAdapter>
        ));
        ScriptedRuntimeExternalSessionHandler {
            registry,
            sink: Arc::new(ScriptedSinkLog::default()),
            log: Arc::new(CallLog::new()),
            start_log,
        }
    }
}

impl Default for ScriptedRuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// A permissive capability set: every managed feature on except `resume`.
///
/// `resume` stays off so a follow-up turn reattaches through the live handle the
/// registry holds rather than the adapter's (unimplemented) resume path — the
/// intended in-process managed-loop path for these offline tests.
fn permissive_capabilities(runtime: ExternalRuntimeKind) -> ExternalRuntimeCapabilities {
    ExternalRuntimeCapabilities {
        runtime,
        streaming: true,
        resume: false,
        permission_bridge: true,
        host_tools: true,
        host_subagents: true,
        artifacts: true,
        usage: true,
        graceful_shutdown: true,
    }
}

#[cfg(test)]
mod tests {
    use super::{ScriptedAdvance, ScriptedRuntimeBuilder};
    use crate::assertions::ExternalInputKind;
    use crate::external::ExternalAgentFixture;
    use crate::fixtures::root_context;
    use crate::ids::SeqIds;
    use agent_lib::agent::ExternalSessionHandler;
    use agent_lib::agent::RequirementResult;
    use agent_lib::agent::external::{
        ExternalAgentError, ExternalSessionInput, ExternalSessionResult,
    };

    /// Unwraps the boxed external result from a family-aligned requirement result.
    fn external_result(result: &RequirementResult) -> &ExternalSessionResult {
        match result {
            RequirementResult::ExternalSession(boxed) => boxed.as_ref(),
            other => panic!("expected an ExternalSession result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn scripted_runtime_start_completes_and_mirrors_events_to_sink() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let fixture = ExternalAgentFixture::new(&ids);

        let handler = ScriptedRuntimeBuilder::new()
            .advance(
                ScriptedAdvance::completed(fixture.output("done"))
                    .expecting(ExternalInputKind::Start)
                    .emitting([fixture.command_finished_event(), fixture.file_patch_event()]),
            )
            .build();

        let result = handler
            .fulfill(&fixture.start_request("refactor the parser"), &ctx)
            .await;

        assert!(matches!(
            external_result(&result),
            ExternalSessionResult::Completed { .. }
        ));
        // The live sink saw both observations on one monotonic seq line.
        assert_eq!(handler.sink().seqs(), vec![0, 1]);
        // The adapter started exactly one fresh session.
        assert_eq!(handler.start_log().len(), 1);
    }

    #[tokio::test]
    async fn scripted_runtime_reattaches_live_handle_without_restart() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let fixture = ExternalAgentFixture::new(&ids);

        let handler = ScriptedRuntimeBuilder::new()
            .session_id("sess-1")
            .advance(ScriptedAdvance::completed(fixture.output("first")))
            .advance(
                ScriptedAdvance::completed(fixture.output("second"))
                    .expecting(ExternalInputKind::Continue),
            )
            .build();

        let start = fixture.start_request("start");
        let first = handler.fulfill(&start, &ctx).await;
        assert!(matches!(
            external_result(&first),
            ExternalSessionResult::Completed { .. }
        ));

        // A follow-up Continue carrying the reported session (same agent id)
        // reattaches the live handle rather than starting a second session.
        let mut cont = start.clone();
        cont.session = Some(fixture.session_ref());
        cont.input = ExternalSessionInput::Continue {
            message: "keep going".to_owned(),
        };
        let second = handler.fulfill(&cont, &ctx).await;
        assert!(matches!(
            external_result(&second),
            ExternalSessionResult::Completed { .. }
        ));
        assert_eq!(handler.start_log().len(), 1);
        assert_eq!(handler.registry().live_len(), 1);
    }

    #[tokio::test]
    async fn scripted_runtime_failure_folds_into_family_aligned_failure() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let fixture = ExternalAgentFixture::new(&ids);

        let handler = ScriptedRuntimeBuilder::new()
            .advance(ScriptedAdvance::failed(ExternalAgentError::LimitExceeded {
                limit: "max_turns=8".to_owned(),
            }))
            .build();

        let result = handler
            .fulfill(&fixture.start_request("refactor the parser"), &ctx)
            .await;

        assert!(matches!(
            external_result(&result),
            ExternalSessionResult::Failed {
                error: ExternalAgentError::LimitExceeded { .. },
                ..
            }
        ));
    }

    #[tokio::test]
    #[should_panic(expected = "scripted advance expected")]
    async fn scripted_runtime_unexpected_input_panics() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let fixture = ExternalAgentFixture::new(&ids);

        let handler = ScriptedRuntimeBuilder::new()
            .advance(
                ScriptedAdvance::completed(fixture.output("done"))
                    .expecting(ExternalInputKind::Continue),
            )
            .build();

        // Driven with a Start, though the step demanded a Continue.
        let _ = handler
            .fulfill(&fixture.start_request("refactor the parser"), &ctx)
            .await;
    }

    #[tokio::test]
    async fn scripted_runtime_exhausted_script_reports_runtime_error() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let fixture = ExternalAgentFixture::new(&ids);

        // One start step, but two fulfil calls on the same live session.
        let handler = ScriptedRuntimeBuilder::new()
            .session_id("sess-1")
            .advance(ScriptedAdvance::completed(fixture.output("first")))
            .build();

        let start = fixture.start_request("start");
        let _ = handler.fulfill(&start, &ctx).await;
        let mut cont = start.clone();
        cont.session = Some(fixture.session_ref());
        cont.input = ExternalSessionInput::Continue {
            message: "again".to_owned(),
        };
        let second = handler.fulfill(&cont, &ctx).await;

        match external_result(&second) {
            ExternalSessionResult::Failed {
                error: ExternalAgentError::Runtime { message, .. },
                ..
            } => assert!(message.contains("advanced past its script")),
            other => panic!("expected a Runtime failure past the script, got {other:?}"),
        }
    }

    #[test]
    fn scripted_advance_input_kind_classifies_shutdown() {
        // Guards the local input classifier against the assertions' private copy
        // drifting.
        assert_eq!(
            super::input_kind(&ExternalSessionInput::Shutdown),
            ExternalInputKind::Shutdown
        );
    }
}
