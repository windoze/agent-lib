//! Private `codex exec --json` JSONL decoder for the managed Codex adapter (M7-2).
//!
//! `codex exec --json` streams newline-delimited JSON `ThreadEvent`s: a
//! `thread.started` line, `turn.started` / `turn.completed` / `turn.failed`
//! turn-boundary lines, `item.started` / `item.updated` / `item.completed` lines
//! wrapping a typed thread *item* (an agent message, a command execution, a file
//! change, an MCP tool call, …), and a top-level `error` line for transient
//! runtime notices. That wire schema is **adapter-private**: this module decodes
//! it entirely through defensive [`serde_json::Value`] navigation and never
//! re-exports a raw frame type, so Codex's private JSON shape does not leak into
//! `agent-lib`'s stable API (design §12, 非目标 §3).
//!
//! What it *does* expose is provider-neutral: [`CodexStreamDecoder::push_line`]
//! turns each frame into sequenced [`ExternalObservedEvent`] observations and,
//! when a turn settles, one [`CodexDecision`]. The live
//! [`ExternalRuntimeSession`](crate::agent::external::ExternalRuntimeSession) that
//! wraps this decoder into start/resume/advance lands in M7-3; this task freezes
//! only the decode.
//!
//! # Why a turn only ever completes or fails
//!
//! Unlike Claude Code's `stream-json`, `codex exec --json` runs **autonomously**:
//! it executes its own tools (including MCP tool calls, which it dispatches and
//! reports the result of) and it resolves approvals against the sandbox/approval
//! policy the host pre-configured on the command line (design §12 — the approval
//! policy is a launch flag, not a mid-turn prompt). The exec JSONL stream
//! therefore carries **no** host-pausable tool-call or permission frame: a turn
//! only ever settles on [`Completed`](CodexDecision::Completed) (`turn.completed`)
//! or [`Failed`](CodexDecision::Failed) (`turn.failed`). A gated action the policy
//! refused surfaces as a `command_execution` with a `declined` status, which the
//! decoder reports as an informational
//! [`PermissionRequested`](ExternalAgentEvent::PermissionRequested) observation
//! (there is nothing for the host to answer — the runtime already decided).
//!
//! # Frame mapping
//!
//! | Codex frame | observation / decision |
//! |---|---|
//! | `thread.started` | [`SessionStarted`](ExternalAgentEvent::SessionStarted) |
//! | `item.*` `agent_message` | [`TextDelta`](ExternalAgentEvent::TextDelta) |
//! | `item.started` `command_execution` | [`CommandStarted`](ExternalAgentEvent::CommandStarted) |
//! | `item.completed` `command_execution` (ok/failed) | [`CommandFinished`](ExternalAgentEvent::CommandFinished) |
//! | `item.completed` `command_execution` (declined) | [`PermissionRequested`](ExternalAgentEvent::PermissionRequested) |
//! | `item.completed` `file_change` | [`FilePatch`](ExternalAgentEvent::FilePatch) per change |
//! | `item.started` `mcp_tool_call` | [`ToolStarted`](ExternalAgentEvent::ToolStarted) |
//! | `item.completed` `mcp_tool_call` | [`ToolFinished`](ExternalAgentEvent::ToolFinished) |
//! | `turn.completed` | [`SessionCompleted`](ExternalAgentEvent::SessionCompleted) + [`Completed`](CodexDecision::Completed) |
//! | `turn.failed` | [`Failed`](CodexDecision::Failed) |
//!
//! # Tolerance policy (stable)
//!
//! The decoder is deliberately forgiving of forward-compatible drift but strict
//! about corruption, so a scheduler sees a stable classification:
//!
//! - a blank line, a `turn.started` frame, a top-level `error` notice, an
//!   `item.updated` progress frame, an **unknown** top-level `type`, and an
//!   unknown or absent item `type` (`reasoning`, `web_search`, `todo_list`,
//!   `collab_tool_call`, an error item, …) are *tolerated* (no observation, no
//!   error);
//! - a line that is not valid JSON, not a JSON object, missing a string `type`,
//!   a `thread.started` without a `thread_id`, or an `item.*` frame whose `item`
//!   is absent or not an object is a real protocol violation and returns
//!   [`ExternalAgentError::Protocol`].
//!
//! Every diagnostic is a fixed string; no prompt text, command line, tool output,
//! or credential is ever folded into an error message.

// The decoder's fallible helpers return the external adapter's canonical
// `ExternalAgentError`, matching the unboxed error contract used across
// `adapter.rs`, `registry.rs`, `probe.rs`, and the public `ExternalSessionResult`
// surface. That enum is intentionally not boxed there, so `result_large_err`
// (which only fires here because these sync helpers have small `Ok` types) would
// force a signature style inconsistent with the rest of the module.
#![allow(clippy::result_large_err)]

use serde_json::{Map, Value};

use crate::agent::external::{
    ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput, ExternalObservedEvent,
};
use crate::model::tool::ToolStatus;
use crate::model::usage::Usage;

/// Host-supplied context the decoder needs while turning `codex exec --json`
/// frames into observations.
///
/// Codex reports a `command_execution` item without the directory it ran in, so
/// the host threads in the worktree it launched `codex exec` under; the decoder
/// stamps it onto every [`CommandStarted`](ExternalAgentEvent::CommandStarted)
/// observation. It is never taken from model output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodexDecodeContext {
    cwd: String,
}

impl CodexDecodeContext {
    /// Creates a decode context with an unknown working directory (the empty
    /// string is stamped onto command observations until one is set).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the working directory Codex runs commands in (typically the agent's
    /// worktree), recorded on decoded command observations.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = cwd.into();
        self
    }
}

/// The control-flow transfer a decoded Codex turn settles on.
///
/// This is the decoder's provider-neutral counterpart of the terminal payload of
/// a [`RuntimeDecisionPoint`](crate::agent::external::RuntimeDecisionPoint): the
/// live session (M7-3) attaches the resumable
/// [`ExternalSessionRef`](crate::agent::external::ExternalSessionRef) and drained
/// observations around it. There is deliberately no paused arm — `codex exec
/// --json` runs autonomously and never hands a tool call or an approval back to
/// the host mid-turn (see the module docs), so a turn only ever completes or
/// fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexDecision {
    /// The turn produced terminal output (`turn.completed`).
    Completed {
        /// Terminal output decoded from the turn and its last agent message.
        output: ExternalAgentOutput,
    },
    /// The turn failed; the runtime reported `turn.failed`.
    Failed {
        /// Classified failure reason.
        error: ExternalAgentError,
    },
}

/// Stateful decoder turning `codex exec --json` frames into sequenced
/// observations and per-turn [`CodexDecision`]s.
///
/// One decoder spans a whole session: [`seq`](ExternalObservedEvent::seq) is
/// assigned monotonically across turns so the machine's replay dedup stays valid
/// across resumes (design §5.5). Feed each raw frame line to
/// [`push_line`](Self::push_line); drain the observations buffered before a
/// decision with [`take_observations`](Self::take_observations).
#[derive(Debug)]
pub struct CodexStreamDecoder {
    context: CodexDecodeContext,
    next_seq: u64,
    session_id: Option<String>,
    last_message: Option<String>,
    pending: Vec<ExternalObservedEvent>,
}

impl CodexStreamDecoder {
    /// Creates a decoder for a fresh session, binding the host decode context.
    #[must_use]
    pub fn new(context: CodexDecodeContext) -> Self {
        Self {
            context,
            next_seq: 0,
            session_id: None,
            last_message: None,
            pending: Vec::new(),
        }
    }

    /// Seeds the `seq` line at `next_seq`, for a session resumed across
    /// processes.
    ///
    /// The machine's replay dedup keeps only observations with `seq` greater
    /// than the persisted [`ExternalSessionRef::last_event_seq`] high-water
    /// mark, so a resumed session must continue the seq line where the previous
    /// process left off instead of restarting at 0 — otherwise every
    /// post-resume observation would be silently dropped as a false duplicate
    /// (design §5.5).
    ///
    /// [`ExternalSessionRef::last_event_seq`]: crate::agent::external::ExternalSessionRef::last_event_seq
    #[must_use]
    pub fn with_next_seq(mut self, next_seq: u64) -> Self {
        self.next_seq = next_seq;
        self
    }

    /// Returns the runtime-assigned thread id, once a `thread.started` frame has
    /// reported one.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Drains the observations buffered since the last drain, transferring
    /// ownership to the caller and leaving the running `seq` untouched.
    #[must_use]
    pub fn take_observations(&mut self) -> Vec<ExternalObservedEvent> {
        std::mem::take(&mut self.pending)
    }

    /// Decodes one raw `codex exec --json` frame line.
    ///
    /// Returns `Ok(Some(decision))` when the frame settles the current turn on a
    /// control transfer, `Ok(None)` when it only buffered observations (or was a
    /// tolerated frame), and `Err` when the frame is corrupt.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Protocol`] for a line that is not valid
    /// JSON, is not a JSON object, is missing a string `type`, is a
    /// `thread.started` without a `thread_id`, or is an `item.*` frame whose
    /// `item` is absent or not an object.
    pub fn push_line(&mut self, line: &str) -> Result<Option<CodexDecision>, ExternalAgentError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let value: Value = serde_json::from_str(trimmed)
            .map_err(|error| protocol(format!("invalid codex exec json frame: {error}")))?;
        let Some(frame) = value.as_object() else {
            return Err(protocol("codex exec json frame is not a JSON object"));
        };
        let Some(frame_type) = frame.get("type").and_then(Value::as_str) else {
            return Err(protocol("codex exec json frame is missing a string `type`"));
        };

        let decision = match frame_type {
            "thread.started" => {
                self.handle_thread_started(frame)?;
                None
            }
            "item.started" => {
                self.handle_item(frame, ItemPhase::Started)?;
                None
            }
            "item.updated" => {
                // Progress updates carry no terminal fact the started/completed
                // pair does not already provide; tolerate them.
                self.require_item(frame)?;
                None
            }
            "item.completed" => {
                self.handle_item(frame, ItemPhase::Completed)?;
                None
            }
            "turn.completed" => Some(self.handle_turn_completed(frame)),
            "turn.failed" => Some(self.handle_turn_failed(frame)),
            // `turn.started`, a top-level transient `error` notice, and any
            // unknown future frame type are tolerated: the turn boundary frames
            // already carry the terminal decision.
            _ => None,
        };

        if decision.is_some() {
            self.last_message = None;
        }
        Ok(decision)
    }

    /// Buffers `event` under the next monotonic sequence number.
    fn emit(&mut self, event: ExternalAgentEvent) {
        self.pending
            .push(ExternalObservedEvent::new(self.next_seq, event));
        self.next_seq += 1;
    }

    /// Handles a `thread.started` frame, emitting
    /// [`SessionStarted`](ExternalAgentEvent::SessionStarted) and capturing the
    /// thread id used to resume the session.
    fn handle_thread_started(
        &mut self,
        frame: &Map<String, Value>,
    ) -> Result<(), ExternalAgentError> {
        let Some(thread_id) = frame.get("thread_id").and_then(Value::as_str) else {
            return Err(protocol(
                "codex thread.started is missing a string `thread_id`",
            ));
        };
        self.session_id = Some(thread_id.to_owned());
        self.emit(ExternalAgentEvent::SessionStarted {
            session_id: Some(thread_id.to_owned()),
        });
        Ok(())
    }

    /// Extracts the required `item` object of an `item.*` frame.
    fn require_item<'a>(
        &self,
        frame: &'a Map<String, Value>,
    ) -> Result<&'a Map<String, Value>, ExternalAgentError> {
        frame
            .get("item")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol("codex item frame is missing an `item` object"))
    }

    /// Handles an `item.started` / `item.completed` frame, dispatching on the
    /// item's `type` tag. Unknown or typeless items are tolerated.
    fn handle_item(
        &mut self,
        frame: &Map<String, Value>,
        phase: ItemPhase,
    ) -> Result<(), ExternalAgentError> {
        let item = self.require_item(frame)?;
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return Ok(());
        };
        match (item_type, phase) {
            ("agent_message", ItemPhase::Completed) => self.handle_agent_message(item),
            ("command_execution", ItemPhase::Started) => self.handle_command_started(item),
            ("command_execution", ItemPhase::Completed) => self.handle_command_completed(item),
            ("file_change", ItemPhase::Completed) => self.handle_file_change(item),
            ("mcp_tool_call", ItemPhase::Started) => self.handle_mcp_started(item),
            ("mcp_tool_call", ItemPhase::Completed) => self.handle_mcp_completed(item),
            // `agent_message`/`file_change` only carry meaning at completion,
            // `reasoning`/`web_search`/`todo_list`/`collab_tool_call`/`error`
            // items and any unknown item type are tolerated.
            _ => {}
        }
        Ok(())
    }

    /// Records and emits a completed agent message as a
    /// [`TextDelta`](ExternalAgentEvent::TextDelta), tracking it as the turn's
    /// running summary.
    fn handle_agent_message(&mut self, item: &Map<String, Value>) {
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.last_message = Some(text.clone());
        self.emit(ExternalAgentEvent::TextDelta { text });
    }

    /// Emits [`CommandStarted`](ExternalAgentEvent::CommandStarted) for a spawned
    /// command execution item.
    fn handle_command_started(&mut self, item: &Map<String, Value>) {
        let command = item
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.emit(ExternalAgentEvent::CommandStarted {
            command,
            cwd: self.context.cwd.clone(),
        });
    }

    /// Emits the terminal observation for a finished command execution item:
    /// [`CommandFinished`](ExternalAgentEvent::CommandFinished) on success or
    /// failure, or [`PermissionRequested`](ExternalAgentEvent::PermissionRequested)
    /// when the approval policy declined it.
    fn handle_command_completed(&mut self, item: &Map<String, Value>) {
        let status = item.get("status").and_then(Value::as_str).unwrap_or("");
        if status == "declined" {
            let command = item
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let action_id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            self.emit(ExternalAgentEvent::PermissionRequested {
                action_id,
                summary: format!("run `{command}` (declined by approval policy)"),
            });
            return;
        }

        let exit_code = item
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok());
        let output = item
            .get("aggregated_output")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let failed = status == "failed" || exit_code.is_some_and(|code| code != 0);
        let (stdout_tail, stderr_tail) = if failed {
            (String::new(), output)
        } else {
            (output, String::new())
        };
        self.emit(ExternalAgentEvent::CommandFinished {
            exit_code,
            stdout_tail,
            stderr_tail,
        });
    }

    /// Emits one [`FilePatch`](ExternalAgentEvent::FilePatch) per change in a
    /// completed file-change item.
    fn handle_file_change(&mut self, item: &Map<String, Value>) {
        let Some(changes) = item.get("changes").and_then(Value::as_array) else {
            return;
        };
        for change in changes {
            let Some(change) = change.as_object() else {
                continue;
            };
            let path = change
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let kind = change
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("update");
            let summary = format!("{kind} {path}");
            self.emit(ExternalAgentEvent::FilePatch {
                path,
                summary,
                diff_ref: None,
            });
        }
    }

    /// Emits [`ToolStarted`](ExternalAgentEvent::ToolStarted) for a dispatched
    /// MCP tool call.
    fn handle_mcp_started(&mut self, item: &Map<String, Value>) {
        self.emit(ExternalAgentEvent::ToolStarted {
            name: mcp_tool_name(item),
        });
    }

    /// Emits [`ToolFinished`](ExternalAgentEvent::ToolFinished) for a completed
    /// MCP tool call, classifying the terminal status.
    fn handle_mcp_completed(&mut self, item: &Map<String, Value>) {
        let status = item.get("status").and_then(Value::as_str).unwrap_or("");
        let has_error = item.get("error").is_some_and(|error| !error.is_null());
        let status = if status == "failed" || has_error {
            ToolStatus::Error
        } else {
            ToolStatus::Ok
        };
        self.emit(ExternalAgentEvent::ToolFinished {
            name: mcp_tool_name(item),
            status,
        });
    }

    /// Handles a `turn.completed` frame, emitting
    /// [`SessionCompleted`](ExternalAgentEvent::SessionCompleted) and a
    /// [`Completed`](CodexDecision::Completed) decision carrying the turn's usage
    /// and last agent message.
    fn handle_turn_completed(&mut self, frame: &Map<String, Value>) -> CodexDecision {
        self.emit(ExternalAgentEvent::SessionCompleted);
        CodexDecision::Completed {
            output: ExternalAgentOutput {
                summary: self.last_message.clone().unwrap_or_default(),
                artifacts: Vec::new(),
                usage: parse_usage(frame.get("usage")),
                cost_micros: None,
            },
        }
    }

    /// Handles a `turn.failed` frame, decoding the reported error into a
    /// classified [`Failed`](CodexDecision::Failed) decision.
    fn handle_turn_failed(&mut self, frame: &Map<String, Value>) -> CodexDecision {
        let message = frame
            .get("error")
            .and_then(Value::as_object)
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("codex turn failed")
            .to_owned();
        CodexDecision::Failed {
            error: ExternalAgentError::Runtime {
                code: None,
                message,
            },
        }
    }
}

/// Which side of an item's lifecycle an `item.*` frame reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemPhase {
    /// An `item.started` frame.
    Started,
    /// An `item.completed` frame.
    Completed,
}

/// Builds an [`ExternalAgentError::Protocol`] from a fixed diagnostic.
fn protocol(detail: impl Into<String>) -> ExternalAgentError {
    ExternalAgentError::Protocol {
        detail: detail.into(),
    }
}

/// Builds the `server/tool` display name of an MCP tool-call item.
fn mcp_tool_name(item: &Map<String, Value>) -> String {
    let server = item
        .get("server")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool = item.get("tool").and_then(Value::as_str).unwrap_or_default();
    format!("{server}/{tool}")
}

/// Decodes a Codex `turn.completed` `usage` object into a provider-neutral
/// [`Usage`].
fn parse_usage(value: Option<&Value>) -> Option<Usage> {
    let usage = value?.as_object()?;
    let field = |key: &str| -> u32 {
        usage
            .get(key)
            .and_then(Value::as_i64)
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or(0)
    };
    Some(Usage {
        input: field("input_tokens"),
        output: field("output_tokens"),
        cache_read: field("cached_input_tokens"),
        cache_write: field("cache_write_input_tokens"),
        reasoning: field("reasoning_output_tokens"),
        total: None,
        extra: Map::new(),
    })
}
