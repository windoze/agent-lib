//! Phase 0 low-fidelity spike (PLAN.md Milestone 1 / design doc §3.1, §13):
//! wrap an external coding-agent CLI as an [`LlmHandler`] to observe *how* an
//! out-of-process runtime plugs into the existing effect boundary before any
//! dedicated external-session DTO/machine is built.
//!
//! This spike is intentionally a throwaway probe, not a stable API:
//!
//! - It lives under `examples/` and touches **nothing** in `src/`.
//! - The "external CLI" is a `sh -c` stub script that streams a few text
//!   chunks; no real Claude Code / Codex / OpenCode process, network, or
//!   credentials are involved, so it runs fully offline.
//! - It folds the subprocess stdout into a [`Response`] and returns it through
//!   `RequirementResult::Llm(Ok(..))`, exactly as a real `LlmHandler` would.
//!
//! It exercises the three behaviors called out by task M1-1:
//!
//! 1. **Normal start + return text** — spawn the CLI, read to EOF, fold text.
//! 2. **Streaming increments** — print each stdout line as it arrives.
//! 3. **Cancellation / kill** — a long-running child is killed as soon as
//!    [`RunContext::is_cancelled`] flips, and the handler returns an error.
//!
//! Run it with:
//!
//! ```text
//! cargo run --example external_cli_spike
//! ```

use agent_lib::agent::{
    BudgetLimits, LlmHandler, LlmStepMode, RequirementResult, RunContext, RunId, TraceNodeId,
};
use agent_lib::client::{ChatRequest, ClientError, Response};
use agent_lib::model::{
    content::ContentBlock,
    message::{Message, Role},
    normalized::StopReason,
    usage::Usage,
};
use async_trait::async_trait;
use serde_json::Map;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// A low-fidelity [`LlmHandler`] that drives an external CLI subprocess.
///
/// The handler spawns `program` with `args`, forwards the request's prompt to
/// the child through the `SPIKE_PROMPT` environment variable, streams its stdout
/// line-by-line, and folds the collected text into a single [`Response`]. It
/// polls [`RunContext::is_cancelled`] while reading so a cancelled run kills the
/// child instead of blocking on it.
struct ExternalCliLlmHandler {
    /// Program to execute as the stand-in external coding-agent CLI.
    program: String,
    /// Arguments passed to `program`.
    args: Vec<String>,
}

impl ExternalCliLlmHandler {
    /// Builds a handler that shells out to the given `sh -c` stub script.
    fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
        }
    }

    /// Runs the child process and folds its streamed stdout into a [`Response`].
    ///
    /// Returns `Err` when the run is cancelled (after killing the child) or when
    /// the subprocess cannot be spawned or its stdout cannot be read.
    async fn run(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        ctx: &RunContext,
    ) -> Result<Response, ClientError> {
        let prompt = last_user_text(request);

        let mut child = Command::new(&self.program)
            .args(&self.args)
            .env("SPIKE_PROMPT", &prompt)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| ClientError::Other(format!("failed to spawn external CLI: {err}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::Other("external CLI stdout was not captured".to_owned()))?;

        // Decode stdout on a dedicated task and forward each line over a channel.
        // `mpsc::Receiver::recv` is cancellation-safe, so the polling loop below
        // can race line delivery against cancellation without losing buffered
        // bytes mid-line.
        let (tx, mut rx) = mpsc::channel::<std::io::Result<Option<String>>>(16);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if tx.send(Ok(Some(line))).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(Ok(None)).await;
                        break;
                    }
                    Err(err) => {
                        let _ = tx.send(Err(err)).await;
                        break;
                    }
                }
            }
        });

        let mut buffer = String::new();
        let mut chunks = 0_u32;
        loop {
            tokio::select! {
                received = rx.recv() => match received {
                    Some(Ok(Some(chunk))) => {
                        chunks += 1;
                        if matches!(mode, LlmStepMode::Streaming) {
                            eprintln!("    [stream +{chunks}] {chunk}");
                        }
                        buffer.push_str(&chunk);
                        buffer.push('\n');
                    }
                    // EOF from the reader task, or the reader task ended.
                    Some(Ok(None)) | None => break,
                    Some(Err(err)) => {
                        return Err(ClientError::Protocol(format!(
                            "external CLI stdout read failed: {err}"
                        )));
                    }
                },
                // Poll cooperatively so a cancelled run does not wait on a child
                // that may never exit on its own.
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    if ctx.is_cancelled() {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        return Err(ClientError::Other(format!(
                            "run cancelled: killed external CLI after {chunks} streamed chunk(s)"
                        )));
                    }
                }
            }
        }

        // Reap the child so it does not linger as a zombie.
        let _ = child.wait().await;

        Ok(fold_response(buffer.trim_end().to_owned(), chunks))
    }
}

#[async_trait]
impl LlmHandler for ExternalCliLlmHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::Llm(self.run(request, mode, ctx).await)
    }
}

/// Folds collected subprocess text into a normal end-of-turn [`Response`].
///
/// Token usage is a coarse word-count estimate; the spike only needs a shape
/// compatible with the real handler contract, not accurate accounting.
fn fold_response(text: String, chunks: u32) -> Response {
    let output_tokens = text.split_whitespace().count() as u32;
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text,
                extra: Map::new(),
            }],
        },
        usage: Usage {
            input: 0,
            output: output_tokens,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            total: Some(output_tokens + chunks),
            extra: Map::new(),
        },
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

/// Extracts the most recent user-authored text from the request, if any.
fn last_user_text(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|message| matches!(message.role, Role::User))
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// Builds a minimal single-user-message [`ChatRequest`] for the spike.
fn spike_request(prompt: &str, stream: bool) -> ChatRequest {
    ChatRequest {
        model: "external-cli-stub".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: prompt.to_owned(),
                extra: Map::new(),
            }],
        }],
        tools: Vec::new(),
        system: None,
        max_tokens: 256,
        temperature: None,
        stream,
        provider_extras: None,
    }
}

/// Builds `sh -c` arguments for a stub CLI that streams `chunks` text lines,
/// sleeping `delay` between each so cancellation has an observable window.
///
/// The script echoes the forwarded `SPIKE_PROMPT` on the first line, then emits
/// numbered chunks. Passing the counts as positional args (`$1`, `$2`) keeps the
/// script body free of interpolation.
fn stub_args(chunks: u32, delay: Duration) -> Vec<String> {
    let script = r#"
prompt="${SPIKE_PROMPT:-<no prompt>}"
printf 'echo: %s\n' "$prompt"
i=1
while [ "$i" -le "$1" ]; do
    printf 'chunk %d of %d\n' "$i" "$1"
    i=$((i + 1))
    sleep "$2"
done
"#;
    vec![
        "-c".to_owned(),
        script.to_owned(),
        "external-cli-stub".to_owned(),
        chunks.to_string(),
        format!("{:.3}", delay.as_secs_f64()),
    ]
}

/// Creates a fresh root [`RunContext`] for one spike scenario.
///
/// Ids are deterministic (the library never mints its own); the spike only
/// needs an unbudgeted, uncancelled context whose cancellation token it can
/// trip manually.
fn spike_context(seed: u128) -> RunContext {
    RunContext::new_root(
        RunId::new(uuid::Uuid::from_u128(seed)),
        BudgetLimits::unbounded(),
        TraceNodeId::new(format!("external-cli-spike-{seed}")),
    )
}

/// Prints the assistant text and usage carried by a folded [`RequirementResult`].
fn report(label: &str, result: &RequirementResult) {
    match result {
        RequirementResult::Llm(Ok(response)) => {
            let text = response
                .message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            println!(
                "[{label}] Ok, folded {} output token(s):",
                response.usage.output
            );
            for line in text.lines() {
                println!("    | {line}");
            }
        }
        RequirementResult::Llm(Err(err)) => {
            println!("[{label}] Err: {err}");
        }
        _ => println!("[{label}] unexpected non-LLM result"),
    }
}

/// Behavior 1: spawn the stub, read to EOF, and fold the text non-streaming.
async fn scenario_normal(handler: &ExternalCliLlmHandler) {
    println!("\n== scenario 1: normal start + return text (non-streaming) ==");
    let ctx = spike_context(1);
    let request = spike_request("summarize the repo", false);
    let result = handler
        .fulfill(&request, LlmStepMode::NonStreaming, &ctx)
        .await;
    report("normal", &result);
}

/// Behavior 2: the same run under streaming mode, printing each increment.
async fn scenario_streaming(handler: &ExternalCliLlmHandler) {
    println!("\n== scenario 2: streaming incremental read ==");
    let ctx = spike_context(2);
    let request = spike_request("stream the plan", true);
    let result = handler
        .fulfill(&request, LlmStepMode::Streaming, &ctx)
        .await;
    report("streaming", &result);
}

/// Behavior 3: cancel mid-stream and confirm the child is killed.
async fn scenario_cancel() {
    println!("\n== scenario 3: cancel / kill mid-stream ==");
    // A long-running child (many chunks, real sleeps) that would never finish
    // within the cancellation window on its own.
    let handler = ExternalCliLlmHandler::new("sh", stub_args(1_000, Duration::from_millis(50)));
    let ctx = spike_context(3);
    let request = spike_request("run a long task", true);

    // Trip the run's cancellation token shortly after the child starts.
    let token = ctx.cancellation().clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        eprintln!("    [control] cancelling run");
        token.cancel();
    });

    let result = handler
        .fulfill(&request, LlmStepMode::Streaming, &ctx)
        .await;
    let _ = canceller.await;
    report("cancel", &result);
}

/// Runs the three spike scenarios in sequence.
#[tokio::main]
async fn main() {
    println!("External CLI low-fidelity spike (Phase 0). Using a `sh -c` stub, no real runtime.");

    // A short, fast stub for the successful behaviors.
    let handler = ExternalCliLlmHandler::new("sh", stub_args(3, Duration::from_millis(20)));
    scenario_normal(&handler).await;
    scenario_streaming(&handler).await;
    scenario_cancel().await;

    println!("\nspike complete: observed start, streaming, and cancel/kill behaviors.");
}
