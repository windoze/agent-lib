//! Private `session/update` → observation decoder for the managed ACP adapter
//! (M10-2, feature `external-acp`).
//!
//! Unlike the three CLI adapters — each of which hand-rolls a `*/decoder.rs` that
//! walks raw `serde_json::Value` frames — ACP rides the official crate's typed
//! JSON-RPC schema. This decoder therefore **normalizes** the crate's
//! agent→client messages into the provider-neutral
//! [`ExternalObservedEvent`](crate::agent::external::ExternalObservedEvent)
//! vocabulary, and it never re-exports a crate protocol type as stable API: the
//! typed entry points that consume [`agent_client_protocol_schema`] values are
//! `pub(crate)` (called by the connection layer), and the only public decode
//! entry, [`AcpStreamDecoder::push_jsonrpc_line`], takes a `&str` wire line and
//! returns the neutral [`AcpDecision`] (design 非目标: no raw frame types leak).
//!
//! # Frame mapping (design §10.3 vocabulary — no new variants)
//!
//! | ACP message | observation / decision |
//! |---|---|
//! | `session/new` / `session/load` result (`sessionId`) | [`SessionStarted`](ExternalAgentEvent::SessionStarted) |
//! | `session/update` `agent_message_chunk` | [`TextDelta`](ExternalAgentEvent::TextDelta) |
//! | `session/update` `tool_call` (`execute`) | [`CommandStarted`](ExternalAgentEvent::CommandStarted) |
//! | `session/update` `tool_call` (other kinds) | [`ToolStarted`](ExternalAgentEvent::ToolStarted) |
//! | `session/update` `tool_call`/`tool_call_update` `diff` content | [`FilePatch`](ExternalAgentEvent::FilePatch) |
//! | `session/update` `tool_call_update` terminal status | [`CommandFinished`](ExternalAgentEvent::CommandFinished) / [`ToolFinished`](ExternalAgentEvent::ToolFinished) |
//! | `session/update` `plan` | one [`TaskUpdated`](ExternalAgentEvent::TaskUpdated) per entry |
//! | `session/request_permission` | [`PermissionRequested`](ExternalAgentEvent::PermissionRequested) + cached [`PendingClientRequest`] |
//! | `fs/*` / `terminal/*` request | cached [`PendingClientRequest`] |
//! | `session/prompt` result (`stopReason`) | [`SessionCompleted`](ExternalAgentEvent::SessionCompleted) + [`AcpDecision::Completed`] |
//! | JSON-RPC `error` response | [`AcpDecision::Failed`] |
//!
//! # Tolerance policy (stable, mirrors the CLI decoders)
//!
//! - a blank line, an object that is neither a request/notification nor a
//!   response, an **unmodeled** `session/update` kind, or an uncorrelated
//!   `tool_call_update` is *tolerated* (no observation, no error);
//! - a line that is not valid JSON, is not a JSON object, or is a `session/update`
//!   whose `params` cannot be decoded into the schema type is a real protocol
//!   violation and returns [`ExternalAgentError::Protocol`].
//!
//! Every diagnostic is a fixed string; no prompt text, tool input, or credential
//! is ever folded into an error message.

// The decoder's fallible entry returns the external adapter's canonical
// `ExternalAgentError`, matching the unboxed error contract used across the rest
// of the external stack; `result_large_err` would otherwise force a boxed
// signature inconsistent with the CLI decoders.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;

use agent_client_protocol_schema::v1::{
    ContentBlock, Plan, PlanEntryStatus, SessionNotification, SessionUpdate, StopReason, ToolCall,
    ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolKind,
};
use serde_json::Value;

use crate::agent::external::{
    ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput, ExternalObservedEvent,
};
use crate::model::tool::ToolStatus;

/// The control-flow transfer a decoded ACP prompt turn settles on.
///
/// This is the decoder's provider-neutral counterpart of the non-`session`
/// payload of a [`RuntimeDecisionPoint`](crate::agent::external::RuntimeDecisionPoint):
/// the live session (M10-3) attaches the resumable
/// [`ExternalSessionRef`](crate::agent::external::ExternalSessionRef) and drained
/// observations around it, and turns a cached
/// [`PendingClientRequest::Permission`] into the host-pausable interaction arm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcpDecision {
    /// The turn produced terminal output (`session/prompt` returned a
    /// `stopReason`).
    Completed {
        /// Terminal output decoded from the accumulated turn.
        output: ExternalAgentOutput,
    },
    /// The turn failed; the agent returned a JSON-RPC error for the prompt.
    Failed {
        /// Classified failure reason.
        error: ExternalAgentError,
    },
}

/// The disposition hint an ACP permission option advertises.
///
/// ACP does not answer a `session/request_permission` with a free-form
/// allow/deny; the agent supplies a list of [`AcpPermissionOption`]s and the
/// client must echo back the `optionId` of the one it selected. This neutral
/// enum mirrors the schema's `PermissionOptionKind` so the live adapter can map
/// a host [`PermissionDecision`](crate::agent::permission::PermissionDecision)
/// onto the right option without leaking a crate type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpPermissionOptionKind {
    /// Allow this operation only this time.
    AllowOnce,
    /// Allow this operation and remember the choice.
    AllowAlways,
    /// Reject this operation only this time.
    RejectOnce,
    /// Reject this operation and remember the choice.
    RejectAlways,
}

impl AcpPermissionOptionKind {
    /// Whether selecting this option grants the gated action.
    #[must_use]
    pub const fn is_allow(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

/// One option the agent offered on a `session/request_permission` request.
///
/// Provider-neutral: the `option_id` is echoed verbatim when answering and the
/// `kind` tells the adapter whether it grants or refuses the action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcpPermissionOption {
    /// The `optionId` echoed back in the permission response (never model logic).
    pub option_id: String,
    /// Whether this option allows or rejects the action.
    pub kind: AcpPermissionOptionKind,
}

/// An agent→client request the decoder recognized and cached for servicing.
///
/// ACP defines several requests the *client* must service (`request_permission`,
/// `fs/*`, `terminal/*`). The decoder identifies and caches each arrival with the
/// JSON-RPC `action_id` needed to answer it; the live adapter (M10-3) drains
/// these with [`AcpStreamDecoder::take_client_requests`] and fulfils them
/// (permission via the host-pausable interaction arm, `fs`/`terminal` against the
/// worktree). Every field is provider-neutral — no crate type leaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingClientRequest {
    /// A `session/request_permission` the host must resolve.
    Permission {
        /// JSON-RPC request id echoed back when answering (never model output).
        action_id: String,
        /// Short human-readable summary of the gated action (untrusted).
        summary: String,
        /// The options the agent offered; the answer selects one by `optionId`.
        options: Vec<AcpPermissionOption>,
    },
    /// A `fs/read_text_file` the client is expected to service.
    ReadFile {
        /// JSON-RPC request id echoed back on the response.
        action_id: String,
        /// Absolute path the agent asked to read (untrusted).
        path: String,
        /// Optional 1-based start line the agent requested.
        line: Option<u32>,
        /// Optional maximum number of lines the agent requested.
        limit: Option<u32>,
    },
    /// A `fs/write_text_file` the client is expected to service.
    WriteFile {
        /// JSON-RPC request id echoed back on the response.
        action_id: String,
        /// Absolute path the agent asked to write (untrusted).
        path: String,
        /// The text content the agent asked to write (untrusted).
        content: String,
    },
    /// A `terminal/*` request the client is expected to service.
    Terminal {
        /// JSON-RPC request id echoed back on the response.
        action_id: String,
        /// The concrete `terminal/*` method that arrived.
        method: String,
    },
}

/// Correlation record for an ACP tool call tracked between its initiating
/// `tool_call` update and the `tool_call_update` that finishes it.
#[derive(Clone, Debug)]
struct ActiveTool {
    /// Human-readable name echoed on the finishing observation.
    name: String,
    /// Whether the call is a shell command (`execute` kind), which finishes as a
    /// [`CommandFinished`](ExternalAgentEvent::CommandFinished) rather than a
    /// [`ToolFinished`](ExternalAgentEvent::ToolFinished).
    is_command: bool,
}

/// Stateful decoder turning ACP agent→client messages into sequenced
/// observations and per-turn [`AcpDecision`]s.
///
/// One decoder spans a whole session: [`seq`](ExternalObservedEvent::seq) is
/// assigned monotonically across turns so the machine's replay dedup stays valid
/// across resumes (design §5.5). The connection layer feeds it typed schema
/// values (or raw wire lines via [`push_jsonrpc_line`](Self::push_jsonrpc_line));
/// drain the observations buffered before a decision with
/// [`take_observations`](Self::take_observations) and any cached client requests
/// with [`take_client_requests`](Self::take_client_requests).
#[derive(Debug, Default)]
pub struct AcpStreamDecoder {
    next_seq: u64,
    session_id: Option<String>,
    cwd: Option<String>,
    agent_text: String,
    active_tools: BTreeMap<String, ActiveTool>,
    pending: Vec<ExternalObservedEvent>,
    client_requests: Vec<PendingClientRequest>,
}

impl AcpStreamDecoder {
    /// Creates a decoder for a fresh session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the working directory stamped onto [`CommandStarted`] observations.
    ///
    /// [`CommandStarted`]: ExternalAgentEvent::CommandStarted
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Returns the ACP session id, once a `session/new` / `session/load` result
    /// has reported one.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Drains the observations buffered since the last drain, leaving the running
    /// `seq` untouched.
    #[must_use]
    pub fn take_observations(&mut self) -> Vec<ExternalObservedEvent> {
        std::mem::take(&mut self.pending)
    }

    /// Drains the client requests (`request_permission` / `fs/*` / `terminal/*`)
    /// recognized since the last drain, for the live adapter (M10-3) to service.
    #[must_use]
    pub fn take_client_requests(&mut self) -> Vec<PendingClientRequest> {
        std::mem::take(&mut self.client_requests)
    }

    /// Decodes one raw JSON-RPC wire line from the agent.
    ///
    /// Returns `Ok(Some(decision))` when the line settles the current turn (a
    /// `session/prompt` result or a JSON-RPC error), `Ok(None)` when it only
    /// buffered observations / cached a client request / was tolerated, and `Err`
    /// when the line is corrupt.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Protocol`] for a line that is not valid JSON,
    /// is not a JSON object, or is a `session/update` whose `params` cannot be
    /// decoded into the schema type.
    pub fn push_jsonrpc_line(
        &mut self,
        line: &str,
    ) -> Result<Option<AcpDecision>, ExternalAgentError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let value: Value = serde_json::from_str(trimmed)
            .map_err(|error| protocol(format!("invalid acp json-rpc line: {error}")))?;
        let Some(frame) = value.as_object() else {
            return Err(protocol("acp json-rpc line is not a JSON object"));
        };

        if let Some(method) = frame.get("method").and_then(Value::as_str) {
            return self.handle_method(method, frame);
        }
        if let Some(result) = frame.get("result").and_then(Value::as_object) {
            return Ok(self.handle_result(result));
        }
        if let Some(error) = frame.get("error").and_then(Value::as_object) {
            return Ok(Some(self.handle_error(error)));
        }
        // A JSON-RPC object carrying none of method/result/error is tolerated.
        Ok(None)
    }

    /// Routes an incoming request/notification by its JSON-RPC `method`.
    fn handle_method(
        &mut self,
        method: &str,
        frame: &serde_json::Map<String, Value>,
    ) -> Result<Option<AcpDecision>, ExternalAgentError> {
        match method {
            "session/update" => {
                let Some(params) = frame.get("params") else {
                    return Err(protocol("acp session/update is missing `params`"));
                };
                let notification: SessionNotification = serde_json::from_value(params.clone())
                    .map_err(|error| {
                        protocol(format!("acp session/update params are malformed: {error}"))
                    })?;
                self.on_session_update(&notification.update);
                Ok(None)
            }
            "session/request_permission" => {
                self.cache_permission_request(frame);
                Ok(None)
            }
            "fs/read_text_file" => {
                let params = frame.get("params");
                self.client_requests.push(PendingClientRequest::ReadFile {
                    action_id: jsonrpc_id(frame),
                    path: request_path(frame),
                    line: params
                        .and_then(|params| params.get("line"))
                        .and_then(Value::as_u64)
                        .and_then(|line| u32::try_from(line).ok()),
                    limit: params
                        .and_then(|params| params.get("limit"))
                        .and_then(Value::as_u64)
                        .and_then(|limit| u32::try_from(limit).ok()),
                });
                Ok(None)
            }
            "fs/write_text_file" => {
                self.client_requests.push(PendingClientRequest::WriteFile {
                    action_id: jsonrpc_id(frame),
                    path: request_path(frame),
                    content: frame
                        .get("params")
                        .and_then(|params| params.get("content"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                });
                Ok(None)
            }
            other if other.starts_with("terminal/") => {
                self.client_requests.push(PendingClientRequest::Terminal {
                    action_id: jsonrpc_id(frame),
                    method: other.to_owned(),
                });
                Ok(None)
            }
            // Any other agent→client method is tolerated (no observation).
            _ => Ok(None),
        }
    }

    /// Handles a JSON-RPC response object, recognizing a session-establishing
    /// `sessionId` and a turn-ending `stopReason`.
    fn handle_result(&mut self, result: &serde_json::Map<String, Value>) -> Option<AcpDecision> {
        if let Some(session_id) = result.get("sessionId").and_then(Value::as_str) {
            self.session_started(Some(session_id.to_owned()));
            return None;
        }
        if let Some(stop_reason) = result.get("stopReason") {
            let stop_reason = serde_json::from_value::<StopReason>(stop_reason.clone())
                .unwrap_or(StopReason::EndTurn);
            return Some(self.finish_turn(stop_reason));
        }
        None
    }

    /// Classifies a JSON-RPC error response as a failed turn.
    fn handle_error(&mut self, error: &serde_json::Map<String, Value>) -> AcpDecision {
        self.active_tools.clear();
        self.agent_text.clear();
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .map(|code| code.to_string());
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("acp agent reported an error")
            .to_owned();
        AcpDecision::Failed {
            error: ExternalAgentError::Runtime { code, message },
        }
    }

    /// Records the ACP session id and emits [`SessionStarted`].
    ///
    /// [`SessionStarted`]: ExternalAgentEvent::SessionStarted
    pub(crate) fn session_started(&mut self, session_id: Option<String>) {
        if let Some(id) = &session_id {
            self.session_id = Some(id.clone());
        }
        self.emit(ExternalAgentEvent::SessionStarted { session_id });
    }

    /// Normalizes one `session/update` payload into observations.
    pub(crate) fn on_session_update(&mut self, update: &SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    self.agent_text.push_str(text);
                    self.emit(ExternalAgentEvent::TextDelta {
                        text: text.to_owned(),
                    });
                }
            }
            SessionUpdate::ToolCall(tool_call) => self.handle_tool_call(tool_call),
            SessionUpdate::ToolCallUpdate(update) => self.handle_tool_call_update(update),
            SessionUpdate::Plan(plan) => self.handle_plan(plan),
            // User echoes, thoughts, command lists, mode/config/session-info,
            // usage, and any future update kind are tolerated (no observation).
            _ => {}
        }
    }

    /// Settles the current turn: emits [`SessionCompleted`] and returns the
    /// terminal [`AcpDecision::Completed`].
    ///
    /// Every ACP `stopReason` ends the prompt turn; the live adapter (M10-3)
    /// refines the classification (e.g. limit handling) at the decision-point
    /// level. The running agent text becomes the terminal summary.
    ///
    /// [`SessionCompleted`]: ExternalAgentEvent::SessionCompleted
    pub(crate) fn finish_turn(&mut self, _stop_reason: StopReason) -> AcpDecision {
        self.emit(ExternalAgentEvent::SessionCompleted);
        let summary = std::mem::take(&mut self.agent_text).trim().to_owned();
        self.active_tools.clear();
        AcpDecision::Completed {
            output: ExternalAgentOutput {
                summary,
                artifacts: Vec::new(),
                usage: None,
                cost_micros: None,
            },
        }
    }

    /// Buffers `event` under the next monotonic sequence number.
    fn emit(&mut self, event: ExternalAgentEvent) {
        self.pending
            .push(ExternalObservedEvent::new(self.next_seq, event));
        self.next_seq += 1;
    }

    /// Emits the start observation for a tool call and any diffs it carries,
    /// recording it for correlation with its later `tool_call_update`.
    fn handle_tool_call(&mut self, tool_call: &ToolCall) {
        let id = tool_call.tool_call_id.0.to_string();
        let name = if tool_call.title.is_empty() {
            id.clone()
        } else {
            tool_call.title.clone()
        };
        let is_command = matches!(tool_call.kind, ToolKind::Execute);

        self.emit_diffs(&tool_call.content);

        if is_command {
            let command = command_text(tool_call).unwrap_or_else(|| name.clone());
            let cwd = self.cwd.clone().unwrap_or_default();
            self.emit(ExternalAgentEvent::CommandStarted { command, cwd });
        } else {
            self.emit(ExternalAgentEvent::ToolStarted { name: name.clone() });
        }
        self.active_tools
            .insert(id.clone(), ActiveTool { name, is_command });

        if let Some(status) = terminal_tool_status(tool_call.status) {
            self.finish_tool(&id, status);
        }
    }

    /// Emits any diffs and, on a terminal status, the finishing observation for a
    /// tracked tool call.
    fn handle_tool_call_update(&mut self, update: &ToolCallUpdate) {
        if let Some(content) = &update.fields.content {
            self.emit_diffs(content);
        }
        let Some(status) = update.fields.status.and_then(terminal_tool_status) else {
            return;
        };
        let id = update.tool_call_id.0.to_string();
        self.finish_tool(&id, status);
    }

    /// Emits the terminal observation for a tracked tool call, if any.
    fn finish_tool(&mut self, id: &str, status: ToolStatus) {
        let Some(active) = self.active_tools.remove(id) else {
            return;
        };
        if active.is_command {
            let exit_code = match status {
                ToolStatus::Ok => Some(0),
                _ => Some(1),
            };
            self.emit(ExternalAgentEvent::CommandFinished {
                exit_code,
                stdout_tail: String::new(),
                stderr_tail: String::new(),
            });
        } else {
            self.emit(ExternalAgentEvent::ToolFinished {
                name: active.name,
                status,
            });
        }
    }

    /// Emits a [`FilePatch`](ExternalAgentEvent::FilePatch) for each `diff`
    /// content block.
    fn emit_diffs(&mut self, content: &[ToolCallContent]) {
        for block in content {
            if let ToolCallContent::Diff(diff) = block {
                let path = diff.path.display().to_string();
                self.emit(ExternalAgentEvent::FilePatch {
                    summary: format!("edit {path}"),
                    path,
                    diff_ref: None,
                });
            }
        }
    }

    /// Emits one [`TaskUpdated`](ExternalAgentEvent::TaskUpdated) per plan entry,
    /// keyed by its stable position in the plan.
    fn handle_plan(&mut self, plan: &Plan) {
        for (index, entry) in plan.entries.iter().enumerate() {
            self.emit(ExternalAgentEvent::TaskUpdated {
                task_id: index.to_string(),
                status: plan_entry_status(&entry.status).to_owned(),
            });
        }
    }

    /// Caches a `session/request_permission` arrival and emits the neutral
    /// [`PermissionRequested`](ExternalAgentEvent::PermissionRequested)
    /// observation. The full host-pausable answer path lands in M10-3.
    fn cache_permission_request(&mut self, frame: &serde_json::Map<String, Value>) {
        let action_id = jsonrpc_id(frame);
        let summary = permission_summary(frame.get("params"));
        let options = permission_options(frame.get("params"));
        self.emit(ExternalAgentEvent::PermissionRequested {
            action_id: action_id.clone(),
            summary: summary.clone(),
        });
        self.client_requests.push(PendingClientRequest::Permission {
            action_id,
            summary,
            options,
        });
    }

    /// Emits a [`FilePatch`](ExternalAgentEvent::FilePatch) for a file the client
    /// itself materialized while servicing a `fs/write_text_file` request.
    ///
    /// The live adapter (M10-3) fulfils `fs/*` requests directly against the
    /// worktree and calls this so the write still appears in the sequenced
    /// observation stream under the decoder's monotonic `seq`.
    pub(crate) fn note_file_patch(&mut self, path: String) {
        self.emit(ExternalAgentEvent::FilePatch {
            summary: format!("edit {path}"),
            path,
            diff_ref: None,
        });
    }
}

/// Builds an [`ExternalAgentError::Protocol`] from a fixed diagnostic.
fn protocol(detail: impl Into<String>) -> ExternalAgentError {
    ExternalAgentError::Protocol {
        detail: detail.into(),
    }
}

/// Extracts plain text from a [`ContentBlock`], if it is a text block.
fn content_block_text(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::Text(text) => Some(&text.text),
        _ => None,
    }
}

/// Maps a terminal ACP tool status to a neutral [`ToolStatus`]; a still-running
/// status yields `None`.
fn terminal_tool_status(status: ToolCallStatus) -> Option<ToolStatus> {
    match status {
        ToolCallStatus::Completed => Some(ToolStatus::Ok),
        ToolCallStatus::Failed => Some(ToolStatus::Error),
        _ => None,
    }
}

/// Renders a plan entry status as a stable neutral label.
fn plan_entry_status(status: &PlanEntryStatus) -> &'static str {
    match status {
        PlanEntryStatus::Pending => "pending",
        PlanEntryStatus::InProgress => "in_progress",
        PlanEntryStatus::Completed => "completed",
        _ => "unknown",
    }
}

/// Extracts a shell command string from an `execute` tool call's `raw_input`.
fn command_text(tool_call: &ToolCall) -> Option<String> {
    let raw_input = tool_call.raw_input.as_ref()?;
    match raw_input.get("command")? {
        Value::String(command) => Some(command.clone()),
        Value::Array(parts) => {
            let joined: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            (!joined.is_empty()).then(|| joined.join(" "))
        }
        _ => None,
    }
}

/// Renders a JSON-RPC `id` (string or number) as the request handle a client
/// answer echoes back.
fn jsonrpc_id(frame: &serde_json::Map<String, Value>) -> String {
    match frame.get("id") {
        Some(Value::String(id)) => id.clone(),
        Some(Value::Number(id)) => id.to_string(),
        _ => String::new(),
    }
}

/// Extracts an untrusted, human-readable permission summary from
/// `request_permission` params.
fn permission_summary(params: Option<&Value>) -> String {
    let tool_call = params.and_then(|params| params.get("toolCall"));
    if let Some(title) = tool_call
        .and_then(|tool_call| tool_call.get("title"))
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty())
    {
        return title.to_owned();
    }
    if let Some(tool_call_id) = tool_call
        .and_then(|tool_call| tool_call.get("toolCallId"))
        .and_then(Value::as_str)
    {
        return format!("permission requested for {tool_call_id}");
    }
    "permission requested".to_owned()
}

/// Extracts the offered permission options (id + allow/reject kind) from
/// `request_permission` params, dropping any malformed entries.
fn permission_options(params: Option<&Value>) -> Vec<AcpPermissionOption> {
    let Some(options) = params
        .and_then(|params| params.get("options"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    options
        .iter()
        .filter_map(|option| {
            let option_id = option.get("optionId").and_then(Value::as_str)?.to_owned();
            let kind = match option.get("kind").and_then(Value::as_str)? {
                "allow_once" => AcpPermissionOptionKind::AllowOnce,
                "allow_always" => AcpPermissionOptionKind::AllowAlways,
                "reject_once" => AcpPermissionOptionKind::RejectOnce,
                "reject_always" => AcpPermissionOptionKind::RejectAlways,
                _ => return None,
            };
            Some(AcpPermissionOption { option_id, kind })
        })
        .collect()
}

/// Extracts the untrusted `path` from an `fs/*` request's params.
fn request_path(frame: &serde_json::Map<String, Value>) -> String {
    frame
        .get("params")
        .and_then(|params| params.get("path"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        AcpDecision, AcpPermissionOption, AcpPermissionOptionKind, AcpStreamDecoder,
        PendingClientRequest,
    };
    use crate::agent::external::{ExternalAgentError, ExternalAgentEvent, ExternalObservedEvent};
    use crate::model::tool::ToolStatus;

    use agent_client_protocol_schema::v1::{
        ContentBlock, ContentChunk, Diff, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus,
        SessionUpdate, StopReason, ToolCall, ToolCallContent, ToolCallStatus, ToolCallUpdate,
        ToolCallUpdateFields, ToolKind,
    };
    use serde_json::json;

    /// An `agent_message_chunk` carrying `text`.
    fn agent_text(text: &str) -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from(text)))
    }

    /// Feeding text / tool_call / diff / plan updates through one decoder yields
    /// the expected neutral observation stream with a strictly monotonic `seq`.
    #[test]
    fn acp_session_update_maps_to_observations() {
        let mut decoder = AcpStreamDecoder::new().with_cwd("/repo");

        // A session/new result establishes the session id.
        decoder.session_started(Some("sess-1".to_owned()));

        // Assistant text.
        decoder.on_session_update(&agent_text("Hello"));

        // A plain (non-command) tool call and its completion.
        decoder.on_session_update(&SessionUpdate::ToolCall(
            ToolCall::new("tc-search", "search docs").kind(ToolKind::Search),
        ));
        decoder.on_session_update(&SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "tc-search",
            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
        )));

        // An execute tool call carrying a command, completing successfully.
        decoder.on_session_update(&SessionUpdate::ToolCall(
            ToolCall::new("tc-cmd", "run the test suite")
                .kind(ToolKind::Execute)
                .raw_input(json!({ "command": "cargo test" })),
        ));
        decoder.on_session_update(&SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "tc-cmd",
            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
        )));

        // A file edit reported as a diff on the update.
        decoder.on_session_update(&SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "tc-edit",
            ToolCallUpdateFields::new().content(vec![ToolCallContent::from(Diff::new(
                "/repo/src/a.rs",
                "new",
            ))]),
        )));

        // A plan / todo update, one TaskUpdated per entry.
        decoder.on_session_update(&SessionUpdate::Plan(Plan::new(vec![
            PlanEntry::new(
                "write parser",
                PlanEntryPriority::High,
                PlanEntryStatus::InProgress,
            ),
            PlanEntry::new(
                "write tests",
                PlanEntryPriority::Medium,
                PlanEntryStatus::Pending,
            ),
        ])));

        // The prompt turn ends with a stopReason.
        let decision = decoder.finish_turn(StopReason::EndTurn);

        let observations = decoder.take_observations();
        let expected = vec![
            ExternalObservedEvent::new(
                0,
                ExternalAgentEvent::SessionStarted {
                    session_id: Some("sess-1".to_owned()),
                },
            ),
            ExternalObservedEvent::new(
                1,
                ExternalAgentEvent::TextDelta {
                    text: "Hello".to_owned(),
                },
            ),
            ExternalObservedEvent::new(
                2,
                ExternalAgentEvent::ToolStarted {
                    name: "search docs".to_owned(),
                },
            ),
            ExternalObservedEvent::new(
                3,
                ExternalAgentEvent::ToolFinished {
                    name: "search docs".to_owned(),
                    status: ToolStatus::Ok,
                },
            ),
            ExternalObservedEvent::new(
                4,
                ExternalAgentEvent::CommandStarted {
                    command: "cargo test".to_owned(),
                    cwd: "/repo".to_owned(),
                },
            ),
            ExternalObservedEvent::new(
                5,
                ExternalAgentEvent::CommandFinished {
                    exit_code: Some(0),
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                },
            ),
            ExternalObservedEvent::new(
                6,
                ExternalAgentEvent::FilePatch {
                    path: "/repo/src/a.rs".to_owned(),
                    summary: "edit /repo/src/a.rs".to_owned(),
                    diff_ref: None,
                },
            ),
            ExternalObservedEvent::new(
                7,
                ExternalAgentEvent::TaskUpdated {
                    task_id: "0".to_owned(),
                    status: "in_progress".to_owned(),
                },
            ),
            ExternalObservedEvent::new(
                8,
                ExternalAgentEvent::TaskUpdated {
                    task_id: "1".to_owned(),
                    status: "pending".to_owned(),
                },
            ),
            ExternalObservedEvent::new(9, ExternalAgentEvent::SessionCompleted),
        ];
        assert_eq!(observations, expected);

        // The seq stream is strictly monotonic with no gaps.
        for (index, observation) in observations.iter().enumerate() {
            assert_eq!(observation.seq, index as u64);
        }

        match decision {
            AcpDecision::Completed { output } => {
                assert_eq!(output.summary, "Hello");
                assert!(output.artifacts.is_empty());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert_eq!(decoder.session_id(), Some("sess-1"));
    }

    /// Unmodeled `session/update` kinds are tolerated: they buffer no
    /// observation and never error.
    #[test]
    fn acp_session_update_tolerates_unmodeled_kinds() {
        let mut decoder = AcpStreamDecoder::new();

        // A thought chunk and a user echo carry no managed observation.
        decoder.on_session_update(&SessionUpdate::AgentThoughtChunk(ContentChunk::new(
            ContentBlock::from("(thinking)"),
        )));
        decoder.on_session_update(&SessionUpdate::UserMessageChunk(ContentChunk::new(
            ContentBlock::from("(echo)"),
        )));

        assert!(decoder.take_observations().is_empty());
    }

    /// `push_jsonrpc_line` decodes a `session/update` notification and a
    /// `session/prompt` result into the same stream the typed path produces.
    #[test]
    fn acp_push_jsonrpc_line_decodes_notification_and_result() {
        let mut decoder = AcpStreamDecoder::new();

        assert_eq!(
            decoder
                .push_jsonrpc_line(
                    &json!({ "jsonrpc": "2.0", "id": 1, "result": { "sessionId": "s9" } })
                        .to_string()
                )
                .expect("session/new result decodes"),
            None,
        );
        assert_eq!(
            decoder
                .push_jsonrpc_line(
                    &json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "s9",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": { "type": "text", "text": "hi" },
                            },
                        },
                    })
                    .to_string(),
                )
                .expect("session/update decodes"),
            None,
        );
        let decision = decoder
            .push_jsonrpc_line(
                &json!({ "jsonrpc": "2.0", "id": 2, "result": { "stopReason": "end_turn" } })
                    .to_string(),
            )
            .expect("prompt result decodes")
            .expect("prompt result settles the turn");
        assert!(matches!(decision, AcpDecision::Completed { .. }));

        let observations = decoder.take_observations();
        assert_eq!(observations.len(), 3);
        assert_eq!(
            observations[0].event,
            ExternalAgentEvent::SessionStarted {
                session_id: Some("s9".to_owned()),
            },
        );
        assert_eq!(
            observations[1].event,
            ExternalAgentEvent::TextDelta {
                text: "hi".to_owned(),
            },
        );
        assert_eq!(observations[2].event, ExternalAgentEvent::SessionCompleted);
        assert_eq!(decoder.session_id(), Some("s9"));
    }

    /// `request_permission`, `fs/*`, and `terminal/*` requests are recognized and
    /// cached (M10-2 does not answer them); a permission also emits the neutral
    /// `PermissionRequested` observation.
    #[test]
    fn acp_push_jsonrpc_line_caches_client_requests() {
        let mut decoder = AcpStreamDecoder::new();

        decoder
            .push_jsonrpc_line(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": "s1",
                        "toolCall": { "toolCallId": "tc1", "title": "write /repo/x.rs" },
                        "options": [
                            { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                            { "optionId": "reject", "name": "Reject", "kind": "reject_once" },
                        ],
                    },
                })
                .to_string(),
            )
            .expect("permission request is tolerated");
        decoder
            .push_jsonrpc_line(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 8,
                    "method": "fs/write_text_file",
                    "params": { "sessionId": "s1", "path": "/repo/x.rs", "content": "..." },
                })
                .to_string(),
            )
            .expect("fs write request is tolerated");
        decoder
            .push_jsonrpc_line(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 9,
                    "method": "terminal/create",
                    "params": { "sessionId": "s1", "command": "ls" },
                })
                .to_string(),
            )
            .expect("terminal request is tolerated");

        let observations = decoder.take_observations();
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].event,
            ExternalAgentEvent::PermissionRequested {
                action_id: "7".to_owned(),
                summary: "write /repo/x.rs".to_owned(),
            },
        );

        let requests = decoder.take_client_requests();
        assert_eq!(
            requests,
            vec![
                PendingClientRequest::Permission {
                    action_id: "7".to_owned(),
                    summary: "write /repo/x.rs".to_owned(),
                    options: vec![
                        AcpPermissionOption {
                            option_id: "allow".to_owned(),
                            kind: AcpPermissionOptionKind::AllowOnce,
                        },
                        AcpPermissionOption {
                            option_id: "reject".to_owned(),
                            kind: AcpPermissionOptionKind::RejectOnce,
                        },
                    ],
                },
                PendingClientRequest::WriteFile {
                    action_id: "8".to_owned(),
                    path: "/repo/x.rs".to_owned(),
                    content: "...".to_owned(),
                },
                PendingClientRequest::Terminal {
                    action_id: "9".to_owned(),
                    method: "terminal/create".to_owned(),
                },
            ],
        );
        // Draining leaves the caches empty.
        assert!(decoder.take_client_requests().is_empty());
    }

    /// A JSON-RPC error response classifies the turn as a failure.
    #[test]
    fn acp_push_jsonrpc_line_classifies_error_response() {
        let mut decoder = AcpStreamDecoder::new();
        let decision = decoder
            .push_jsonrpc_line(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "error": { "code": -32000, "message": "usage limit reached" },
                })
                .to_string(),
            )
            .expect("error response decodes")
            .expect("error response settles the turn");
        match decision {
            AcpDecision::Failed {
                error: ExternalAgentError::Runtime { code, message },
            } => {
                assert_eq!(code.as_deref(), Some("-32000"));
                assert_eq!(message, "usage limit reached");
            }
            other => panic!("expected a Runtime failure, got {other:?}"),
        }
    }

    /// Blank lines and objects lacking method/result/error are tolerated; corrupt
    /// lines classify as `Protocol`, never a panic.
    #[test]
    fn acp_push_jsonrpc_line_tolerates_and_rejects() {
        let mut decoder = AcpStreamDecoder::new();
        for line in ["", "   ", &json!({ "jsonrpc": "2.0", "id": 1 }).to_string()] {
            assert_eq!(decoder.push_jsonrpc_line(line).expect("tolerated"), None);
        }
        assert!(decoder.take_observations().is_empty());

        for line in [
            "this is not json",
            &json!([1, 2, 3]).to_string(),
            &json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": { "sessionId": "s", "update": { "sessionUpdate": 7 } },
            })
            .to_string(),
        ] {
            match decoder.push_jsonrpc_line(line) {
                Err(ExternalAgentError::Protocol { .. }) => {}
                other => panic!("expected Protocol for {line:?}, got {other:?}"),
            }
        }
    }
}
