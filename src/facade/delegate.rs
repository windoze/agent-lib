//! Local subagent delegation surface for the [`Agent`](crate::facade::Agent)
//! facade (`docs/facade-api.md` §10).
//!
//! A subagent is a same-library child [`AgentMachine`](crate::agent) exposed as a
//! *local delegate*. This module lands the first slice of that surface
//! (milestone M3-1): the [`AgentWorkerBuilder`] reached through
//! [`Agent::worker`](crate::facade::Agent::worker), and the data-first
//! [`LocalSubagent`] it produces.
//!
//! # Data-first worker spec (§10.3)
//!
//! [`Agent::worker`](crate::facade::Agent::worker) does **not** return a live,
//! client-bound session. It returns a [`LocalSubagent`]: a serializable-shaped
//! recipe carrying the child [`AgentSpec`], its advertised tool declarations, and
//! its [`ApprovalPolicy`]. The child [`AgentState`],
//! machine, and [`RunContext`] are built only when a
//! [`NeedSubagent`](crate::agent) delegation is fulfilled (milestone M3-2), so a
//! [`LocalSubagent`] never holds an LLM client, tool closures, or the approval
//! handler. That keeps snapshot and restore simple.
//!
//! # Model inheritance (R4)
//!
//! Per `PLAN.md` R4 a worker **inherits** the supervisor's provider/model by
//! default, which keeps the common case terse:
//!
//! ```
//! # fn demo() -> Result<(), agent_lib::facade::FacadeError> {
//! use agent_lib::facade::Agent;
//!
//! // Inherit the supervisor model (default).
//! let reviewer = Agent::worker()
//!     .system("You are a strict code reviewer; report only issues and evidence.")
//!     .build()?;
//! assert!(reviewer.inherits_model());
//!
//! // Or pin an explicit (usually cheaper) model.
//! let researcher = Agent::worker()
//!     .model("gpt-5.5")
//!     .system("You are a focused researcher.")
//!     .build()?;
//! assert!(!researcher.inherits_model());
//! # Ok(())
//! # }
//! ```
//!
//! When a worker inherits, its [`AgentSpec`] carries a placeholder model
//! ([`inherits_model`](LocalSubagent::inherits_model) reports `true`); the real
//! supervisor model is substituted when the delegation is fulfilled.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::agent::external::ExternalSessionRef;
use crate::agent::{
    AgentError, AgentInput, AgentMachine, AgentSpec, AgentSpecRef, AgentState, CancellationToken,
    DefaultAgentMachine, DrivingSubagentHandler, HandlerScope, Interaction, InteractionHandler,
    LlmClientHandler, LlmHandler, LoopCursor, LoopPolicy, ModelRef, RequirementResult, RunContext,
    RunId, ScopePop, SpawnedChild, StepInput, StepOutcome, SubagentHandler, SubagentOutput,
    SubagentSpawner, ToolFailurePolicy, ToolHandler, ToolRegistry, ToolRegistryHandler,
    ToolRuntimeError, ToolSetRef, TraceHandle, TraceNodeId, TurnDone, WorktreeRef,
};
use crate::client::LlmClient;
use crate::conversation::{Conversation, ConversationConfig, ToolCallId};
use crate::facade::agent::{
    DEFAULT_MAX_STEPS, DEFAULT_MAX_TOOL_ROUNDS, assemble_machine, build_loop_policy,
    final_turn_summary,
};
use crate::facade::approval::{ApprovalPolicy, FacadeApproval};
use crate::facade::config::ModelConfig;
use crate::facade::error::FacadeError;
use crate::facade::external::{ManagedExternalDelegate, drive_external};
use crate::facade::ids::FacadeIds;
use crate::facade::run::{ArtifactRef, DelegationStatus, DelegationTrace};
use crate::facade::tool::{FacadeToolRegistry, ToolContextParts};
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::tool::Tool as ToolDecl;
use crate::model::tool::{ToolCall, ToolResponse, ToolStatus};
use crate::model::usage::Usage;

/// The placeholder model recorded on a worker that inherits its supervisor's
/// model (R4).
///
/// A [`LocalSubagent`] is built before the supervisor exists, so an inheriting
/// worker cannot know the concrete model yet. Its [`AgentSpec`] carries this
/// sentinel and [`LocalSubagent::inherits_model`] returns `true`; the real model
/// is substituted when the delegation is fulfilled (milestone M3-2).
pub(crate) const INHERITED_MODEL_PLACEHOLDER: &str = "<inherited>";

/// A data-first recipe for one local subagent delegate (`docs/facade-api.md`
/// §10.3).
///
/// A `LocalSubagent` is produced by [`Agent::worker`](crate::facade::Agent::worker)
/// and registered with a supervisor through
/// [`AgentBuilder::subagent`](crate::facade::AgentBuilder::subagent). It is
/// deliberately *data only*: it holds the child [`AgentSpec`], the child's
/// advertised tool declarations ([`ToolSetRef`]), and its [`ApprovalPolicy`], but
/// never an LLM client, tool closures, or the approval handler. The live child
/// runtime is assembled only when a delegation is fulfilled (milestone M3-2).
///
/// The type is `Clone`/`Debug` but intentionally not `PartialEq`/`Serialize`
/// (the [`ApprovalPolicy`] carries an optional handler and is neither); the
/// serializable [`spec`](LocalSubagent::spec) is the data authority a snapshot
/// persists.
#[derive(Clone, Debug)]
pub struct LocalSubagent {
    name: String,
    description: String,
    spec: AgentSpec,
    tools: ToolSetRef,
    approval: ApprovalPolicy,
    inherit_model: bool,
}

impl LocalSubagent {
    /// Returns the delegate name.
    ///
    /// A freshly [`build`](AgentWorkerBuilder::build)-ed worker has an empty
    /// name; the supervisor sets it at registration through
    /// [`AgentBuilder::subagent`](crate::facade::AgentBuilder::subagent).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the human-readable delegate description advertised to the model.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the data-only child [`AgentSpec`].
    #[must_use]
    pub const fn spec(&self) -> &AgentSpec {
        &self.spec
    }

    /// Returns the child's advertised tool declarations.
    #[must_use]
    pub const fn tools(&self) -> &ToolSetRef {
        &self.tools
    }

    /// Returns the child's [`ApprovalPolicy`].
    #[must_use]
    pub const fn approval(&self) -> &ApprovalPolicy {
        &self.approval
    }

    /// Reports whether this worker inherits the supervisor's model (R4).
    ///
    /// When `true`, the [`spec`](Self::spec) carries a placeholder model that is
    /// substituted with the supervisor's model when the delegation is fulfilled.
    #[must_use]
    pub const fn inherits_model(&self) -> bool {
        self.inherit_model
    }

    /// Returns a copy of this delegate with `name` applied.
    ///
    /// Used by [`AgentBuilder::subagent`](crate::facade::AgentBuilder::subagent)
    /// to stamp the registration name onto a worker built without one.
    #[must_use]
    pub(crate) fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Rebuilds a data-first delegate from its persisted parts.
    ///
    /// A snapshot ([`DelegateSnapshot`](crate::facade::DelegateSnapshot))
    /// persists a delegate's data — its `name`, `description`, child
    /// [`AgentSpec`], advertised [`ToolSetRef`], and inheritance flag — but never
    /// its [`ApprovalPolicy`] (a possibly closure-bearing runtime handle, §15.2).
    /// Restore therefore re-supplies the policy, defaulting to
    /// [`ApprovalPolicy::default`] unless the caller re-registers the delegate.
    #[must_use]
    pub(crate) fn from_parts(
        name: String,
        description: String,
        spec: AgentSpec,
        tools: ToolSetRef,
        approval: ApprovalPolicy,
        inherit_model: bool,
    ) -> Self {
        Self {
            name,
            description,
            spec,
            tools,
            approval,
            inherit_model,
        }
    }
}

/// A fluent builder for a data-first [`LocalSubagent`], reached through
/// [`Agent::worker`](crate::facade::Agent::worker).
///
/// A worker requires far less configuration than a full
/// [`Agent`](crate::facade::Agent): it needs no client or provider, since the
/// live child is assembled later and, by default, inherits the supervisor's
/// model (R4). Set a [`system`](Self::system) prompt, optionally pin an explicit
/// [`model`](Self::model), attach an [`approval`](Self::approval) policy, and
/// [`build`](Self::build).
#[derive(Debug, Default)]
pub struct AgentWorkerBuilder {
    description: Option<String>,
    model: Option<String>,
    inherit_model: bool,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    system: Option<String>,
    approval: Option<ApprovalPolicy>,
    extra_declarations: Vec<ToolDecl>,
    max_steps: Option<u32>,
    max_tool_rounds: Option<u32>,
    tool_failure_policy: Option<ToolFailurePolicy>,
    worktree: Option<WorktreeRef>,
    ids: Option<FacadeIds>,
}

impl AgentWorkerBuilder {
    /// Sets the human-readable description advertised to the supervising model.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Pins an explicit model for the worker, opting out of inheritance (R4).
    ///
    /// Setting a model clears any prior [`inherit_model`](Self::inherit_model)
    /// request; the last of the two wins.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self.inherit_model = false;
        self
    }

    /// Requests that the worker inherit the supervisor's provider/model (R4).
    ///
    /// This is already the default; call it to be explicit or to override a
    /// prior [`model`](Self::model) call. It clears any pinned model.
    #[must_use]
    pub fn inherit_model(mut self) -> Self {
        self.inherit_model = true;
        self.model = None;
        self
    }

    /// Sets the maximum number of output tokens per LLM step.
    ///
    /// Ignored when the worker inherits its model, since the inherited model
    /// carries the supervisor's request settings. A value of `0` is treated as
    /// "leave at the default" (see [`ModelConfig::max_tokens`]).
    #[must_use]
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Sets the sampling temperature.
    ///
    /// Ignored when the worker inherits its model.
    #[must_use]
    pub fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets the child system prompt applied to every delegated turn.
    #[must_use]
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Sets the child's approval policy.
    ///
    /// Accepts either a whole-agent [`Approval`](crate::facade::Approval) tier or
    /// a fully built [`ApprovalPolicy`]. A subagent's tools stay inside the same
    /// effect model, so tools requiring approval still trigger it (§9.2).
    #[must_use]
    pub fn approval(mut self, approval: impl Into<ApprovalPolicy>) -> Self {
        self.approval = Some(approval.into());
        self
    }

    /// Advertises the child's tool declarations (data-only escape hatch, §7.3).
    ///
    /// A [`LocalSubagent`] stays data-first, so a worker carries only tool
    /// *declarations*, never executable closures; the executable side is
    /// re-supplied when the delegation is fulfilled (milestone M3-2).
    #[must_use]
    pub fn tool_declarations(mut self, declarations: Vec<ToolDecl>) -> Self {
        self.extra_declarations = declarations;
        self
    }

    /// Overrides the child's per-turn LLM-step budget (default `8`).
    #[must_use]
    pub fn max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = Some(max_steps);
        self
    }

    /// Overrides the child's maximum tool-call rounds per turn (default `4`).
    #[must_use]
    pub fn max_tool_rounds(mut self, max_tool_rounds: u32) -> Self {
        self.max_tool_rounds = Some(max_tool_rounds);
        self
    }

    /// Overrides how a failed child tool call is handled (default
    /// [`ToolFailurePolicy::ReturnErrorToModel`]).
    #[must_use]
    pub fn tool_failure_policy(mut self, policy: ToolFailurePolicy) -> Self {
        self.tool_failure_policy = Some(policy);
        self
    }

    /// Sets the isolated worktree the child runs against (default `"."`).
    #[must_use]
    pub fn worktree(mut self, worktree: WorktreeRef) -> Self {
        self.worktree = Some(worktree);
        self
    }

    /// Overrides the identity source used to mint the child's spec ids (mainly
    /// for deterministic tests).
    #[must_use]
    pub fn ids(mut self, ids: FacadeIds) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Finalizes the builder into a data-first [`LocalSubagent`].
    ///
    /// The produced delegate has an empty [`name`](LocalSubagent::name); the
    /// supervisor stamps the registration name through
    /// [`AgentBuilder::subagent`](crate::facade::AgentBuilder::subagent).
    ///
    /// # Errors
    ///
    /// Currently infallible, but returns [`FacadeError`] so future validation can
    /// be added without a breaking signature change (and so the documented
    /// `Agent::worker()...build()?` form reads naturally).
    pub fn build(self) -> Result<LocalSubagent, FacadeError> {
        let ids = self.ids.unwrap_or_default();
        let inherit_model = self.inherit_model || self.model.is_none();

        // An inheriting worker cannot know the supervisor model yet, so its spec
        // records a placeholder that the delegation fulfillment substitutes.
        let model_ref = if inherit_model {
            ModelRef::new(
                INHERITED_MODEL_PLACEHOLDER,
                nonzero_default_tokens(),
                None,
                None,
            )
        } else {
            let mut model = ModelConfig::new(
                self.model
                    .clone()
                    .expect("explicit model present when not inheriting"),
            );
            if let Some(max_tokens) = self.max_tokens {
                model = model.max_tokens(max_tokens);
            }
            if let Some(temperature) = self.temperature {
                model = model.temperature(temperature);
            }
            model.to_model_ref()
        };

        let loop_policy: LoopPolicy = build_loop_policy(
            self.max_steps.unwrap_or(DEFAULT_MAX_STEPS),
            self.max_tool_rounds.unwrap_or(DEFAULT_MAX_TOOL_ROUNDS),
            self.tool_failure_policy
                .unwrap_or(ToolFailurePolicy::ReturnErrorToModel),
        );

        let tools = ToolSetRef::new(ids.tool_set_id(), self.extra_declarations);
        let spec = AgentSpec::new(
            ids.agent_id(),
            self.worktree.unwrap_or_else(|| WorktreeRef::new(".")),
            self.system,
            tools.clone(),
            model_ref,
            loop_policy,
        );

        Ok(LocalSubagent {
            name: String::new(),
            description: self.description.unwrap_or_default(),
            spec,
            tools,
            approval: self.approval.unwrap_or_default(),
            inherit_model,
        })
    }
}

/// The default token ceiling stamped onto an inheriting worker's placeholder
/// model (mirrors [`ModelConfig::new`]'s default).
fn nonzero_default_tokens() -> std::num::NonZeroU32 {
    ModelConfig::new(INHERITED_MODEL_PLACEHOLDER)
        .to_model_ref()
        .max_tokens()
}

// ---------------------------------------------------------------------------
// Model-routed delegation (milestone M3-2)
// ---------------------------------------------------------------------------

/// The delegation tool-name prefix: each registered subagent is advertised as
/// `ask_<name>` so the supervising model can route work to it (§10.1).
const DELEGATION_TOOL_PREFIX: &str = "ask_";

/// Greatest [`RunContext::depth`](crate::agent::RunContext::depth) at which a
/// delegation may still derive a child.
///
/// The supervisor drives at depth `0`, so this bounds nested delegation; a child
/// asking to delegate past this depth is refused with a classified
/// [`AgentError::SubagentDepthExceeded`](crate::agent::AgentError).
pub(crate) const DEFAULT_MAX_DELEGATION_DEPTH: u32 = 8;

/// Builds the delegation tool name (`ask_<name>`) advertised for a delegate.
#[must_use]
pub(crate) fn delegation_tool_name(name: &str) -> String {
    format!("{DELEGATION_TOOL_PREFIX}{name}")
}

/// Synthesizes the delegation tool declaration advertised to the supervising
/// model for one registered subagent (§10.1).
///
/// The declaration takes a single required `task` string — the brief folded into
/// the child's opening turn when the model calls it. A worker with no
/// description gets a terse generated one so the tool is never advertised blank.
#[must_use]
pub(crate) fn delegation_declaration(name: &str, description: &str) -> ToolDecl {
    let description = if description.is_empty() {
        format!("Delegate a task to the `{name}` subagent.")
    } else {
        description.to_owned()
    };
    ToolDecl {
        name: delegation_tool_name(name),
        description,
        input_schema: json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task to delegate to the subagent."
                }
            },
            "required": ["task"]
        }),
    }
}

/// The delegation routing strategy for an agent's registered subagents
/// (`docs/facade-api.md` §10.2, §13.1).
///
/// Two shapes are supported at this milestone:
///
/// - [`model_routed`](Self::model_routed) (the default): every registered
///   subagent is advertised to the supervising model as its own
///   `ask_<name>(task)` tool. Separate tools make it easy for the model to call
///   the right delegate and keep each delegation's trace distinct.
/// - [`single_tool`](Self::single_tool): all delegates are collapsed behind one
///   unified `<name>(agent, task)` tool that routes to the requested delegate by
///   its `agent` argument. This suits a dynamic delegate roster or an outer
///   policy that wants to own routing.
///
/// ```
/// use agent_lib::facade::Delegation;
///
/// // Default: one tool per subagent.
/// let per_subagent = Delegation::model_routed().expose_subagents_as_tools();
///
/// // Advanced: a single unified delegation tool.
/// let unified = Delegation::single_tool("delegate");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    mode: DelegationMode,
}

/// The internal routing mode carried by a [`Delegation`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum DelegationMode {
    /// One `ask_<name>` tool per registered subagent (§13.1).
    PerSubagentTool,
    /// A single unified `<tool_name>(agent, task)` tool routing by `agent`
    /// (§10.2).
    SingleTool {
        /// The advertised name of the unified delegation tool.
        tool_name: String,
    },
}

impl Default for Delegation {
    /// The default is [`model_routed`](Self::model_routed): one tool per
    /// subagent, the mode closest to the ordinary tool-use loop (§13.1).
    fn default() -> Self {
        Self::model_routed()
    }
}

impl Delegation {
    /// Model-routed delegation: expose each subagent as its own `ask_<name>`
    /// tool (the default, §13.1).
    #[must_use]
    pub fn model_routed() -> Self {
        Self {
            mode: DelegationMode::PerSubagentTool,
        }
    }

    /// A no-op refinement making the model-routed intent explicit (§13.1).
    ///
    /// Model-routed delegation already exposes each subagent as a tool, so this
    /// only documents that choice at the call site; it is idempotent and leaves
    /// the mode unchanged.
    #[must_use]
    pub fn expose_subagents_as_tools(self) -> Self {
        self
    }

    /// A no-op refinement making the external-delegate intent explicit (§13.1).
    ///
    /// Model-routed delegation already exposes each registered managed external
    /// agent as its own `ask_<name>` tool, exactly like a local subagent, so this
    /// only documents that choice at the call site; it is idempotent and leaves
    /// the mode unchanged.
    #[must_use]
    pub fn expose_external_agents_as_tools(self) -> Self {
        self
    }

    /// Alias of [`expose_subagents_as_tools`](Self::expose_subagents_as_tools)
    /// matching the spelling used in `docs/facade-api.md` §13.1.
    #[must_use]
    pub fn expose_as_tools(self) -> Self {
        self
    }

    /// Single-tool delegation: collapse every delegate behind one unified
    /// `<tool_name>(agent, task)` tool that routes by the `agent` argument
    /// (§10.2).
    #[must_use]
    pub fn single_tool(tool_name: impl Into<String>) -> Self {
        Self {
            mode: DelegationMode::SingleTool {
                tool_name: tool_name.into(),
            },
        }
    }

    /// Synthesizes the tool declarations this delegation advertises for
    /// `subagents` and `external` delegates, appended to the supervisor's
    /// advertised tool set at build time (§10.1, §13.1).
    ///
    /// Model-routed delegation yields one `ask_<name>` declaration per delegate
    /// (local subagents first, then managed external agents); single-tool
    /// delegation yields exactly one unified declaration whose `agent` argument
    /// enumerates every delegate name.
    #[must_use]
    pub(crate) fn declarations(
        &self,
        subagents: &[LocalSubagent],
        external: &[ManagedExternalDelegate],
    ) -> Vec<ToolDecl> {
        match &self.mode {
            DelegationMode::PerSubagentTool => subagents
                .iter()
                .map(|delegate| delegation_declaration(delegate.name(), delegate.description()))
                .chain(external.iter().map(|delegate| {
                    delegation_declaration(delegate.name(), &delegate.description())
                }))
                .collect(),
            DelegationMode::SingleTool { tool_name } => {
                vec![delegation_single_tool_declaration(
                    tool_name, subagents, external,
                )]
            }
        }
    }

    /// Builds the per-run [`DelegationRoute`] that a
    /// [`DelegationToolHandler`] consults to recognize and dispatch delegation
    /// calls for `subagents` and `external` delegates.
    #[must_use]
    pub(crate) fn route(
        &self,
        subagents: &[LocalSubagent],
        external: &[ManagedExternalDelegate],
    ) -> DelegationRoute {
        match &self.mode {
            DelegationMode::PerSubagentTool => DelegationRoute::PerSubagent {
                local: subagents
                    .iter()
                    .map(|delegate| (delegation_tool_name(delegate.name()), delegate.clone()))
                    .collect(),
                external: external
                    .iter()
                    .map(|delegate| (delegation_tool_name(delegate.name()), delegate.clone()))
                    .collect(),
            },
            DelegationMode::SingleTool { tool_name } => DelegationRoute::SingleTool {
                tool_name: tool_name.clone(),
                local_by_name: subagents
                    .iter()
                    .map(|delegate| (delegate.name().to_owned(), delegate.clone()))
                    .collect(),
                external_by_name: external
                    .iter()
                    .map(|delegate| (delegate.name().to_owned(), delegate.clone()))
                    .collect(),
            },
        }
    }

    /// Returns the model-routed start-tool names for `external` delegates that
    /// the drive layer gates (`ask_<name>`), so the machine tool gate can exempt
    /// them and avoid double-prompting the same start (§9.2).
    ///
    /// Single-tool delegation shares one tool name across every delegate, so it
    /// cannot be safely exempted at the machine layer; it yields no exemptions
    /// here and its unified tool passes through the ordinary machine gate.
    #[must_use]
    pub(crate) fn external_tool_names(&self, external: &[ManagedExternalDelegate]) -> Vec<String> {
        match &self.mode {
            DelegationMode::PerSubagentTool => external
                .iter()
                .map(|delegate| delegation_tool_name(delegate.name()))
                .collect(),
            DelegationMode::SingleTool { .. } => Vec::new(),
        }
    }
}

/// Synthesizes the unified single-tool delegation declaration (§10.2).
///
/// The tool takes a required `agent` (which delegate to route to, enumerated
/// from every registered delegate name, local subagents then managed external
/// agents) and a required `task` (the brief). The description lists the
/// available delegates so the model can choose.
#[must_use]
pub(crate) fn delegation_single_tool_declaration(
    tool_name: &str,
    subagents: &[LocalSubagent],
    external: &[ManagedExternalDelegate],
) -> ToolDecl {
    let names: Vec<Value> = subagents
        .iter()
        .map(|delegate| Value::String(delegate.name().to_owned()))
        .chain(
            external
                .iter()
                .map(|delegate| Value::String(delegate.name().to_owned())),
        )
        .collect();
    let roster = subagents
        .iter()
        .map(|delegate| {
            if delegate.description().is_empty() {
                delegate.name().to_owned()
            } else {
                format!("`{}` ({})", delegate.name(), delegate.description())
            }
        })
        .chain(
            external
                .iter()
                .map(|delegate| format!("`{}` ({})", delegate.name(), delegate.description())),
        )
        .collect::<Vec<_>>()
        .join(", ");
    let description = if roster.is_empty() {
        "Delegate a task to a subagent by name.".to_owned()
    } else {
        format!("Delegate a task to one of the available subagents: {roster}.")
    };
    ToolDecl {
        name: tool_name.to_owned(),
        description,
        input_schema: json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Which subagent to delegate to.",
                    "enum": names
                },
                "task": {
                    "type": "string",
                    "description": "The task to delegate to the subagent."
                }
            },
            "required": ["agent", "task"]
        }),
    }
}

/// One recorded delegation: its [`DelegationTrace`], the artifacts an external
/// delegate reported, and whether it routed to an external agent.
///
/// The [`DelegationToolHandler`] writes one entry per delegation call; the run
/// assembly (`collect_traces`) reads it to split delegation calls out from
/// ordinary tool calls, to fold child usage into the summary (external usage is
/// folded separately from local-subagent usage, §17.3), and to surface external
/// [`artifacts`](RecordedDelegation::artifacts) on the run output.
#[derive(Clone, Debug)]
pub(crate) struct RecordedDelegation {
    /// The trace (delegate name, terminal status, usage) for this call.
    pub trace: DelegationTrace,
    /// Artifacts an external delegate reported; always empty for a local
    /// subagent.
    pub artifacts: Vec<ArtifactRef>,
    /// Whether this delegation routed to a managed external agent.
    pub is_external: bool,
    /// Whether this external delegation was denied before it started by the
    /// approval policy (§9.2). Always `false` for a local subagent. The Agent
    /// facade folds this into a run-level
    /// [`FacadeError::ApprovalDenied`](crate::facade::FacadeError::ApprovalDenied).
    pub approval_denied: bool,
    /// The resumable session facts an external delegate's last drive reported, if
    /// any; retained data-only for a later snapshot (§15.2). Always `None` for a
    /// local subagent.
    pub session: Option<ExternalSessionRef>,
}

/// A per-run map from a delegation call's framework id to its recorded trace.
///
/// The [`DelegationToolHandler`] writes one entry per delegation call; the run
/// assembly (`collect_traces`) reads it to split delegation calls out from
/// ordinary tool calls and to fold child usage into the summary.
pub(crate) type DelegationRecorder = Arc<Mutex<HashMap<String, RecordedDelegation>>>;

/// Creates an empty [`DelegationRecorder`].
#[must_use]
pub(crate) fn new_delegation_recorder() -> DelegationRecorder {
    Arc::new(Mutex::new(HashMap::new()))
}

/// The final text and usage captured from a driven child turn.
struct ChildSummary {
    /// The child's final assistant text, folded back as the tool result.
    text: String,
    /// The child's aggregated token usage for the delegated turn.
    usage: Usage,
}

/// A shared, single-slot capture of a child's [`ChildSummary`].
type ChildSummarySlot = Arc<Mutex<Option<ChildSummary>>>;

/// Wraps a child [`DefaultAgentMachine`] to capture its final turn summary.
///
/// [`SubagentSpawner::summarize`] only observes the drained [`TurnDone`], never
/// the child machine state, so this wrapper snapshots the committed turn's text
/// and usage into a shared slot the instant the child cursor reaches
/// [`Done`](LoopCursor::Done). The [`DelegationToolHandler`] then reads the slot
/// to fold the summary back as the tool result and to record the child usage.
struct RecordingChildMachine {
    inner: DefaultAgentMachine,
    slot: ChildSummarySlot,
}

impl AgentMachine for RecordingChildMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        let outcome = self.inner.step(input);
        if matches!(self.inner.cursor(), LoopCursor::Done(_)) {
            let (text, usage, _stop_reason) = final_turn_summary(self.inner.state().conversation());
            *self.slot.lock().expect("child summary slot poisoned") =
                Some(ChildSummary { text, usage });
        }
        outcome
    }

    fn cursor(&self) -> &LoopCursor {
        self.inner.cursor()
    }
}

/// The child's own drain layer: the shared LLM client, a declaration-only tool
/// registry, and the child's approval handler.
///
/// A subagent stays data-first (declaration-only tools), so the tool handler
/// only serves declared names; an approval-requiring child tool still pauses on
/// the child's [`FacadeApproval`] before any execution (§9.2). Requirements this
/// scope cannot serve pop to the outer [`EmptyScope`].
struct ChildAgentScope {
    llm: LlmClientHandler,
    tool: ToolRegistryHandler,
    interaction: Arc<FacadeApproval>,
}

impl HandlerScope for ChildAgentScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        Some(&self.llm)
    }

    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&self.tool)
    }

    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        Some(self.interaction.as_ref())
    }
}

/// An empty outer layer for the child drive.
///
/// The child's own [`ChildAgentScope`] serves every family it emits, so nothing
/// should pop here; an unexpected pop surfaces as an
/// [`AgentError::UnhandledRequirement`](crate::agent::AgentError), the correct
/// failure for a child asking for a capability M3-2 does not wire (for example
/// nested delegation).
#[derive(Default)]
struct EmptyScope;

impl HandlerScope for EmptyScope {}

/// Turns one delegation into a drivable child machine, scope, and opening input.
///
/// Built fresh per delegation call so its capture `slot` is call-local: the
/// [`RecordingChildMachine`] it spawns writes the child's final summary there,
/// and [`summarize`](SubagentSpawner::summarize) reads it back. The child spec
/// is rebuilt from the delegate's data-first [`AgentSpec`], substituting the
/// supervisor's model when the worker inherits (R4).
struct FacadeSubagentSpawner {
    subagent: LocalSubagent,
    client: Arc<dyn LlmClient>,
    supervisor_model: ModelRef,
    ids: FacadeIds,
    task: String,
    cancel: CancellationToken,
    trace: TraceHandle,
    slot: ChildSummarySlot,
}

impl SubagentSpawner for FacadeSubagentSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        Ok((
            self.ids.run_id(),
            TraceNodeId::new(format!("subagent:{}", self.subagent.name())),
        ))
    }

    fn spawn(
        &self,
        _spec_ref: &AgentSpecRef,
        _brief: &Interaction,
        _result_schema: Option<&Value>,
    ) -> Result<SpawnedChild, AgentError> {
        // R4: an inheriting worker adopts the supervisor's concrete model; an
        // explicit worker keeps its own.
        let effective_model = if self.subagent.inherits_model() {
            self.supervisor_model.clone()
        } else {
            self.subagent.spec().model().clone()
        };
        let base = self.subagent.spec();
        let child_spec = AgentSpec::new(
            base.id(),
            base.worktree().clone(),
            base.system_prompt().map(str::to_owned),
            base.initial_tools().clone(),
            effective_model,
            *base.loop_policy(),
        );

        let child_state = AgentState::new(
            child_spec,
            Conversation::new(self.ids.conversation_id(), ConversationConfig::new(None)),
        );

        // One FacadeApproval bridges the child's ToolApprovalPolicy and its
        // scope InteractionHandler, so an approval-requiring child tool pauses.
        let child_approval = Arc::new(FacadeApproval::new(self.subagent.approval().clone()));
        let child_machine = assemble_machine(child_state, &self.ids, child_approval.clone());
        let recording = RecordingChildMachine {
            inner: child_machine,
            slot: self.slot.clone(),
        };

        let context = ToolContextParts {
            run_id: self.ids.run_id(),
            agent_id: base.id(),
            worktree: base.worktree().clone(),
            cancel: self.cancel.clone(),
            trace: self.trace.clone(),
        };
        let registry = FacadeToolRegistry::new(
            Vec::new(),
            None,
            self.subagent.tools().tools().to_vec(),
            context,
        )
        .map_err(|error| AgentError::Other(error.to_string()))?;
        let registry: Arc<dyn ToolRegistry> = Arc::new(registry);

        let scope = ChildAgentScope {
            llm: LlmClientHandler::new(self.client.clone()),
            tool: ToolRegistryHandler::new(registry),
            interaction: child_approval,
        };

        let user = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: self.task.clone(),
                extra: Map::new(),
            }],
        };
        let opening = AgentInput::user_message(
            self.ids.turn_id(),
            self.ids.message_id(),
            user,
            self.ids.message_id(),
            self.ids.step_id(),
        )?;

        Ok(SpawnedChild {
            machine: Box::new(recording),
            scope: Box::new(scope),
            opening,
        })
    }

    fn summarize(&self, _done: &TurnDone) -> SubagentOutput {
        let summary = self
            .slot
            .lock()
            .expect("child summary slot poisoned")
            .as_ref()
            .map(|captured| captured.text.clone())
            .unwrap_or_default();
        SubagentOutput { summary }
    }
}

/// The run-scoped routing table a [`DelegationToolHandler`] consults to
/// recognize a delegation tool call and select the target subagent.
///
/// Built once per run from the agent's [`Delegation`] config and its registered
/// delegates (see [`Delegation::route`]). Two shapes mirror the two delegation
/// modes: [`PerSubagent`](Self::PerSubagent) keys delegates by their synthesized
/// `ask_<name>` tool name, while [`SingleTool`](Self::SingleTool) recognizes one
/// unified tool name and routes by the call's `agent` argument.
pub(crate) enum DelegationRoute {
    /// Model-routed: `ask_<name>` tool name → delegate (§13.1). Local subagents
    /// and managed external agents share the tool-name space but keep separate
    /// maps so the handler can pick the right fulfillment path.
    PerSubagent {
        /// Local subagents keyed by their `ask_<name>` tool name.
        local: HashMap<String, LocalSubagent>,
        /// Managed external agents keyed by their `ask_<name>` tool name.
        external: HashMap<String, ManagedExternalDelegate>,
    },
    /// Single-tool: one unified tool name plus delegate-name → delegate maps the
    /// `agent` argument selects into (§10.2).
    SingleTool {
        /// The advertised name of the unified delegation tool.
        tool_name: String,
        /// Local subagents keyed by their registration name.
        local_by_name: HashMap<String, LocalSubagent>,
        /// Managed external agents keyed by their registration name.
        external_by_name: HashMap<String, ManagedExternalDelegate>,
    },
}

impl DelegationRoute {
    /// Reports whether `name` is this route's delegation tool.
    fn is_delegation(&self, name: &str) -> bool {
        match self {
            Self::PerSubagent { local, external } => {
                local.contains_key(name) || external.contains_key(name)
            }
            Self::SingleTool { tool_name, .. } => name == tool_name,
        }
    }

    /// Resolves a tool call into the target delegate and task brief, or reports
    /// that the call is not a delegation / names an unknown delegate.
    fn resolve<'a>(&'a self, call: &ToolCall) -> Resolved<'a> {
        let task = call
            .input
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match self {
            Self::PerSubagent { local, external } => {
                if let Some(subagent) = local.get(&call.name) {
                    Resolved::Delegate { subagent, task }
                } else if let Some(delegate) = external.get(&call.name) {
                    Resolved::External { delegate, task }
                } else {
                    Resolved::NotDelegation
                }
            }
            Self::SingleTool {
                tool_name,
                local_by_name,
                external_by_name,
            } => {
                if &call.name != tool_name {
                    return Resolved::NotDelegation;
                }
                let requested = call
                    .input
                    .get("agent")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if let Some(subagent) = local_by_name.get(&requested) {
                    Resolved::Delegate { subagent, task }
                } else if let Some(delegate) = external_by_name.get(&requested) {
                    Resolved::External { delegate, task }
                } else {
                    Resolved::UnknownDelegate {
                        requested,
                        available: {
                            let mut names: Vec<&str> = local_by_name
                                .keys()
                                .chain(external_by_name.keys())
                                .map(String::as_str)
                                .collect();
                            names.sort_unstable();
                            names.join(", ")
                        },
                    }
                }
            }
        }
    }
}

/// The outcome of resolving a tool call against a [`DelegationRoute`].
enum Resolved<'a> {
    /// The call routes to a local `subagent` with the given `task` brief.
    Delegate {
        subagent: &'a LocalSubagent,
        task: String,
    },
    /// The call routes to a managed external `delegate` with the given `task`
    /// brief.
    External {
        delegate: &'a ManagedExternalDelegate,
        task: String,
    },
    /// A single-tool delegation named a delegate that is not registered.
    UnknownDelegate {
        requested: String,
        available: String,
    },
    /// The call is not a delegation and belongs to the base registry.
    NotDelegation,
}

/// The run-scoped [`ToolHandler`] that routes delegation tool calls to the
/// subagent path and forwards every other call to the base registry handler.
///
/// A call the run's [`DelegationRoute`] recognizes — either a model-routed
/// `ask_<name>` tool or the unified single-tool name — is fulfilled by building
/// a child machine from the target delegate's data-first spec and driving it
/// through the reference
/// [`DrivingSubagentHandler`](crate::agent::DrivingSubagentHandler) — the same
/// `NeedSubagent` mechanism the agent layer already owns — then folding the
/// child's summary back as the tool result and recording a [`DelegationTrace`].
/// Any other call is delegated to the wrapped
/// [`ToolRegistryHandler`](crate::agent::ToolRegistryHandler) unchanged, so an
/// agent with no delegates behaves exactly as before (§10.1, §19).
pub(crate) struct DelegationToolHandler {
    base: ToolRegistryHandler,
    route: DelegationRoute,
    client: Arc<dyn LlmClient>,
    supervisor_model: ModelRef,
    ids: FacadeIds,
    recorder: DelegationRecorder,
    approval: Arc<FacadeApproval>,
    max_depth: u32,
}

impl DelegationToolHandler {
    /// Wraps `base`, routing calls the `route` recognizes through the subagent
    /// path and recording each delegation's trace into `recorder`.
    ///
    /// `approval` is the run's [`FacadeApproval`]; a managed external delegate is
    /// gated through its
    /// [`resolve_external_start`](FacadeApproval::resolve_external_start) before
    /// it is driven (§9.2).
    pub(crate) fn new(
        base: ToolRegistryHandler,
        route: DelegationRoute,
        client: Arc<dyn LlmClient>,
        supervisor_model: ModelRef,
        ids: FacadeIds,
        recorder: DelegationRecorder,
        approval: Arc<FacadeApproval>,
    ) -> Self {
        Self {
            base,
            route,
            client,
            supervisor_model,
            ids,
            recorder,
            approval,
            max_depth: DEFAULT_MAX_DELEGATION_DEPTH,
        }
    }

    /// Reports whether `name` is a registered delegation tool.
    pub(crate) fn is_delegation(&self, name: &str) -> bool {
        self.route.is_delegation(name)
    }

    /// Drives one delegation to completion and folds its summary back as the
    /// tool result, recording the delegation trace under `call_id`.
    async fn drive_delegation(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        subagent: &LocalSubagent,
        task: String,
        ctx: &RunContext,
    ) -> RequirementResult {
        let slot: ChildSummarySlot = Arc::new(Mutex::new(None));
        let spawner = Arc::new(FacadeSubagentSpawner {
            subagent: subagent.clone(),
            client: self.client.clone(),
            supervisor_model: self.supervisor_model.clone(),
            ids: self.ids.clone(),
            task: task.clone(),
            cancel: ctx.cancellation().clone(),
            trace: ctx.trace().clone(),
            slot: slot.clone(),
        });
        let handler = DrivingSubagentHandler::new(spawner, self.max_depth);

        let spec_ref = AgentSpecRef(subagent.spec().id());
        let brief = Interaction::question(self.ids.step_id(), task);
        let empty = EmptyScope;
        let mut outer = ScopePop::new(&empty, None);

        let result = handler
            .fulfill(&spec_ref, &brief, None, &mut outer, ctx)
            .await;

        let child_usage = slot
            .lock()
            .expect("child summary slot poisoned")
            .as_ref()
            .map(|captured| captured.usage.clone())
            .unwrap_or_default();

        match result {
            RequirementResult::Subagent(Ok(output)) => {
                self.record(
                    &call_id,
                    subagent.name(),
                    DelegationStatus::Completed,
                    child_usage,
                );
                RequirementResult::Tool(Ok(delegation_response(call, &output.summary)))
            }
            RequirementResult::Subagent(Err(error)) => {
                let message = error.to_string();
                self.record(
                    &call_id,
                    subagent.name(),
                    DelegationStatus::Failed,
                    child_usage,
                );
                RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed {
                    tool_name: call.name.clone(),
                    message,
                }))
            }
            // The reference handler only ever returns a `Subagent` result;
            // forward any other family unchanged so a future variant is not
            // silently dropped.
            other => other,
        }
    }

    /// Records one delegation trace under its framework call id.
    fn record(&self, call_id: &ToolCallId, delegate: &str, status: DelegationStatus, usage: Usage) {
        self.recorder
            .lock()
            .expect("delegation recorder poisoned")
            .insert(
                call_id.to_string(),
                RecordedDelegation {
                    trace: DelegationTrace {
                        delegate: delegate.to_owned(),
                        status,
                        usage,
                    },
                    artifacts: Vec::new(),
                    is_external: false,
                    approval_denied: false,
                    session: None,
                },
            );
    }

    /// Drives one managed external delegation and folds its summary back as the
    /// tool result, recording the delegation trace, usage, and artifacts under
    /// `call_id`.
    ///
    /// The external agent is driven through the shared
    /// [`drive_external`](crate::facade::external) helper — the same
    /// `NeedSubagent`/[`DrivingSubagentHandler`] mechanism a local subagent uses
    /// — so cancellation, budget, and trace propagation are identical. A drive
    /// that reaches its terminal `Done` cursor without a lingering cleanup marker
    /// is [`Completed`](DelegationStatus::Completed); a failed drive or a
    /// cancel-abandoned session is [`Failed`](DelegationStatus::Failed) and folds
    /// a classified error back to the model.
    async fn drive_external_delegation(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        delegate: &ManagedExternalDelegate,
        task: String,
        ctx: &RunContext,
    ) -> RequirementResult {
        // Gate the external start at the drive layer (§9.2). The machine tool
        // gate exempts the delegate's start tool, so this is the sole authority.
        if !self.approval.resolve_external_start(&call.name) {
            self.record_external(
                &call_id,
                delegate.name(),
                DelegationStatus::Failed,
                Usage::default(),
                Vec::new(),
                None,
                true,
            );
            return RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed {
                tool_name: call.name.clone(),
                message: format!(
                    "managed external agent `{}` denied by approval policy",
                    delegate.name()
                ),
            }));
        }
        match drive_external(delegate.name(), delegate.agent(), &self.ids, task, ctx).await {
            Ok(outcome) => {
                let status = if outcome.completed && !outcome.cleanup_required {
                    DelegationStatus::Completed
                } else {
                    DelegationStatus::Failed
                };
                let summary = outcome.summary;
                self.record_external(
                    &call_id,
                    delegate.name(),
                    status,
                    outcome.usage,
                    outcome.artifacts,
                    outcome.session,
                    false,
                );
                match status {
                    DelegationStatus::Completed => {
                        RequirementResult::Tool(Ok(delegation_response(call, &summary)))
                    }
                    DelegationStatus::Failed => {
                        RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed {
                            tool_name: call.name.clone(),
                            message: "external delegation did not reach a completed state"
                                .to_owned(),
                        }))
                    }
                }
            }
            Err(error) => {
                self.record_external(
                    &call_id,
                    delegate.name(),
                    DelegationStatus::Failed,
                    Usage::default(),
                    Vec::new(),
                    None,
                    false,
                );
                RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed {
                    tool_name: call.name.clone(),
                    message: error.to_string(),
                }))
            }
        }
    }

    /// Records one external delegation trace (with any reported artifacts and
    /// resumable session) under its framework call id.
    #[allow(clippy::too_many_arguments)]
    fn record_external(
        &self,
        call_id: &ToolCallId,
        delegate: &str,
        status: DelegationStatus,
        usage: Usage,
        artifacts: Vec<ArtifactRef>,
        session: Option<ExternalSessionRef>,
        approval_denied: bool,
    ) {
        self.recorder
            .lock()
            .expect("delegation recorder poisoned")
            .insert(
                call_id.to_string(),
                RecordedDelegation {
                    trace: DelegationTrace {
                        delegate: delegate.to_owned(),
                        status,
                        usage,
                    },
                    artifacts,
                    is_external: true,
                    approval_denied,
                    session,
                },
            );
    }
}

#[async_trait]
impl ToolHandler for DelegationToolHandler {
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        ctx: &RunContext,
    ) -> RequirementResult {
        match self.route.resolve(call) {
            Resolved::Delegate { subagent, task } => {
                self.drive_delegation(call_id, call, subagent, task, ctx)
                    .await
            }
            Resolved::External { delegate, task } => {
                self.drive_external_delegation(call_id, call, delegate, task, ctx)
                    .await
            }
            Resolved::UnknownDelegate {
                requested,
                available,
            } => {
                // A single-tool delegation named a delegate that is not
                // registered. Record a failed trace (so the tap still brackets
                // it) and fold a classified error back to the model.
                self.record(
                    &call_id,
                    &requested,
                    DelegationStatus::Failed,
                    Usage::default(),
                );
                RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed {
                    tool_name: call.name.clone(),
                    message: format!(
                        "unknown delegate `{requested}`; available delegates: {available}"
                    ),
                }))
            }
            Resolved::NotDelegation => self.base.fulfill(call_id, call, ctx).await,
        }
    }
}

/// Folds a child's summary text into the [`ToolResponse`] returned for the
/// delegation tool call, keyed by the provider call id the model matches.
fn delegation_response(call: &ToolCall, summary: &str) -> ToolResponse {
    ToolResponse {
        tool_call_id: call.id.clone(),
        content: vec![ContentBlock::Text {
            text: summary.to_owned(),
            extra: Map::new(),
        }],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentWorkerBuilder, INHERITED_MODEL_PLACEHOLDER, LocalSubagent};
    use crate::facade::approval::Approval;
    use crate::facade::ids::FacadeIds;
    use crate::model::tool::Tool as ToolDecl;
    use serde_json::json;

    fn worker() -> AgentWorkerBuilder {
        AgentWorkerBuilder::default()
    }

    fn review_decl() -> ToolDecl {
        ToolDecl {
            name: "grep".to_owned(),
            description: "Search the tree.".to_owned(),
            input_schema: json!({ "type": "object" }),
        }
    }

    #[test]
    fn explicit_model_worker_is_data_only_and_not_inheriting() {
        let sub = worker()
            .description("Strict reviewer")
            .model("gpt-5.5")
            .temperature(0.1)
            .system("You review code.")
            .build()
            .expect("worker builds");

        assert_eq!(sub.name(), "");
        assert_eq!(sub.description(), "Strict reviewer");
        assert!(!sub.inherits_model());
        assert_eq!(sub.spec().model().model(), "gpt-5.5");
        assert_eq!(sub.spec().model().temperature(), Some(0.1));
        assert_eq!(sub.spec().system_prompt(), Some("You review code."));
        assert!(sub.tools().tools().is_empty());

        // Data-first: the spec round-trips through serde with no runtime handles.
        let value = serde_json::to_value(sub.spec()).expect("spec serializes");
        assert_eq!(value["model"]["model"], "gpt-5.5");
    }

    #[test]
    fn worker_inherits_model_by_default() {
        let sub = worker().system("reviewer").build().expect("worker builds");

        assert!(sub.inherits_model());
        assert_eq!(sub.spec().model().model(), INHERITED_MODEL_PLACEHOLDER);
    }

    #[test]
    fn inherit_and_explicit_toggle_last_call_wins() {
        // model(..) after inherit_model() pins the model.
        let pinned = worker()
            .inherit_model()
            .model("gpt-5.5")
            .build()
            .expect("worker builds");
        assert!(!pinned.inherits_model());
        assert_eq!(pinned.spec().model().model(), "gpt-5.5");

        // inherit_model() after model(..) reverts to inheritance.
        let inherited = worker()
            .model("gpt-5.5")
            .inherit_model()
            .build()
            .expect("worker builds");
        assert!(inherited.inherits_model());
        assert_eq!(
            inherited.spec().model().model(),
            INHERITED_MODEL_PLACEHOLDER
        );
    }

    #[test]
    fn tool_declarations_flow_into_the_child_spec() {
        let sub = worker()
            .tool_declarations(vec![review_decl()])
            .build()
            .expect("worker builds");

        assert_eq!(sub.tools().tools().len(), 1);
        assert_eq!(sub.tools().tools()[0].name, "grep");
        // The spec's initial tool set mirrors the exposed declarations.
        assert_eq!(
            sub.spec().initial_tools().tools()[0].name,
            sub.tools().tools()[0].name
        );
    }

    #[test]
    fn approval_policy_is_carried_through() {
        let sub = worker()
            .approval(Approval::auto_deny())
            .build()
            .expect("worker builds");
        // A defaulted policy is present and usable (data, no handler required).
        let _ = sub.approval();
    }

    #[test]
    fn deterministic_ids_yield_stable_spec_identity() {
        let a = worker()
            .ids(FacadeIds::seeded(100))
            .build()
            .expect("worker builds");
        let b = worker()
            .ids(FacadeIds::seeded(100))
            .build()
            .expect("worker builds");
        assert_eq!(a.spec().id(), b.spec().id());
    }

    #[test]
    fn with_name_stamps_the_registration_name() {
        let sub = worker()
            .build()
            .expect("worker builds")
            .with_name("reviewer");
        assert_eq!(sub.name(), "reviewer");
        // Cloning a LocalSubagent keeps it data-only and equal by field.
        let clone: LocalSubagent = sub.clone();
        assert_eq!(clone.name(), "reviewer");
    }

    #[test]
    fn delegation_declaration_advertises_ask_tool_with_task_input() {
        use super::{delegation_declaration, delegation_tool_name};

        assert_eq!(delegation_tool_name("reviewer"), "ask_reviewer");

        let decl = delegation_declaration("reviewer", "Strict code reviewer.");
        assert_eq!(decl.name, "ask_reviewer");
        assert_eq!(decl.description, "Strict code reviewer.");
        assert_eq!(decl.input_schema["properties"]["task"]["type"], "string");
        assert_eq!(decl.input_schema["required"][0], "task");

        // A blank description gets a terse generated one so no tool is advertised
        // without any hint of its purpose.
        let generated = delegation_declaration("researcher", "");
        assert_eq!(
            generated.description,
            "Delegate a task to the `researcher` subagent."
        );
    }
}

/// Offline coverage for the model-routed delegation path (milestone M3-2).
///
/// Every test is fully offline: a [`RoutingClient`] returns scripted responses
/// selected by the requesting agent's system prompt, so the supervisor and each
/// child are driven deterministically with no network, credential, or CLI, and
/// each finishes well under a second.
#[cfg(test)]
mod model_routed_tests {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use serde_json::{Map, json};

    use crate::client::{Capability, ChatRequest, ClientError, LlmClient, Response};
    use crate::facade::approval::{Approval, ApprovalDecision, ApprovalPolicy};
    use crate::facade::run::{DelegationStatus, RunEvent};
    use crate::facade::{Agent, AgentBuilder};
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
            let mut calls = route.calls.lock().expect("route calls");
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

    #[tokio::test]
    async fn model_routed_delegation_drives_child_and_folds_result() {
        let client = RoutingClient::new(vec![
            route(
                "SUPERVISOR",
                vec![
                    tool_call_response(
                        "del-1",
                        "ask_reviewer",
                        json!({ "task": "review the diff" }),
                    ),
                    text_response("Final: the reviewer approved."),
                ],
            ),
            route("REVIEWER", vec![text_response("LGTM: no issues found")]),
        ]);

        let reviewer = Agent::worker()
            .description("Strict code reviewer.")
            .system("You are the REVIEWER.")
            .build()
            .expect("worker builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("reviewer", reviewer)
            .build()
            .expect("agent builds");

        let output = agent.run_full("Please review the diff.").await.unwrap();

        // The supervisor advanced past the delegation to its final message.
        assert_eq!(output.reply.text(), "Final: the reviewer approved.");

        // The child summary was folded back as the delegation tool result.
        assert!(
            tool_result_texts(&agent)
                .iter()
                .any(|text| text == "LGTM: no issues found"),
            "the child summary is folded back as the tool result"
        );

        // Exactly one delegation trace, attributed to the reviewer, completed,
        // carrying the child's usage.
        assert_eq!(output.delegations.len(), 1);
        let trace = &output.delegations[0];
        assert_eq!(trace.delegate, "reviewer");
        assert_eq!(trace.status, DelegationStatus::Completed);
        assert_eq!(trace.usage.input, 11);
        assert_eq!(trace.usage.output, 7);

        // Child usage is attributed to the subagent slice, not the supervisor.
        assert_eq!(output.usage.subagents.input, 11);
        assert_eq!(output.usage.subagents.output, 7);

        // The delegation is not double-counted as an ordinary tool call.
        assert!(
            output.tool_calls.is_empty(),
            "a delegation is not an ordinary tool call"
        );

        // The event order brackets the delegation with Started then Finished.
        let started = output
            .events
            .iter()
            .position(|event| matches!(event, RunEvent::DelegationStarted(_)))
            .expect("a DelegationStarted event");
        let finished = output
            .events
            .iter()
            .position(|event| matches!(event, RunEvent::DelegationFinished(_)))
            .expect("a DelegationFinished event");
        assert!(started < finished, "DelegationStarted precedes Finished");
        assert!(
            !output
                .events
                .iter()
                .any(|event| matches!(event, RunEvent::ToolStarted(_))),
            "no ordinary tool events for a delegation"
        );
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

    #[tokio::test]
    async fn model_routed_external_delegation_records_trace_artifacts_and_usage() {
        use crate::facade::ManagedExternalAgent;

        let client = RoutingClient::new(vec![route(
            "SUPERVISOR",
            vec![
                tool_call_response(
                    "del-1",
                    "ask_coder",
                    json!({ "task": "refactor the parser" }),
                ),
                text_response("Final: the coder finished."),
            ],
        )]);

        let handler = completed_external_handler(
            "refactor complete",
            "src/parser.rs",
            Usage {
                input: 13,
                output: 9,
                ..Usage::default()
            },
        );
        let coder = ManagedExternalAgent::claude_code()
            .session_handler(Arc::new(handler))
            .build()
            .expect("managed external agent builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("coder", coder)
            .build()
            .expect("agent builds");

        let output = agent.run_full("Please refactor the parser.").await.unwrap();

        // The supervisor advanced past the delegation to its final message.
        assert_eq!(output.reply.text(), "Final: the coder finished.");

        // The external session summary was folded back as the tool result.
        assert!(
            tool_result_texts(&agent)
                .iter()
                .any(|text| text == "refactor complete"),
            "the external summary is folded back as the tool result"
        );

        // Exactly one delegation trace, attributed to the external delegate,
        // completed, carrying the runtime-reported usage.
        assert_eq!(output.delegations.len(), 1);
        let trace = &output.delegations[0];
        assert_eq!(trace.delegate, "coder");
        assert_eq!(trace.status, DelegationStatus::Completed);
        assert_eq!(trace.usage.input, 13);
        assert_eq!(trace.usage.output, 9);

        // External usage is attributed to the external slice, not the subagent or
        // supervisor slices (§17.3).
        assert_eq!(output.usage.external.input, 13);
        assert_eq!(output.usage.external.output, 9);
        assert_eq!(output.usage.subagents.input, 0);
        assert_eq!(output.usage.subagents.output, 0);

        // The reported artifact surfaces on the run output, projected to its
        // locating path.
        assert_eq!(output.artifacts.len(), 1);
        assert_eq!(output.artifacts[0].path, "src/parser.rs");

        // The delegation is not double-counted as an ordinary tool call.
        assert!(
            output.tool_calls.is_empty(),
            "an external delegation is not an ordinary tool call"
        );
        assert!(
            !output
                .events
                .iter()
                .any(|event| matches!(event, RunEvent::ToolStarted(_))),
            "no ordinary tool events for a delegation"
        );

        // The event order brackets the delegation with Started, then the
        // artifact, then Finished.
        let started = output
            .events
            .iter()
            .position(|event| matches!(event, RunEvent::DelegationStarted(_)))
            .expect("a DelegationStarted event");
        let artifact = output
            .events
            .iter()
            .position(|event| matches!(event, RunEvent::DelegationArtifact(_)))
            .expect("a DelegationArtifact event");
        let finished = output
            .events
            .iter()
            .position(|event| matches!(event, RunEvent::DelegationFinished(_)))
            .expect("a DelegationFinished event");
        assert!(
            started < artifact,
            "DelegationStarted precedes the artifact"
        );
        assert!(
            artifact < finished,
            "the artifact precedes DelegationFinished"
        );
    }

    #[tokio::test]
    async fn external_delegate_is_advertised_as_an_ask_tool() {
        use crate::facade::ManagedExternalAgent;

        let handler = completed_external_handler(
            "done",
            "src/lib.rs",
            Usage {
                input: 1,
                output: 1,
                ..Usage::default()
            },
        );
        let coder = ManagedExternalAgent::claude_code()
            .session_handler(Arc::new(handler))
            .build()
            .expect("managed external agent builds");

        let agent = AgentBuilder::default()
            .client(RoutingClient::new(vec![route(
                "SUPERVISOR",
                vec![text_response("nothing to do")],
            )]))
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .external_agent("coder", coder)
            .build()
            .expect("agent builds");

        // The delegate is registered and exposed as its own `ask_coder` tool.
        assert_eq!(agent.external_agents().len(), 1);
        assert_eq!(agent.external_agents()[0].name(), "coder");
        assert!(
            agent
                .state()
                .spec()
                .initial_tools()
                .tools()
                .iter()
                .any(|tool| tool.name == "ask_coder"),
            "the external delegate mints an `ask_coder` delegation tool"
        );
    }

    #[tokio::test]
    async fn external_delegation_without_session_handler_fails_the_delegation() {
        use crate::facade::ManagedExternalAgent;

        let client = RoutingClient::new(vec![route(
            "SUPERVISOR",
            vec![
                tool_call_response(
                    "del-1",
                    "ask_coder",
                    json!({ "task": "refactor the parser" }),
                ),
                text_response("Final: gave up on the coder."),
            ],
        )]);

        // No session handler is attached, so the delegation cannot be driven.
        let coder = ManagedExternalAgent::claude_code()
            .build()
            .expect("managed external agent builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("coder", coder)
            .build()
            .expect("agent builds");

        let output = agent.run_full("Please refactor the parser.").await.unwrap();

        // The supervisor still reached its final message after the failed tool.
        assert_eq!(output.reply.text(), "Final: gave up on the coder.");

        // The delegation is recorded as failed, with no artifacts.
        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].delegate, "coder");
        assert_eq!(output.delegations[0].status, DelegationStatus::Failed);
        assert!(
            output.artifacts.is_empty(),
            "a failed drive yields no artifacts"
        );

        // A failed delegation emits Failed, never Finished.
        assert!(
            output
                .events
                .iter()
                .any(|event| matches!(event, RunEvent::DelegationFailed(_))),
            "a failed external delegation emits DelegationFailed"
        );
        assert!(
            !output
                .events
                .iter()
                .any(|event| matches!(event, RunEvent::DelegationFinished(_))),
            "a failed external delegation does not emit DelegationFinished"
        );
    }

    #[tokio::test]
    async fn child_approval_gated_tool_still_triggers_approval() {
        let client = RoutingClient::new(vec![
            route(
                "SUPERVISOR",
                vec![
                    tool_call_response(
                        "del-9",
                        "ask_reviewer",
                        json!({ "task": "inspect the tree" }),
                    ),
                    text_response("Final: done."),
                ],
            ),
            route(
                "REVIEWER",
                vec![
                    tool_call_response("child-shell-1", "shell", json!({ "cmd": "ls" })),
                    text_response("I could not run shell; reporting from memory."),
                ],
            ),
        ]);

        // The child's approval handler records that it was consulted, then denies
        // so the gated tool never executes (§9.2).
        let consulted = Arc::new(AtomicBool::new(false));
        let flag = consulted.clone();
        let child_approval = ApprovalPolicy::new(Approval::ask(move |request| {
            if request.tool_name == "shell" {
                flag.store(true, Ordering::SeqCst);
            }
            ApprovalDecision::Deny
        }));

        let reviewer = Agent::worker()
            .system("You are the REVIEWER.")
            .tool_declarations(vec![shell_decl()])
            .approval(child_approval)
            .build()
            .expect("worker builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("reviewer", reviewer)
            .build()
            .expect("agent builds");

        let output = agent.run_full("Delegate an inspection.").await.unwrap();

        assert!(
            consulted.load(Ordering::SeqCst),
            "the child's approval-requiring tool still triggered approval"
        );
        assert_eq!(output.reply.text(), "Final: done.");
        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].status, DelegationStatus::Completed);
    }

    // -----------------------------------------------------------------------
    // Delegation config, multi-delegate, and snapshot coverage (milestone M3-3)
    // -----------------------------------------------------------------------

    use crate::facade::{Delegation, DelegationSnapshot};

    #[tokio::test]
    async fn two_subagents_each_expose_independent_tools_and_route() {
        // The supervisor calls each delegate's own `ask_<name>` tool in turn;
        // each routes to the matching child and folds that child's summary back.
        let client = RoutingClient::new(vec![
            route(
                "SUPERVISOR",
                vec![
                    tool_call_response("d1", "ask_reviewer", json!({ "task": "review" })),
                    tool_call_response("d2", "ask_researcher", json!({ "task": "research" })),
                    text_response("Final: both done."),
                ],
            ),
            route("REVIEWER", vec![text_response("review: LGTM")]),
            route("RESEARCHER", vec![text_response("research: found it")]),
        ]);

        let reviewer = Agent::worker()
            .description("Strict reviewer.")
            .system("You are the REVIEWER.")
            .build()
            .expect("worker builds");
        let researcher = Agent::worker()
            .description("Focused researcher.")
            .system("You are the RESEARCHER.")
            .build()
            .expect("worker builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("reviewer", reviewer)
            .subagent("researcher", researcher)
            .build()
            .expect("agent builds");

        // Both `ask_reviewer` and `ask_researcher` are advertised to the model.
        let advertised: Vec<&str> = agent
            .state()
            .spec()
            .initial_tools()
            .tools()
            .iter()
            .map(|decl| decl.name.as_str())
            .collect();
        assert!(advertised.contains(&"ask_reviewer"));
        assert!(advertised.contains(&"ask_researcher"));

        let output = agent.run_full("Do both.").await.unwrap();
        assert_eq!(output.reply.text(), "Final: both done.");

        // One trace per delegate, recorded in call order.
        assert_eq!(output.delegations.len(), 2);
        assert_eq!(output.delegations[0].delegate, "reviewer");
        assert_eq!(output.delegations[1].delegate, "researcher");
        assert!(
            output
                .delegations
                .iter()
                .all(|trace| trace.status == DelegationStatus::Completed)
        );

        // Each child's summary was folded back as its own tool result.
        let texts = tool_result_texts(&agent);
        assert!(texts.iter().any(|text| text == "review: LGTM"));
        assert!(texts.iter().any(|text| text == "research: found it"));
    }

    #[tokio::test]
    async fn single_tool_delegation_routes_by_agent_argument() {
        // One unified `delegate(agent, task)` tool routes to the delegate named
        // by the `agent` argument.
        let client = RoutingClient::new(vec![
            route(
                "SUPERVISOR",
                vec![
                    tool_call_response(
                        "d1",
                        "delegate",
                        json!({ "agent": "researcher", "task": "dig in" }),
                    ),
                    text_response("Final: routed."),
                ],
            ),
            route("REVIEWER", vec![text_response("review: unused")]),
            route(
                "RESEARCHER",
                vec![text_response("research: the answer is 42")],
            ),
        ]);

        let reviewer = Agent::worker()
            .system("You are the REVIEWER.")
            .build()
            .expect("worker builds");
        let researcher = Agent::worker()
            .system("You are the RESEARCHER.")
            .build()
            .expect("worker builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("reviewer", reviewer)
            .subagent("researcher", researcher)
            .delegation(Delegation::single_tool("delegate"))
            .build()
            .expect("agent builds");

        // Exactly one unified delegation tool is advertised (no `ask_*`).
        let delegation_tools: Vec<&str> = agent
            .state()
            .spec()
            .initial_tools()
            .tools()
            .iter()
            .map(|decl| decl.name.as_str())
            .filter(|name| *name == "delegate" || name.starts_with("ask_"))
            .collect();
        assert_eq!(delegation_tools, vec!["delegate"]);

        let output = agent.run_full("Route this.").await.unwrap();
        assert_eq!(output.reply.text(), "Final: routed.");

        // The call routed to the researcher, and only the researcher.
        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].delegate, "researcher");
        let texts = tool_result_texts(&agent);
        assert!(
            texts
                .iter()
                .any(|text| text == "research: the answer is 42")
        );
        assert!(texts.iter().all(|text| text != "review: unused"));
    }

    #[test]
    fn duplicate_delegate_name_is_rejected_at_build() {
        let first = Agent::worker()
            .system("You are the REVIEWER.")
            .build()
            .expect("worker builds");
        let second = Agent::worker()
            .system("You are another REVIEWER.")
            .build()
            .expect("worker builds");

        let error = AgentBuilder::default()
            .client(RoutingClient::new(vec![route(
                "SUPERVISOR",
                vec![text_response("unused")],
            )]))
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .subagent("reviewer", first)
            .subagent("reviewer", second)
            .build()
            .expect_err("two delegates under the same name collide");

        match error {
            crate::facade::FacadeError::DuplicateTool { name } => {
                assert_eq!(name, "ask_reviewer");
            }
            other => panic!("expected a DuplicateTool error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_carries_delegates_and_restore_can_delegate_again() {
        let client = RoutingClient::new(vec![
            route(
                "SUPERVISOR",
                vec![
                    tool_call_response("d1", "ask_reviewer", json!({ "task": "review the diff" })),
                    text_response("Final: first pass done."),
                ],
            ),
            route("REVIEWER", vec![text_response("review: LGTM")]),
        ]);

        let reviewer = Agent::worker()
            .description("Strict reviewer.")
            .system("You are the REVIEWER.")
            .build()
            .expect("worker builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("reviewer", reviewer)
            .build()
            .expect("agent builds");

        agent.run_full("Please review.").await.unwrap();

        // The snapshot carries the delegate as a data-only recipe and the
        // model-routed delegation mode.
        let snapshot = agent.snapshot().expect("snapshot at a committed point");
        assert_eq!(snapshot.delegates.len(), 1);
        assert_eq!(snapshot.delegates[0].name, "reviewer");
        assert_eq!(snapshot.delegates[0].description, "Strict reviewer.");
        assert!(snapshot.delegates[0].inherit_model);
        assert!(snapshot.pending_delegations.is_empty());
        assert_eq!(snapshot.delegation, Delegation::model_routed());

        // A restored agent re-advertises the delegate and can delegate again.
        let restore_client = RoutingClient::new(vec![
            route(
                "SUPERVISOR",
                vec![
                    tool_call_response("d2", "ask_reviewer", json!({ "task": "review again" })),
                    text_response("Final: second pass done."),
                ],
            ),
            route("REVIEWER", vec![text_response("review: still LGTM")]),
        ]);
        let restore_reviewer = Agent::worker()
            .system("You are the REVIEWER.")
            .approval(Approval::auto_allow())
            .build()
            .expect("worker builds");
        let mut restored = Agent::restore()
            .snapshot(snapshot)
            .client(restore_client)
            .approval(Approval::auto_allow())
            .subagent("reviewer", restore_reviewer)
            .build()
            .expect("restore agent");

        let output = restored.run_full("Review once more.").await.unwrap();
        assert_eq!(output.reply.text(), "Final: second pass done.");
        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].delegate, "reviewer");
        assert!(
            tool_result_texts(&restored)
                .iter()
                .any(|text| text == "review: still LGTM"),
            "the restored agent drove its re-registered delegate"
        );
    }

    #[tokio::test]
    async fn snapshot_does_not_persist_the_task_brief_in_delegation_data() {
        // A distinctive brief only the supervising model routes through the
        // delegation tool call.
        const BRIEF: &str = "SECRET_TASK_BRIEF_9f2a";
        let client = RoutingClient::new(vec![
            route(
                "SUPERVISOR",
                vec![
                    tool_call_response("d1", "ask_reviewer", json!({ "task": BRIEF })),
                    text_response("Final: done."),
                ],
            ),
            route("REVIEWER", vec![text_response("review: LGTM")]),
        ]);

        let reviewer = Agent::worker()
            .system("You are the REVIEWER.")
            .build()
            .expect("worker builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("reviewer", reviewer)
            .build()
            .expect("agent builds");

        agent
            .run_full("Delegate with a secret brief.")
            .await
            .unwrap();
        let snapshot = agent.snapshot().expect("snapshot");

        // The delegation-specific persistence (delegate recipes + in-flight
        // delegations) never carries the runtime task brief (R5): delegates hold
        // only static spec, and no child is left in flight.
        let delegates_json =
            serde_json::to_string(&snapshot.delegates).expect("serialize delegates");
        assert!(
            !delegates_json.contains(BRIEF),
            "delegate recipes must not carry the runtime task brief"
        );
        let pending_json =
            serde_json::to_string(&snapshot.pending_delegations).expect("serialize pending");
        assert!(
            !pending_json.contains(BRIEF),
            "pending-delegation persistence must not carry the runtime task brief"
        );
        assert!(snapshot.pending_delegations.is_empty());
    }

    #[tokio::test]
    async fn delegation_snapshot_round_trips_and_rebuilds_child_conversation() {
        // Drive a standalone child agent to produce a committed child
        // conversation, then round-trip it through a `DelegationSnapshot` and
        // rebuild the child's live conversation from it (§15.2).
        let child_client = RoutingClient::new(vec![route(
            "REVIEWER",
            vec![text_response("review: child ran")],
        )]);
        let mut child = AgentBuilder::default()
            .client(child_client)
            .model("child-model")
            .system("You are the REVIEWER.")
            .approval(Approval::auto_allow())
            .build()
            .expect("child agent builds");
        child.run_full("child task").await.unwrap();
        let turns_before = child.conversation().turns().len();
        assert!(turns_before > 0);

        let pending =
            DelegationSnapshot::capture("reviewer", child.conversation()).expect("capture pending");
        assert_eq!(pending.delegate, "reviewer");

        // Serde round-trip preserves the pending delegation exactly.
        let json = serde_json::to_string(&pending).expect("serialize pending");
        let restored: DelegationSnapshot =
            serde_json::from_str(&json).expect("deserialize pending");
        assert_eq!(restored, pending);

        // Restore rebuilds the child's live conversation with its committed turns.
        let rebuilt = restored
            .restore_conversation()
            .expect("rebuild child conversation");
        assert_eq!(rebuilt.turns().len(), turns_before);
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

    #[tokio::test]
    async fn external_delegation_denied_by_auto_deny_surfaces_approval_denied() {
        let mut agent = AgentBuilder::default()
            .client(external_supervisor_client())
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(ApprovalPolicy::default().tool("ask_coder", Approval::auto_deny()))
            .external_agent("coder", completed_coder())
            .build()
            .expect("agent builds");

        let error = agent
            .run_full("Please refactor.")
            .await
            .expect_err("an auto-denied external delegate fails the run");
        assert!(
            matches!(error, crate::facade::FacadeError::ApprovalDenied),
            "auto_deny on the external start tool surfaces ApprovalDenied, got {error:?}"
        );

        // The denied external agent never drove a session, so no summary was
        // folded back as a tool result.
        assert!(
            !tool_result_texts(&agent)
                .iter()
                .any(|text| text == "refactor complete"),
            "a denied external delegate is not driven"
        );
    }

    #[tokio::test]
    async fn external_delegation_denied_headless_when_ask_external_agents_has_no_handler() {
        // `ask_external_agents` with an auto-allow default and no `ask` handler is
        // a headless run: the external start is denied rather than left blocking.
        let mut agent = AgentBuilder::default()
            .client(external_supervisor_client())
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(ApprovalPolicy::from(Approval::auto_allow()).ask_external_agents())
            .external_agent("coder", completed_coder())
            .build()
            .expect("agent builds");

        let error = agent
            .run_full("Please refactor.")
            .await
            .expect_err("a headless ask_external_agents run denies the external start");
        assert!(
            matches!(error, crate::facade::FacadeError::ApprovalDenied),
            "headless ask_external_agents surfaces ApprovalDenied, got {error:?}"
        );
    }

    #[tokio::test]
    async fn external_delegation_approved_by_ask_handler_runs_to_completion() {
        let approved = Arc::new(AtomicBool::new(false));
        let approved_probe = approved.clone();
        let policy = ApprovalPolicy::default().tool(
            "ask_coder",
            Approval::ask(move |request| {
                if request.tool_name == "ask_coder" {
                    approved_probe.store(true, Ordering::SeqCst);
                    ApprovalDecision::Approve
                } else {
                    ApprovalDecision::Deny
                }
            }),
        );

        let mut agent = AgentBuilder::default()
            .client(external_supervisor_client())
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(policy)
            .external_agent("coder", completed_coder())
            .build()
            .expect("agent builds");

        let output = agent.run_full("Please refactor.").await.unwrap();

        assert!(
            approved.load(Ordering::SeqCst),
            "the external start consulted the ask handler"
        );
        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].delegate, "coder");
        assert_eq!(output.delegations[0].status, DelegationStatus::Completed);
        assert!(
            tool_result_texts(&agent)
                .iter()
                .any(|text| text == "refactor complete"),
            "an approved external delegate is driven and folds its summary back"
        );
    }

    #[tokio::test]
    async fn driven_external_snapshot_is_data_only_with_session_facts() {
        use crate::agent::external::ExternalRuntimeKind;
        use crate::facade::ExternalDelegateStatus;

        let mut agent = AgentBuilder::default()
            .client(external_supervisor_client())
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("coder", completed_coder())
            .build()
            .expect("agent builds");

        agent.run_full("Please refactor.").await.unwrap();

        let snapshot = agent.snapshot().expect("snapshot at a committed point");
        assert_eq!(snapshot.external_delegates.len(), 1);
        let delegate = &snapshot.external_delegates[0];
        assert_eq!(delegate.name, "coder");
        assert_eq!(delegate.runtime, ExternalRuntimeKind::ClaudeCode);
        assert_eq!(delegate.status, ExternalDelegateStatus::Completed);

        // The resumable session facts are captured as data (session id + resume
        // token), and the reported artifact surfaces on the snapshot.
        let session = delegate.session.as_ref().expect("a captured session ref");
        assert_eq!(session.session_id.as_deref(), Some("sess-1"));
        assert_eq!(session.resume_token.as_deref(), Some("resume-1"));
        assert_eq!(delegate.artifacts.len(), 1);
        assert_eq!(delegate.artifacts[0].path, "src/parser.rs");

        // The snapshot serializes to data only — no runtime handle or closure
        // leaks into the persisted form — and round-trips exactly.
        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        assert!(
            !json.contains("session_handler") && !json.contains("handler"),
            "no runtime session handler leaks into the snapshot"
        );
        let restored: crate::facade::AgentSnapshot =
            serde_json::from_str(&json).expect("deserialize snapshot");
        assert_eq!(restored, snapshot);
    }

    #[tokio::test]
    async fn restore_external_mark_interrupted_marks_the_delegate_interrupted() {
        use crate::facade::ExternalDelegateStatus;

        let mut agent = AgentBuilder::default()
            .client(external_supervisor_client())
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("coder", completed_coder())
            .build()
            .expect("agent builds");
        agent.run_full("Please refactor.").await.unwrap();
        let snapshot = agent.snapshot().expect("snapshot at a committed point");

        // The default restore policy marks the delegate interrupted without
        // touching any external runtime.
        let restored = Agent::restore()
            .snapshot(snapshot)
            .client(external_supervisor_client())
            .build()
            .expect("restore rebuilds the agent");

        // The restored agent re-advertises the external delegate, and a
        // re-snapshot reports its reconciled interrupted status with the recorded
        // session preserved.
        assert_eq!(restored.external_agents().len(), 1);
        assert_eq!(restored.external_agents()[0].name(), "coder");
        let resnapshot = restored.snapshot().expect("re-snapshot the restored agent");
        assert_eq!(resnapshot.external_delegates.len(), 1);
        assert_eq!(
            resnapshot.external_delegates[0].status,
            ExternalDelegateStatus::Interrupted
        );
        assert_eq!(
            resnapshot.external_delegates[0]
                .session
                .as_ref()
                .and_then(|session| session.session_id.as_deref()),
            Some("sess-1"),
            "MarkInterrupted preserves the recorded session facts"
        );
    }

    #[tokio::test]
    async fn restore_external_attach_or_fail_errors_when_unattachable() {
        use crate::facade::RestoreExternal;

        let mut agent = AgentBuilder::default()
            .client(external_supervisor_client())
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("coder", completed_coder())
            .build()
            .expect("agent builds");
        agent.run_full("Please refactor.").await.unwrap();
        let snapshot = agent.snapshot().expect("snapshot at a committed point");

        // AttachOrFail with no re-registered runtime (hence no session handler to
        // attach with) is an explicit, non-silent failure.
        let error = Agent::restore()
            .snapshot(snapshot)
            .client(external_supervisor_client())
            .restore_external(RestoreExternal::AttachOrFail)
            .build()
            .expect_err("attach_or_fail without a re-registered runtime fails");
        assert!(
            matches!(error, crate::facade::FacadeError::InvalidState(_)),
            "an unattachable AttachOrFail restore fails explicitly, got {error:?}"
        );
    }
}
