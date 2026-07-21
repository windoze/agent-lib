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
//! its [`ApprovalPolicy`]. The child [`AgentState`](crate::agent::AgentState),
//! machine, and [`RunContext`](crate::agent::RunContext) are built only when a
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
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::{
    AgentSpec, LoopPolicy, ModelRef, TaskEvaluator, ToolFailurePolicy, ToolSetRef, Verifier,
    WorktreeRef,
};
use crate::facade::agent::{DEFAULT_MAX_STEPS, DEFAULT_MAX_TOOL_ROUNDS, build_loop_policy};
use crate::facade::approval::ApprovalPolicy;
use crate::facade::config::ModelConfig;
use crate::facade::error::FacadeError;
use crate::facade::external::ManagedExternalDelegate;
use crate::facade::ids::FacadeIds;
use crate::model::extras::ProviderExtras;
use crate::model::tool::Tool as ToolDecl;

mod handler;

pub(crate) use handler::{
    DelegationInteractionRouter, DelegationRecorder, DelegationRoute, DelegationToolHandler,
    RecordedDelegation, RulesRoutedTarget, delegation_child_ids, delegation_opening_input,
    delegation_single_tool_declaration, new_delegation_recorder, summarize_delegation_slot,
};

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

#[cfg(test)]
mod tests;

/// Offline coverage for the model-routed delegation path (milestone M3-2).
///
/// Every test is fully offline: a [`RoutingClient`] returns scripted responses
/// selected by the requesting agent's system prompt, so the supervisor and each
/// child are driven deterministically with no network, credential, or CLI, and
/// each finishes well under a second.
#[cfg(test)]
mod model_routed_tests;
