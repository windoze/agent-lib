//! Offline registry-backed drain coverage for the ACP live session adapter
//! (milestone 10, M10-3).
//!
//! This is the production-shaped composition the managed real e2e uses, but over
//! a *fake* ACP transport instead of a spawned CLI: an [`AcpAdapter`] built with
//! a scripted [`AcpLauncher`] sits behind an [`ExternalSessionRegistry`], a
//! registry-backed [`ExternalSessionHandler`] resolves and advances the live
//! session one [`RuntimeDecisionPoint`] at a time, and a real
//! [`ExternalAgentMachine`] is [`drain`]ed to `Done` through the testkit. The
//! scripted agent streams a text chunk, asks one `session/request_permission`,
//! and then completes — so the drain exercises the full
//! start → pause → RespondInteraction → completed path end to end, with the
//! permission answered by a [`ScriptedInteractionHandler`].
//!
//! The whole file compiles only under `--features external-acp`; with the
//! feature off it is an empty crate, so the default offline suite never links
//! the ACP adapter.
//!
//! ```text
//! cargo test --features external-acp --test agent_acp_adapter_drain
//! ```

#![cfg(feature = "external-acp")]

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use agent_lib::agent::external::{
    AcpAdapter, AcpConfig, AcpLauncher, ExternalSessionRegistry, RuntimeDecisionPoint,
    SpawnedAcpAgent, acp_runtime_kind,
};
use agent_lib::agent::{
    ExternalAgentError, ExternalAgentMachine, ExternalAgentSpec, ExternalPermissionMode,
    ExternalSessionHandler, ExternalSessionPolicy, ExternalSessionRequest, ExternalSessionResult,
    ExternalStreamPolicy, LoopCursorKind, RequirementResult, RunContext, ToolSetRef,
    WorktreeIsolation, WorktreeRef, drain,
};
use agent_lib::conversation::{Conversation, ConversationConfig};
use agent_testkit::prelude::*;
use async_trait::async_trait;

const SESSION_ID: &str = "acp-drain-1";

/// A capturing `AsyncWrite` recording every byte the session writes to the agent.
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

/// A fake ACP launcher replaying a canned agent transcript once and capturing the
/// client frames the session writes back.
struct FakeLauncher {
    lines: Mutex<Option<String>>,
    written: Arc<Mutex<Vec<u8>>>,
}

impl FakeLauncher {
    fn new(lines: &[&str]) -> Self {
        Self {
            lines: Mutex::new(Some(lines.join("\n"))),
            written: Arc::new(Mutex::new(Vec::new())),
        }
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
        Ok(SpawnedAcpAgent::new(writer, reader, Duration::from_secs(5)))
    }
}

/// A production-shaped [`ExternalSessionHandler`] holding no machine state: it
/// resolves a live handle through its [`ExternalSessionRegistry`]
/// (`get_or_start` on the first `Start`, reattach on every follow-up) and
/// advances it exactly one [`RuntimeDecisionPoint`].
struct AcpManagedHandler {
    registry: Arc<ExternalSessionRegistry>,
}

impl AcpManagedHandler {
    async fn advance(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> ExternalSessionResult {
        let handle = match self.registry.get_or_start(request, ctx, None).await {
            Ok(handle) => handle,
            Err(error) => return Err::<RuntimeDecisionPoint, _>(error).into(),
        };
        let point = {
            let mut session = handle.lock().await;
            session.advance(&request.input, ctx).await
        };
        point.into()
    }
}

#[async_trait]
impl ExternalSessionHandler for AcpManagedHandler {
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::ExternalSession(Box::new(self.advance(request, ctx).await))
    }
}

fn init_line() -> String {
    r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}"#
        .to_owned()
}

fn new_session_line() -> String {
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

fn acp_policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        // Prompt so the scripted permission ask surfaces as a machine interaction.
        permission_mode: ExternalPermissionMode::Prompt,
        // The fake transport never touches the filesystem; the test owns the dir.
        isolation: WorktreeIsolation::Shared,
        max_turns: Some(8),
        stream_events: ExternalStreamPolicy::Streaming,
    }
}

fn acp_machine(ids: &SeqIds, worktree: &std::path::Path) -> ExternalAgentMachine {
    let spec = ExternalAgentSpec::new(
        ids.agent_id(),
        acp_runtime_kind(),
        WorktreeRef::new(worktree),
        None,
        ToolSetRef::new(ids.tool_set_id(), Vec::new()),
        acp_policy(),
    );
    let state = agent_lib::agent::ExternalAgentState::new(
        spec,
        Conversation::new(
            ids.conversation_id(),
            ConversationConfig::new(Some("ACP drain conversation.".to_owned())),
        ),
    );
    ExternalAgentMachine::new(state, Arc::new(ids.clone()))
}

/// A registry-backed ACP handler drains a real `ExternalAgentMachine` through the
/// start → permission pause → RespondInteraction → completed path.
#[tokio::test]
async fn acp_adapter_drain_start_pause_respond_completed() {
    let launcher = Arc::new(FakeLauncher::new(&[
        &init_line(),
        &new_session_line(),
        &text_line("working"),
        &permission_line(),
        &text_line(" done"),
        &prompt_result_line(),
    ]));
    let adapter = AcpAdapter::with_launcher(
        AcpConfig::opencode_acp(),
        Arc::clone(&launcher) as Arc<dyn AcpLauncher>,
    );
    let registry = Arc::new(ExternalSessionRegistry::new(Arc::new(adapter)));
    let handler = AcpManagedHandler {
        registry: Arc::clone(&registry),
    };

    let interaction = Arc::new(ScriptedInteractionHandler::sequence([
        InteractionDecision::Approve,
    ]));
    let interaction_log = Arc::clone(interaction.log());

    let ids = SeqIds::new();
    let agent_id = ids.agent_id();
    let worktree = std::env::temp_dir();
    let mut machine = acp_machine(&ids, &worktree);
    let scope = TestScope::builder()
        .external(Arc::new(handler) as Arc<dyn ExternalSessionHandler>)
        .interaction(interaction)
        .build();
    let ctx = root_context(&ids);

    let done = drain(
        &mut machine,
        user_input(&ids, "investigate the failing test"),
        &scope,
        None,
        &ctx,
    )
    .await
    .expect("the ACP session drains to completion");

    let _ = registry.cleanup_agent(agent_id).await;

    assert_eq!(
        done.cursor().kind(),
        LoopCursorKind::Done,
        "the drain settles on Done"
    );
    // Exactly one permission interaction was resolved mid-turn.
    assert_calls(&interaction_log).count(1).all_completed();
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none();

    // The client completed the ACP handshake and answered the permission ask by
    // selecting the allow option.
    let written = launcher.written();
    assert!(written.contains(r#""method":"initialize""#));
    assert!(written.contains(r#""method":"session/new""#));
    assert!(written.contains(r#""method":"session/prompt""#));
    assert!(written.contains(r#""optionId":"allow""#));
}
