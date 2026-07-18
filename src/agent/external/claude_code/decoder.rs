//! Private `stream-json` decoder for the managed Claude Code adapter (M6-2).
//!
//! Claude Code's `--output-format stream-json` emits newline-delimited JSON
//! frames (a `system`/`init` line, `assistant` / `user` message lines wrapping
//! Anthropic message objects, control requests for permission prompts, and a
//! terminal `result` line). That wire schema is **adapter-private**: this module
//! decodes it entirely through defensive [`serde_json::Value`] navigation and
//! never re-exports a raw frame type, so Claude Code's private JSON shape does
//! not leak into `agent-lib`'s stable API (design §12.2, 非目标 §3).
//!
//! What it *does* expose is provider-neutral: [`ClaudeStreamDecoder::push_line`]
//! turns each frame into sequenced [`ExternalObservedEvent`] observations and,
//! when a turn settles, one [`ClaudeDecision`] — the same
//! `Completed` / `PausedForToolCalls` / `PausedForInteraction` / failure control
//! transfers a [`RuntimeDecisionPoint`](crate::agent::external::RuntimeDecisionPoint)
//! carries. The live [`ExternalRuntimeSession`](crate::agent::external::ExternalRuntimeSession)
//! that wraps this decoder into start/resume/advance lands in M6-3; this task
//! freezes only the decode.
//!
//! # Frame mapping (design §12.2)
//!
//! | Claude frame | observation / decision |
//! |---|---|
//! | `system`/`init` | [`SessionStarted`](ExternalAgentEvent::SessionStarted) |
//! | assistant `text` block | [`TextDelta`](ExternalAgentEvent::TextDelta) |
//! | assistant `tool_use` `Bash` | [`CommandStarted`](ExternalAgentEvent::CommandStarted) |
//! | assistant `tool_use` edit/write | [`FilePatch`](ExternalAgentEvent::FilePatch) |
//! | assistant `tool_use` other built-in | [`ToolStarted`](ExternalAgentEvent::ToolStarted) |
//! | assistant `tool_use` `mcp__…` (host tool) | [`ClaudeDecision::PausedForToolCalls`] |
//! | user `tool_result` for a command | [`CommandFinished`](ExternalAgentEvent::CommandFinished) |
//! | user `tool_result` for another tool | [`ToolFinished`](ExternalAgentEvent::ToolFinished) |
//! | `control_request` `can_use_tool` | [`PermissionRequested`](ExternalAgentEvent::PermissionRequested) + [`ClaudeDecision::PausedForInteraction`] |
//! | `result` `success` | [`SessionCompleted`](ExternalAgentEvent::SessionCompleted) + [`ClaudeDecision::Completed`] |
//! | `result` error subtype | [`ClaudeDecision::Failed`] |
//!
//! # Tolerance policy (stable)
//!
//! The decoder is deliberately forgiving of forward-compatible drift but strict
//! about corruption, so a scheduler sees a stable classification:
//!
//! - a blank line, a `stream_event` partial-message frame, an **unknown** `type`,
//!   an unknown content block, or an uncorrelated `tool_result` is *tolerated*
//!   (no observation, no error);
//! - a line that is not valid JSON, not a JSON object, missing a string `type`,
//!   or a **known** frame whose required inner object is absent is a real
//!   protocol violation and returns [`ExternalAgentError::Protocol`].
//!
//! Every diagnostic is a fixed string; no prompt text, tool input, or credential
//! is ever folded into an error message.

// The decoder's fallible helpers return the external adapter's canonical
// `ExternalAgentError`, matching the unboxed error contract used across
// `adapter.rs`, `registry.rs`, `probe.rs`, and the public `ExternalSessionResult`
// surface. That enum is intentionally not boxed there, so `result_large_err`
// (which only fires here because these sync helpers have small `Ok` types) would
// force a signature style inconsistent with the rest of the module.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::agent::external::{
    ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput, ExternalObservedEvent,
    ExternalToolBatchId, ExternalToolCall,
};
use crate::agent::id::{AgentId, StepId};
use crate::agent::interaction::Interaction;
use crate::agent::permission::{PermissionCategory, PermissionRequest, PermissionRisk};
use crate::model::tool::ToolStatus;
use crate::model::usage::Usage;

/// Host-supplied identities the decoder needs to mint a permission
/// [`Interaction`] when Claude Code asks to use a gated tool.
///
/// A permission prompt is bound to the host's own step and requesting agent, not
/// to anything the runtime reports, so these are threaded in from the live
/// session (design §12.2). Neither is ever taken from model output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaudeDecodeContext {
    step_id: StepId,
    actor: AgentId,
}

impl ClaudeDecodeContext {
    /// Creates a decode context bound to the host `step_id` awaiting the session
    /// and the `actor` recorded as the permission requester.
    #[must_use]
    pub const fn new(step_id: StepId, actor: AgentId) -> Self {
        Self { step_id, actor }
    }
}

/// The control-flow transfer a decoded Claude Code turn settles on.
///
/// This is the decoder's provider-neutral counterpart of the non-`session`
/// payload of a [`RuntimeDecisionPoint`](crate::agent::external::RuntimeDecisionPoint):
/// the live session (M6-3) attaches the resumable
/// [`ExternalSessionRef`](crate::agent::external::ExternalSessionRef) and drained
/// observations around it. The failure arm carries the classified
/// [`ExternalAgentError`] a `result` error subtype decoded to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeDecision {
    /// The turn produced terminal output (`result`/`success`).
    Completed {
        /// Terminal output decoded from the `result` frame.
        output: ExternalAgentOutput,
    },
    /// The turn paused awaiting host execution of a host-bridged tool batch.
    PausedForToolCalls {
        /// Identifier the matching results echo back (the assistant message id).
        batch_id: ExternalToolBatchId,
        /// Host tool calls the runtime is blocked on.
        calls: Vec<ExternalToolCall>,
    },
    /// The turn paused awaiting a permission decision (`control_request`).
    PausedForInteraction {
        /// Runtime handle echoed back on resume (the control `request_id`).
        action_id: String,
        /// The permission interaction the host must resolve.
        request: Interaction,
    },
    /// The turn failed; the runtime reported an error `result`.
    Failed {
        /// Classified failure reason.
        error: ExternalAgentError,
    },
}

/// Correlation record for a Claude built-in tool the decoder is tracking between
/// its `tool_use` start and the `tool_result` that finishes it.
#[derive(Clone, Debug)]
struct ActiveTool {
    /// Tool name reported at `tool_use`, echoed on the finishing observation.
    name: String,
    /// Whether the tool is a shell command (`Bash`), which finishes as a
    /// [`CommandFinished`](ExternalAgentEvent::CommandFinished) rather than a
    /// [`ToolFinished`](ExternalAgentEvent::ToolFinished).
    is_command: bool,
}

/// Stateful decoder turning Claude Code `stream-json` frames into sequenced
/// observations and per-turn [`ClaudeDecision`]s.
///
/// One decoder spans a whole session: [`seq`](ExternalObservedEvent::seq) is
/// assigned monotonically across turns so the machine's replay dedup stays valid
/// across resumes (design §5.5). Feed each raw frame line to
/// [`push_line`](Self::push_line); drain the observations buffered before a
/// decision with [`take_observations`](Self::take_observations).
#[derive(Debug)]
pub struct ClaudeStreamDecoder {
    context: ClaudeDecodeContext,
    next_seq: u64,
    session_id: Option<String>,
    cwd: Option<String>,
    pending: Vec<ExternalObservedEvent>,
    active_tools: BTreeMap<String, ActiveTool>,
}

impl ClaudeStreamDecoder {
    /// Creates a decoder for a fresh session, binding the host identities used to
    /// mint permission interactions.
    #[must_use]
    pub fn new(context: ClaudeDecodeContext) -> Self {
        Self {
            context,
            next_seq: 0,
            session_id: None,
            cwd: None,
            pending: Vec::new(),
            active_tools: BTreeMap::new(),
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

    /// Returns the runtime-assigned session id, once a `system`/`init` frame has
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

    /// Decodes one raw `stream-json` frame line.
    ///
    /// Returns `Ok(Some(decision))` when the frame settles the current turn on a
    /// control transfer, `Ok(None)` when it only buffered observations (or was a
    /// tolerated frame), and `Err` when the frame is corrupt.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Protocol`] for a line that is not valid
    /// JSON, is not a JSON object, is missing a string `type`, or is a known
    /// frame missing a required inner object.
    pub fn push_line(&mut self, line: &str) -> Result<Option<ClaudeDecision>, ExternalAgentError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let value: Value = serde_json::from_str(trimmed)
            .map_err(|error| protocol(format!("invalid claude stream-json frame: {error}")))?;
        let Some(frame) = value.as_object() else {
            return Err(protocol("claude stream-json frame is not a JSON object"));
        };
        let Some(frame_type) = frame.get("type").and_then(Value::as_str) else {
            return Err(protocol(
                "claude stream-json frame is missing a string `type`",
            ));
        };

        let decision = match frame_type {
            "system" => {
                self.handle_system(frame);
                None
            }
            "assistant" => self.handle_assistant(frame)?,
            "user" => {
                self.handle_user(frame)?;
                None
            }
            "control_request" => self.handle_control_request(frame)?,
            "result" => Some(self.handle_result(frame)?),
            // `stream_event` partial-message deltas and any unknown frame type are
            // tolerated: the whole assistant message already carries the text.
            _ => None,
        };

        if decision.is_some() {
            self.active_tools.clear();
        }
        Ok(decision)
    }

    /// Buffers `event` under the next monotonic sequence number.
    fn emit(&mut self, event: ExternalAgentEvent) {
        self.pending
            .push(ExternalObservedEvent::new(self.next_seq, event));
        self.next_seq += 1;
    }

    /// Handles a `system` frame, emitting [`SessionStarted`] for the `init`
    /// subtype and capturing the session id and working directory.
    ///
    /// [`SessionStarted`]: ExternalAgentEvent::SessionStarted
    fn handle_system(&mut self, frame: &Map<String, Value>) {
        if frame.get("subtype").and_then(Value::as_str) != Some("init") {
            return;
        }
        let session_id = frame
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(cwd) = frame.get("cwd").and_then(Value::as_str) {
            self.cwd = Some(cwd.to_owned());
        }
        if let Some(id) = &session_id {
            self.session_id = Some(id.clone());
        }
        self.emit(ExternalAgentEvent::SessionStarted { session_id });
    }

    /// Handles an `assistant` frame: text blocks become
    /// [`TextDelta`](ExternalAgentEvent::TextDelta), built-in `tool_use` blocks
    /// become command/patch/tool observations, and any host-bridged (`mcp__…`)
    /// `tool_use` blocks fold into one [`PausedForToolCalls`] decision.
    ///
    /// [`PausedForToolCalls`]: ClaudeDecision::PausedForToolCalls
    fn handle_assistant(
        &mut self,
        frame: &Map<String, Value>,
    ) -> Result<Option<ClaudeDecision>, ExternalAgentError> {
        let Some(message) = frame.get("message").and_then(Value::as_object) else {
            return Err(protocol(
                "claude assistant frame is missing a `message` object",
            ));
        };

        let mut host_calls = Vec::new();
        if let Some(blocks) = message.get("content").and_then(Value::as_array) {
            for block in blocks {
                let Some(block) = block.as_object() else {
                    continue;
                };
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            self.emit(ExternalAgentEvent::TextDelta {
                                text: text.to_owned(),
                            });
                        }
                    }
                    Some("tool_use") => {
                        let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        if is_host_tool(name) {
                            host_calls.push(ExternalToolCall {
                                provider_call_id: id.to_owned(),
                                name: name.to_owned(),
                                input,
                                raw: None,
                            });
                        } else {
                            self.emit_builtin_tool_use(id, name, &input);
                        }
                    }
                    // `thinking`, `redacted_thinking`, and unknown block kinds are tolerated.
                    _ => {}
                }
            }
        }

        if host_calls.is_empty() {
            return Ok(None);
        }
        let batch_id = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("claude-batch-{}", self.next_seq));
        Ok(Some(ClaudeDecision::PausedForToolCalls {
            batch_id: ExternalToolBatchId::new(batch_id),
            calls: host_calls,
        }))
    }

    /// Emits the start observation for a Claude built-in tool and records it for
    /// correlation with its later `tool_result`.
    fn emit_builtin_tool_use(&mut self, id: &str, name: &str, input: &Value) {
        if name == "Bash" {
            let command = input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let cwd = self.cwd.clone().unwrap_or_default();
            self.emit(ExternalAgentEvent::CommandStarted { command, cwd });
            self.active_tools.insert(
                id.to_owned(),
                ActiveTool {
                    name: name.to_owned(),
                    is_command: true,
                },
            );
        } else if is_file_edit_tool(name) {
            let path = input
                .get("file_path")
                .or_else(|| input.get("path"))
                .or_else(|| input.get("notebook_path"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let summary = format!("{name} {path}");
            self.emit(ExternalAgentEvent::FilePatch {
                path,
                summary,
                diff_ref: None,
            });
            self.active_tools.insert(
                id.to_owned(),
                ActiveTool {
                    name: name.to_owned(),
                    is_command: false,
                },
            );
        } else {
            self.emit(ExternalAgentEvent::ToolStarted {
                name: name.to_owned(),
            });
            self.active_tools.insert(
                id.to_owned(),
                ActiveTool {
                    name: name.to_owned(),
                    is_command: false,
                },
            );
        }
    }

    /// Handles a `user` frame carrying `tool_result` blocks, finishing whichever
    /// built-in tool each result correlates to.
    fn handle_user(&mut self, frame: &Map<String, Value>) -> Result<(), ExternalAgentError> {
        let Some(message) = frame.get("message").and_then(Value::as_object) else {
            return Err(protocol("claude user frame is missing a `message` object"));
        };

        if let Some(blocks) = message.get("content").and_then(Value::as_array) {
            for block in blocks {
                let Some(block) = block.as_object() else {
                    continue;
                };
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                let tool_use_id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let is_error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // An uncorrelated result (e.g. one already paused on) is tolerated.
                let Some(active) = self.active_tools.remove(tool_use_id) else {
                    continue;
                };
                if active.is_command {
                    let text = tool_result_text(block.get("content"));
                    let (stdout_tail, stderr_tail) = if is_error {
                        (String::new(), text)
                    } else {
                        (text, String::new())
                    };
                    self.emit(ExternalAgentEvent::CommandFinished {
                        exit_code: Some(i32::from(is_error)),
                        stdout_tail,
                        stderr_tail,
                    });
                } else {
                    self.emit(ExternalAgentEvent::ToolFinished {
                        name: active.name,
                        status: if is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Ok
                        },
                    });
                }
            }
        }
        Ok(())
    }

    /// Handles a `control_request` frame, mapping a `can_use_tool` permission ask
    /// into a [`PermissionRequested`](ExternalAgentEvent::PermissionRequested)
    /// observation plus a [`PausedForInteraction`](ClaudeDecision::PausedForInteraction)
    /// decision. Other control subtypes are tolerated.
    fn handle_control_request(
        &mut self,
        frame: &Map<String, Value>,
    ) -> Result<Option<ClaudeDecision>, ExternalAgentError> {
        let Some(request_id) = frame.get("request_id").and_then(Value::as_str) else {
            return Err(protocol(
                "claude control_request is missing a string `request_id`",
            ));
        };
        let Some(request) = frame.get("request").and_then(Value::as_object) else {
            return Err(protocol(
                "claude control_request is missing a `request` object",
            ));
        };
        if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
            return Ok(None);
        }

        let action_id = request_id.to_owned();
        let tool_name = request
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let input = request.get("input").cloned().unwrap_or(Value::Null);
        let summary = permission_summary(tool_name, &input);

        self.emit(ExternalAgentEvent::PermissionRequested {
            action_id: action_id.clone(),
            summary: summary.clone(),
        });

        let permission = PermissionRequest::new(
            action_id.clone(),
            self.context.actor,
            permission_category(tool_name),
            summary,
            input,
            PermissionRisk::Medium,
            None,
        );
        Ok(Some(ClaudeDecision::PausedForInteraction {
            action_id,
            request: Interaction::permission(self.context.step_id, permission),
        }))
    }

    /// Handles a terminal `result` frame, emitting
    /// [`SessionCompleted`](ExternalAgentEvent::SessionCompleted) and a
    /// [`Completed`](ClaudeDecision::Completed) decision on success, or a
    /// classified [`Failed`](ClaudeDecision::Failed) on an error subtype.
    fn handle_result(
        &mut self,
        frame: &Map<String, Value>,
    ) -> Result<ClaudeDecision, ExternalAgentError> {
        let subtype = frame
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("success");
        let is_error = frame
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if is_error || subtype != "success" {
            let error = match subtype {
                "error_max_turns" => ExternalAgentError::LimitExceeded {
                    limit: "claude code reached its max turns".to_owned(),
                },
                other => ExternalAgentError::Runtime {
                    code: Some(other.to_owned()),
                    message: frame
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or("claude code runtime error")
                        .to_owned(),
                },
            };
            return Ok(ClaudeDecision::Failed { error });
        }

        self.emit(ExternalAgentEvent::SessionCompleted);
        let summary = frame
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let cost_micros = frame
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .map(|usd| (usd * 1_000_000.0).round() as u64);
        Ok(ClaudeDecision::Completed {
            output: ExternalAgentOutput {
                summary,
                artifacts: Vec::new(),
                usage: parse_usage(frame.get("usage")),
                cost_micros,
            },
        })
    }
}

/// Builds an [`ExternalAgentError::Protocol`] from a fixed diagnostic.
fn protocol(detail: impl Into<String>) -> ExternalAgentError {
    ExternalAgentError::Protocol {
        detail: detail.into(),
    }
}

/// Whether `name` is a host-bridged tool (an `mcp__…` server tool) the runtime
/// must pause on rather than execute itself.
fn is_host_tool(name: &str) -> bool {
    name.starts_with("mcp__")
}

/// Whether `name` is a Claude built-in that mutates a file, decoded as a
/// [`FilePatch`](ExternalAgentEvent::FilePatch).
fn is_file_edit_tool(name: &str) -> bool {
    matches!(
        name,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Update"
    )
}

/// Maps a Claude tool name to the permission category recorded on a prompt.
fn permission_category(tool_name: &str) -> PermissionCategory {
    match tool_name {
        "Bash" => PermissionCategory::Shell,
        "Read" | "Grep" | "Glob" | "LS" => PermissionCategory::FileRead,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Update" => PermissionCategory::FileWrite,
        "WebFetch" | "WebSearch" => PermissionCategory::Network,
        "Task" => PermissionCategory::SpawnAgent,
        name if is_host_tool(name) => PermissionCategory::Mcp,
        _ => PermissionCategory::Other,
    }
}

/// Builds the untrusted, human-readable summary shown for a permission prompt.
fn permission_summary(tool_name: &str, input: &Value) -> String {
    if tool_name == "Bash"
        && let Some(command) = input.get("command").and_then(Value::as_str)
    {
        return format!("run `{command}`");
    }
    format!("use {tool_name}")
}

/// Flattens a `tool_result` `content` field (a string or an array of text
/// blocks) into plain text.
fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Decodes a Claude `result` `usage` object into a provider-neutral [`Usage`].
fn parse_usage(value: Option<&Value>) -> Option<Usage> {
    let usage = value?.as_object()?;
    let field = |key: &str| -> u32 {
        usage
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or(0)
    };
    Some(Usage {
        input: field("input_tokens"),
        output: field("output_tokens"),
        cache_read: field("cache_read_input_tokens"),
        cache_write: field("cache_creation_input_tokens"),
        reasoning: 0,
        total: None,
        extra: Map::new(),
    })
}
