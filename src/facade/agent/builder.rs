//! [`AgentBuilder`] and the build/validation helpers behind [`Agent::builder`].
//!
//! Split out of `agent.rs`: this module owns everything that turns builder
//! options into a ready-to-run [`Agent`] — client/provider resolution, tool
//! declaration assembly and validation, machine assembly, loop policy, the
//! run-scope handler pieces shared with the streaming path, and reconfigure
//! admission checks.

use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::agent::{
    AgentError, AgentSpec, AgentState, BudgetLimits, DeclaredOnlyToolRegistryResolver,
    DefaultAgentMachine, ErrorCursor, ErrorCursorKind, HandlerScope, Interaction,
    InteractionHandler, InteractionKind, LlmClientHandler, LlmHandler, LlmStepMode, LoopCursor,
    LoopPolicy, ModelRef, ReconfigHandler, ReconfigRegistryHandler, ReconfigRequest,
    RequirementIds, RequirementResult, RunContext, ToolApprovalPolicy, ToolExecutionIds,
    ToolFailurePolicy, ToolHandler, ToolRegistry, ToolSetRef, WorktreeRef,
};
use crate::client::LlmClient;
use crate::conversation::{Conversation, ConversationConfig};
use crate::facade::approval::{ApprovalPolicy, FacadeApproval, enriched_approval_request};
use crate::facade::chat::client_for_provider;
use crate::facade::collab::{CollabState, Collaboration, resolve};
use crate::facade::config::{
    ModelConfig, ProviderConfig, ensure_finite_temperature, ensure_non_blank_model,
    ensure_provider_extras_match_provider,
};
use crate::facade::delegate::{Delegation, DelegationToolHandler, LocalSubagent};
use crate::facade::error::FacadeError;
use crate::facade::external::{ManagedExternalAgent, ManagedExternalDelegate};
use crate::facade::ids::FacadeIds;
use crate::facade::run::{ApprovalRequest, RunEvent};
use crate::facade::tool::{Tool, ensure_unique_declaration_names, ensure_unique_tool_names};
use crate::model::extras::ProviderExtras;
use crate::model::tool::Tool as ToolDecl;

use super::reconfig::FacadeToolRegistryResolver;
use super::{Agent, DEFAULT_MAX_STEPS, DEFAULT_MAX_TOOL_ROUNDS};

/// A fluent builder for [`Agent`].
///
/// Set either an explicit [`client`](AgentBuilder::client) (handy for offline
/// tests) or a [`provider`](AgentBuilder::provider), a `model`, and then any
/// number of typed [`tool`](AgentBuilder::tool)s, an
/// [`approval`](AgentBuilder::approval) policy, and loop-policy overrides.
#[derive(Default)]
pub struct AgentBuilder {
    provider: Option<ProviderConfig>,
    client: Option<Arc<dyn LlmClient>>,
    model: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    provider_extras: Option<ProviderExtras>,
    system: Option<String>,
    tools: Vec<Tool>,
    custom_registry: Option<Arc<dyn ToolRegistry>>,
    extra_declarations: Vec<ToolDecl>,
    approval: Option<ApprovalPolicy>,
    interaction_handler: Option<Arc<dyn InteractionHandler>>,
    max_steps: Option<u32>,
    max_tool_rounds: Option<u32>,
    budget: Option<BudgetLimits>,
    tool_failure_policy: Option<ToolFailurePolicy>,
    worktree: Option<WorktreeRef>,
    ids: Option<FacadeIds>,
    delegates: Vec<LocalSubagent>,
    external_agents: Vec<ManagedExternalDelegate>,
    delegation: Option<Delegation>,
    collaboration: Option<Collaboration>,
}

impl std::fmt::Debug for AgentBuilder {
    /// Prints structural fields while treating the client as opaque.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentBuilder")
            .field("provider", &self.provider)
            .field("has_client", &self.client.is_some())
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("provider_extras", &self.provider_extras)
            .field("system", &self.system)
            .field(
                "tools",
                &self.tools.iter().map(Tool::name).collect::<Vec<_>>(),
            )
            .field("has_custom_registry", &self.custom_registry.is_some())
            .field("approval", &self.approval)
            .field(
                "has_interaction_handler",
                &self.interaction_handler.is_some(),
            )
            .field("max_steps", &self.max_steps)
            .field("max_tool_rounds", &self.max_tool_rounds)
            .field("budget", &self.budget)
            .field("tool_failure_policy", &self.tool_failure_policy)
            .field(
                "delegates",
                &self
                    .delegates
                    .iter()
                    .map(LocalSubagent::name)
                    .collect::<Vec<_>>(),
            )
            .field(
                "external_agents",
                &self
                    .external_agents
                    .iter()
                    .map(ManagedExternalDelegate::name)
                    .collect::<Vec<_>>(),
            )
            .field("delegation", &self.delegation)
            .field("collaboration", &self.collaboration)
            .finish_non_exhaustive()
    }
}

impl AgentBuilder {
    /// Sets the provider used to construct the client when none is injected.
    ///
    /// Ignored when an explicit [`client`](AgentBuilder::client) is also set.
    #[must_use]
    pub fn provider(mut self, provider: ProviderConfig) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Injects a concrete client, bypassing provider-based construction.
    ///
    /// This is the recommended path for offline tests: a scripted fake client
    /// can be supplied without touching the network.
    #[must_use]
    pub fn client(mut self, client: Arc<dyn LlmClient>) -> Self {
        self.client = Some(client);
        self
    }

    /// Sets the model or deployment identifier (required).
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Sets the maximum number of output tokens per LLM step.
    ///
    /// A value of `0` is treated as "leave at the default" (see
    /// [`ModelConfig::max_tokens`]).
    #[must_use]
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Sets the sampling temperature.
    #[must_use]
    pub fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets provider-specific request fields for every supervisor LLM request.
    ///
    /// When this builder also has a [`provider`](Self::provider), the extras'
    /// [`ProviderId`](crate::model::extras::ProviderId) must match that provider.
    /// Builders that use only an injected [`client`](Self::client) cannot infer a
    /// provider id and pass the extras through to the injected client unchanged.
    #[must_use]
    pub fn provider_extras(mut self, provider_extras: ProviderExtras) -> Self {
        self.provider_extras = Some(provider_extras);
        self
    }

    /// Sets the system prompt applied to every turn.
    #[must_use]
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Registers one typed function [`Tool`].
    ///
    /// Any tool-level [`Approval`](crate::facade::Approval) override attached to
    /// the tool is folded into the effective approval policy at
    /// [`build`](AgentBuilder::build) time, where it wins over the agent-level
    /// entry for the same name.
    #[must_use]
    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Registers an escape-hatch [`ToolRegistry`] whose tools the facade does not
    /// own (`docs/facade-api.md` §7.3).
    ///
    /// The tool names admissible through [`Agent::reconfigure`] are frozen from
    /// the declarations known at build/restore time; a custom registry that
    /// changes its declarations later does not update that admission surface.
    #[must_use]
    pub fn tool_registry(mut self, registry: Arc<dyn ToolRegistry>) -> Self {
        self.custom_registry = Some(registry);
        self
    }

    /// Advertises extra tool declarations executed elsewhere (§7.3).
    #[must_use]
    pub fn tool_declarations(mut self, declarations: Vec<ToolDecl>) -> Self {
        self.extra_declarations = declarations;
        self
    }

    /// Sets the agent-level approval policy.
    ///
    /// Accepts either a whole-agent [`Approval`](crate::facade::Approval) tier or
    /// a fully built [`ApprovalPolicy`], since `Approval` converts into a policy
    /// whose default is that tier.
    #[must_use]
    pub fn approval(mut self, approval: impl Into<ApprovalPolicy>) -> Self {
        self.approval = Some(approval.into());
        self
    }

    /// Injects a custom async [`InteractionHandler`] that answers whatever the
    /// agent machine pauses on (chiefly tool-call approvals), replacing the
    /// synchronous [`FacadeApproval`] fallback (`docs/facade-api.md` §19).
    ///
    /// The default facade approval path resolves a decision **synchronously** on
    /// the drive task, so it cannot `await` a cross-process answer. The
    /// lower-layer [`InteractionHandler`] is an
    /// `async` pause point: a host can emit a request from
    /// [`fulfill`](crate::agent::InteractionHandler::fulfill), `await` a
    /// `oneshot`, and return the caller's
    /// [`InteractionResponse`](crate::agent::InteractionResponse) once it
    /// arrives. Both the blocking [`run`](Agent::run) path and the incremental
    /// [`stream`](Agent::stream) path route their paused interactions through the
    /// injected handler.
    ///
    /// # Priority relative to [`approval`](Self::approval)
    ///
    /// When a handler is injected it becomes the **sole authority** for
    /// *answering* a paused interaction: the [`ApprovalPolicy`]'s per-decision
    /// `ask`/`deny` logic is overridden by the handler's own decision. The policy
    /// still governs the machine **gate** — that is, which tool calls pause at
    /// all (an [`auto_allow`](crate::facade::Approval::auto_allow) tool runs
    /// unattended and never reaches the handler). To route every tool call
    /// through the injected handler, pair it with an ask/deny default such as
    /// [`Approval::auto_deny`](crate::facade::Approval::auto_deny) or
    /// [`ask_tool`](ApprovalPolicy::ask_tool). When no handler is injected the
    /// behavior is identical to Milestone 2's [`FacadeApproval`].
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use agent_lib::agent::InteractionHandler;
    /// # use agent_lib::facade::{AgentBuilder, Approval};
    /// # fn wire(builder: AgentBuilder, handler: Arc<dyn InteractionHandler>) -> AgentBuilder {
    /// // Pause every tool call, then let the injected handler decide.
    /// builder
    ///     .approval(Approval::auto_deny())
    ///     .interaction_handler(handler)
    /// # }
    /// ```
    #[must_use]
    pub fn interaction_handler(mut self, handler: Arc<dyn InteractionHandler>) -> Self {
        self.interaction_handler = Some(handler);
        self
    }

    /// Overrides the per-turn LLM-step budget (default `8`).
    #[must_use]
    pub fn max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = Some(max_steps);
        self
    }

    /// Overrides the maximum number of tool-call rounds per turn (default `4`).
    #[must_use]
    pub fn max_tool_rounds(mut self, max_tool_rounds: u32) -> Self {
        self.max_tool_rounds = Some(max_tool_rounds);
        self
    }

    /// Sets the run-level budget shared by the supervisor and any child agents.
    ///
    /// The default is [`BudgetLimits::unbounded`]. Each facade `run` / `stream`
    /// creates a fresh [`RunContext`] with these limits, so counters reset between
    /// top-level runs while subagents and managed delegates within one run share
    /// the same ledger.
    #[must_use]
    pub fn budget(mut self, budget: BudgetLimits) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Overrides how a failed tool call is handled (default
    /// [`ToolFailurePolicy::ReturnErrorToModel`]).
    #[must_use]
    pub fn tool_failure_policy(mut self, policy: ToolFailurePolicy) -> Self {
        self.tool_failure_policy = Some(policy);
        self
    }

    /// Sets the isolated worktree the agent runs against (default `"."`).
    #[must_use]
    pub fn worktree(mut self, worktree: WorktreeRef) -> Self {
        self.worktree = Some(worktree);
        self
    }

    /// Overrides the built-in identity source (mainly for deterministic tests).
    #[must_use]
    pub fn ids(mut self, ids: FacadeIds) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Registers a local subagent delegate under `name`.
    ///
    /// The `worker` is a data-first [`LocalSubagent`] produced by
    /// [`Agent::worker`]; this stamps `name` onto it and records it in the
    /// agent's delegate table (`docs/facade-api.md` §10.1). The base path only
    /// stores local delegates; the unified delegate abstraction of §12 is
    /// reserved for later milestones. Registration order is preserved and
    /// exposed through [`Agent::subagents`].
    #[must_use]
    pub fn subagent(mut self, name: impl Into<String>, worker: LocalSubagent) -> Self {
        self.delegates.push(worker.with_name(name));
        self
    }

    /// Registers a managed external agent delegate under `name`.
    ///
    /// The `agent` is a data-first [`ManagedExternalAgent`] recipe; this stamps
    /// `name` onto it and records it in the agent's external-delegate table
    /// (`docs/facade-api.md` §13.1). Like a local subagent it is exposed to the
    /// supervising model as its own `ask_<name>` tool, but a fulfilled delegation
    /// drives the external CLI runtime instead of an in-library child (milestone
    /// M4-2). Registration order is preserved and exposed through
    /// [`Agent::external_agents`].
    ///
    /// The delegate must carry a runtime session handler (attached with
    /// [`ManagedExternalAgentBuilder::session_handler`](crate::facade::ManagedExternalAgentBuilder::session_handler))
    /// before a delegation can be driven; a delegate without one fails the
    /// delegation with [`FacadeError::ExternalAgent`].
    ///
    /// ```no_run
    /// # fn demo() -> Result<(), agent_lib::facade::FacadeError> {
    /// use agent_lib::facade::{Agent, ManagedExternalAgent, ProviderConfig};
    ///
    /// let coder = ManagedExternalAgent::claude_code().build()?;
    /// let agent = Agent::builder()
    ///     .provider(ProviderConfig::openai_from_env()?)
    ///     .model("gpt-5.5")
    ///     .system("You coordinate a managed coding agent.")
    ///     .external_agent("coder", coder)
    ///     .build()?;
    ///
    /// // The delegate is exposed to the supervising model as an `ask_coder` tool.
    /// assert_eq!(agent.external_agents().len(), 1);
    /// assert_eq!(agent.external_agents()[0].name(), "coder");
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn external_agent(mut self, name: impl Into<String>, agent: ManagedExternalAgent) -> Self {
        self.external_agents
            .push(ManagedExternalDelegate::new(name, agent));
        self
    }

    /// Sets the delegation routing strategy for the registered subagents.
    ///
    /// Defaults to [`Delegation::model_routed`] (one `ask_<name>` tool per
    /// subagent, `docs/facade-api.md` §13.1). Pass
    /// [`Delegation::single_tool`] to collapse every delegate behind one unified
    /// `<name>(agent, task)` tool that routes by its `agent` argument (§10.2), or
    /// [`Delegation::rules`] to let the facade route a whole task to a delegate by
    /// keyword — exposing no delegate to the model at all (§13.2). A rules-routed
    /// delegation whose rules name a delegate no agent registered is rejected by
    /// [`build`](Self::build).
    #[must_use]
    pub fn delegation(mut self, delegation: Delegation) -> Self {
        self.delegation = Some(delegation);
        self
    }

    /// Sets the collaboration substrate for the registered delegates.
    ///
    /// By default the substrate is derived from the delegate topology
    /// (`docs/facade-api.md` §14): no delegate enables nothing, multiple
    /// delegates auto-enable a shared mailbox, a dispatcher-routed loop
    /// additionally enables a plan board and blackboard, and a managed external
    /// delegate enables the artifact store. Passing an explicit
    /// [`Collaboration`] **replaces** that derived default in full, so a caller
    /// can enable exactly the subset they want:
    ///
    /// ```no_run
    /// # fn demo(builder: agent_lib::facade::AgentBuilder) -> agent_lib::facade::AgentBuilder {
    /// use agent_lib::facade::Collaboration;
    ///
    /// builder.collaboration(Collaboration::new().plan().blackboard().mailbox().artifacts())
    /// # }
    /// ```
    ///
    /// Enabling a substrate provisions a live, shared primitive reachable through
    /// [`Agent::mailbox`], [`Agent::blackboard`], and [`Agent::plan`]. The
    /// external-runtime collab-event bridge that populates them is a later
    /// milestone; this layer provisions the substrate that bridge writes into.
    #[must_use]
    pub fn collaboration(mut self, collaboration: Collaboration) -> Self {
        self.collaboration = Some(collaboration);
        self
    }

    /// Finalizes the builder into an [`Agent`], assembling the §8.3 machine stack.
    ///
    /// # Errors
    ///
    /// - [`FacadeError::Config`] when no model was set, or when neither an
    ///   explicit client nor a provider was supplied.
    /// - [`FacadeError::DuplicateTool`] when a tool name is declared more than
    ///   once across the typed tools, the escape-hatch declarations, the custom
    ///   registry, and the synthesized delegation tools (two subagents minting
    ///   the same `ask_<name>`, or a delegation tool clashing with another tool).
    pub fn build(self) -> Result<Agent, FacadeError> {
        let model_name = self.model.ok_or_else(|| {
            FacadeError::Config("agent configuration is missing a `model`".to_owned())
        })?;
        let model_name = ensure_non_blank_model("agent", model_name)?;
        if let Some(provider_extras) = &self.provider_extras {
            ensure_provider_extras_match_provider(
                "agent",
                self.provider.as_ref().map(ProviderConfig::provider),
                provider_extras,
            )?;
        }
        let client = match (self.client, self.provider) {
            (Some(client), _) => client,
            (None, Some(provider)) => client_for_provider(provider),
            (None, None) => {
                return Err(FacadeError::Config(
                    "agent configuration needs either a `client` or a `provider`".to_owned(),
                ));
            }
        };

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

        let ids = self.ids.unwrap_or_default();
        let loop_policy = build_loop_policy(
            self.max_steps.unwrap_or(DEFAULT_MAX_STEPS),
            self.max_tool_rounds.unwrap_or(DEFAULT_MAX_TOOL_ROUNDS),
            self.tool_failure_policy
                .unwrap_or(ToolFailurePolicy::ReturnErrorToModel),
        );

        let delegation = self.delegation.unwrap_or_default();
        let declarations = build_agent_tool_declarations(
            &self.tools,
            &self.extra_declarations,
            self.custom_registry.as_ref(),
            &delegation,
            &self.delegates,
            &self.external_agents,
        )?;
        let tools: Arc<[Tool]> = Arc::from(self.tools);
        let extra_declarations: Arc<[ToolDecl]> = Arc::from(self.extra_declarations);
        let tool_registry_resolver = Arc::new(FacadeToolRegistryResolver::new(
            tools.clone(),
            self.custom_registry.clone(),
            extra_declarations.clone(),
            declarations.clone(),
        ));

        let spec = AgentSpec::new(
            ids.agent_id(),
            self.worktree.unwrap_or_else(|| WorktreeRef::new(".")),
            self.system,
            ToolSetRef::new(ids.tool_set_id(), declarations),
            model.to_model_ref(),
            loop_policy,
        );
        let state = AgentState::new(
            spec,
            Conversation::new(ids.conversation_id(), ConversationConfig::new(None)),
        );

        // One FacadeApproval bridges both runtime roles: it is the machine's pure
        // ToolApprovalPolicy and the scope's InteractionHandler, sharing one
        // pending-decision map through a single Arc. The model-routed external
        // start tools are registered so the machine gate exempts them and the
        // drive layer is the sole approval authority for external delegates.
        let external_tool_names = delegation.external_tool_names(&self.external_agents);
        let approval = build_facade_approval(
            self.approval.unwrap_or_default(),
            &tools,
            external_tool_names,
        );

        let machine = assemble_machine(state, &ids, approval.clone())
            .with_tool_registry_resolver(tool_registry_resolver.clone());
        let budget = self.budget.unwrap_or_else(BudgetLimits::unbounded);

        // Resolve the collaboration substrate from the delegate topology (§14),
        // letting an explicit `Collaboration` override the derived default, then
        // provision the live shared primitives each enabled substrate needs.
        let collaboration = resolve(
            self.collaboration,
            &delegation,
            self.delegates.len(),
            self.external_agents.len(),
        );
        let collab = CollabState::provision(collaboration, &ids);

        Ok(Agent {
            machine,
            client,
            tools,
            custom_registry: self.custom_registry,
            extra_declarations,
            tool_registry_resolver,
            approval,
            interaction_handler: self.interaction_handler,
            ids,
            delegates: self.delegates,
            external_agents: self.external_agents,
            delegation,
            budget,
            collab,
            last_external_sessions: HashMap::new(),
        })
    }
}

/// Builds the shared [`FacadeApproval`] bridge from an agent-level policy, the
/// per-tool overrides carried on each typed [`Tool`], and the model-routed
/// external start-tool names to exempt from the machine gate.
///
/// A tool-level [`Approval`](crate::facade::Approval) override wins over the
/// agent-level entry for the same name (`docs/facade-api.md` §9.1). The returned
/// value is shared behind one [`Arc`] so the machine (as
/// [`ToolApprovalPolicy`]) and the drive scope (as [`InteractionHandler`])
/// observe the same pending-decision map. `external_tools` names the model-routed
/// `ask_<name>` delegate start tools; they are gated at the drive layer, so the
/// machine gate exempts them to avoid double-prompting (§9.2).
pub(crate) fn build_facade_approval(
    policy: ApprovalPolicy,
    tools: &[Tool],
    external_tools: Vec<String>,
) -> Arc<FacadeApproval> {
    let mut approval = FacadeApproval::new(policy).with_external_tools(external_tools);
    for tool in tools {
        if let Some(tool_approval) = tool.approval_override() {
            approval = approval.with_tool_override(tool.name(), tool_approval.clone());
        }
    }
    Arc::new(approval)
}

/// Builds and validates the complete model-visible tool declaration surface.
///
/// Fresh build and restore both need the same checks: the runtime tool handlers
/// cannot collide with each other, routing modes may not reference missing
/// delegates, and synthesized delegation tools must not collide with any base
/// tool declaration.
pub(crate) fn build_agent_tool_declarations(
    tools: &[Tool],
    extra_declarations: &[ToolDecl],
    custom_registry: Option<&Arc<dyn ToolRegistry>>,
    delegation: &Delegation,
    delegates: &[LocalSubagent],
    external_agents: &[ManagedExternalDelegate],
) -> Result<Vec<ToolDecl>, FacadeError> {
    // Reject duplicate base tool names before adding delegation declarations.
    ensure_unique_tool_names(tools, extra_declarations, custom_registry)?;

    validate_delegation_references(delegation, delegates, external_agents)?;

    // The advertised tool set must mirror what the run-scoped
    // FacadeToolRegistry reports, then include any synthesized delegation tools:
    // one `ask_<name>` tool per delegate (model-routed, §10.1) or a single
    // unified `<name>(agent, task)` tool (§10.2).
    let mut declarations: Vec<ToolDecl> = tools.iter().map(Tool::declaration).collect();
    declarations.extend(extra_declarations.iter().cloned());
    if let Some(custom) = custom_registry {
        declarations.extend(custom.declarations());
    }
    declarations.extend(delegation.declarations(delegates, external_agents));

    // Reject collisions introduced by the delegation layer: two delegates minting
    // the same `ask_<name>`, or a delegation tool clashing with a typed tool /
    // escape-hatch declaration (§10.1).
    ensure_unique_declaration_names(&declarations)?;
    Ok(declarations)
}

fn validate_delegation_references(
    delegation: &Delegation,
    delegates: &[LocalSubagent],
    external_agents: &[ManagedExternalDelegate],
) -> Result<(), FacadeError> {
    delegation.validate_configuration()?;

    // Rules-routed delegation names delegates by string; a name no agent
    // registered can never route, so reject it up front (§13.2).
    if let Some(unknown) = delegation.first_unknown_rule_delegate(delegates, external_agents) {
        return Err(FacadeError::Config(format!(
            "rules-routed delegation references unregistered delegate `{unknown}`"
        )));
    }

    // Dispatcher-routed delegation likewise names its primary / verifier /
    // escalation delegates by string: a missing primary or an unregistered name
    // can never run, so reject both up front (§13.3).
    if let Some(config) = delegation.dispatcher_config() {
        if config.primary().is_empty() {
            return Err(FacadeError::Config(
                "dispatcher-routed delegation is missing a `primary` delegate".to_owned(),
            ));
        }
        if let Some(unknown) =
            delegation.first_unknown_dispatcher_delegate(delegates, external_agents)
        {
            return Err(FacadeError::Config(format!(
                "dispatcher-routed delegation references unregistered delegate `{unknown}`"
            )));
        }
    }

    Ok(())
}

/// Assembles the §8.3 [`DefaultAgentMachine`] over `state`, wiring the facade
/// identity source and the shared approval policy.
///
/// Both [`AgentBuilder::build`] and the restore path share this so a rebuilt
/// machine is wired identically to a freshly built one.
pub(crate) fn assemble_machine(
    state: AgentState,
    ids: &FacadeIds,
    approval: Arc<FacadeApproval>,
) -> DefaultAgentMachine {
    let requirement_ids: Arc<dyn RequirementIds> = Arc::new(ids.clone());
    let tool_ids: Arc<dyn ToolExecutionIds> = Arc::new(ids.clone());
    let approval_policy: Arc<dyn ToolApprovalPolicy> = approval;
    DefaultAgentMachine::new(state, LlmStepMode::NonStreaming, requirement_ids)
        .with_tool_execution_ids(tool_ids)
        .with_approval_policy(approval_policy)
        .with_tool_registry_resolver(Arc::new(DeclaredOnlyToolRegistryResolver))
}

/// One total drain layer carrying the LLM client, active tool registry,
/// reconfiguration registry swapper, and resolved interaction handler.
///
/// Model / system / loop reconfigurations apply directly at the turn boundary;
/// tool-set changes park on `NeedReconfigRegistry`, which this scope fulfills by
/// swapping the same registry slot read by tool execution. The
/// [`interaction`](Self::interaction) handler is the host-injected
/// [`InteractionHandler`] when one was supplied, otherwise the shared
/// [`FacadeApproval`] (§19).
pub(crate) struct FacadeAgentScope {
    pub(crate) llm: LlmClientHandler,
    pub(crate) tool: DelegationToolHandler,
    pub(crate) interaction: Arc<dyn InteractionHandler>,
    pub(crate) reconfig: ReconfigRegistryHandler,
}

impl HandlerScope for FacadeAgentScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        Some(&self.llm)
    }

    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&self.tool)
    }

    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        Some(self.interaction.as_ref())
    }

    fn reconfig(&self) -> Option<&dyn ReconfigHandler> {
        Some(&self.reconfig)
    }
}

/// An ordered, interior-mutable log of the approval requests a non-streaming
/// [`Agent::run_full`] drive paused on, filled in fulfill order by
/// [`RecordingInteractionHandler`].
pub(crate) type ApprovalRecorder = Arc<Mutex<Vec<ApprovalRequest>>>;

/// Wraps the resolved [`InteractionHandler`] for a non-streaming
/// [`Agent::run_full`] drive, recording each paused approval as an
/// [`ApprovalRequest`] before delegating so the terminal [`RunOutput::events`]
/// can surface a [`RunEvent::ApprovalRequested`] the streaming path emits live
/// through its `TapInteractionHandler` (M2-1).
///
/// The delegate `inner` is the host-injected handler when one was supplied to
/// [`AgentBuilder::interaction_handler`], otherwise the shared [`FacadeApproval`]
/// fallback, so this never changes which handler decides approve / deny /
/// fallback — it only *observes* the request on the way through. The enriched
/// request is built by [`enriched_approval_request`], the same helper the
/// streaming tap handler uses, so both paths map the `FacadeApproval` fields
/// identically.
pub(crate) struct RecordingInteractionHandler {
    pub(crate) approval: Arc<FacadeApproval>,
    pub(crate) inner: Arc<dyn InteractionHandler>,
    pub(crate) recorder: ApprovalRecorder,
}

#[async_trait]
impl InteractionHandler for RecordingInteractionHandler {
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult {
        if let InteractionKind::Approval {
            call_id,
            requirement,
        } = request.kind()
        {
            let approval_request = enriched_approval_request(&self.approval, *call_id, requirement);
            self.recorder
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(approval_request);
        }
        self.inner.fulfill(request, ctx).await
    }
}

/// Weaves the recorded [`ApprovalRequest`]s of a non-streaming drive into the
/// projected tool/delegation `events`, mirroring the order the streaming path
/// emits them live: a [`RunEvent::ApprovalRequested`] lands immediately before
/// the tool lifecycle of the call it gated.
///
/// Approvals are matched to tool events by `call_id` and anchored before the
/// first [`RunEvent::ToolStarted`] bearing that id, so an approved call surfaces
/// the approval immediately before its `ToolStarted`/`ToolFinished` pair. A
/// denied call never starts and therefore leaves *no* tool lifecycle event
/// (matching the streaming path); its approval has no tool-event anchor and is
/// flushed in recorded order at the point its decision was made — just before
/// the next anchored call it precedes, or at the tail. This guarantees every
/// paused approval stays observable even when the tool never executed.
pub(crate) fn weave_approval_events(
    events: Vec<RunEvent>,
    approvals: Vec<ApprovalRequest>,
) -> Vec<RunEvent> {
    if approvals.is_empty() {
        return events;
    }
    let mut merged = Vec::with_capacity(events.len() + approvals.len());
    let mut next = 0usize;
    for event in events {
        if let Some(call_id) = tool_event_call_id(&event) {
            // Flush pending approvals up to and including the one that gated this
            // call, so any earlier denied approvals keep their relative order.
            if let Some(offset) = approvals[next..]
                .iter()
                .position(|approval| approval.call_id.as_deref() == Some(call_id))
            {
                let through = next + offset;
                for approval in &approvals[next..=through] {
                    merged.push(RunEvent::ApprovalRequested(approval.clone()));
                }
                next = through + 1;
            }
        }
        merged.push(event);
    }
    for approval in &approvals[next..] {
        merged.push(RunEvent::ApprovalRequested(approval.clone()));
    }
    merged
}

/// Returns the framework `call_id` a tool-lifecycle [`RunEvent`] addresses, used
/// by [`weave_approval_events`] to anchor an approval before the call it gated.
///
/// Only [`RunEvent::ToolStarted`] / [`RunEvent::ToolFinished`] carry a `call_id`
/// (delegation traces do not). An approved call's `ToolStarted` is the anchor
/// for its gating approval; a denied call emits no tool event at all, so its
/// approval is instead flushed by [`weave_approval_events`] at the tail or
/// before the next anchored call.
fn tool_event_call_id(event: &RunEvent) -> Option<&str> {
    match event {
        RunEvent::ToolStarted(trace) | RunEvent::ToolFinished(trace) => Some(&trace.call_id),
        _ => None,
    }
}

/// Maps the facade's `max_steps` / `max_tool_rounds` knobs onto the single
/// per-turn step budget of a [`LoopPolicy`] (§8.4).
///
/// A successful run needs one LLM step per tool round plus one final response,
/// so the tighter of the two limits binds: `min(max_steps, max_tool_rounds + 1)`,
/// clamped to at least one step. Parallel tool execution is pinned to one, the
/// core default the base machine does not otherwise consume.
pub(crate) fn build_loop_policy(
    max_steps: u32,
    max_tool_rounds: u32,
    tool_failure_policy: ToolFailurePolicy,
) -> LoopPolicy {
    let effective = max_steps.min(max_tool_rounds.saturating_add(1)).max(1);
    LoopPolicy::new(
        NonZeroU32::new(effective).expect("effective step budget is clamped to at least one"),
        NonZeroU32::new(1).expect("one is non-zero"),
        tool_failure_policy,
    )
}

pub(crate) fn ensure_facade_reconfig_request_supported(
    request: &ReconfigRequest,
) -> Result<(), FacadeError> {
    match request {
        ReconfigRequest::ActivateSkill { .. }
        | ReconfigRequest::DeactivateSkill { .. }
        | ReconfigRequest::ReplaceActiveSkills { .. } => Err(FacadeError::Config(
            "facade reconfigure does not support skill activation requests; use the agent layer \
             until facade skill registry wiring exists"
                .to_owned(),
        )),
        ReconfigRequest::SetSystemPromptOverlay { .. }
        | ReconfigRequest::ReplaceToolSet { .. }
        | ReconfigRequest::PatchToolSet { .. }
        | ReconfigRequest::SetModel { .. }
        | ReconfigRequest::SetLoopPolicy { .. } => Ok(()),
    }
}

/// Merges the synthesized delegation declarations into a tool-set reconfig so
/// the model-visible surface always mirrors the currently registered delegates
/// (mag gap B1).
///
/// The agent layer applies `ReplaceToolSet` verbatim, but the facade owns the
/// delegation declarations (one `ask_<name>` per model-routed delegate, or the
/// unified single-tool name): they are synthesized `pub(crate)`-side, so a
/// caller's replacement set cannot contain them, and a verbatim apply would
/// silently drop every still-registered delegate from the tool surface. The
/// facade therefore intercepts here — the state layer has no delegate
/// knowledge — and splits responsibility: the caller's declarations cover the
/// non-delegation surface, while the delegation declarations are re-derived
/// from `delegation` + `delegates` + `external_agents` on every request and
/// appended to the queued set.
///
/// Delegation declarations are derived state, never caller-managed input:
///
/// - a `ReplaceToolSet` whose set declares a name that collides with a
///   synthesized delegation declaration is rejected (`FacadeError::Config`);
/// - a `PatchToolSet` that removes or shadows (add-or-replace) such a name is
///   rejected the same way, rather than silently re-synthesizing over the
///   caller's edit — an explicit error surfaces the mistaken assumption that
///   delegation tools are patch-managed. To retire a delegation tool, drop the
///   delegate registration (or prune it on restore) instead.
///
/// Agents without delegation declarations (rules/dispatcher routing, or no
/// delegates) pass every request through unchanged.
pub(crate) fn merge_facade_delegation_declarations(
    request: ReconfigRequest,
    delegation: &Delegation,
    delegates: &[LocalSubagent],
    external_agents: &[ManagedExternalDelegate],
) -> Result<ReconfigRequest, FacadeError> {
    let delegation_declarations = delegation.declarations(delegates, external_agents);
    if delegation_declarations.is_empty() {
        return Ok(request);
    }
    let delegation_names: BTreeSet<&str> = delegation_declarations
        .iter()
        .map(|declaration| declaration.name.as_str())
        .collect();

    match request {
        ReconfigRequest::ReplaceToolSet { tool_set } => {
            if let Some(conflict) = tool_set
                .tools()
                .iter()
                .find(|declaration| delegation_names.contains(declaration.name.as_str()))
            {
                return Err(FacadeError::Config(format!(
                    "replace-tool-set reconfig must not declare `{}`: delegation tool \
                     declarations are synthesized from the registered delegates and merged \
                     automatically",
                    conflict.name
                )));
            }
            let mut tools = tool_set.tools().to_vec();
            tools.extend(delegation_declarations);
            Ok(ReconfigRequest::ReplaceToolSet {
                tool_set: ToolSetRef::new(tool_set.id(), tools),
            })
        }
        ReconfigRequest::PatchToolSet { patch } => {
            for name in patch.remove() {
                if delegation_names.contains(name.as_str()) {
                    return Err(FacadeError::Config(format!(
                        "patch-tool-set reconfig cannot remove `{name}`: delegation tool \
                         declarations are derived from the registered delegates, not managed \
                         by callers; drop the delegate registration instead"
                    )));
                }
            }
            for tool in patch.add_or_replace() {
                if delegation_names.contains(tool.name.as_str()) {
                    return Err(FacadeError::Config(format!(
                        "patch-tool-set reconfig cannot add or replace `{}`: delegation tool \
                         declarations are synthesized from the registered delegates and must \
                         not be shadowed by caller declarations",
                        tool.name
                    )));
                }
            }
            Ok(ReconfigRequest::PatchToolSet { patch })
        }
        other => Ok(other),
    }
}

/// Validates a `SetModel` reconfig payload with the same checks
/// [`AgentBuilder::build`] applies to an initial model, so a reconfigured model
/// can never render an invalid request: the model name must be non-blank and
/// the temperature finite.
///
/// Provider extras are checked against the provider the *current* model's
/// extras target when one is inferable — the facade does not retain the
/// builder's [`ProviderConfig`] (an injected client has no reliable provider
/// id), so the current model is the only in-band provider signal. When neither
/// side carries extras the check passes through, mirroring the builder's
/// client-only escape hatch where the injected client decides how to handle
/// provider-specific fields.
pub(crate) fn ensure_facade_set_model_valid(
    model: &ModelRef,
    current: &ModelRef,
) -> Result<(), FacadeError> {
    ensure_non_blank_model("agent reconfigure", model.model().to_owned())?;
    if let Some(temperature) = model.temperature() {
        ensure_finite_temperature("agent reconfigure", temperature)?;
    }
    if let Some(provider_extras) = model.provider_extras() {
        ensure_provider_extras_match_provider(
            "agent reconfigure",
            current.provider_extras().map(|extras| extras.provider),
            provider_extras,
        )?;
    }
    Ok(())
}

pub(crate) fn ensure_facade_reconfig_rest_boundary(cursor: &LoopCursor) -> Result<(), FacadeError> {
    if matches!(
        cursor,
        LoopCursor::Idle
            | LoopCursor::Done(_)
            | LoopCursor::Error(_)
            | LoopCursor::CancelRecovery(_)
    ) {
        return Ok(());
    }

    Err(FacadeError::InvalidState(format!(
        "facade reconfigure is only accepted between runs; current cursor is {:?}",
        cursor.kind()
    )))
}

/// Classifies an [`ErrorCursor`] into a [`FacadeError`].
///
/// Since M4-4 the base machine reports an exhausted per-turn step budget as a
/// normal terminal ([`LoopDoneReason::StepLimitReached`]), which both run paths
/// map to [`FacadeError::LoopLimitExceeded`] structurally before reaching this
/// function. Error cursors use [`ErrorCursorKind`] for any additional stable
/// classification; the message is human-readable context only.
pub(crate) fn classify_error(error: &ErrorCursor) -> FacadeError {
    match error.kind() {
        ErrorCursorKind::LoopLimitExceeded => FacadeError::LoopLimitExceeded,
        ErrorCursorKind::Other => FacadeError::Agent(AgentError::Other(error.message().to_owned())),
    }
}
