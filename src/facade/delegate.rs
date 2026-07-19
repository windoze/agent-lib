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
    AgentError, AgentInput, AgentMachine, AgentSpec, AgentSpecRef, AgentState, ApprovalDecision,
    ApprovalRequirement, ApprovalResponse, CancellationToken, DefaultAgentMachine,
    DrivingSubagentHandler, HandlerScope, Interaction, InteractionHandler, InteractionKind,
    InteractionOrigin, InteractionResponse, LlmClientHandler, LlmHandler, LoopCursor, LoopPolicy,
    ModelRef, PermissionResponse, RequirementResult, RunContext, RunId, ScopePop, SpawnedChild,
    StepInput, StepOutcome, SubagentHandler, SubagentOutput, SubagentSpawner, TaskEvaluator,
    ToolFailurePolicy, ToolHandler, ToolRegistry, ToolRegistryHandler, ToolRuntimeError,
    ToolSetRef, TraceHandle, TraceNodeId, TurnDone, Verifier, WorktreeRef,
};
use crate::client::LlmClient;
use crate::conversation::{Conversation, ConversationConfig, ToolCallId};
use crate::facade::agent::{
    DEFAULT_MAX_STEPS, DEFAULT_MAX_TOOL_ROUNDS, assemble_machine, build_loop_policy,
    final_turn_summary,
};
use crate::facade::approval::{ApprovalPolicy, FacadeApproval};
use crate::facade::collab::CollabBridge;
use crate::facade::config::ModelConfig;
use crate::facade::error::FacadeError;
use crate::facade::external::{ManagedExternalDelegate, drive_external};
use crate::facade::ids::FacadeIds;
use crate::facade::run::{ArtifactRef, DelegationStatus, DelegationTrace};
use crate::facade::tool::{FacadeToolRegistry, ToolContextParts};
use crate::model::content::ContentBlock;
use crate::model::extras::ProviderExtras;
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
    provider_extras: Option<ProviderExtras>,
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
        self.provider_extras = None;
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

    /// Sets provider-specific request fields for an explicitly pinned worker model.
    ///
    /// Inheriting workers use the supervisor's model configuration wholesale,
    /// including its provider extras. Calling this without an explicit
    /// [`model`](Self::model) is rejected at build time instead of being ignored.
    #[must_use]
    pub fn provider_extras(mut self, provider_extras: ProviderExtras) -> Self {
        self.provider_extras = Some(provider_extras);
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
            if self.provider_extras.is_some() {
                return Err(FacadeError::Config(
                    "worker provider_extras require an explicit `model`; inherited workers use the supervisor model extras"
                        .to_owned(),
                ));
            }
            ModelRef::new(
                INHERITED_MODEL_PLACEHOLDER,
                nonzero_default_tokens(),
                None,
                None,
            )
        } else {
            let model_name = crate::facade::config::ensure_non_blank_model(
                "worker",
                self.model
                    .clone()
                    .expect("explicit model present when not inheriting"),
            )?;
            let mut model = ModelConfig::new(model_name);
            if let Some(max_tokens) = self.max_tokens {
                model = model.max_tokens(max_tokens);
            }
            if let Some(temperature) = self.temperature {
                model = model.temperature(temperature)?;
            }
            if let Some(provider_extras) = self.provider_extras {
                model = model.provider_extras(provider_extras);
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
/// (`docs/facade-api.md` §10.2, §13.1, §13.2).
///
/// Three shapes are supported:
///
/// - [`model_routed`](Self::model_routed) (the default): every registered
///   subagent is advertised to the supervising model as its own
///   `ask_<name>(task)` tool. Separate tools make it easy for the model to call
///   the right delegate and keep each delegation's trace distinct.
/// - [`single_tool`](Self::single_tool): all delegates are collapsed behind one
///   unified `<name>(agent, task)` tool that routes to the requested delegate by
///   its `agent` argument. This suits a dynamic delegate roster or an outer
///   policy that wants to own routing.
/// - [`rules`](Self::rules): the facade (not the model) routes each *task* to a
///   delegate by matching keywords in the task text
///   ([`when_task_contains`](Self::when_task_contains)). No delegate is exposed
///   to the model as a tool, so the model need not know the delegates exist —
///   the fit when a product must not let the model start an expensive worker on
///   its own (§13.2).
///
/// ```
/// use agent_lib::facade::Delegation;
///
/// // Default: one tool per subagent.
/// let per_subagent = Delegation::model_routed().expose_subagents_as_tools();
///
/// // Advanced: a single unified delegation tool.
/// let unified = Delegation::single_tool("delegate");
///
/// // Rules-routed: the facade routes by keywords, the model stays unaware.
/// let ruled = Delegation::rules()
///     .when_task_contains(["fix", "test", "compile"], "coder")
///     .when_task_contains(["review", "audit"], "reviewer");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    mode: DelegationMode,
    /// Optional host-injected decision hooks for a dispatcher-routed delegation
    /// (the AI-routing / AI-verification seam of `docs/facade-api.md` §19).
    ///
    /// These are runtime handlers, not serializable configuration, so — like the
    /// approval `ask` handlers and external session handlers — they are dropped
    /// on [`snapshot`](crate::facade::Agent::snapshot) (§15.2) and default to
    /// absent, in which case the dispatcher behaves exactly as Milestone 5.
    #[serde(skip)]
    dispatcher_hooks: DispatcherHooks,
}

/// A boxed [`TaskEvaluator`] a host injects to route a dispatcher escalation.
pub(crate) type SharedTaskEvaluator = Arc<dyn TaskEvaluator + Send + Sync>;

/// A boxed [`Verifier`] a host injects to judge a dispatcher worker's output.
pub(crate) type SharedVerifier = Arc<dyn Verifier + Send + Sync>;

/// Host-injected decision hooks for a dispatcher-routed delegation.
///
/// Both hooks are optional and default to absent; when absent the facade
/// dispatcher uses its built-in Milestone 5 defaults (a clean worker run is
/// accepted, and escalation is resolved by `agent::external::Escalator` against
/// the configured roster). They are the formal seam for AI-based routing
/// ([`TaskEvaluator`]) and AI-based verification ([`Verifier`]); the facade never
/// implements the AI itself (`docs/facade-api.md` §19).
#[derive(Clone, Default)]
pub(crate) struct DispatcherHooks {
    /// Chooses the escalation target after a worker is rejected; `None` uses the
    /// built-in [`Escalator`](crate::agent::Escalator) roster logic.
    evaluator: Option<SharedTaskEvaluator>,
    /// Judges whether a worker's output is rejected (and escalation warranted);
    /// `None` uses the built-in verdict (worker failure plus the verifier
    /// delegate's `ESCALATE` token, if any).
    verifier: Option<SharedVerifier>,
}

impl std::fmt::Debug for DispatcherHooks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DispatcherHooks")
            .field("has_evaluator", &self.evaluator.is_some())
            .field("has_verifier", &self.verifier.is_some())
            .finish()
    }
}

impl PartialEq for DispatcherHooks {
    /// Two delegations are equal when their serializable routing *configuration*
    /// matches; the injected runtime hooks — like any other runtime handler — do
    /// not participate in configuration identity, so they always compare equal.
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for DispatcherHooks {}

/// One rules-routed routing rule: any of `keywords` present in a task routes it
/// to the delegate registered under `delegate` (`docs/facade-api.md` §13.2).
///
/// Matching is case-insensitive substring containment on the task text, and the
/// first rule (in registration order) whose keywords hit wins, so rule order is
/// the routing priority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingRule {
    /// The keywords that, if any appears in the task text, select this rule.
    keywords: Vec<String>,
    /// The registration name of the delegate this rule routes to.
    delegate: String,
}

impl RoutingRule {
    /// Returns the delegate name this rule routes to.
    #[must_use]
    pub fn delegate(&self) -> &str {
        &self.delegate
    }

    /// Returns the keywords that select this rule.
    #[must_use]
    pub fn keywords(&self) -> &[String] {
        &self.keywords
    }

    /// Reports whether `haystack` (already lowercased) contains any keyword.
    fn matches(&self, haystack: &str) -> bool {
        self.keywords
            .iter()
            .any(|keyword| haystack.contains(&keyword.to_lowercase()))
    }
}

/// The default per-task attempt cap for a dispatcher-routed delegation: the
/// primary worker plus one escalation (`docs/facade-api.md` §13.3).
pub(crate) const DEFAULT_DISPATCHER_MAX_ATTEMPTS: u32 = 2;

/// The case-insensitive token a verifier delegate emits to reject a worker's
/// output and force an escalation (`docs/facade-api.md` §13.3).
///
/// A verifier whose reply contains this token (or whose delegation fails) is
/// treated as *not passing*; any other reply is a pass. Held lowercase so the
/// verdict check is a single case-insensitive `contains`.
pub(crate) const DISPATCHER_ESCALATE_MARKER: &str = "escalate";

/// Dispatcher-routed configuration: a fixed cheap→verify→strong escalation loop
/// the facade drives itself, mapping onto `agent::external::{Dispatcher,
/// Escalator}` semantics (`docs/facade-api.md` §13.3).
///
/// The `primary` delegate runs first; when a `verifier` is configured its reply
/// is checked (the verifier requests escalation by emitting the case-insensitive
/// token `ESCALATE`, or by failing its own delegation), and a rejected — or a
/// failed — primary run escalates to `escalate_to`. The whole loop is capped at
/// `max_attempts` worker runs. Every delegate is named by its registration
/// string and none is advertised to the supervising model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatcherConfig {
    /// The registration name of the cheap worker tried first.
    primary: String,
    /// The registration name of the verifier delegate, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verifier: Option<String>,
    /// The registration name of the stronger worker escalation hands off to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    escalate_to: Option<String>,
    /// The maximum number of worker runs before the loop gives up.
    max_attempts: u32,
}

impl DispatcherConfig {
    /// An empty dispatcher config: no primary yet, no verifier / escalation, and
    /// the default attempt cap. Refined through the [`Delegation`] builder.
    fn empty() -> Self {
        Self {
            primary: String::new(),
            verifier: None,
            escalate_to: None,
            max_attempts: DEFAULT_DISPATCHER_MAX_ATTEMPTS,
        }
    }

    /// Returns the primary (cheap) worker's registration name.
    #[must_use]
    pub fn primary(&self) -> &str {
        &self.primary
    }

    /// Returns the verifier delegate's registration name, if configured.
    #[must_use]
    pub fn verifier(&self) -> Option<&str> {
        self.verifier.as_deref()
    }

    /// Returns the escalation target's registration name, if configured.
    #[must_use]
    pub fn escalate_to(&self) -> Option<&str> {
        self.escalate_to.as_deref()
    }

    /// Returns the maximum number of worker runs allowed for one task.
    #[must_use]
    pub const fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// Iterates every delegate name this config references (primary, then
    /// verifier, then escalation target), skipping any that are unset.
    fn referenced_delegates(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.primary.as_str())
            .chain(self.verifier.as_deref())
            .chain(self.escalate_to.as_deref())
    }
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
    /// Facade-owned routing: the task is matched against `rules` and routed to a
    /// delegate without exposing any delegate to the model (§13.2).
    Rules {
        /// The ordered routing rules; the first whose keywords hit wins.
        rules: Vec<RoutingRule>,
    },
    /// Facade-owned dispatcher: a fixed cheap→verify→strong escalation loop the
    /// facade drives itself, exposing no delegate to the model (§13.3).
    Dispatcher {
        /// The primary / verifier / escalation names and attempt cap.
        config: DispatcherConfig,
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
    /// Builds a delegation in `mode` with no injected dispatcher hooks.
    fn from_mode(mode: DelegationMode) -> Self {
        Self {
            mode,
            dispatcher_hooks: DispatcherHooks::default(),
        }
    }

    /// Model-routed delegation: expose each subagent as its own `ask_<name>`
    /// tool (the default, §13.1).
    #[must_use]
    pub fn model_routed() -> Self {
        Self::from_mode(DelegationMode::PerSubagentTool)
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
        Self::from_mode(DelegationMode::SingleTool {
            tool_name: tool_name.into(),
        })
    }

    /// Rules-routed delegation: the facade routes each task to a delegate by
    /// matching keywords, without exposing any delegate to the model (§13.2).
    ///
    /// Start from an empty rule set and add rules with
    /// [`when_task_contains`](Self::when_task_contains). Because no delegate is
    /// advertised as a tool, the supervising model need not know the delegates
    /// exist; a task that matches no rule is answered by the supervisor itself.
    ///
    /// ```
    /// use agent_lib::facade::Delegation;
    ///
    /// let routing = Delegation::rules()
    ///     .when_task_contains(["fix", "test", "compile"], "coder")
    ///     .when_task_contains(["review", "audit"], "reviewer");
    /// ```
    #[must_use]
    pub fn rules() -> Self {
        Self::from_mode(DelegationMode::Rules { rules: Vec::new() })
    }

    /// Appends a rules-routed rule: if the task text contains **any** of
    /// `keywords`, route the whole task to the delegate registered under
    /// `delegate` (§13.2).
    ///
    /// Matching is case-insensitive substring containment, and rules are tried
    /// in the order they were added — the first rule whose keywords hit wins, so
    /// registration order is the routing priority. This method is meant to chain
    /// after [`rules`](Self::rules); calling it on a non-rules [`Delegation`]
    /// switches the mode to rules-routed, starting from this single rule.
    #[must_use]
    pub fn when_task_contains<I, S>(mut self, keywords: I, delegate: impl Into<String>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let rule = RoutingRule {
            keywords: keywords.into_iter().map(Into::into).collect(),
            delegate: delegate.into(),
        };
        match &mut self.mode {
            DelegationMode::Rules { rules } => rules.push(rule),
            _ => self.mode = DelegationMode::Rules { rules: vec![rule] },
        }
        self
    }

    /// Reports whether this delegation routes by facade rules rather than by the
    /// model (§13.2).
    #[must_use]
    pub(crate) fn is_rules_routed(&self) -> bool {
        matches!(self.mode, DelegationMode::Rules { .. })
    }

    /// Resolves the rules-routed delegate name for `task`, if any rule matches.
    ///
    /// Returns the delegate name of the first rule (in registration order) whose
    /// keywords appear in `task`, or `None` when no rule matches or this
    /// delegation is not rules-routed (§13.2).
    #[must_use]
    pub(crate) fn route_task(&self, task: &str) -> Option<&str> {
        let DelegationMode::Rules { rules } = &self.mode else {
            return None;
        };
        let haystack = task.to_lowercase();
        rules
            .iter()
            .find(|rule| rule.matches(&haystack))
            .map(|rule| rule.delegate.as_str())
    }

    /// Returns the first rule delegate name that is not registered among
    /// `subagents` or `external`, for build-time validation (§13.2).
    ///
    /// A rules-routed delegation that names a delegate no agent registered can
    /// never route correctly, so [`AgentBuilder::build`](crate::facade::AgentBuilder::build)
    /// rejects it up front rather than failing silently at run time.
    #[must_use]
    pub(crate) fn first_unknown_rule_delegate(
        &self,
        subagents: &[LocalSubagent],
        external: &[ManagedExternalDelegate],
    ) -> Option<String> {
        let DelegationMode::Rules { rules } = &self.mode else {
            return None;
        };
        rules
            .iter()
            .find(|rule| {
                !subagents.iter().any(|s| s.name() == rule.delegate)
                    && !external.iter().any(|e| e.name() == rule.delegate)
            })
            .map(|rule| rule.delegate.clone())
    }

    /// Validates delegation configuration that cannot be checked by the builder
    /// methods themselves because they are infallible chaining APIs.
    pub(crate) fn validate_configuration(&self) -> Result<(), FacadeError> {
        match &self.mode {
            DelegationMode::SingleTool { tool_name } if tool_name.trim().is_empty() => {
                Err(FacadeError::Config(
                    "single-tool delegation requires a non-empty tool name".to_owned(),
                ))
            }
            DelegationMode::Rules { rules } => {
                if rules.is_empty() {
                    // A rules-routed delegation with no rules routes nothing
                    // and exposes no delegate tools, so registered subagents
                    // would be silently unreachable.
                    return Err(FacadeError::Config(
                        "rules-routed delegation requires at least one rule".to_owned(),
                    ));
                }
                for (index, rule) in rules.iter().enumerate() {
                    if rule.delegate.trim().is_empty() {
                        return Err(FacadeError::Config(format!(
                            "rules-routed delegation rule {index} has a blank delegate name"
                        )));
                    }
                    if rule.keywords.is_empty() {
                        return Err(FacadeError::Config(format!(
                            "rules-routed delegation rule {index} has no keywords"
                        )));
                    }
                    if rule
                        .keywords
                        .iter()
                        .any(|keyword| keyword.trim().is_empty())
                    {
                        return Err(FacadeError::Config(format!(
                            "rules-routed delegation rule {index} has a blank keyword"
                        )));
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Dispatcher-routed delegation: a fixed cheap→verify→strong escalation loop
    /// the facade drives itself, exposing no delegate to the model (§13.3).
    ///
    /// Refine it with [`primary`](Self::primary), [`verify_with`](Self::verify_with),
    /// [`escalate_to`](Self::escalate_to), and [`max_attempts`](Self::max_attempts).
    /// The named delegates may be local subagents or managed external agents, and
    /// map onto `agent::external::{Dispatcher, Escalator}` scheduling rather than
    /// any new orchestration runtime (§19). Dispatcher-routed is an advanced,
    /// opt-in mode — never a default (§13.3).
    ///
    /// ```
    /// use agent_lib::facade::Delegation;
    ///
    /// let routing = Delegation::dispatcher()
    ///     .primary("cheap-coder")
    ///     .verify_with("verifier")
    ///     .escalate_to("strong-coder")
    ///     .max_attempts(2);
    /// ```
    #[must_use]
    pub fn dispatcher() -> Self {
        Self::from_mode(DelegationMode::Dispatcher {
            config: DispatcherConfig::empty(),
        })
    }

    /// Returns a mutable reference to the dispatcher config, switching the
    /// delegation into dispatcher-routed mode (from a fresh, empty config) if it
    /// is not already, so the `dispatcher()` builder methods chain from any base.
    fn dispatcher_config_mut(&mut self) -> &mut DispatcherConfig {
        if !matches!(self.mode, DelegationMode::Dispatcher { .. }) {
            self.mode = DelegationMode::Dispatcher {
                config: DispatcherConfig::empty(),
            };
        }
        let DelegationMode::Dispatcher { config } = &mut self.mode else {
            unreachable!("mode was just set to Dispatcher");
        };
        config
    }

    /// Sets the primary (cheap) worker tried first, switching to
    /// dispatcher-routed mode if needed (§13.3).
    #[must_use]
    pub fn primary(mut self, delegate: impl Into<String>) -> Self {
        self.dispatcher_config_mut().primary = delegate.into();
        self
    }

    /// Sets the verifier delegate consulted after each worker run (§13.3).
    ///
    /// The verifier requests an escalation by emitting the case-insensitive token
    /// `ESCALATE` in its reply (or by failing its own delegation); any other
    /// reply is treated as a pass. Switches to dispatcher-routed mode if needed.
    #[must_use]
    pub fn verify_with(mut self, delegate: impl Into<String>) -> Self {
        self.dispatcher_config_mut().verifier = Some(delegate.into());
        self
    }

    /// Sets the stronger worker an escalation hands off to, switching to
    /// dispatcher-routed mode if needed (§13.3).
    #[must_use]
    pub fn escalate_to(mut self, delegate: impl Into<String>) -> Self {
        self.dispatcher_config_mut().escalate_to = Some(delegate.into());
        self
    }

    /// Sets the maximum number of worker runs before the loop gives up,
    /// switching to dispatcher-routed mode if needed (§13.3).
    ///
    /// Clamped to at least `1` so the primary worker always runs at least once.
    #[must_use]
    pub fn max_attempts(mut self, max_attempts: u32) -> Self {
        self.dispatcher_config_mut().max_attempts = max_attempts.max(1);
        self
    }

    /// Injects a custom [`TaskEvaluator`] that
    /// chooses which worker a rejected task escalates to, switching to
    /// dispatcher-routed mode if needed.
    ///
    /// This is the formal seam for **AI-based routing** (`docs/facade-api.md`
    /// §19): the facade itself implements no model logic, it only forwards the
    /// decision to the injected evaluator. On each escalation the evaluator is
    /// consulted with the task descriptor and the worker roster (primary plus the
    /// configured escalation target); it returns the
    /// [`WorkerProfileRef`](crate::agent::WorkerProfileRef) to run next, or `None`
    /// to decline (which stops the loop). A returned worker that names an
    /// unregistered delegate — or the worker that just ran — is treated as a
    /// decline. When **no** evaluator is injected the escalation target is
    /// resolved by the built-in
    /// [`Escalator`](crate::agent::Escalator), exactly as Milestone 5.
    ///
    /// The evaluator is a runtime handler, so it is dropped when the agent is
    /// snapshotted (§15.2); a restored agent falls back to the built-in default.
    #[must_use]
    pub fn dispatcher_evaluator(mut self, evaluator: SharedTaskEvaluator) -> Self {
        let _ = self.dispatcher_config_mut();
        self.dispatcher_hooks.evaluator = Some(evaluator);
        self
    }

    /// Injects a custom [`Verifier`] that judges whether a
    /// worker's output is rejected (and an escalation warranted), switching to
    /// dispatcher-routed mode if needed.
    ///
    /// This is the formal seam for **AI-based verification** (`docs/facade-api.md`
    /// §19), replacing the built-in inert `ScriptedVerifier::passing()` the
    /// facade wires by default. After each worker run the verifier is consulted
    /// with the task descriptor and a
    /// [`WorkerReport`](crate::agent::WorkerReport) for the worker; a
    /// [`Some`] verdict rejects the output and forces an escalation. It composes
    /// with (does not replace) the verifier delegate configured through
    /// [`verify_with`](Self::verify_with) and a worker's own failure: the output
    /// is rejected if **any** of them rejects. When **no** verifier is injected
    /// the verdict is exactly Milestone 5 (worker failure plus the delegate's
    /// `ESCALATE` token, if a verifier delegate is configured).
    ///
    /// The verifier is a runtime handler, so it is dropped when the agent is
    /// snapshotted (§15.2); a restored agent falls back to the built-in default.
    #[must_use]
    pub fn dispatcher_verifier(mut self, verifier: SharedVerifier) -> Self {
        let _ = self.dispatcher_config_mut();
        self.dispatcher_hooks.verifier = Some(verifier);
        self
    }

    /// Returns the injected escalation [`TaskEvaluator`],
    /// if any (§19).
    #[must_use]
    pub(crate) fn dispatcher_evaluator_hook(&self) -> Option<&SharedTaskEvaluator> {
        self.dispatcher_hooks.evaluator.as_ref()
    }

    /// Returns the injected verification [`Verifier`], if
    /// any (§19).
    #[must_use]
    pub(crate) fn dispatcher_verifier_hook(&self) -> Option<&SharedVerifier> {
        self.dispatcher_hooks.verifier.as_ref()
    }

    /// Reports whether this delegation routes through the facade dispatcher
    /// rather than by the model or by keyword rules (§13.3).
    #[must_use]
    pub(crate) fn is_dispatcher_routed(&self) -> bool {
        matches!(self.mode, DelegationMode::Dispatcher { .. })
    }

    /// Returns the dispatcher config when this delegation is dispatcher-routed
    /// (§13.3), else `None`.
    #[must_use]
    pub(crate) fn dispatcher_config(&self) -> Option<&DispatcherConfig> {
        match &self.mode {
            DelegationMode::Dispatcher { config } => Some(config),
            _ => None,
        }
    }

    /// Returns the first dispatcher delegate name that is not registered among
    /// `subagents` or `external`, for build-time validation (§13.3).
    ///
    /// A dispatcher-routed delegation naming a delegate no agent registered can
    /// never run it, so [`AgentBuilder::build`](crate::facade::AgentBuilder::build)
    /// rejects it up front rather than failing silently at run time. An empty
    /// `primary` is reported separately by the builder.
    #[must_use]
    pub(crate) fn first_unknown_dispatcher_delegate(
        &self,
        subagents: &[LocalSubagent],
        external: &[ManagedExternalDelegate],
    ) -> Option<String> {
        let DelegationMode::Dispatcher { config } = &self.mode else {
            return None;
        };
        config
            .referenced_delegates()
            .filter(|name| !name.is_empty())
            .find(|name| {
                !subagents.iter().any(|s| s.name() == *name)
                    && !external.iter().any(|e| e.name() == *name)
            })
            .map(str::to_owned)
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
            // Rules-routed delegation never advertises a delegate to the model:
            // the facade routes the task itself, so no tool is synthesized (§13.2).
            // Dispatcher-routed delegation is the same: the facade drives the
            // cheap→verify→strong loop itself, exposing nothing (§13.3).
            DelegationMode::Rules { .. } | DelegationMode::Dispatcher { .. } => Vec::new(),
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
            // Rules-routed delegation exposes no delegation tool to the machine,
            // so the run-scoped handler recognizes nothing and forwards every
            // call to the base registry; the facade drives the routed delegate
            // directly instead (§13.2). Dispatcher-routed is identical: the
            // facade owns the cheap→verify→strong loop (§13.3).
            DelegationMode::Rules { .. } | DelegationMode::Dispatcher { .. } => {
                DelegationRoute::PerSubagent {
                    local: HashMap::new(),
                    external: HashMap::new(),
                }
            }
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
            // Rules-routed delegation never advertises an external start tool to
            // the machine (the facade drives it directly), so nothing to exempt.
            // Dispatcher-routed is identical (§13.3).
            DelegationMode::Rules { .. } | DelegationMode::Dispatcher { .. } => Vec::new(),
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
            let mut slot = self
                .slot
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *slot = Some(ChildSummary { text, usage });
        }
        outcome
    }

    fn cursor(&self) -> &LoopCursor {
        self.inner.cursor()
    }
}

/// The child's own drain layer: the shared LLM client, a declaration-only tool
/// registry, and the child's interaction answer path.
///
/// A subagent stays data-first (declaration-only tools), so the tool handler
/// only serves declared names; an approval-requiring child tool still pauses on
/// the child's [`FacadeApproval`] gate before any execution (§9.2). When the
/// supervisor supplied an async interaction handler, this scope routes the
/// paused answer there; otherwise it keeps the child's synchronous approval
/// fallback.
struct ChildAgentScope {
    llm: LlmClientHandler,
    tool: ToolRegistryHandler,
    interaction: Arc<dyn InteractionHandler>,
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

/// Routes a child interaction to the supervisor's injected handler.
///
/// The child machine still uses its own [`FacadeApproval`] as the tool gate; this
/// handler only decides where the already-paused interaction is answered. It
/// annotates the forwarded request with display-only delegate attribution so the
/// parent UI can render which worker asked.
struct ChildInteractionRouter {
    delegate: String,
    parent: Arc<dyn InteractionHandler>,
}

#[async_trait]
impl InteractionHandler for ChildInteractionRouter {
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult {
        let routed = request
            .clone()
            .with_origin(InteractionOrigin::new(self.delegate.clone(), ctx.depth()));
        tokio::select! {
            biased;
            _ = ctx.cancellation().cancelled() => cancelled_interaction_result(&routed),
            result = self.parent.fulfill(&routed, ctx) => result,
        }
    }
}

/// Builds an in-family interaction result for a child interaction abandoned by
/// cancellation before the parent handler answered.
fn cancelled_interaction_result(request: &Interaction) -> RequirementResult {
    let response = match request.kind() {
        InteractionKind::Approval { call_id, .. } => {
            InteractionResponse::Approval(ApprovalResponse::new(
                request.step_id(),
                *call_id,
                ApprovalDecision::Deny,
                Some("interaction cancelled".to_owned()),
            ))
        }
        InteractionKind::Question { .. } => InteractionResponse::answer(String::new()),
        InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
        InteractionKind::Permission { request } => InteractionResponse::Permission(
            PermissionResponse::cancel(request.action_id().to_owned()),
        ),
    };
    RequirementResult::Interaction(response)
}

/// An empty outer layer for the child drive.
///
/// The facade installs the child interaction answer path directly in
/// [`ChildAgentScope`], so no local child requirement should pop here; an
/// unexpected pop surfaces as an
/// [`AgentError::UnhandledRequirement`](crate::agent::AgentError), the correct
/// failure for a child asking for a capability this local path does not wire
/// (for example nested delegation).
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
    parent_interaction: Option<Arc<dyn InteractionHandler>>,
    ids: FacadeIds,
    task: String,
    cancel: CancellationToken,
    trace: TraceHandle,
    slot: ChildSummarySlot,
}

impl SubagentSpawner for FacadeSubagentSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        // The run id is freshly minted per drive, so folding it into the trace
        // node id keeps the id unique even when the same delegate is driven more
        // than once in a single run (e.g. a dispatcher verifier re-run per
        // attempt, §13.3); a fixed `subagent:{name}` would collide.
        let run_id = self.ids.run_id();
        let node = TraceNodeId::new(format!("subagent:{}:{run_id}", self.subagent.name()));
        Ok((run_id, node))
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

        // One FacadeApproval remains the child's ToolApprovalPolicy gate. The
        // answer path is either the supervisor-injected async handler (with
        // attribution) or this same child approval fallback for headless runs.
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

        let interaction: Arc<dyn InteractionHandler> = match &self.parent_interaction {
            Some(parent) => Arc::new(ChildInteractionRouter {
                delegate: self.subagent.name().to_owned(),
                parent: parent.clone(),
            }),
            None => child_approval,
        };

        let scope = ChildAgentScope {
            llm: LlmClientHandler::new(self.client.clone()),
            tool: ToolRegistryHandler::new(registry),
            interaction,
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
            .unwrap_or_else(|poison| poison.into_inner())
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
///
/// Route recognition is gated on the **current** active tool set: the route is
/// fixed per run, but the base registry sits behind a slot a turn-boundary
/// tool-set reconfig can swap, so a call to a reconfig-removed `ask_<name>`
/// resolves to the same `UnknownTool` tool result the filtered registry
/// returns for any other removed tool and never drives a delegation (M2-3).
pub(crate) struct DelegationToolHandler {
    base: ToolRegistryHandler,
    route: DelegationRoute,
    client: Arc<dyn LlmClient>,
    supervisor_model: ModelRef,
    parent_interaction: Option<Arc<dyn InteractionHandler>>,
    ids: FacadeIds,
    recorder: DelegationRecorder,
    approval: Arc<FacadeApproval>,
    max_depth: u32,
    /// Bridge a driven managed external delegate's collab observations flow into
    /// (§14 末段). Empty when no substrate is provisioned.
    collab: CollabBridge,
}

impl DelegationToolHandler {
    /// Wraps `base`, routing calls the `route` recognizes through the subagent
    /// path and recording each delegation's trace into `recorder`.
    ///
    /// `parent_interaction` is present only when the supervisor has an injected
    /// async [`InteractionHandler`]; local child interactions and external-start
    /// asks are then answered there with delegate attribution while each approval
    /// policy remains the gate. `approval` is the run's [`FacadeApproval`]; a
    /// managed external delegate is gated before it is driven (§9.2), using the
    /// async parent handler for ask tiers when available and
    /// [`resolve_external_start`](FacadeApproval::resolve_external_start) as the
    /// synchronous fallback. `collab` is the facade's provisioned collaboration
    /// substrate (§14); a driven external delegate's collab observations are
    /// reflected into it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        base: ToolRegistryHandler,
        route: DelegationRoute,
        client: Arc<dyn LlmClient>,
        supervisor_model: ModelRef,
        parent_interaction: Option<Arc<dyn InteractionHandler>>,
        ids: FacadeIds,
        recorder: DelegationRecorder,
        approval: Arc<FacadeApproval>,
        collab: CollabBridge,
    ) -> Self {
        Self {
            base,
            route,
            client,
            supervisor_model,
            parent_interaction,
            ids,
            recorder,
            approval,
            max_depth: DEFAULT_MAX_DELEGATION_DEPTH,
            collab,
        }
    }

    /// Reports whether `name` is a delegation tool the **current** active tool
    /// set still declares.
    ///
    /// The per-run [`DelegationRoute`] is fixed at build time, but the base
    /// registry sits behind a swappable slot: a run start filters it by
    /// `current_tool_set` and a turn-boundary tool-set reconfig swaps a smaller
    /// registry in. A reconfig-removed `ask_<name>` must therefore stop routing
    /// even though the fixed route still recognizes the name — event taps use
    /// this predicate so such a call is bracketed like any other unknown tool,
    /// not like a delegation (M2-3).
    pub(crate) fn is_active_delegation(&self, name: &str) -> bool {
        self.route.is_delegation(name) && self.active_set_declares(name)
    }

    /// Reports whether the currently-installed base registry declares `name`.
    ///
    /// The facade's active registry advertises exactly the active tool set's
    /// declarations and rejects every other name with `UnknownTool`, so its
    /// declaration set is the authoritative "still active" check for both the
    /// run-start filtered registry and a mid-run swapped one (M2-3).
    fn active_set_declares(&self, name: &str) -> bool {
        self.base
            .current()
            .declarations()
            .iter()
            .any(|declaration| declaration.name == name)
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
            parent_interaction: self.parent_interaction.clone(),
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
            .unwrap_or_else(|poison| poison.into_inner())
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
            .unwrap_or_else(|poison| poison.into_inner())
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
        if !self
            .resolve_external_start(call_id, call, delegate.name(), ctx)
            .await
        {
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
        match drive_external(
            delegate.name(),
            delegate.agent(),
            &self.ids,
            task,
            &self.collab,
            self.parent_interaction.clone(),
            ctx,
        )
        .await
        {
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

    /// Resolves the drive-layer approval gate for one managed external start.
    ///
    /// Auto allow/deny remain synchronous. When the effective policy tier is ask
    /// and the supervisor injected an async [`InteractionHandler`], the start
    /// decision is routed through that handler with delegate attribution; without
    /// such a handler, the legacy synchronous [`FacadeApproval`] path is used.
    async fn resolve_external_start(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        delegate: &str,
        ctx: &RunContext,
    ) -> bool {
        if self.approval.external_start_requires_ask(&call.name)
            && let Some(parent) = &self.parent_interaction
        {
            return self
                .resolve_external_start_with_parent(parent, call_id, call, delegate, ctx)
                .await;
        }
        self.approval.resolve_external_start(&call.name)
    }

    /// Asks the injected parent interaction handler whether an external delegate
    /// may start, using the same approval interaction family as tool approvals.
    async fn resolve_external_start_with_parent(
        &self,
        parent: &Arc<dyn InteractionHandler>,
        call_id: ToolCallId,
        call: &ToolCall,
        delegate: &str,
        ctx: &RunContext,
    ) -> bool {
        let reason = format!(
            "approve start of managed external agent `{delegate}` via `{}`",
            call.name
        );
        let request = Interaction::approval(
            self.ids.step_id(),
            call_id,
            ApprovalRequirement::required(Some(reason)),
        )
        .with_origin(InteractionOrigin::new(
            delegate.to_owned(),
            ctx.depth().saturating_add(1),
        ));
        let result = tokio::select! {
            biased;
            _ = ctx.cancellation().cancelled() => cancelled_interaction_result(&request),
            result = parent.fulfill(&request, ctx) => result,
        };

        let RequirementResult::Interaction(response) = result else {
            return false;
        };
        if request.accepts_response(&response).is_err() {
            return false;
        }
        matches!(
            response,
            InteractionResponse::Approval(approval)
                if approval.decision() == ApprovalDecision::Approve
        )
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
            .unwrap_or_else(|poison| poison.into_inner())
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

    /// Drives one rules-routed delegation to `target` and records its trace,
    /// usage, and any artifacts under `call_id` (`docs/facade-api.md` §13.2).
    ///
    /// Rules-routed delegation is initiated by the facade, not the model, so
    /// there is no model-issued tool call to fulfill. This synthesizes the
    /// `ask_<name>(task)` call the delegate drive expects and reuses the exact
    /// same fulfillment path a model-routed delegation would — a local subagent
    /// through [`drive_delegation`](Self::drive_delegation), a managed external
    /// agent through [`drive_external_delegation`](Self::drive_external_delegation)
    /// (including its §9.2 approval gate) — so the recorder entry, usage
    /// attribution, and artifact capture are identical.
    pub(crate) async fn fulfill_rules_routed(
        &self,
        call_id: ToolCallId,
        target: &RulesRoutedTarget,
        task: String,
        ctx: &RunContext,
    ) -> RequirementResult {
        match target {
            RulesRoutedTarget::Local(subagent) => {
                let call = synthetic_delegation_call(&call_id, subagent.name(), &task);
                self.drive_delegation(call_id, &call, subagent, task, ctx)
                    .await
            }
            RulesRoutedTarget::External(delegate) => {
                let call = synthetic_delegation_call(&call_id, delegate.name(), &task);
                self.drive_external_delegation(call_id, &call, delegate, task, ctx)
                    .await
            }
        }
    }
}

/// The delegate a rules-routed delegation resolves a task to (§13.2).
///
/// Resolved by the facade from a [`Delegation::route_task`] match against the
/// agent's registered delegates. It owns the delegate recipe (both variants are
/// cheap, data-only clones) so the drive holds no borrow of the agent across an
/// `await`. Dispatcher-routed delegation (§13.3) reuses the same target type,
/// cloning it per worker attempt.
#[derive(Clone)]
pub(crate) enum RulesRoutedTarget {
    /// The task routes to a local subagent.
    Local(LocalSubagent),
    /// The task routes to a managed external agent.
    External(ManagedExternalDelegate),
}

/// Synthesizes the `ask_<name>(task)` tool call a rules-routed delegation drive
/// consumes, keyed by the framework `call_id` so its recorder entry matches.
fn synthetic_delegation_call(call_id: &ToolCallId, delegate_name: &str, task: &str) -> ToolCall {
    ToolCall {
        id: call_id.to_string(),
        name: delegation_tool_name(delegate_name),
        input: json!({ "task": task }),
        extra: Map::new(),
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
        // The fixed per-run route still recognizes a delegation tool a
        // tool-set reconfig removed; honor the active registry's declaration
        // set instead and fold back the same `UnknownTool` the filtered
        // registry returns for any other removed tool, without recording or
        // driving a delegation (M2-3).
        if self.route.is_delegation(&call.name) && !self.active_set_declares(&call.name) {
            return RequirementResult::Tool(Err(ToolRuntimeError::UnknownTool {
                name: call.name.clone(),
            }));
        }
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
    use crate::facade::error::FacadeError;
    use crate::facade::ids::FacadeIds;
    use crate::model::extras::{ProviderExtras, ProviderId};
    use crate::model::tool::Tool as ToolDecl;
    use serde_json::{Map, json};

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

    fn provider_extras() -> ProviderExtras {
        ProviderExtras {
            provider: ProviderId::Anthropic,
            fields: Map::from_iter([("top_k".to_owned(), json!(25))]),
        }
    }

    #[test]
    fn explicit_model_worker_is_data_only_and_not_inheriting() {
        let extras = provider_extras();
        let sub = worker()
            .description("Strict reviewer")
            .model("gpt-5.5")
            .temperature(0.1)
            .provider_extras(extras.clone())
            .system("You review code.")
            .build()
            .expect("worker builds");

        assert_eq!(sub.name(), "");
        assert_eq!(sub.description(), "Strict reviewer");
        assert!(!sub.inherits_model());
        assert_eq!(sub.spec().model().model(), "gpt-5.5");
        assert_eq!(sub.spec().model().temperature(), Some(0.1));
        assert_eq!(sub.spec().model().provider_extras(), Some(&extras));
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
    fn inherited_worker_rejects_provider_extras_without_explicit_model() {
        let error = worker()
            .provider_extras(provider_extras())
            .build()
            .expect_err("inherited model has no worker-local provider extras slot");

        let FacadeError::Config(message) = error else {
            panic!("expected config error")
        };
        assert!(message.contains("provider_extras"));
    }

    #[test]
    fn explicit_worker_rejects_blank_model() {
        let error = worker()
            .model("  ")
            .build()
            .expect_err("blank model is rejected");

        let FacadeError::Config(message) = error else {
            panic!("expected config error")
        };
        assert!(message.contains("model"));
    }

    #[test]
    fn explicit_worker_rejects_non_finite_temperature() {
        let error = worker()
            .model("gpt-5.5")
            .temperature(f32::NEG_INFINITY)
            .build()
            .expect_err("non-finite temperature is rejected");

        let FacadeError::Config(message) = error else {
            panic!("expected config error")
        };
        assert!(message.contains("temperature"));
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

    use crate::agent::{
        AgentId, ApprovalResponse, Interaction, InteractionHandler, InteractionKind,
        InteractionResponse, RequirementResult, RunContext,
    };
    use crate::client::{Capability, ChatRequest, ClientError, LlmClient, Response};
    use crate::facade::approval::{Approval, ApprovalDecision, ApprovalPolicy};
    use crate::facade::run::{DelegationStatus, RunEvent};
    use crate::facade::{Agent, AgentBuilder, CancelHandle};
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
                RequirementResult::Interaction(InteractionResponse::Approval(
                    ApprovalResponse::new(request.step_id(), *call_id, decision, None),
                ))
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

    #[tokio::test]
    async fn external_collab_observations_bridge_into_provisioned_primitives() {
        // §14 末段: an external delegate's send_message / plan_update /
        // blackboard_post observations reflect into the facade's provisioned
        // collab substrate. An explicit `Collaboration` provisions all three
        // primitives (a lone external delegate would otherwise only get
        // artifacts), so the bridge has somewhere to write.
        use crate::facade::{Collaboration, ManagedExternalAgent};

        let recipient =
            AgentId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890c4").expect("agent id");
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

        let coder = ManagedExternalAgent::claude_code()
            .session_handler(Arc::new(collab_external_handler(
                "refactor complete",
                recipient,
            )))
            .build()
            .expect("managed external agent builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("coder", coder)
            .collaboration(
                Collaboration::new()
                    .plan()
                    .blackboard()
                    .mailbox()
                    .artifacts(),
            )
            .build()
            .expect("agent builds");

        agent.run_full("Please refactor the parser.").await.unwrap();

        // send_message → the shared mailbox, attributed to the delegate.
        let mailbox = agent.mailbox().expect("mailbox provisioned");
        let inbox = mailbox.inbox(&recipient.to_string());
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, "coder");
        assert_eq!(inbox[0].text, "please review the parser change");

        // plan_update → the shared plan, reconciled to the reported status and
        // owned by the delegate.
        let plan = agent.plan().expect("plan provisioned");
        let snapshot = plan.snapshot();
        let task = snapshot.tasks.get("parser").expect("task reflected");
        assert_eq!(task.status, crate::agent::collab::TaskStatus::Completed);
        assert_eq!(task.owner.as_deref(), Some("coder"));

        // blackboard_post → the shared blackboard channel it named.
        let blackboard = agent.blackboard().expect("blackboard provisioned");
        let posts = blackboard.snapshot("status");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].sender, "coder");
        assert_eq!(posts[0].text, "parser done");
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
    async fn child_approval_interaction_routes_to_parent_handler_with_origin() {
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

        let child_sync_called = Arc::new(AtomicBool::new(false));
        let child_sync_probe = child_sync_called.clone();
        let child_approval = ApprovalPolicy::new(Approval::ask(move |request| {
            if request.tool_name == "shell" {
                child_sync_probe.store(true, Ordering::SeqCst);
            }
            ApprovalDecision::Deny
        }));
        let reviewer = Agent::worker()
            .system("You are the REVIEWER.")
            .tool_declarations(vec![shell_decl()])
            .approval(child_approval)
            .build()
            .expect("worker builds");
        let parent_handler = Arc::new(RecordingParentInteractionHandler::new(
            ApprovalDecision::Deny,
        ));

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .interaction_handler(parent_handler.clone())
            .subagent("reviewer", reviewer)
            .build()
            .expect("agent builds");

        let output = agent.run_full("Delegate an inspection.").await.unwrap();

        assert_eq!(output.reply.text(), "Final: done.");
        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].status, DelegationStatus::Completed);
        assert!(
            !child_sync_called.load(Ordering::SeqCst),
            "the child worker policy gates the call, but the parent handler answers it"
        );
        let seen = parent_handler.seen();
        assert_eq!(seen.len(), 1, "the parent handler receives the child ask");
        let origin = seen[0]
            .origin()
            .expect("child interaction carries delegate attribution");
        assert_eq!(origin.delegate, "reviewer");
        assert_eq!(origin.depth, 1);
        assert!(
            matches!(seen[0].kind(), InteractionKind::Approval { .. }),
            "the forwarded interaction remains an approval"
        );
    }

    #[tokio::test]
    async fn cancelling_while_parent_child_interaction_handler_is_parked_does_not_hang() {
        let client = RoutingClient::new(vec![
            route(
                "SUPERVISOR",
                vec![
                    tool_call_response(
                        "del-9",
                        "ask_reviewer",
                        json!({ "task": "inspect the tree" }),
                    ),
                    text_response("Final: should not be reached."),
                ],
            ),
            route(
                "REVIEWER",
                vec![
                    tool_call_response("child-shell-1", "shell", json!({ "cmd": "ls" })),
                    text_response("reviewer would continue after an answer"),
                ],
            ),
        ]);

        let reviewer = Agent::worker()
            .system("You are the REVIEWER.")
            .tool_declarations(vec![shell_decl()])
            .approval(ApprovalPolicy::new(Approval::ask(|_| {
                ApprovalDecision::Deny
            })))
            .build()
            .expect("worker builds");
        let (parent_handler, reached_rx) = ParkingParentInteractionHandler::new();
        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .interaction_handler(parent_handler)
            .subagent("reviewer", reviewer)
            .build()
            .expect("agent builds");
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();

        let run = agent.run_full_with_cancel("Delegate an inspection.", cancel.clone());
        let canceller = async move {
            let interaction = tokio::time::timeout(std::time::Duration::from_secs(1), reached_rx)
                .await
                .expect("parent handler should be reached before the test timeout")
                .expect("parent handler sends the interaction");
            let origin = interaction
                .origin()
                .expect("parked child interaction carries delegate attribution");
            assert_eq!(origin.delegate, "reviewer");
            assert_eq!(origin.depth, 1);
            trigger.cancel();
        };

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let (result, ()) = tokio::join!(run, canceller);
            result
        })
        .await
        .expect("cancelling a parked child interaction must not hang");

        let error = result.expect_err("the run should stop through cancellation");
        assert!(
            matches!(&error, crate::facade::FacadeError::Agent(agent) if agent.to_string().contains("cancelled")),
            "cancelled run should surface an agent cancellation diagnostic, got {error:?}"
        );
        assert!(cancel.is_cancelled());
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
    async fn external_start_ask_external_agents_routes_to_parent_handler() {
        let parent_handler = Arc::new(RecordingParentInteractionHandler::new(
            ApprovalDecision::Approve,
        ));
        let mut agent = AgentBuilder::default()
            .client(external_supervisor_client())
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(ApprovalPolicy::from(Approval::auto_allow()).ask_external_agents())
            .interaction_handler(parent_handler.clone())
            .external_agent("coder", completed_coder())
            .build()
            .expect("agent builds");

        let output = agent.run_full("Please refactor.").await.unwrap();

        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].delegate, "coder");
        assert_eq!(output.delegations[0].status, DelegationStatus::Completed);
        assert!(
            tool_result_texts(&agent)
                .iter()
                .any(|text| text == "refactor complete"),
            "an async-approved external delegate is driven"
        );

        let seen = parent_handler.seen();
        assert_eq!(seen.len(), 1, "the parent handler receives the start ask");
        let origin = seen[0]
            .origin()
            .expect("external-start approval carries delegate attribution");
        assert_eq!(origin.delegate, "coder");
        assert_eq!(origin.depth, 1);
        let InteractionKind::Approval { requirement, .. } = seen[0].kind() else {
            panic!("external-start approval uses the approval interaction family");
        };
        assert!(
            requirement
                .reason()
                .is_some_and(|reason| reason.contains("managed external agent `coder`")),
            "the approval reason identifies the delegate start"
        );
    }

    #[tokio::test]
    async fn external_start_ask_tool_denied_by_parent_handler_surfaces_approval_denied() {
        let parent_handler = Arc::new(RecordingParentInteractionHandler::new(
            ApprovalDecision::Deny,
        ));
        let mut agent = AgentBuilder::default()
            .client(external_supervisor_client())
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(ApprovalPolicy::default().ask_tool("ask_coder"))
            .interaction_handler(parent_handler.clone())
            .external_agent("coder", completed_coder())
            .build()
            .expect("agent builds");

        let error = agent
            .run_full("Please refactor.")
            .await
            .expect_err("the parent handler denies the external start");
        assert!(
            matches!(error, crate::facade::FacadeError::ApprovalDenied),
            "async-denied external start surfaces ApprovalDenied, got {error:?}"
        );
        assert_eq!(
            parent_handler.seen().len(),
            1,
            "per-tool ask_tool routes the start ask to the parent handler"
        );
        assert!(
            !tool_result_texts(&agent)
                .iter()
                .any(|text| text == "refactor complete"),
            "a denied external delegate is not driven"
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

    // ---- rules-routed delegation (`docs/facade-api.md` §13.2) ----

    #[test]
    fn rules_route_task_first_match_wins_and_is_case_insensitive() {
        let routing = Delegation::rules()
            .when_task_contains(["review", "audit"], "reviewer")
            .when_task_contains(["fix", "compile"], "coder");
        assert!(routing.is_rules_routed());

        // The first rule whose keyword hits wins, even when a later rule would
        // also match — registration order is the routing priority.
        assert_eq!(
            routing.route_task("Please REVIEW and fix the diff"),
            Some("reviewer")
        );
        // Matching is case-insensitive substring containment.
        assert_eq!(routing.route_task("time to COMPILE it"), Some("coder"));
        // No keyword present routes nowhere (the supervisor answers instead).
        assert_eq!(routing.route_task("write documentation"), None);
    }

    #[test]
    fn rules_mode_advertises_no_delegate_tools() {
        let routing = Delegation::rules().when_task_contains(["fix"], "coder");
        assert!(
            routing.declarations(&[], &[]).is_empty(),
            "rules-routed delegation exposes no delegate to the model"
        );
        assert!(routing.external_tool_names(&[]).is_empty());
    }

    #[test]
    fn when_task_contains_switches_a_non_rules_delegation_to_rules() {
        // Chaining onto the default model-routed delegation flips it to rules
        // mode, starting from the single appended rule.
        let routing = Delegation::model_routed().when_task_contains(["fix"], "coder");
        assert!(routing.is_rules_routed());
        assert_eq!(routing.route_task("fix it"), Some("coder"));
    }

    #[test]
    fn unknown_rule_delegate_is_detected_for_build_validation() {
        let routing = Delegation::rules().when_task_contains(["x"], "ghost");
        assert_eq!(
            routing.first_unknown_rule_delegate(&[], &[]).as_deref(),
            Some("ghost")
        );
    }

    #[tokio::test]
    async fn rules_routed_task_routes_to_matching_local_subagent() {
        // No SUPERVISOR route is scripted: a rules-routed turn must not take an
        // LLM step, so the supervisor client is never asked to `chat`.
        let client = RoutingClient::new(vec![route(
            "REVIEWER",
            vec![text_response("LGTM: no issues found")],
        )]);

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
            .delegation(Delegation::rules().when_task_contains(["review", "audit"], "reviewer"))
            .build()
            .expect("agent builds");

        // The model is never told a delegate exists: no delegation tool is
        // advertised on the supervisor spec.
        let advertised: Vec<&str> = agent
            .state()
            .spec()
            .initial_tools()
            .tools()
            .iter()
            .map(|decl| decl.name.as_str())
            .collect();
        assert!(
            !advertised.iter().any(|name| name.starts_with("ask_")),
            "rules-routed delegation advertises no delegate tool, got {advertised:?}"
        );

        let output = agent.run_full("Please review the diff.").await.unwrap();

        // With no supervisor step the delegate's summary is the whole reply.
        assert_eq!(output.reply.text(), "LGTM: no issues found");
        // The supervisor took no LLM step, so its usage slice is zero.
        assert_eq!(output.usage.supervisor.input, 0);
        assert_eq!(output.usage.supervisor.output, 0);

        // Exactly one delegation trace, attributed to the reviewer, completed.
        assert_eq!(output.delegations.len(), 1);
        let trace = &output.delegations[0];
        assert_eq!(trace.delegate, "reviewer");
        assert_eq!(trace.status, DelegationStatus::Completed);

        // Child usage is attributed to the subagent slice, not the supervisor.
        assert_eq!(output.usage.subagents.input, 11);
        assert_eq!(output.usage.subagents.output, 7);

        // The routed exchange is not folded into the supervisor conversation.
        assert!(
            agent.conversation().turns().is_empty(),
            "a rules-routed turn does not commit to the supervisor conversation"
        );

        // Bracketing events: Started then Finished, no ordinary tool events.
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
            "no ordinary tool events for a rules-routed delegation"
        );
    }

    #[tokio::test]
    async fn rules_routed_task_routes_to_external_delegate() {
        // The supervisor client is never asked to chat; only the external
        // delegate's scripted session runs.
        let client = RoutingClient::new(vec![route("SUPERVISOR", vec![text_response("unused")])]);

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("coder", completed_coder())
            .delegation(Delegation::rules().when_task_contains(["refactor", "fix"], "coder"))
            .build()
            .expect("agent builds");

        let output = agent.run_full("Please refactor the parser.").await.unwrap();

        // The external summary is the whole reply.
        assert_eq!(output.reply.text(), "refactor complete");

        // One external delegation trace, completed, with runtime usage on the
        // external slice.
        assert_eq!(output.delegations.len(), 1);
        let trace = &output.delegations[0];
        assert_eq!(trace.delegate, "coder");
        assert_eq!(trace.status, DelegationStatus::Completed);
        assert_eq!(output.usage.external.input, 4);
        assert_eq!(output.usage.external.output, 2);
        assert_eq!(output.usage.subagents.input, 0);

        // The reported artifact surfaces on the run output.
        assert_eq!(output.artifacts.len(), 1);
        assert_eq!(output.artifacts[0].path, "src/parser.rs");

        // The external delegate's resumable session facts are retained for a
        // later snapshot (§15.2).
        let snapshot = agent.snapshot().expect("snapshot at a committed point");
        let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
        assert!(
            json.contains("resume-1"),
            "the retained external session token is persisted in the snapshot"
        );
    }

    #[tokio::test]
    async fn rules_routed_no_match_runs_the_supervisor_normally() {
        // A task matching no rule falls through to the ordinary supervisor drive.
        let client = RoutingClient::new(vec![route(
            "SUPERVISOR",
            vec![text_response("I answered it myself.")],
        )]);

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
            .delegation(Delegation::rules().when_task_contains(["review", "audit"], "reviewer"))
            .build()
            .expect("agent builds");

        let output = agent.run_full("Write the documentation.").await.unwrap();

        // The supervisor answered directly; no delegation happened.
        assert_eq!(output.reply.text(), "I answered it myself.");
        assert!(
            output.delegations.is_empty(),
            "a non-matching task is not delegated"
        );
        assert_eq!(output.usage.supervisor.input, 11);
        // The supervisor turn is committed to the conversation as usual.
        assert!(!agent.conversation().turns().is_empty());
    }

    #[tokio::test]
    async fn rules_routed_first_matching_rule_wins_across_delegates() {
        // Both rules would match "review and refactor"; the first (reviewer) wins.
        let client = RoutingClient::new(vec![
            route("REVIEWER", vec![text_response("reviewer handled it")]),
            route("CODER", vec![text_response("coder handled it")]),
        ]);

        let reviewer = Agent::worker()
            .description("Strict reviewer.")
            .system("You are the REVIEWER.")
            .build()
            .expect("worker builds");
        let coder = Agent::worker()
            .description("Focused coder.")
            .system("You are the CODER.")
            .build()
            .expect("worker builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("reviewer", reviewer)
            .subagent("coder", coder)
            .delegation(
                Delegation::rules()
                    .when_task_contains(["review"], "reviewer")
                    .when_task_contains(["review", "refactor"], "coder"),
            )
            .build()
            .expect("agent builds");

        let output = agent
            .run_full("Please review and refactor the module.")
            .await
            .unwrap();
        assert_eq!(output.reply.text(), "reviewer handled it");
        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].delegate, "reviewer");
    }

    #[test]
    fn rules_routed_unknown_delegate_is_rejected_at_build() {
        let client = RoutingClient::new(vec![route("SUPERVISOR", vec![text_response("x")])]);
        let error = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .delegation(Delegation::rules().when_task_contains(["fix"], "ghost"))
            .build()
            .expect_err("a rule naming an unregistered delegate is rejected");
        assert!(
            matches!(error, crate::facade::FacadeError::Config(_)),
            "an unknown rule delegate is a build-time Config error, got {error:?}"
        );
    }

    #[tokio::test]
    async fn rules_routed_stream_yields_delegation_events_then_done() {
        let client = RoutingClient::new(vec![route(
            "REVIEWER",
            vec![text_response("streamed review done")],
        )]);

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
            .delegation(Delegation::rules().when_task_contains(["review"], "reviewer"))
            .build()
            .expect("agent builds");

        let mut stream = agent
            .stream("Please review the diff.")
            .await
            .expect("stream starts");
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item.expect("stream item is ok"));
        }

        assert!(
            events
                .iter()
                .any(|event| matches!(event, RunEvent::DelegationStarted(_))),
            "the stream surfaces a DelegationStarted event"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RunEvent::DelegationFinished(_))),
            "the stream surfaces a DelegationFinished event"
        );
        let done = events
            .iter()
            .find_map(|event| match event {
                RunEvent::Done(output) => Some(output),
                _ => None,
            })
            .expect("the stream ends with a Done event");
        assert_eq!(done.reply.text(), "streamed review done");
        assert_eq!(done.delegations.len(), 1);
        assert_eq!(done.delegations[0].delegate, "reviewer");
    }

    // ---- dispatcher-routed delegation (`docs/facade-api.md` §13.3) ----

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

    #[test]
    fn dispatcher_builder_sets_config_and_advertises_no_tools() {
        let routing = Delegation::dispatcher()
            .primary("cheap-coder")
            .verify_with("verifier")
            .escalate_to("strong-coder")
            .max_attempts(3);
        assert!(routing.is_dispatcher_routed());

        let config = routing
            .dispatcher_config()
            .expect("dispatcher config present");
        assert_eq!(config.primary(), "cheap-coder");
        assert_eq!(config.verifier(), Some("verifier"));
        assert_eq!(config.escalate_to(), Some("strong-coder"));
        assert_eq!(config.max_attempts(), 3);

        // No delegate is ever advertised to the supervising model (§13.3).
        assert!(
            routing.declarations(&[], &[]).is_empty(),
            "dispatcher-routed delegation exposes no delegate to the model"
        );
        assert!(routing.external_tool_names(&[]).is_empty());
    }

    #[test]
    fn dispatcher_max_attempts_is_clamped_to_at_least_one() {
        let routing = Delegation::dispatcher().primary("cheap").max_attempts(0);
        assert_eq!(
            routing.dispatcher_config().expect("config").max_attempts(),
            1,
            "max_attempts clamps up to 1 so the primary always runs once"
        );
    }

    #[test]
    fn dispatcher_builder_switches_a_non_dispatcher_delegation() {
        // Chaining a dispatcher setter onto the default model-routed delegation
        // flips it into dispatcher mode, starting from a fresh config.
        let routing = Delegation::model_routed().primary("cheap");
        assert!(routing.is_dispatcher_routed());
        assert_eq!(
            routing.dispatcher_config().expect("config").primary(),
            "cheap"
        );
    }

    #[test]
    fn unknown_dispatcher_delegate_is_detected_for_build_validation() {
        let routing = Delegation::dispatcher()
            .primary("cheap")
            .escalate_to("ghost");
        // `cheap` is unregistered too, but the primary is reported first.
        assert_eq!(
            routing
                .first_unknown_dispatcher_delegate(&[], &[])
                .as_deref(),
            Some("cheap")
        );
    }

    #[tokio::test]
    async fn dispatcher_empty_primary_is_rejected_at_build() {
        let error = AgentBuilder::default()
            .client(RoutingClient::new(vec![route(
                "SUPERVISOR",
                vec![text_response("x")],
            )]))
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .subagent("strong", dispatch_worker("You are the STRONG worker."))
            .delegation(Delegation::dispatcher().escalate_to("strong"))
            .build()
            .expect_err("an empty primary is rejected");
        assert!(
            matches!(error, crate::facade::FacadeError::Config(_)),
            "a dispatcher with no primary fails to build, got {error:?}"
        );
    }

    #[tokio::test]
    async fn dispatcher_unknown_delegate_is_rejected_at_build() {
        let error = AgentBuilder::default()
            .client(RoutingClient::new(vec![route(
                "SUPERVISOR",
                vec![text_response("x")],
            )]))
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .delegation(Delegation::dispatcher().primary("ghost"))
            .build()
            .expect_err("an unregistered delegate is rejected");
        assert!(
            matches!(error, crate::facade::FacadeError::Config(_)),
            "a dispatcher naming an unregistered delegate fails to build, got {error:?}"
        );
    }

    #[tokio::test]
    async fn dispatcher_escalates_when_primary_fails_then_strong_succeeds() {
        use crate::facade::ManagedExternalAgent;

        // Only the strong worker takes an LLM step; the primary is a managed
        // external agent with no session handler, so its delegation fails outright
        // and the loop escalates without ever consulting a verifier.
        let client = RoutingClient::new(vec![route(
            "STRONG",
            vec![text_response("strong solution complete")],
        )]);

        let cheap = ManagedExternalAgent::claude_code()
            .build()
            .expect("managed external agent builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("cheap", cheap)
            .subagent("strong", dispatch_worker("You are the STRONG worker."))
            .delegation(
                Delegation::dispatcher()
                    .primary("cheap")
                    .escalate_to("strong")
                    .max_attempts(2),
            )
            .build()
            .expect("agent builds");

        let output = agent
            .run_full("Please implement the feature.")
            .await
            .unwrap();

        // The final reply is the escalation target's summary, not the failed
        // primary's error.
        assert_eq!(output.reply.text(), "strong solution complete");

        // The escalation path (primary → strong) is captured as an event.
        assert_eq!(
            escalation_edge(&output),
            Some(("cheap".to_owned(), "strong".to_owned())),
            "an Escalated event records the primary → strong hand-off"
        );

        // Both attempts are recorded: the failed primary and the successful strong
        // worker, in order.
        assert_eq!(output.delegations.len(), 2);
        assert_eq!(output.delegations[0].delegate, "cheap");
        assert_eq!(output.delegations[0].status, DelegationStatus::Failed);
        assert_eq!(output.delegations[1].delegate, "strong");
        assert_eq!(output.delegations[1].status, DelegationStatus::Completed);

        // The failed primary emits DelegationFailed; the strong worker Finished.
        assert!(
            output
                .events
                .iter()
                .any(|event| matches!(event, RunEvent::DelegationFailed(_))),
            "the failed primary emits DelegationFailed"
        );

        // The supervisor took no LLM step.
        assert_eq!(output.usage.supervisor.input, 0);
        assert_eq!(output.usage.supervisor.output, 0);
        assert!(
            agent.conversation().turns().is_empty(),
            "a dispatcher-routed turn does not commit to the supervisor conversation"
        );
    }

    #[tokio::test]
    async fn dispatcher_verifier_rejection_escalates_to_strong() {
        // The verifier rejects the primary's output (call 1) then approves the
        // strong worker's output (call 2), driving exactly one escalation.
        let client = RoutingClient::new(vec![
            route("CHEAP", vec![text_response("cheap attempt at the task")]),
            route(
                "VERIFIER",
                vec![
                    text_response("ESCALATE: this is insufficient"),
                    text_response("approved, this looks good"),
                ],
            ),
            route("STRONG", vec![text_response("strong result delivered")]),
        ]);

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("cheap", dispatch_worker("You are the CHEAP worker."))
            .subagent("verifier", dispatch_worker("You are the VERIFIER."))
            .subagent("strong", dispatch_worker("You are the STRONG worker."))
            .delegation(
                Delegation::dispatcher()
                    .primary("cheap")
                    .verify_with("verifier")
                    .escalate_to("strong")
                    .max_attempts(2),
            )
            .build()
            .expect("agent builds");

        let output = agent.run_full("Please solve the problem.").await.unwrap();

        // The final reply is the strong worker's summary, never the verifier's.
        assert_eq!(output.reply.text(), "strong result delivered");
        assert_eq!(
            escalation_edge(&output),
            Some(("cheap".to_owned(), "strong".to_owned()))
        );

        // Four delegations run: cheap, verifier (reject), strong, verifier (pass).
        let names: Vec<&str> = output
            .delegations
            .iter()
            .map(|trace| trace.delegate.as_str())
            .collect();
        assert_eq!(names, ["cheap", "verifier", "strong", "verifier"]);
        assert!(
            output
                .delegations
                .iter()
                .all(|trace| trace.status == DelegationStatus::Completed),
            "every worker and verifier delegation completed cleanly"
        );

        // Child usage is attributed to the subagent slice, not the supervisor.
        assert_eq!(output.usage.supervisor.input, 0);
        assert!(output.usage.subagents.input > 0);
    }

    #[tokio::test]
    async fn dispatcher_verifier_pass_does_not_escalate() {
        // The verifier approves the primary on the first pass, so the strong
        // worker — though configured — never runs.
        let client = RoutingClient::new(vec![
            route("CHEAP", vec![text_response("cheap solved it cleanly")]),
            route("VERIFIER", vec![text_response("approved, looks good")]),
            route("STRONG", vec![text_response("UNUSED strong output")]),
        ]);

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("cheap", dispatch_worker("You are the CHEAP worker."))
            .subagent("verifier", dispatch_worker("You are the VERIFIER."))
            .subagent("strong", dispatch_worker("You are the STRONG worker."))
            .delegation(
                Delegation::dispatcher()
                    .primary("cheap")
                    .verify_with("verifier")
                    .escalate_to("strong")
                    .max_attempts(2),
            )
            .build()
            .expect("agent builds");

        let output = agent.run_full("Please solve the problem.").await.unwrap();

        // The primary's summary is the whole reply; no escalation happened.
        assert_eq!(output.reply.text(), "cheap solved it cleanly");
        assert!(
            escalation_edge(&output).is_none(),
            "a passing verifier produces no Escalated event"
        );

        // Only the primary and its verifier ran; the strong worker did not.
        let names: Vec<&str> = output
            .delegations
            .iter()
            .map(|trace| trace.delegate.as_str())
            .collect();
        assert_eq!(names, ["cheap", "verifier"]);
    }

    #[tokio::test]
    async fn dispatcher_respects_max_attempts_of_one() {
        use crate::facade::ManagedExternalAgent;

        // A single attempt runs the primary once and never escalates, even though
        // the primary fails and an escalation target is configured.
        let client = RoutingClient::new(vec![route(
            "STRONG",
            vec![text_response("UNUSED strong output")],
        )]);

        let cheap = ManagedExternalAgent::claude_code()
            .build()
            .expect("managed external agent builds");

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("cheap", cheap)
            .subagent("strong", dispatch_worker("You are the STRONG worker."))
            .delegation(
                Delegation::dispatcher()
                    .primary("cheap")
                    .escalate_to("strong")
                    .max_attempts(1),
            )
            .build()
            .expect("agent builds");

        let output = agent
            .run_full("Please implement the feature.")
            .await
            .unwrap();

        assert!(
            escalation_edge(&output).is_none(),
            "max_attempts(1) never escalates"
        );
        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].delegate, "cheap");
        assert_eq!(output.delegations[0].status, DelegationStatus::Failed);
    }

    #[tokio::test]
    async fn dispatcher_stream_yields_escalated_then_done() {
        let client = RoutingClient::new(vec![
            route("CHEAP", vec![text_response("cheap attempt at the task")]),
            route(
                "VERIFIER",
                vec![
                    text_response("ESCALATE: this is insufficient"),
                    text_response("approved, this looks good"),
                ],
            ),
            route("STRONG", vec![text_response("strong result delivered")]),
        ]);

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("cheap", dispatch_worker("You are the CHEAP worker."))
            .subagent("verifier", dispatch_worker("You are the VERIFIER."))
            .subagent("strong", dispatch_worker("You are the STRONG worker."))
            .delegation(
                Delegation::dispatcher()
                    .primary("cheap")
                    .verify_with("verifier")
                    .escalate_to("strong")
                    .max_attempts(2),
            )
            .build()
            .expect("agent builds");

        let mut stream = agent
            .stream("Please solve the problem.")
            .await
            .expect("stream starts");
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item.expect("stream item is ok"));
        }

        assert!(
            events
                .iter()
                .any(|event| matches!(event, RunEvent::Escalated(_))),
            "the dispatcher stream surfaces an Escalated event"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RunEvent::DelegationStarted(_))),
            "the dispatcher stream surfaces a DelegationStarted event"
        );
        let done = events
            .iter()
            .find_map(|event| match event {
                RunEvent::Done(output) => Some(output),
                _ => None,
            })
            .expect("the stream ends with a Done event");
        assert_eq!(done.reply.text(), "strong result delivered");
        assert_eq!(
            escalation_edge(done),
            Some(("cheap".to_owned(), "strong".to_owned()))
        );
    }

    // -----------------------------------------------------------------------
    // AI-decision injection seams (milestone M7-5, docs/facade-api.md §19)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dispatcher_injected_verifier_forces_escalation() {
        use crate::agent::{EscalationTrigger, ScriptedVerifier, Verifier};

        // No verifier delegate is configured, so by default a clean primary run
        // is accepted and never escalates. Injecting a Verifier that always
        // rejects (the AI-verification seam, §19) overrides that default verdict
        // and forces exactly one escalation to the strong worker.
        let client = RoutingClient::new(vec![
            route("CHEAP", vec![text_response("cheap attempt at the task")]),
            route("STRONG", vec![text_response("strong result delivered")]),
        ]);

        let verifier: Arc<dyn Verifier + Send + Sync> = Arc::new(ScriptedVerifier::rejecting(
            EscalationTrigger::ReviewRejected,
        ));

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .subagent("cheap", dispatch_worker("You are the CHEAP worker."))
            .subagent("strong", dispatch_worker("You are the STRONG worker."))
            .delegation(
                Delegation::dispatcher()
                    .primary("cheap")
                    .escalate_to("strong")
                    .max_attempts(2)
                    .dispatcher_verifier(verifier),
            )
            .build()
            .expect("agent builds");

        let output = agent.run_full("Please solve the problem.").await.unwrap();

        // The injected verifier rejected the clean primary, so the loop escalated
        // to the strong worker and returned its output.
        assert_eq!(output.reply.text(), "strong result delivered");
        assert_eq!(
            escalation_edge(&output),
            Some(("cheap".to_owned(), "strong".to_owned()))
        );
        let names: Vec<&str> = output
            .delegations
            .iter()
            .map(|trace| trace.delegate.as_str())
            .collect();
        assert_eq!(names, ["cheap", "strong"]);
    }

    #[tokio::test]
    async fn dispatcher_injected_evaluator_declines_escalation() {
        use crate::agent::{ScriptedTaskEvaluator, TaskEvaluator};
        use crate::facade::ManagedExternalAgent;

        // The primary is a managed external agent with no session handler, so it
        // fails its delegation — which by default escalates to the configured
        // strong worker (cf. `dispatcher_escalates_when_primary_fails...`).
        // Injecting a TaskEvaluator that declines (returns `None`, the AI-routing
        // seam, §19) suppresses that escalation entirely.
        let client = RoutingClient::new(vec![route(
            "STRONG",
            vec![text_response("UNUSED strong output")],
        )]);

        let cheap = ManagedExternalAgent::claude_code()
            .build()
            .expect("managed external agent builds");

        let evaluator: Arc<dyn TaskEvaluator + Send + Sync> =
            Arc::new(ScriptedTaskEvaluator::new(|_, _| None));

        let mut agent = AgentBuilder::default()
            .client(client)
            .model("supervisor-model")
            .system("You are the SUPERVISOR.")
            .approval(Approval::auto_allow())
            .external_agent("cheap", cheap)
            .subagent("strong", dispatch_worker("You are the STRONG worker."))
            .delegation(
                Delegation::dispatcher()
                    .primary("cheap")
                    .escalate_to("strong")
                    .max_attempts(2)
                    .dispatcher_evaluator(evaluator),
            )
            .build()
            .expect("agent builds");

        let output = agent
            .run_full("Please implement the feature.")
            .await
            .unwrap();

        assert!(
            escalation_edge(&output).is_none(),
            "the injected evaluator declined, so no escalation occurred"
        );
        assert_eq!(output.delegations.len(), 1);
        assert_eq!(output.delegations[0].delegate, "cheap");
        assert_eq!(output.delegations[0].status, DelegationStatus::Failed);
    }

    #[test]
    fn dispatcher_injection_hooks_stored_and_serde_drops_them() {
        use crate::agent::{
            EscalationTrigger, ScriptedTaskEvaluator, ScriptedVerifier, TaskEvaluator, Verifier,
            WorkerProfileRef,
        };

        let evaluator: Arc<dyn TaskEvaluator + Send + Sync> = Arc::new(
            ScriptedTaskEvaluator::always(WorkerProfileRef::new("strong")),
        );
        let verifier: Arc<dyn Verifier + Send + Sync> = Arc::new(ScriptedVerifier::rejecting(
            EscalationTrigger::ReviewRejected,
        ));

        // The builder switches to dispatcher mode and stores both runtime hooks.
        let with_hooks = Delegation::dispatcher()
            .primary("cheap")
            .escalate_to("strong")
            .dispatcher_evaluator(evaluator)
            .dispatcher_verifier(verifier);
        assert!(with_hooks.is_dispatcher_routed());
        assert!(with_hooks.dispatcher_evaluator_hook().is_some());
        assert!(with_hooks.dispatcher_verifier_hook().is_some());

        // The same config without hooks: neither hook is present, but the two are
        // config-equal because the injected handlers are runtime-only identity.
        let without_hooks = Delegation::dispatcher()
            .primary("cheap")
            .escalate_to("strong");
        assert!(without_hooks.dispatcher_evaluator_hook().is_none());
        assert!(without_hooks.dispatcher_verifier_hook().is_none());
        assert_eq!(
            with_hooks, without_hooks,
            "injected runtime hooks do not change config identity"
        );

        // Snapshotting (a serde round-trip) drops the runtime hooks (§15.2), so a
        // restored delegation falls back to the built-in defaults.
        let json = serde_json::to_string(&with_hooks).expect("serializes");
        let restored: Delegation = serde_json::from_str(&json).expect("deserializes");
        assert!(restored.dispatcher_evaluator_hook().is_none());
        assert!(restored.dispatcher_verifier_hook().is_none());
    }
}
