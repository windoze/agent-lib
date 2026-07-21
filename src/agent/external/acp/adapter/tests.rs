use super::{
    AcpAdapter, apply_line_window, implemented_capabilities, json_rpc_id_value, permission_outcome,
    select_option,
};
use crate::agent::external::acp::{
    AcpConfig, AcpLauncher, AcpPermissionOption, AcpPermissionOptionKind, SpawnedAcpAgent,
    acp_runtime_kind,
};
use crate::agent::external::process;
use crate::agent::external::{
    ExternalAgentError, ExternalAgentEvent, ExternalCapability, ExternalEventSink,
    ExternalObservedEvent, ExternalPermissionMode, ExternalRuntimeAdapter, ExternalRuntimeSession,
    ExternalSessionInput, ExternalSessionPolicy, ExternalSessionRef, ExternalSessionRequest,
    ExternalSessionShutdown, ExternalStreamPolicy, ExternalSubagentOutput,
    ExternalSubagentRequestId, ExternalToolBatchId, RuntimeDecisionPoint, WorktreeIsolation,
};
use crate::agent::id::StepId;
use crate::agent::interaction::{InteractionKind, InteractionResponse};
use crate::agent::permission::{PermissionDecision, PermissionResponse};
use crate::agent::spec::WorktreeRef;
use crate::agent::{AgentId, BudgetLimits, RunContext, RunId, TraceNodeId};
use async_trait::async_trait;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

const RUN_UUID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890e0";
const AGENT_UUID: &str = "018f0d9c-7b6a-7c12-8f31-1234567890f0";
const SESSION_ID: &str = "sess-1";

fn agent_id() -> AgentId {
    AGENT_UUID.parse().expect("agent id parses")
}

fn run_context() -> RunContext {
    let run_id: RunId = RUN_UUID.parse().expect("run id parses");
    RunContext::new_root(
        run_id,
        BudgetLimits::unbounded(),
        TraceNodeId::new("acp-adapter-test"),
    )
}

fn expected_step_id() -> StepId {
    StepId::new(*run_context().run_id().as_uuid())
}

fn policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        permission_mode: ExternalPermissionMode::Prompt,
        isolation: WorktreeIsolation::EphemeralGitWorktree,
        max_turns: Some(8),
        stream_events: ExternalStreamPolicy::Streaming,
    }
}

fn start_request(tools: Vec<crate::model::tool::Tool>) -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent_id(),
        runtime: acp_runtime_kind(),
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

/// A capturing `AsyncWrite` recording every byte the session writes.
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl tokio::io::AsyncWrite for SharedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// A fake launcher replaying canned agent lines and capturing written frames.
struct FakeLauncher {
    lines: Mutex<Option<String>>,
    written: Arc<Mutex<Vec<u8>>>,
    shutdown: ExternalSessionShutdown,
}

impl FakeLauncher {
    fn new(lines: &[&str]) -> Self {
        Self {
            lines: Mutex::new(Some(lines.join("\n"))),
            written: Arc::new(Mutex::new(Vec::new())),
            shutdown: ExternalSessionShutdown::Graceful,
        }
    }

    fn with_shutdown(mut self, disposition: ExternalSessionShutdown) -> Self {
        self.shutdown = disposition;
        self
    }

    fn written(&self) -> String {
        String::from_utf8(self.written.lock().unwrap().clone()).expect("utf8 frames")
    }
}

#[async_trait]
impl AcpLauncher for FakeLauncher {
    async fn launch(&self, _config: &AcpConfig) -> Result<SpawnedAcpAgent, ExternalAgentError> {
        let lines = self.lines.lock().unwrap().take().unwrap_or_default();
        let reader = std::io::Cursor::new(lines.into_bytes());
        let writer = SharedWriter(Arc::clone(&self.written));
        Ok(SpawnedAcpAgent::new(writer, reader, Duration::from_secs(5))
            .with_shutdown_disposition(self.shutdown))
    }
}

/// A collecting sink recording every live observation.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<ExternalObservedEvent>>,
}

impl ExternalEventSink for RecordingSink {
    fn emit(&self, event: &ExternalObservedEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn init_line(load_session: bool) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":1,"agentCapabilities":{{"loadSession":{load_session}}}}}}}"#
    )
}

fn new_session_line() -> String {
    format!(r#"{{"jsonrpc":"2.0","id":2,"result":{{"sessionId":"{SESSION_ID}"}}}}"#)
}

fn load_session_line() -> String {
    format!(r#"{{"jsonrpc":"2.0","id":2,"result":{{"sessionId":"{SESSION_ID}"}}}}"#)
}

fn text_line(text: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"{SESSION_ID}","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"{text}"}}}}}}}}"#
    )
}

fn permission_line() -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{{"sessionId":"{SESSION_ID}","toolCall":{{"toolCallId":"call-1","title":"write src/x.rs"}},"options":[{{"optionId":"allow","name":"Allow","kind":"allow_once"}},{{"optionId":"reject","name":"Reject","kind":"reject_once"}}]}}}}"#
    )
}

fn prompt_result_line() -> String {
    r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#.to_owned()
}

async fn start_session(
    launcher: Arc<FakeLauncher>,
    sink: Option<Arc<dyn ExternalEventSink>>,
) -> (Box<dyn ExternalRuntimeSession>, RunContext) {
    let adapter =
        AcpAdapter::with_launcher(AcpConfig::opencode_acp(), launcher as Arc<dyn AcpLauncher>);
    let ctx = run_context();
    let session = match adapter.start(&start_request(Vec::new()), &ctx, sink).await {
        Ok(session) => session,
        Err(error) => panic!("start completes the handshake, got {error:?}"),
    };
    (session, ctx)
}

#[tokio::test]
async fn acp_adapter_start_permission_completion() {
    let sink = Arc::new(RecordingSink::default());
    let launcher = Arc::new(FakeLauncher::new(&[
        &init_line(true),
        &new_session_line(),
        &text_line("working"),
        &permission_line(),
        &text_line(" done"),
        &prompt_result_line(),
    ]));
    let (mut session, ctx) = start_session(
        Arc::clone(&launcher),
        Some(Arc::clone(&sink) as Arc<dyn ExternalEventSink>),
    )
    .await;

    assert_eq!(
        session.session_ref().session_id.as_deref(),
        Some(SESSION_ID)
    );

    // Turn 1: the prompt streams text then pauses for the permission request.
    let first = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect("first advance settles on a decision");
    let (action_id, interaction) = match first {
        RuntimeDecisionPoint::PausedForInteraction {
            action_id,
            request,
            observations,
            ..
        } => {
            // SessionStarted + text delta + permission observation ride the
            // first decision point.
            assert!(observations.len() >= 3, "carried handshake + turn events");
            (action_id, request)
        }
        other => panic!("expected a permission pause, got {other:?}"),
    };
    assert_eq!(action_id, "100");

    // The paused interaction's identities are bound to the host, not runtime
    // output: the step id comes from run_id and the actor from the request.
    assert_eq!(interaction.step_id(), expected_step_id());
    match interaction.kind() {
        InteractionKind::Permission { request } => {
            assert_eq!(request.actor(), agent_id());
            assert_eq!(request.action_id(), "100");
            assert_eq!(request.summary, "write src/x.rs");
        }
        other => panic!("expected a permission interaction, got {other:?}"),
    }

    // Turn 2: approving writes the ACP permission response and completes.
    let approve = InteractionResponse::Permission(PermissionResponse::approve(action_id.clone()));
    let respond = ExternalSessionInput::RespondInteraction {
        action_id,
        response: approve,
    };
    let second = session
        .advance(&respond, &ctx)
        .await
        .expect("the approval completes the turn");
    match second {
        RuntimeDecisionPoint::Completed { output, .. } => {
            assert_eq!(output.summary, "working done");
        }
        other => panic!("expected completion, got {other:?}"),
    }

    // The client wrote initialize, session/new, session/prompt, and a
    // permission response selecting the allow option.
    let written = launcher.written();
    assert!(written.contains(r#""method":"initialize""#));
    assert!(written.contains(r#""method":"session/new""#));
    assert!(written.contains(r#""method":"session/prompt""#));
    assert!(written.contains(r#""outcome":"selected""#));
    assert!(written.contains(r#""optionId":"allow""#));

    // The sink saw the same sequenced observations, monotonically.
    let seqs: Vec<u64> = sink.events.lock().unwrap().iter().map(|e| e.seq).collect();
    assert!(!seqs.is_empty());
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq is monotonic: {seqs:?}"
    );
}

#[tokio::test]
async fn acp_adapter_permission_deny_selects_reject() {
    let launcher = Arc::new(FakeLauncher::new(&[
        &init_line(true),
        &new_session_line(),
        &permission_line(),
        &prompt_result_line(),
    ]));
    let (mut session, ctx) = start_session(Arc::clone(&launcher), None).await;

    let first = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect("pauses for the permission request");
    let action_id = match first {
        RuntimeDecisionPoint::PausedForInteraction { action_id, .. } => action_id,
        other => panic!("expected a permission pause, got {other:?}"),
    };

    let deny = InteractionResponse::Permission(PermissionResponse::deny(
        action_id.clone(),
        Some("not allowed".to_owned()),
    ));
    let respond = ExternalSessionInput::RespondInteraction {
        action_id,
        response: deny,
    };
    session
        .advance(&respond, &ctx)
        .await
        .expect("the denial resolves and the turn completes");

    let written = launcher.written();
    assert!(written.contains(r#""outcome":"selected""#));
    assert!(written.contains(r#""optionId":"reject""#));
    assert!(!written.contains(r#""optionId":"allow""#));
}

#[tokio::test]
async fn acp_adapter_services_fs_write_after_approval() {
    let dir = std::env::temp_dir().join(format!(
        "acp-adapter-fs-{}-{}",
        std::process::id(),
        SESSION_ID
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp worktree");
    let target = dir.join("out.txt");
    let target_wire = target.to_string_lossy().replace('\\', "\\\\");
    let write_line = format!(
        r#"{{"jsonrpc":"2.0","id":200,"method":"fs/write_text_file","params":{{"sessionId":"{SESSION_ID}","path":"{target_wire}","content":"hello"}}}}"#
    );

    let launcher = Arc::new(FakeLauncher::new(&[
        &init_line(true),
        &new_session_line(),
        &write_line,
        &prompt_result_line(),
    ]));
    let adapter = AcpAdapter::with_launcher(
        AcpConfig::opencode_acp().with_working_dir(&dir),
        Arc::clone(&launcher) as Arc<dyn AcpLauncher>,
    );
    let ctx = run_context();
    let mut session = match adapter.start(&start_request(Vec::new()), &ctx, None).await {
        Ok(session) => session,
        Err(error) => panic!("handshake, got {error:?}"),
    };

    let decision = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect("the fs write is serviced inline and the turn completes");
    match decision {
        RuntimeDecisionPoint::Completed { observations, .. } => {
            assert!(
                observations.iter().any(|event| matches!(
                    &event.event,
                    ExternalAgentEvent::FilePatch { path, .. } if path == &target.to_string_lossy()
                )),
                "the serviced write surfaces as a FilePatch observation"
            );
        }
        other => panic!("expected completion, got {other:?}"),
    }

    assert_eq!(
        std::fs::read_to_string(&target).expect("written file"),
        "hello"
    );
    assert!(launcher.written().contains(r#""id":200"#));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn acp_adapter_plan_mode_refuses_fs_write() {
    let dir = std::env::temp_dir().join(format!(
        "acp-adapter-plan-{}-{}",
        std::process::id(),
        SESSION_ID
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp worktree");
    let target = dir.join("blocked.txt");
    let target_wire = target.to_string_lossy().replace('\\', "\\\\");
    let write_line = format!(
        r#"{{"jsonrpc":"2.0","id":200,"method":"fs/write_text_file","params":{{"sessionId":"{SESSION_ID}","path":"{target_wire}","content":"hello"}}}}"#
    );

    let launcher = Arc::new(FakeLauncher::new(&[
        &init_line(true),
        &new_session_line(),
        &write_line,
        &prompt_result_line(),
    ]));
    let adapter = AcpAdapter::with_launcher(
        // The construction-time mode is Prompt; the *request* policy carries
        // Plan, and the request level wins (M2-7) — the write must still be
        // refused.
        AcpConfig::opencode_acp()
            .with_working_dir(&dir)
            .with_permission_mode(ExternalPermissionMode::Prompt),
        Arc::clone(&launcher) as Arc<dyn AcpLauncher>,
    );
    let ctx = run_context();
    let mut request = start_request(Vec::new());
    request.policy.permission_mode = ExternalPermissionMode::Plan;
    let mut session = match adapter.start(&request, &ctx, None).await {
        Ok(session) => session,
        Err(error) => panic!("handshake, got {error:?}"),
    };
    session
        .advance(&request.input, &ctx)
        .await
        .expect("the refused write still lets the turn complete");

    assert!(!target.exists(), "plan mode must not materialize the write");
    assert!(launcher.written().contains("plan mode"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn acp_adapter_connection_drop_is_session_lost() {
    let launcher = Arc::new(FakeLauncher::new(&[&init_line(true), &new_session_line()]));
    let (mut session, ctx) = start_session(Arc::clone(&launcher), None).await;

    let error = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect_err("an EOF before a decision is a lost session");
    match error {
        ExternalAgentError::SessionLost { session, .. } => {
            assert_eq!(
                session.and_then(|s| s.session_id).as_deref(),
                Some(SESSION_ID)
            );
        }
        other => panic!("expected SessionLost, got {other:?}"),
    }
}

/// An async reader that serves scripted bytes and then pends forever,
/// modelling a live but silent agent that never writes another line.
struct ScriptedThenSilent {
    scripted: std::io::Cursor<Vec<u8>>,
}

impl tokio::io::AsyncRead for ScriptedThenSilent {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        #[allow(clippy::cast_possible_truncation)]
        if self.scripted.position() < self.scripted.get_ref().len() as u64 {
            return Pin::new(&mut self.scripted).poll_read(cx, buf);
        }
        Poll::Pending
    }
}

/// A fake launcher whose agent answers the handshake from a script and
/// then stays silent forever.
struct SilentTurnLauncher {
    handshake: Mutex<Option<String>>,
    written: Arc<Mutex<Vec<u8>>>,
}

impl SilentTurnLauncher {
    fn new(lines: &[&str]) -> Self {
        // Every scripted line must be newline-terminated: the reader never
        // reports EOF, so an unterminated tail would pend forever.
        Self {
            handshake: Mutex::new(Some(format!("{}\n", lines.join("\n")))),
            written: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl AcpLauncher for SilentTurnLauncher {
    async fn launch(&self, _config: &AcpConfig) -> Result<SpawnedAcpAgent, ExternalAgentError> {
        let script = self.handshake.lock().unwrap().take().unwrap_or_default();
        let reader = ScriptedThenSilent {
            scripted: std::io::Cursor::new(script.into_bytes()),
        };
        let writer = SharedWriter(Arc::clone(&self.written));
        // A read timeout far beyond the test's settle bound: settling fast
        // proves cancellation — not the IO timeout — ended the wait.
        Ok(SpawnedAcpAgent::new(
            writer,
            reader,
            Duration::from_secs(60),
        ))
    }
}

/// M3-1: a cancel landing while the agent is silent settles the advance in
/// seconds through the cancellation path, not after the read timeout.
#[tokio::test]
async fn acp_adapter_cancel_settles_a_silent_advance_promptly() {
    let launcher = Arc::new(SilentTurnLauncher::new(&[
        &init_line(true),
        &new_session_line(),
    ]));
    let adapter =
        AcpAdapter::with_launcher(AcpConfig::opencode_acp(), launcher as Arc<dyn AcpLauncher>);
    let ctx = run_context();
    let mut session = match adapter.start(&start_request(Vec::new()), &ctx, None).await {
        Ok(session) => session,
        Err(error) => panic!("handshake, got {error:?}"),
    };

    let token = ctx.cancellation().clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });
    let settled = tokio::time::timeout(
        Duration::from_secs(2),
        session.advance(&start_request(Vec::new()).input, &ctx),
    )
    .await
    .expect("a cancelled advance settles well before the read timeout");
    canceller.await.expect("canceller task");

    match settled {
        Err(ExternalAgentError::SessionLost { session, detail }) => {
            assert!(detail.contains("cancelled"), "detail: {detail}");
            assert_eq!(
                session.and_then(|s| s.session_id).as_deref(),
                Some(SESSION_ID)
            );
        }
        other => panic!("expected a cancellation SessionLost, got {other:?}"),
    }
}

#[tokio::test]
async fn acp_adapter_protocol_violation_is_protocol() {
    let launcher = Arc::new(FakeLauncher::new(&[
        &init_line(true),
        &new_session_line(),
        "this is not json",
    ]));
    let (mut session, ctx) = start_session(Arc::clone(&launcher), None).await;

    let error = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect_err("a corrupt line is a protocol violation");
    assert!(matches!(error, ExternalAgentError::Protocol { .. }));
}

#[tokio::test]
async fn acp_adapter_shutdown_classifies_disposition() {
    let launcher = Arc::new(
        FakeLauncher::new(&[&init_line(true), &new_session_line()])
            .with_shutdown(ExternalSessionShutdown::ForcedKill),
    );
    let (mut session, _ctx) = start_session(Arc::clone(&launcher), None).await;

    assert_eq!(
        session.shutdown().await,
        ExternalSessionShutdown::ForcedKill
    );
    // The graceful stop wrote a session/cancel notification.
    assert!(launcher.written().contains(r#""method":"session/cancel""#));
}

#[tokio::test]
async fn acp_adapter_rejects_declared_tools() {
    let tool = crate::model::tool::Tool {
        name: "search".to_owned(),
        description: "search the repo".to_owned(),
        input_schema: serde_json::json!({ "type": "object" }),
    };
    let launcher = Arc::new(FakeLauncher::new(&[&init_line(true), &new_session_line()]));
    let adapter =
        AcpAdapter::with_launcher(AcpConfig::opencode_acp(), launcher as Arc<dyn AcpLauncher>);
    let ctx = run_context();
    let outcome = adapter.start(&start_request(vec![tool]), &ctx, None).await;
    match outcome {
        Ok(_) => panic!("declared host tools must be refused before spawning"),
        Err(ExternalAgentError::UnsupportedCapability {
            capability,
            runtime,
            ..
        }) => {
            assert_eq!(capability, ExternalCapability::HostTools);
            assert_eq!(runtime, acp_runtime_kind());
        }
        Err(other) => panic!("expected an UnsupportedCapability rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn acp_adapter_rejects_tool_and_subagent_results() {
    let launcher = Arc::new(FakeLauncher::new(&[&init_line(true), &new_session_line()]));
    let (mut session, ctx) = start_session(Arc::clone(&launcher), None).await;

    let tool_results = ExternalSessionInput::RespondToolResults {
        batch_id: ExternalToolBatchId::new("batch-1"),
        results: Vec::new(),
    };
    match session.advance(&tool_results, &ctx).await {
        Err(ExternalAgentError::UnsupportedCapability { capability, .. }) => {
            assert_eq!(capability, ExternalCapability::HostTools);
        }
        other => panic!("expected HostTools rejection, got {other:?}"),
    }

    let subagent = ExternalSessionInput::RespondSubagent {
        request_id: ExternalSubagentRequestId::new("req-1"),
        output: ExternalSubagentOutput {
            summary: "child done".to_owned(),
            raw: None,
        },
    };
    match session.advance(&subagent, &ctx).await {
        Err(ExternalAgentError::UnsupportedCapability { capability, .. }) => {
            assert_eq!(capability, ExternalCapability::HostSubagents);
        }
        other => panic!("expected HostSubagents rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn acp_adapter_resume_requires_load_session() {
    let launcher = Arc::new(FakeLauncher::new(&[&init_line(false)]));
    let adapter =
        AcpAdapter::with_launcher(AcpConfig::opencode_acp(), launcher as Arc<dyn AcpLauncher>);
    let ctx = run_context();
    let session_ref = ExternalSessionRef {
        runtime: acp_runtime_kind(),
        session_id: Some(SESSION_ID.to_owned()),
        transcript_ref: None,
        resume_token: Some(SESSION_ID.to_owned()),
        last_event_seq: None,
    };
    let outcome = adapter
        .resume(&session_ref, &start_request(Vec::new()), &ctx, None)
        .await;
    assert!(matches!(
        outcome,
        Err(ExternalAgentError::ResumeUnavailable { .. })
    ));
}

#[tokio::test]
async fn acp_adapter_resume_continues_the_seq_line_past_the_high_water() {
    // A resume must continue the decoder's seq line past the persisted
    // `last_event_seq`: restarting at 0 would let the machine's replay dedup
    // silently drop every post-resume observation (design §5.5, review
    // M-EXT-1).
    let launcher = Arc::new(FakeLauncher::new(&[
        &init_line(true),
        &load_session_line(),
        &text_line("resumed"),
        &prompt_result_line(),
    ]));
    let adapter =
        AcpAdapter::with_launcher(AcpConfig::opencode_acp(), launcher as Arc<dyn AcpLauncher>);
    let ctx = run_context();
    let session_ref = ExternalSessionRef {
        runtime: acp_runtime_kind(),
        session_id: Some(SESSION_ID.to_owned()),
        transcript_ref: None,
        resume_token: Some(SESSION_ID.to_owned()),
        last_event_seq: Some(50),
    };
    let mut session = adapter
        .resume(&session_ref, &start_request(Vec::new()), &ctx, None)
        .await
        .expect("resume attaches via session/load");
    // The handshake already emitted its first observations past the mark.
    assert!(
        session.session_ref().last_event_seq >= Some(50),
        "the reported water mark never regresses below the persisted one"
    );

    let decision = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect("completion");
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

#[test]
fn acp_adapter_capabilities_are_honest() {
    let capabilities = implemented_capabilities();
    assert_eq!(capabilities.runtime, acp_runtime_kind());
    assert!(capabilities.streaming);
    assert!(capabilities.resume);
    assert!(capabilities.permission_bridge);
    assert!(capabilities.graceful_shutdown);
    assert!(!capabilities.host_tools);
    assert!(!capabilities.host_subagents);
    assert!(!capabilities.artifacts);
    assert!(!capabilities.usage);
    assert!(!capabilities.reconfigure);

    // Intersecting with a set that lacks resume disables resume but keeps the
    // ACP runtime label.
    let mut probed = implemented_capabilities();
    probed.resume = false;
    let intersected = process::intersect_capabilities(&implemented_capabilities(), &probed);
    assert!(!intersected.resume);
    assert_eq!(intersected.runtime, acp_runtime_kind());
}

#[test]
fn acp_permission_outcome_maps_decisions() {
    let options = vec![
        AcpPermissionOption {
            option_id: "allow".to_owned(),
            kind: AcpPermissionOptionKind::AllowOnce,
        },
        AcpPermissionOption {
            option_id: "reject".to_owned(),
            kind: AcpPermissionOptionKind::RejectOnce,
        },
    ];
    assert_eq!(
        permission_outcome(&PermissionDecision::Approve, &options)["optionId"],
        "allow"
    );
    assert_eq!(
        permission_outcome(&PermissionDecision::Deny { reason: None }, &options)["optionId"],
        "reject"
    );
    assert_eq!(
        permission_outcome(&PermissionDecision::Cancel, &options)["outcome"],
        "cancelled"
    );
    // With no allow option offered, an approval falls back to cancelled.
    let reject_only = vec![AcpPermissionOption {
        option_id: "reject".to_owned(),
        kind: AcpPermissionOptionKind::RejectOnce,
    }];
    assert_eq!(
        permission_outcome(&PermissionDecision::Approve, &reject_only)["outcome"],
        "cancelled"
    );
    assert_eq!(select_option(&reject_only, true), None);
}

#[test]
fn acp_json_rpc_id_preserves_numeric_form() {
    assert_eq!(json_rpc_id_value("100"), serde_json::json!(100));
    assert_eq!(json_rpc_id_value("abc"), serde_json::json!("abc"));
}

#[test]
fn acp_line_window_applies_start_and_limit() {
    let content = "one\ntwo\nthree\nfour";
    assert_eq!(apply_line_window(content, None, None), content);
    assert_eq!(apply_line_window(content, Some(2), Some(2)), "two\nthree");
    assert_eq!(apply_line_window(content, Some(3), None), "three\nfour");
}

#[test]
fn session_config_applies_request_level_policy_overrides() {
    // M2-7: the request's policy overrides the construction-time config,
    // and the session cwd prefers the prepared session dir, then the
    // config working dir, then the request worktree.
    let adapter = AcpAdapter::new(
        AcpConfig::new("acp-stub", Vec::<String>::new())
            .with_permission_mode(ExternalPermissionMode::Prompt)
            .with_working_dir("/config/dir"),
    );

    let mut request = start_request(Vec::new());
    request.policy.permission_mode = ExternalPermissionMode::Plan;
    request.session_dir = Some(WorktreeRef::new("/prepared/session-0"));

    let effective = adapter.session_config(&request);
    assert_eq!(effective.permission_mode(), ExternalPermissionMode::Plan);
    assert_eq!(
        effective.working_dir(),
        Some(std::path::Path::new("/prepared/session-0")),
    );
    assert_eq!(
        adapter.session_cwd(&request),
        std::path::PathBuf::from("/prepared/session-0"),
        "the prepared session dir is the session cwd"
    );

    let fallback = adapter.session_cwd(&start_request(Vec::new()));
    assert_eq!(
        fallback,
        std::path::PathBuf::from("/config/dir"),
        "without a prepared session dir the config working dir stays"
    );
}
