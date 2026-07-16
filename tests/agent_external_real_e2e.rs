//! Opt-in real-runtime coverage for `ExternalAgentMachine` with Claude Code,
//! Codex, and a DeepSeek-backed coordinator.
//!
//! These tests are intentionally `#[ignore]`: they call live services and spawn
//! local coding-agent CLIs. They are not part of the default offline suite.
//! Run explicitly, for example:
//!
//! ```text
//! cargo test --test agent_external_real_e2e -- --ignored --nocapture
//! ```

use std::{
    collections::{BTreeMap, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_lib::{
    agent::{
        AgentError, AgentInput, AgentMachine, AgentSpecRef, CursorRequirement, ExternalAgentError,
        ExternalAgentEvent, ExternalAgentMachine, ExternalAgentOutput, ExternalAgentSpec,
        ExternalArtifactKind, ExternalArtifactRef, ExternalPermissionMode, ExternalRuntimeKind,
        ExternalSessionHandler, ExternalSessionInput, ExternalSessionPolicy, ExternalSessionRef,
        ExternalSessionRequest, ExternalSessionResult, ExternalStreamPolicy, Interaction,
        InteractionKind, LlmHandler, LlmStepMode, LoopCursor, LoopCursorKind, LoopDoneReason,
        Requirement, RequirementIds, RequirementKind, RequirementKindTag, RequirementResolution,
        RequirementResult, RunContext, RunId, SpawnedChild, StepId, StepInput, StepOutcome,
        SubagentOutput, SubagentSpawner, ToolSetRef, TraceNodeId, WorktreeIsolation, WorktreeRef,
        drain,
    },
    client::{ChatRequest, ClientError, Response as LlmResponse},
    conversation::{Conversation, ConversationConfig},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::{Normalized, StopReason},
        usage::Usage,
    },
};
use agent_testkit::prelude::*;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::{process::Command, time::timeout};

const CLAUDE_MARKER: &str = "CLAUDE_CODE_E2E_OK";
const CODEX_MARKER: &str = "CODEX_E2E_OK";
const FINAL_MARKER: &str = "MULTI_AGENT_E2E_OK";

const DIRECT_TIMEOUT: Duration = Duration::from_secs(240);
const COORDINATOR_TIMEOUT: Duration = Duration::from_secs(420);
const HTTP_TIMEOUT: Duration = Duration::from_secs(75);
const CLI_TIMEOUT: Duration = Duration::from_secs(180);

// ----- environment ---------------------------------------------------------

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

    fn command_envs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
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

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ----- DeepSeek LLM handler ------------------------------------------------

#[derive(Clone, Debug)]
struct DeepSeekConfig {
    api_key: String,
    base_url: String,
    model: String,
}

impl DeepSeekConfig {
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

    fn chat_url(&self) -> Result<reqwest::Url, ClientError> {
        let mut url = reqwest::Url::parse(&self.base_url)
            .map_err(|err| ClientError::Other(format!("invalid DeepSeek base URL: {err}")))?;
        let current = url.path().trim_end_matches('/');
        let path = if current.ends_with("/chat/completions") {
            current.to_owned()
        } else if current.is_empty() {
            "/chat/completions".to_owned()
        } else {
            format!("{current}/chat/completions")
        };
        url.set_path(&path);
        Ok(url)
    }
}

#[derive(Default)]
struct DeepSeekCallLog {
    prompts: Mutex<Vec<String>>,
    responses: Mutex<Vec<String>>,
}

impl DeepSeekCallLog {
    fn record_prompt(&self, prompt: String) {
        self.prompts
            .lock()
            .expect("DeepSeek prompt log")
            .push(prompt);
    }

    fn record_response(&self, response: String) {
        self.responses
            .lock()
            .expect("DeepSeek response log")
            .push(response);
    }

    fn call_count(&self) -> usize {
        self.responses.lock().expect("DeepSeek response log").len()
    }
}

struct DeepSeekLlmHandler {
    config: DeepSeekConfig,
    http: reqwest::Client,
    log: Arc<DeepSeekCallLog>,
}

impl DeepSeekLlmHandler {
    fn new(config: DeepSeekConfig, log: Arc<DeepSeekCallLog>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("build DeepSeek HTTP client");
        Self { config, http, log }
    }

    async fn chat(&self, request: &ChatRequest) -> Result<LlmResponse, ClientError> {
        let prompt = request_text(request);
        self.log.record_prompt(prompt);

        let mut body = json!({
            "model": if request.model.is_empty() {
                self.config.model.as_str()
            } else {
                request.model.as_str()
            },
            "messages": chat_messages(request),
            "max_tokens": request.max_tokens,
            "temperature": request.temperature.unwrap_or(0.0),
            "stream": false,
        });
        if request
            .system
            .as_deref()
            .is_some_and(|system| system.contains("JSON_OBJECT"))
        {
            body["response_format"] = json!({ "type": "json_object" });
        }

        let url = self.config.chat_url()?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|err| ClientError::Network(err.to_string()))?;

        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let text = response
            .text()
            .await
            .map_err(|err| ClientError::Network(err.to_string()))?;

        if !status.is_success() {
            return Err(ClientError::from_http_response(
                status.as_u16(),
                text,
                retry_after.as_deref(),
            ));
        }

        let wire: DeepSeekChatResponse = serde_json::from_str(&text)
            .map_err(|err| ClientError::Protocol(format!("invalid DeepSeek JSON: {err}")))?;
        let choice = wire
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ClientError::Protocol("DeepSeek returned no choices".to_owned()))?;
        let content = choice.message.content.unwrap_or_default();
        if content.trim().is_empty() {
            return Err(ClientError::Protocol(
                "DeepSeek returned an empty assistant message".to_owned(),
            ));
        }
        self.log.record_response(content.clone());

        let mut extra = Map::new();
        if let Some(id) = wire.id {
            extra.insert("id".to_owned(), Value::String(id));
        }
        if let Some(model) = wire.model {
            extra.insert("model".to_owned(), Value::String(model));
        }

        Ok(LlmResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: content,
                    extra: Map::new(),
                }],
            },
            usage: wire.usage.unwrap_or_default(),
            stop_reason: normalize_finish_reason(choice.finish_reason.as_deref()),
            extra,
        })
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

#[derive(Debug, Deserialize)]
struct DeepSeekChatResponse {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<DeepSeekChoice>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekChoice {
    message: DeepSeekMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekMessage {
    content: Option<String>,
}

fn chat_messages(request: &ChatRequest) -> Vec<Value> {
    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for message in &request.messages {
        let text = message_text(message);
        if text.trim().is_empty() {
            continue;
        }
        messages.push(json!({
            "role": match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
                Role::Tool => "tool",
            },
            "content": text,
        }));
    }
    messages
}

fn request_text(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .map(message_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_finish_reason(raw: Option<&str>) -> Normalized<StopReason> {
    match raw.unwrap_or("stop") {
        "stop" => Normalized::from_mapped(StopReason::EndTurn, "stop"),
        "length" => Normalized::from_mapped(StopReason::MaxTokens, "length"),
        "content_filter" => Normalized::from_mapped(StopReason::Refusal, "content_filter"),
        other => StopReason::normalize(other),
    }
}

// ----- real CLI external-session handler ----------------------------------

#[derive(Clone, Debug)]
struct CliSessionHandler {
    runtime: ExternalRuntimeKind,
    env: E2eEnv,
    log: Arc<CliSessionLog>,
    timeout: Duration,
}

impl CliSessionHandler {
    fn new(runtime: ExternalRuntimeKind, env: E2eEnv, log: Arc<CliSessionLog>) -> Self {
        Self {
            runtime,
            env,
            log,
            timeout: CLI_TIMEOUT,
        }
    }

    async fn run(&self, request: &ExternalSessionRequest) -> ExternalSessionResult {
        let prompt = match session_prompt(request) {
            Ok(prompt) => prompt,
            Err(error) => {
                return ExternalSessionResult::Failed {
                    session: request.session.clone(),
                    error: *error,
                    observations: Vec::new(),
                };
            }
        };
        let marker = runtime_marker(&self.runtime);
        let guarded_prompt = runtime_guarded_prompt(&self.runtime, marker, &prompt);

        let output = match self.runtime {
            ExternalRuntimeKind::ClaudeCode => {
                run_claude_code(
                    &self.env,
                    request.worktree.path(),
                    &guarded_prompt,
                    self.timeout,
                )
                .await
            }
            ExternalRuntimeKind::Codex => {
                run_codex(
                    &self.env,
                    request.worktree.path(),
                    &guarded_prompt,
                    self.timeout,
                )
                .await
            }
            ref other => Err(Box::new(ExternalAgentError::Launch {
                runtime: other.clone(),
                detail: "this e2e handler only supports Claude Code and Codex".to_owned(),
            })),
        };

        match output {
            Ok(summary) => {
                let summary = normalize_runtime_summary(&self.runtime, summary.trim());
                let session = ExternalSessionRef {
                    runtime: self.runtime.clone(),
                    session_id: Some(format!(
                        "{}-{}",
                        runtime_label(&self.runtime),
                        self.log.next_sequence()
                    )),
                    transcript_ref: None,
                    resume_token: None,
                    last_event_seq: Some(3),
                };
                self.log.record(self.runtime.clone(), summary.clone());
                ExternalSessionResult::Completed {
                    session: session.clone(),
                    output: ExternalAgentOutput {
                        summary: summary.clone(),
                        artifacts: vec![ExternalArtifactRef {
                            kind: ExternalArtifactKind::Other,
                            summary: format!(
                                "{} read-only e2e transcript",
                                runtime_label(&self.runtime)
                            ),
                            path: None,
                            reference: session.session_id.clone(),
                        }],
                        usage: None,
                        cost_micros: None,
                    },
                    observations: vec![
                        ExternalAgentEvent::SessionStarted {
                            session_id: session.session_id,
                        },
                        ExternalAgentEvent::TextDelta {
                            text: bounded(&summary, 1_000),
                        },
                        ExternalAgentEvent::SessionCompleted,
                    ],
                }
            }
            Err(error) => ExternalSessionResult::Failed {
                session: request.session.clone(),
                error: *error,
                observations: Vec::new(),
            },
        }
    }
}

#[async_trait]
impl ExternalSessionHandler for CliSessionHandler {
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        _ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::ExternalSession(Box::new(self.run(request).await))
    }
}

#[derive(Clone, Debug)]
struct CliSessionRecord {
    runtime: ExternalRuntimeKind,
    summary: String,
}

#[derive(Default, Debug)]
struct CliSessionLog {
    records: Mutex<Vec<CliSessionRecord>>,
}

impl CliSessionLog {
    fn next_sequence(&self) -> usize {
        self.records.lock().expect("CLI log").len() + 1
    }

    fn record(&self, runtime: ExternalRuntimeKind, summary: String) {
        self.records
            .lock()
            .expect("CLI log")
            .push(CliSessionRecord { runtime, summary });
    }

    fn records(&self) -> Vec<CliSessionRecord> {
        self.records.lock().expect("CLI log").clone()
    }

    fn latest_summary(&self, runtime: &ExternalRuntimeKind) -> Option<String> {
        self.records
            .lock()
            .expect("CLI log")
            .iter()
            .rev()
            .find(|record| &record.runtime == runtime)
            .map(|record| record.summary.clone())
    }

    fn contains_marker(&self, runtime: &ExternalRuntimeKind, marker: &str) -> bool {
        self.records()
            .iter()
            .any(|record| &record.runtime == runtime && record.summary.contains(marker))
    }
}

fn session_prompt(request: &ExternalSessionRequest) -> Result<String, Box<ExternalAgentError>> {
    match &request.input {
        ExternalSessionInput::Start { prompt } => Ok(prompt.clone()),
        ExternalSessionInput::Continue { message } => Ok(message.clone()),
        ExternalSessionInput::RespondInteraction { .. } => {
            Err(Box::new(ExternalAgentError::Protocol {
                detail: "real e2e CLI handler does not expect permission pauses".to_owned(),
            }))
        }
        ExternalSessionInput::Shutdown => Err(Box::new(ExternalAgentError::Protocol {
            detail: "real e2e CLI handler is not used for shutdown".to_owned(),
        })),
    }
}

fn runtime_guarded_prompt(runtime: &ExternalRuntimeKind, marker: &str, brief: &str) -> String {
    format!(
        "You are running as an agent-lib ExternalAgentMachine e2e child backed by {runtime:?}.\n\
         Operate in read-only mode. Do not edit files. Do not run long commands. \
         Inspect only the repository context needed to answer.\n\
         Task brief:\n{brief}\n\n\
         Final response requirements:\n\
         - Return exactly one short paragraph.\n\
         - The paragraph must start with `{marker}:`.\n\
         - Mention one concrete repository file or module you inspected."
    )
}

async fn run_claude_code(
    env: &E2eEnv,
    worktree: &Path,
    prompt: &str,
    timeout_for_run: Duration,
) -> Result<String, Box<ExternalAgentError>> {
    let mut command = Command::new("claude");
    command
        .arg("--print")
        .arg("--output-format")
        .arg("json")
        .arg("--permission-mode")
        .arg("plan")
        .arg("--no-session-persistence")
        .arg("--max-budget-usd")
        .arg(
            env.get("CLAUDE_CODE_E2E_MAX_BUDGET_USD")
                .unwrap_or_else(|| "0.25".to_owned()),
        );
    if let Some(model) = env.get("CLAUDE_CODE_MODEL") {
        command.arg("--model").arg(model);
    }
    command.arg(prompt);

    let output = run_command(
        command,
        env,
        worktree,
        timeout_for_run,
        ExternalRuntimeKind::ClaudeCode,
    )
    .await?;
    Ok(extract_claude_text(&output.stdout).unwrap_or_else(|| output.stdout.trim().to_owned()))
}

async fn run_codex(
    env: &E2eEnv,
    worktree: &Path,
    prompt: &str,
    timeout_for_run: Duration,
) -> Result<String, Box<ExternalAgentError>> {
    let output_file = env::temp_dir().join(format!(
        "agent-lib-codex-e2e-{}-{}.txt",
        std::process::id(),
        monotonic_temp_suffix()
    ));

    let mut command = Command::new("codex");
    command
        .arg("-s")
        .arg("read-only")
        .arg("-a")
        .arg("never")
        .arg("exec")
        .arg("--ephemeral")
        .arg("--skip-git-repo-check")
        .arg("--color")
        .arg("never")
        .arg("-C")
        .arg(worktree)
        .arg("-o")
        .arg(&output_file);
    if let Some(model) = env.get("CODEX_E2E_MODEL") {
        command.arg("--model").arg(model);
    }
    command.arg(prompt);

    let output = run_command(
        command,
        env,
        worktree,
        timeout_for_run,
        ExternalRuntimeKind::Codex,
    )
    .await?;
    let final_message = tokio::fs::read_to_string(&output_file).await.ok();
    let _ = tokio::fs::remove_file(&output_file).await;

    Ok(final_message
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| output.stdout.trim().to_owned()))
}

#[derive(Debug)]
struct CommandOutput {
    stdout: String,
}

async fn run_command(
    mut command: Command,
    env: &E2eEnv,
    worktree: &Path,
    timeout_for_run: Duration,
    runtime: ExternalRuntimeKind,
) -> Result<CommandOutput, Box<ExternalAgentError>> {
    command
        .current_dir(worktree)
        .envs(env.command_envs())
        .env("NO_COLOR", "1")
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = command.spawn().map_err(|err| {
        Box::new(ExternalAgentError::Launch {
            runtime: runtime.clone(),
            detail: err.to_string(),
        })
    })?;
    let output = timeout(timeout_for_run, child.wait_with_output())
        .await
        .map_err(|_| {
            Box::new(ExternalAgentError::LimitExceeded {
                limit: format!(
                    "{} CLI exceeded {:?}",
                    runtime_label(&runtime),
                    timeout_for_run
                ),
            })
        })?
        .map_err(|err| {
            Box::new(ExternalAgentError::SessionLost {
                session: None,
                detail: err.to_string(),
            })
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(Box::new(ExternalAgentError::Runtime {
            code: output.status.code().map(|code| code.to_string()),
            message: format!(
                "{} exited unsuccessfully; stdout tail: {}; stderr tail: {}",
                runtime_label(&runtime),
                tail(&stdout, 1_000),
                tail(&stderr, 1_000)
            ),
        }));
    }
    Ok(CommandOutput { stdout })
}

fn extract_claude_text(stdout: &str) -> Option<String> {
    let value: Value = serde_json::from_str(stdout).ok()?;
    for key in ["result", "response", "text", "content"] {
        if let Some(text) = value.get(key).and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            return Some(text.to_owned());
        }
    }
    recursive_string(&value)
}

fn recursive_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Array(items) => items.iter().find_map(recursive_string),
        Value::Object(object) => object.values().find_map(recursive_string),
        _ => None,
    }
}

fn monotonic_temp_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

fn runtime_marker(runtime: &ExternalRuntimeKind) -> &'static str {
    match runtime {
        ExternalRuntimeKind::ClaudeCode => CLAUDE_MARKER,
        ExternalRuntimeKind::Codex => CODEX_MARKER,
        ExternalRuntimeKind::OpenCode | ExternalRuntimeKind::Custom(_) => "EXTERNAL_E2E_OK",
    }
}

fn normalize_runtime_summary(runtime: &ExternalRuntimeKind, text: &str) -> String {
    let marker = runtime_marker(runtime);
    let text = bounded(text, 8_000);
    if text.contains(marker) {
        text
    } else {
        format!("{marker}: {text}")
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

fn tail(text: &str, max_chars: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(max_chars);
    chars[start..].iter().collect()
}

fn bounded(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

// ----- external machine fixtures ------------------------------------------

fn external_policy() -> ExternalSessionPolicy {
    ExternalSessionPolicy {
        permission_mode: ExternalPermissionMode::Plan,
        isolation: WorktreeIsolation::Shared,
        max_turns: Some(2),
        stream_events: ExternalStreamPolicy::Buffered,
    }
}

fn external_machine(
    ids: &SeqIds,
    runtime: ExternalRuntimeKind,
    agent_id: agent_lib::agent::AgentId,
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
            ConversationConfig::new(Some("External e2e conversation.".to_owned())),
        ),
    );
    ExternalAgentMachine::new(state, Arc::new(ids.clone()))
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

// ----- DeepSeek coordinator machine ---------------------------------------

#[derive(Clone, Debug)]
struct CoordinatorPlan {
    claude_brief: String,
    codex_brief: String,
}

#[derive(Debug)]
enum CoordinatorStage {
    Idle,
    AwaitingPlan {
        requirement: agent_lib::agent::RequirementId,
        step_id: StepId,
        task: String,
    },
    AwaitingClaude {
        requirement: agent_lib::agent::RequirementId,
        step_id: StepId,
        plan: CoordinatorPlan,
    },
    AwaitingCodex {
        requirement: agent_lib::agent::RequirementId,
        step_id: StepId,
        claude_summary: String,
    },
    AwaitingFinal {
        requirement: agent_lib::agent::RequirementId,
    },
    Done,
    Error,
}

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
                "JSON_OBJECT. You are a deterministic coordinator for an agent-lib e2e test. \
                 Return one JSON object with string fields `claude_brief` and `codex_brief`. \
                 The Claude brief must ask Claude Code to inspect ExternalAgentMachine basics. \
                 The Codex brief must ask Codex to inspect subagent composition. \
                 Do not include markdown. The user's task is: {task}"
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

// ----- real subagent spawner ----------------------------------------------

struct RealExternalSubagentSpawner {
    ids: SeqIds,
    env: E2eEnv,
    worktree: PathBuf,
    claude_spec: AgentSpecRef,
    codex_spec: AgentSpecRef,
    log: Arc<CliSessionLog>,
    spawned: Mutex<Vec<ExternalRuntimeKind>>,
    pending_summaries: Mutex<VecDeque<ExternalRuntimeKind>>,
}

impl RealExternalSubagentSpawner {
    fn new(
        ids: SeqIds,
        env: E2eEnv,
        worktree: PathBuf,
        claude_spec: AgentSpecRef,
        codex_spec: AgentSpecRef,
        log: Arc<CliSessionLog>,
    ) -> Self {
        Self {
            ids,
            env,
            worktree,
            claude_spec,
            codex_spec,
            log,
            spawned: Mutex::new(Vec::new()),
            pending_summaries: Mutex::new(VecDeque::new()),
        }
    }

    fn spawned(&self) -> Vec<ExternalRuntimeKind> {
        self.spawned.lock().expect("spawn log").clone()
    }
}

impl SubagentSpawner for RealExternalSubagentSpawner {
    fn child_ids(&self, spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        Ok((
            self.ids.run_id(),
            self.ids.trace_node(&format!(
                "real-{}",
                runtime_label(&runtime_for_spec(
                    spec_ref,
                    self.claude_spec,
                    self.codex_spec,
                )?)
            )),
        ))
    }

    fn spawn(
        &self,
        spec_ref: &AgentSpecRef,
        brief: &Interaction,
        _result_schema: Option<&Value>,
    ) -> Result<SpawnedChild, AgentError> {
        let runtime = runtime_for_spec(spec_ref, self.claude_spec, self.codex_spec)?;
        let prompt = brief_text(brief)?;
        self.spawned
            .lock()
            .expect("spawn log")
            .push(runtime.clone());
        self.pending_summaries
            .lock()
            .expect("pending summaries")
            .push_back(runtime.clone());

        let child_ids = self.ids.fork(runtime_label(&runtime));
        let machine = external_machine(&child_ids, runtime.clone(), spec_ref.0, &self.worktree);
        let handler = CliSessionHandler::new(runtime, self.env.clone(), Arc::clone(&self.log));
        let scope = TestScope::builder().external(Arc::new(handler)).build();
        Ok(SpawnedChild {
            machine: Box::new(machine),
            scope: Box::new(scope),
            opening: user_input(&child_ids, &prompt),
        })
    }

    fn summarize(&self, _done: &agent_lib::agent::TurnDone) -> SubagentOutput {
        let runtime = self
            .pending_summaries
            .lock()
            .expect("pending summaries")
            .pop_front();
        let summary = runtime
            .and_then(|runtime| self.log.latest_summary(&runtime))
            .unwrap_or_else(|| "external subagent completed without a captured summary".to_owned());
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
            "unknown real external subagent spec {}",
            spec_ref.0
        )))
    }
}

fn brief_text(brief: &Interaction) -> Result<String, AgentError> {
    match brief.kind() {
        InteractionKind::Question { prompt } => Ok(prompt.clone()),
        other => Err(AgentError::Other(format!(
            "real e2e subagent brief must be a question, got {:?}",
            other.tag()
        ))),
    }
}

// ----- tests ---------------------------------------------------------------

#[tokio::test]
#[ignore = "requires local Claude Code auth/runtime; spawns `claude`"]
async fn external_agent_machine_claude_code_real_cli_start_completes() {
    if !command_available("claude").await {
        eprintln!("skipping: `claude` is not available on PATH");
        return;
    }

    let env = E2eEnv::load();
    let ids = SeqIds::new();
    let log = Arc::new(CliSessionLog::default());
    let mut machine = external_machine(
        &ids,
        ExternalRuntimeKind::ClaudeCode,
        ids.agent_id(),
        &repo_root(),
    );
    let scope = TestScope::builder()
        .external(Arc::new(CliSessionHandler::new(
            ExternalRuntimeKind::ClaudeCode,
            env,
            Arc::clone(&log),
        )))
        .build();
    let ctx = root_context(&ids);

    let done = timeout(
        DIRECT_TIMEOUT,
        drain(
            &mut machine,
            user_input(
                &ids,
                "Verify the external agent machine start/completed path at a high level.",
            ),
            &scope,
            None,
            &ctx,
        ),
    )
    .await
    .expect("Claude Code e2e exceeded its wall-clock limit")
    .expect("Claude Code e2e drain failed");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text_contains(CLAUDE_MARKER);
    assert!(log.contains_marker(&ExternalRuntimeKind::ClaudeCode, CLAUDE_MARKER));
}

#[tokio::test]
#[ignore = "requires local Codex auth/runtime; spawns `codex exec`"]
async fn external_agent_machine_codex_real_cli_start_completes() {
    if !command_available("codex").await {
        eprintln!("skipping: `codex` is not available on PATH");
        return;
    }

    let env = E2eEnv::load();
    let ids = SeqIds::new();
    let log = Arc::new(CliSessionLog::default());
    let mut machine = external_machine(
        &ids,
        ExternalRuntimeKind::Codex,
        ids.agent_id(),
        &repo_root(),
    );
    let scope = TestScope::builder()
        .external(Arc::new(CliSessionHandler::new(
            ExternalRuntimeKind::Codex,
            env,
            Arc::clone(&log),
        )))
        .build();
    let ctx = root_context(&ids);

    let done = timeout(
        DIRECT_TIMEOUT,
        drain(
            &mut machine,
            user_input(
                &ids,
                "Verify the external agent machine start/completed path at a high level.",
            ),
            &scope,
            None,
            &ctx,
        ),
    )
    .await
    .expect("Codex e2e exceeded its wall-clock limit")
    .expect("Codex e2e drain failed");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .pending_none()
        .last_assistant_text_contains(CODEX_MARKER);
    assert!(log.contains_marker(&ExternalRuntimeKind::Codex, CODEX_MARKER));
}

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY plus local Claude Code and Codex runtimes"]
async fn deepseek_coordinator_runs_claude_code_and_codex_external_subagents() {
    let env = E2eEnv::load();
    let Some(deepseek) = DeepSeekConfig::from_env(&env) else {
        return;
    };
    if !command_available("claude").await {
        eprintln!("skipping: `claude` is not available on PATH");
        return;
    }
    if !command_available("codex").await {
        eprintln!("skipping: `codex` is not available on PATH");
        return;
    }

    let ids = SeqIds::new();
    let claude_spec = AgentSpecRef(ids.agent_id());
    let codex_spec = AgentSpecRef(ids.agent_id());
    let deepseek_log = Arc::new(DeepSeekCallLog::default());
    let cli_log = Arc::new(CliSessionLog::default());
    let spawner = Arc::new(RealExternalSubagentSpawner::new(
        ids.clone(),
        env,
        repo_root(),
        claude_spec,
        codex_spec,
        Arc::clone(&cli_log),
    ));
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

    let done = timeout(
        COORDINATOR_TIMEOUT,
        drain(
            &mut coordinator,
            user_input(
                &ids,
                "Plan and execute a read-only two-worker e2e check over ExternalAgentMachine.",
            ),
            &scope,
            None,
            &ctx,
        ),
    )
    .await
    .expect("DeepSeek multi-agent e2e exceeded its wall-clock limit")
    .expect("DeepSeek multi-agent e2e drain failed");

    assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(
        spawner.spawned(),
        vec![ExternalRuntimeKind::ClaudeCode, ExternalRuntimeKind::Codex]
    );
    assert!(
        cli_log.contains_marker(&ExternalRuntimeKind::ClaudeCode, CLAUDE_MARKER),
        "Claude Code child summary did not contain {CLAUDE_MARKER}"
    );
    assert!(
        cli_log.contains_marker(&ExternalRuntimeKind::Codex, CODEX_MARKER),
        "Codex child summary did not contain {CODEX_MARKER}"
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
}
