//! Shared wiring for the managed-external examples
//! (`managed_claude_code`, `managed_codex`, `managed_opencode`, `managed_mixed`).
//!
//! Every example drives a coding-agent runtime the *managed* way: through an
//! [`ExternalAgentMachine`] and a scoped [`ExternalSessionHandler`], never by
//! calling the runtime adapter directly. The wiring mirrors the milestone-9 real
//! e2e (`tests/agent_external_managed_real_e2e.rs`) so the examples stay a
//! faithful, copy-pasteable template:
//!
//! 1. Build a runtime [`config`](agent_lib::agent::external) and run its
//!    **capability probe**. A missing binary or a probe that reports an
//!    unsupported capability turns the run into a **skip**, not a crash — this is
//!    the unsupported-capability fallback the design calls for.
//! 2. Wrap the probed adapter in an [`ExternalSessionRegistry`] and expose it
//!    through a registry-backed [`ExternalSessionHandler`] ([`RegistryHandler`])
//!    that `get_or_start`s a live session, advances it one
//!    [`RuntimeDecisionPoint`], and mirrors observations to a live sink.
//! 3. Drive an [`ExternalAgentMachine`] with a [`TestScope`] that serves the
//!    external family plus an approve-all interaction handler, so a runtime
//!    permission prompt is bridged back into the machine and approved in place.
//! 4. Run each child inside a throwaway `git init` worktree under the OS temp
//!    dir (worktree isolation) and force-close the live session + delete the
//!    temp dir when the drive finishes.
//!
//! Credentials are never printed: the helpers only read binary/model overrides
//! from the environment and never echo their values (secret redaction).
//!
//! This module is included directly by each managed example with
//! `#[path = "support/managed.rs"] mod managed;`; it is not part of the
//! `support` module used by the non-managed examples.

#![allow(dead_code)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_lib::agent::external::{
    ExternalEventSink, ExternalSessionRegistry, RuntimeDecisionPoint,
};
use agent_lib::agent::{
    AgentId, ExternalAgentEvent, ExternalAgentMachine, ExternalAgentSpec, ExternalObservedEvent,
    ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionHandler, ExternalSessionPolicy,
    ExternalSessionRequest, ExternalSessionResult, ExternalStreamPolicy, LoopCursorKind,
    RequirementResult, RunContext, ToolSetRef, WorktreeIsolation, WorktreeRef, drain,
};
use agent_lib::conversation::{Conversation, ConversationConfig};
use agent_testkit::prelude::{
    ScriptedInteractionHandler, SeqIds, TestScope, root_context, user_input,
};
use async_trait::async_trait;

/// Per-read/shutdown timeout handed to each managed adapter's transport.
const IO_TIMEOUT: Duration = Duration::from_secs(120);

/// Upper bound on a single managed child's `start → advance* → done` drive.
const MAX_CHILD_TURNS: u32 = 12;

// ----- live-session bookkeeping --------------------------------------------

/// A live sink that records every [`ExternalAgentEvent`] the managed adapter
/// mirrors while decoding the runtime stream, so an example can report how many
/// stream events (and permission prompts) it observed.
#[derive(Default)]
pub struct CountingSink {
    events: Mutex<Vec<ExternalAgentEvent>>,
}

impl CountingSink {
    /// Number of streamed events mirrored to the sink so far.
    pub fn event_count(&self) -> usize {
        self.events.lock().expect("sink mutex").len()
    }

    /// Number of runtime permission prompts bridged through the sink.
    pub fn permission_prompts(&self) -> usize {
        self.events
            .lock()
            .expect("sink mutex")
            .iter()
            .filter(|event| matches!(event, ExternalAgentEvent::PermissionRequested { .. }))
            .count()
    }
}

impl ExternalEventSink for CountingSink {
    fn emit(&self, event: &ExternalObservedEvent) {
        self.events
            .lock()
            .expect("sink mutex")
            .push(event.event.clone());
    }
}

/// Running totals the registry-backed handler folds out of each decision point:
/// how many observations were replayed and the runtime's final summary.
#[derive(Default)]
pub struct ObservationLog {
    count: Mutex<usize>,
    summary: Mutex<Option<String>>,
}

impl ObservationLog {
    fn add(&self, count: usize) {
        *self.count.lock().expect("observation counter") += count;
    }

    fn record_summary(&self, summary: String) {
        *self.summary.lock().expect("summary slot") = Some(summary);
    }

    /// Total observations replayed across every advance.
    pub fn count(&self) -> usize {
        *self.count.lock().expect("observation counter")
    }

    /// The runtime's completed summary, if it finished.
    pub fn summary(&self) -> Option<String> {
        self.summary.lock().expect("summary slot").clone()
    }
}

// ----- registry-backed managed external session handler --------------------

/// A production-shaped [`ExternalSessionHandler`] that holds no machine state: it
/// resolves a live handle through its [`ExternalSessionRegistry`]
/// (`get_or_start` on the first `Start`, reattach on every follow-up), advances
/// it exactly one [`RuntimeDecisionPoint`], mirrors observations to the live
/// sink, and folds the outcome into an [`ExternalSessionResult`]. This is the
/// same composition the scripted/cassette handlers use, but over a *real* probed
/// adapter.
pub struct RegistryHandler {
    registry: Arc<ExternalSessionRegistry>,
    sink: Arc<CountingSink>,
    log: Arc<ObservationLog>,
}

impl RegistryHandler {
    fn registry(&self) -> &Arc<ExternalSessionRegistry> {
        &self.registry
    }

    async fn advance(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> ExternalSessionResult {
        let sink: Arc<dyn ExternalEventSink> = Arc::clone(&self.sink) as Arc<dyn ExternalEventSink>;
        let handle = match self.registry.get_or_start(request, ctx, Some(sink)).await {
            Ok(handle) => handle,
            Err(error) => return Err::<RuntimeDecisionPoint, _>(error).into(),
        };
        let point = {
            let mut session = handle.lock().await;
            session.advance(&request.input, ctx).await
        };
        if let Ok(decision) = &point {
            self.log.add(decision.observations().len());
            if let RuntimeDecisionPoint::Completed { output, .. } = decision {
                self.log.record_summary(output.summary.clone());
            }
        }
        point.into()
    }
}

#[async_trait]
impl ExternalSessionHandler for RegistryHandler {
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::ExternalSession(Box::new(self.advance(request, ctx).await))
    }
}

// ----- runtime adapter construction ----------------------------------------

/// Builds a probed [`ExternalSessionRegistry`] for `runtime` rooted at
/// `worktree`, or a non-secret skip reason when the CLI is missing or its
/// capability probe fails (treated as an auth/runtime signal, not a failure).
///
/// Each arm is gated on the runtime's feature flag; a runtime whose feature is
/// off falls through to the catch-all and reports how to enable it.
async fn build_registry(
    runtime: &ExternalRuntimeKind,
    worktree: &Path,
) -> Result<ExternalSessionRegistry, String> {
    match runtime {
        #[cfg(feature = "external-claude-code")]
        ExternalRuntimeKind::ClaudeCode => {
            use agent_lib::agent::external::{ClaudeCodeAdapter, ClaudeCodeConfig, probe};
            let mut config = ClaudeCodeConfig::new()
                .with_working_dir(worktree)
                // Prompt mode leaves gated actions (file edits, shell) to a
                // permission prompt so the child exercises the managed
                // interaction bridge rather than silently auto-approving.
                .with_permission_mode(ExternalPermissionMode::Prompt)
                .with_timeout(IO_TIMEOUT)
                .with_binary(binary_for(runtime));
            if let Some(model) = model_for(runtime) {
                config = config.with_model(model);
            }
            let probed = probe(&config)
                .await
                .map_err(|error| format!("Claude Code probe failed: {error:?}"))?;
            let adapter = ClaudeCodeAdapter::with_probed_capabilities(config, &probed);
            Ok(ExternalSessionRegistry::new(Arc::new(adapter)))
        }
        #[cfg(feature = "external-codex")]
        ExternalRuntimeKind::Codex => {
            use agent_lib::agent::external::{CodexAdapter, CodexConfig, codex_probe};
            let mut config = CodexConfig::new()
                .with_working_dir(worktree)
                .with_permission_mode(ExternalPermissionMode::Prompt)
                .with_timeout(IO_TIMEOUT)
                .with_binary(binary_for(runtime));
            if let Some(model) = model_for(runtime) {
                config = config.with_model(model);
            }
            let probed = codex_probe(&config)
                .await
                .map_err(|error| format!("Codex probe failed: {error:?}"))?;
            let adapter = CodexAdapter::with_probed_capabilities(config, &probed);
            Ok(ExternalSessionRegistry::new(Arc::new(adapter)))
        }
        #[cfg(feature = "external-opencode")]
        ExternalRuntimeKind::OpenCode => {
            use agent_lib::agent::external::{OpenCodeAdapter, OpenCodeConfig, opencode_probe};
            let mut config = OpenCodeConfig::new()
                .with_working_dir(worktree)
                .with_permission_mode(ExternalPermissionMode::Prompt)
                .with_timeout(IO_TIMEOUT)
                .with_binary(binary_for(runtime));
            if let Some(model) = model_for(runtime) {
                config = config.with_model(model);
            }
            let probed = opencode_probe(&config)
                .await
                .map_err(|error| format!("OpenCode probe failed: {error:?}"))?;
            let adapter = OpenCodeAdapter::with_probed_capabilities(config, &probed);
            Ok(ExternalSessionRegistry::new(Arc::new(adapter)))
        }
        other => Err(format!(
            "runtime {other:?} is not enabled in this build; rebuild with the matching \
             `external-*` feature flag"
        )),
    }
}

/// Composes a probed registry with a fresh sink/log into a [`RegistryHandler`].
async fn build_handler(
    runtime: &ExternalRuntimeKind,
    worktree: &Path,
    sink: Arc<CountingSink>,
    log: Arc<ObservationLog>,
) -> Result<RegistryHandler, String> {
    let registry = build_registry(runtime, worktree).await?;
    Ok(RegistryHandler {
        registry: Arc::new(registry),
        sink,
        log,
    })
}

// ----- machine fixtures ----------------------------------------------------

fn child_policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        // Prompt so a managed child surfaces gated actions as an interaction.
        permission_mode: ExternalPermissionMode::Prompt,
        // The example owns the throwaway worktree; the machine must not create a
        // second one.
        isolation: WorktreeIsolation::Shared,
        max_turns: Some(MAX_CHILD_TURNS),
        stream_events: ExternalStreamPolicy::Buffered,
    }
}

fn child_machine(
    ids: &SeqIds,
    runtime: ExternalRuntimeKind,
    agent_id: AgentId,
    worktree: &Path,
) -> ExternalAgentMachine {
    let spec = ExternalAgentSpec::new(
        agent_id,
        runtime,
        WorktreeRef::new(worktree),
        None,
        ToolSetRef::new(ids.tool_set_id(), Vec::new()),
        child_policy(),
    );
    let state = agent_lib::agent::ExternalAgentState::new(
        spec,
        Conversation::new(
            ids.conversation_id(),
            ConversationConfig::new(Some("Managed external example conversation.".to_owned())),
        ),
    );
    ExternalAgentMachine::new(state, Arc::new(ids.clone()))
}

fn child_prompt(runtime: &ExternalRuntimeKind, brief: &str) -> String {
    format!(
        "You are an agent-lib ExternalAgentMachine managed child backed by {runtime:?}.\n\
         Stay inside the current working directory and keep your actions minimal.\n\
         Task: {brief}\n\
         Reply with a single short paragraph confirming what you did."
    )
}

// ----- environment / process helpers ---------------------------------------

/// Resolves the binary name/path for a runtime, honouring its `*_BIN` override.
/// The override value is used but never printed.
fn binary_for(runtime: &ExternalRuntimeKind) -> String {
    let (var, default) = match runtime {
        ExternalRuntimeKind::ClaudeCode => ("CLAUDE_CODE_BIN", "claude"),
        ExternalRuntimeKind::Codex => ("CODEX_BIN", "codex"),
        ExternalRuntimeKind::OpenCode => ("OPENCODE_BIN", "opencode"),
        ExternalRuntimeKind::Custom(name) => return name.clone(),
    };
    env::var(var)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// Resolves the optional model override for a runtime; never printed.
fn model_for(runtime: &ExternalRuntimeKind) -> Option<String> {
    let var = match runtime {
        ExternalRuntimeKind::ClaudeCode => "CLAUDE_CODE_MODEL",
        ExternalRuntimeKind::Codex => "CODEX_MODEL",
        ExternalRuntimeKind::OpenCode => "OPENCODE_MODEL",
        ExternalRuntimeKind::Custom(_) => return None,
    };
    env::var(var).ok().filter(|value| !value.is_empty())
}

/// A short, filesystem-safe label for a runtime.
fn runtime_label(runtime: &ExternalRuntimeKind) -> &str {
    match runtime {
        ExternalRuntimeKind::ClaudeCode => "claude-code",
        ExternalRuntimeKind::Codex => "codex",
        ExternalRuntimeKind::OpenCode => "opencode",
        ExternalRuntimeKind::Custom(name) => name,
    }
}

/// Returns `true` when `program --version` runs successfully, used to skip a run
/// whose CLI is not installed before any probe attempt.
async fn command_available(program: &str) -> bool {
    tokio::process::Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// Creates an isolated `git init` worktree under the OS temp dir so a child that
/// writes files never touches the checkout it was launched from.
fn make_worktree(label: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "agent-lib-managed-example-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).expect("create temp worktree");
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

fn cleanup_worktree(worktree: &Path) {
    let _ = std::fs::remove_dir_all(worktree);
}

// ----- public drive surface ------------------------------------------------

/// What a completed managed drive observed.
pub struct ManagedOutcome {
    pub runtime: ExternalRuntimeKind,
    /// `true` when the machine drained all the way to `Done`.
    pub done: bool,
    /// The runtime's final summary, if it finished a turn.
    pub summary: Option<String>,
    /// Observations replayed into the machine across the drive.
    pub observations: usize,
    /// Stream events mirrored to the live sink.
    pub streamed_events: usize,
    /// Runtime permission prompts bridged into the interaction path.
    pub permission_prompts: usize,
}

/// Result of trying to drive one managed child.
pub enum DriveResult {
    /// The runtime ran and produced an outcome.
    Ran(ManagedOutcome),
    /// The runtime was unavailable/un-probeable; carries a non-secret reason.
    Skipped(String),
}

/// Drives one managed child [`ExternalAgentMachine`] through a single turn:
/// build the probed adapter, wire it behind a scoped [`ExternalSessionHandler`],
/// drain the machine, then force-close the session and delete the worktree.
///
/// Returns [`DriveResult::Skipped`] (never panics) when the CLI is missing or
/// its capability probe fails, so an unconfigured machine stays green.
pub async fn drive_managed_child(runtime: ExternalRuntimeKind, brief: &str) -> DriveResult {
    let binary = binary_for(&runtime);
    if !command_available(&binary).await {
        return DriveResult::Skipped(format!(
            "`{binary}` is not available (install it or set the matching `*_BIN` override)"
        ));
    }

    let label = runtime_label(&runtime).to_owned();
    let worktree = make_worktree(&label);
    let sink = Arc::new(CountingSink::default());
    let log = Arc::new(ObservationLog::default());

    let handler =
        match build_handler(&runtime, &worktree, Arc::clone(&sink), Arc::clone(&log)).await {
            Ok(handler) => handler,
            Err(reason) => {
                cleanup_worktree(&worktree);
                return DriveResult::Skipped(reason);
            }
        };
    let registry = Arc::clone(handler.registry());

    let ids = SeqIds::new();
    let agent_id = ids.agent_id();
    let mut machine = child_machine(&ids, runtime.clone(), agent_id, &worktree);

    let interaction = Arc::new(ScriptedInteractionHandler::approve_all());
    let scope = TestScope::builder()
        .external(Arc::new(handler) as Arc<dyn ExternalSessionHandler>)
        .interaction(interaction)
        .build();
    let ctx = root_context(&ids);

    let prompt = child_prompt(&runtime, brief);
    let result = drain(&mut machine, user_input(&ids, &prompt), &scope, None, &ctx).await;

    // Force-close the live session and drop the worktree before returning, so a
    // failure still leaves no orphaned CLI process or temp directory.
    let _ = registry.cleanup_agent(agent_id).await;
    cleanup_worktree(&worktree);

    match result {
        Ok(done) => DriveResult::Ran(ManagedOutcome {
            runtime,
            done: done.cursor().kind() == LoopCursorKind::Done,
            summary: log.summary(),
            observations: log.count(),
            streamed_events: sink.event_count(),
            permission_prompts: sink.permission_prompts(),
        }),
        Err(error) => DriveResult::Skipped(format!("{label} managed drive failed: {error:?}")),
    }
}

/// Prints a managed drive result in a consistent, non-secret format.
pub fn report(result: &DriveResult) {
    match result {
        DriveResult::Skipped(reason) => {
            println!("  skipped: {reason}");
        }
        DriveResult::Ran(outcome) => {
            println!(
                "  {:?}: done={}, observations={}, streamed_events={}, permission_prompts={}",
                outcome.runtime,
                outcome.done,
                outcome.observations,
                outcome.streamed_events,
                outcome.permission_prompts,
            );
            match &outcome.summary {
                Some(summary) => println!("  summary: {summary}"),
                None => println!("  summary: <none>"),
            }
        }
    }
}
