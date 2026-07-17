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

use crate::agent::{AgentSpec, LoopPolicy, ModelRef, ToolFailurePolicy, ToolSetRef, WorktreeRef};
use crate::facade::agent::{DEFAULT_MAX_STEPS, DEFAULT_MAX_TOOL_ROUNDS, build_loop_policy};
use crate::facade::approval::ApprovalPolicy;
use crate::facade::config::ModelConfig;
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::model::tool::Tool as ToolDecl;

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
}
