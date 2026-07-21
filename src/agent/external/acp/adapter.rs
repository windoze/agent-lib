//! Live ACP runtime session and adapter (M10-3, feature `external-acp`).
//!
//! M10-1 froze the [`AcpConfig`](super::AcpConfig) launch recipe and the
//! `initialize` → [`capabilities_from_initialize`](super::capabilities_from_initialize)
//! negotiation mapping; M10-2 froze the [`connection`](super::connection) transport
//! ([`SpawnedAcpAgent`]) and the [`AcpStreamDecoder`] that normalizes agent→client
//! wire lines into sequenced observations and per-turn [`AcpDecision`]s. This module
//! wires those into the two live-IO traits the milestone-5 abstraction defines
//! (design §11, §12):
//!
//! - [`AcpAdapter`] is the per-runtime factory ([`ExternalRuntimeAdapter`]). It
//!   reports the managed capabilities its sessions can fulfil,
//!   [`start`](ExternalRuntimeAdapter::start)s a fresh ACP session over a single
//!   long-lived bidirectional connection, and
//!   [`resume`](ExternalRuntimeAdapter::resume)s a prior one — but only when the
//!   agent advertised `session/load`.
//! - `AcpSession` (private) is one live session ([`ExternalRuntimeSession`]). It
//!   owns the [`SpawnedAcpAgent`] transport and a single [`AcpStreamDecoder`] whose
//!   `seq` spans the whole session, writes host turns as `session/prompt` requests,
//!   feeds each agent line to the decoder, mirrors observations to the live sink,
//!   and [`advance`](ExternalRuntimeSession::advance)s to the next
//!   [`RuntimeDecisionPoint`].
//!
//! # First host-pausable arm
//!
//! ACP is the first adapter that truly drives the machine's *host-pausable* path:
//! a `session/request_permission` (agent→client request) becomes a
//! [`PausedForInteraction`](RuntimeDecisionPoint::PausedForInteraction); the host
//! resolves it; the resolution returns as
//! [`RespondInteraction`](ExternalSessionInput::RespondInteraction); and this
//! adapter — after validating the response with
//! [`Interaction::accepts_response`] — writes it back as the ACP permission
//! response. The paused [`Interaction`]'s
//! [`step_id`](crate::agent::interaction::Interaction::step_id) and permission
//! `actor` are bound to the host's [`RunContext::run_id`] and the requesting
//! `agent_id` — **never** to anything the runtime reports.
//!
//! # Client environment services
//!
//! ACP has the *client* service `fs/read_text_file`, `fs/write_text_file`, and
//! `terminal/*`. This adapter fulfils the `fs/*` requests directly against the
//! session's working directory (the worktree), honouring the
//! [`ExternalPermissionMode`] (a [`Plan`](ExternalPermissionMode::Plan) session
//! refuses writes), and surfaces a serviced write as a
//! [`FilePatch`](crate::agent::external::ExternalAgentEvent::FilePatch)
//! observation. It does **not** fold these into a host tool call: the first ACP
//! version reports [`host_tools`](crate::agent::external::ExternalRuntimeCapabilities::host_tools)
//! `false` and rejects a request that declares tools with
//! [`ExternalAgentError::UnsupportedCapability`]. `terminal/*` is rejected at the
//! JSON-RPC layer because the client advertises `terminal: false`.
//!
//! # Offline testability
//!
//! The session drives its IO through the injectable [`AcpLauncher`] trait, not a
//! process directly. Production uses [`TokioProcessLauncher`], which spawns the
//! real ACP agent; the unit tests inject a fake launcher that replays canned
//! agent lines and captures the JSON-RPC the session writes, so the whole
//! initialize/prompt/permission/fs/cancel/shutdown state machine is exercised with
//! no ACP agent binary and no network. The real end-to-end coverage lives behind
//! an `#[ignore]` in `tests/external_acp.rs`.

// The session's fallible helpers return the external adapter's canonical
// `ExternalAgentError`, matching the unboxed error contract used across the rest
// of the external stack; `result_large_err` would otherwise force a boxed
// signature inconsistent with the CLI adapters.
#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::agent::RunContext;
use crate::agent::id::{AgentId, StepId};
use crate::agent::interaction::{Interaction, InteractionResponse};
use crate::agent::permission::{
    PermissionCategory, PermissionDecision, PermissionRequest, PermissionRisk,
};

use crate::agent::external::process;
use crate::agent::external::{
    ExternalAgentError, ExternalAgentOutput, ExternalCapability, ExternalEventSink,
    ExternalObservedEvent, ExternalPermissionMode, ExternalRuntimeAdapter,
    ExternalRuntimeCapabilities, ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionInput,
    ExternalSessionRef, ExternalSessionRequest, ExternalSessionShutdown, RuntimeDecisionPoint,
};

use super::{
    ACP_WIRE_VERSION, AcpConfig, AcpDecision, AcpLauncher, AcpNegotiatedCapabilities,
    AcpPermissionOption, AcpPermissionOptionKind, AcpStreamDecoder, PendingClientRequest,
    SpawnedAcpAgent, TokioProcessLauncher, acp_runtime_kind,
};

/// A permission request the session paused on, retained across the pause so the
/// later [`RespondInteraction`](ExternalSessionInput::RespondInteraction) can be
/// validated against the exact [`Interaction`] and mapped onto the agent's offered
/// options.
struct PendingPermission {
    /// JSON-RPC id of the `session/request_permission` request being answered.
    request_id: String,
    /// The host-bound interaction the pause reified, kept for
    /// [`Interaction::accepts_response`] validation.
    interaction: Interaction,
    /// The options the agent offered; the answer selects one by `optionId`.
    options: Vec<AcpPermissionOption>,
}

/// One live ACP session over a single long-lived connection.
struct AcpSession {
    transport: SpawnedAcpAgent,
    decoder: AcpStreamDecoder,
    session_id: String,
    last_event_seq: Option<u64>,
    sink: Option<Arc<dyn ExternalEventSink>>,
    capabilities: ExternalRuntimeCapabilities,
    permission_mode: ExternalPermissionMode,
    /// Working directory the session runs in; the `session/new` `cwd` and the
    /// root the `fs/*` services are fulfilled against.
    cwd: PathBuf,
    /// Host-bound step id for every permission interaction (from `run_id`).
    step_id: StepId,
    /// Host-bound actor for every permission interaction (the requesting agent).
    actor: AgentId,
    /// Monotonic JSON-RPC id counter for the requests this client originates.
    next_request_id: u64,
    /// Capabilities the `initialize` handshake negotiated (drives resume).
    negotiated: AcpNegotiatedCapabilities,
    /// The permission request awaiting a host resolution, if the session paused.
    pending: Option<PendingPermission>,
    /// Observations buffered by the handshake, prepended to the first turn.
    carried: Vec<ExternalObservedEvent>,
    /// Grace period a graceful [`shutdown`](ExternalRuntimeSession::shutdown)
    /// waits before force-killing the child.
    shutdown_grace: Duration,
}

impl AcpSession {
    /// Builds a session over `transport`, binding the host identities used for
    /// permission interactions and the capability set the adapter reports.
    #[allow(clippy::too_many_arguments)]
    fn new(
        transport: SpawnedAcpAgent,
        step_id: StepId,
        actor: AgentId,
        sink: Option<Arc<dyn ExternalEventSink>>,
        capabilities: ExternalRuntimeCapabilities,
        permission_mode: ExternalPermissionMode,
        cwd: PathBuf,
        shutdown_grace: Duration,
    ) -> Self {
        let decoder = AcpStreamDecoder::new().with_cwd(cwd.to_string_lossy().into_owned());
        Self {
            transport,
            decoder,
            session_id: String::new(),
            last_event_seq: None,
            sink,
            capabilities,
            permission_mode,
            cwd,
            step_id,
            actor,
            next_request_id: 0,
            negotiated: AcpNegotiatedCapabilities::none(),
            pending: None,
            carried: Vec::new(),
            shutdown_grace,
        }
    }

    /// Seeds the session from the persisted high-water mark of a resumed
    /// session.
    ///
    /// Continues the decoder's `seq` line past `high_water` and restores the
    /// session's own water mark so [`session_ref`](ExternalRuntimeSession::session_ref)
    /// never reports a regressed `last_event_seq`. See
    /// [`AcpStreamDecoder::with_next_seq`] for why a resume must not restart
    /// the seq line at 0.
    #[must_use]
    fn with_resume_high_water(mut self, high_water: Option<u64>) -> Self {
        if let Some(high_water) = high_water {
            self.decoder = self.decoder.with_next_seq(high_water.saturating_add(1));
            self.last_event_seq = Some(high_water);
        }
        self
    }

    /// Runs the startup handshake for a fresh (`resume == None`) or resumed
    /// (`resume == Some(id)`) session.
    ///
    /// Both paths first send `initialize` and record the negotiated capabilities.
    /// A fresh session then sends `session/new` and adopts the returned
    /// `sessionId`; a resume sends `session/load` for the known id, but only when
    /// the agent advertised `session/load`.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Protocol`] for a corrupt handshake frame,
    /// [`SessionLost`](ExternalAgentError::SessionLost) on a read/write failure,
    /// [`Launch`](ExternalAgentError::Launch) when a fresh session never reports an
    /// id, or [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable) when a
    /// resume is requested but unsupported.
    async fn begin(&mut self, resume: Option<String>) -> Result<(), ExternalAgentError> {
        let init_id = self.next_id();
        self.send_request(
            init_id,
            "initialize",
            json!({
                "protocolVersion": ACP_WIRE_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": true, "writeTextFile": true },
                    "terminal": false,
                },
            }),
        )
        .await?;
        let init_result = self.read_response(init_id).await?;
        self.negotiated = negotiated_from_initialize(&init_result);

        match resume {
            Some(session_id) => self.begin_resume(session_id).await,
            None => self.begin_fresh().await,
        }
    }

    /// Establishes a fresh session id via `session/new`.
    async fn begin_fresh(&mut self) -> Result<(), ExternalAgentError> {
        let new_id = self.next_id();
        self.send_request(
            new_id,
            "session/new",
            json!({ "cwd": self.cwd_string(), "mcpServers": [] }),
        )
        .await?;
        let result = self.read_response(new_id).await?;
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| ExternalAgentError::Launch {
                runtime: acp_runtime_kind(),
                detail: "acp session/new did not return a sessionId".to_owned(),
            })?
            .to_owned();
        self.adopt_session(session_id);
        Ok(())
    }

    /// Reattaches to a prior session id via `session/load`.
    async fn begin_resume(&mut self, session_id: String) -> Result<(), ExternalAgentError> {
        self.session_id = session_id.clone();
        if !self.negotiated.load_session {
            return Err(ExternalAgentError::ResumeUnavailable {
                session: self.session_ref(),
                detail: "acp agent did not advertise session/load".to_owned(),
            });
        }
        let load_id = self.next_id();
        self.send_request(
            load_id,
            "session/load",
            json!({
                "sessionId": session_id,
                "cwd": self.cwd_string(),
                "mcpServers": [],
            }),
        )
        .await?;
        let _ = self.read_response(load_id).await?;
        self.adopt_session(session_id);
        Ok(())
    }

    /// Records the session id and emits its [`SessionStarted`] observation.
    ///
    /// [`SessionStarted`]: crate::agent::external::ExternalAgentEvent::SessionStarted
    fn adopt_session(&mut self, session_id: String) {
        self.session_id = session_id.clone();
        self.decoder.session_started(Some(session_id));
        let observed = self.drain_and_emit();
        self.carried.extend(observed);
    }

    /// Returns and advances the monotonic JSON-RPC request id counter.
    fn next_id(&mut self) -> u64 {
        self.next_request_id += 1;
        self.next_request_id
    }

    /// The working directory as a wire string for the `cwd` params.
    fn cwd_string(&self) -> String {
        self.cwd.to_string_lossy().into_owned()
    }

    /// Sends a JSON-RPC request line.
    async fn send_request(
        &mut self,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<(), ExternalAgentError> {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();
        self.write_line(&frame).await
    }

    /// Sends a JSON-RPC notification (no id, no response expected).
    async fn send_notification(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<(), ExternalAgentError> {
        let frame = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        })
        .to_string();
        self.write_line(&frame).await
    }

    /// Writes one line to the transport, tagging a failure with the session ref.
    async fn write_line(&mut self, frame: &str) -> Result<(), ExternalAgentError> {
        let session = self.maybe_session_ref();
        self.transport
            .write_line(frame)
            .await
            .map_err(|error| with_session(session, error))
    }

    /// Reads agent lines until the JSON-RPC response with `expected_id` arrives,
    /// feeding any interleaved notification to the decoder along the way.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::Protocol`] for a corrupt line,
    /// [`Runtime`](ExternalAgentError::Runtime) when the response is a JSON-RPC
    /// error, or [`SessionLost`](ExternalAgentError::SessionLost) when the
    /// connection closes before the response arrives.
    async fn read_response(
        &mut self,
        expected_id: u64,
    ) -> Result<serde_json::Map<String, Value>, ExternalAgentError> {
        let expected = json!(expected_id);
        loop {
            let line = self
                .read_line()
                .await?
                .ok_or_else(|| ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: "acp connection closed during handshake".to_owned(),
                })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value =
                serde_json::from_str(trimmed).map_err(|error| ExternalAgentError::Protocol {
                    detail: format!("invalid acp json-rpc line: {error}"),
                })?;
            let Some(object) = value.as_object() else {
                return Err(ExternalAgentError::Protocol {
                    detail: "acp json-rpc line is not a JSON object".to_owned(),
                });
            };
            if object.get("id") == Some(&expected) {
                if let Some(result) = object.get("result").and_then(Value::as_object) {
                    return Ok(result.clone());
                }
                if let Some(error) = object.get("error").and_then(Value::as_object) {
                    return Err(classify_error(error));
                }
                return Err(ExternalAgentError::Protocol {
                    detail: "acp handshake response carried neither result nor error".to_owned(),
                });
            }
            // A notification (or an unexpected client request) interleaved with the
            // handshake: feed it to the decoder so its observations are carried.
            self.decoder.push_jsonrpc_line(trimmed)?;
            let observed = self.drain_and_emit();
            self.carried.extend(observed);
        }
    }

    /// Reads one transport line, classifying a read failure as
    /// [`SessionLost`](ExternalAgentError::SessionLost).
    async fn read_line(&mut self) -> Result<Option<String>, ExternalAgentError> {
        let session = self.maybe_session_ref();
        self.transport
            .read_line()
            .await
            .map_err(|error| with_session(session, error))
    }

    /// Drains the decoder's buffered observations, mirroring each to the live sink
    /// and advancing the high-water `seq`.
    fn drain_and_emit(&mut self) -> Vec<ExternalObservedEvent> {
        let observed = self.decoder.take_observations();
        process::emit_observations(&observed, self.sink.as_ref(), &mut self.last_event_seq);
        observed
    }

    /// Sends `session/prompt` carrying `text` as a single text content block.
    async fn send_prompt(&mut self, text: &str) -> Result<(), ExternalAgentError> {
        let id = self.next_id();
        self.send_request(
            id,
            "session/prompt",
            json!({
                "sessionId": self.session_id,
                "prompt": [ { "type": "text", "text": text } ],
            }),
        )
        .await
    }

    /// Reads agent lines, servicing client requests inline, until the turn settles
    /// on a decision or pauses for a permission interaction.
    ///
    /// Each line read is raced against run cancellation (M3-1): a silent agent
    /// must not hold a cancel hostage until a line arrives or the read timeout
    /// fires. The configured read timeout stays armed inside
    /// [`read_line`](Self::read_line) as the last-resort error for a genuinely
    /// dead peer, so a slow-but-alive agent is unaffected.
    async fn read_to_decision(
        &mut self,
        mut collected: Vec<ExternalObservedEvent>,
        ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        loop {
            // `biased` lets an already-landed cancel win over a simultaneously
            // ready line; `CancellationToken::cancelled` resolves immediately
            // for a pre-cancelled run, so no separate pre-check is needed.
            let line = tokio::select! {
                biased;
                () = ctx.cancellation().cancelled() => None,
                line = self.read_line() => Some(line?),
            };
            let Some(line) = line else {
                return Err(ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: "acp session advance was cancelled".to_owned(),
                });
            };
            let Some(line) = line else {
                return Err(ExternalAgentError::SessionLost {
                    session: self.maybe_session_ref(),
                    detail: "acp connection closed before reaching a decision point".to_owned(),
                });
            };
            let decision = self.decoder.push_jsonrpc_line(&line)?;
            collected.extend(self.drain_and_emit());

            for request in self.decoder.take_client_requests() {
                match request {
                    PendingClientRequest::Permission {
                        action_id,
                        summary,
                        options,
                    } => {
                        let interaction = self.permission_interaction(&action_id, summary);
                        self.pending = Some(PendingPermission {
                            request_id: action_id.clone(),
                            interaction: interaction.clone(),
                            options,
                        });
                        return Ok(RuntimeDecisionPoint::PausedForInteraction {
                            session: self.session_ref(),
                            action_id,
                            request: interaction,
                            observations: collected,
                        });
                    }
                    PendingClientRequest::ReadFile {
                        action_id,
                        path,
                        line,
                        limit,
                    } => {
                        self.service_read_file(&action_id, &path, line, limit)
                            .await?;
                    }
                    PendingClientRequest::WriteFile {
                        action_id,
                        path,
                        content,
                    } => {
                        self.service_write_file(&action_id, &path, &content).await?;
                        collected.extend(self.drain_and_emit());
                    }
                    PendingClientRequest::Terminal { action_id, method } => {
                        self.reject_terminal(&action_id, &method).await?;
                    }
                }
            }

            if let Some(decision) = decision {
                return self.finish(decision, collected);
            }
        }
    }

    /// Builds the host-bound permission [`Interaction`] for a paused request.
    ///
    /// The step id comes from the caller-supplied
    /// [`run_id`](RunContext::run_id) and the actor is the request's own agent —
    /// never anything the runtime reports.
    fn permission_interaction(&self, action_id: &str, summary: String) -> Interaction {
        let request = PermissionRequest::new(
            action_id.to_owned(),
            self.actor,
            PermissionCategory::Other,
            summary,
            Value::Null,
            PermissionRisk::Medium,
            None,
        );
        Interaction::permission(self.step_id, request)
    }

    /// Writes the ACP permission response for a resolved host interaction.
    ///
    /// The response is validated against the pending [`Interaction`] with
    /// [`accepts_response`](Interaction::accepts_response) before it is mapped onto
    /// one of the agent's offered options and written back.
    async fn answer_permission(
        &mut self,
        action_id: &str,
        response: &InteractionResponse,
    ) -> Result<(), ExternalAgentError> {
        let pending = self
            .pending
            .take()
            .ok_or_else(|| ExternalAgentError::Protocol {
                detail: "acp received a permission response with no pending request".to_owned(),
            })?;
        if action_id != pending.request_id {
            return Err(ExternalAgentError::Protocol {
                detail: "acp permission response addresses a different action".to_owned(),
            });
        }
        pending
            .interaction
            .accepts_response(response)
            .map_err(|error| ExternalAgentError::Protocol {
                detail: format!("acp permission response rejected: {error}"),
            })?;
        let InteractionResponse::Permission(permission) = response else {
            return Err(ExternalAgentError::Protocol {
                detail: "acp permission pause requires a permission response".to_owned(),
            });
        };
        let outcome = permission_outcome(permission.decision(), &pending.options);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": json_rpc_id_value(&pending.request_id),
            "result": { "outcome": outcome },
        })
        .to_string();
        self.write_line(&frame).await
    }

    /// Services a `fs/read_text_file` request against the working directory.
    async fn service_read_file(
        &mut self,
        action_id: &str,
        path: &str,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> Result<(), ExternalAgentError> {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => {
                let content = apply_line_window(&content, line, limit);
                self.respond_result(action_id, json!({ "content": content }))
                    .await
            }
            Err(error) => {
                self.respond_error(
                    action_id,
                    JSONRPC_INTERNAL_ERROR,
                    &format!("failed reading file: {:?}", error.kind()),
                )
                .await
            }
        }
    }

    /// Services a `fs/write_text_file` request against the working directory,
    /// enforcing the permission mode (a plan session refuses writes) and surfacing
    /// a successful write as a [`FilePatch`] observation.
    ///
    /// [`FilePatch`]: crate::agent::external::ExternalAgentEvent::FilePatch
    async fn service_write_file(
        &mut self,
        action_id: &str,
        path: &str,
        content: &str,
    ) -> Result<(), ExternalAgentError> {
        if matches!(self.permission_mode, ExternalPermissionMode::Plan) {
            return self
                .respond_error(
                    action_id,
                    JSONRPC_INTERNAL_ERROR,
                    "filesystem writes are disabled in plan mode",
                )
                .await;
        }
        let target = Path::new(path);
        if let Some(parent) = target.parent()
            && let Err(error) = tokio::fs::create_dir_all(parent).await
        {
            return self
                .respond_error(
                    action_id,
                    JSONRPC_INTERNAL_ERROR,
                    &format!("failed creating parent directory: {:?}", error.kind()),
                )
                .await;
        }
        if let Err(error) = tokio::fs::write(target, content).await {
            return self
                .respond_error(
                    action_id,
                    JSONRPC_INTERNAL_ERROR,
                    &format!("failed writing file: {:?}", error.kind()),
                )
                .await;
        }
        self.decoder.note_file_patch(path.to_owned());
        self.respond_result(action_id, json!({})).await
    }

    /// Rejects a `terminal/*` request; the client advertised `terminal: false`.
    async fn reject_terminal(
        &mut self,
        action_id: &str,
        method: &str,
    ) -> Result<(), ExternalAgentError> {
        self.respond_error(
            action_id,
            JSONRPC_METHOD_NOT_FOUND,
            &format!("{method} is not supported by this client"),
        )
        .await
    }

    /// Writes a JSON-RPC success response for a serviced client request.
    async fn respond_result(
        &mut self,
        action_id: &str,
        result: Value,
    ) -> Result<(), ExternalAgentError> {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": json_rpc_id_value(action_id),
            "result": result,
        })
        .to_string();
        self.write_line(&frame).await
    }

    /// Writes a JSON-RPC error response for a client request we cannot fulfil.
    async fn respond_error(
        &mut self,
        action_id: &str,
        code: i64,
        message: &str,
    ) -> Result<(), ExternalAgentError> {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": json_rpc_id_value(action_id),
            "error": { "code": code, "message": message },
        })
        .to_string();
        self.write_line(&frame).await
    }

    /// Folds a settled [`AcpDecision`] into a [`RuntimeDecisionPoint`].
    fn finish(
        &self,
        decision: AcpDecision,
        observations: Vec<ExternalObservedEvent>,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        match decision {
            AcpDecision::Completed { output } => Ok(RuntimeDecisionPoint::Completed {
                session: self.session_ref(),
                output,
                observations,
            }),
            AcpDecision::Failed { error } => Err(error),
        }
    }

    /// Returns the session facts, or `None` before an id has been assigned.
    fn maybe_session_ref(&self) -> Option<ExternalSessionRef> {
        process::maybe_session_ref_for_id(acp_runtime_kind(), &self.session_id, self.last_event_seq)
    }
}

#[async_trait]
impl ExternalRuntimeSession for AcpSession {
    fn session_ref(&self) -> ExternalSessionRef {
        process::session_ref_for_id(acp_runtime_kind(), &self.session_id, self.last_event_seq)
    }

    async fn advance(
        &mut self,
        input: &ExternalSessionInput,
        ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        let collected = std::mem::take(&mut self.carried);
        match input {
            ExternalSessionInput::Start { prompt } => self.send_prompt(prompt).await?,
            ExternalSessionInput::Continue { message } => self.send_prompt(message).await?,
            ExternalSessionInput::RespondInteraction {
                action_id,
                response,
            } => self.answer_permission(action_id, response).await?,
            ExternalSessionInput::RespondToolResults { .. } => {
                return Err(self.capabilities.unsupported(
                    ExternalCapability::HostTools,
                    "acp adapter does not bridge host tool results",
                ));
            }
            ExternalSessionInput::RespondSubagent { .. } => {
                return Err(self.capabilities.unsupported(
                    ExternalCapability::HostSubagents,
                    "acp adapter does not bridge host subagents",
                ));
            }
            ExternalSessionInput::Shutdown => {
                // The graceful stop is driven by `shutdown`; there is no prompt
                // turn to run and reading further would hang.
                return Ok(RuntimeDecisionPoint::Completed {
                    session: self.session_ref(),
                    output: ExternalAgentOutput {
                        summary: String::new(),
                        artifacts: Vec::new(),
                        usage: None,
                        cost_micros: None,
                    },
                    observations: collected,
                });
            }
        }
        self.read_to_decision(collected, ctx).await
    }

    async fn shutdown(&mut self) -> ExternalSessionShutdown {
        if !self.session_id.is_empty() {
            // Best-effort cancel; a failure just means the agent is already gone.
            let params = json!({ "sessionId": self.session_id });
            let _ = self.send_notification("session/cancel", params).await;
        }
        self.transport.close(self.shutdown_grace).await
    }
}

/// Managed adapter that starts and resumes live ACP sessions.
///
/// This is the module's only public type. Construct one from an [`AcpConfig`] with
/// [`new`](Self::new) (assuming a fully capable agent) or
/// [`with_probed_capabilities`](Self::with_probed_capabilities) to intersect the
/// adapter's implemented features with what an `initialize` handshake negotiated
/// (via [`capabilities_from_initialize`](super::capabilities_from_initialize)).
/// Wrap the adapter in an
/// [`ExternalSessionRegistry`](crate::agent::external::ExternalSessionRegistry) to
/// own its live sessions between decision points.
pub struct AcpAdapter {
    config: AcpConfig,
    capabilities: ExternalRuntimeCapabilities,
    launcher: Arc<dyn AcpLauncher>,
}

impl AcpAdapter {
    /// Builds an adapter for `config` reporting every managed feature this adapter
    /// implements.
    ///
    /// The reported set is fixed: streaming, resume, permission bridging, and
    /// graceful shutdown are on; host-tool and host-subagent bridging are off (no
    /// client MCP bridge), and artifacts/usage stay off until the crate surfaces
    /// them. Prefer [`with_probed_capabilities`](Self::with_probed_capabilities)
    /// when a handshake has negotiated what the live agent actually advertises.
    #[must_use]
    pub fn new(config: AcpConfig) -> Self {
        Self {
            config,
            capabilities: implemented_capabilities(),
            launcher: Arc::new(TokioProcessLauncher),
        }
    }

    /// Builds an adapter whose reported capabilities are the intersection of what
    /// this adapter implements and what an `initialize` handshake negotiated.
    ///
    /// A feature is reported supported only when *both* the adapter implements it
    /// and the negotiation confirmed it, so an agent lacking `session/load`
    /// disables resume while host-tool bridging stays off regardless (this adapter
    /// never serves it).
    #[must_use]
    pub fn with_probed_capabilities(
        config: AcpConfig,
        probed: &ExternalRuntimeCapabilities,
    ) -> Self {
        Self {
            config,
            capabilities: process::intersect_capabilities(&implemented_capabilities(), probed),
            launcher: Arc::new(TokioProcessLauncher),
        }
    }

    /// Builds an adapter that launches sessions through a custom
    /// [`AcpLauncher`], reporting every managed feature this adapter implements.
    ///
    /// Production wires [`TokioProcessLauncher`] through [`new`](Self::new); this
    /// constructor lets a test (or an embedder) inject a launcher that spawns a
    /// wrapped binary or an in-memory transport, keeping the whole session state
    /// machine drivable offline.
    #[must_use]
    pub fn with_launcher(config: AcpConfig, launcher: Arc<dyn AcpLauncher>) -> Self {
        Self {
            config,
            capabilities: implemented_capabilities(),
            launcher,
        }
    }

    /// Returns the launch configuration backing this adapter.
    #[must_use]
    pub const fn config(&self) -> &AcpConfig {
        &self.config
    }

    /// Refuses a request that declares host tools this adapter cannot inject.
    fn reject_unsupported_tools(
        &self,
        request: &ExternalSessionRequest,
    ) -> Result<(), ExternalAgentError> {
        process::reject_unsupported_tools(
            &self.capabilities,
            request,
            "acp adapter cannot inject host tools without a client MCP bridge",
        )
    }

    /// Resolves the effective session configuration for `request`.
    ///
    /// Request-level policy wins over the construction-time config (M2-7 /
    /// M-PROM-5): [`ExternalSessionPolicy::permission_mode`] overrides
    /// [`with_permission_mode`](AcpConfig::with_permission_mode), and a prepared
    /// [`session_dir`](ExternalSessionRequest::session_dir) overrides
    /// [`with_working_dir`](AcpConfig::with_working_dir).
    fn session_config(&self, request: &ExternalSessionRequest) -> AcpConfig {
        let mut config = self
            .config
            .clone()
            .with_permission_mode(request.policy.permission_mode);
        if let Some(dir) = &request.session_dir {
            config = config.with_working_dir(dir.path().to_path_buf());
        }
        config
    }

    /// Resolves the working directory a session runs in: the request's prepared
    /// session dir when the session layer resolved one, then the configured
    /// working dir, otherwise the request's worktree.
    fn session_cwd(&self, request: &ExternalSessionRequest) -> PathBuf {
        request
            .session_dir
            .as_ref()
            .map(|dir| dir.path().to_path_buf())
            .or_else(|| self.config.working_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| request.worktree.path().to_path_buf())
    }

    /// Builds a session over a freshly launched transport.
    ///
    /// The session's permission mode comes from the request's policy (the
    /// request-level override, M2-7), so the plan-mode write gate inside the
    /// session follows the per-session policy rather than the construction-time
    /// config.
    fn session_over(
        &self,
        transport: SpawnedAcpAgent,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> AcpSession {
        AcpSession::new(
            transport,
            StepId::new(*ctx.run_id().as_uuid()),
            request.agent_id,
            sink,
            self.capabilities.clone(),
            request.policy.permission_mode,
            self.session_cwd(request),
            self.config.timeout(),
        )
    }
}

#[async_trait]
impl ExternalRuntimeAdapter for AcpAdapter {
    fn kind(&self) -> ExternalRuntimeKind {
        acp_runtime_kind()
    }

    fn capabilities(&self) -> ExternalRuntimeCapabilities {
        self.capabilities.clone()
    }

    async fn start(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
        self.reject_unsupported_tools(request)?;
        let config = self.session_config(request);
        let transport = self.launcher.launch(&config).await?;
        let mut session = self.session_over(transport, request, ctx, sink);
        session.begin(None).await?;
        Ok(Box::new(session))
    }

    async fn resume(
        &self,
        session: &ExternalSessionRef,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
        self.reject_unsupported_tools(request)?;
        let Some(session_id) = session.session_id.clone() else {
            return Err(ExternalAgentError::ResumeUnavailable {
                session: session.clone(),
                detail: "acp session has no id to resume".to_owned(),
            });
        };
        let config = self.session_config(request);
        let transport = self.launcher.launch(&config).await.map_err(|error| {
            ExternalAgentError::ResumeUnavailable {
                session: session.clone(),
                detail: format!("failed launching acp agent to resume: {error}"),
            }
        })?;
        let mut live = self
            .session_over(transport, request, ctx, sink)
            .with_resume_high_water(session.last_event_seq);
        live.begin(Some(session_id)).await?;
        Ok(Box::new(live))
    }
}

/// JSON-RPC reserved error code for an unsupported method.
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC reserved error code for an internal client error.
const JSONRPC_INTERNAL_ERROR: i64 = -32603;

/// Returns the managed features this adapter can actually fulfil.
///
/// Host-tool and host-subagent bridging are off (no client MCP bridge) and
/// artifacts/usage stay off until the crate surfaces them; the rest are on because
/// `session/update` streaming, the `session/request_permission` control channel,
/// `session/load`, and `session/cancel` plus a clean connection close back them.
/// Resume is reported optimistically here and refined per session:
/// [`resume`](AcpAdapter::resume) returns
/// [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable) when the agent did
/// not advertise `session/load`.
fn implemented_capabilities() -> ExternalRuntimeCapabilities {
    let mut capabilities = ExternalRuntimeCapabilities::none(acp_runtime_kind());
    capabilities.streaming = true;
    capabilities.resume = true;
    capabilities.permission_bridge = true;
    capabilities.graceful_shutdown = true;
    capabilities
}

/// Projects an `initialize` result into the neutral negotiated-capability record.
fn negotiated_from_initialize(
    result: &serde_json::Map<String, Value>,
) -> AcpNegotiatedCapabilities {
    let load_session = result
        .get("agentCapabilities")
        .and_then(|caps| caps.get("loadSession"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    AcpNegotiatedCapabilities::none()
        .with_load_session(load_session)
        // The client advertised `fs` services and no `terminal`; recorded for
        // completeness even though neither widens a managed capability bit.
        .with_fs(true)
        .with_terminal(false)
}

/// Classifies a JSON-RPC error object as a [`Runtime`](ExternalAgentError::Runtime)
/// failure. The agent-reported message is untrusted text, so it is preserved in
/// `runtime_output` while `message` stays a fixed diagnostic that cannot leak it
/// via `Display`.
fn classify_error(error: &serde_json::Map<String, Value>) -> ExternalAgentError {
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .map(|code| code.to_string());
    let runtime_output = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned);
    ExternalAgentError::Runtime {
        code,
        message: "acp agent reported an error".to_owned(),
        runtime_output,
    }
}

/// Re-tags a transport [`SessionLost`](ExternalAgentError::SessionLost) with the
/// known session ref; other errors pass through unchanged.
fn with_session(
    session: Option<ExternalSessionRef>,
    error: ExternalAgentError,
) -> ExternalAgentError {
    match error {
        ExternalAgentError::SessionLost { detail, .. } => {
            ExternalAgentError::SessionLost { session, detail }
        }
        other => other,
    }
}

/// Renders a captured JSON-RPC id string back to its wire value, preserving the
/// numeric form most ACP agents use so the response id correlates exactly.
fn json_rpc_id_value(id: &str) -> Value {
    id.parse::<i64>().map_or_else(
        |_| Value::String(id.to_owned()),
        |number| Value::Number(number.into()),
    )
}

/// Maps a host [`PermissionDecision`] onto the ACP permission `outcome` object,
/// selecting one of the agent's offered options.
///
/// An [`Approve`](PermissionDecision::Approve) selects an allow option, a
/// [`Deny`](PermissionDecision::Deny) selects a reject option, and a
/// [`Cancel`](PermissionDecision::Cancel) — or a decision with no matching option
/// — resolves as `cancelled`.
fn permission_outcome(decision: &PermissionDecision, options: &[AcpPermissionOption]) -> Value {
    let selected = match decision {
        PermissionDecision::Approve => select_option(options, true),
        PermissionDecision::Deny { .. } => select_option(options, false),
        PermissionDecision::Cancel => None,
    };
    match selected {
        Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
        None => json!({ "outcome": "cancelled" }),
    }
}

/// Picks the option id that grants (`allow`) or refuses (`!allow`) the action,
/// preferring the "once" variant over the "always" variant.
fn select_option(options: &[AcpPermissionOption], allow: bool) -> Option<String> {
    let (once, always) = if allow {
        (
            AcpPermissionOptionKind::AllowOnce,
            AcpPermissionOptionKind::AllowAlways,
        )
    } else {
        (
            AcpPermissionOptionKind::RejectOnce,
            AcpPermissionOptionKind::RejectAlways,
        )
    };
    options
        .iter()
        .find(|option| option.kind == once)
        .or_else(|| options.iter().find(|option| option.kind == always))
        .or_else(|| {
            options
                .iter()
                .find(|option| option.kind.is_allow() == allow)
        })
        .map(|option| option.option_id.clone())
}

/// Applies an optional 1-based `line`/`limit` window to file `content`.
fn apply_line_window(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return content.to_owned();
    }
    let start = line.unwrap_or(1).saturating_sub(1) as usize;
    let take = limit.map_or(usize::MAX, |limit| limit as usize);
    content
        .lines()
        .skip(start)
        .take(take)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;
