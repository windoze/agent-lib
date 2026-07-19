//! Typed function tools for the Agent facade.
//!
//! This module lets a caller register a tool as an ordinary async Rust function
//! instead of hand-implementing [`crate::agent::ToolRegistry`]. A
//! `Tool::function` (or [`Tool::function_with_schema`]) captures three
//! responsibilities described in `docs/facade-api.md` §7:
//!
//! ```text
//! Args        -> JSON schema -> model tool declaration
//! JSON value  -> Args        (deserialize the model-supplied arguments)
//! Result<T>   -> provider-neutral tool result
//! ```
//!
//! The handler receives a [`ToolContext`] carrying run-scoped, read-only handles
//! (ids, worktree, cancellation, trace) and never a mutable Conversation
//! reference, so a tool cannot break Conversation invariants.
//!
//! # Schema derivation (`facade-schema`)
//!
//! Deriving the JSON input schema from `Args` requires
//! [`schemars`](https://docs.rs/schemars), which is **not** a core dependency.
//! Per `PLAN.md` R1 the derive path is therefore gated behind the off-by-default
//! `facade-schema` feature:
//!
//! - With `--features facade-schema`, `Tool::function` derives the schema from
//!   `Args: schemars::JsonSchema` (matching the `docs/facade-api.md` §7.1
//!   example).
//! - Without the feature, callers use the always-available
//!   [`Tool::function_with_schema`] and pass the JSON input schema explicitly.
//!
//! A default build links no `schemars`.

use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::agent::{
    AgentId, CancellationToken, RunId, ToolRegistry, ToolRuntimeError, TraceHandle, WorktreeRef,
};
use crate::conversation::ToolCallId;
use crate::facade::approval::Approval;
use crate::facade::error::FacadeError;
use crate::model::content::ContentBlock;
use crate::model::tool::{Tool as ToolDecl, ToolCall, ToolResponse, ToolStatus};

/// Run-scoped context handed to a typed tool handler.
///
/// Every field is a controlled, cloneable handle: it exposes run identity, the
/// isolated worktree, cooperative cancellation, and the trace sink, but never a
/// mutable reference that could violate Conversation invariants
/// (`docs/facade-api.md` §7.2). Writes to shared state (blackboard, artifacts,
/// mailbox) are added by later milestones through their own controlled handles.
#[derive(Clone, Debug)]
pub struct ToolContext {
    /// Identity of the run currently invoking the tool.
    pub run_id: RunId,
    /// Identity of the agent that owns this run.
    pub agent_id: AgentId,
    /// Framework identity of the specific tool call being executed.
    pub tool_call_id: ToolCallId,
    /// The isolated worktree the agent is running against.
    pub worktree: WorktreeRef,
    /// Cooperative cancellation token; tools should check it for long work.
    ///
    /// Checking is effectively required for anything that can block: a
    /// cancelled run pre-empts the drive's batch wait (M3-3) and **drops
    /// (detaches)** a still-blocked tool future after a bounded unwind grace,
    /// so a tool must not rely on running to completion — long-running tools
    /// should select on this token to abort their own work promptly.
    pub cancel: CancellationToken,
    /// Trace handle for recording tool-scoped diagnostic records.
    pub trace: TraceHandle,
}

/// A provider-neutral result produced by a typed tool handler.
///
/// This is the facade's explicit result type: a handler may return one directly
/// to control the [`ToolStatus`] and multimodal content, or return any
/// [`IntoToolResult`] value (a `String`, a `serde_json::Value`, or any
/// [`Serialize`] type) and let the facade normalize it.
///
/// It intentionally does **not** implement [`Serialize`]; that keeps the blanket
/// `impl<T: Serialize> IntoToolResult for T` coherent with the explicit
/// `impl IntoToolResult for ToolResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    content: Vec<ContentBlock>,
    status: ToolStatus,
    extra: Map<String, Value>,
}

impl ToolResult {
    /// Creates a successful text result.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text {
                text: text.into(),
                extra: Map::new(),
            }],
            status: ToolStatus::Ok,
            extra: Map::new(),
        }
    }

    /// Creates a successful result from explicit multimodal content blocks.
    #[must_use]
    pub fn blocks(content: Vec<ContentBlock>) -> Self {
        Self {
            content,
            status: ToolStatus::Ok,
            extra: Map::new(),
        }
    }

    /// Creates a model-visible error result (status [`ToolStatus::Error`]).
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text {
                text: message.into(),
                extra: Map::new(),
            }],
            status: ToolStatus::Error,
            extra: Map::new(),
        }
    }

    /// Overrides the outcome status of this result.
    #[must_use]
    pub fn with_status(mut self, status: ToolStatus) -> Self {
        self.status = status;
        self
    }

    /// Attaches unmodeled result metadata preserved across content conversion.
    #[must_use]
    pub fn with_extra(mut self, extra: Map<String, Value>) -> Self {
        self.extra = extra;
        self
    }

    /// Returns the multimodal content blocks.
    #[must_use]
    pub fn content(&self) -> &[ContentBlock] {
        &self.content
    }

    /// Returns the outcome status.
    #[must_use]
    pub const fn status(&self) -> ToolStatus {
        self.status
    }

    /// Returns the unmodeled result metadata.
    #[must_use]
    pub const fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }

    /// Converts this result into a complete [`ToolResponse`] for one call id.
    fn into_response(self, tool_call_id: String) -> ToolResponse {
        ToolResponse {
            tool_call_id,
            content: self.content,
            status: self.status,
            extra: self.extra,
        }
    }
}

/// Conversion from a typed handler return value into a [`ToolResult`].
///
/// Implemented for every [`Serialize`] type (via a blanket impl) and for the
/// explicit [`ToolResult`]. A `String` (or any value that serializes to a JSON
/// string) becomes a raw text result; any other value becomes a compact-JSON
/// text result.
pub trait IntoToolResult {
    /// Normalizes `self` into a [`ToolResult`].
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if the value cannot be serialized.
    fn into_tool_result(self) -> Result<ToolResult, serde_json::Error>;
}

impl<T: Serialize> IntoToolResult for T {
    fn into_tool_result(self) -> Result<ToolResult, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        let text = match value {
            Value::String(text) => text,
            other => serde_json::to_string(&other)?,
        };
        Ok(ToolResult::text(text))
    }
}

impl IntoToolResult for ToolResult {
    fn into_tool_result(self) -> Result<ToolResult, serde_json::Error> {
        Ok(self)
    }
}

/// Object-safe executor for one typed tool, erasing the handler's concrete
/// argument and return types.
#[async_trait]
trait ToolExecutorFn: Send + Sync {
    /// Executes the tool for one deserialized-from-JSON argument object.
    async fn call(&self, ctx: ToolContext, input: Value) -> Result<ToolResult, ToolCallError>;
}

/// Classified failure from invoking a typed tool.
///
/// Every variant maps to a [`ToolRuntimeError::ExecutionFailed`] so the Agent
/// loop's [`crate::agent::ToolFailurePolicy`] governs the outcome rather than the
/// tool silently pre-empting it.
#[derive(Debug)]
enum ToolCallError {
    /// The model-supplied arguments did not match the tool's `Args` type.
    InvalidArgs(String),
    /// The handler returned an error.
    Handler(String),
    /// The handler's return value could not be serialized into a result.
    Result(String),
}

impl ToolCallError {
    /// Renders a stable, model-visible diagnostic message.
    fn message(&self) -> String {
        match self {
            Self::InvalidArgs(detail) => format!("invalid arguments: {detail}"),
            Self::Handler(detail) => detail.clone(),
            Self::Result(detail) => format!("failed to serialize tool result: {detail}"),
        }
    }
}

/// Concrete executor wrapping a typed async handler `fn(ToolContext, Args)`.
struct FunctionTool<F, Args> {
    handler: F,
    _args: PhantomData<fn(Args)>,
}

#[async_trait]
impl<F, Fut, Args, Out, Err> ToolExecutorFn for FunctionTool<F, Args>
where
    F: Fn(ToolContext, Args) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, Err>> + Send + 'static,
    Args: DeserializeOwned + Send + 'static,
    Out: IntoToolResult + Send + 'static,
    Err: fmt::Display + Send + 'static,
{
    async fn call(&self, ctx: ToolContext, input: Value) -> Result<ToolResult, ToolCallError> {
        let args: Args =
            serde_json::from_value(input).map_err(|e| ToolCallError::InvalidArgs(e.to_string()))?;
        let output = (self.handler)(ctx, args)
            .await
            .map_err(|e| ToolCallError::Handler(e.to_string()))?;
        output
            .into_tool_result()
            .map_err(|e| ToolCallError::Result(e.to_string()))
    }
}

/// A tool registered with the Agent facade as a typed async function.
///
/// Construct one with `Tool::function` (requires the `facade-schema` feature)
/// or [`Tool::function_with_schema`]. The facade bridges a collection of these
/// into an internal [`crate::agent::ToolRegistry`] when an agent is built.
#[derive(Clone)]
pub struct Tool {
    name: String,
    description: String,
    input_schema: Value,
    executor: Arc<dyn ToolExecutorFn>,
    approval: Option<Approval>,
}

impl fmt::Debug for Tool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .field("has_approval_override", &self.approval.is_some())
            .finish_non_exhaustive()
    }
}

impl Tool {
    /// Registers a typed function tool with an explicit JSON input schema.
    ///
    /// This constructor is always available and does not require the
    /// `facade-schema` feature: the caller supplies `input_schema` (a JSON
    /// Schema object describing `Args`) directly. The `handler` receives the
    /// run-scoped [`ToolContext`] and the model-supplied arguments deserialized
    /// into `Args`, and returns any [`IntoToolResult`] value (or an error).
    ///
    /// ```
    /// use agent_lib::facade::tool::{Tool, ToolContext};
    /// use serde::Deserialize;
    /// use serde_json::json;
    ///
    /// #[derive(Deserialize)]
    /// struct WeatherArgs {
    ///     city: String,
    /// }
    ///
    /// let tool = Tool::function_with_schema(
    ///     "get_weather",
    ///     "Look up current weather for a city.",
    ///     json!({
    ///         "type": "object",
    ///         "properties": { "city": { "type": "string" } },
    ///         "required": ["city"]
    ///     }),
    ///     |_ctx: ToolContext, args: WeatherArgs| async move {
    ///         Ok::<_, std::convert::Infallible>(format!("{}: sunny, 26C", args.city))
    ///     },
    /// );
    /// assert_eq!(tool.name(), "get_weather");
    /// ```
    pub fn function_with_schema<F, Fut, Args, Out, Err>(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(ToolContext, Args) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Out, Err>> + Send + 'static,
        Args: DeserializeOwned + Send + 'static,
        Out: IntoToolResult + Send + 'static,
        Err: fmt::Display + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            executor: Arc::new(FunctionTool {
                handler,
                _args: PhantomData,
            }),
            approval: None,
        }
    }

    /// Registers a typed function tool, deriving the JSON input schema from
    /// `Args`.
    ///
    /// Requires the `facade-schema` feature, which pulls in
    /// [`schemars`](https://docs.rs/schemars); without it, use
    /// [`Tool::function_with_schema`] and pass the schema explicitly (see the
    /// module documentation and `PLAN.md` R1).
    ///
    /// ```
    /// # #[cfg(feature = "facade-schema")]
    /// # {
    /// use agent_lib::facade::tool::{Tool, ToolContext};
    /// use schemars::JsonSchema;
    /// use serde::Deserialize;
    ///
    /// #[derive(Deserialize, JsonSchema)]
    /// struct WeatherArgs {
    ///     city: String,
    /// }
    ///
    /// let tool = Tool::function(
    ///     "get_weather",
    ///     "Look up current weather for a city.",
    ///     |_ctx: ToolContext, args: WeatherArgs| async move {
    ///         Ok::<_, std::convert::Infallible>(format!("{}: sunny, 26C", args.city))
    ///     },
    /// );
    /// assert_eq!(tool.name(), "get_weather");
    /// # }
    /// ```
    #[cfg(feature = "facade-schema")]
    pub fn function<F, Fut, Args, Out, Err>(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: F,
    ) -> Self
    where
        F: Fn(ToolContext, Args) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Out, Err>> + Send + 'static,
        Args: DeserializeOwned + schemars::JsonSchema + Send + 'static,
        Out: IntoToolResult + Send + 'static,
        Err: fmt::Display + Send + 'static,
    {
        Self::function_with_schema(name, description, derive_schema::<Args>(), handler)
    }

    /// Returns the tool name advertised to the model.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the human-readable tool description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the JSON input schema advertised to the model.
    #[must_use]
    pub const fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    /// Returns the provider-neutral declaration sent to a model.
    #[must_use]
    pub fn declaration(&self) -> ToolDecl {
        ToolDecl {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }

    /// Overrides the approval treatment for this specific tool.
    ///
    /// A tool-level [`Approval`] takes precedence over the agent-level
    /// [`crate::facade::ApprovalPolicy`] (see `docs/facade-api.md` §9.1). For
    /// example, gate one dangerous tool behind an interactive prompt while the
    /// rest of the agent auto-allows:
    ///
    /// ```
    /// use agent_lib::facade::{Approval, tool::{Tool, ToolContext}};
    /// use serde_json::json;
    ///
    /// let tool = Tool::function_with_schema(
    ///     "shell",
    ///     "Run a shell command.",
    ///     json!({ "type": "object" }),
    ///     |_ctx: ToolContext, _args: serde_json::Value| async move {
    ///         Ok::<_, std::convert::Infallible>("ok")
    ///     },
    /// )
    /// .approval(Approval::auto_deny());
    /// assert!(tool.approval_override().is_some());
    /// ```
    #[must_use]
    pub fn approval(mut self, approval: Approval) -> Self {
        self.approval = Some(approval);
        self
    }

    /// Returns this tool's approval override, if one was set.
    ///
    /// The Agent facade merges each override into the agent-level approval
    /// policy at build time, where it wins over any agent-level entry for the
    /// same tool name.
    #[must_use]
    pub const fn approval_override(&self) -> Option<&Approval> {
        self.approval.as_ref()
    }
}

/// Derives a provider-friendly JSON input schema from `Args`.
///
/// The top-level `$schema` meta key emitted by `schemars` is removed because
/// providers expect a bare input-object schema.
#[cfg(feature = "facade-schema")]
fn derive_schema<Args: schemars::JsonSchema>() -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(Args))
        .unwrap_or_else(|_| serde_json::json!({ "type": "object" }));
    if let Value::Object(map) = &mut schema {
        map.remove("$schema");
    }
    schema
}

/// Adapter bridging a set of facade [`Tool`]s (plus the optional escape-hatch
/// registry and declarations) into one [`crate::agent::ToolRegistry`].
///
/// This is the facade's low-level assembly seam: the Agent builder constructs it
/// (see `docs/facade-api.md` §8.3) and drives it through the sans-io machine, but
/// it is exposed publicly so advanced callers composing the layers by hand can
/// reuse it. The typed tools own executable closures; the optional `custom`
/// registry and `extra` declarations are the §7.3 escape hatch. Name conflicts
/// across all three sources are rejected at construction. Each typed execution
/// builds a fresh [`ToolContext`] from the run-scoped handles supplied when the
/// registry was assembled.
pub struct FacadeToolRegistry {
    tools: Arc<[Tool]>,
    custom: Option<Arc<dyn ToolRegistry>>,
    extra: Arc<[ToolDecl]>,
    context: ToolContextParts,
}

/// The call-independent, run-scoped portion of a [`ToolContext`].
///
/// A [`FacadeToolRegistry`] holds one of these and stamps a per-call
/// [`ToolContext::tool_call_id`] onto it for each execution.
#[derive(Clone, Debug)]
pub struct ToolContextParts {
    /// Identity of the run the registry is bound to.
    pub run_id: RunId,
    /// Identity of the agent that owns the run.
    pub agent_id: AgentId,
    /// The isolated worktree the agent is running against.
    pub worktree: WorktreeRef,
    /// Cooperative cancellation token propagated to each tool.
    pub cancel: CancellationToken,
    /// Trace handle propagated to each tool.
    pub trace: TraceHandle,
}

impl ToolContextParts {
    /// Builds a full [`ToolContext`] for one specific tool call.
    fn context(&self, tool_call_id: ToolCallId) -> ToolContext {
        ToolContext {
            run_id: self.run_id,
            agent_id: self.agent_id,
            tool_call_id,
            worktree: self.worktree.clone(),
            cancel: self.cancel.clone(),
            trace: self.trace.clone(),
        }
    }
}

impl FacadeToolRegistry {
    /// Assembles a registry from typed tools plus the optional §7.3 escape
    /// hatch, validating that no tool name is declared more than once.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::DuplicateTool`] when the same name appears across
    /// the typed tools, the `extra` declarations, or the `custom` registry.
    pub fn new(
        tools: Vec<Tool>,
        custom: Option<Arc<dyn ToolRegistry>>,
        extra: Vec<ToolDecl>,
        context: ToolContextParts,
    ) -> Result<Self, FacadeError> {
        Self::from_shared(Arc::from(tools), custom, Arc::from(extra), context)
    }

    /// Assembles a registry from shared tool/declaration slices.
    pub(crate) fn from_shared(
        tools: Arc<[Tool]>,
        custom: Option<Arc<dyn ToolRegistry>>,
        extra: Arc<[ToolDecl]>,
        context: ToolContextParts,
    ) -> Result<Self, FacadeError> {
        ensure_unique_tool_names(&tools, &extra, custom.as_ref())?;

        Ok(Self {
            tools,
            custom,
            extra,
            context,
        })
    }
}

/// Validates that no tool name is declared more than once across the typed
/// tools, the escape-hatch declarations, and the custom registry.
///
/// Shared by [`FacadeToolRegistry::new`] and the Agent facade's build step so a
/// name conflict is reported once, up front, regardless of which assembly path
/// discovers it.
///
/// # Errors
///
/// Returns [`FacadeError::DuplicateTool`] on the first repeated name.
pub(crate) fn ensure_unique_tool_names(
    tools: &[Tool],
    extra: &[ToolDecl],
    custom: Option<&Arc<dyn ToolRegistry>>,
) -> Result<(), FacadeError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut check = |name: &str| -> Result<(), FacadeError> {
        if !seen.insert(name.to_owned()) {
            return Err(FacadeError::DuplicateTool {
                name: name.to_owned(),
            });
        }
        Ok(())
    };
    for tool in tools {
        check(tool.name())?;
    }
    for declaration in extra {
        check(&declaration.name)?;
    }
    if let Some(custom) = custom {
        for declaration in custom.declarations() {
            check(&declaration.name)?;
        }
    }
    Ok(())
}

/// Rejects duplicate names across a fully assembled tool-declaration list.
///
/// [`ensure_unique_tool_names`] checks the three base tool sources before the
/// delegation layer is added; this complements it by scanning the final
/// advertised declaration list (base tools plus synthesized delegation tools),
/// so a delegation tool that collides with a typed tool, an escape-hatch
/// declaration, or another delegate is rejected at build time with
/// [`FacadeError::DuplicateTool`] (`docs/facade-api.md` §10.1).
pub(crate) fn ensure_unique_declaration_names(
    declarations: &[ToolDecl],
) -> Result<(), FacadeError> {
    let mut seen = std::collections::BTreeSet::new();
    for declaration in declarations {
        if !seen.insert(declaration.name.as_str()) {
            return Err(FacadeError::DuplicateTool {
                name: declaration.name.clone(),
            });
        }
    }
    Ok(())
}

impl fmt::Debug for FacadeToolRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FacadeToolRegistry")
            .field(
                "tools",
                &self.tools.iter().map(Tool::name).collect::<Vec<_>>(),
            )
            .field("has_custom", &self.custom.is_some())
            .field(
                "extra",
                &self
                    .extra
                    .iter()
                    .map(|declaration| declaration.name.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolRegistry for FacadeToolRegistry {
    fn declarations(&self) -> Vec<ToolDecl> {
        let mut declarations: Vec<ToolDecl> = self.tools.iter().map(Tool::declaration).collect();
        declarations.extend(self.extra.iter().cloned());
        if let Some(custom) = &self.custom {
            declarations.extend(custom.declarations());
        }
        declarations
    }

    async fn execute(
        &self,
        call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        if let Some(tool) = self.tools.iter().find(|tool| tool.name == call.name) {
            let ctx = self.context.context(call_id);
            let provider_call_id = call.id.clone();
            return match tool.executor.call(ctx, call.input).await {
                Ok(result) => Ok(result.into_response(provider_call_id)),
                Err(error) => Err(ToolRuntimeError::ExecutionFailed {
                    tool_name: call.name,
                    message: error.message(),
                }),
            };
        }

        if let Some(custom) = &self.custom {
            return custom.execute(call_id, call).await;
        }

        Err(ToolRuntimeError::UnknownTool { name: call.name })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ToolRuntimeError, TraceNodeId};
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::convert::Infallible;
    use uuid::Uuid;

    #[derive(Deserialize)]
    #[cfg_attr(feature = "facade-schema", derive(schemars::JsonSchema))]
    struct EchoArgs {
        city: String,
    }

    #[derive(Serialize)]
    struct Report {
        city: String,
        temp: u32,
    }

    fn uuid(seed: u128) -> Uuid {
        Uuid::from_u128(seed)
    }

    fn parts() -> ToolContextParts {
        let run_id = RunId::new(uuid(1));
        ToolContextParts {
            run_id,
            agent_id: AgentId::new(uuid(2)),
            worktree: WorktreeRef::new("."),
            cancel: CancellationToken::new(),
            trace: TraceHandle::new_root(TraceNodeId::new("test-root"), run_id),
        }
    }

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        })
    }

    fn registry(tools: Vec<Tool>) -> FacadeToolRegistry {
        FacadeToolRegistry::new(tools, None, Vec::new(), parts()).expect("no name conflicts")
    }

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: "call-1".to_owned(),
            name: name.to_owned(),
            input,
            extra: Map::new(),
        }
    }

    fn text_of(response: &ToolResponse) -> String {
        response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    fn echo_tool() -> Tool {
        Tool::function_with_schema(
            "echo",
            "Echo the city back.",
            schema(),
            |_ctx: ToolContext, args: EchoArgs| async move {
                Ok::<_, Infallible>(format!("city: {}", args.city))
            },
        )
    }

    #[test]
    fn declaration_carries_name_description_and_schema() {
        let tool = echo_tool();
        assert_eq!(tool.name(), "echo");
        assert_eq!(tool.description(), "Echo the city back.");
        assert_eq!(tool.input_schema(), &schema());

        let declaration = tool.declaration();
        assert_eq!(declaration.name, "echo");
        assert_eq!(declaration.description, "Echo the city back.");
        assert_eq!(declaration.input_schema, schema());
    }

    #[tokio::test]
    async fn execute_with_valid_args_returns_ok_result() {
        let registry = registry(vec![echo_tool()]);
        let response = registry
            .execute(
                ToolCallId::new(uuid(10)),
                call("echo", json!({ "city": "Shanghai" })),
            )
            .await
            .expect("valid call succeeds");

        assert_eq!(response.tool_call_id, "call-1");
        assert_eq!(response.status, ToolStatus::Ok);
        assert_eq!(text_of(&response), "city: Shanghai");
    }

    #[tokio::test]
    async fn execute_with_invalid_args_is_structured_execution_error() {
        let registry = registry(vec![echo_tool()]);
        let error = registry
            .execute(
                ToolCallId::new(uuid(11)),
                call("echo", json!({ "town": "Shanghai" })),
            )
            .await
            .expect_err("missing required `city` fails");

        match error {
            ToolRuntimeError::ExecutionFailed { tool_name, message } => {
                assert_eq!(tool_name, "echo");
                assert!(
                    message.contains("invalid arguments"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_propagates_handler_error_under_policy() {
        let tool = Tool::function_with_schema(
            "boom",
            "Always fails.",
            schema(),
            |_ctx: ToolContext, _args: EchoArgs| async move {
                Err::<String, _>("weather service offline")
            },
        );
        let registry = registry(vec![tool]);
        let error = registry
            .execute(
                ToolCallId::new(uuid(12)),
                call("boom", json!({ "city": "X" })),
            )
            .await
            .expect_err("handler error surfaces");

        match error {
            ToolRuntimeError::ExecutionFailed { tool_name, message } => {
                assert_eq!(tool_name, "boom");
                assert_eq!(message, "weather service offline");
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_unknown_tool_reports_unknown() {
        let registry = registry(vec![echo_tool()]);
        let error = registry
            .execute(ToolCallId::new(uuid(13)), call("missing", json!({})))
            .await
            .expect_err("unknown tool fails");
        assert!(matches!(error, ToolRuntimeError::UnknownTool { .. }));
    }

    #[test]
    fn into_tool_result_normalizes_each_return_shape() {
        // String -> raw text.
        let from_string = "plain".to_owned().into_tool_result().unwrap();
        assert_eq!(from_string, ToolResult::text("plain"));

        // JSON string value -> raw text.
        let from_json_string = json!("hello").into_tool_result().unwrap();
        assert_eq!(from_json_string, ToolResult::text("hello"));

        // JSON object value -> compact JSON text.
        let from_json_object = json!({ "a": 1 }).into_tool_result().unwrap();
        assert_eq!(from_json_object, ToolResult::text(r#"{"a":1}"#));

        // impl Serialize struct -> compact JSON text.
        let from_struct = Report {
            city: "SH".to_owned(),
            temp: 26,
        }
        .into_tool_result()
        .unwrap();
        assert_eq!(from_struct, ToolResult::text(r#"{"city":"SH","temp":26}"#));

        // Explicit ToolResult -> preserved verbatim (including status).
        let explicit = ToolResult::error("denied").with_status(ToolStatus::Denied);
        assert_eq!(explicit.clone().into_tool_result().unwrap(), explicit);
    }

    #[tokio::test]
    async fn explicit_tool_result_status_flows_through_execute() {
        let tool = Tool::function_with_schema(
            "deny",
            "Returns an explicit denied result.",
            schema(),
            |_ctx: ToolContext, _args: EchoArgs| async move {
                Ok::<_, Infallible>(
                    ToolResult::error("not allowed").with_status(ToolStatus::Denied),
                )
            },
        );
        let registry = registry(vec![tool]);
        let response = registry
            .execute(
                ToolCallId::new(uuid(14)),
                call("deny", json!({ "city": "X" })),
            )
            .await
            .expect("explicit result returns Ok");
        assert_eq!(response.status, ToolStatus::Denied);
        assert_eq!(text_of(&response), "not allowed");
    }

    #[test]
    fn duplicate_typed_tool_names_are_rejected() {
        let error =
            FacadeToolRegistry::new(vec![echo_tool(), echo_tool()], None, Vec::new(), parts())
                .expect_err("duplicate typed names rejected");
        assert!(matches!(error, FacadeError::DuplicateTool { name } if name == "echo"));
    }

    #[test]
    fn typed_tool_conflicting_with_extra_declaration_is_rejected() {
        let extra = vec![ToolDecl {
            name: "echo".to_owned(),
            description: "clash".to_owned(),
            input_schema: schema(),
        }];
        let error = FacadeToolRegistry::new(vec![echo_tool()], None, extra, parts())
            .expect_err("typed/declaration clash rejected");
        assert!(matches!(error, FacadeError::DuplicateTool { name } if name == "echo"));
    }

    #[tokio::test]
    async fn custom_registry_declarations_merge_and_conflicts_are_rejected() {
        #[derive(Debug)]
        struct OneTool;

        #[async_trait]
        impl ToolRegistry for OneTool {
            fn declarations(&self) -> Vec<ToolDecl> {
                vec![ToolDecl {
                    name: "custom".to_owned(),
                    description: "a custom tool".to_owned(),
                    input_schema: schema(),
                }]
            }

            async fn execute(
                &self,
                _call_id: ToolCallId,
                call: ToolCall,
            ) -> Result<ToolResponse, ToolRuntimeError> {
                Ok(ToolResult::text(format!("custom saw {}", call.name)).into_response(call.id))
            }
        }

        // Non-conflicting: merged declarations expose both tools, and unknown
        // typed calls delegate to the custom registry.
        let registry = FacadeToolRegistry::new(
            vec![echo_tool()],
            Some(Arc::new(OneTool)),
            Vec::new(),
            parts(),
        )
        .expect("no conflict");
        let names: Vec<String> = registry
            .declarations()
            .into_iter()
            .map(|declaration| declaration.name)
            .collect();
        assert_eq!(names, vec!["echo".to_owned(), "custom".to_owned()]);

        let delegated = registry
            .execute(ToolCallId::new(uuid(15)), call("custom", json!({})))
            .await
            .expect("delegates to custom registry");
        assert_eq!(text_of(&delegated), "custom saw custom");

        // Conflicting: a typed tool named the same as a custom declaration.
        let conflict_tool = Tool::function_with_schema(
            "custom",
            "clash",
            schema(),
            |_ctx: ToolContext, _args: EchoArgs| async move { Ok::<_, Infallible>("x") },
        );
        let error = FacadeToolRegistry::new(
            vec![conflict_tool],
            Some(Arc::new(OneTool)),
            Vec::new(),
            parts(),
        )
        .expect_err("typed/custom clash rejected");
        assert!(matches!(error, FacadeError::DuplicateTool { name } if name == "custom"));
    }

    #[cfg(feature = "facade-schema")]
    #[test]
    fn function_derives_schema_from_args() {
        let tool = Tool::function(
            "echo",
            "Echo the city back.",
            |_ctx: ToolContext, args: EchoArgs| async move {
                Ok::<_, Infallible>(format!("city: {}", args.city))
            },
        );
        let derived = tool.input_schema();
        assert_eq!(derived.get("type"), Some(&json!("object")));
        assert_eq!(
            derived.pointer("/properties/city/type"),
            Some(&json!("string"))
        );
        assert_eq!(derived.get("required"), Some(&json!(["city"])));
        // The provider-facing schema drops the `$schema` meta key.
        assert!(derived.get("$schema").is_none());
    }
}
