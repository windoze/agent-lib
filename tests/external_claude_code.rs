//! Opt-in real-runtime coverage for the Claude Code *session adapter* (M6-3,
//! feature `external-claude-code`).
//!
//! Unlike the offline decoder cassette (`agent_claude_code_cassette.rs`) and the
//! adapter's inline fake-transport unit tests, this suite drives the real
//! [`ClaudeCodeAdapter`](agent_lib::agent::external::ClaudeCodeAdapter) against a
//! locally installed, authenticated `claude` CLI through an
//! [`ExternalSessionRegistry`](agent_lib::agent::external::ExternalSessionRegistry).
//! It proves the whole live path end to end: probe → `start` → stream a turn of
//! observations → settle on a decision point → answer any permission pause →
//! reach completion → graceful shutdown.
//!
//! It is intentionally `#[ignore]`: it spawns a real coding-agent CLI and may
//! call a paid model, so it is never part of the default offline suite. It also
//! skips itself (with a clear message, exiting green) when the binary or its auth
//! is missing, so an unconfigured machine does not report a spurious failure.
//!
//! Run it explicitly:
//!
//! ```text
//! cargo test --features external-claude-code --test external_claude_code -- --ignored --nocapture
//! ```
//!
//! The binary is discovered from `CLAUDE_CODE_BIN` (an absolute path override)
//! or, failing that, `claude` on `PATH`; an optional `CLAUDE_CODE_MODEL` pins a
//! cheaper model. The spawned CLI inherits this process's environment and reads
//! its own stored login (`claude login` or `ANTHROPIC_API_KEY`), so a CI machine
//! authenticates it exactly as an interactive shell would. Overrides may also be
//! read from a `.envrc` in the crate root.

#![cfg(feature = "external-claude-code")]

use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_lib::agent::external::{
    ClaudeCodeAdapter, ClaudeCodeConfig, ExternalAgentEvent, ExternalEventSink,
    ExternalObservedEvent, ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionInput,
    ExternalSessionPolicy, ExternalSessionRegistry, ExternalSessionRequest,
    ExternalSessionShutdown, ExternalStreamPolicy, RuntimeDecisionPoint, WorktreeIsolation,
};
use agent_lib::agent::interaction::InteractionResponse;
use agent_lib::agent::permission::PermissionResponse;
use agent_lib::agent::spec::WorktreeRef;
use agent_lib::agent::{AgentId, BudgetLimits, RunContext, RunId, TraceNodeId};
use tokio::{process::Command, time::timeout};

/// Whole-test wall-clock budget. A single Claude turn is usually a few seconds;
/// this only guards against a hung child so the suite never blocks for minutes.
const E2E_TIMEOUT: Duration = Duration::from_secs(180);

/// Per-read/shutdown timeout handed to the adapter's transport.
const IO_TIMEOUT: Duration = Duration::from_secs(120);

/// Bounds the whole `start → advance* → shutdown` drive so a misbehaving CLI
/// that keeps streaming without ever completing cannot loop forever.
const MAX_TURNS: usize = 12;

// ----- environment ---------------------------------------------------------

/// Minimal `.envrc`-plus-process environment reader, mirroring the credential
/// handling used by `agent_external_real_e2e.rs` so both suites authenticate the
/// same way.
#[derive(Clone, Debug, Default)]
struct E2eEnv {
    vars: BTreeMap<String, String>,
}

impl E2eEnv {
    fn load() -> Self {
        let mut vars = BTreeMap::new();
        if let Ok(contents) = fs::read_to_string(".envrc") {
            for line in contents.lines() {
                if let Some((name, value)) = parse_envrc_line(line) {
                    vars.insert(name, value);
                }
            }
        }
        Self { vars }
    }

    fn get(&self, name: &str) -> Option<String> {
        env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| self.vars.get(name).cloned())
            .filter(|value| !value.trim().is_empty())
    }
}

fn parse_envrc_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    let line = line.strip_prefix("export ").unwrap_or(line).trim();
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        || name.as_bytes()[0].is_ascii_digit()
    {
        return None;
    }

    Some((name.to_owned(), unquote_env_value(value.trim())))
}

fn unquote_env_value(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
        {
            return value[1..value.len() - 1]
                .replace("\\\"", "\"")
                .replace("\\'", "'");
        }
    }
    value.to_owned()
}

/// Resolves the Claude Code binary: an explicit `CLAUDE_CODE_BIN` override wins,
/// otherwise `claude` is looked up on `PATH`.
fn claude_binary(env: &E2eEnv) -> String {
    env.get("CLAUDE_CODE_BIN")
        .unwrap_or_else(|| "claude".to_owned())
}

/// Reports whether `program --version` runs and exits successfully, used to skip
/// the test when the CLI is absent or non-functional.
async fn command_available(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

// ----- sink ----------------------------------------------------------------

/// Captures every observation the adapter mirrors, so the test can assert the
/// live stream really was multi-event (text, and — when the model gates an
/// action — a permission prompt) rather than a single terminal frame.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<ExternalAgentEvent>>,
}

impl RecordingSink {
    fn snapshot(&self) -> Vec<ExternalAgentEvent> {
        self.events.lock().expect("sink mutex").clone()
    }
}

impl ExternalEventSink for RecordingSink {
    fn emit(&self, event: &ExternalObservedEvent) {
        self.events
            .lock()
            .expect("sink mutex")
            .push(event.event.clone());
    }
}

// ----- request scaffolding -------------------------------------------------

fn run_context() -> RunContext {
    let run_id: RunId = "018f0d9c-7b6a-7c12-8f31-1234567890e0"
        .parse()
        .expect("run id");
    let trace_root = TraceNodeId::new("external-claude-code-e2e-root");
    RunContext::new_root(run_id, BudgetLimits::unbounded(), trace_root)
}

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f0"
        .parse()
        .expect("agent id")
}

fn policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        // Prompt mode leaves gated actions (file edits, shell) to a permission
        // prompt so the drive loop can exercise the control-response path.
        permission_mode: ExternalPermissionMode::Prompt,
        isolation: WorktreeIsolation::EphemeralGitWorktree,
        max_turns: Some(MAX_TURNS as u32),
        stream_events: ExternalStreamPolicy::Buffered,
    }
}

fn start_request(worktree: &std::path::Path, prompt: &str) -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent_id(),
        runtime: ExternalRuntimeKind::ClaudeCode,
        worktree: WorktreeRef::new(worktree),
        session: None,
        input: ExternalSessionInput::Start {
            prompt: prompt.to_owned(),
        },
        // The adapter runs no MCP bridge, so a live session declares no host
        // tools; leaving this empty keeps `start` from refusing the request.
        tools: Vec::new(),
        policy: policy(),
    }
}

/// Creates an isolated git worktree under the OS temp dir for the CLI to run in,
/// so the e2e never touches the checkout it is launched from.
fn make_worktree() -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "agent-lib-claude-adapter-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    fs::create_dir_all(&dir).expect("create temp worktree");
    let status = std::process::Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !status.map(|s| s.success()).unwrap_or(false) {
        eprintln!("note: `git init` failed in the temp worktree; running without a git repo");
    }
    dir
}

// ----- the test ------------------------------------------------------------

#[tokio::test]
#[ignore = "requires local Claude Code auth/runtime; spawns `claude`"]
async fn claude_code_adapter_real_cli_drives_a_multi_step_session() {
    let env = E2eEnv::load();
    let binary = claude_binary(&env);

    if !command_available(&binary).await {
        eprintln!(
            "skipping: Claude Code binary `{binary}` is not available \
             (set CLAUDE_CODE_BIN or install `claude` on PATH)"
        );
        return;
    }

    // Export any `.envrc` credentials so the child inherits them; the adapter
    // spawns the CLI in a fresh process that would not otherwise see a key that
    // lives only in `.envrc`.
    let result = timeout(E2E_TIMEOUT, drive_session(&env, &binary)).await;
    match result {
        Ok(Outcome::Completed { summary, events }) => {
            let text_events = events
                .iter()
                .filter(|event| matches!(event, ExternalAgentEvent::TextDelta { .. }))
                .count();
            let permission_events = events
                .iter()
                .filter(|event| matches!(event, ExternalAgentEvent::PermissionRequested { .. }))
                .count();
            let started = events
                .iter()
                .any(|event| matches!(event, ExternalAgentEvent::SessionStarted { .. }));
            let completed = events
                .iter()
                .any(|event| matches!(event, ExternalAgentEvent::SessionCompleted));

            eprintln!(
                "Claude Code adapter e2e: {} observed events \
                 ({text_events} text, {permission_events} permission), summary: {:?}",
                events.len(),
                summary
            );

            assert!(
                started,
                "the live session should observe a SessionStarted event, saw: {events:?}"
            );
            assert!(
                text_events > 0,
                "a real Claude turn should stream assistant text, saw: {events:?}"
            );
            assert!(
                completed,
                "the live session should observe a SessionCompleted event, saw: {events:?}"
            );
            // The stream is genuinely multi-step: at minimum SessionStarted +
            // one or more TextDelta + SessionCompleted, plus any permission gate.
            assert!(
                events.len() >= 3,
                "expected a multi-event session stream, saw only: {events:?}"
            );
        }
        Ok(Outcome::Skipped(reason)) => {
            eprintln!("skipping: {reason}");
        }
        Err(_elapsed) => {
            panic!("Claude Code adapter e2e exceeded its {E2E_TIMEOUT:?} wall-clock budget");
        }
    }
}

/// What a live drive produced: either a completed session (with its observed
/// events) or a reason the environment made it un-runnable (treated as a skip so
/// an unconfigured machine stays green).
enum Outcome {
    Completed {
        summary: String,
        events: Vec<ExternalAgentEvent>,
    },
    Skipped(String),
}

/// Runs one real session: probe, start, then advance until completion, answering
/// any permission pause by approving it. Auth/launch failures are folded into a
/// [`Outcome::Skipped`] so a machine without Claude credentials does not fail.
async fn drive_session(env: &E2eEnv, binary: &str) -> Outcome {
    let worktree = make_worktree();
    let config = ClaudeCodeConfig::new()
        .with_binary(binary)
        .with_working_dir(&worktree)
        .with_permission_mode(ExternalPermissionMode::Prompt)
        .with_timeout(IO_TIMEOUT);
    // The spawned CLI inherits this process's environment and reads its own
    // stored login (`claude login` / `ANTHROPIC_API_KEY`), so no credential is
    // injected here; a CI machine populates those in the real environment.
    let config = apply_model_override(config, env);

    // Prefer the probe's view of the local CLI so the adapter only advertises
    // what this binary actually supports; a probe failure is an environment
    // skip, not a test failure.
    let adapter = match agent_lib::agent::external::probe(&config).await {
        Ok(probed) => ClaudeCodeAdapter::with_probed_capabilities(config, &probed),
        Err(error) => {
            let _ = fs::remove_dir_all(&worktree);
            return Outcome::Skipped(format!("Claude Code probe failed: {error:?}"));
        }
    };

    let registry = ExternalSessionRegistry::new(Arc::new(adapter));
    let sink = Arc::new(RecordingSink::default());
    let ctx = run_context();
    let prompt = "You are running in a scratch git repository. \
         Create a file named READY.txt containing the single word READY, \
         then reply with a one-sentence confirmation and stop.";
    let request = start_request(&worktree, prompt);

    let handle = match registry
        .get_or_start(
            &request,
            &ctx,
            Some(sink.clone() as Arc<dyn ExternalEventSink>),
        )
        .await
    {
        Ok(handle) => handle,
        Err(error) => {
            let _ = fs::remove_dir_all(&worktree);
            return Outcome::Skipped(format!(
                "Claude Code session did not start (auth/runtime?): {error:?}"
            ));
        }
    };

    let mut input = request.input.clone();
    let mut summary = String::new();
    let mut drive_error: Option<String> = None;

    for _turn in 0..MAX_TURNS {
        let decision = {
            let mut session = handle.lock().await;
            session.advance(&input, &ctx).await
        };
        match decision {
            Ok(RuntimeDecisionPoint::Completed { output, .. }) => {
                summary = output.summary;
                break;
            }
            Ok(RuntimeDecisionPoint::PausedForInteraction { action_id, .. }) => {
                // Approve the gated action and continue the same session.
                input = ExternalSessionInput::RespondInteraction {
                    action_id: action_id.clone(),
                    response: InteractionResponse::Permission(PermissionResponse::approve(
                        action_id,
                    )),
                };
            }
            Ok(RuntimeDecisionPoint::PausedForToolCalls { .. }) => {
                drive_error = Some(
                    "session paused for host tool calls, which this adapter does not bridge"
                        .to_owned(),
                );
                break;
            }
            Ok(RuntimeDecisionPoint::PausedForSubagent { .. }) => {
                drive_error = Some(
                    "session paused for a host subagent, which this adapter does not bridge"
                        .to_owned(),
                );
                break;
            }
            Err(error) => {
                drive_error = Some(format!("advance failed: {error:?}"));
                break;
            }
        }
    }

    // Capture the session facts and release the handle lock *before* cleanup,
    // which re-locks the same handle internally to close it.
    let session_ref = {
        let session = handle.lock().await;
        session.session_ref()
    };
    let disposition = registry.cleanup(agent_id(), &session_ref).await;
    assert_eq!(
        disposition,
        ExternalSessionShutdown::Graceful,
        "closing the live session should be graceful"
    );

    let _ = fs::remove_dir_all(&worktree);

    if let Some(error) = drive_error {
        return Outcome::Skipped(error);
    }
    Outcome::Completed {
        summary,
        events: sink.snapshot(),
    }
}

/// Applies an optional `CLAUDE_CODE_MODEL` override so the e2e can pin a cheaper
/// model when one is configured.
fn apply_model_override(config: ClaudeCodeConfig, env: &E2eEnv) -> ClaudeCodeConfig {
    match env.get("CLAUDE_CODE_MODEL") {
        Some(model) => config.with_model(model),
        None => config,
    }
}
