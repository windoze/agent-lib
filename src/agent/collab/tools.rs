//! Bridge tool adapters that expose the collaboration primitives to agents.
//!
//! Design `agent-layer.md` §5 / `external-agent.md` §3.4 mandate an **API-first**
//! shape: each vertical feature is a first-class Rust API ([`Plan`],
//! [`Blackboard`], [`Mailbox`]) and the model-facing tool is a *thin adapter* over
//! it. These adapters never bypass the host's guards:
//!
//! - Every adapter dispatch first checks the shared [`RunContext`] for
//!   cancellation, so a cancelled run refuses further tool work.
//! - The claiming / posting / sending identity is the **injected** agent
//!   identity, never a model-supplied argument — an agent cannot claim a plan
//!   task as, or post as, someone else.
//! - `plan_claim` enforces dependency completion through [`Plan::claim`]
//!   (design §6.2), so a dependency-blocked claim changes nothing.
//! - `send_message` writes the library [`Mailbox`], not an external runtime's
//!   private inbox (design §3.5); `mailbox_read` reads the **calling** agent's
//!   own inbox — the recipient identity is the injected one, so an agent cannot
//!   read someone else's mail.
//! - `blackboard_read` / `mailbox_read` paginate by cursor (`from`) and page
//!   size (`limit`), and truncate long message bodies, so a busy board or a
//!   huge post cannot blow up the model's context window.
//!
//! `spawn_agent` is special: spawning a child *deepens the scope chain*, which a
//! plain tool execution cannot do. It is therefore modeled as a **translation**
//! ([`SpawnAgentRequest`]) from a tool call into a
//! [`RequirementKind::NeedSubagent`], reusing the existing
//! [`SubagentHandler`](crate::agent::SubagentHandler) derivation path rather than
//! introducing any new orchestration runtime (design §6.3). A host that wires
//! `spawn_agent` intercepts the call and emits the requirement; the
//! [`CollabToolHandler`] declines to run it inline.
//!
//! [`bridge_tool_set`] packages every declaration into a [`ToolSetRef`] a host
//! injects as an external (or internal) agent's `initial_tools`, and
//! [`CollabToolHandler`] is the matching [`ToolHandler`] that executes the
//! inline (non-spawn) tools.

use crate::agent::{
    context::RunContext,
    drive::ToolHandler,
    external::{ExternalArtifactKind, ExternalArtifactRef},
    id::{AgentId, StepId, ToolSetId},
    interaction::Interaction,
    requirement::{AgentSpecRef, RequirementKind, RequirementResult},
    spec::ToolSetRef,
    tool::{ToolRegistry, ToolRuntimeError},
};
use crate::conversation::ToolCallId;
use crate::model::{
    content::ContentBlock,
    tool::{Tool, ToolCall, ToolResponse, ToolStatus},
};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};
use thiserror::Error;

use super::{Blackboard, Mailbox, Plan, TaskStatus};

/// Tool name: derive and drive a child agent (translated to `NeedSubagent`).
pub const SPAWN_AGENT: &str = "spawn_agent";
/// Tool name: add a task to the shared plan.
pub const PLAN_ADD_TASK: &str = "plan_add_task";
/// Tool name: read the shared plan (version + tasks) for a later CAS.
pub const PLAN_READ: &str = "plan_read";
/// Tool name: claim a specific plan task.
pub const PLAN_CLAIM: &str = "plan_claim";
/// Tool name: claim the first available plan task.
pub const PLAN_CLAIM_FIRST_AVAILABLE: &str = "plan_claim_first_available";
/// Tool name: update the status of an owned plan task.
pub const PLAN_UPDATE: &str = "plan_update";
/// Tool name: post a message to the shared blackboard.
pub const BLACKBOARD_POST: &str = "blackboard_post";
/// Tool name: read messages from the shared blackboard.
pub const BLACKBOARD_READ: &str = "blackboard_read";
/// Tool name: send a direct message to another agent's mailbox.
pub const SEND_MESSAGE: &str = "send_message";
/// Tool name: read the calling agent's own mailbox inbox.
pub const MAILBOX_READ: &str = "mailbox_read";
/// Tool name: record a produced artifact (patch / diff / test result / file).
pub const REPORT_ARTIFACT: &str = "report_artifact";
/// Tool name: invoke a host-registered tool under the run's guards.
pub const RUN_HOST_TOOL: &str = "run_host_tool";

/// Default page size (in messages) for `blackboard_read` / `mailbox_read` when
/// the call carries no explicit `limit`.
const DEFAULT_READ_LIMIT: usize = 50;

/// Maximum body characters shown per message in a read page; longer bodies are
/// truncated so a single huge post cannot flood the model's context window.
const MAX_MESSAGE_BODY_CHARS: usize = 200;

// ----- tool declarations ---------------------------------------------------

/// Builds one [`Tool`] declaration from a name, description, and JSON schema.
fn tool(name: &str, description: &str, input_schema: Value) -> Tool {
    Tool {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
    }
}

/// Returns the full set of bridge [`Tool`] declarations.
///
/// The declarations are what a model sees; [`CollabToolHandler`] executes the
/// inline (non-spawn) ones and a host translates [`SPAWN_AGENT`] into a
/// [`RequirementKind::NeedSubagent`] via [`SpawnAgentRequest`].
#[must_use]
pub fn bridge_tool_declarations() -> Vec<Tool> {
    vec![
        tool(
            SPAWN_AGENT,
            "Derive and drive a child agent for a delegated task; \
             the host turns this into a subagent requirement.",
            json!({
                "type": "object",
                "properties": {
                    "spec": { "type": "string", "description": "child agent spec id (UUID)" },
                    "brief": { "type": "string", "description": "task brief for the child agent" },
                    "result_schema": {
                        "type": "object",
                        "description": "optional JSON schema the child result must satisfy"
                    }
                },
                "required": ["spec", "brief"]
            }),
        ),
        tool(
            PLAN_ADD_TASK,
            "Add a task to the shared plan, optionally with dependency ids.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "depends_on": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["id"]
            }),
        ),
        tool(
            PLAN_READ,
            "Read the shared plan's current version and tasks.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            PLAN_CLAIM,
            "Claim a specific plan task; fails if its dependencies are unfinished.",
            json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string" },
                    "expected_version": { "type": "integer", "minimum": 0 }
                },
                "required": ["task", "expected_version"]
            }),
        ),
        tool(
            PLAN_CLAIM_FIRST_AVAILABLE,
            "Claim the first unclaimed, dependency-satisfied plan task.",
            json!({
                "type": "object",
                "properties": {
                    "expected_version": { "type": "integer", "minimum": 0 }
                },
                "required": ["expected_version"]
            }),
        ),
        tool(
            PLAN_UPDATE,
            "Update the status of a plan task you own.",
            json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string" },
                    "status": {
                        "type": "string",
                        "enum": ["todo", "in_progress", "completed", "blocked", "cancelled"]
                    },
                    "expected_version": { "type": "integer", "minimum": 0 }
                },
                "required": ["task", "status", "expected_version"]
            }),
        ),
        tool(
            BLACKBOARD_POST,
            "Post a message to the shared blackboard.",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "channel": { "type": "string" }
                },
                "required": ["text"]
            }),
        ),
        tool(
            BLACKBOARD_READ,
            "Read blackboard messages at or after an offset.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "integer", "minimum": 0 },
                    "channel": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 0 }
                }
            }),
        ),
        tool(
            SEND_MESSAGE,
            "Send a direct message to another agent's mailbox.",
            json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["to", "text"]
            }),
        ),
        tool(
            MAILBOX_READ,
            "Read your own mailbox inbox at or after a sequence number.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 0 }
                }
            }),
        ),
        tool(
            REPORT_ARTIFACT,
            "Record a produced artifact (patch / diff / test result / file).",
            json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["patch", "diff", "test_result", "file", "other"]
                    },
                    "summary": { "type": "string" },
                    "path": { "type": "string" },
                    "reference": { "type": "string" }
                },
                "required": ["summary"]
            }),
        ),
        tool(
            RUN_HOST_TOOL,
            "Invoke a host-registered tool by name under the run's guards.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "input": { "type": "object" }
                },
                "required": ["name"]
            }),
        ),
    ]
}

/// Packages the bridge [`Tool`] declarations into a [`ToolSetRef`] for injection.
///
/// A host uses the returned set as an external (or internal) agent's
/// `initial_tools`, then wires a [`CollabToolHandler`] to execute the inline
/// tools and intercepts [`SPAWN_AGENT`] via [`SpawnAgentRequest`].
#[must_use]
pub fn bridge_tool_set(id: ToolSetId) -> ToolSetRef {
    ToolSetRef::new(id, bridge_tool_declarations())
}

// ----- spawn_agent translation ---------------------------------------------

/// A parsed, structured `spawn_agent` request (design §3.4 / §6.3).
///
/// `spawn_agent` is not run as an inline tool: deriving a child deepens the scope
/// chain, which only a [`RequirementKind::NeedSubagent`] can express. This is the
/// structured intermediate a host translates a `spawn_agent` [`ToolCall`] into
/// with [`parse`](Self::parse), then converts to the requirement with
/// [`into_requirement_kind`](Self::into_requirement_kind).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnAgentRequest {
    spec: AgentSpecRef,
    brief: String,
    result_schema: Option<Value>,
}

impl SpawnAgentRequest {
    /// Creates a request from an already-resolved child spec and brief.
    #[must_use]
    pub fn new(spec: AgentSpecRef, brief: impl Into<String>, result_schema: Option<Value>) -> Self {
        Self {
            spec,
            brief: brief.into(),
            result_schema,
        }
    }

    /// Returns whether `name` is the `spawn_agent` tool name.
    #[must_use]
    pub fn matches(name: &str) -> bool {
        name == SPAWN_AGENT
    }

    /// Parses a `spawn_agent` [`ToolCall`] into a structured request.
    ///
    /// The `spec` argument is a child agent spec id (UUID); `brief` is the task
    /// brief; `result_schema` is an optional JSON schema object.
    ///
    /// # Errors
    ///
    /// Returns [`ToolAdapterError::WrongTool`] when the call is not
    /// `spawn_agent`, [`ToolAdapterError::MissingArgument`] /
    /// [`ToolAdapterError::InvalidArgument`] for malformed arguments, or
    /// [`ToolAdapterError::InvalidAgentId`] when `spec` is not a valid id.
    pub fn parse(call: &ToolCall) -> Result<Self, ToolAdapterError> {
        if !Self::matches(&call.name) {
            return Err(ToolAdapterError::WrongTool {
                expected: SPAWN_AGENT,
                actual: call.name.clone(),
            });
        }
        let spec_str = required_str(&call.input, "spec")?;
        let spec = AgentId::parse_str(&spec_str)
            .map(AgentSpecRef)
            .map_err(|_| ToolAdapterError::InvalidAgentId(spec_str))?;
        let brief = required_str(&call.input, "brief")?;
        let result_schema = match call.input.get("result_schema") {
            None | Some(Value::Null) => None,
            Some(value @ Value::Object(_)) => Some(value.clone()),
            Some(_) => {
                return Err(ToolAdapterError::InvalidArgument {
                    argument: "result_schema",
                    reason: "must be a JSON object".to_owned(),
                });
            }
        };
        Ok(Self::new(spec, brief, result_schema))
    }

    /// Returns the child agent spec reference.
    #[must_use]
    pub const fn spec(&self) -> &AgentSpecRef {
        &self.spec
    }

    /// Returns the task brief.
    #[must_use]
    pub fn brief(&self) -> &str {
        &self.brief
    }

    /// Returns the optional result schema.
    #[must_use]
    pub const fn result_schema(&self) -> Option<&Value> {
        self.result_schema.as_ref()
    }

    /// Converts this request into a [`RequirementKind::NeedSubagent`].
    ///
    /// The brief is presented to the child as an
    /// [`Interaction::question`] addressed to `step_id` (the step the
    /// `spawn_agent` call was issued on), reusing the standard subagent brief
    /// shape so the existing derivation path drives the child.
    #[must_use]
    pub fn into_requirement_kind(self, step_id: StepId) -> RequirementKind {
        RequirementKind::NeedSubagent {
            spec_ref: self.spec,
            brief: Interaction::question(step_id, self.brief),
            result_schema: self.result_schema,
        }
    }
}

// ----- artifact sink -------------------------------------------------------

/// A host sink that records artifacts a `report_artifact` call surfaces.
///
/// The full artifact content (diff, log, blob) is never carried inline — only the
/// redaction-safe [`ExternalArtifactRef`] (design §11). A host implements this to
/// route references into its own store / notification stream.
pub trait ArtifactSink: Send + Sync + std::fmt::Debug {
    /// Records one artifact reference.
    fn record(&self, artifact: ExternalArtifactRef);
}

/// An [`ArtifactSink`] that collects references in memory for later inspection.
#[derive(Debug, Default)]
pub struct RecordingArtifactSink {
    artifacts: Mutex<Vec<ExternalArtifactRef>>,
}

impl RecordingArtifactSink {
    /// Creates an empty recording sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of every recorded artifact, in report order.
    #[must_use]
    pub fn artifacts(&self) -> Vec<ExternalArtifactRef> {
        self.artifacts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

impl ArtifactSink for RecordingArtifactSink {
    fn record(&self, artifact: ExternalArtifactRef) {
        self.artifacts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(artifact);
    }
}

// ----- collaboration tool handler ------------------------------------------

/// A [`ToolHandler`] that bridges the inline collaboration tools to the shared
/// [`Plan`] / [`Blackboard`] / [`Mailbox`] primitives (design §3.4).
///
/// One handler is built per agent with that agent's `identity`; the identity is
/// the owner used for plan claims / updates, the sender used for blackboard
/// posts, and the sender used for mailbox sends — the model never supplies it.
/// `report_artifact` routes references to the configured [`ArtifactSink`];
/// `run_host_tool` forwards to an optional host [`ToolRegistry`]. `spawn_agent`
/// is *not* run here: it is a scope-deepening op a host translates to a
/// [`RequirementKind::NeedSubagent`] via [`SpawnAgentRequest`].
#[derive(Clone, Debug)]
pub struct CollabToolHandler {
    identity: String,
    plan: Arc<Plan>,
    blackboard: Arc<Blackboard>,
    mailbox: Arc<Mailbox>,
    artifacts: Arc<dyn ArtifactSink>,
    host_tools: Option<Arc<dyn ToolRegistry>>,
}

impl CollabToolHandler {
    /// Builds a handler for `identity` over the shared primitives.
    ///
    /// Artifacts default to a discarded [`RecordingArtifactSink`] and no host
    /// tools; use [`with_artifact_sink`](Self::with_artifact_sink) and
    /// [`with_host_tools`](Self::with_host_tools) to attach them.
    #[must_use]
    pub fn new(
        identity: impl Into<String>,
        plan: Arc<Plan>,
        blackboard: Arc<Blackboard>,
        mailbox: Arc<Mailbox>,
    ) -> Self {
        Self {
            identity: identity.into(),
            plan,
            blackboard,
            mailbox,
            artifacts: Arc::new(RecordingArtifactSink::new()),
            host_tools: None,
        }
    }

    /// Attaches an [`ArtifactSink`] for `report_artifact` to route references to.
    #[must_use]
    pub fn with_artifact_sink(mut self, sink: Arc<dyn ArtifactSink>) -> Self {
        self.artifacts = sink;
        self
    }

    /// Attaches a host [`ToolRegistry`] `run_host_tool` forwards calls to.
    #[must_use]
    pub fn with_host_tools(mut self, host_tools: Arc<dyn ToolRegistry>) -> Self {
        self.host_tools = Some(host_tools);
        self
    }

    /// Returns the injected agent identity used for owner / sender fields.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Dispatches one inline tool call to the shared primitives.
    ///
    /// Returns `Ok(ToolResponse)` (possibly a model-visible error response) for a
    /// known tool, or `Err(ToolRuntimeError)` for an unknown / mis-routed tool.
    async fn dispatch(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        let id = call.id.as_str();
        let input = &call.input;
        let response = match call.name.as_str() {
            PLAN_ADD_TASK => self.plan_add_task(id, input),
            PLAN_READ => self.plan_read(id),
            PLAN_CLAIM => self.plan_claim(id, input),
            PLAN_CLAIM_FIRST_AVAILABLE => self.plan_claim_first_available(id, input),
            PLAN_UPDATE => self.plan_update(id, input),
            BLACKBOARD_POST => self.blackboard_post(id, input),
            BLACKBOARD_READ => self.blackboard_read(id, input),
            SEND_MESSAGE => self.send_message(id, input),
            MAILBOX_READ => self.mailbox_read(id, input),
            REPORT_ARTIFACT => self.report_artifact(id, input),
            RUN_HOST_TOOL => return self.run_host_tool(call_id, id, input).await,
            SPAWN_AGENT => {
                return Err(ToolRuntimeError::ExecutionFailed {
                    tool_name: SPAWN_AGENT.to_owned(),
                    message: "spawn_agent is a subagent requirement, not an inline tool; \
                              translate it with SpawnAgentRequest"
                        .to_owned(),
                });
            }
            other => {
                return Err(ToolRuntimeError::UnknownTool {
                    name: other.to_owned(),
                });
            }
        };
        Ok(response)
    }

    /// Dispatches [`PLAN_ADD_TASK`].
    fn plan_add_task(&self, call_id: &str, input: &Value) -> ToolResponse {
        guarded(call_id, || {
            let task_id = str_arg(input, "id")?;
            let depends_on = opt_str_vec_arg(input, "depends_on")?;
            Ok(self
                .plan
                .add_task(task_id.clone(), depends_on)
                .map(|version| format!("task `{task_id}` added at v{version}"))
                .map_err(|error| error.to_string()))
        })
    }

    /// Dispatches [`PLAN_READ`].
    ///
    /// Each task entry is `id=status` plus `@owner` when claimed and
    /// ` deps:[..]` when it declares dependencies, so a reader can see who owns
    /// what and which tasks are still blocked without a second round trip.
    fn plan_read(&self, call_id: &str) -> ToolResponse {
        let snapshot = self.plan.snapshot();
        let tasks: Vec<String> = snapshot
            .task_order
            .iter()
            .map(|id| {
                let task = &snapshot.tasks[id];
                let mut entry = format!("{id}={}", task.status.label());
                if let Some(owner) = &task.owner {
                    entry.push_str(&format!("@{owner}"));
                }
                if !task.depends_on.is_empty() {
                    entry.push_str(&format!(" deps:[{}]", task.depends_on.join(",")));
                }
                entry
            })
            .collect();
        tool_ok(
            call_id,
            &format!("plan v{} [{}]", snapshot.version, tasks.join(", ")),
        )
    }

    /// Dispatches [`PLAN_CLAIM`], claiming as the injected identity.
    fn plan_claim(&self, call_id: &str, input: &Value) -> ToolResponse {
        guarded(call_id, || {
            let task = str_arg(input, "task")?;
            let expected_version = u64_arg(input, "expected_version")?;
            Ok(self
                .plan
                .claim(task.clone(), self.identity.clone(), expected_version)
                .map(|version| format!("`{task}` claimed by `{}` at v{version}", self.identity))
                .map_err(|error| error.to_string()))
        })
    }

    /// Dispatches [`PLAN_CLAIM_FIRST_AVAILABLE`], claiming as the injected identity.
    fn plan_claim_first_available(&self, call_id: &str, input: &Value) -> ToolResponse {
        guarded(call_id, || {
            let expected_version = u64_arg(input, "expected_version")?;
            Ok(self
                .plan
                .claim_first_available(self.identity.clone(), expected_version)
                .map(|(task, version)| {
                    format!("`{task}` claimed by `{}` at v{version}", self.identity)
                })
                .map_err(|error| error.to_string()))
        })
    }

    /// Dispatches [`PLAN_UPDATE`], updating as the injected identity.
    fn plan_update(&self, call_id: &str, input: &Value) -> ToolResponse {
        guarded(call_id, || {
            let task = str_arg(input, "task")?;
            let status = status_arg(input, "status")?;
            let expected_version = u64_arg(input, "expected_version")?;
            Ok(self
                .plan
                .update_status(
                    task.clone(),
                    self.identity.clone(),
                    status,
                    expected_version,
                )
                .map(|version| format!("`{task}` -> {} at v{version}", status.label()))
                .map_err(|error| error.to_string()))
        })
    }

    /// Dispatches [`BLACKBOARD_POST`], posting as the injected identity.
    fn blackboard_post(&self, call_id: &str, input: &Value) -> ToolResponse {
        guarded(call_id, || {
            let text = str_arg(input, "text")?;
            let channel = opt_str_arg(input, "channel")?
                .unwrap_or_else(|| super::blackboard::DEFAULT_CHANNEL.to_owned());
            let offset = self
                .blackboard
                .post(channel.clone(), self.identity.clone(), text);
            Ok(Ok(format!("posted to `{channel}` at offset {offset}")))
        })
    }

    /// Dispatches [`BLACKBOARD_READ`], returning the message bodies (not just
    /// a count) as a cursor-paginated, length-capped page.
    fn blackboard_read(&self, call_id: &str, input: &Value) -> ToolResponse {
        guarded(call_id, || {
            let from = opt_u64_arg(input, "from")?.unwrap_or(0);
            let channel = opt_str_arg(input, "channel")?
                .unwrap_or_else(|| super::blackboard::DEFAULT_CHANNEL.to_owned());
            let limit = read_limit(input)?;
            let messages = self.blackboard.read_from(&channel, from);
            let entries: Vec<(u64, &str, &str)> = messages
                .iter()
                .map(|message| {
                    (
                        message.offset,
                        message.sender.as_str(),
                        message.text.as_str(),
                    )
                })
                .collect();
            Ok(Ok(format_read_page(
                |shown| format!("read {shown} message(s) from `{channel}` offset {from}"),
                from,
                limit,
                &entries,
            )))
        })
    }

    /// Dispatches [`SEND_MESSAGE`], sending from the injected identity.
    fn send_message(&self, call_id: &str, input: &Value) -> ToolResponse {
        guarded(call_id, || {
            let to = str_arg(input, "to")?;
            let text = str_arg(input, "text")?;
            let seq = self.mailbox.send(self.identity.clone(), to.clone(), text);
            Ok(Ok(format!("message to `{to}` delivered as #{seq}")))
        })
    }

    /// Dispatches [`MAILBOX_READ`], reading the **injected identity's own**
    /// inbox as a cursor-paginated, length-capped page — the recipient is never
    /// a model-supplied argument, so an agent cannot read someone else's mail.
    fn mailbox_read(&self, call_id: &str, input: &Value) -> ToolResponse {
        guarded(call_id, || {
            let from = opt_u64_arg(input, "from")?.unwrap_or(0);
            let limit = read_limit(input)?;
            let messages = self.mailbox.read_from(&self.identity, from);
            let entries: Vec<(u64, &str, &str)> = messages
                .iter()
                .map(|message| (message.seq, message.from.as_str(), message.text.as_str()))
                .collect();
            let identity = &self.identity;
            Ok(Ok(format_read_page(
                |shown| format!("read {shown} message(s) from `{identity}`'s inbox at seq {from}"),
                from,
                limit,
                &entries,
            )))
        })
    }

    /// Dispatches [`REPORT_ARTIFACT`], recording a reference to the sink.
    fn report_artifact(&self, call_id: &str, input: &Value) -> ToolResponse {
        guarded(call_id, || {
            let summary = str_arg(input, "summary")?;
            let kind = opt_str_arg(input, "kind")?.map_or(ExternalArtifactKind::Other, |label| {
                artifact_kind_from_label(&label)
            });
            let path = opt_str_arg(input, "path")?;
            let reference = opt_str_arg(input, "reference")?;
            self.artifacts.record(ExternalArtifactRef {
                kind,
                summary: summary.clone(),
                path,
                reference,
            });
            Ok(Ok(format!("recorded {} artifact", kind_label(kind))))
        })
    }

    /// Dispatches [`RUN_HOST_TOOL`] by forwarding to the host [`ToolRegistry`].
    async fn run_host_tool(
        &self,
        call_id: ToolCallId,
        outer_call_id: &str,
        input: &Value,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        let Some(host_tools) = self.host_tools.as_ref() else {
            return Ok(tool_error(
                outer_call_id,
                "no host tools are registered for run_host_tool",
            ));
        };
        let name = match str_arg(input, "name") {
            Ok(name) => name,
            Err(message) => return Ok(tool_error(outer_call_id, &message)),
        };
        let inner_input = input.get("input").cloned().unwrap_or(Value::Null);
        let inner_call = ToolCall {
            id: outer_call_id.to_owned(),
            name,
            input: inner_input,
        };
        let mut response = host_tools.execute(call_id, inner_call).await?;
        // Re-pair the response with the outer provider call id so the model
        // matches it to the bridge tool call it issued.
        response.tool_call_id = outer_call_id.to_owned();
        Ok(response)
    }
}

#[async_trait]
impl ToolHandler for CollabToolHandler {
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        ctx: &RunContext,
    ) -> RequirementResult {
        // Host guard: a cancelled run refuses further tool work before any
        // primitive is touched (design §3.4 "不绕过 RunContext 护栏").
        if let Err(error) = ctx.check_cancelled() {
            return RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed {
                tool_name: call.name.clone(),
                message: error.to_string(),
            }));
        }
        RequirementResult::Tool(self.dispatch(call_id, call).await)
    }
}

// ----- argument parsing & responses ----------------------------------------

/// Classified failure from parsing or routing a bridge tool call.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ToolAdapterError {
    /// The call targeted a different tool than the adapter expected.
    #[error("expected tool `{expected}`, got `{actual}`")]
    WrongTool {
        /// Tool the adapter handles.
        expected: &'static str,
        /// Tool the call actually named.
        actual: String,
    },
    /// A required argument was missing.
    #[error("missing or non-string argument `{0}`")]
    MissingArgument(&'static str),
    /// An argument was present but malformed.
    #[error("invalid argument `{argument}`: {reason}")]
    InvalidArgument {
        /// Argument name.
        argument: &'static str,
        /// Why it was rejected.
        reason: String,
    },
    /// The `spec` argument was not a valid agent id.
    #[error("invalid agent id `{0}`")]
    InvalidAgentId(String),
}

impl From<ToolAdapterError> for String {
    fn from(error: ToolAdapterError) -> Self {
        error.to_string()
    }
}

/// Runs `body`, folding an argument error or a primitive error into a
/// model-visible [`ToolStatus::Error`] response and a success into a
/// [`ToolStatus::Ok`] response carrying the summary text.
fn guarded(
    call_id: &str,
    body: impl FnOnce() -> Result<Result<String, String>, String>,
) -> ToolResponse {
    match body() {
        Err(arg_error) => tool_error(call_id, &arg_error),
        Ok(Ok(summary)) => tool_ok(call_id, &summary),
        Ok(Err(primitive_error)) => tool_error(call_id, &primitive_error),
    }
}

/// Extracts the optional `limit` argument of a read tool, falling back to
/// [`DEFAULT_READ_LIMIT`].
fn read_limit(input: &Value) -> Result<usize, String> {
    Ok(
        opt_u64_arg(input, "limit")?.map_or(DEFAULT_READ_LIMIT, |n| {
            usize::try_from(n).unwrap_or(usize::MAX)
        }),
    )
}

/// Shared formatter for the `blackboard_read` / `mailbox_read` pages: the
/// `header` line (built from the number of shown messages), one
/// `#<cursor> <sender>: <body>` line per message with bodies truncated to
/// [`MAX_MESSAGE_BODY_CHARS`], and a resume hint naming the `from` cursor to
/// continue from when the page limit held back further messages.
///
/// `messages` is `(cursor, sender, body)` triples in read order, already
/// filtered to the requested `from` cursor by the primitive.
fn format_read_page(
    header: impl FnOnce(usize) -> String,
    from: u64,
    limit: usize,
    messages: &[(u64, &str, &str)],
) -> String {
    let shown = messages.len().min(limit);
    let page = &messages[..shown];
    let mut text = header(shown);
    if !page.is_empty() {
        text.push(':');
        for (cursor, sender, body) in page {
            text.push_str(&format!("\n#{cursor} {sender}: {}", truncate_body(body)));
        }
    }
    if messages.len() > shown {
        let next = page.last().map_or(from, |(cursor, ..)| cursor + 1);
        text.push_str(&format!(
            "\n… {} more; resume with from={next}",
            messages.len() - shown
        ));
    }
    text
}

/// Truncates a message body for tool output, keeping a read page bounded
/// regardless of how large a single posted message was.
fn truncate_body(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE_BODY_CHARS {
        text.to_owned()
    } else {
        let head: String = text.chars().take(MAX_MESSAGE_BODY_CHARS).collect();
        format!("{head}… [truncated]")
    }
}

/// Builds a successful text tool result.
fn tool_ok(call_id: &str, text: &str) -> ToolResponse {
    ToolResponse {
        tool_call_id: call_id.to_owned(),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
            extra: Map::new(),
        }],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

/// Builds a model-visible error tool result.
fn tool_error(call_id: &str, message: &str) -> ToolResponse {
    ToolResponse {
        tool_call_id: call_id.to_owned(),
        content: vec![ContentBlock::Text {
            text: message.to_owned(),
            extra: Map::new(),
        }],
        status: ToolStatus::Error,
        extra: Map::new(),
    }
}

/// Extracts a required string argument for [`SpawnAgentRequest::parse`],
/// returning a typed [`ToolAdapterError`].
fn required_str(input: &Value, key: &'static str) -> Result<String, ToolAdapterError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(ToolAdapterError::MissingArgument(key))
}

/// Extracts a required string argument.
fn str_arg(input: &Value, key: &'static str) -> Result<String, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ToolAdapterError::MissingArgument(key).to_string())
}

/// Extracts an optional string argument (absent / null -> `None`).
fn opt_str_arg(input: &Value, key: &'static str) -> Result<Option<String>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ToolAdapterError::InvalidArgument {
            argument: key,
            reason: "must be a string".to_owned(),
        }
        .to_string()),
    }
}

/// Extracts a required unsigned-integer argument.
fn u64_arg(input: &Value, key: &'static str) -> Result<u64, String> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolAdapterError::MissingArgument(key).to_string())
}

/// Extracts an optional unsigned-integer argument.
fn opt_u64_arg(input: &Value, key: &'static str) -> Result<Option<u64>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            ToolAdapterError::InvalidArgument {
                argument: key,
                reason: "must be a non-negative integer".to_owned(),
            }
            .to_string()
        }),
    }
}

/// Extracts an optional array-of-strings argument (absent / null -> empty).
fn opt_str_vec_arg(input: &Value, key: &'static str) -> Result<Vec<String>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    ToolAdapterError::InvalidArgument {
                        argument: key,
                        reason: "must be an array of strings".to_owned(),
                    }
                    .to_string()
                })
            })
            .collect(),
        Some(_) => Err(ToolAdapterError::InvalidArgument {
            argument: key,
            reason: "must be an array of strings".to_owned(),
        }
        .to_string()),
    }
}

/// Parses a `status` string argument into a typed [`TaskStatus`].
fn status_arg(input: &Value, key: &'static str) -> Result<TaskStatus, String> {
    let label = str_arg(input, key)?;
    TaskStatus::from_label(&label).ok_or_else(|| {
        ToolAdapterError::InvalidArgument {
            argument: key,
            reason: format!("unknown status `{label}`"),
        }
        .to_string()
    })
}

/// Maps an artifact `kind` label onto an [`ExternalArtifactKind`], defaulting to
/// [`Other`](ExternalArtifactKind::Other) for an unrecognized label.
fn artifact_kind_from_label(label: &str) -> ExternalArtifactKind {
    match label {
        "patch" => ExternalArtifactKind::Patch,
        "diff" => ExternalArtifactKind::Diff,
        "test_result" => ExternalArtifactKind::TestResult,
        "file" => ExternalArtifactKind::File,
        _ => ExternalArtifactKind::Other,
    }
}

/// Returns the lowercase label of an [`ExternalArtifactKind`] for tool output.
fn kind_label(kind: ExternalArtifactKind) -> &'static str {
    match kind {
        ExternalArtifactKind::Patch => "patch",
        ExternalArtifactKind::Diff => "diff",
        ExternalArtifactKind::TestResult => "test_result",
        ExternalArtifactKind::File => "file",
        ExternalArtifactKind::Other => "other",
    }
}
