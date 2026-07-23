//! Opt-in *managed*-runtime coverage: a DeepSeek-backed coordinator that derives
//! Claude Code and Codex external subagents driven through the **managed adapter
//! stack** (`ClaudeCodeAdapter` / `CodexAdapter` behind an
//! [`ExternalSessionRegistry`]), not hand-rolled `claude`/`codex` CLI shells.
//!
//! This is the milestone-9 mixed real e2e (M9-4). It complements
//! `agent_external_real_e2e.rs` (which shells out directly) by exercising the
//! full managed path built in milestones 5–7: a registry-backed
//! [`ExternalSessionHandler`] that `get_or_start`s a live session, `advance`s it
//! one [`RuntimeDecisionPoint`] at a time, decodes the runtime's structured
//! stream into sequenced observations, and bridges permission pauses back into
//! the machine's [`Interaction`] path (approved in place by the child's own
//! attended scope). The DeepSeek coordinator plans two briefs, derives one child
//! per runtime via `NeedSubagent`, and synthesizes their reports into a final
//! status.
//!
//! # Feature gate
//!
//! The whole file compiles only under both managed adapters; with either feature
//! off it is an empty crate, so the default `cargo test --all --all-targets`
//! build never references the CLI-adapter machinery.
//!
//! # Running it
//!
//! These tests are `#[ignore]`: they call live services (DeepSeek over HTTPS) and
//! spawn local coding-agent CLIs (`claude`, `codex`). They are not part of the
//! default offline suite. Load credentials (for example `direnv allow` to export
//! `.envrc`, or set the vars in the shell) and run explicitly:
//!
//! ```text
//! cargo test --features "external-claude-code external-codex" \
//!     --test agent_external_managed_real_e2e -- --ignored --nocapture
//! ```
//!
//! ## Environment
//!
//! - `DEEPSEEK_API_KEY` (required for the coordinator test): the coordinator LLM
//!   key. Read from the process environment or a `.envrc` in the crate root; it
//!   is never logged. Optional `DEEPSEEK_BASE_URL` / `DEEPSEEK_MODEL` override the
//!   endpoint and model.
//! - `CLAUDE_CODE_BIN` / `CODEX_BIN`: absolute-path overrides for the CLIs
//!   (default: `claude` / `codex` on `PATH`). `CLAUDE_CODE_MODEL` / `CODEX_MODEL`
//!   pin cheaper models. Each spawned CLI inherits this process's environment and
//!   reads its own stored login, exactly as an interactive shell would.
//!
//! Any missing credential, absent binary, or failed capability probe turns the
//! test into a **skip** (a non-secret `eprintln!` and an early return), so an
//! unconfigured machine stays green rather than failing.
//!
//! ## Isolation & cleanup
//!
//! Every child runs in its own throwaway git worktree under the OS temp dir, so a
//! child that edits files never touches the checkout it launched from. Live
//! sessions are force-closed through the registry and the temp worktrees are
//! removed when each test finishes; both adapters also `kill_on_drop` their child
//! process as a backstop.

#![cfg(all(feature = "external-claude-code", feature = "external-codex"))]

use std::{
    collections::{BTreeMap, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_lib::agent::external::{
    ClaudeCodeAdapter, ClaudeCodeConfig, CodexAdapter, CodexConfig, ExternalEventSink,
    ExternalSessionRegistry, RuntimeDecisionPoint, codex_probe, probe as claude_probe,
};
use agent_lib::agent::{
    AgentError, AgentId, AgentInput, AgentMachine, AgentSpecRef, CursorRequirement,
    ExternalAgentEvent, ExternalAgentMachine, ExternalAgentSpec, ExternalObservedEvent,
    ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionHandler, ExternalSessionPolicy,
    ExternalSessionRequest, ExternalSessionResult, ExternalStreamPolicy, Interaction,
    InteractionKind, LlmHandler, LlmStepMode, LoopCursor, LoopCursorKind, LoopDoneReason,
    Requirement, RequirementId, RequirementIds, RequirementKind, RequirementKindTag,
    RequirementResolution, RequirementResult, RunContext, RunId, SpawnedChild, StepId, StepInput,
    StepOutcome, SubagentOutput, SubagentSpawner, ToolSetRef, TraceNodeId, TurnDone,
    WorktreeIsolation, WorktreeRef, drain,
};
use agent_lib::{
    adapter::openai_chat::OpenAiChatAdapter,
    client::{AuthScheme, ChatRequest, ClientError, EndpointConfig, Response as LlmResponse},
    conversation::{Conversation, ConversationConfig},
    model::{
        content::ContentBlock,
        extras::{ProviderExtras, ProviderId},
        message::{Message, Role},
    },
};
use agent_testkit::prelude::*;
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use tokio::{process::Command, time::timeout};

/// Marker the Claude child is asked to lead its report with, so the coordinator
/// (and the assertions) can confirm the real child answered.
const CLAUDE_MARKER: &str = "CLAUDE_CODE_MANAGED_OK";
/// Marker the Codex child is asked to lead its report with.
const CODEX_MARKER: &str = "CODEX_MANAGED_OK";
/// Marker the coordinator's final synthesis must contain.
const FINAL_MARKER: &str = "MANAGED_MULTI_AGENT_OK";

/// Per-read/shutdown timeout handed to each managed adapter's transport.
const IO_TIMEOUT: Duration = Duration::from_secs(120);
/// Whole-child wall-clock budget for a single managed subagent drive.
const CHILD_TIMEOUT: Duration = Duration::from_secs(240);
/// Whole-coordinator budget: plan call + two children + final synthesis.
const COORDINATOR_TIMEOUT: Duration = Duration::from_secs(600);
/// Bounds each child's `start → advance* → done` drive at the policy level.
const MAX_CHILD_TURNS: u32 = 12;

// ----- environment ---------------------------------------------------------

/// Minimal `.envrc`-plus-process environment reader.
///
/// Mirrors the credential handling used by `agent_external_real_e2e.rs` and the
/// per-adapter e2e suites so every managed test authenticates the same way. It
/// never prints a value, so a secret read from `.envrc` cannot leak into test
/// output.
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

    /// Resolves a variable from the process environment first, then `.envrc`,
    /// ignoring blank values so an exported-but-empty key does not mask a
    /// populated `.envrc` entry.
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

/// Returns `true` when `program --version` runs successfully, used to skip a
/// test whose CLI is not installed before any probe attempt.
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

/// Resolves the binary name/path for a runtime from its override env var.
fn runtime_binary(runtime: &ExternalRuntimeKind, env: &E2eEnv) -> String {
    match runtime {
        ExternalRuntimeKind::ClaudeCode => env
            .get("CLAUDE_CODE_BIN")
            .unwrap_or_else(|| "claude".to_owned()),
        ExternalRuntimeKind::Codex => env.get("CODEX_BIN").unwrap_or_else(|| "codex".to_owned()),
        other => format!("{other:?}"),
    }
}

// ----- DeepSeek coordinator LLM handler ------------------------------------

#[derive(Clone, Debug)]
struct DeepSeekConfig {
    api_key: String,
    base_url: String,
    model: String,
}

impl DeepSeekConfig {
    /// Reads the DeepSeek endpoint/key/model, returning `None` (with a non-secret
    /// skip note) when the key is absent.
    fn from_env(env: &E2eEnv) -> Option<Self> {
        let Some(api_key) = env.get("DEEPSEEK_API_KEY") else {
            eprintln!("skipping: DEEPSEEK_API_KEY is not configured in env or .envrc");
            return None;
        };
        Some(Self {
            api_key,
            base_url: env
                .get("DEEPSEEK_BASE_URL")
                .unwrap_or_else(|| "https://api.deepseek.com".to_owned()),
            model: env
                .get("DEEPSEEK_MODEL")
                .unwrap_or_else(|| "deepseek-chat".to_owned()),
        })
    }
}

/// Counts DeepSeek round-trips so the coordinator test can assert the model was
/// really consulted for planning and synthesis. It stores no prompt text to
/// avoid retaining anything sensitive.
#[derive(Default)]
struct DeepSeekCallLog {
    calls: Mutex<usize>,
}

impl DeepSeekCallLog {
    fn record(&self) {
        *self.calls.lock().expect("DeepSeek call log") += 1;
    }

    fn call_count(&self) -> usize {
        *self.calls.lock().expect("DeepSeek call log")
    }
}

struct DeepSeekLlmHandler {
    config: DeepSeekConfig,
    adapter: OpenAiChatAdapter,
    log: Arc<DeepSeekCallLog>,
}

impl DeepSeekLlmHandler {
    fn new(config: DeepSeekConfig, log: Arc<DeepSeekCallLog>) -> Self {
        // Bearer direct-connect, no Azure-style `api-key` header / `api-version`
        // query: the chat/completions adapter owns URL + header assembly.
        let endpoint = EndpointConfig {
            base_url: config.base_url.clone(),
            auth: AuthScheme::Bearer(config.api_key.clone()),
            query_params: Vec::new(),
            extra_headers: Vec::new(),
        };
        Self {
            config,
            adapter: OpenAiChatAdapter::new(endpoint),
            log,
        }
    }

    async fn chat(&self, request: &ChatRequest) -> Result<LlmResponse, ClientError> {
        // The adapter owns transport and wire parsing; this handler only applies
        // the e2e coordinator's request shaping and counts the round trip.
        let mut request = request.clone();
        if request.model.is_empty() {
            request.model = self.config.model.clone();
        }
        // The coordinator's planning system prompt requests a JSON object; force
        // structured output through the provider-extras escape hatch, which the
        // adapter merges into the request body verbatim.
        if request
            .system
            .as_deref()
            .is_some_and(|system| system.contains("JSON_OBJECT"))
        {
            request.provider_extras = Some(ProviderExtras {
                provider: ProviderId::OpenAiChat,
                fields: Map::from_iter([(
                    "response_format".to_owned(),
                    json!({ "type": "json_object" }),
                )]),
            });
        }

        let response = self.adapter.chat(request).await?;
        let content = response_text(&response);
        if content.trim().is_empty() {
            return Err(ClientError::Protocol(
                "DeepSeek returned an empty assistant message".to_owned(),
            ));
        }
        self.log.record();

        Ok(response)
    }
}

#[async_trait]
impl LlmHandler for DeepSeekLlmHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        _mode: LlmStepMode,
        _ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::Llm(self.chat(request).await)
    }
}

// ----- managed live-session observation bookkeeping ------------------------

/// A live side channel that counts observations the managed adapter mirrors as it
/// decodes the runtime stream, so a test can confirm the stream was genuinely
/// multi-event rather than a single terminal frame.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<ExternalAgentEvent>>,
}

impl RecordingSink {
    fn permission_prompts(&self) -> usize {
        self.events
            .lock()
            .expect("sink mutex")
            .iter()
            .filter(|event| matches!(event, ExternalAgentEvent::PermissionRequested { .. }))
            .count()
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

/// One completed managed subagent turn, keyed so the spawner's `summarize` can
/// fetch the right child's report.
#[derive(Clone, Debug)]
struct ManagedRecord {
    runtime: ExternalRuntimeKind,
    summary: String,
}

/// Shared bookkeeping across the managed handler(s): completed summaries and the
/// running total of replayed observations.
#[derive(Default)]
struct ManagedSessionLog {
    completed: Mutex<Vec<ManagedRecord>>,
    observations: Mutex<usize>,
}

impl ManagedSessionLog {
    fn add_observations(&self, count: usize) {
        *self.observations.lock().expect("observation counter") += count;
    }

    fn record_completed(&self, runtime: ExternalRuntimeKind, summary: String) {
        self.completed
            .lock()
            .expect("managed completion log")
            .push(ManagedRecord { runtime, summary });
    }

    fn total_observations(&self) -> usize {
        *self.observations.lock().expect("observation counter")
    }

    fn latest_summary(&self, runtime: &ExternalRuntimeKind) -> Option<String> {
        self.completed
            .lock()
            .expect("managed completion log")
            .iter()
            .rev()
            .find(|record| &record.runtime == runtime)
            .map(|record| record.summary.clone())
    }

    fn completed_runtime(&self, runtime: &ExternalRuntimeKind) -> bool {
        self.completed
            .lock()
            .expect("managed completion log")
            .iter()
            .any(|record| &record.runtime == runtime)
    }
}

// ----- registry-backed managed external session handler --------------------

/// A production-shaped [`ExternalSessionHandler`] that holds no machine state: it
/// resolves a live handle through its [`ExternalSessionRegistry`]
/// (`get_or_start` on the first `Start`, reattach on every follow-up), advances
/// it exactly one [`RuntimeDecisionPoint`], and folds the outcome into a
/// family-aligned [`ExternalSessionResult`]. This is the same composition the
/// scripted/cassette handlers use, but over a *real* probed adapter.
struct ManagedRuntimeHandler {
    runtime: ExternalRuntimeKind,
    registry: Arc<ExternalSessionRegistry>,
    sink: Arc<RecordingSink>,
    log: Arc<ManagedSessionLog>,
}

impl ManagedRuntimeHandler {
    fn registry(&self) -> &Arc<ExternalSessionRegistry> {
        &self.registry
    }

    fn sink(&self) -> &Arc<RecordingSink> {
        &self.sink
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
            self.log.add_observations(decision.observations().len());
            if let RuntimeDecisionPoint::Completed { output, .. } = decision {
                self.log
                    .record_completed(self.runtime.clone(), output.summary.clone());
            }
        }
        point.into()
    }
}

#[async_trait]
impl ExternalSessionHandler for ManagedRuntimeHandler {
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::ExternalSession(Box::new(self.advance(request, ctx).await))
    }
}

/// Builds a probed managed handler for `runtime` rooted at `worktree`, or returns
/// a non-secret skip reason when the CLI is missing or its capability probe
/// fails (an auth/runtime signal, not a test failure).
async fn build_managed_handler(
    runtime: ExternalRuntimeKind,
    worktree: &Path,
    env: &E2eEnv,
    log: Arc<ManagedSessionLog>,
) -> Result<ManagedRuntimeHandler, String> {
    let registry = match runtime {
        ExternalRuntimeKind::ClaudeCode => {
            let mut config = ClaudeCodeConfig::new()
                .with_working_dir(worktree)
                // Prompt mode leaves gated actions (file edits, shell) to a
                // permission prompt so the child exercises the managed
                // interaction bridge rather than silently auto-approving.
                .with_permission_mode(ExternalPermissionMode::Prompt)
                .with_timeout(IO_TIMEOUT)
                .with_binary(runtime_binary(&runtime, env));
            if let Some(model) = env.get("CLAUDE_CODE_MODEL") {
                config = config.with_model(model);
            }
            let probed = claude_probe(&config)
                .await
                .map_err(|error| format!("Claude Code probe failed: {error:?}"))?;
            let adapter = ClaudeCodeAdapter::with_probed_capabilities(config, &probed);
            ExternalSessionRegistry::new(Arc::new(adapter))
        }
        ExternalRuntimeKind::Codex => {
            let mut config = CodexConfig::new()
                .with_working_dir(worktree)
                .with_permission_mode(ExternalPermissionMode::Prompt)
                .with_timeout(IO_TIMEOUT)
                .with_binary(runtime_binary(&runtime, env));
            if let Some(model) = env.get("CODEX_MODEL") {
                config = config.with_model(model);
            }
            let probed = codex_probe(&config)
                .await
                .map_err(|error| format!("Codex probe failed: {error:?}"))?;
            let adapter = CodexAdapter::with_probed_capabilities(config, &probed);
            ExternalSessionRegistry::new(Arc::new(adapter))
        }
        other => return Err(format!("unsupported managed runtime {other:?}")),
    };

    Ok(ManagedRuntimeHandler {
        runtime,
        registry: Arc::new(registry),
        sink: Arc::new(RecordingSink::default()),
        log,
    })
}

// ----- external machine fixtures -------------------------------------------

fn external_policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        // Prompt so a managed child surfaces gated actions as an interaction.
        permission_mode: ExternalPermissionMode::Prompt,
        // The test owns each child's throwaway worktree; the machine must not
        // create a second one.
        isolation: WorktreeIsolation::Shared,
        max_turns: Some(MAX_CHILD_TURNS),
        stream_events: ExternalStreamPolicy::Buffered,
    }
}

fn external_machine(
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
        external_policy(),
    );
    let state = agent_lib::agent::ExternalAgentState::new(
        spec,
        Conversation::new(
            ids.conversation_id(),
            ConversationConfig::new(Some("Managed external e2e conversation.".to_owned())),
        ),
    );
    ExternalAgentMachine::new(state, Arc::new(ids.clone()))
}

/// Wraps a child brief in a read-only guardrail plus a single gated file write,
/// so the child both answers concisely and (in Prompt mode) exercises the
/// managed permission bridge at least once.
fn managed_child_prompt(runtime: &ExternalRuntimeKind, marker: &str, brief: &str) -> String {
    format!(
        "You are an agent-lib ExternalAgentMachine managed e2e child backed by {runtime:?}.\n\
         Do only what this task needs; do not run long commands or touch anything outside \
         the current working directory.\n\
         Task brief:\n{brief}\n\n\
         Concrete step: create a file named `MANAGED_READY.txt` in the current directory \
         containing the single word READY (this action requires host approval).\n\n\
         Final response requirements:\n\
         - Return exactly one short paragraph.\n\
         - The paragraph must start with `{marker}:`.\n\
         - Mention the file you created."
    )
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            ContentBlock::ToolResult { content, .. } => Some(content_text(content)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn response_text(response: &LlmResponse) -> String {
    message_text(&response.message)
}

// ----- worktree helpers ----------------------------------------------------

/// Creates an isolated `git init` worktree under the OS temp dir so a child that
/// writes files never touches the checkout it was launched from.
fn make_worktree(label: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "agent-lib-managed-e2e-{label}-{}-{}",
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

fn cleanup_worktree(worktree: &Path) {
    let _ = fs::remove_dir_all(worktree);
}

// ----- DeepSeek coordinator machine ----------------------------------------

#[derive(Clone, Debug)]
struct CoordinatorPlan {
    claude_brief: String,
    codex_brief: String,
}

#[derive(Debug)]
enum CoordinatorStage {
    Idle,
    AwaitingPlan {
        requirement: RequirementId,
        step_id: StepId,
        task: String,
    },
    AwaitingClaude {
        requirement: RequirementId,
        step_id: StepId,
        plan: CoordinatorPlan,
    },
    AwaitingCodex {
        requirement: RequirementId,
        step_id: StepId,
        claude_summary: String,
    },
    AwaitingFinal {
        requirement: RequirementId,
    },
    Done,
    Error,
}

/// A bespoke coordinator machine: it asks DeepSeek to plan two briefs, derives a
/// Claude Code child then a Codex child through `NeedSubagent`, and finally asks
/// DeepSeek to synthesize both child reports into a single status line.
struct DeepSeekCoordinatorMachine {
    ids: SeqIds,
    model: String,
    claude_spec: AgentSpecRef,
    codex_spec: AgentSpecRef,
    cursor: LoopCursor,
    stage: CoordinatorStage,
    final_text: Option<String>,
}

impl DeepSeekCoordinatorMachine {
    fn new(
        ids: SeqIds,
        model: String,
        claude_spec: AgentSpecRef,
        codex_spec: AgentSpecRef,
    ) -> Self {
        Self {
            ids,
            model,
            claude_spec,
            codex_spec,
            cursor: LoopCursor::Idle,
            stage: CoordinatorStage::Idle,
            final_text: None,
        }
    }

    fn final_text(&self) -> Option<&str> {
        self.final_text.as_deref()
    }

    fn begin(&mut self, input: AgentInput) -> StepOutcome {
        let AgentInput::UserMessage(user) = input else {
            return self.fail("coordinator accepts only user-message input");
        };
        let task = message_text(user.message());
        let step_id = user.step_id();
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![user.message().clone()],
            tools: Vec::new(),
            system: Some(format!(
                "JSON_OBJECT. You are a deterministic coordinator for an agent-lib managed e2e \
                 test. Return one JSON object with string fields `claude_brief` and \
                 `codex_brief`. The Claude brief must ask Claude Code to inspect \
                 ExternalAgentMachine basics. The Codex brief must ask Codex to inspect subagent \
                 composition. Do not include markdown. The user's task is: {task}"
            )),
            max_tokens: 360,
            temperature: Some(0.0),
            stream: false,
            provider_extras: None,
        };
        let requirement = self.need_llm(step_id, request);
        self.stage = CoordinatorStage::AwaitingPlan {
            requirement: requirement.id,
            step_id,
            task,
        };
        StepOutcome::new(Vec::new(), vec![requirement], true)
    }

    fn resume(&mut self, resolution: RequirementResolution) -> StepOutcome {
        match std::mem::replace(&mut self.stage, CoordinatorStage::Error) {
            CoordinatorStage::AwaitingPlan {
                requirement,
                step_id,
                task,
            } => {
                if resolution.id != requirement {
                    return self.fail("plan response targeted the wrong requirement");
                }
                let RequirementResult::Llm(Ok(response)) = resolution.result else {
                    return self.fail("DeepSeek plan call failed or returned the wrong result");
                };
                let plan =
                    parse_plan(&response_text(&response)).unwrap_or_else(|| CoordinatorPlan {
                        claude_brief: format!(
                            "Inspect ExternalAgentMachine start/completed basics for task: {task}"
                        ),
                        codex_brief: format!(
                            "Inspect ExternalAgentMachine subagent composition for task: {task}"
                        ),
                    });
                let requirement = self.need_subagent(
                    step_id,
                    self.claude_spec,
                    format!(
                        "{}\nReturn a concise report for the coordinator. Include `{CLAUDE_MARKER}`.",
                        plan.claude_brief
                    ),
                );
                self.stage = CoordinatorStage::AwaitingClaude {
                    requirement: requirement.id,
                    step_id,
                    plan,
                };
                StepOutcome::new(Vec::new(), vec![requirement], true)
            }
            CoordinatorStage::AwaitingClaude {
                requirement,
                step_id,
                plan,
            } => {
                if resolution.id != requirement {
                    return self.fail("Claude subagent response targeted the wrong requirement");
                }
                let RequirementResult::Subagent(Ok(output)) = resolution.result else {
                    return self.fail("Claude subagent failed or returned the wrong result");
                };
                let requirement = self.need_subagent(
                    step_id,
                    self.codex_spec,
                    format!(
                        "{}\nUse this Claude summary as context:\n{}\n\
                         Return a concise report for the coordinator. Include `{CODEX_MARKER}`.",
                        plan.codex_brief, output.summary
                    ),
                );
                self.stage = CoordinatorStage::AwaitingCodex {
                    requirement: requirement.id,
                    step_id,
                    claude_summary: output.summary,
                };
                StepOutcome::new(Vec::new(), vec![requirement], true)
            }
            CoordinatorStage::AwaitingCodex {
                requirement,
                step_id,
                claude_summary,
            } => {
                if resolution.id != requirement {
                    return self.fail("Codex subagent response targeted the wrong requirement");
                }
                let RequirementResult::Subagent(Ok(output)) = resolution.result else {
                    return self.fail("Codex subagent failed or returned the wrong result");
                };
                let request = ChatRequest {
                    model: self.model.clone(),
                    messages: vec![Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: format!(
                                "Synthesize the two child reports into one short final status. \
                                 The answer must start with `{FINAL_MARKER}:`.\n\n\
                                 Claude report:\n{claude_summary}\n\nCodex report:\n{}",
                                output.summary
                            ),
                            extra: Map::new(),
                        }],
                    }],
                    tools: Vec::new(),
                    system: Some(
                        "You are the final coordinator. Return one short paragraph only."
                            .to_owned(),
                    ),
                    max_tokens: 240,
                    temperature: Some(0.0),
                    stream: false,
                    provider_extras: None,
                };
                let requirement = self.need_llm(step_id, request);
                self.stage = CoordinatorStage::AwaitingFinal {
                    requirement: requirement.id,
                };
                StepOutcome::new(Vec::new(), vec![requirement], true)
            }
            CoordinatorStage::AwaitingFinal { requirement, .. } => {
                if resolution.id != requirement {
                    return self.fail("final DeepSeek response targeted the wrong requirement");
                }
                let RequirementResult::Llm(Ok(response)) = resolution.result else {
                    return self.fail("DeepSeek final call failed or returned the wrong result");
                };
                self.final_text = Some(response_text(&response));
                self.cursor = LoopCursor::done(LoopDoneReason::Completed);
                self.stage = CoordinatorStage::Done;
                StepOutcome::new(Vec::new(), Vec::new(), true)
            }
            CoordinatorStage::Idle | CoordinatorStage::Done | CoordinatorStage::Error => {
                self.fail("coordinator received resume while not awaiting a requirement")
            }
        }
    }

    fn need_llm(&mut self, step_id: StepId, request: ChatRequest) -> Requirement {
        let id = self
            .ids
            .next_requirement_id(RequirementKindTag::Llm)
            .expect("coordinator LLM requirement id");
        self.cursor = LoopCursor::streaming_step(step_id, Some(CursorRequirement::root(id)));
        Requirement::at_root(
            id,
            RequirementKind::NeedLlm {
                request,
                mode: LlmStepMode::NonStreaming,
            },
        )
    }

    fn need_subagent(
        &mut self,
        step_id: StepId,
        spec_ref: AgentSpecRef,
        brief: String,
    ) -> Requirement {
        let id = self
            .ids
            .next_requirement_id(RequirementKindTag::Subagent)
            .expect("coordinator subagent requirement id");
        self.cursor = LoopCursor::streaming_step(step_id, Some(CursorRequirement::root(id)));
        Requirement::at_root(
            id,
            RequirementKind::NeedSubagent {
                spec_ref,
                brief: Interaction::question(step_id, brief),
                result_schema: None,
            },
        )
    }

    fn fail(&mut self, message: impl Into<String>) -> StepOutcome {
        self.stage = CoordinatorStage::Error;
        let message = message.into();
        self.cursor = LoopCursor::error(message).unwrap_or(LoopCursor::Idle);
        StepOutcome::new(Vec::new(), Vec::new(), true)
    }
}

impl AgentMachine for DeepSeekCoordinatorMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        match input {
            StepInput::External(input) => self.begin(input),
            StepInput::Resume(resolution) => self.resume(resolution),
            StepInput::Abandon(_) => {
                self.stage = CoordinatorStage::Idle;
                self.cursor = LoopCursor::Idle;
                StepOutcome::new(Vec::new(), Vec::new(), true)
            }
        }
    }

    fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }
}

fn parse_plan(text: &str) -> Option<CoordinatorPlan> {
    let value: Value = serde_json::from_str(text)
        .ok()
        .or_else(|| extract_json_object(text).and_then(|json| serde_json::from_str(&json).ok()))?;
    Some(CoordinatorPlan {
        claude_brief: value.get("claude_brief")?.as_str()?.to_owned(),
        codex_brief: value.get("codex_brief")?.as_str()?.to_owned(),
    })
}

fn extract_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| text[start..=end].to_owned())
}

// ----- managed external subagent spawner -----------------------------------

/// Resolves a coordinator `NeedSubagent` into a child [`ExternalAgentMachine`]
/// driven through a pre-probed managed handler, with an attended scope that
/// approves any permission the runtime raises in place. The adapters are probed
/// once during test setup (probing is async, `spawn` is sync), so each spawn only
/// clones a pre-built handler and mints a fresh interaction handler.
struct ManagedExternalSubagentSpawner {
    ids: SeqIds,
    claude_spec: AgentSpecRef,
    codex_spec: AgentSpecRef,
    claude_handler: Arc<ManagedRuntimeHandler>,
    codex_handler: Arc<ManagedRuntimeHandler>,
    claude_worktree: PathBuf,
    codex_worktree: PathBuf,
    log: Arc<ManagedSessionLog>,
    spawned: Mutex<Vec<ExternalRuntimeKind>>,
    pending: Mutex<VecDeque<ExternalRuntimeKind>>,
    interactions: Mutex<Vec<Arc<InteractionCallLog>>>,
}

impl ManagedExternalSubagentSpawner {
    fn spawned(&self) -> Vec<ExternalRuntimeKind> {
        self.spawned.lock().expect("spawn log").clone()
    }

    fn total_interactions(&self) -> usize {
        self.interactions
            .lock()
            .expect("interaction logs")
            .iter()
            .map(|log| log.len())
            .sum()
    }
}

impl SubagentSpawner for ManagedExternalSubagentSpawner {
    fn child_ids(&self, spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        let runtime = runtime_for_spec(spec_ref, self.claude_spec, self.codex_spec)?;
        Ok((
            self.ids.run_id(),
            self.ids
                .trace_node(&format!("managed-{}", runtime_label(&runtime))),
        ))
    }

    fn spawn(
        &self,
        spec_ref: &AgentSpecRef,
        brief: &Interaction,
        _result_schema: Option<&Value>,
    ) -> Result<SpawnedChild, AgentError> {
        let runtime = runtime_for_spec(spec_ref, self.claude_spec, self.codex_spec)?;
        let marker = runtime_marker(&runtime);
        let brief_text = brief_text(brief)?;
        let prompt = managed_child_prompt(&runtime, marker, &brief_text);

        self.spawned
            .lock()
            .expect("spawn log")
            .push(runtime.clone());
        self.pending
            .lock()
            .expect("pending summaries")
            .push_back(runtime.clone());

        let (handler, worktree): (Arc<ManagedRuntimeHandler>, &Path) = match runtime {
            ExternalRuntimeKind::ClaudeCode => {
                (Arc::clone(&self.claude_handler), &self.claude_worktree)
            }
            ExternalRuntimeKind::Codex => (Arc::clone(&self.codex_handler), &self.codex_worktree),
            ref other => {
                return Err(AgentError::Other(format!(
                    "managed spawner only drives Claude Code and Codex, got {other:?}"
                )));
            }
        };

        let interaction = Arc::new(ScriptedInteractionHandler::approve_all());
        self.interactions
            .lock()
            .expect("interaction logs")
            .push(Arc::clone(interaction.log()));

        let child_ids = self.ids.fork(runtime_label(&runtime));
        let machine = external_machine(&child_ids, runtime.clone(), spec_ref.0, worktree);
        let scope = TestScope::builder()
            .external(handler as Arc<dyn ExternalSessionHandler>)
            .interaction(interaction)
            .build();
        Ok(SpawnedChild {
            machine: Box::new(machine),
            scope: Box::new(scope),
            opening: user_input(&child_ids, &prompt),
        })
    }

    fn summarize(&self, _done: &TurnDone) -> SubagentOutput {
        let runtime = self.pending.lock().expect("pending summaries").pop_front();
        let summary = runtime
            .and_then(|runtime| self.log.latest_summary(&runtime))
            .unwrap_or_else(|| {
                "managed external subagent completed without a captured summary".to_owned()
            });
        SubagentOutput { summary }
    }
}

fn runtime_for_spec(
    spec_ref: &AgentSpecRef,
    claude_spec: AgentSpecRef,
    codex_spec: AgentSpecRef,
) -> Result<ExternalRuntimeKind, AgentError> {
    if *spec_ref == claude_spec {
        Ok(ExternalRuntimeKind::ClaudeCode)
    } else if *spec_ref == codex_spec {
        Ok(ExternalRuntimeKind::Codex)
    } else {
        Err(AgentError::Other(format!(
            "unknown managed external subagent spec {}",
            spec_ref.0
        )))
    }
}

fn brief_text(brief: &Interaction) -> Result<String, AgentError> {
    match brief.kind() {
        InteractionKind::Question { prompt } => Ok(prompt.clone()),
        other => Err(AgentError::Other(format!(
            "managed e2e subagent brief must be a question, got {:?}",
            other.tag()
        ))),
    }
}

fn runtime_label(runtime: &ExternalRuntimeKind) -> &'static str {
    match runtime {
        ExternalRuntimeKind::ClaudeCode => "claude-code",
        ExternalRuntimeKind::Codex => "codex",
        ExternalRuntimeKind::OpenCode => "opencode",
        ExternalRuntimeKind::Custom(_) => "custom",
    }
}

fn runtime_marker(runtime: &ExternalRuntimeKind) -> &'static str {
    match runtime {
        ExternalRuntimeKind::ClaudeCode => CLAUDE_MARKER,
        ExternalRuntimeKind::Codex => CODEX_MARKER,
        _ => FINAL_MARKER,
    }
}

// ----- single-runtime managed child drive ----------------------------------

/// Drives one managed child [`ExternalAgentMachine`] to the end of its opening
/// turn and asserts it committed a conversation turn with at least one replayed
/// observation. Returns `false` when the runtime was un-probeable (a skip).
async fn drive_single_managed_child(runtime: ExternalRuntimeKind, env: &E2eEnv) -> bool {
    let label = runtime_label(&runtime);
    let worktree = make_worktree(label);
    let log = Arc::new(ManagedSessionLog::default());

    let handler =
        match build_managed_handler(runtime.clone(), &worktree, env, Arc::clone(&log)).await {
            Ok(handler) => handler,
            Err(reason) => {
                eprintln!("skipping {label} managed child: {reason}");
                cleanup_worktree(&worktree);
                return false;
            }
        };
    let registry = Arc::clone(handler.registry());
    let sink = Arc::clone(handler.sink());

    let interaction = Arc::new(ScriptedInteractionHandler::approve_all());
    let interaction_log = Arc::clone(interaction.log());

    let ids = SeqIds::new();
    let agent_id = ids.agent_id();
    let mut machine = external_machine(&ids, runtime.clone(), agent_id, &worktree);
    let scope = TestScope::builder()
        .external(Arc::new(handler) as Arc<dyn ExternalSessionHandler>)
        .interaction(interaction)
        .build();
    let ctx = root_context(&ids);

    let prompt = managed_child_prompt(
        &runtime,
        runtime_marker(&runtime),
        "Confirm the managed ExternalAgentMachine start/completed path end to end.",
    );

    let result = timeout(
        CHILD_TIMEOUT,
        drain(&mut machine, user_input(&ids, &prompt), &scope, None, &ctx),
    )
    .await;

    // Force-close the live session and drop the worktree before asserting, so a
    // panic still leaves no orphaned CLI process or temp directory.
    let _ = registry.cleanup_agent(agent_id).await;
    cleanup_worktree(&worktree);

    let done = result
        .unwrap_or_else(|_| panic!("{label} managed child exceeded its {CHILD_TIMEOUT:?} budget"))
        .unwrap_or_else(|error| panic!("{label} managed child drain failed: {error:?}"));

    assert_eq!(
        done.cursor().kind(),
        LoopCursorKind::Done,
        "{label} managed child should drain to Done"
    );
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none();
    assert!(
        log.total_observations() >= 1,
        "{label} managed child should replay at least one external observation"
    );

    eprintln!(
        "{label} managed child: {} replayed observations, {} managed interactions, \
         {} permission prompts observed",
        log.total_observations(),
        interaction_log.len(),
        sink.permission_prompts(),
    );
    true
}

// ----- tests ---------------------------------------------------------------

/// The managed Claude Code adapter drives a real child `ExternalAgentMachine`
/// turn (start → structured stream → completed) and commits its conversation.
#[tokio::test]
#[ignore = "requires local Claude Code auth/runtime; spawns `claude`"]
async fn managed_claude_code_child_commits_turn() {
    let env = E2eEnv::load();
    if !command_available(&runtime_binary(&ExternalRuntimeKind::ClaudeCode, &env)).await {
        eprintln!("skipping: `claude` is not available (set CLAUDE_CODE_BIN or install `claude`)");
        return;
    }
    let _ran = drive_single_managed_child(ExternalRuntimeKind::ClaudeCode, &env).await;
}

/// The managed Codex adapter drives a real child `ExternalAgentMachine` turn and
/// commits its conversation.
#[tokio::test]
#[ignore = "requires local Codex auth/runtime; spawns `codex`"]
async fn managed_codex_child_commits_turn() {
    let env = E2eEnv::load();
    if !command_available(&runtime_binary(&ExternalRuntimeKind::Codex, &env)).await {
        eprintln!("skipping: `codex` is not available (set CODEX_BIN or install `codex`)");
        return;
    }
    let _ran = drive_single_managed_child(ExternalRuntimeKind::Codex, &env).await;
}

/// The headline M9-4 test: a DeepSeek coordinator plans, derives a Claude Code
/// child and a Codex child through the managed adapter stack, and synthesizes
/// their reports into one final status.
#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY plus local Claude Code and Codex runtimes"]
async fn deepseek_coordinator_drives_managed_claude_code_and_codex_subagents() {
    let env = E2eEnv::load();
    let Some(deepseek) = DeepSeekConfig::from_env(&env) else {
        return;
    };
    if !command_available(&runtime_binary(&ExternalRuntimeKind::ClaudeCode, &env)).await {
        eprintln!("skipping: `claude` is not available on PATH");
        return;
    }
    if !command_available(&runtime_binary(&ExternalRuntimeKind::Codex, &env)).await {
        eprintln!("skipping: `codex` is not available on PATH");
        return;
    }

    let ids = SeqIds::new();
    let claude_spec = AgentSpecRef(ids.agent_id());
    let codex_spec = AgentSpecRef(ids.agent_id());
    let managed_log = Arc::new(ManagedSessionLog::default());

    // Probe both managed adapters up front (probing is async; `spawn` is sync).
    let claude_worktree = make_worktree("claude-code");
    let codex_worktree = make_worktree("codex");
    let claude_handler = match build_managed_handler(
        ExternalRuntimeKind::ClaudeCode,
        &claude_worktree,
        &env,
        Arc::clone(&managed_log),
    )
    .await
    {
        Ok(handler) => Arc::new(handler),
        Err(reason) => {
            eprintln!("skipping: {reason}");
            cleanup_worktree(&claude_worktree);
            cleanup_worktree(&codex_worktree);
            return;
        }
    };
    let codex_handler = match build_managed_handler(
        ExternalRuntimeKind::Codex,
        &codex_worktree,
        &env,
        Arc::clone(&managed_log),
    )
    .await
    {
        Ok(handler) => Arc::new(handler),
        Err(reason) => {
            eprintln!("skipping: {reason}");
            cleanup_worktree(&claude_worktree);
            cleanup_worktree(&codex_worktree);
            return;
        }
    };

    let deepseek_log = Arc::new(DeepSeekCallLog::default());
    let spawner = Arc::new(ManagedExternalSubagentSpawner {
        ids: ids.clone(),
        claude_spec,
        codex_spec,
        claude_handler: Arc::clone(&claude_handler),
        codex_handler: Arc::clone(&codex_handler),
        claude_worktree: claude_worktree.clone(),
        codex_worktree: codex_worktree.clone(),
        log: Arc::clone(&managed_log),
        spawned: Mutex::new(Vec::new()),
        pending: Mutex::new(VecDeque::new()),
        interactions: Mutex::new(Vec::new()),
    });
    let subagent_handler = agent_lib::agent::DrivingSubagentHandler::new(
        Arc::clone(&spawner) as Arc<dyn SubagentSpawner>,
        4,
    );
    let scope = TestScope::builder()
        .llm(Arc::new(DeepSeekLlmHandler::new(
            deepseek.clone(),
            Arc::clone(&deepseek_log),
        )))
        .subagent(Arc::new(subagent_handler))
        .build();
    let ctx = root_context(&ids);
    let mut coordinator =
        DeepSeekCoordinatorMachine::new(ids.clone(), deepseek.model, claude_spec, codex_spec);

    let result = timeout(
        COORDINATOR_TIMEOUT,
        drain(
            &mut coordinator,
            user_input(
                &ids,
                "Plan and execute a two-worker managed e2e check over ExternalAgentMachine.",
            ),
            &scope,
            None,
            &ctx,
        ),
    )
    .await;

    // Force-close both live sessions and drop the worktrees before asserting.
    let _ = claude_handler.registry().cleanup_agent(claude_spec.0).await;
    let _ = codex_handler.registry().cleanup_agent(codex_spec.0).await;
    cleanup_worktree(&claude_worktree);
    cleanup_worktree(&codex_worktree);

    let done = result
        .expect("DeepSeek managed multi-agent e2e exceeded its wall-clock limit")
        .expect("DeepSeek managed multi-agent e2e drain failed");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(
        spawner.spawned(),
        vec![ExternalRuntimeKind::ClaudeCode, ExternalRuntimeKind::Codex],
        "the coordinator should derive one Claude Code child then one Codex child"
    );
    assert!(
        managed_log.completed_runtime(&ExternalRuntimeKind::ClaudeCode),
        "the managed Claude Code child should complete a session"
    );
    assert!(
        managed_log.completed_runtime(&ExternalRuntimeKind::Codex),
        "the managed Codex child should complete a session"
    );
    assert!(
        managed_log.total_observations() >= 1,
        "at least one external observation should be replayed across the two children"
    );
    assert!(
        deepseek_log.call_count() >= 2,
        "coordinator should call DeepSeek for planning and final synthesis"
    );
    let final_text = coordinator
        .final_text()
        .expect("coordinator records final DeepSeek synthesis");
    assert!(
        final_text.contains(FINAL_MARKER),
        "final coordinator text should contain {FINAL_MARKER}, got {final_text:?}"
    );

    eprintln!(
        "managed coordinator e2e: {} DeepSeek calls, {} replayed observations, \
         {} managed interactions across children",
        deepseek_log.call_count(),
        managed_log.total_observations(),
        spawner.total_interactions(),
    );
}
