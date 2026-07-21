//! Offline coverage for the model-routed delegation path (milestone M3-2).
//!
//! Every test is fully offline: a [`RoutingClient`] returns scripted responses
//! selected by the requesting agent's system prompt, so the supervisor and each
//! child are driven deterministically with no network, credential, or CLI, and
//! each finishes well under a second.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::{Map, json};

use crate::agent::{
    AgentId, ApprovalResponse, Interaction, InteractionHandler, InteractionKind,
    InteractionResponse, RequirementResult, RunContext,
};
use crate::client::{Capability, ChatRequest, ClientError, LlmClient, Response};
use crate::facade::approval::{Approval, ApprovalDecision, ApprovalPolicy};
use crate::facade::run::{DelegationStatus, RunEvent};
use crate::facade::{Agent, AgentBuilder, CancelHandle};
use crate::facade::{Delegation, DelegationSnapshot};
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::normalized::StopReason;
use crate::model::tool::Tool as ToolDecl;
use crate::model::usage::Usage;
use crate::stream::StreamEvent;

/// One system-prompt-keyed script: responses are returned in order, repeating
/// the last once exhausted.
struct Route {
    marker: &'static str,
    responses: Vec<Response>,
    calls: Mutex<usize>,
}

/// A client that dispatches each `chat` to the [`Route`] whose marker appears
/// in the request's system prompt, so a supervisor and its children can be
/// scripted independently while sharing one client handle.
struct RoutingClient {
    routes: Vec<Route>,
}

impl RoutingClient {
    fn new(routes: Vec<Route>) -> Arc<Self> {
        Arc::new(Self { routes })
    }

    fn respond(&self, system: Option<&str>) -> Response {
        let system = system.unwrap_or_default();
        let route = self
            .routes
            .iter()
            .find(|route| system.contains(route.marker))
            .expect("a route matches the request system prompt");
        let mut calls = route
            .calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let index = (*calls).min(route.responses.len() - 1);
        *calls += 1;
        route.responses[index].clone()
    }
}

#[async_trait]
impl LlmClient for RoutingClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        Ok(self.respond(request.system.as_deref()))
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        Err(ClientError::Other(
            "streaming not used in fixture".to_owned(),
        ))
    }
}

fn route(marker: &'static str, responses: Vec<Response>) -> Route {
    Route {
        marker,
        responses,
        calls: Mutex::new(0),
    }
}

/// An assistant response carrying only `text`.
fn text_response(text: &str) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        },
        usage: Usage {
            input: 11,
            output: 7,
            ..Usage::default()
        },
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

/// An assistant response asking to call `tool` with the given provider id and
/// JSON `input`.
fn tool_call_response(id: &str, tool: &str, input: serde_json::Value) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.to_owned(),
                name: tool.to_owned(),
                input,
                extra: Map::new(),
            }],
        },
        usage: Usage {
            input: 5,
            output: 3,
            ..Usage::default()
        },
        stop_reason: StopReason::normalize("tool_use"),
        extra: Map::new(),
    }
}

/// Collects the text of every tool-result block committed in the agent's
/// conversation, so a folded delegation summary can be asserted directly.
fn tool_result_texts(agent: &Agent) -> Vec<String> {
    let mut texts = Vec::new();
    for turn in agent.conversation().turns() {
        for message in turn.messages() {
            for block in &message.payload().content {
                if let ContentBlock::ToolResult { content, .. } = block {
                    for inner in content {
                        if let ContentBlock::Text { text, .. } = inner {
                            texts.push(text.clone());
                        }
                    }
                }
            }
        }
    }
    texts
}

fn shell_decl() -> ToolDecl {
    ToolDecl {
        name: "shell".to_owned(),
        description: "Run a shell command.".to_owned(),
        input_schema: json!({ "type": "object" }),
    }
}

fn approval_interaction_result(
    request: &Interaction,
    decision: ApprovalDecision,
) -> RequirementResult {
    match request.kind() {
        InteractionKind::Approval { call_id, .. } => {
            RequirementResult::Interaction(InteractionResponse::Approval(ApprovalResponse::new(
                request.step_id(),
                *call_id,
                decision,
                None,
            )))
        }
        _ => RequirementResult::Interaction(InteractionResponse::answer(String::new())),
    }
}

/// Parent-side test handler that records every forwarded interaction before
/// returning a fixed approval decision.
struct RecordingParentInteractionHandler {
    decision: ApprovalDecision,
    seen: Mutex<Vec<Interaction>>,
}

impl RecordingParentInteractionHandler {
    fn new(decision: ApprovalDecision) -> Self {
        Self {
            decision,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<Interaction> {
        self.seen.lock().expect("seen mutex").clone()
    }
}

#[async_trait]
impl InteractionHandler for RecordingParentInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        self.seen.lock().expect("seen mutex").push(request.clone());
        approval_interaction_result(request, self.decision)
    }
}

/// Parent-side handler that proves the routing layer can abandon a parked
/// interaction when the run is cancelled, even if the handler never returns.
struct ParkingParentInteractionHandler {
    reached: Mutex<Option<tokio::sync::oneshot::Sender<Interaction>>>,
}

impl ParkingParentInteractionHandler {
    fn new() -> (Arc<Self>, tokio::sync::oneshot::Receiver<Interaction>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                reached: Mutex::new(Some(tx)),
            }),
            rx,
        )
    }
}

#[async_trait]
impl InteractionHandler for ParkingParentInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        if let Some(sender) = self.reached.lock().expect("reached mutex").take() {
            let _ = sender.send(request.clone());
        }
        std::future::pending::<RequirementResult>().await
    }
}

/// M3-R (C9): a parent handler that answers every interaction with a
/// wrong-family response, proving a mismatched answer to the
/// external-start ask is treated as a denial rather than a start.
struct MismatchedFamilyParentHandler {
    seen: Mutex<Vec<Interaction>>,
}

impl MismatchedFamilyParentHandler {
    fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<Interaction> {
        self.seen.lock().expect("seen mutex").clone()
    }
}

#[async_trait]
impl InteractionHandler for MismatchedFamilyParentHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        self.seen.lock().expect("seen mutex").push(request.clone());
        RequirementResult::Interaction(InteractionResponse::answer(String::new()))
    }
}

/// A minimal in-crate [`ExternalSessionHandler`](crate::agent::ExternalSessionHandler)
/// double that returns a fixed [`ExternalSessionResult`] on every `fulfill`.
///
/// An in-crate unit test cannot use `agent-testkit`'s scripted handler: the
/// testkit implements the trait against the *dependency* copy of `agent-lib`,
/// which the test harness treats as a different crate than the `crate::` under
/// test. This local double implements the `crate::` trait directly, keeping
/// the delegation drive fully offline.
struct FixedExternalSessionHandler {
    result: crate::agent::ExternalSessionResult,
}

#[async_trait]
impl crate::agent::ExternalSessionHandler for FixedExternalSessionHandler {
    async fn fulfill(
        &self,
        _request: &crate::agent::ExternalSessionRequest,
        _ctx: &crate::agent::RunContext,
    ) -> crate::agent::RequirementResult {
        crate::agent::RequirementResult::ExternalSession(Box::new(self.result.clone()))
    }
}

/// Builds a [`FixedExternalSessionHandler`] that completes with `summary`, one
/// patch artifact at `path`, and the given runtime-reported `usage`, plus a
/// command/patch observation trail.
fn completed_external_handler(
    summary: &str,
    path: &str,
    usage: Usage,
) -> FixedExternalSessionHandler {
    use crate::agent::external::{
        ExternalAgentEvent, ExternalAgentOutput, ExternalArtifactKind, ExternalArtifactRef,
        ExternalObservedEvent, ExternalRuntimeKind, ExternalSessionRef, ExternalSessionResult,
    };

    let result = ExternalSessionResult::Completed {
        session: ExternalSessionRef {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: Some("sess-1".to_owned()),
            transcript_ref: None,
            resume_token: Some("resume-1".to_owned()),
            last_event_seq: Some(2),
        },
        output: ExternalAgentOutput {
            summary: summary.to_owned(),
            artifacts: vec![ExternalArtifactRef {
                kind: ExternalArtifactKind::Patch,
                summary: "parser patch".to_owned(),
                path: Some(path.to_owned()),
                reference: Some("diff-1".to_owned()),
            }],
            usage: Some(usage),
            cost_micros: None,
        },
        observations: ExternalObservedEvent::unsequenced_for_tests(vec![
            ExternalAgentEvent::CommandFinished {
                exit_code: Some(0),
                stdout_tail: "test result: ok. 1 passed".to_owned(),
                stderr_tail: String::new(),
            },
            ExternalAgentEvent::FilePatch {
                path: path.to_owned(),
                summary: "tighten the token loop".to_owned(),
                diff_ref: Some("diff-1".to_owned()),
            },
        ]),
    };
    FixedExternalSessionHandler { result }
}

/// Builds a [`FixedExternalSessionHandler`] that completes with `summary` and
/// a trail of the three collaboration observations §14 bridges: a directed
/// `send_message`, a `plan_update`, and a `blackboard_post`.
fn collab_external_handler(summary: &str, recipient: AgentId) -> FixedExternalSessionHandler {
    use crate::agent::external::{
        ExternalAgentEvent, ExternalAgentOutput, ExternalObservedEvent, ExternalRuntimeKind,
        ExternalSessionRef, ExternalSessionResult,
    };

    let result = ExternalSessionResult::Completed {
        session: ExternalSessionRef {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: Some("sess-collab".to_owned()),
            transcript_ref: None,
            resume_token: None,
            last_event_seq: Some(2),
        },
        output: ExternalAgentOutput {
            summary: summary.to_owned(),
            artifacts: Vec::new(),
            usage: None,
            cost_micros: None,
        },
        observations: ExternalObservedEvent::unsequenced_for_tests(vec![
            ExternalAgentEvent::MessageSent {
                to: recipient,
                summary: "please review the parser change".to_owned(),
            },
            ExternalAgentEvent::TaskUpdated {
                task_id: "parser".to_owned(),
                status: "completed".to_owned(),
            },
            ExternalAgentEvent::BlackboardPosted {
                channel: "status".to_owned(),
                summary: "parser done".to_owned(),
            },
        ]),
    };
    FixedExternalSessionHandler { result }
}

/// Builds a supervisor client that delegates once to `ask_coder` then closes
/// with a final message, for the external approval/restore tests.
fn external_supervisor_client() -> Arc<RoutingClient> {
    RoutingClient::new(vec![route(
        "SUPERVISOR",
        vec![
            tool_call_response("del-1", "ask_coder", json!({ "task": "refactor" })),
            text_response("Final: done."),
        ],
    )])
}

/// Builds a `coder` external agent whose scripted session completes.
fn completed_coder() -> crate::facade::ManagedExternalAgent {
    crate::facade::ManagedExternalAgent::claude_code()
        .session_handler(Arc::new(completed_external_handler(
            "refactor complete",
            "src/parser.rs",
            Usage {
                input: 4,
                output: 2,
                ..Usage::default()
            },
        )))
        .build()
        .expect("managed external agent builds")
}

/// Builds a local worker subagent whose scripted client route is keyed by the
/// marker embedded in its system prompt.
fn dispatch_worker(system: &str) -> super::LocalSubagent {
    Agent::worker()
        .description("A dispatcher worker.")
        .system(system)
        .build()
        .expect("worker builds")
}

/// Extracts the `(from, to)` of the first [`RunEvent::Escalated`], if any.
fn escalation_edge(output: &crate::facade::run::RunOutput) -> Option<(String, String)> {
    output.events.iter().find_map(|event| match event {
        RunEvent::Escalated(trace) => Some((trace.from.clone(), trace.to.clone())),
        _ => None,
    })
}

/// Core model-routed coverage (§13.1): the per-delegate `ask_<name>` tools,
/// the unified single-tool route (§10.2), and multi-delegate routing.
mod model_routed;

/// Child interaction/approval routing: the child's paused asks forward to
/// the supervisor's injected handler, and cancellation abandons them cleanly.
mod child_interaction;

/// Managed external delegate coverage: driving, collab bridging, the §9.2
/// start-approval gate, and external snapshot/restore (§15.2).
mod external;

/// Local-delegate snapshot coverage: data-only persistence and restore (§15.2).
mod snapshot;

/// Rules-routed delegation coverage (`docs/facade-api.md` §13.2).
mod rules_routed;

/// Dispatcher-routed delegation coverage (§13.3) and the §19 AI-decision
/// injection seams.
mod dispatcher;
