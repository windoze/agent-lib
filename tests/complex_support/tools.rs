//! Complex-test tool adapter over the mock plan/blackboard store (milestone 1,
//! M1-2).
//!
//! The complex agent-effect scenarios drive the mock [`MockPlanBlackboardStore`]
//! *through the model*: the scripted LLM emits tool calls, the machine reifies
//! them as `NeedTool` requirements, and the [`ComplexToolHandler`] here fulfils
//! them at the effect boundary. This keeps every plan/blackboard mutation on the
//! same [`RequirementKind::NeedTool`](agent_lib::agent::RequirementKind) path a
//! real registry would use, without mocking any provider wire format or wiring a
//! live [`ToolRegistry`](agent_lib::agent::ToolRegistry) backend.
//!
//! The module provides three things the higher-level suites reuse:
//!
//! - stable tool-name [constants](self#constants) and [`tool_declarations`], the
//!   `Vec<Tool>` an [`AgentSpec`](agent_lib::agent::AgentSpec) advertises;
//! - the [`ComplexToolHandler`], which dispatches a [`ToolCall`] to the store,
//!   records a per-tool call log, returns model-visible tool errors for store or
//!   argument failures (never panicking), and reports an unknown tool as a
//!   [`ToolRuntimeError::UnknownTool`];
//! - the [`RequireDangerousWriteApprovalPolicy`], which forces
//!   [`DANGEROUS_WRITE`] through an approval interaction while auto-approving
//!   every other tool, plus the [`complex_agent_machine`] / [`complex_scope`]
//!   scenario setup helpers.

use std::sync::{Arc, Mutex};

use agent_lib::agent::{
    ApprovalRequirement, DefaultAgentMachine, InteractionHandler, LlmHandler, RequirementResult,
    RunContext, ToolApprovalPolicy, ToolHandler, ToolRuntimeError,
};
use agent_lib::conversation::ToolCallId;
use agent_lib::model::tool::{Tool, ToolCall, ToolResponse, ToolStatus};
use async_trait::async_trait;
use serde_json::{Value, json};

use agent_testkit::fixtures::{
    agent_spec_with_tools, agent_state, default_machine, tool_error_response, tool_ok,
};
use agent_testkit::ids::SeqIds;
use agent_testkit::scope::TestScope;

use super::plan_blackboard::{MockPlanBlackboardStore, StoreError, TaskStatus};

// ----- tool-name constants -------------------------------------------------

/// Creates the plan, resetting it to an empty version `0`.
pub const PLAN_CREATE: &str = "plan_create";
/// Adds a plan task with an optional `depends_on` edge set.
pub const PLAN_ADD_TASK: &str = "plan_add_task";
/// Claims a named plan task under an optimistic version check.
pub const PLAN_CLAIM: &str = "plan_claim";
/// Claims the first available plan task in stable order.
pub const PLAN_CLAIM_FIRST_AVAILABLE: &str = "plan_claim_first_available";
/// Updates the status of a plan task the caller owns.
pub const PLAN_UPDATE: &str = "plan_update";
/// Appends a message to the append-only blackboard.
pub const BLACKBOARD_POST: &str = "blackboard_post";
/// Reads blackboard messages from a cursor.
pub const BLACKBOARD_READ: &str = "blackboard_read";
/// A high-risk tool gated behind an approval interaction.
pub const DANGEROUS_WRITE: &str = "dangerous_write";
/// A benign, auto-approved read tool.
pub const SAFE_READ: &str = "safe_read";

// ----- tool declarations ---------------------------------------------------

/// Builds one [`Tool`] declaration from a name, description, and JSON schema.
fn tool(name: &str, description: &str, input_schema: Value) -> Tool {
    Tool {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
    }
}

/// Returns the full set of complex-test [`Tool`] declarations.
///
/// Pass the result to
/// [`agent_spec_with_tools`](agent_testkit::fixtures::agent_spec_with_tools) (or
/// straight to [`complex_agent_machine`]) so the agent advertises exactly the
/// tools the [`ComplexToolHandler`] can dispatch.
#[must_use]
pub fn tool_declarations() -> Vec<Tool> {
    vec![
        tool(
            PLAN_CREATE,
            "Create (or reset) the plan to an empty version 0.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            PLAN_ADD_TASK,
            "Add a plan task, optionally declaring prerequisite task ids.",
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
            PLAN_CLAIM,
            "Claim a named plan task under an optimistic version check.",
            json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string" },
                    "owner": { "type": "string" },
                    "expected_version": { "type": "integer", "minimum": 0 }
                },
                "required": ["task", "owner", "expected_version"]
            }),
        ),
        tool(
            PLAN_CLAIM_FIRST_AVAILABLE,
            "Claim the first available plan task in stable order.",
            json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "expected_version": { "type": "integer", "minimum": 0 }
                },
                "required": ["owner", "expected_version"]
            }),
        ),
        tool(
            PLAN_UPDATE,
            "Update the status of a plan task the caller owns.",
            json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string" },
                    "owner": { "type": "string" },
                    "status": {
                        "type": "string",
                        "enum": ["todo", "in_progress", "completed", "blocked", "cancelled"]
                    },
                    "expected_version": { "type": "integer", "minimum": 0 }
                },
                "required": ["task", "owner", "status", "expected_version"]
            }),
        ),
        tool(
            BLACKBOARD_POST,
            "Append a message to the append-only blackboard.",
            json!({
                "type": "object",
                "properties": {
                    "sender": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["sender", "text"]
            }),
        ),
        tool(
            BLACKBOARD_READ,
            "Read blackboard messages at and beyond a cursor offset.",
            json!({
                "type": "object",
                "properties": { "from": { "type": "integer", "minimum": 0 } }
            }),
        ),
        tool(
            DANGEROUS_WRITE,
            "Perform a high-risk write. Requires human approval.",
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        ),
        tool(
            SAFE_READ,
            "Perform a benign read. Auto-approved.",
            json!({
                "type": "object",
                "properties": { "from": { "type": "integer", "minimum": 0 } }
            }),
        ),
    ]
}

// ----- call log ------------------------------------------------------------

/// One recorded invocation of the [`ComplexToolHandler`].
///
/// Because the handler only runs *after* any approval interaction resolves in
/// its favour, one recorded invocation per tool means one real execution — so a
/// denied or cancelled tool call leaves no entry. The `outcome` carries the
/// resulting [`ToolStatus`] (`Ok`/`Error`) or the [`ToolRuntimeError`] for an
/// unknown tool.
#[derive(Clone, Debug)]
pub struct ToolInvocation {
    /// Tool name the model selected.
    pub name: String,
    /// Raw JSON arguments the model supplied.
    pub input: Value,
    /// Result family/status of the dispatch.
    pub outcome: Result<ToolStatus, ToolRuntimeError>,
}

// ----- tool handler --------------------------------------------------------

/// A [`ToolHandler`] that dispatches complex-test tool calls to a shared
/// [`MockPlanBlackboardStore`].
///
/// Every fulfilled call is appended to an observable log (see
/// [`calls`](Self::calls) / [`execution_count`](Self::execution_count)). Store
/// and argument failures fold into a model-visible
/// [`ToolStatus::Error`](agent_lib::model::tool::ToolStatus::Error) tool result
/// rather than a panic or a runtime error, mirroring how a real registry surfaces
/// a recoverable tool failure back to the model. An unrecognized tool name is the
/// one hard failure: it returns a
/// [`ToolRuntimeError::UnknownTool`], fixing the testkit's chosen style for that
/// case.
#[derive(Debug)]
pub struct ComplexToolHandler {
    /// Shared plan/blackboard store the tools mutate.
    store: Arc<MockPlanBlackboardStore>,
    /// Ordered log of every fulfilled (executed) tool call.
    calls: Mutex<Vec<ToolInvocation>>,
}

impl ComplexToolHandler {
    /// Builds a handler over the shared `store`.
    #[must_use]
    pub fn new(store: Arc<MockPlanBlackboardStore>) -> Self {
        Self {
            store,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Returns the shared store this handler mutates.
    #[must_use]
    pub fn store(&self) -> &Arc<MockPlanBlackboardStore> {
        &self.store
    }

    /// Locks the call log, panicking on poison.
    fn calls_guard(&self) -> std::sync::MutexGuard<'_, Vec<ToolInvocation>> {
        self.calls.lock().expect("tool call log mutex poisoned")
    }

    /// Returns a snapshot of every recorded (executed) invocation, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<ToolInvocation> {
        self.calls_guard().clone()
    }

    /// Returns, in order, the invocations recorded for tool `name`.
    #[must_use]
    pub fn calls_named(&self, name: &str) -> Vec<ToolInvocation> {
        self.calls_guard()
            .iter()
            .filter(|call| call.name == name)
            .cloned()
            .collect()
    }

    /// Returns how many times tool `name` actually executed.
    #[must_use]
    pub fn execution_count(&self, name: &str) -> usize {
        self.calls_guard()
            .iter()
            .filter(|call| call.name == name)
            .count()
    }

    /// Dispatches one tool call to the store, folding failures per the type docs.
    fn dispatch(&self, call: &ToolCall) -> Result<ToolResponse, ToolRuntimeError> {
        let id = call.id.as_str();
        let input = &call.input;
        let response = match call.name.as_str() {
            PLAN_CREATE => {
                let (plan_id, version) = self.store.create_plan();
                tool_ok(id, &format!("plan {plan_id} created at v{version}"))
            }
            PLAN_ADD_TASK => self.plan_add_task(id, input),
            PLAN_CLAIM => self.plan_claim(id, input),
            PLAN_CLAIM_FIRST_AVAILABLE => self.plan_claim_first_available(id, input),
            PLAN_UPDATE => self.plan_update(id, input),
            BLACKBOARD_POST => self.blackboard_post(id, input),
            BLACKBOARD_READ => self.blackboard_read(id, input),
            DANGEROUS_WRITE => self.dangerous_write(id, input),
            SAFE_READ => self.safe_read(id, input),
            other => {
                return Err(ToolRuntimeError::UnknownTool {
                    name: other.to_owned(),
                });
            }
        };
        Ok(response)
    }

    /// Runs `body`, converting an argument-parse error or a store error into a
    /// model-visible [`ToolStatus::Error`] response and a store success into a
    /// [`ToolStatus::Ok`] response carrying `body`'s summary text.
    fn guarded(
        &self,
        call_id: &str,
        body: impl FnOnce() -> Result<Result<String, StoreError>, String>,
    ) -> ToolResponse {
        match body() {
            Err(arg_error) => tool_error_response(call_id, &arg_error),
            Ok(Ok(summary)) => tool_ok(call_id, &summary),
            Ok(Err(store_error)) => tool_error_response(call_id, &store_error.to_string()),
        }
    }

    /// Dispatches [`PLAN_ADD_TASK`].
    fn plan_add_task(&self, call_id: &str, input: &Value) -> ToolResponse {
        self.guarded(call_id, || {
            let task_id = str_field(input, "id")?;
            let depends_on = str_vec_field(input, "depends_on")?;
            Ok(self
                .store
                .add_task(task_id.clone(), depends_on)
                .map(|version| format!("task `{task_id}` added at v{version}")))
        })
    }

    /// Dispatches [`PLAN_CLAIM`].
    fn plan_claim(&self, call_id: &str, input: &Value) -> ToolResponse {
        self.guarded(call_id, || {
            let task = str_field(input, "task")?;
            let owner = str_field(input, "owner")?;
            let expected_version = u64_field(input, "expected_version")?;
            Ok(self
                .store
                .claim(task.clone(), owner.clone(), expected_version)
                .map(|version| format!("`{task}` claimed by `{owner}` at v{version}")))
        })
    }

    /// Dispatches [`PLAN_CLAIM_FIRST_AVAILABLE`].
    fn plan_claim_first_available(&self, call_id: &str, input: &Value) -> ToolResponse {
        self.guarded(call_id, || {
            let owner = str_field(input, "owner")?;
            let expected_version = u64_field(input, "expected_version")?;
            Ok(self
                .store
                .claim_first_available(owner.clone(), expected_version)
                .map(|(task, version)| format!("`{task}` claimed by `{owner}` at v{version}")))
        })
    }

    /// Dispatches [`PLAN_UPDATE`].
    fn plan_update(&self, call_id: &str, input: &Value) -> ToolResponse {
        self.guarded(call_id, || {
            let task = str_field(input, "task")?;
            let owner = str_field(input, "owner")?;
            let status = status_field(input, "status")?;
            let expected_version = u64_field(input, "expected_version")?;
            Ok(self
                .store
                .update_status(task.clone(), owner.clone(), status, expected_version)
                .map(|version| format!("`{task}` -> {} at v{version}", status.label())))
        })
    }

    /// Dispatches [`BLACKBOARD_POST`].
    fn blackboard_post(&self, call_id: &str, input: &Value) -> ToolResponse {
        self.guarded(call_id, || {
            let sender = str_field(input, "sender")?;
            let text = str_field(input, "text")?;
            let offset = self.store.post(sender, text);
            Ok(Ok(format!("posted at offset {offset}")))
        })
    }

    /// Dispatches [`BLACKBOARD_READ`].
    fn blackboard_read(&self, call_id: &str, input: &Value) -> ToolResponse {
        self.guarded(call_id, || {
            let from = opt_u64_field(input, "from")?.unwrap_or(0);
            let messages = self.store.read_from(from);
            Ok(Ok(format!(
                "read {} message(s) from offset {from}",
                messages.len()
            )))
        })
    }

    /// Dispatches [`DANGEROUS_WRITE`], recording a visible blackboard side effect
    /// so that "the approved write actually ran" is observable in the store.
    fn dangerous_write(&self, call_id: &str, input: &Value) -> ToolResponse {
        self.guarded(call_id, || {
            let text = str_field(input, "text")?;
            let offset = self.store.post(DANGEROUS_WRITE, text);
            Ok(Ok(format!("dangerous write committed at offset {offset}")))
        })
    }

    /// Dispatches [`SAFE_READ`].
    fn safe_read(&self, call_id: &str, input: &Value) -> ToolResponse {
        self.guarded(call_id, || {
            let from = opt_u64_field(input, "from")?.unwrap_or(0);
            let messages = self.store.read_from(from);
            Ok(Ok(format!(
                "safe read {} message(s) from offset {from}",
                messages.len()
            )))
        })
    }
}

#[async_trait]
impl ToolHandler for ComplexToolHandler {
    async fn fulfill(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let result = self.dispatch(call);
        let outcome = match &result {
            Ok(response) => Ok(response.status),
            Err(error) => Err(error.clone()),
        };
        self.calls_guard().push(ToolInvocation {
            name: call.name.clone(),
            input: call.input.clone(),
            outcome,
        });
        RequirementResult::Tool(result)
    }
}

// ----- argument parsing ----------------------------------------------------

/// Extracts a required string field, returning a model-visible error string.
fn str_field(input: &Value, key: &str) -> Result<String, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing or non-string argument `{key}`"))
}

/// Extracts a required unsigned-integer field.
fn u64_field(input: &Value, key: &str) -> Result<u64, String> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing or non-integer argument `{key}`"))
}

/// Extracts an optional unsigned-integer field.
fn opt_u64_field(input: &Value, key: &str) -> Result<Option<u64>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("argument `{key}` must be an unsigned integer")),
    }
}

/// Extracts an optional array-of-strings field, defaulting to an empty vector.
fn str_vec_field(input: &Value, key: &str) -> Result<Vec<String>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("argument `{key}` must be an array of strings"))
            })
            .collect(),
        Some(_) => Err(format!("argument `{key}` must be an array of strings")),
    }
}

/// Extracts a required task-status field and parses its wire label.
fn status_field(input: &Value, key: &str) -> Result<TaskStatus, String> {
    let label = str_field(input, key)?;
    TaskStatus::from_label(&label).ok_or_else(|| format!("unknown task status `{label}`"))
}

// ----- approval policy -----------------------------------------------------

/// A [`ToolApprovalPolicy`] that gates only [`DANGEROUS_WRITE`] behind approval.
///
/// [`DANGEROUS_WRITE`] returns
/// [`ApprovalRequirement::RequireApproval`](agent_lib::agent::ApprovalRequirement::RequireApproval)
/// so the machine emits a `NeedInteraction` before executing it; every other
/// tool auto-approves and runs straight through.
#[derive(Clone, Copy, Debug, Default)]
pub struct RequireDangerousWriteApprovalPolicy;

impl ToolApprovalPolicy for RequireDangerousWriteApprovalPolicy {
    fn approval_requirement(&self, _call_id: ToolCallId, call: &ToolCall) -> ApprovalRequirement {
        if call.name == DANGEROUS_WRITE {
            ApprovalRequirement::required(Some(format!(
                "`{DANGEROUS_WRITE}` requires human approval"
            )))
        } else {
            ApprovalRequirement::AutoApprove
        }
    }
}

// ----- scenario setup helpers ----------------------------------------------

/// Builds a [`DefaultAgentMachine`] advertising every complex tool and gating
/// [`DANGEROUS_WRITE`] with [`RequireDangerousWriteApprovalPolicy`].
///
/// The store does not belong to the machine: it lives behind the
/// [`ComplexToolHandler`] wired into the scope (see [`complex_tool_handler`] /
/// [`complex_scope`]), because the machine only carries the tool *declarations*
/// and the approval policy at the effect boundary. Pair this machine with a
/// scope built from the same store.
#[must_use]
pub fn complex_agent_machine(ids: &SeqIds) -> DefaultAgentMachine {
    let spec = agent_spec_with_tools(ids, tool_declarations());
    let state = agent_state(ids, spec);
    default_machine(ids, state).with_approval_policy(Arc::new(RequireDangerousWriteApprovalPolicy))
}

/// Builds a [`ComplexToolHandler`] over the shared `store`, ready to wire into a
/// scope with [`complex_scope`].
#[must_use]
pub fn complex_tool_handler(store: Arc<MockPlanBlackboardStore>) -> Arc<ComplexToolHandler> {
    Arc::new(ComplexToolHandler::new(store))
}

/// Assembles a [`TestScope`] from an LLM handler, a tool handler, and an optional
/// interaction backend.
///
/// Passing `interaction` makes the layer attended (approvals resolve here);
/// leaving it `None` keeps the layer headless, so an approval it is the top layer
/// for surfaces as an unhandled requirement rather than being auto-granted.
#[must_use]
pub fn complex_scope(
    llm: Arc<dyn LlmHandler>,
    tool: Arc<dyn ToolHandler>,
    interaction: Option<Arc<dyn InteractionHandler>>,
) -> TestScope {
    let mut builder = TestScope::builder().llm(llm).tool(tool);
    if let Some(interaction) = interaction {
        builder = builder.interaction(interaction);
    }
    builder.build()
}
