//! Opt-in real-runtime coverage for the Codex *session adapter* (M7-3, feature
//! `external-codex`).
//!
//! Unlike the offline decoder cassette (`agent_codex_cassette.rs`) and the
//! adapter's inline fake-launcher unit tests, this suite drives the real
//! [`CodexAdapter`](agent_lib::agent::external::CodexAdapter) against a locally
//! installed, authenticated `codex` CLI through an
//! [`ExternalSessionRegistry`](agent_lib::agent::external::ExternalSessionRegistry).
//! It proves the whole live path end to end: probe → `start` → stream a turn of
//! observations → settle on completion → graceful shutdown.
//!
//! It is intentionally `#[ignore]`: it spawns a real coding-agent CLI and may
//! call a paid model, so it is never part of the default offline suite. It also
//! skips itself (with a clear message, exiting green) when the binary or its auth
//! is missing, so an unconfigured machine does not report a spurious failure.
//!
//! Run it explicitly:
//!
//! ```text
//! cargo test --features external-codex --test external_codex -- --ignored --nocapture
//! ```
//!
//! The binary is discovered from `CODEX_BIN` (an absolute path override) or,
//! failing that, `codex` on `PATH`; an optional `CODEX_MODEL` pins a model. The
//! spawned CLI inherits this process's environment and reads its own stored login
//! (`codex login` / `~/.codex/auth.json` / `OPENAI_API_KEY`), so a CI machine
//! authenticates it exactly as an interactive shell would. Overrides may also be
//! read from a `.envrc` in the crate root.
//!
//! # Why `AcceptEdits`
//!
//! `codex exec --json` runs **autonomously**: it resolves approvals against the
//! sandbox/approval policy the host pre-set on the command line and never hands a
//! gated action back to the host mid-turn (M7-2). Running under
//! [`AcceptEdits`](agent_lib::agent::external::ExternalPermissionMode::AcceptEdits)
//! (`workspace-write`) therefore lets the CLI create a file inside its scratch
//! worktree and finish the turn without a host approval this adapter could not
//! answer.

#![cfg(feature = "external-codex")]

use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_lib::agent::external::{
    CodexAdapter, CodexConfig, ExternalAgentEvent, ExternalEventSink, ExternalObservedEvent,
    ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionInput, ExternalSessionPolicy,
    ExternalSessionRegistry, ExternalSessionRequest, ExternalSessionShutdown, ExternalStreamPolicy,
    RuntimeDecisionPoint, WorktreeIsolation,
};
use agent_lib::agent::spec::WorktreeRef;
use agent_lib::agent::{AgentId, BudgetLimits, RunContext, RunId, TraceNodeId};
use tokio::{process::Command, time::timeout};

/// Whole-test wall-clock budget. A single Codex turn can take a while (model
/// latency plus tool execution); this only guards against a hung child so the
/// suite never blocks indefinitely.
const E2E_TIMEOUT: Duration = Duration::from_secs(300);

/// Per-read/shutdown timeout handed to the adapter's transport. A long, quiet
/// stretch during a tool run must not trip the inter-frame read timeout.
const IO_TIMEOUT: Duration = Duration::from_secs(240);

/// Bounds the whole `start → advance* → shutdown` drive so a misbehaving CLI that
/// keeps streaming without ever completing cannot loop forever.
const MAX_TURNS: usize = 8;

// ----- environment ---------------------------------------------------------

/// Minimal `.envrc`-plus-process environment reader, mirroring the credential
/// handling used by the Claude Code adapter e2e so both suites authenticate the
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

/// Resolves the Codex binary: an explicit `CODEX_BIN` override wins, otherwise
/// `codex` is looked up on `PATH`.
fn codex_binary(env: &E2eEnv) -> String {
    env.get("CODEX_BIN").unwrap_or_else(|| "codex".to_owned())
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
/// live stream really was multi-event (session start, assistant text, session
/// completion) rather than a single terminal frame.
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
    let trace_root = TraceNodeId::new("external-codex-e2e-root");
    RunContext::new_root(run_id, BudgetLimits::unbounded(), trace_root)
}

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f0"
        .parse()
        .expect("agent id")
}

fn policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        // AcceptEdits maps to `on-request` + `workspace-write`, letting the
        // autonomous CLI write inside its scratch worktree without a host
        // approval this adapter cannot answer.
        permission_mode: ExternalPermissionMode::AcceptEdits,
        isolation: WorktreeIsolation::EphemeralGitWorktree,
        max_turns: Some(MAX_TURNS as u32),
        stream_events: ExternalStreamPolicy::Buffered,
    }
}

fn start_request(worktree: &std::path::Path, prompt: &str) -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent_id(),
        runtime: ExternalRuntimeKind::Codex,
        worktree: WorktreeRef::new(worktree),
        session: None,
        input: ExternalSessionInput::Start {
            prompt: prompt.to_owned(),
        },
        // The adapter bridges no host tools, so a live session declares none;
        // leaving this empty keeps `start` from refusing the request.
        tools: Vec::new(),
        policy: policy(),
    }
}

/// Creates an isolated git worktree under the OS temp dir for the CLI to run in,
/// so the e2e never touches the checkout it is launched from.
fn make_worktree() -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "agent-lib-codex-adapter-e2e-{}-{}",
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
#[ignore = "requires local Codex auth/runtime; spawns `codex`"]
async fn codex_adapter_real_cli_drives_a_session() {
    let env = E2eEnv::load();
    let binary = codex_binary(&env);

    if !command_available(&binary).await {
        eprintln!(
            "skipping: Codex binary `{binary}` is not available \
             (set CODEX_BIN or install `codex` on PATH)"
        );
        return;
    }

    let result = timeout(E2E_TIMEOUT, drive_session(&env, &binary)).await;
    match result {
        Ok(Outcome::Completed { summary, events }) => {
            let text_events = events
                .iter()
                .filter(|event| matches!(event, ExternalAgentEvent::TextDelta { .. }))
                .count();
            let started = events
                .iter()
                .any(|event| matches!(event, ExternalAgentEvent::SessionStarted { .. }));
            let completed = events
                .iter()
                .any(|event| matches!(event, ExternalAgentEvent::SessionCompleted));

            eprintln!(
                "Codex adapter e2e: {} observed events ({text_events} text), summary: {:?}",
                events.len(),
                summary
            );

            assert!(
                started,
                "the live session should observe a SessionStarted event, saw: {events:?}"
            );
            assert!(
                text_events > 0,
                "a real Codex turn should stream at least one assistant message, saw: {events:?}"
            );
            assert!(
                completed,
                "the live session should observe a SessionCompleted event, saw: {events:?}"
            );
            // The stream is genuinely multi-step: at minimum SessionStarted +
            // one or more TextDelta + SessionCompleted.
            assert!(
                events.len() >= 3,
                "expected a multi-event session stream, saw only: {events:?}"
            );
        }
        Ok(Outcome::Skipped(reason)) => {
            eprintln!("skipping: {reason}");
        }
        Err(_elapsed) => {
            panic!("Codex adapter e2e exceeded its {E2E_TIMEOUT:?} wall-clock budget");
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

/// Runs one real session: probe, start, then advance until completion. Auth /
/// launch failures are folded into a [`Outcome::Skipped`] so a machine without
/// Codex credentials does not fail.
async fn drive_session(env: &E2eEnv, binary: &str) -> Outcome {
    let worktree = make_worktree();
    let config = CodexConfig::new()
        .with_binary(binary)
        .with_working_dir(&worktree)
        .with_permission_mode(ExternalPermissionMode::AcceptEdits)
        .with_timeout(IO_TIMEOUT);
    // The spawned CLI inherits this process's environment and reads its own
    // stored login (`codex login` / `~/.codex/auth.json` / `OPENAI_API_KEY`), so
    // no credential is injected here; a CI machine populates those in the real
    // environment.
    let config = apply_model_override(config, env);

    // Prefer the probe's view of the local CLI so the adapter only advertises
    // what this binary actually supports; a probe failure is an environment
    // skip, not a test failure.
    let adapter = match agent_lib::agent::external::codex_probe(&config).await {
        Ok(probed) => CodexAdapter::with_probed_capabilities(config, &probed),
        Err(error) => {
            let _ = fs::remove_dir_all(&worktree);
            return Outcome::Skipped(format!("Codex probe failed: {error:?}"));
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
                "Codex session did not start (auth/runtime?): {error:?}"
            ));
        }
    };

    let input = request.input.clone();
    let mut summary = String::new();
    let mut drive_error: Option<String> = None;

    // `codex exec` runs a whole turn autonomously and settles in a single
    // advance (it never pauses for the host, M7-2), so one advance drives the
    // session to completion — no continuation loop is needed.
    let decision = {
        let mut session = handle.lock().await;
        session.advance(&input, &ctx).await
    };
    match decision {
        Ok(RuntimeDecisionPoint::Completed { output, .. }) => {
            summary = output.summary;
        }
        // Codex exec runs autonomously and never pauses for the host; a pause
        // would mean the decoder saw something this adapter cannot serve, so
        // treat it as an environment skip rather than a hard failure.
        Ok(RuntimeDecisionPoint::PausedForInteraction { .. }) => {
            drive_error =
                Some("session paused for an interaction, unexpected for codex exec".to_owned());
        }
        Ok(RuntimeDecisionPoint::PausedForToolCalls { .. }) => {
            drive_error = Some(
                "session paused for host tool calls, which this adapter does not bridge".to_owned(),
            );
        }
        Ok(RuntimeDecisionPoint::PausedForSubagent { .. }) => {
            drive_error = Some(
                "session paused for a host subagent, which this adapter does not bridge".to_owned(),
            );
        }
        Err(error) => {
            drive_error = Some(format!("advance failed: {error:?}"));
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

/// Applies an optional `CODEX_MODEL` override so the e2e can pin a specific model
/// when one is configured.
fn apply_model_override(config: CodexConfig, env: &E2eEnv) -> CodexConfig {
    match env.get("CODEX_MODEL") {
        Some(model) => config.with_model(model),
        None => config,
    }
}
