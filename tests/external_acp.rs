//! Opt-in real-runtime coverage for the ACP *session adapter* (M10-3, feature
//! `external-acp`).
//!
//! Unlike the offline decoder cassette (`agent_acp_cassette.rs`), the adapter's
//! inline fake-transport unit tests, and the registry-backed drain
//! (`agent_acp_adapter_drain.rs`), this suite drives the real
//! [`AcpAdapter`](agent_lib::agent::external::AcpAdapter) against a locally
//! installed, authenticated ACP agent through an
//! [`ExternalSessionRegistry`](agent_lib::agent::external::ExternalSessionRegistry).
//! It proves the whole live path end to end: spawn + `initialize` handshake →
//! `start` a turn → stream observations → auto-approve any permission pause →
//! settle on completion → graceful shutdown.
//!
//! It is intentionally `#[ignore]`: it spawns a real coding-agent CLI and may
//! call a paid model, so it is never part of the default offline suite. It also
//! skips itself (with a clear, non-secret message, exiting green) when no ACP
//! agent binary or its login is available, so an unconfigured machine does not
//! report a spurious failure.
//!
//! Run it explicitly:
//!
//! ```text
//! cargo test --features external-acp --test external_acp -- --ignored --nocapture
//! ```
//!
//! # Agent discovery
//!
//! The ACP agent is discovered from `ACP_AGENT_BIN` (an absolute-path or on-PATH
//! override) plus optional whitespace-separated `ACP_AGENT_ARGS`; failing that,
//! the first available of `opencode acp`, `claude-agent-acp`, or `codex-acp` on
//! `PATH` is used. The spawned agent inherits this process's environment and
//! reads its **own** stored login — this crate injects **no** provider API key.
//!
//! # Pointing an agent at non-default config
//!
//! Because the three reference agents commonly run under non-default config, the
//! following test-only overrides map onto [`AcpConfig`] env/args (never logged):
//!
//! - `ACP_CODEX_HOME` → child env `CODEX_HOME` (Codex config/login dir).
//! - `ACP_OPENCODE_CONFIG` → child env `OPENCODE_CONFIG` (OpenCode config file).
//! - `ACP_CLAUDE_SETTINGS` → appended `--settings <file>` launch arg (Claude).
//!
//! Unset overrides fall back to inherited defaults. Overrides may also be read
//! from a `.envrc` in the crate root.

#![cfg(feature = "external-acp")]

use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_lib::agent::external::{
    AcpAdapter, AcpConfig, ExternalAgentEvent, ExternalEventSink, ExternalObservedEvent,
    ExternalPermissionMode, ExternalSessionInput, ExternalSessionPolicy, ExternalSessionRegistry,
    ExternalSessionRequest, ExternalSessionShutdown, ExternalStreamPolicy, RuntimeDecisionPoint,
    WorktreeIsolation, acp_runtime_kind,
};
use agent_lib::agent::interaction::InteractionResponse;
use agent_lib::agent::permission::PermissionResponse;
use agent_lib::agent::spec::WorktreeRef;
use agent_lib::agent::{AgentId, BudgetLimits, RunContext, RunId, TraceNodeId};
use tokio::{process::Command, time::timeout};

/// Whole-test wall-clock budget. A single ACP turn can take a while (model
/// latency plus tool execution); this only guards against a hung child so the
/// suite never blocks indefinitely.
const E2E_TIMEOUT: Duration = Duration::from_secs(300);

/// Per-read/shutdown timeout handed to the adapter's transport. A long, quiet
/// stretch during a tool run must not trip the inter-frame read timeout.
const IO_TIMEOUT: Duration = Duration::from_secs(240);

/// Bounds the whole `start → advance* → shutdown` drive so a misbehaving agent
/// that keeps streaming (or re-prompting for permission) cannot loop forever.
const MAX_TURNS: usize = 12;

// ----- environment ---------------------------------------------------------

/// Minimal `.envrc`-plus-process environment reader, mirroring the credential
/// handling used by the CLI adapter e2e suites so every test authenticates the
/// same way. It never prints a value, so a secret read from `.envrc` cannot leak
/// into test output.
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

/// Reports whether `program --version` runs and exits successfully, used to
/// discover an ACP agent binary on `PATH`.
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

/// A discovered ACP agent: its base [`AcpConfig`] and a human label for logs.
struct DiscoveredAgent {
    label: String,
    config: AcpConfig,
}

/// Resolves an ACP agent to drive: an explicit `ACP_AGENT_BIN` (+ optional
/// `ACP_AGENT_ARGS`) wins; otherwise the first available reference agent on
/// `PATH` is used. Returns `None` (a skip) when nothing is available.
async fn discover_agent(env: &E2eEnv) -> Option<DiscoveredAgent> {
    if let Some(binary) = env.get("ACP_AGENT_BIN") {
        if command_available(&binary).await {
            let args: Vec<String> = env
                .get("ACP_AGENT_ARGS")
                .map(|raw| raw.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default();
            let config = AcpConfig::new(binary.clone(), args);
            return Some(DiscoveredAgent {
                label: format!("ACP_AGENT_BIN={binary}"),
                config,
            });
        }
        eprintln!("note: ACP_AGENT_BIN=`{binary}` is not runnable; trying PATH presets");
    }

    // OpenCode speaks ACP itself; the two Zed bridges wrap Claude Code / Codex.
    if command_available("opencode").await {
        return Some(DiscoveredAgent {
            label: "opencode acp".to_owned(),
            config: AcpConfig::opencode_acp(),
        });
    }
    if command_available("claude-agent-acp").await {
        return Some(DiscoveredAgent {
            label: "claude-agent-acp".to_owned(),
            config: AcpConfig::claude_agent_acp(),
        });
    }
    if command_available("codex-acp").await {
        return Some(DiscoveredAgent {
            label: "codex-acp".to_owned(),
            config: AcpConfig::codex_acp(),
        });
    }
    None
}

/// Layers the test-only config-directory overrides onto a discovered agent's
/// config, mapping each to [`AcpConfig`] env/args. These point a wrapped CLI at
/// its real (non-default) config/login; none is a provider API key.
fn apply_config_overrides(mut config: AcpConfig, env: &E2eEnv) -> AcpConfig {
    if let Some(codex_home) = env.get("ACP_CODEX_HOME") {
        config = config.with_env("CODEX_HOME", codex_home);
    }
    if let Some(opencode_config) = env.get("ACP_OPENCODE_CONFIG") {
        config = config.with_env("OPENCODE_CONFIG", opencode_config);
    }
    if let Some(claude_settings) = env.get("ACP_CLAUDE_SETTINGS") {
        config = config.with_arg("--settings").with_arg(claude_settings);
    }
    config
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
    let trace_root = TraceNodeId::new("external-acp-e2e-root");
    RunContext::new_root(run_id, BudgetLimits::unbounded(), trace_root)
}

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890f0"
        .parse()
        .expect("agent id")
}

fn policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        // Prompt so a gated write surfaces as a host interaction the drive loop
        // auto-approves, exercising the permission bridge end to end.
        permission_mode: ExternalPermissionMode::Prompt,
        // The test owns the throwaway worktree it launches the CLI in
        // (created by `make_worktree`), so the registry must not prepare a
        // second one: Shared passes the base through unchanged (M2-7).
        isolation: WorktreeIsolation::Shared,
        max_turns: Some(MAX_TURNS as u32),
        stream_events: ExternalStreamPolicy::Buffered,
    }
}

fn start_request(worktree: &std::path::Path, prompt: &str) -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent_id(),
        runtime: acp_runtime_kind(),
        worktree: WorktreeRef::new(worktree),
        session_dir: None,
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

/// Creates an isolated git worktree under the OS temp dir for the agent to run
/// in, so the e2e never touches the checkout it is launched from.
fn make_worktree() -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "agent-lib-acp-adapter-e2e-{}-{}",
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
#[ignore = "requires a local ACP agent + its login; spawns an ACP agent CLI"]
async fn acp_adapter_real_cli_drives_a_session() {
    let env = E2eEnv::load();

    let Some(agent) = discover_agent(&env).await else {
        eprintln!(
            "skipping: no ACP agent available (set ACP_AGENT_BIN, or install \
             `opencode` / `claude-agent-acp` / `codex-acp` on PATH)"
        );
        return;
    };
    let label = agent.label.clone();

    let result = timeout(E2E_TIMEOUT, drive_session(&env, agent)).await;
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
                "ACP adapter e2e ({label}): {} observed events ({text_events} text), summary: {:?}",
                events.len(),
                summary
            );

            assert!(
                started,
                "the live session should observe a SessionStarted event, saw: {events:?}"
            );
            assert!(
                text_events > 0,
                "a real ACP turn should stream at least one assistant message, saw: {events:?}"
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
            eprintln!("skipping ({label}): {reason}");
        }
        Err(_elapsed) => {
            panic!("ACP adapter e2e ({label}) exceeded its {E2E_TIMEOUT:?} wall-clock budget");
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

/// Runs one real session: spawn + initialize, start, then advance until
/// completion (auto-approving any permission pause). Auth / launch failures are
/// folded into a [`Outcome::Skipped`] so a machine without a logged-in agent does
/// not fail.
async fn drive_session(env: &E2eEnv, agent: DiscoveredAgent) -> Outcome {
    let worktree = make_worktree();
    let config = apply_config_overrides(
        agent
            .config
            .with_working_dir(&worktree)
            .with_permission_mode(ExternalPermissionMode::Prompt)
            .with_timeout(IO_TIMEOUT),
        env,
    );

    // The adapter's `new` reports its implemented capabilities; the live
    // `initialize` handshake inside `start` negotiates the agent's actual set
    // (for example `loadSession`). No separate probe binary exists for ACP.
    let adapter = AcpAdapter::new(config);
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
                "ACP session did not start (auth/runtime?): {error:?}"
            ));
        }
    };

    let mut input = request.input.clone();
    let mut summary = String::new();
    let mut drive_error: Option<String> = None;

    // Advance one decision at a time. A `session/request_permission` pause is
    // auto-approved and the drive continues; a completion ends the loop. The
    // turn budget guards against an agent that never settles.
    for _ in 0..MAX_TURNS {
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
                // Auto-approve the gated action and feed the approval back in.
                let response =
                    InteractionResponse::Permission(PermissionResponse::approve(action_id.clone()));
                input = ExternalSessionInput::RespondInteraction {
                    action_id,
                    response,
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

    // Worktree isolation (design §16): the agent is confined to the working-dir
    // worktree and must never touch the checkout the test is launched from. When
    // the turn completed, verify the artifact landed inside the worktree and that
    // no stray `READY.txt` leaked into the current working directory (the repo
    // root). Any leak is scrubbed before asserting so a regression cannot leave
    // the checkout dirty for the next run.
    if drive_error.is_none() {
        let created_in_worktree = worktree.join("READY.txt").is_file();
        let cwd = env::current_dir().expect("resolve current dir");
        let leaked_path = cwd.join("READY.txt");
        let leaked_to_cwd = leaked_path.is_file();
        if leaked_to_cwd {
            let _ = fs::remove_file(&leaked_path);
        }
        let _ = fs::remove_dir_all(&worktree);
        assert!(
            !leaked_to_cwd,
            "the ACP agent leaked READY.txt into the launching checkout ({}) instead of \
             confining writes to the working-dir worktree",
            leaked_path.display()
        );
        assert!(
            created_in_worktree,
            "the ACP agent did not create READY.txt inside the scratch worktree ({})",
            worktree.display()
        );
        return Outcome::Completed {
            summary,
            events: sink.snapshot(),
        };
    }

    let _ = fs::remove_dir_all(&worktree);

    if let Some(error) = drive_error {
        return Outcome::Skipped(error);
    }
    Outcome::Completed {
        summary,
        events: sink.snapshot(),
    }
}
