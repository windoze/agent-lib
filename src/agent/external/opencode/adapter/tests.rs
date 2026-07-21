use super::{
    FirstLaunch, OpenCodeAdapter, OpenCodeLauncher, OpenCodeSession, OpenCodeTurnSpec,
    OpenCodeTurnStream, implemented_capabilities, turn_message,
};
use crate::agent::external::OpenCodeConfig;
use crate::agent::external::process;
use crate::agent::external::{
    ExternalAgentError, ExternalCapability, ExternalEventSink, ExternalObservedEvent,
    ExternalPermissionMode, ExternalRuntimeAdapter, ExternalRuntimeCapabilities,
    ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionInput, ExternalSessionPolicy,
    ExternalSessionRef, ExternalSessionRequest, ExternalSessionShutdown, ExternalStreamPolicy,
    ExternalToolBatchId, RuntimeDecisionPoint, WorktreeIsolation,
};
use crate::agent::spec::WorktreeRef;
use crate::agent::{AgentId, BudgetLimits, RunContext, RunId, TraceNodeId, TraceNodeKind};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const SESSION_ID: &str = "ses_8b1f7a2c9d3e4f50";
const RUN_UUID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890e0";
const AGENT_UUID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890f0";
/// Generous prelude bound for tests that never exercise the deadline.
const PRELUDE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn agent_id() -> AgentId {
    AGENT_UUID.parse().expect("agent id parses")
}

fn run_context() -> RunContext {
    let run_id: RunId = RUN_UUID.parse().expect("run id parses");
    RunContext::new_root(
        run_id,
        BudgetLimits::unbounded(),
        TraceNodeId::new("opencode-adapter-test"),
    )
}

fn policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        permission_mode: ExternalPermissionMode::AcceptEdits,
        isolation: WorktreeIsolation::EphemeralGitWorktree,
        max_turns: Some(8),
        stream_events: ExternalStreamPolicy::Streaming,
    }
}

fn start_request(tools: Vec<crate::model::tool::Tool>) -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent_id(),
        runtime: ExternalRuntimeKind::OpenCode,
        worktree: WorktreeRef::new("/repo/agent-lib"),
        session_dir: None,
        session: None,
        input: ExternalSessionInput::Start {
            prompt: "investigate the failing test".to_owned(),
        },
        tools,
        policy: policy(),
    }
}

fn resume_ref() -> ExternalSessionRef {
    ExternalSessionRef {
        runtime: ExternalRuntimeKind::OpenCode,
        session_id: Some(SESSION_ID.to_owned()),
        transcript_ref: None,
        resume_token: Some(SESSION_ID.to_owned()),
        last_event_seq: Some(3),
    }
}

/// A `step_start` boundary frame carrying the runtime session id — the first
/// frame a real `opencode run` process emits, from which the decoder captures
/// the id and announces the session.
fn step_start(session_id: &str) -> String {
    format!(
        r#"{{"type":"step_start","sessionID":"{session_id}","part":{{"type":"step-start","sessionID":"{session_id}"}}}}"#
    )
}

fn text(session_id: &str, body: &str) -> String {
    format!(
        r#"{{"type":"text","sessionID":"{session_id}","part":{{"type":"text","text":"{body}","time":{{"end":1}}}}}}"#
    )
}

fn step_finish_stop(session_id: &str) -> String {
    format!(
        r#"{{"type":"step_finish","sessionID":"{session_id}","part":{{"type":"step-finish","reason":"stop","cost":0.001,"tokens":{{"input":10,"output":5,"reasoning":0,"cache":{{"read":0,"write":0}}}}}}}}"#
    )
}

fn error_frame(session_id: &str) -> String {
    format!(
        r#"{{"type":"error","sessionID":"{session_id}","error":{{"name":"ProviderError","data":{{"message":"boom"}}}}}}"#
    )
}

/// A fake turn stream replaying canned stdout lines.
struct FakeTurn {
    lines: VecDeque<String>,
    close_disposition: ExternalSessionShutdown,
    /// Line replayed forever once `lines` drains (prelude-deadline tests).
    repeat: Option<String>,
    /// When set, reads pend forever once `lines` drains (silent-peer
    /// cancellation tests).
    pending: bool,
}

#[async_trait]
impl OpenCodeTurnStream for FakeTurn {
    async fn read_frame(&mut self) -> std::io::Result<Option<String>> {
        match self.lines.pop_front() {
            Some(line) => Ok(Some(line)),
            None if self.pending => std::future::pending().await,
            None => Ok(self.repeat.clone()),
        }
    }

    async fn close(&mut self) -> ExternalSessionShutdown {
        self.close_disposition
    }
}

/// Shared recorder of the specs a [`FakeLauncher`] was asked to launch.
type RecordedSpecs = Arc<Mutex<Vec<OpenCodeTurnSpec>>>;

/// A fake launcher popping one canned turn per launch and recording specs.
struct FakeLauncher {
    turns: Mutex<VecDeque<Vec<String>>>,
    specs: RecordedSpecs,
    /// Close disposition for turns without a queued per-turn entry.
    default_close: ExternalSessionShutdown,
    /// Per-turn close dispositions, popped one per launch.
    close_sequence: Mutex<VecDeque<ExternalSessionShutdown>>,
    /// Line replayed forever by every spawned turn (prelude-deadline tests).
    repeat: Option<String>,
    /// When set, every spawned turn's reads pend forever once its canned
    /// lines drain (silent-peer cancellation tests).
    pending: bool,
    fail_kind: Option<std::io::ErrorKind>,
}

impl FakeLauncher {
    fn new(turns: Vec<Vec<String>>) -> Self {
        Self {
            turns: Mutex::new(turns.into_iter().collect()),
            specs: Arc::new(Mutex::new(Vec::new())),
            default_close: ExternalSessionShutdown::Graceful,
            close_sequence: Mutex::new(VecDeque::new()),
            repeat: None,
            pending: false,
            fail_kind: None,
        }
    }

    fn recorded_specs(&self) -> RecordedSpecs {
        Arc::clone(&self.specs)
    }

    /// Closes every spawned turn with `disposition`.
    fn with_close(mut self, disposition: ExternalSessionShutdown) -> Self {
        self.default_close = disposition;
        self
    }

    /// Closes the Nth spawned turn with the Nth entry (later turns fall back
    /// to the default).
    fn with_close_sequence(self, dispositions: &[ExternalSessionShutdown]) -> Self {
        *self.close_sequence.lock().unwrap() = dispositions.iter().copied().collect();
        self
    }

    /// Every spawned turn replays `line` forever once its canned lines drain.
    fn repeating(mut self, line: String) -> Self {
        self.repeat = Some(line);
        self
    }

    /// Every spawned turn's reads pend forever once its canned lines drain,
    /// modelling a live but silent CLI.
    fn silent_after_script(mut self) -> Self {
        self.pending = true;
        self
    }

    fn failing(kind: std::io::ErrorKind) -> Self {
        let mut launcher = Self::new(Vec::new());
        launcher.fail_kind = Some(kind);
        launcher
    }
}

#[async_trait]
impl OpenCodeLauncher for FakeLauncher {
    async fn launch(
        &self,
        spec: &OpenCodeTurnSpec,
    ) -> std::io::Result<Box<dyn OpenCodeTurnStream>> {
        self.specs.lock().unwrap().push(spec.clone());
        if let Some(kind) = self.fail_kind {
            return Err(std::io::Error::new(kind, "fake launch failure"));
        }
        let lines = self.turns.lock().unwrap().pop_front().unwrap_or_default();
        let close_disposition = self
            .close_sequence
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(self.default_close);
        Ok(Box::new(FakeTurn {
            lines: lines.into_iter().collect(),
            close_disposition,
            repeat: self.repeat.clone(),
            pending: self.pending,
        }))
    }
}

/// Collecting sink recording every live observation.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<ExternalObservedEvent>>,
}

impl ExternalEventSink for RecordingSink {
    fn emit(&self, event: &ExternalObservedEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn session_over(
    launcher: FakeLauncher,
    sink: Option<Arc<dyn ExternalEventSink>>,
) -> OpenCodeSession<FakeLauncher> {
    let context =
        OpenCodeAdapter::decode_context(&OpenCodeConfig::new(), &start_request(Vec::new()));
    OpenCodeSession::new(launcher, context, sink, implemented_capabilities())
}

fn fresh_spec(prompt: &str) -> OpenCodeTurnSpec {
    OpenCodeTurnSpec::Fresh {
        prompt: prompt.to_owned(),
    }
}

#[tokio::test]
async fn opencode_adapter_advance_drives_text_and_completion() {
    let sink = Arc::new(RecordingSink::default());
    let launcher = FakeLauncher::new(vec![vec![
        step_start(SESSION_ID),
        text(SESSION_ID, "looking into it"),
        step_finish_stop(SESSION_ID),
    ]]);
    let specs = launcher.recorded_specs();
    let mut session = session_over(
        launcher,
        Some(Arc::clone(&sink) as Arc<dyn ExternalEventSink>),
    );

    session
        .begin(
            &fresh_spec("investigate the failing test"),
            FirstLaunch::Fresh,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("begin launches the first turn and captures the session id");
    assert_eq!(
        session.session_ref().session_id.as_deref(),
        Some(SESSION_ID)
    );

    let ctx = run_context();
    let decision = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect("first advance settles the turn");
    match decision {
        RuntimeDecisionPoint::Completed {
            output,
            observations,
            session,
        } => {
            // The carried SessionStarted plus the TextDelta plus the
            // SessionCompleted all ride the first decision point.
            assert!(observations.len() >= 3, "prelude + turn observations");
            assert_eq!(output.summary, "looking into it");
            assert!(output.usage.is_some());
            assert_eq!(session.session_id.as_deref(), Some(SESSION_ID));
        }
        other => panic!("expected completion, got {other:?}"),
    }

    // Exactly one process was launched, a Fresh turn carrying the prompt.
    let recorded = specs.lock().unwrap().clone();
    assert_eq!(recorded, vec![fresh_spec("investigate the failing test")]);

    // The sink saw the same sequenced observations, monotonically.
    let seqs: Vec<u64> = sink.events.lock().unwrap().iter().map(|e| e.seq).collect();
    assert!(seqs.len() >= 3, "streamed at least three observations");
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq is monotonic: {seqs:?}"
    );
}

/// M3-1: a cancel landing while the CLI is silent settles the advance in
/// seconds through the cancellation path, not after the read idle timeout.
#[tokio::test]
async fn opencode_adapter_cancel_settles_a_silent_advance_promptly() {
    let launcher = FakeLauncher::new(vec![vec![step_start(SESSION_ID)]]).silent_after_script();
    let mut session = session_over(launcher, None);
    let ctx = run_context();
    session
        .begin(
            &fresh_spec("investigate the failing test"),
            FirstLaunch::Fresh,
            &ctx,
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("begin launches the first turn and captures the session id");

    let token = ctx.cancellation().clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        token.cancel();
    });
    let settled = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.advance(&start_request(Vec::new()).input, &ctx),
    )
    .await
    .expect("a cancelled advance settles well before the read idle timeout");
    canceller.await.expect("canceller task");

    match settled {
        Err(ExternalAgentError::SessionLost { detail, .. }) => {
            assert!(detail.contains("cancelled"), "detail: {detail}");
        }
        other => panic!("expected a cancellation SessionLost, got {other:?}"),
    }
}

#[tokio::test]
async fn opencode_adapter_follow_up_turn_resumes_with_session_id() {
    let launcher = FakeLauncher::new(vec![
        vec![
            step_start(SESSION_ID),
            text(SESSION_ID, "first"),
            step_finish_stop(SESSION_ID),
        ],
        vec![
            step_start(SESSION_ID),
            text(SESSION_ID, "second"),
            step_finish_stop(SESSION_ID),
        ],
    ]);
    let specs = launcher.recorded_specs();
    let mut session = session_over(launcher, None);
    session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("begin");

    let ctx = run_context();
    let first = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect("first completion");
    assert!(matches!(first, RuntimeDecisionPoint::Completed { .. }));

    // A follow-up turn spawns a fresh `run --session` process for the session.
    let follow_up = ExternalSessionInput::Continue {
        message: "keep going".to_owned(),
    };
    let second = session
        .advance(&follow_up, &ctx)
        .await
        .expect("second completion");
    match second {
        RuntimeDecisionPoint::Completed { output, .. } => {
            assert_eq!(output.summary, "second");
        }
        other => panic!("expected completion, got {other:?}"),
    }

    let recorded = specs.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            fresh_spec("start"),
            OpenCodeTurnSpec::Resume {
                session_id: SESSION_ID.to_owned(),
                message: "keep going".to_owned(),
            },
        ]
    );
}

#[tokio::test]
async fn opencode_adapter_advance_reports_session_lost_on_early_eof() {
    let launcher = FakeLauncher::new(vec![vec![step_start(SESSION_ID)]]);
    let mut session = session_over(launcher, None);
    session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("begin");

    let ctx = run_context();
    let error = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect_err("eof before a decision is a lost session");
    match error {
        ExternalAgentError::SessionLost { session, detail } => {
            assert_eq!(
                session.and_then(|s| s.session_id).as_deref(),
                Some(SESSION_ID)
            );
            assert!(detail.contains("decision point"));
        }
        other => panic!("expected SessionLost, got {other:?}"),
    }
}

#[tokio::test]
async fn opencode_adapter_advance_propagates_protocol_error_on_malformed_frame() {
    let mut frames = vec![step_start(SESSION_ID)];
    frames.extend((0..=8).map(|_| "{ not json".to_owned()));
    let launcher = FakeLauncher::new(vec![frames]);
    let mut session = session_over(launcher, None);
    session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("begin");

    let ctx = run_context();
    let error = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect_err("too much non-json noise is a protocol error");
    assert!(matches!(error, ExternalAgentError::Protocol { .. }));
}

#[tokio::test]
async fn opencode_adapter_advance_propagates_turn_failed() {
    let launcher = FakeLauncher::new(vec![vec![step_start(SESSION_ID), error_frame(SESSION_ID)]]);
    let mut session = session_over(launcher, None);
    session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("begin");

    let ctx = run_context();
    let error = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect_err("an error frame fails the turn");
    assert!(matches!(error, ExternalAgentError::Runtime { .. }));
}

#[tokio::test]
async fn opencode_adapter_shutdown_classifies_the_close() {
    let launcher = FakeLauncher::new(vec![vec![step_start(SESSION_ID)]])
        .with_close(ExternalSessionShutdown::ForcedKill);
    let mut session = session_over(launcher, None);
    session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("begin");
    assert_eq!(
        session.shutdown().await,
        ExternalSessionShutdown::ForcedKill
    );
}

#[tokio::test]
async fn opencode_adapter_begin_times_out_when_session_id_never_arrives() {
    // A CLI babbling tolerated frames that carry no `sessionID` would loop
    // the prelude forever on the per-line read timeout alone (each line
    // resets it); the launch deadline caps the whole prelude (review
    // M-EXT-6).
    let launcher = FakeLauncher::new(Vec::new()).repeating(r#"{"type":"ping"}"#.to_owned());
    let mut session = session_over(launcher, None);
    let started = std::time::Instant::now();
    let error = session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &run_context(),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect_err("a prelude that never reports a session id hits the launch deadline");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the prelude deadline fires promptly"
    );
    match error {
        ExternalAgentError::Launch { runtime, detail } => {
            assert_eq!(runtime, ExternalRuntimeKind::OpenCode);
            assert!(detail.contains("launch timeout"), "detail: {detail}");
        }
        other => panic!("expected Launch, got {other:?}"),
    }
}

#[tokio::test]
async fn opencode_adapter_begin_resume_times_out_when_session_id_never_arrives() {
    // The same prelude deadline on the resume path is classified as
    // `ResumeUnavailable`, matching the spawn-failure classification axis.
    let launcher = FakeLauncher::new(Vec::new()).repeating(r#"{"type":"ping"}"#.to_owned());
    let mut session = session_over(launcher, None);
    let spec = OpenCodeTurnSpec::Resume {
        session_id: SESSION_ID.to_owned(),
        message: "continue".to_owned(),
    };
    let error = session
        .begin(
            &spec,
            FirstLaunch::Resume(resume_ref()),
            &run_context(),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect_err("a resumed prelude that never re-reports its id hits the deadline");
    match error {
        ExternalAgentError::ResumeUnavailable { session, detail } => {
            assert_eq!(session.session_id.as_deref(), Some(SESSION_ID));
            assert!(detail.contains("launch timeout"), "detail: {detail}");
        }
        other => panic!("expected ResumeUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn opencode_adapter_begin_honours_cancellation() {
    // The prelude checks `ctx.is_cancelled()` per iteration, just like the
    // advance loop (review M-EXT-6).
    let launcher = FakeLauncher::new(vec![vec![step_start(SESSION_ID)]]);
    let mut session = session_over(launcher, None);
    let ctx = run_context();
    ctx.cancellation().cancel();
    let error = session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &ctx,
            PRELUDE_TIMEOUT,
        )
        .await
        .expect_err("a cancelled run aborts the prelude");
    match error {
        ExternalAgentError::SessionLost { detail, .. } => {
            assert!(detail.contains("cancelled"), "detail: {detail}");
        }
        other => panic!("expected SessionLost, got {other:?}"),
    }
}

#[tokio::test]
async fn opencode_adapter_mid_turn_close_is_traced_and_marks_the_session_dirty() {
    // Turn 1's process has to be force-killed when turn 2 spawns; that
    // disposition must reach the trace and the session's final shutdown
    // report instead of being dropped (review M-EXT-5).
    let launcher = FakeLauncher::new(vec![
        vec![
            step_start(SESSION_ID),
            text(SESSION_ID, "first"),
            step_finish_stop(SESSION_ID),
        ],
        vec![
            step_start(SESSION_ID),
            text(SESSION_ID, "second"),
            step_finish_stop(SESSION_ID),
        ],
    ])
    .with_close_sequence(&[
        ExternalSessionShutdown::ForcedKill,
        ExternalSessionShutdown::Graceful,
    ]);
    let mut session = session_over(launcher, None);
    session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("begin");

    let ctx = run_context();
    session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect("first completion");
    let follow_up = ExternalSessionInput::Continue {
        message: "keep going".to_owned(),
    };
    session
        .advance(&follow_up, &ctx)
        .await
        .expect("second completion");

    // The mid-turn close disposition was recorded to the trace.
    let shutdowns: Vec<TraceNodeKind> = ctx
        .trace()
        .records()
        .into_iter()
        .map(|record| record.kind())
        .filter(|kind| matches!(kind, TraceNodeKind::ExternalShutdown { .. }))
        .collect();
    assert_eq!(
        shutdowns,
        vec![TraceNodeKind::ExternalShutdown {
            disposition: ExternalSessionShutdown::ForcedKill,
        }],
        "the force-killed turn process is traced"
    );

    // ...and folded into the final shutdown even though turn 2's own close
    // was graceful, so the worktree is judged as potentially dirty.
    assert_eq!(
        session.shutdown().await,
        ExternalSessionShutdown::ForcedKill
    );
}

#[tokio::test]
async fn opencode_adapter_resume_defers_and_records_session_id() {
    let launcher = FakeLauncher::new(vec![vec![
        step_start(SESSION_ID),
        text(SESSION_ID, "resumed"),
        step_finish_stop(SESSION_ID),
    ]]);
    let specs = launcher.recorded_specs();
    let mut session = session_over(launcher, None);

    let spec = OpenCodeTurnSpec::Resume {
        session_id: SESSION_ID.to_owned(),
        message: "continue".to_owned(),
    };
    session
        .begin(
            &spec,
            FirstLaunch::Resume(resume_ref()),
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("resume begin");
    assert_eq!(
        session.session_ref().session_id.as_deref(),
        Some(SESSION_ID)
    );

    let ctx = run_context();
    let follow_up = ExternalSessionInput::Continue {
        message: "continue".to_owned(),
    };
    let decision = session.advance(&follow_up, &ctx).await.expect("completion");
    assert!(matches!(decision, RuntimeDecisionPoint::Completed { .. }));

    // The one recorded spec is the resume turn carrying the session id.
    let recorded = specs.lock().unwrap().clone();
    assert_eq!(recorded, vec![spec]);
}

#[tokio::test]
async fn opencode_adapter_resume_continues_the_seq_line_past_the_high_water() {
    // A resume must continue the decoder's seq line past the persisted
    // `last_event_seq`: restarting at 0 would let the machine's replay dedup
    // silently drop every post-resume observation (design §5.5, review
    // M-EXT-1).
    let launcher = FakeLauncher::new(vec![vec![
        step_start(SESSION_ID),
        text(SESSION_ID, "resumed"),
        step_finish_stop(SESSION_ID),
    ]]);
    let mut session = session_over(launcher, None).with_resume_high_water(Some(50));

    let spec = OpenCodeTurnSpec::Resume {
        session_id: SESSION_ID.to_owned(),
        message: "continue".to_owned(),
    };
    session
        .begin(
            &spec,
            FirstLaunch::Resume(resume_ref()),
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("resume begin");
    // The prelude already emitted its first observations past the mark.
    assert!(
        session.session_ref().last_event_seq >= Some(50),
        "the reported water mark never regresses below the persisted one"
    );

    let ctx = run_context();
    let follow_up = ExternalSessionInput::Continue {
        message: "continue".to_owned(),
    };
    let decision = session.advance(&follow_up, &ctx).await.expect("completion");
    let RuntimeDecisionPoint::Completed { observations, .. } = decision else {
        panic!("expected completion");
    };
    assert!(!observations.is_empty());
    assert_eq!(
        observations[0].seq, 51,
        "the first post-resume observation continues past the high water"
    );
    assert!(
        observations
            .windows(2)
            .all(|pair| pair[1].seq == pair[0].seq + 1),
        "the seq line stays contiguous"
    );
    assert_eq!(
        session.session_ref().last_event_seq,
        Some(observations.last().expect("non-empty").seq),
        "the reported water mark never regresses below the persisted one"
    );
}

#[tokio::test]
async fn opencode_adapter_resume_survives_a_session_that_never_re_reports_its_id() {
    // A resumed process whose only frame settles the turn without re-reporting
    // a `sessionID` still exposes the pre-seeded id and completes.
    let launcher = FakeLauncher::new(vec![vec![
        r#"{"type":"step_finish","part":{"type":"step-finish","reason":"stop"}}"#.to_owned(),
    ]]);
    let mut session = session_over(launcher, None);

    let spec = OpenCodeTurnSpec::Resume {
        session_id: SESSION_ID.to_owned(),
        message: "continue".to_owned(),
    };
    session
        .begin(
            &spec,
            FirstLaunch::Resume(resume_ref()),
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("resume begin pre-seeds the id");
    assert_eq!(
        session.session_ref().session_id.as_deref(),
        Some(SESSION_ID)
    );

    let ctx = run_context();
    let decision = session
        .advance(
            &ExternalSessionInput::Continue {
                message: "continue".to_owned(),
            },
            &ctx,
        )
        .await
        .expect("completion");
    assert!(matches!(decision, RuntimeDecisionPoint::Completed { .. }));
}

#[tokio::test]
async fn opencode_adapter_follow_up_respond_tool_results_is_unsupported() {
    let launcher = FakeLauncher::new(vec![vec![
        step_start(SESSION_ID),
        text(SESSION_ID, "done"),
        step_finish_stop(SESSION_ID),
    ]]);
    let mut session = session_over(launcher, None);
    session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("begin");

    let ctx = run_context();
    let first = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect("first completion");
    assert!(matches!(first, RuntimeDecisionPoint::Completed { .. }));

    let input = ExternalSessionInput::RespondToolResults {
        batch_id: ExternalToolBatchId::new("batch-1"),
        results: Vec::new(),
    };
    let error = session
        .advance(&input, &ctx)
        .await
        .expect_err("host tool results are unsupported");
    match error {
        ExternalAgentError::UnsupportedCapability { capability, .. } => {
            assert_eq!(capability, ExternalCapability::HostTools);
        }
        other => panic!("expected UnsupportedCapability, got {other:?}"),
    }
}

#[tokio::test]
async fn opencode_adapter_begin_reports_launch_failure() {
    let launcher = FakeLauncher::failing(std::io::ErrorKind::NotFound);
    let mut session = session_over(launcher, None);
    let error = session
        .begin(
            &fresh_spec("start"),
            FirstLaunch::Fresh,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect_err("a spawn failure is a launch error");
    assert!(matches!(
        error,
        ExternalAgentError::Launch {
            runtime: ExternalRuntimeKind::OpenCode,
            ..
        }
    ));
}

#[tokio::test]
async fn opencode_adapter_begin_reports_resume_failure() {
    let launcher = FakeLauncher::failing(std::io::ErrorKind::NotFound);
    let mut session = session_over(launcher, None);
    let spec = OpenCodeTurnSpec::Resume {
        session_id: SESSION_ID.to_owned(),
        message: "continue".to_owned(),
    };
    let error = session
        .begin(
            &spec,
            FirstLaunch::Resume(resume_ref()),
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect_err("a resume spawn failure is unavailable");
    assert!(matches!(
        error,
        ExternalAgentError::ResumeUnavailable { .. }
    ));
}

#[test]
fn opencode_adapter_turn_message_maps_inputs_and_refusals() {
    let caps = implemented_capabilities();
    assert_eq!(
        turn_message(
            &caps,
            &ExternalSessionInput::Start {
                prompt: "p".to_owned()
            }
        )
        .expect("start text"),
        "p"
    );
    assert_eq!(
        turn_message(
            &caps,
            &ExternalSessionInput::Continue {
                message: "m".to_owned()
            }
        )
        .expect("continue text"),
        "m"
    );
    assert!(matches!(
        turn_message(
            &caps,
            &ExternalSessionInput::RespondInteraction {
                action_id: "a".to_owned(),
                response: crate::agent::interaction::InteractionResponse::Answer("yes".to_owned()),
            }
        ),
        Err(ExternalAgentError::UnsupportedCapability {
            capability: ExternalCapability::PermissionBridge,
            ..
        })
    ));
    assert!(matches!(
        turn_message(
            &caps,
            &ExternalSessionInput::RespondSubagent {
                request_id: crate::agent::external::ExternalSubagentRequestId::new("req-1"),
                output: crate::agent::external::ExternalSubagentOutput {
                    summary: "done".to_owned(),
                    raw: None,
                },
            }
        ),
        Err(ExternalAgentError::UnsupportedCapability {
            capability: ExternalCapability::HostSubagents,
            ..
        })
    ));
    assert!(matches!(
        turn_message(&caps, &ExternalSessionInput::Shutdown),
        Err(ExternalAgentError::Protocol { .. })
    ));
}

#[test]
fn opencode_turn_spec_appends_prompt_and_message_to_base_args() {
    let config = OpenCodeConfig::new().with_permission_mode(ExternalPermissionMode::AcceptEdits);

    let fresh = OpenCodeTurnSpec::Fresh {
        prompt: "do it".to_owned(),
    }
    .args(&config);
    assert_eq!(fresh.last().map(String::as_str), Some("do it"));
    assert!(fresh.iter().any(|a| a == "run"));
    assert!(!fresh.iter().any(|a| a == "--session"));

    let resume = OpenCodeTurnSpec::Resume {
        session_id: "ses_9".to_owned(),
        message: "again".to_owned(),
    }
    .args(&config);
    assert_eq!(resume.last().map(String::as_str), Some("again"));
    assert!(resume.iter().any(|a| a == "--session"));
    // The session id flag is followed by a `--` separator and then the
    // message.
    let id_pos = resume
        .iter()
        .position(|a| a == "ses_9")
        .expect("session id present");
    assert_eq!(
        id_pos,
        resume.len() - 3,
        "id precedes the `--` separator and the appended message"
    );
    assert_eq!(resume.get(id_pos + 1).map(String::as_str), Some("--"));
    assert_eq!(resume.get(id_pos + 2).map(String::as_str), Some("again"));

    // A configured working directory rides along as `--dir <path>` for both a
    // fresh and a resumed turn, so OpenCode confines its file operations to
    // the intended worktree rather than the launching checkout.
    let scoped = OpenCodeConfig::new().with_working_dir("/tmp/wt");
    let scoped_fresh = OpenCodeTurnSpec::Fresh {
        prompt: "go".to_owned(),
    }
    .args(&scoped);
    assert!(scoped_fresh.windows(2).any(|w| w == ["--dir", "/tmp/wt"]));
    let scoped_resume = OpenCodeTurnSpec::Resume {
        session_id: "ses_x".to_owned(),
        message: "more".to_owned(),
    }
    .args(&scoped);
    assert!(scoped_resume.windows(2).any(|w| w == ["--dir", "/tmp/wt"]));
}

#[test]
fn opencode_turn_spec_separates_dash_prefixed_prompt_with_double_dash() {
    // M2-4 / M-EXT-4: a message that starts with `-` must not be parsed
    // as a flag; a `--` separator keeps it positional (OpenCode's yargs
    // sets `populate--: true`).
    let config = OpenCodeConfig::new();

    let fresh = OpenCodeTurnSpec::Fresh {
        prompt: "--model openai/gpt-5".to_owned(),
    }
    .args(&config);
    assert_eq!(
        fresh.last().map(String::as_str),
        Some("--model openai/gpt-5")
    );
    assert_eq!(
        fresh.get(fresh.len() - 2).map(String::as_str),
        Some("--"),
        "prompt follows a `--` separator"
    );

    let resume = OpenCodeTurnSpec::Resume {
        session_id: "ses_9".to_owned(),
        message: "--session other".to_owned(),
    }
    .args(&config);
    assert_eq!(resume.last().map(String::as_str), Some("--session other"));
    assert_eq!(
        resume.get(resume.len() - 2).map(String::as_str),
        Some("--"),
        "message follows a `--` separator"
    );
}

#[test]
fn opencode_adapter_implemented_capabilities_disable_host_bridges() {
    let caps = implemented_capabilities();
    assert!(caps.streaming);
    assert!(caps.resume);
    assert!(caps.artifacts);
    assert!(caps.usage);
    assert!(caps.graceful_shutdown);
    assert!(
        !caps.permission_bridge,
        "opencode run never pauses for approval"
    );
    assert!(!caps.host_tools, "no host-tool bridge");
    assert!(!caps.host_subagents, "no subagent bridge");
}

#[test]
fn opencode_adapter_probed_capabilities_intersect_with_implemented() {
    let mut probed = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::OpenCode);
    // A CLI that advertises streaming but not resume, and claims host tools.
    probed.streaming = true;
    probed.resume = false;
    probed.host_tools = true;
    probed.artifacts = true;
    probed.usage = true;
    probed.graceful_shutdown = true;

    let adapter = OpenCodeAdapter::with_probed_capabilities(OpenCodeConfig::new(), &probed);
    let caps = adapter.capabilities();
    assert!(caps.streaming, "streaming is implemented and probed");
    assert!(!caps.resume, "resume is off because the probe lacked it");
    assert!(
        !caps.host_tools,
        "host tools stay off even though the probe claimed them"
    );
    assert_eq!(adapter.kind(), ExternalRuntimeKind::OpenCode);
}

#[test]
fn opencode_adapter_intersect_keeps_left_runtime_and_ands_flags() {
    let left = implemented_capabilities();
    let right = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::OpenCode);
    let both = process::intersect_capabilities(&left, &right);
    assert_eq!(both.runtime, ExternalRuntimeKind::OpenCode);
    for capability in ExternalCapability::ALL {
        assert!(!both.supports(capability));
    }
}

#[tokio::test]
async fn opencode_adapter_start_rejects_declared_tools() {
    let tool = crate::model::tool::Tool {
        name: "search".to_owned(),
        description: "search the repo".to_owned(),
        input_schema: serde_json::json!({ "type": "object" }),
    };
    let adapter = OpenCodeAdapter::new(OpenCodeConfig::new());
    let ctx = run_context();
    let outcome = adapter.start(&start_request(vec![tool]), &ctx, None).await;
    match outcome {
        Err(ExternalAgentError::UnsupportedCapability {
            capability,
            runtime,
            ..
        }) => {
            assert_eq!(capability, ExternalCapability::HostTools);
            assert_eq!(runtime, ExternalRuntimeKind::OpenCode);
        }
        Err(other) => panic!("expected UnsupportedCapability, got {other:?}"),
        Ok(_) => panic!("declared host tools must be refused before spawning"),
    }
}

/// H-EXT-3: `close` classifies the child exit by status code, so a crashed
/// turn process is never mistaken for a clean close (which would mark a
/// dirty worktree as reusable). These tests spawn a real short-lived `sh`
/// child wired exactly like the production turn stream.
mod close_classification {
    use crate::agent::external::ExternalSessionShutdown;
    use crate::agent::external::process::{self, ChildStdinMode, ManagedChild};
    use std::time::Duration;
    use tokio::process::Command;

    /// Spawns a real `sh -c <script>` child with piped stdout.
    fn spawn_sh(script: &str) -> ManagedChild {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        ManagedChild::spawn(
            command,
            ChildStdinMode::Null,
            Duration::from_secs(1),
            Duration::from_millis(250),
            "stdout is piped",
            "test read timed out",
        )
        .expect("spawn sh")
    }

    /// A zero exit status closes `Graceful`.
    #[tokio::test]
    async fn zero_exit_is_graceful() {
        let mut turn = spawn_sh("exit 0");
        assert_eq!(turn.close().await, ExternalSessionShutdown::Graceful);
    }

    /// A non-zero exit status closes `Failed`, not `Graceful`.
    #[tokio::test]
    async fn nonzero_exit_is_failed() {
        let mut turn = spawn_sh("exit 1");
        assert_eq!(turn.close().await, ExternalSessionShutdown::Failed);
    }

    /// A child still running past the grace window is force-killed.
    #[tokio::test]
    async fn grace_overrun_is_forced_kill() {
        let mut turn = spawn_sh("sleep 30");
        assert_eq!(turn.close().await, ExternalSessionShutdown::ForcedKill);
    }

    /// H-EXT-2: a force-close kills the whole process group, so
    /// grandchildren the CLI spawned (builds, dev servers, ...) cannot
    /// outlive the turn.
    #[cfg(unix)]
    #[tokio::test]
    async fn force_close_kills_the_whole_process_group() {
        let mut turn = spawn_sh("sleep 300 & sleep 300");
        let pgid = turn.child_id().expect("child id") as i32;
        assert_eq!(turn.close().await, ExternalSessionShutdown::ForcedKill);
        process::assert_process_group_reaped(pgid).await;
    }
}

#[test]
fn session_config_applies_request_level_policy_overrides() {
    // M2-7: the request's policy overrides the construction-time config;
    // the prepared session dir flows into the `--dir` flag OpenCode
    // actually resolves file operations from.
    let adapter = OpenCodeAdapter::new(
        OpenCodeConfig::new()
            .with_permission_mode(ExternalPermissionMode::Prompt)
            .with_working_dir("/config/dir"),
    );

    let mut request = start_request(Vec::new());
    request.policy.permission_mode = ExternalPermissionMode::BypassPermissions;
    request.session_dir = Some(WorktreeRef::new("/prepared/session-0"));

    let effective = adapter.session_config(&request);
    assert_eq!(
        effective.permission_mode(),
        ExternalPermissionMode::BypassPermissions,
    );
    assert!(effective.auto_approve());
    let spec = OpenCodeTurnSpec::Fresh {
        prompt: "do the thing".to_owned(),
    };
    let args = spec.args(&effective);
    assert!(args.iter().any(|arg| arg == "--auto"));
    let dir = args
        .iter()
        .position(|arg| arg == "--dir")
        .expect("--dir flag present");
    assert_eq!(args[dir + 1], "/prepared/session-0");

    let fallback = adapter.session_config(&start_request(Vec::new()));
    assert!(
        !fallback.auto_approve(),
        "the fixture policy is AcceptEdits"
    );
    assert_eq!(
        fallback.working_dir(),
        Some(std::path::Path::new("/config/dir")),
        "without a prepared session dir the config working dir stays"
    );
}
