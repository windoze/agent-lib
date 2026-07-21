use super::{
    ClaudeCodeAdapter, ClaudeCodeSession, ClaudeSessionIo, control_response_frame,
    implemented_capabilities, user_text_frame,
};
use crate::agent::external::ClaudeCodeConfig;
use crate::agent::external::process;
use crate::agent::external::{
    ExternalAgentError, ExternalCapability, ExternalEventSink, ExternalObservedEvent,
    ExternalPermissionMode, ExternalRuntimeAdapter, ExternalRuntimeCapabilities,
    ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionInput, ExternalSessionPolicy,
    ExternalSessionRequest, ExternalSessionShutdown, ExternalStreamPolicy, RuntimeDecisionPoint,
    WorktreeIsolation,
};
use crate::agent::interaction::InteractionResponse;
use crate::agent::permission::PermissionResponse;
use crate::agent::spec::WorktreeRef;
use crate::agent::{AgentId, BudgetLimits, RunContext, RunId, TraceNodeId};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const SESSION_ID: &str = "claude-sess-1";
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
        TraceNodeId::new("claude-code-adapter-test"),
    )
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
        runtime: ExternalRuntimeKind::ClaudeCode,
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

/// A fake transport replaying canned stdout lines and capturing stdin frames.
struct FakeIo {
    lines: VecDeque<String>,
    written: Arc<Mutex<Vec<String>>>,
    close_disposition: ExternalSessionShutdown,
    closed: Arc<Mutex<Option<ExternalSessionShutdown>>>,
    /// Line replayed forever once `lines` drains (prelude-deadline tests).
    repeat: Option<String>,
    /// When set, reads pend forever once `lines` drains (silent-peer
    /// cancellation tests).
    pending: bool,
}

impl FakeIo {
    fn new(lines: Vec<String>) -> (Self, Arc<Mutex<Vec<String>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let io = Self {
            lines: lines.into_iter().collect(),
            written: Arc::clone(&written),
            close_disposition: ExternalSessionShutdown::Graceful,
            closed: Arc::new(Mutex::new(None)),
            repeat: None,
            pending: false,
        };
        (io, written)
    }

    fn with_close(mut self, disposition: ExternalSessionShutdown) -> Self {
        self.close_disposition = disposition;
        self
    }

    /// Replays `line` forever once the canned lines drain, so a prelude that
    /// never sees its `init` frame can only end on the launch deadline.
    fn repeating(mut self, line: String) -> Self {
        self.repeat = Some(line);
        self
    }

    /// Reads pend forever once the canned lines drain, modelling a live but
    /// silent CLI.
    fn silent_after_script(mut self) -> Self {
        self.pending = true;
        self
    }
}

#[async_trait]
impl ClaudeSessionIo for FakeIo {
    async fn write_frame(&mut self, frame: &str) -> std::io::Result<()> {
        self.written.lock().unwrap().push(frame.to_owned());
        Ok(())
    }

    async fn read_frame(&mut self) -> std::io::Result<Option<String>> {
        match self.lines.pop_front() {
            Some(line) => Ok(Some(line)),
            None if self.pending => std::future::pending().await,
            None => Ok(self.repeat.clone()),
        }
    }

    async fn close(&mut self) -> ExternalSessionShutdown {
        *self.closed.lock().unwrap() = Some(self.close_disposition);
        self.close_disposition
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
    lines: Vec<String>,
    sink: Option<Arc<dyn ExternalEventSink>>,
) -> (ClaudeCodeSession<FakeIo>, Arc<Mutex<Vec<String>>>) {
    let (io, written) = FakeIo::new(lines);
    let context = ClaudeCodeAdapter::decode_context(&run_context(), &start_request(Vec::new()));
    let session = ClaudeCodeSession::new(io, context, sink, implemented_capabilities());
    (session, written)
}

fn init_frame() -> String {
    format!(r#"{{"type":"system","subtype":"init","session_id":"{SESSION_ID}","cwd":"/repo"}}"#)
}

fn assistant_text_frame(text: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"id":"msg-1","role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

fn permission_request_frame(request_id: &str) -> String {
    format!(
        r#"{{"type":"control_request","request_id":"{request_id}","request":{{"subtype":"can_use_tool","tool_name":"Bash","input":{{"command":"cargo test"}}}}}}"#
    )
}

fn result_frame() -> String {
    r#"{"type":"result","subtype":"success","result":"all good","total_cost_usd":0.01,"usage":{"input_tokens":10,"output_tokens":5}}"#.to_owned()
}

#[tokio::test]
async fn claude_code_adapter_advance_drives_text_permission_completion() {
    let sink = Arc::new(RecordingSink::default());
    let (mut session, written) = session_over(
        vec![
            init_frame(),
            assistant_text_frame("looking into it"),
            permission_request_frame("perm-1"),
            assistant_text_frame("running the test"),
            result_frame(),
        ],
        Some(Arc::clone(&sink) as Arc<dyn ExternalEventSink>),
    );

    session
        .begin(
            Some(&start_request(Vec::new()).input),
            None,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("start writes the first turn and reads the init frame");
    assert_eq!(
        session.session_ref().session_id.as_deref(),
        Some(SESSION_ID)
    );

    let ctx = run_context();
    // Turn 1: the prompt is written, the session streams text then pauses for
    // the permission control request.
    let first = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect("first advance settles on a decision");
    let action_id = match first {
        RuntimeDecisionPoint::PausedForInteraction {
            action_id,
            observations,
            ..
        } => {
            // The init SessionStarted plus the text delta plus the permission
            // observation all ride the first decision point.
            assert!(
                observations.len() >= 3,
                "carried prelude + turn observations"
            );
            action_id
        }
        other => panic!("expected a permission pause, got {other:?}"),
    };
    assert_eq!(action_id, "perm-1");
    // The prompt frame was written to stdin.
    assert!(written.lock().unwrap()[0].contains("investigate the failing test"));

    // Turn 2: answering the permission writes a control_response and the
    // session runs to completion.
    let approve = InteractionResponse::Permission(PermissionResponse::approve(action_id.clone()));
    let respond = ExternalSessionInput::RespondInteraction {
        action_id,
        response: approve,
    };
    let second = session.advance(&respond, &ctx).await.expect("completion");
    match second {
        RuntimeDecisionPoint::Completed { output, .. } => {
            assert_eq!(output.summary, "all good");
            assert_eq!(output.cost_micros, Some(10_000));
            assert!(output.usage.is_some());
        }
        other => panic!("expected completion, got {other:?}"),
    }
    // The control_response frame echoes the runtime request id and allows.
    let frames = written.lock().unwrap().clone();
    assert!(
        frames
            .iter()
            .any(|f| f.contains("control_response") && f.contains("perm-1") && f.contains("allow"))
    );

    // The sink saw the same sequenced observations, monotonically.
    let seqs: Vec<u64> = sink.events.lock().unwrap().iter().map(|e| e.seq).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq is monotonic: {seqs:?}"
    );
}

#[tokio::test]
async fn claude_code_adapter_advance_reports_session_lost_on_early_eof() {
    let (mut session, _written) = session_over(vec![init_frame()], None);
    session
        .begin(
            Some(&start_request(Vec::new()).input),
            None,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("start prelude");
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
async fn claude_code_adapter_advance_propagates_protocol_error_on_malformed_frame() {
    let mut frames = vec![init_frame()];
    frames.extend((0..=8).map(|_| "{ not json".to_owned()));
    let (mut session, _written) = session_over(frames, None);
    session
        .begin(
            Some(&start_request(Vec::new()).input),
            None,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("start prelude");
    let ctx = run_context();
    let error = session
        .advance(&start_request(Vec::new()).input, &ctx)
        .await
        .expect_err("too much non-json noise is a protocol error");
    assert!(matches!(error, ExternalAgentError::Protocol { .. }));
}

#[tokio::test]
async fn claude_code_adapter_shutdown_classifies_the_close() {
    let (io, _written) = FakeIo::new(vec![init_frame()]);
    let io = io.with_close(ExternalSessionShutdown::ForcedKill);
    let context = ClaudeCodeAdapter::decode_context(&run_context(), &start_request(Vec::new()));
    let mut session = ClaudeCodeSession::new(io, context, None, implemented_capabilities());
    session
        .begin(
            Some(&start_request(Vec::new()).input),
            None,
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("start prelude");
    assert_eq!(
        session.shutdown().await,
        ExternalSessionShutdown::ForcedKill
    );
}

#[tokio::test]
async fn claude_code_adapter_begin_times_out_when_init_never_arrives() {
    // A CLI babbling tolerated non-init frames would loop the prelude forever
    // on the per-line read timeout alone (each line resets it); the launch
    // deadline caps the whole prelude (review M-EXT-6).
    let (io, _written) = FakeIo::new(Vec::new());
    let io = io.repeating(r#"{"type":"ping"}"#.to_owned());
    let context = ClaudeCodeAdapter::decode_context(&run_context(), &start_request(Vec::new()));
    let mut session = ClaudeCodeSession::new(io, context, None, implemented_capabilities());
    let started = std::time::Instant::now();
    let error = session
        .begin(
            Some(&start_request(Vec::new()).input),
            None,
            &run_context(),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect_err("a prelude that never reports an id hits the launch deadline");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the prelude deadline fires promptly"
    );
    match error {
        ExternalAgentError::Launch { runtime, detail } => {
            assert_eq!(runtime, ExternalRuntimeKind::ClaudeCode);
            assert!(detail.contains("launch timeout"), "detail: {detail}");
        }
        other => panic!("expected Launch, got {other:?}"),
    }
}

#[tokio::test]
async fn claude_code_adapter_begin_honours_cancellation() {
    // The prelude checks `ctx.is_cancelled()` per iteration, just like the
    // advance loop (review M-EXT-6).
    let (mut session, _written) = session_over(vec![init_frame()], None);
    let ctx = run_context();
    ctx.cancellation().cancel();
    let error = session
        .begin(
            Some(&start_request(Vec::new()).input),
            None,
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

/// M3-1: a cancel landing while the CLI is silent settles the advance in
/// seconds through the cancellation path, not after the read idle timeout.
#[tokio::test]
async fn claude_code_adapter_cancel_settles_a_silent_advance_promptly() {
    let (io, _written) = FakeIo::new(vec![init_frame()]);
    let io = io.silent_after_script();
    let context = ClaudeCodeAdapter::decode_context(&run_context(), &start_request(Vec::new()));
    let mut session = ClaudeCodeSession::new(io, context, None, implemented_capabilities());
    let ctx = run_context();
    session
        .begin(
            Some(&start_request(Vec::new()).input),
            None,
            &ctx,
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("start prelude");

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
async fn claude_code_adapter_respond_tool_results_is_unsupported() {
    // Resume-style begin: the session id is already known and no first turn is
    // pending, so the advance below reaches the input's capability check.
    let (mut session, _written) = session_over(vec![init_frame()], None);
    session
        .begin(
            None,
            Some(SESSION_ID.to_owned()),
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("resume prelude");
    let ctx = run_context();
    let input = ExternalSessionInput::RespondToolResults {
        batch_id: crate::agent::external::ExternalToolBatchId::new("batch-1"),
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
async fn claude_code_adapter_start_writes_prompt_before_reading_init() {
    // Claude Code stays silent until it receives the first turn, so `begin`
    // for a fresh start must write the prompt *before* consuming the init
    // frame that carries the session id.
    let (mut session, written) = session_over(
        vec![init_frame(), assistant_text_frame("hi"), result_frame()],
        None,
    );
    let input = start_request(Vec::new()).input;
    session
        .begin(Some(&input), None, &run_context(), PRELUDE_TIMEOUT)
        .await
        .expect("start prelude");

    // The session id was learned from the init frame, and the prompt was the
    // first (and so far only) frame written to stdin.
    assert_eq!(
        session.session_ref().session_id.as_deref(),
        Some(SESSION_ID)
    );
    let frames = written.lock().unwrap().clone();
    assert_eq!(frames.len(), 1, "only the prompt is written during begin");
    assert!(frames[0].contains("investigate the failing test"));

    // The first advance continues the in-flight turn without re-sending the
    // prompt, running it to completion.
    let ctx = run_context();
    let decision = session.advance(&input, &ctx).await.expect("completion");
    assert!(matches!(decision, RuntimeDecisionPoint::Completed { .. }));
    assert_eq!(
        written.lock().unwrap().len(),
        1,
        "the first advance must not write the prompt a second time"
    );
}

#[tokio::test]
async fn claude_code_adapter_resume_defers_first_turn_to_advance() {
    // Resume already knows the session id, so `begin` reads nothing; the first
    // advance writes its continuation turn and reads the fresh init + result.
    let (mut session, written) = session_over(
        vec![
            init_frame(),
            assistant_text_frame("resumed"),
            result_frame(),
        ],
        None,
    );
    session
        .begin(
            None,
            Some(SESSION_ID.to_owned()),
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("resume prelude");
    assert_eq!(
        session.session_ref().session_id.as_deref(),
        Some(SESSION_ID)
    );
    assert!(
        written.lock().unwrap().is_empty(),
        "resume writes nothing until the first advance"
    );

    let ctx = run_context();
    let input = ExternalSessionInput::Continue {
        message: "keep going".to_owned(),
    };
    let decision = session.advance(&input, &ctx).await.expect("completion");
    assert!(matches!(decision, RuntimeDecisionPoint::Completed { .. }));
    let frames = written.lock().unwrap().clone();
    assert_eq!(frames.len(), 1, "the continuation turn is written once");
    assert!(frames[0].contains("keep going"));
}

#[tokio::test]
async fn claude_code_adapter_resume_continues_the_seq_line_past_the_high_water() {
    // A resume must continue the decoder's seq line past the persisted
    // `last_event_seq`: restarting at 0 would let the machine's replay dedup
    // silently drop every post-resume observation (design §5.5, review
    // M-EXT-1).
    let (session, _written) = session_over(
        vec![
            init_frame(),
            assistant_text_frame("resumed"),
            result_frame(),
        ],
        None,
    );
    let mut session = session.with_resume_high_water(Some(50));
    session
        .begin(
            None,
            Some(SESSION_ID.to_owned()),
            &run_context(),
            PRELUDE_TIMEOUT,
        )
        .await
        .expect("resume prelude");
    // The restored water mark is reported even before any fresh event.
    assert_eq!(session.session_ref().last_event_seq, Some(50));

    let ctx = run_context();
    let input = ExternalSessionInput::Continue {
        message: "keep going".to_owned(),
    };
    let decision = session.advance(&input, &ctx).await.expect("completion");
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
fn claude_code_adapter_implemented_capabilities_disable_host_tools_and_subagents() {
    let caps = implemented_capabilities();
    assert!(caps.streaming);
    assert!(caps.resume);
    assert!(caps.permission_bridge);
    assert!(caps.artifacts);
    assert!(caps.usage);
    assert!(caps.graceful_shutdown);
    assert!(!caps.host_tools, "no MCP bridge means no host tools");
    assert!(
        !caps.host_subagents,
        "no spawn bridge means no host subagents"
    );
}

#[test]
fn claude_code_adapter_probed_capabilities_intersect_with_implemented() {
    let mut probed = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::ClaudeCode);
    // A CLI that advertises streaming but not resume, and claims host tools.
    probed.streaming = true;
    probed.resume = false;
    probed.permission_bridge = true;
    probed.host_tools = true;
    probed.artifacts = true;
    probed.usage = true;
    probed.graceful_shutdown = true;

    let adapter = ClaudeCodeAdapter::with_probed_capabilities(ClaudeCodeConfig::new(), &probed);
    let caps = adapter.capabilities();
    assert!(caps.streaming, "streaming is implemented and probed");
    assert!(!caps.resume, "resume is off because the probe lacked it");
    assert!(
        !caps.host_tools,
        "host tools stay off even though the probe claimed them"
    );
    assert_eq!(adapter.kind(), ExternalRuntimeKind::ClaudeCode);
}

#[test]
fn claude_code_adapter_intersect_keeps_left_runtime_and_ands_flags() {
    let left = implemented_capabilities();
    let right = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::ClaudeCode);
    let both = process::intersect_capabilities(&left, &right);
    assert_eq!(both.runtime, ExternalRuntimeKind::ClaudeCode);
    for capability in ExternalCapability::ALL {
        assert!(!both.supports(capability));
    }
}

#[tokio::test]
async fn claude_code_adapter_start_rejects_declared_tools() {
    let tool = crate::model::tool::Tool {
        name: "search".to_owned(),
        description: "search the repo".to_owned(),
        input_schema: serde_json::json!({ "type": "object" }),
    };
    let adapter = ClaudeCodeAdapter::new(ClaudeCodeConfig::new());
    let ctx = run_context();
    let outcome = adapter.start(&start_request(vec![tool]), &ctx, None).await;
    match outcome {
        Err(ExternalAgentError::UnsupportedCapability {
            capability,
            runtime,
            ..
        }) => {
            assert_eq!(capability, ExternalCapability::HostTools);
            assert_eq!(runtime, ExternalRuntimeKind::ClaudeCode);
        }
        Err(other) => panic!("expected UnsupportedCapability, got {other:?}"),
        Ok(_) => panic!("declared host tools must be refused before spawning"),
    }
}

#[test]
fn claude_code_adapter_user_text_frame_is_a_valid_stream_json_user_turn() {
    let frame = user_text_frame("hello \"world\"");
    let value: serde_json::Value = serde_json::from_str(&frame).expect("valid json");
    assert_eq!(value["type"], "user");
    assert_eq!(value["message"]["content"][0]["text"], "hello \"world\"");
}

#[test]
fn claude_code_adapter_control_response_frame_maps_allow_and_deny() {
    let allow = control_response_frame(
        "perm-9",
        &InteractionResponse::Permission(PermissionResponse::approve("perm-9".to_owned())),
    )
    .expect("allow frame");
    let allow_value: serde_json::Value = serde_json::from_str(&allow).expect("json");
    assert_eq!(allow_value["response"]["request_id"], "perm-9");
    assert_eq!(allow_value["response"]["response"]["behavior"], "allow");

    let deny = control_response_frame(
        "perm-9",
        &InteractionResponse::Permission(PermissionResponse::deny(
            "perm-9".to_owned(),
            Some("not allowed".to_owned()),
        )),
    )
    .expect("deny frame");
    let deny_value: serde_json::Value = serde_json::from_str(&deny).expect("json");
    assert_eq!(deny_value["response"]["response"]["behavior"], "deny");
    assert_eq!(deny_value["response"]["response"]["message"], "not allowed");
}

#[test]
fn claude_code_adapter_control_response_frame_rejects_non_permission_response() {
    let error = control_response_frame("perm-9", &InteractionResponse::Answer("yes".to_owned()))
        .expect_err("only permission responses are valid");
    assert!(matches!(error, ExternalAgentError::Protocol { .. }));
}

#[test]
fn claude_code_adapter_cancel_decision_maps_to_deny() {
    let frame = control_response_frame(
        "perm-9",
        &InteractionResponse::Permission(PermissionResponse::cancel("perm-9".to_owned())),
    )
    .expect("cancel frame");
    let value: serde_json::Value = serde_json::from_str(&frame).expect("json");
    assert_eq!(value["response"]["response"]["behavior"], "deny");
}

/// H-EXT-3: `close` classifies the child exit by status code, so a crashed
/// CLI is never mistaken for a clean close (which would mark a dirty
/// worktree as reusable). These tests spawn a real short-lived `sh` child
/// wired exactly like the production transport.
mod close_classification {
    use crate::agent::external::ExternalSessionShutdown;
    use crate::agent::external::process::{self, ChildStdinMode, ManagedChild};
    use std::time::Duration;
    use tokio::process::Command;

    /// Spawns a real `sh -c <script>` child with piped stdio.
    fn spawn_sh(script: &str) -> ManagedChild {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        ManagedChild::spawn(
            command,
            ChildStdinMode::Piped,
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
        let mut io = spawn_sh("exit 0");
        assert_eq!(io.close().await, ExternalSessionShutdown::Graceful);
    }

    /// A non-zero exit status closes `Failed`, not `Graceful`.
    #[tokio::test]
    async fn nonzero_exit_is_failed() {
        let mut io = spawn_sh("exit 1");
        assert_eq!(io.close().await, ExternalSessionShutdown::Failed);
    }

    /// A child still running past the grace window is force-killed.
    #[tokio::test]
    async fn grace_overrun_is_forced_kill() {
        let mut io = spawn_sh("sleep 30");
        assert_eq!(io.close().await, ExternalSessionShutdown::ForcedKill);
    }

    /// H-EXT-2: a force-close kills the whole process group, so
    /// grandchildren the CLI spawned (builds, dev servers, ...) cannot
    /// outlive the session.
    #[cfg(unix)]
    #[tokio::test]
    async fn force_close_kills_the_whole_process_group() {
        let mut io = spawn_sh("sleep 300 & sleep 300");
        let pgid = io.child_id().expect("child id") as i32;
        assert_eq!(io.close().await, ExternalSessionShutdown::ForcedKill);
        process::assert_process_group_reaped(pgid).await;
    }
}

#[test]
fn session_config_applies_request_level_policy_overrides() {
    // M2-7: the request's policy overrides the construction-time config —
    // permission_mode always, session_dir (the registry-prepared worktree)
    // when present; the config's working_dir remains the fallback.
    let adapter = ClaudeCodeAdapter::new(
        ClaudeCodeConfig::new()
            .with_permission_mode(ExternalPermissionMode::Prompt)
            .with_working_dir("/config/dir"),
    );

    let mut request = start_request(Vec::new());
    request.policy.permission_mode = ExternalPermissionMode::Plan;
    request.session_dir = Some(WorktreeRef::new("/prepared/session-0"));

    let effective = adapter.session_config(&request);
    assert_eq!(
        effective.permission_mode(),
        ExternalPermissionMode::Plan,
        "the request policy mode wins over the config mode"
    );
    assert_eq!(
        effective.working_dir(),
        Some(std::path::Path::new("/prepared/session-0")),
        "the prepared session dir wins over the config working dir"
    );
    let args = effective.base_session_args();
    let flag = args
        .iter()
        .position(|arg| arg == "--permission-mode")
        .expect("permission-mode flag present");
    assert_eq!(args[flag + 1], "plan");

    let fallback = adapter.session_config(&start_request(Vec::new()));
    assert_eq!(
        fallback.working_dir(),
        Some(std::path::Path::new("/config/dir")),
        "without a prepared session dir the config working dir stays"
    );
    assert_eq!(
        fallback.permission_mode(),
        ExternalPermissionMode::Prompt,
        "the fixture request policy still overrides (here: same value)"
    );
}
