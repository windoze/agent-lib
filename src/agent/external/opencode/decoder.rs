//! Private `opencode run --format json` decoder for the managed OpenCode adapter
//! (M8-2).
//!
//! `opencode run --format json` streams newline-delimited JSON event frames. Each
//! line is an envelope `{ "type": <emit-type>, "timestamp": <ms>, "sessionID":
//! <id>, ...data }` the CLI writes for a handful of *mirrored* SDK events (see
//! `packages/opencode/src/cli/cmd/run.ts`): a finished `text` block, a
//! `tool_use` frame wrapping a settled tool part (a shell command, a file
//! edit/write, an MCP tool call, a `task` subagent), `step_start` /
//! `step_finish` step-boundary frames, an optional `reasoning` frame, and a
//! top-level `error` frame for a session error. That wire schema is
//! **adapter-private**: this module decodes it entirely through defensive
//! [`serde_json::Value`] navigation and never re-exports a raw frame type, so
//! OpenCode's private JSON shape does not leak into `agent-lib`'s stable API
//! (design §14, 非目标 §3).
//!
//! What it *does* expose is provider-neutral: [`OpenCodeStreamDecoder::push_line`]
//! turns each frame into sequenced [`ExternalObservedEvent`] observations and,
//! when a turn settles, one [`OpenCodeDecision`]. The live
//! [`ExternalRuntimeSession`](crate::agent::external::ExternalRuntimeSession) that
//! wraps this decoder into start/resume/advance lands in M8-3; this task freezes
//! only the decode.
//!
//! # Why a turn only ever completes or fails
//!
//! Like `codex exec --json`, `opencode run --format json` runs **autonomously**:
//! it executes its own tools and it resolves permission prompts against the
//! `--auto` launch flag — a permission the host pre-configured, not a mid-turn
//! bridge (the JSON `run` loop auto-approves under `--auto` and otherwise
//! auto-rejects, and never mirrors a `permission.asked` frame to stdout). The
//! stream therefore carries **no** host-pausable tool-call or permission frame: a
//! turn only ever settles on [`Completed`](OpenCodeDecision::Completed) (a
//! terminal `step_finish`) or [`Failed`](OpenCodeDecision::Failed) (a top-level
//! `error`). A permission the runtime refused surfaces as a `tool_use` whose
//! `state.status` is `error` and whose error text is one of OpenCode's stable
//! rejection messages, which the decoder reports as an informational
//! [`PermissionRequested`](ExternalAgentEvent::PermissionRequested) observation
//! (there is nothing for the host to answer — the runtime already decided).
//!
//! # Frame mapping
//!
//! | OpenCode frame | observation / decision |
//! |---|---|
//! | first frame carrying a `sessionID` | [`SessionStarted`](ExternalAgentEvent::SessionStarted) |
//! | `text` (finished block) | [`TextDelta`](ExternalAgentEvent::TextDelta) |
//! | `tool_use` `bash` | [`CommandStarted`](ExternalAgentEvent::CommandStarted) + [`CommandFinished`](ExternalAgentEvent::CommandFinished) |
//! | `tool_use` `edit`/`write`/`patch` | [`FilePatch`](ExternalAgentEvent::FilePatch) |
//! | `tool_use` `task` (subagent) | [`ToolStarted`](ExternalAgentEvent::ToolStarted) + [`ToolFinished`](ExternalAgentEvent::ToolFinished) |
//! | `tool_use` other tool | [`ToolStarted`](ExternalAgentEvent::ToolStarted) + [`ToolFinished`](ExternalAgentEvent::ToolFinished) |
//! | `tool_use` `state.status = error` (permission rejection) | [`PermissionRequested`](ExternalAgentEvent::PermissionRequested) |
//! | `step_finish` `reason = "tool-calls"` | tolerated (the agentic loop continues) |
//! | `step_finish` other reason | [`SessionCompleted`](ExternalAgentEvent::SessionCompleted) + [`Completed`](OpenCodeDecision::Completed) |
//! | `error` | [`Failed`](OpenCodeDecision::Failed) |
//!
//! Because `run --format json` only mirrors a *settled* tool part (its
//! `state.status` is already `completed`/`error`), the decoder reconstructs the
//! whole command/tool lifecycle from that one frame, emitting the started/finished
//! pair the internal event model uses.
//!
//! # Tolerance policy (stable)
//!
//! The decoder is deliberately forgiving of forward-compatible drift but strict
//! about corruption, so a scheduler sees a stable classification:
//!
//! - a blank line, a bounded run of non-JSON runtime noise, a `step_start`
//!   boundary frame, a `reasoning` frame, an unknown item `tool`, and an
//!   **unknown** top-level `type` are *tolerated* (no observation, no error);
//! - too many consecutive non-JSON lines, a JSON value that is not an object,
//!   missing a string `type`, or a `text` / `tool_use` / `step_finish` frame
//!   whose `part` is absent or not an object is a real protocol violation and
//!   returns [`ExternalAgentError::Protocol`].
//!
//! Every diagnostic is a fixed string; no prompt text, command line, tool output,
//! or credential is ever folded into an error message. The raw runtime-reported
//! text of an `error` frame is preserved separately in
//! [`ExternalAgentError::Runtime::runtime_output`], outside the `Display`
//! rendering.

// The decoder's fallible helpers return the external adapter's canonical
// `ExternalAgentError`, matching the unboxed error contract used across
// `adapter.rs`, `registry.rs`, `probe.rs`, and the public `ExternalSessionResult`
// surface. That enum is intentionally not boxed there, so `result_large_err`
// (which only fires here because these sync helpers have small `Ok` types) would
// force a signature style inconsistent with the rest of the module.
#![allow(clippy::result_large_err)]

use serde_json::{Map, Value};

use crate::agent::external::process::jsonl::JsonlDecoderCore;
#[cfg(test)]
use crate::agent::external::process::jsonl::MAX_CONSECUTIVE_NON_JSON_LINES;
use crate::agent::external::{
    ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput, ExternalObservedEvent,
};
use crate::model::tool::ToolStatus;
use crate::model::usage::Usage;

/// Host-supplied context the decoder needs while turning `opencode run --format
/// json` frames into observations.
///
/// OpenCode reports a `bash` tool part without the directory it ran in, so the
/// host threads in the worktree it launched `opencode run` under; the decoder
/// stamps it onto every [`CommandStarted`](ExternalAgentEvent::CommandStarted)
/// observation. It is never taken from model output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenCodeDecodeContext {
    cwd: String,
}

impl OpenCodeDecodeContext {
    /// Creates a decode context with an unknown working directory (the empty
    /// string is stamped onto command observations until one is set).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the working directory OpenCode runs commands in (typically the
    /// agent's worktree), recorded on decoded command observations.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = cwd.into();
        self
    }
}

/// The control-flow transfer a decoded OpenCode turn settles on.
///
/// This is the decoder's provider-neutral counterpart of the terminal payload of
/// a [`RuntimeDecisionPoint`](crate::agent::external::RuntimeDecisionPoint): the
/// live session (M8-3) attaches the resumable
/// [`ExternalSessionRef`](crate::agent::external::ExternalSessionRef) and drained
/// observations around it. There is deliberately no paused arm — `opencode run
/// --format json` runs autonomously and never hands a tool call or an approval
/// back to the host mid-turn (see the module docs), so a turn only ever completes
/// or fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenCodeDecision {
    /// The turn produced terminal output (a terminal `step_finish`).
    Completed {
        /// Terminal output decoded from the turn and its last text block.
        output: ExternalAgentOutput,
    },
    /// The turn failed; the runtime reported a top-level `error`.
    Failed {
        /// Classified failure reason.
        error: ExternalAgentError,
    },
}

/// Stateful decoder turning `opencode run --format json` frames into sequenced
/// observations and per-turn [`OpenCodeDecision`]s.
///
/// One decoder spans a whole session: [`seq`](ExternalObservedEvent::seq) is
/// assigned monotonically across turns so the machine's replay dedup stays valid
/// across resumes (design §5.5). Feed each raw frame line to
/// [`push_line`](Self::push_line); drain the observations buffered before a
/// decision with [`take_observations`](Self::take_observations).
#[derive(Debug)]
pub struct OpenCodeStreamDecoder {
    context: OpenCodeDecodeContext,
    core: JsonlDecoderCore,
    last_message: Option<String>,
    usage: Usage,
    cost_micros: u64,
}

impl OpenCodeStreamDecoder {
    /// Creates a decoder for a fresh session, binding the host decode context.
    #[must_use]
    pub fn new(context: OpenCodeDecodeContext) -> Self {
        Self {
            context,
            core: JsonlDecoderCore::new(),
            last_message: None,
            usage: Usage::default(),
            cost_micros: 0,
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
        self.core = self.core.with_next_seq(next_seq);
        self
    }

    /// Returns the runtime-assigned session id, once a frame has reported one.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.core.session_id()
    }

    /// Drains the observations buffered since the last drain, transferring
    /// ownership to the caller and leaving the running `seq` untouched.
    #[must_use]
    pub fn take_observations(&mut self) -> Vec<ExternalObservedEvent> {
        self.core.take_observations()
    }

    /// Decodes one raw `opencode run --format json` frame line.
    ///
    /// Returns `Ok(Some(decision))` when the frame settles the current turn on a
    /// control transfer, `Ok(None)` when it only buffered observations (or was a
    /// tolerated frame), and `Err` when the frame is corrupt.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Protocol`] after too many consecutive
    /// non-JSON lines, or for a JSON line that is not an object, is missing a
    /// string `type`, or is a `text` / `tool_use` / `step_finish` frame whose
    /// `part` is absent or not an object.
    pub fn push_line(
        &mut self,
        line: &str,
    ) -> Result<Option<OpenCodeDecision>, ExternalAgentError> {
        let Some((frame_type, frame)) = self.core.parse_frame(line, "opencode run json")? else {
            return Ok(None);
        };

        // OpenCode has no dedicated init frame; the session id rides on every
        // mirrored event, so capture it lazily from the first frame that reports
        // one and announce the session once.
        self.ensure_session(&frame);

        let decision = match frame_type.as_str() {
            "text" => {
                self.handle_text(&frame)?;
                None
            }
            "tool_use" => {
                self.handle_tool_use(&frame)?;
                None
            }
            "step_finish" => self.handle_step_finish(&frame)?,
            "error" => Some(decode_error(&frame)),
            // `step_start` is a pure step boundary, `reasoning` is internal
            // thinking, and any unknown future frame type carries no terminal
            // fact the step-finish/error frames do not already provide.
            _ => None,
        };

        if decision.is_some() {
            self.reset_turn();
        }
        Ok(decision)
    }

    /// Buffers `event` under the next monotonic sequence number.
    fn emit(&mut self, event: ExternalAgentEvent) {
        self.core.emit(event);
    }

    /// Captures the runtime session id from a frame's `sessionID` and emits
    /// [`SessionStarted`](ExternalAgentEvent::SessionStarted) the first time one
    /// is seen.
    fn ensure_session(&mut self, frame: &Map<String, Value>) {
        if self.core.session_id().is_some() {
            return;
        }
        let Some(session_id) = frame.get("sessionID").and_then(Value::as_str) else {
            return;
        };
        self.core.set_session_id(session_id.to_owned());
        self.emit(ExternalAgentEvent::SessionStarted {
            session_id: Some(session_id.to_owned()),
        });
    }

    /// Extracts the required `part` object of a part-carrying frame.
    fn require_part<'a>(
        &self,
        frame: &'a Map<String, Value>,
    ) -> Result<&'a Map<String, Value>, ExternalAgentError> {
        frame
            .get("part")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol("opencode frame is missing a `part` object"))
    }

    /// Handles a finished `text` frame, emitting it as a
    /// [`TextDelta`](ExternalAgentEvent::TextDelta) and tracking it as the turn's
    /// running summary.
    fn handle_text(&mut self, frame: &Map<String, Value>) -> Result<(), ExternalAgentError> {
        let part = self.require_part(frame)?;
        let text = part
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.last_message = Some(text.clone());
        self.emit(ExternalAgentEvent::TextDelta { text });
        Ok(())
    }

    /// Handles a settled `tool_use` frame, dispatching on the tool part's `tool`
    /// tag and terminal `state.status`.
    fn handle_tool_use(&mut self, frame: &Map<String, Value>) -> Result<(), ExternalAgentError> {
        let part = self.require_part(frame)?;
        let tool = part.get("tool").and_then(Value::as_str).unwrap_or_default();
        let state = part.get("state").and_then(Value::as_object);
        let status = state
            .and_then(|state| state.get("status"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let is_error = status == "error";

        // A permission the runtime refused surfaces as a tool error whose text is
        // one of OpenCode's stable rejection messages; report it as an
        // informational permission observation (the runtime already decided).
        if is_error {
            let error_text = state
                .and_then(|state| state.get("error"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if is_permission_rejection(error_text) {
                let call_id = part
                    .get("callID")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                self.emit(ExternalAgentEvent::PermissionRequested {
                    action_id: call_id,
                    summary: format!("`{tool}` (rejected by permission policy)"),
                });
                return Ok(());
            }
        }

        let tool_status = if is_error {
            ToolStatus::Error
        } else {
            ToolStatus::Ok
        };
        match tool {
            "bash" => self.emit_command(state, is_error),
            "edit" | "write" | "patch" | "apply_patch" => self.emit_file_patch(tool, state),
            // Every other settled tool (including the `task` subagent OpenCode
            // runs autonomously) is a host-visible tool call.
            _ => {
                self.emit(ExternalAgentEvent::ToolStarted {
                    name: tool.to_owned(),
                });
                self.emit(ExternalAgentEvent::ToolFinished {
                    name: tool.to_owned(),
                    status: tool_status,
                });
            }
        }
        Ok(())
    }

    /// Reconstructs the command lifecycle of a settled `bash` tool part as a
    /// [`CommandStarted`](ExternalAgentEvent::CommandStarted) +
    /// [`CommandFinished`](ExternalAgentEvent::CommandFinished) pair.
    fn emit_command(&mut self, state: Option<&Map<String, Value>>, is_error: bool) {
        let input = state
            .and_then(|state| state.get("input"))
            .and_then(Value::as_object);
        let command = input
            .and_then(|input| input.get("command"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.emit(ExternalAgentEvent::CommandStarted {
            command,
            cwd: self.context.cwd.clone(),
        });

        let metadata = state
            .and_then(|state| state.get("metadata"))
            .and_then(Value::as_object);
        let exit_code = metadata
            .and_then(|metadata| metadata.get("exit"))
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok());
        let output = state
            .and_then(|state| state.get("output"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let failed = is_error || exit_code.is_some_and(|code| code != 0);
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

    /// Emits a [`FilePatch`](ExternalAgentEvent::FilePatch) for a settled
    /// edit/write/patch tool part.
    fn emit_file_patch(&mut self, tool: &str, state: Option<&Map<String, Value>>) {
        let input = state
            .and_then(|state| state.get("input"))
            .and_then(Value::as_object);
        let path = input
            .and_then(|input| input.get("filePath"))
            .and_then(Value::as_str)
            .or_else(|| {
                state
                    .and_then(|state| state.get("title"))
                    .and_then(Value::as_str)
            })
            .unwrap_or_default()
            .to_owned();
        let summary = format!("{tool} {path}");
        self.emit(ExternalAgentEvent::FilePatch {
            path,
            summary,
            diff_ref: None,
        });
    }

    /// Handles a `step_finish` frame, accumulating its usage and settling the
    /// turn on [`Completed`](OpenCodeDecision::Completed) unless the model asked
    /// for more tools (`reason = "tool-calls"`), in which case the agentic loop
    /// continues.
    fn handle_step_finish(
        &mut self,
        frame: &Map<String, Value>,
    ) -> Result<Option<OpenCodeDecision>, ExternalAgentError> {
        let part = self.require_part(frame)?;
        self.accumulate_usage(part);
        let reason = part.get("reason").and_then(Value::as_str).unwrap_or("stop");
        if reason == "tool-calls" {
            return Ok(None);
        }
        self.emit(ExternalAgentEvent::SessionCompleted);
        Ok(Some(OpenCodeDecision::Completed {
            output: ExternalAgentOutput {
                summary: self.last_message.clone().unwrap_or_default(),
                artifacts: Vec::new(),
                usage: Some(self.usage.clone()),
                cost_micros: Some(self.cost_micros),
            },
        }))
    }

    /// Folds a `step_finish` part's per-step `tokens` and `cost` into the turn's
    /// running usage total.
    fn accumulate_usage(&mut self, part: &Map<String, Value>) {
        if let Some(tokens) = part.get("tokens").and_then(Value::as_object) {
            let field = |key: &str| -> u32 {
                tokens
                    .get(key)
                    .and_then(Value::as_i64)
                    .and_then(|count| u32::try_from(count).ok())
                    .unwrap_or(0)
            };
            let cache = tokens.get("cache").and_then(Value::as_object);
            let cache_field = |key: &str| -> u32 {
                cache
                    .and_then(|cache| cache.get(key))
                    .and_then(Value::as_i64)
                    .and_then(|count| u32::try_from(count).ok())
                    .unwrap_or(0)
            };
            self.usage.input = self.usage.input.saturating_add(field("input"));
            self.usage.output = self.usage.output.saturating_add(field("output"));
            self.usage.reasoning = self.usage.reasoning.saturating_add(field("reasoning"));
            self.usage.cache_read = self.usage.cache_read.saturating_add(cache_field("read"));
            self.usage.cache_write = self.usage.cache_write.saturating_add(cache_field("write"));
        }
        if let Some(cost) = part.get("cost").and_then(Value::as_f64)
            && cost.is_finite()
            && cost > 0.0
        {
            let micros = (cost * 1_000_000.0).round();
            if micros.is_finite() && micros >= 0.0 {
                self.cost_micros = self.cost_micros.saturating_add(micros as u64);
            }
        }
    }

    /// Resets the per-turn scratch (running summary and usage totals) after a
    /// turn settles, so a resumed turn accounts only its own output.
    fn reset_turn(&mut self) {
        self.last_message = None;
        self.usage = Usage::default();
        self.cost_micros = 0;
    }
}

/// Handles a top-level `error` frame, decoding the reported error into a
/// classified [`Failed`](OpenCodeDecision::Failed) decision. Kept free-standing
/// because it borrows the frame immutably and returns a value. The reported
/// `data.message` / `name` text is model-influenced output, so it is preserved
/// in [`ExternalAgentError::Runtime::runtime_output`] while `message` stays a
/// fixed diagnostic.
fn decode_error(frame: &Map<String, Value>) -> OpenCodeDecision {
    let error = frame.get("error").and_then(Value::as_object);
    let runtime_output = error
        .and_then(|error| error.get("data"))
        .and_then(Value::as_object)
        .and_then(|data| data.get("message"))
        .and_then(Value::as_str)
        .or_else(|| {
            error
                .and_then(|error| error.get("name"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned);
    OpenCodeDecision::Failed {
        error: ExternalAgentError::Runtime {
            code: None,
            message: "opencode session failed".to_owned(),
            runtime_output,
        },
    }
}

/// Builds an [`ExternalAgentError::Protocol`] from a fixed diagnostic.
fn protocol(detail: impl Into<String>) -> ExternalAgentError {
    ExternalAgentError::Protocol {
        detail: detail.into(),
    }
}

/// Whether a tool error message is one of OpenCode's stable permission-rejection
/// forms (`PermissionRejectedError` / `PermissionCorrectedError` /
/// `PermissionDeniedError`).
///
/// OpenCode's `run --format json` loop never mirrors a `permission.asked` frame,
/// so the only in-stream trace of a refused permission is the tool error text it
/// produces. Matching these fixed prefixes lets the decoder surface the refusal
/// as an informational observation without misclassifying an ordinary tool
/// failure. If OpenCode reworded these messages the cassette drift tests catch it.
fn is_permission_rejection(error: &str) -> bool {
    error.contains("rejected permission to use this specific tool call")
        || error.contains("prevents you from using this specific tool call")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_json_noise_is_bounded_and_does_not_leak_line_contents() {
        let mut decoder = OpenCodeStreamDecoder::new(OpenCodeDecodeContext::new());
        for _ in 0..MAX_CONSECUTIVE_NON_JSON_LINES {
            assert_eq!(decoder.push_line("runtime warning: warming up"), Ok(None));
        }

        decoder
            .push_line(r#"{"type":"step_start","sessionID":"session-1"}"#)
            .expect("valid JSON resets noise counter");
        assert_eq!(decoder.session_id(), Some("session-1"));

        for _ in 0..MAX_CONSECUTIVE_NON_JSON_LINES {
            assert_eq!(decoder.push_line("SECRET=sk-test warning"), Ok(None));
        }
        let error = decoder
            .push_line("SECRET=sk-test warning")
            .expect_err("too many consecutive non-json lines fail");
        assert!(matches!(error, ExternalAgentError::Protocol { .. }));
        let message = error.to_string();
        assert!(message.contains("too many consecutive non-json"));
        assert!(!message.contains("sk-test"));
    }
}
