//! The multi-turn Agent facade over the sans-io agent machine stack.
//!
//! [`Agent`] is the facade's tool-using, approval-gated counterpart to
//! [`Chat`](crate::facade::Chat). Where `Chat` drives a bare
//! [`Conversation`] and rejects any tool use,
//! an `Agent` assembles the full agent-layer machine so a run can loop through
//! tool calls, approvals, and multiple LLM steps before returning a final
//! assistant message (`docs/facade-api.md` §8).
//!
//! # What `build()` assembles
//!
//! [`AgentBuilder::build`] performs the §8.3 wiring exactly once, then holds the
//! resulting machine across every [`run`](Agent::run) so history accumulates:
//!
//! ```text
//! AgentBuilder
//!   -> ToolSetRef (typed tools + escape-hatch declarations)
//!   -> AgentSpec (worktree, system prompt, model, loop policy)
//!   -> AgentState(Conversation::new)
//!   -> DefaultAgentMachine
//!        .with_tool_execution_ids(FacadeIds)
//!        .with_approval_policy(FacadeApproval)
//! ```
//!
//! No new effect family or bespoke state machine is introduced: a run is a
//! [`drain`] of the [`DefaultAgentMachine`] against a per-run
//! [`HandlerScope`] carrying the LLM client, the [`FacadeToolRegistry`], and the
//! [`FacadeApproval`] interaction handler (`docs/facade-api.md` §19).
//!
//! # Loop policy mapping
//!
//! The facade exposes two ergonomic knobs, `max_steps` (default `8`) and
//! `max_tool_rounds` (default `4`), while the underlying [`LoopPolicy`] has a
//! single per-turn step budget. A successful run needs one LLM step per tool
//! round plus one final response, so the effective budget is
//! `min(max_steps, max_tool_rounds + 1)` (§8.4). When that budget is exhausted
//! before a final assistant message the run fails with
//! [`FacadeError::LoopLimitExceeded`].

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::agent::requirement::AgentSpecRef;
use crate::agent::{
    AgentError, AgentInput, AgentSpec, AgentState, Blackboard, BudgetLimits, Capability, CostTier,
    DefaultAgentMachine, EscalationError, EscalationOutcome, EscalationRules, EscalationTrigger,
    Escalator, HandlerScope, HumanGate, ImpactScope, Interaction, InteractionHandler,
    InteractionKind, LlmClientHandler, LlmHandler, LlmStepMode, LoopCursor, LoopPolicy, Mailbox,
    ModelRef, Notification, PermissionRisk, Plan, RequirementIds, RequirementResult, RunContext,
    RunId, ScriptedVerifier, TaskDescriptor, TaskEvaluator, ToolApprovalPolicy, ToolExecutionIds,
    ToolFailurePolicy, ToolHandler, ToolRegistry, ToolRegistryHandler, ToolSetRef, Uncertainty,
    Verifier, WorkerProfile, WorkerProfileRef, WorkerReport, WorkerRoster, WorktreeRef, drain,
};
use crate::client::LlmClient;
use crate::conversation::{Conversation, ConversationConfig};
use crate::facade::approval::{ApprovalPolicy, FacadeApproval, enriched_approval_request};
use crate::facade::chat::client_for_provider;
use crate::facade::collab::{CollabBridge, CollabState, Collaboration, resolve};
use crate::facade::config::{ModelConfig, ProviderConfig};
use crate::facade::delegate::{
    AgentWorkerBuilder, DISPATCHER_ESCALATE_MARKER, Delegation, DelegationRecorder,
    DelegationRoute, DelegationToolHandler, DispatcherConfig, LocalSubagent, RecordedDelegation,
    RulesRoutedTarget, SharedTaskEvaluator, SharedVerifier, new_delegation_recorder,
};
use crate::facade::error::FacadeError;
use crate::facade::external::{
    ExternalDelegateStatus, ManagedExternalAgent, ManagedExternalDelegate, RetainedExternalSession,
};
use crate::facade::ids::FacadeIds;
use crate::facade::run::{
    ApprovalRequest, ArtifactRef, DelegationStatus, DelegationTrace, EscalationTrace,
    IntoUserMessage, Reply, RunEvent, RunOutput, ToolTrace, UsageSummary,
};
use crate::facade::tool::{
    FacadeToolRegistry, Tool, ToolContextParts, ensure_unique_declaration_names,
    ensure_unique_tool_names,
};
use crate::model::content::ContentBlock;
use crate::model::message::Message;
use crate::model::tool::Tool as ToolDecl;

mod snapshot;
mod stream;

pub use snapshot::{
    AgentParts, AgentRestoreBuilder, AgentSnapshot, AgentStateSnapshot, BlackboardSnapshot,
    DelegateSnapshot, DelegationSnapshot, ExternalDelegateSnapshot, MailboxSnapshot,
};
pub use stream::AgentRunStream;

/// Default per-turn LLM-step budget when a builder does not set one (§8.4).
pub(crate) const DEFAULT_MAX_STEPS: u32 = 8;
/// Default number of tool-call rounds allowed per turn when unset (§8.4).
pub(crate) const DEFAULT_MAX_TOOL_ROUNDS: u32 = 4;

/// A stateful, tool-using agent backed by one live [`DefaultAgentMachine`].
///
/// An `Agent` reuses a single machine (and therefore one
/// [`Conversation`]) across every
/// [`run`](Agent::run), so each call appends to the committed history and later
/// requests replay the full context. Build one with [`Agent::builder`]:
///
/// ```no_run
/// # async fn demo() -> Result<(), agent_lib::facade::FacadeError> {
/// use agent_lib::facade::{Agent, Approval, ProviderConfig};
/// use agent_lib::facade::tool::{Tool, ToolContext};
/// use serde_json::json;
///
/// let mut agent = Agent::builder()
///     .provider(ProviderConfig::openai_from_env()?)
///     .model("gpt-5.5")
///     .system("You are a concise weather assistant.")
///     .tool(Tool::function_with_schema(
///         "get_weather",
///         "Look up the current weather for a city.",
///         json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
///         |_ctx: ToolContext, args: serde_json::Value| async move {
///             let city = args.get("city").and_then(|v| v.as_str()).unwrap_or("?");
///             Ok::<_, std::convert::Infallible>(format!("{city}: sunny, 26C"))
///         },
///     ))
///     .approval(Approval::auto_allow())
///     .build()?;
///
/// let reply = agent.run("What is the weather in Shanghai?").await?;
/// println!("{}", reply.text());
/// # Ok(())
/// # }
/// ```
pub struct Agent {
    machine: DefaultAgentMachine,
    client: Arc<dyn LlmClient>,
    tools: Vec<Tool>,
    custom_registry: Option<Arc<dyn ToolRegistry>>,
    extra_declarations: Vec<ToolDecl>,
    approval: Arc<FacadeApproval>,
    /// An optional host-supplied interaction handler that replaces
    /// [`FacadeApproval`] as the scope's [`InteractionHandler`] when set (§19).
    ///
    /// When present it answers every interaction the machine pauses on — chiefly
    /// tool-call approvals — so a host can `await` a cross-process decision
    /// instead of resolving synchronously. When absent the run falls back to the
    /// conservative [`FacadeApproval`] behavior of Milestone 2.
    interaction_handler: Option<Arc<dyn InteractionHandler>>,
    ids: FacadeIds,
    delegates: Vec<LocalSubagent>,
    external_agents: Vec<ManagedExternalDelegate>,
    delegation: Delegation,
    /// The resolved collaboration substrate (config plus the live shared
    /// mailbox/blackboard/plan primitives), derived from the delegate topology
    /// or from an explicit [`Collaboration`] (`docs/facade-api.md` §14).
    collab: CollabState,
    /// The last-known data-only session facts for each managed external delegate,
    /// keyed by delegate name, refreshed after every `run_full` drive so a later
    /// [`snapshot`](Agent::snapshot) can persist them (§15.2).
    last_external_sessions: HashMap<String, RetainedExternalSession>,
}

impl std::fmt::Debug for Agent {
    /// Prints the registered tool names while treating the client and machine as
    /// opaque so no credential or large state is rendered.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Agent")
            .field("client", &"<dyn LlmClient>")
            .field(
                "tools",
                &self.tools.iter().map(Tool::name).collect::<Vec<_>>(),
            )
            .field("has_custom_registry", &self.custom_registry.is_some())
            .field(
                "has_interaction_handler",
                &self.interaction_handler.is_some(),
            )
            .field(
                "extra_declarations",
                &self
                    .extra_declarations
                    .iter()
                    .map(|declaration| declaration.name.as_str())
                    .collect::<Vec<_>>(),
            )
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
            .field("collaboration", &self.collab.config)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// Starts a fluent [`AgentBuilder`].
    #[must_use]
    pub fn builder() -> AgentBuilder {
        AgentBuilder::default()
    }

    /// Starts a fluent [`AgentWorkerBuilder`] for a data-first local subagent.
    ///
    /// Unlike [`builder`](Agent::builder), a worker needs no client or provider:
    /// it produces a [`LocalSubagent`] recipe whose live child runtime is
    /// assembled only when a delegation is fulfilled, and which inherits the
    /// supervisor's model by default (`docs/facade-api.md` §10.3, R4). Register
    /// the result with [`AgentBuilder::subagent`].
    #[must_use]
    pub fn worker() -> AgentWorkerBuilder {
        AgentWorkerBuilder::default()
    }

    /// Runs one agent turn and returns the minimal [`Reply`].
    ///
    /// This is a convenience wrapper over [`run_full`](Agent::run_full); see that
    /// method for the exact drive and error semantics.
    ///
    /// # Errors
    ///
    /// Returns any [`FacadeError`] produced by [`run_full`](Agent::run_full),
    /// including [`FacadeError::LoopLimitExceeded`] when the loop budget is
    /// exhausted before a final assistant message.
    pub async fn run(&mut self, input: impl IntoUserMessage) -> Result<Reply, FacadeError> {
        Ok(self.run_full(input).await?.reply)
    }

    /// Runs one agent turn and returns the full [`RunOutput`].
    ///
    /// The turn is driven by [`drain`]ing the held [`DefaultAgentMachine`]: the
    /// machine loops through LLM steps, tool calls, and approvals until it
    /// reaches a final assistant response or exhausts its loop budget. Tool
    /// execution, the approval policy, and the LLM client are all supplied
    /// through a per-run [`HandlerScope`]; the run-scoped
    /// [`FacadeToolRegistry`] is rebuilt each call so each tool sees the current
    /// run id, worktree, cancellation token, and trace handle.
    ///
    /// On success the committed turn's aggregated token usage and final stop
    /// reason are folded into the returned [`Reply`], and every
    /// [`Notification::ToolCallStarted`] / [`Notification::ToolCallFinished`] is
    /// projected into [`RunOutput::tool_calls`] and [`RunOutput::events`].
    /// Because the drive folds each LLM response into the Conversation rather
    /// than handing one back, [`RunOutput::response`] is always `None`.
    ///
    /// # Errors
    ///
    /// - [`FacadeError::Agent`] if input validation, the LLM client, a tool, the
    ///   Conversation, or the run context fails (any classified [`AgentError`]
    ///   surfaced by the machine).
    /// - [`FacadeError::LoopLimitExceeded`] if the effective per-turn step budget
    ///   is exhausted before a final assistant message.
    /// - [`FacadeError::DuplicateTool`] if the run-scoped registry rejects a
    ///   duplicate tool name (already validated at build, so this is defensive).
    ///
    /// A failed turn discards its uncommitted work inside the machine, so the
    /// `Agent` stays usable and its committed history is unchanged.
    pub async fn run_full(
        &mut self,
        input: impl IntoUserMessage,
    ) -> Result<RunOutput, FacadeError> {
        let message = input.into_user_message();

        // Rules-routed delegation routes the whole task to a delegate the model
        // never sees; a task that matches no rule falls through to the ordinary
        // supervisor drive (which advertises no delegate tools) (§13.2).
        if self.delegation.is_rules_routed() {
            let routed = self
                .delegation
                .route_task(&user_message_text(&message))
                .map(str::to_owned);
            if let Some(delegate_name) = routed {
                let task = user_message_text(&message);
                return self.run_rules_routed(delegate_name, task).await;
            }
        }

        // Dispatcher-routed delegation routes *every* task through the facade
        // cheap→verify→strong escalation loop, again without exposing any
        // delegate to the model (§13.3).
        if self.delegation.is_dispatcher_routed() {
            let task = user_message_text(&message);
            return self.run_dispatcher_routed(task).await;
        }

        let run_id = self.ids.run_id();
        let ctx = RunContext::new_root(
            run_id,
            BudgetLimits::unbounded(),
            self.ids.trace_root("agent-run"),
        );

        // The registry and scope are per-run: a tool must observe this turn's run
        // id, worktree, cancellation, and trace handle.
        let context = ToolContextParts {
            run_id,
            agent_id: self.machine.state().spec().id(),
            worktree: self.machine.state().spec().worktree().clone(),
            cancel: ctx.cancellation().clone(),
            trace: ctx.trace().clone(),
        };
        let registry = FacadeToolRegistry::new(
            self.tools.clone(),
            self.custom_registry.clone(),
            self.extra_declarations.clone(),
            context,
        )?;
        let registry: Arc<dyn ToolRegistry> = Arc::new(registry);

        let recorder = new_delegation_recorder();
        // Records each approval the drive pauses on so the non-streaming
        // `RunOutput.events` can surface an `ApprovalRequested`, matching what
        // the streaming path emits live (M2-1).
        let approvals: ApprovalRecorder = Arc::new(Mutex::new(Vec::new()));
        let scope = FacadeAgentScope {
            llm: LlmClientHandler::new(self.client.clone()),
            tool: DelegationToolHandler::new(
                ToolRegistryHandler::new(registry),
                self.delegation_route(),
                self.client.clone(),
                self.supervisor_model(),
                self.ids.clone(),
                recorder.clone(),
                self.approval.clone(),
                self.collab_bridge(),
            ),
            interaction: Arc::new(RecordingInteractionHandler {
                approval: self.approval.clone(),
                inner: self.interaction_handler(),
                recorder: approvals.clone(),
            }),
        };

        let agent_input = AgentInput::user_message(
            self.ids.turn_id(),
            self.ids.message_id(),
            message,
            self.ids.message_id(),
            self.ids.step_id(),
        )?;

        let done = drain(&mut self.machine, agent_input, &scope, None, &ctx).await?;
        let collected = collect_traces(done.notifications(), &recorder);
        // Recovered in fulfill order so a paused approval sits before the tool
        // lifecycle it gated (or at the tail when the tool never started).
        let recorded_approvals = approvals
            .lock()
            .expect("approval recorder poisoned")
            .clone();

        // Refresh the retained per-delegate external session facts so a later
        // snapshot can persist them (§15.2), then surface an external-delegate
        // denial as a run-level error (§9.2).
        for (name, session) in collected.external_sessions {
            self.last_external_sessions.insert(name, session);
        }
        if collected.external_approval_denied {
            return Err(FacadeError::ApprovalDenied);
        }

        match done.cursor() {
            LoopCursor::Done(_) => {
                let (text, usage, stop_reason) =
                    final_turn_summary(self.machine.state().conversation());
                let mut usage_summary = UsageSummary::from_supervisor(usage.clone());
                usage_summary.add_subagent(collected.subagent_usage);
                usage_summary.add_external(collected.external_usage);
                Ok(RunOutput {
                    reply: Reply::from_parts(text, Some(usage), stop_reason),
                    response: None,
                    usage: usage_summary,
                    tool_calls: collected.tool_calls,
                    delegations: collected.delegations,
                    artifacts: collected.artifacts,
                    events: weave_approval_events(collected.events, recorded_approvals),
                })
            }
            LoopCursor::Error(error) => Err(classify_error(error.message())),
            other => Err(FacadeError::Agent(AgentError::Other(format!(
                "agent run ended on a non-terminal cursor ({:?})",
                other.kind()
            )))),
        }
    }

    /// Returns the agent's live [`Conversation`] through a read-only view.
    ///
    /// The Conversation accumulates every committed turn, so this is the entry
    /// point for inspecting history between runs.
    #[must_use]
    pub const fn conversation(&self) -> &Conversation {
        self.machine.state().conversation()
    }

    /// Returns the agent's live [`AgentState`] through a read-only view.
    ///
    /// Use [`snapshot`](Agent::snapshot) to capture a serializable copy, or
    /// [`into_parts`](Agent::into_parts) to consume the agent and take ownership
    /// of the underlying state.
    #[must_use]
    pub const fn state(&self) -> &AgentState {
        self.machine.state()
    }

    /// Returns the local subagent delegates registered on this agent.
    ///
    /// Each entry is a data-first [`LocalSubagent`] recipe registered through
    /// [`AgentBuilder::subagent`]; the live child runtime is assembled only when
    /// a delegation is fulfilled (milestone M3-2). The base path exposes them for
    /// inspection and future delegation routing.
    #[must_use]
    pub fn subagents(&self) -> &[LocalSubagent] {
        &self.delegates
    }

    /// Returns the managed external agents registered as delegates on this agent.
    ///
    /// Each entry is a data-first [`ManagedExternalDelegate`] registered through
    /// [`AgentBuilder::external_agent`]; the live external runtime is driven only
    /// when a delegation is fulfilled (milestone M4-2), exposed to the
    /// supervising model as its own `ask_<name>` tool.
    #[must_use]
    pub fn external_agents(&self) -> &[ManagedExternalDelegate] {
        &self.external_agents
    }

    /// Returns the delegation routing strategy configured on this agent.
    ///
    /// Defaults to [`Delegation::model_routed`] (one `ask_<name>` tool per
    /// subagent) unless overridden through [`AgentBuilder::delegation`].
    #[must_use]
    pub const fn delegation(&self) -> &Delegation {
        &self.delegation
    }

    /// Returns the resolved collaboration substrate set for this agent.
    ///
    /// The set is derived from the delegate topology (`docs/facade-api.md` §14)
    /// unless an explicit [`Collaboration`] was supplied through
    /// [`AgentBuilder::collaboration`], in which case that value applies verbatim.
    #[must_use]
    pub const fn collaboration(&self) -> &Collaboration {
        &self.collab.config
    }

    /// Returns the shared [`Mailbox`] when the collaboration set enabled it.
    ///
    /// The returned handle is shared (`Arc`), so a caller, a delegate, or the
    /// external collab-event bridge all message through the one live inbox layer
    /// (`docs/facade-api.md` §14). Returns `None` when the mailbox is not enabled.
    #[must_use]
    pub fn mailbox(&self) -> Option<Arc<Mailbox>> {
        self.collab.mailbox.clone()
    }

    /// Returns the shared [`Blackboard`] when the collaboration set enabled it.
    ///
    /// The returned handle is shared (`Arc`). Returns `None` when the blackboard
    /// is not enabled.
    #[must_use]
    pub fn blackboard(&self) -> Option<Arc<Blackboard>> {
        self.collab.blackboard.clone()
    }

    /// Returns the shared [`Plan`] board when the collaboration set enabled it.
    ///
    /// The returned handle is shared (`Arc`). Returns `None` when the plan board
    /// is not enabled.
    #[must_use]
    pub fn plan(&self) -> Option<Arc<Plan>> {
        self.collab.plan.clone()
    }

    /// Builds the per-run [`DelegationRoute`] the run-scoped
    /// [`DelegationToolHandler`] (and the streaming tap) consult to recognize and
    /// dispatch delegation calls (§10.1, §10.2).
    fn delegation_route(&self) -> DelegationRoute {
        self.delegation
            .route(&self.delegates, &self.external_agents)
    }

    /// Builds the collab bridge a run-scoped [`DelegationToolHandler`] hands to a
    /// driven managed external delegate so its `send_message` / `plan_update` /
    /// `blackboard_post` observations reflect into the provisioned substrate
    /// (§14 末段). The bridge shares the same live primitives the accessors
    /// return, so a caller reading [`mailbox`](Self::mailbox) /
    /// [`blackboard`](Self::blackboard) / [`plan`](Self::plan) sees the bridged
    /// writes.
    fn collab_bridge(&self) -> CollabBridge {
        CollabBridge::from_state(&self.collab)
    }

    /// Returns the supervisor's own model, substituted into any inheriting child
    /// spec when a delegation is fulfilled (R4).
    fn supervisor_model(&self) -> ModelRef {
        self.machine.state().spec().model().clone()
    }

    /// Resolves the [`InteractionHandler`] a drive scope answers paused
    /// interactions with: the host-injected handler when one was supplied to
    /// [`AgentBuilder::interaction_handler`], otherwise the shared
    /// [`FacadeApproval`] fallback (§19).
    ///
    /// The machine gate ([`ToolApprovalPolicy`]) is always [`FacadeApproval`], so
    /// it still decides which tool calls pause and records the pending decision
    /// the streaming path peeks; only the *answer* to a paused interaction is
    /// delegated to the resolved handler here.
    fn interaction_handler(&self) -> Arc<dyn InteractionHandler> {
        match &self.interaction_handler {
            Some(handler) => handler.clone(),
            None => self.approval.clone(),
        }
    }

    /// Builds the per-run [`DelegationToolHandler`] used to drive a rules-routed
    /// delegation, wired with a fresh `recorder` and the run's identity, tools,
    /// client, model, and approval policy (§13.2).
    pub(crate) fn build_delegation_handler(
        &self,
        run_id: RunId,
        ctx: &RunContext,
        recorder: DelegationRecorder,
    ) -> Result<DelegationToolHandler, FacadeError> {
        let context = ToolContextParts {
            run_id,
            agent_id: self.machine.state().spec().id(),
            worktree: self.machine.state().spec().worktree().clone(),
            cancel: ctx.cancellation().clone(),
            trace: ctx.trace().clone(),
        };
        let registry = FacadeToolRegistry::new(
            self.tools.clone(),
            self.custom_registry.clone(),
            self.extra_declarations.clone(),
            context,
        )?;
        let registry: Arc<dyn ToolRegistry> = Arc::new(registry);
        Ok(DelegationToolHandler::new(
            ToolRegistryHandler::new(registry),
            self.delegation_route(),
            self.client.clone(),
            self.supervisor_model(),
            self.ids.clone(),
            recorder,
            self.approval.clone(),
            self.collab_bridge(),
        ))
    }

    /// Resolves a rules-routed delegate name to an owned drive target (§13.2).
    ///
    /// The delegate name is validated at build time, so an unregistered name
    /// here is a defensive [`FacadeError::InvalidState`].
    pub(crate) fn resolve_rules_target(
        &self,
        name: &str,
    ) -> Result<RulesRoutedTarget, FacadeError> {
        if let Some(subagent) = self.delegates.iter().find(|d| d.name() == name) {
            Ok(RulesRoutedTarget::Local(subagent.clone()))
        } else if let Some(delegate) = self.external_agents.iter().find(|d| d.name() == name) {
            Ok(RulesRoutedTarget::External(delegate.clone()))
        } else {
            Err(FacadeError::InvalidState(format!(
                "rules-routed delegate `{name}` is not registered"
            )))
        }
    }

    /// Routes one task to a rules-matched delegate, driving it to completion and
    /// assembling the terminal [`RunOutput`] (§13.2).
    ///
    /// The delegate is driven through the same delegation machinery a
    /// model-routed call uses, so its trace, usage, artifacts, and events match
    /// field for field. Unlike a model-routed turn there is no supervisor LLM
    /// step, so the supervisor's own usage is zero and the routed exchange is
    /// **not** folded into the supervisor [`Conversation`]; the delegation is
    /// reported entirely through the returned [`RunOutput`]. A managed external
    /// delegate that the approval policy denies fails the run with
    /// [`FacadeError::ApprovalDenied`] (§9.2), and its resumable session facts are
    /// retained for a later [`snapshot`](Agent::snapshot) (§15.2).
    async fn run_rules_routed(
        &mut self,
        delegate_name: String,
        task: String,
    ) -> Result<RunOutput, FacadeError> {
        let run_id = self.ids.run_id();
        let ctx = RunContext::new_root(
            run_id,
            BudgetLimits::unbounded(),
            self.ids.trace_root("agent-run"),
        );
        let recorder = new_delegation_recorder();
        let handler = self.build_delegation_handler(run_id, &ctx, recorder.clone())?;
        let target = self.resolve_rules_target(&delegate_name)?;

        let drive = drive_rules_routed(&handler, &recorder, &self.ids, &target, task, &ctx).await?;

        // Retain the external delegate's data-only session facts so a later
        // snapshot can persist them (§15.2), mirroring `run_full`.
        if drive.record.is_external {
            let status = match drive.record.trace.status {
                DelegationStatus::Completed => ExternalDelegateStatus::Completed,
                DelegationStatus::Failed => ExternalDelegateStatus::Failed,
            };
            self.last_external_sessions.insert(
                drive.record.trace.delegate.clone(),
                RetainedExternalSession {
                    status,
                    session: drive.record.session.clone(),
                    artifacts: drive.record.artifacts.clone(),
                },
            );
        }

        Ok(drive.output)
    }

    /// Resolves every delegate a dispatcher config references (primary, verifier,
    /// escalation target) to an owned drive target, keyed by registration name
    /// (§13.3).
    ///
    /// Each name is validated at build time, so an unregistered name here is a
    /// defensive [`FacadeError::InvalidState`]. The returned map is reused across
    /// worker attempts (a verifier runs once per attempt), so targets are cloned.
    pub(crate) fn resolve_dispatcher_targets(
        &self,
        config: &DispatcherConfig,
    ) -> Result<HashMap<String, RulesRoutedTarget>, FacadeError> {
        let mut targets = HashMap::new();
        for name in [config.primary()]
            .into_iter()
            .chain(config.verifier())
            .chain(config.escalate_to())
        {
            if name.is_empty() || targets.contains_key(name) {
                continue;
            }
            targets.insert(name.to_owned(), self.resolve_rules_target(name)?);
        }
        Ok(targets)
    }

    /// Routes one task through the dispatcher cheap→verify→strong escalation loop
    /// and assembles the terminal [`RunOutput`] (§13.3).
    ///
    /// The primary worker runs first; when a verifier is configured its verdict
    /// (or a worker's own failure) drives an escalation to the stronger worker,
    /// capped at the config's `max_attempts`. Each worker and verifier run is
    /// driven through the same delegation machinery a model-routed call uses, so
    /// traces, usage, artifacts, and events match field for field; the escalation
    /// *decision* is delegated to `agent::external::Escalator` (§19). As with
    /// rules-routed delegation there is no supervisor LLM step, so the
    /// supervisor's own usage is zero and the routed exchange is **not** folded
    /// into the supervisor [`Conversation`]. A managed external delegate the
    /// approval policy denies fails with [`FacadeError::ApprovalDenied`] (§9.2),
    /// and every external delegate's resumable session facts are retained for a
    /// later [`snapshot`](Agent::snapshot) (§15.2).
    async fn run_dispatcher_routed(&mut self, task: String) -> Result<RunOutput, FacadeError> {
        let config = self
            .delegation
            .dispatcher_config()
            .cloned()
            .ok_or_else(|| {
                FacadeError::InvalidState(
                    "dispatcher config missing on a dispatcher run".to_owned(),
                )
            })?;
        let run_id = self.ids.run_id();
        let ctx = RunContext::new_root(
            run_id,
            BudgetLimits::unbounded(),
            self.ids.trace_root("agent-run"),
        );
        let recorder = new_delegation_recorder();
        let handler = self.build_delegation_handler(run_id, &ctx, recorder.clone())?;
        let targets = self.resolve_dispatcher_targets(&config)?;
        let evaluator = self.delegation.dispatcher_evaluator_hook().cloned();
        let verifier = self.delegation.dispatcher_verifier_hook().cloned();

        let drive = drive_dispatcher_routed(
            &handler, &recorder, &self.ids, &config, &targets, task, &ctx, evaluator, verifier,
        )
        .await?;

        // Retain each external delegate's data-only session facts so a later
        // snapshot can persist them (§15.2), mirroring `run_full`.
        for record in &drive.records {
            if !record.is_external {
                continue;
            }
            let status = match record.trace.status {
                DelegationStatus::Completed => ExternalDelegateStatus::Completed,
                DelegationStatus::Failed => ExternalDelegateStatus::Failed,
            };
            self.last_external_sessions.insert(
                record.trace.delegate.clone(),
                RetainedExternalSession {
                    status,
                    session: record.session.clone(),
                    artifacts: record.artifacts.clone(),
                },
            );
        }

        Ok(drive.output)
    }

    /// The returned stream is the tool-using, approval-gated analog of
    /// [`ChatSession::stream`](crate::facade::ChatSession::stream). It forwards
    /// each incremental [`RunEvent::TextDelta`] as the assistant text arrives and
    /// each [`RunEvent::ToolStarted`] / [`RunEvent::ToolFinished`] /
    /// [`RunEvent::ApprovalRequested`] as the drive reaches it, then yields
    /// exactly one terminal [`RunEvent::Done`] carrying the complete
    /// [`RunOutput`]. That terminal `RunOutput` is built exactly as
    /// [`run_full`](Agent::run_full) builds it, so a streamed turn and a
    /// non-streamed turn agree field for field.
    ///
    /// Streaming is realized by a per-run LLM handler that always drives
    /// [`LlmClient::chat_stream`] and folds
    /// the deltas back into the same [`Response`](crate::client::Response) the
    /// machine consumes, so no new effect family is introduced and the held
    /// [`DefaultAgentMachine`] runs its ordinary loop. The turn is committed to
    /// this agent's [`Conversation`] only when the drive reaches a final
    /// assistant message; dropping the stream before completion leaves the
    /// agent's committed history unchanged.
    ///
    /// # Errors
    ///
    /// The `await` itself returns:
    ///
    /// - [`FacadeError::Agent`] if building the user input for the turn fails.
    /// - [`FacadeError::DuplicateTool`] if the run-scoped registry rejects a
    ///   duplicate tool name (already validated at build, so this is defensive).
    ///
    /// Failures observed while driving the turn (LLM transport errors, tool
    /// failures, an exhausted loop budget) are surfaced as an `Err` item yielded
    /// by the stream. On any such failure the in-flight turn is discarded inside
    /// the machine, so the agent stays usable and its committed history is
    /// unchanged.
    pub async fn stream(
        &mut self,
        input: impl IntoUserMessage,
    ) -> Result<AgentRunStream<'_>, FacadeError> {
        stream::start(self, input.into_user_message())
    }

    /// Captures a serializable [`AgentSnapshot`] of the supervisor state.
    ///
    /// The snapshot carries only data — the accumulated [`Conversation`] plus the
    /// serializable [`AgentState`] (spec, active tool-set declarations, model,
    /// loop policy, and loop cursor). It never contains the LLM client, provider
    /// credentials, tool closures, or the approval handler, so it is safe to
    /// persist and later feed to [`Agent::restore`]. When the agent has a
    /// collaboration substrate provisioned, the mailbox, blackboard, and plan
    /// slices carry that substrate's data-only snapshot (each `None` when its
    /// substrate is disabled); the delegate slice carries the registered subagent
    /// recipes, and the artifact slice is reserved for a later milestone
    /// (`docs/facade-api.md` §15.2).
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Conversation`] if an uncommitted turn is in flight
    /// (a [`ConversationSnapshot`](crate::conversation::ConversationSnapshot) is
    /// only available at a committed consistency point). In normal use each
    /// [`run`](Agent::run) commits before returning, so the agent rests at a
    /// snapshot-able point.
    pub fn snapshot(&self) -> Result<AgentSnapshot, FacadeError> {
        AgentSnapshot::capture(
            self.machine.state(),
            &self.delegates,
            &self.external_agents,
            &self.last_external_sessions,
            &self.delegation,
            &self.collab,
        )
    }

    /// Starts a fluent [`AgentRestoreBuilder`] that rebuilds an [`Agent`] from an
    /// [`AgentSnapshot`].
    ///
    /// A snapshot is data-only, so the restore builder re-injects the runtime
    /// handles a snapshot deliberately omits: the LLM client (through a
    /// [`provider`](AgentRestoreBuilder::provider) or an explicit
    /// [`client`](AgentRestoreBuilder::client)), the executable
    /// [`tool`](AgentRestoreBuilder::tool)s, and the
    /// [`approval`](AgentRestoreBuilder::approval) policy. The restored agent
    /// continues the snapshotted conversation, so the next [`run`](Agent::run)
    /// appends to that history.
    #[must_use]
    pub fn restore() -> AgentRestoreBuilder {
        AgentRestoreBuilder::default()
    }

    /// Consumes the agent and returns its internal parts as an escape hatch.
    ///
    /// This hands ownership of the underlying [`AgentState`] (which owns the live
    /// [`Conversation`]), the LLM client, the registered tools and escape-hatch
    /// declarations, the shared approval bridge, any injected
    /// [`InteractionHandler`], the identity source, the registered local
    /// subagent and managed external delegates, the delegation routing mode, the
    /// last-known external session facts, and the live collaboration substrate
    /// (config plus the shared mailbox / blackboard / plan handles) to an
    /// advanced caller who needs to drive the layers directly or take over the
    /// still-live handles (`docs/facade-api.md` §8.2). No semantically meaningful
    /// state is silently dropped: every field the agent held is surfaced on the
    /// returned [`AgentParts`].
    ///
    /// The facade never reclaims these parts, so the caller owns the assembled
    /// state after this call. This is a decomposition hatch, **not** a restore
    /// API: it returns the live parts as-is and offers no helper to reassemble an
    /// [`Agent`] from them. Use [`snapshot`](Agent::snapshot) /
    /// [`restore`](Agent::restore) for data-only persistence and rebuild, and
    /// [`builder`](Agent::builder) for ordinary construction; reach for
    /// `into_parts` only when a caller must take ownership of the live handles
    /// themselves (`docs/facade-api.md` §8.2).
    #[must_use]
    pub fn into_parts(self) -> AgentParts {
        let collab = self.collab;
        AgentParts {
            state: self.machine.into_state(),
            client: self.client,
            tools: self.tools,
            custom_registry: self.custom_registry,
            extra_declarations: self.extra_declarations,
            approval: self.approval,
            interaction_handler: self.interaction_handler,
            ids: self.ids,
            delegates: self.delegates,
            external_agents: self.external_agents,
            delegation: self.delegation,
            retained_external_sessions: self.last_external_sessions,
            collaboration: collab.config,
            mailbox: collab.mailbox,
            blackboard: collab.blackboard,
            plan: collab.plan,
        }
    }
}

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
    system: Option<String>,
    tools: Vec<Tool>,
    custom_registry: Option<Arc<dyn ToolRegistry>>,
    extra_declarations: Vec<ToolDecl>,
    approval: Option<ApprovalPolicy>,
    interaction_handler: Option<Arc<dyn InteractionHandler>>,
    max_steps: Option<u32>,
    max_tool_rounds: Option<u32>,
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
        let client = match (self.client, self.provider) {
            (Some(client), _) => client,
            (None, Some(provider)) => client_for_provider(provider),
            (None, None) => {
                return Err(FacadeError::Config(
                    "agent configuration needs either a `client` or a `provider`".to_owned(),
                ));
            }
        };

        // Reject duplicate tool names up front, before any machine is assembled.
        ensure_unique_tool_names(
            &self.tools,
            &self.extra_declarations,
            self.custom_registry.as_ref(),
        )?;

        let mut model = ModelConfig::new(model_name);
        if let Some(max_tokens) = self.max_tokens {
            model = model.max_tokens(max_tokens);
        }
        if let Some(temperature) = self.temperature {
            model = model.temperature(temperature);
        }

        let ids = self.ids.unwrap_or_default();
        let loop_policy = build_loop_policy(
            self.max_steps.unwrap_or(DEFAULT_MAX_STEPS),
            self.max_tool_rounds.unwrap_or(DEFAULT_MAX_TOOL_ROUNDS),
            self.tool_failure_policy
                .unwrap_or(ToolFailurePolicy::ReturnErrorToModel),
        );

        // The advertised tool set must mirror what the run-scoped
        // FacadeToolRegistry reports, so build it from the same three sources,
        // then append the delegation tool declarations the configured
        // `Delegation` mode advertises for the registered subagents: one
        // `ask_<name>` tool per delegate (model-routed, §10.1) or a single
        // unified `<name>(agent, task)` tool (§10.2).
        let delegation = self.delegation.unwrap_or_default();

        // Rules-routed delegation names its delegates by string; a name no agent
        // registered can never route, so reject it up front rather than failing
        // silently at run time (§13.2).
        if let Some(unknown) =
            delegation.first_unknown_rule_delegate(&self.delegates, &self.external_agents)
        {
            return Err(FacadeError::Config(format!(
                "rules-routed delegation references unregistered delegate `{unknown}`"
            )));
        }

        // Dispatcher-routed delegation likewise names its primary / verifier /
        // escalation delegates by string: a missing primary or an unregistered
        // name can never run, so reject both up front (§13.3).
        if let Some(config) = delegation.dispatcher_config() {
            if config.primary().is_empty() {
                return Err(FacadeError::Config(
                    "dispatcher-routed delegation is missing a `primary` delegate".to_owned(),
                ));
            }
            if let Some(unknown) =
                delegation.first_unknown_dispatcher_delegate(&self.delegates, &self.external_agents)
            {
                return Err(FacadeError::Config(format!(
                    "dispatcher-routed delegation references unregistered delegate `{unknown}`"
                )));
            }
        }

        let mut declarations: Vec<ToolDecl> = self.tools.iter().map(Tool::declaration).collect();
        declarations.extend(self.extra_declarations.iter().cloned());
        if let Some(custom) = &self.custom_registry {
            declarations.extend(custom.declarations());
        }
        declarations.extend(delegation.declarations(&self.delegates, &self.external_agents));

        // Reject any name collision the delegation tools introduce — two
        // delegates minting the same `ask_<name>`, or a delegation tool clashing
        // with a typed tool / escape-hatch declaration (§10.1). The base tool
        // sources were already checked above; this covers the delegation layer.
        ensure_unique_declaration_names(&declarations)?;

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
            &self.tools,
            external_tool_names,
        );

        let machine = assemble_machine(state, &ids, approval.clone());

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
            tools: self.tools,
            custom_registry: self.custom_registry,
            extra_declarations: self.extra_declarations,
            approval,
            interaction_handler: self.interaction_handler,
            ids,
            delegates: self.delegates,
            external_agents: self.external_agents,
            delegation,
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
fn build_facade_approval(
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
}

/// One total drain layer carrying the LLM client, the run-scoped tool registry,
/// and the resolved interaction handler.
///
/// The three accessors [`drain`] consults are provided; every other handler
/// family defaults to `None` because the facade never emits those requirements
/// (no reconfiguration, subagents, or host permissions on the base agent path).
/// The [`interaction`](Self::interaction) handler is the host-injected
/// [`InteractionHandler`] when one was supplied, otherwise the shared
/// [`FacadeApproval`] (§19).
struct FacadeAgentScope {
    llm: LlmClientHandler,
    tool: DelegationToolHandler,
    interaction: Arc<dyn InteractionHandler>,
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
}

/// An ordered, interior-mutable log of the approval requests a non-streaming
/// [`Agent::run_full`] drive paused on, filled in fulfill order by
/// [`RecordingInteractionHandler`].
type ApprovalRecorder = Arc<Mutex<Vec<ApprovalRequest>>>;

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
struct RecordingInteractionHandler {
    approval: Arc<FacadeApproval>,
    inner: Arc<dyn InteractionHandler>,
    recorder: ApprovalRecorder,
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
                .expect("approval recorder poisoned")
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
fn weave_approval_events(events: Vec<RunEvent>, approvals: Vec<ApprovalRequest>) -> Vec<RunEvent> {
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
                .position(|approval| approval.call_id == call_id)
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

/// Classifies an [`ErrorCursor`](crate::agent::ErrorCursor) message into a
/// [`FacadeError`].
///
/// The base machine reports an exhausted per-turn step budget with a stable
/// message; that maps to [`FacadeError::LoopLimitExceeded`], while any other
/// runtime error is preserved as [`FacadeError::Agent`].
fn classify_error(message: &str) -> FacadeError {
    if message.contains("loop step limit") {
        FacadeError::LoopLimitExceeded
    } else {
        FacadeError::Agent(AgentError::Other(message.to_owned()))
    }
}

/// The per-run traces and UI events projected from a drained turn.
pub(crate) struct CollectedTraces {
    /// Traces for ordinary (non-delegation) tool calls.
    pub tool_calls: Vec<ToolTrace>,
    /// Traces for delegation calls, recorded by the delegation handler.
    pub delegations: Vec<DelegationTrace>,
    /// Aggregate token usage reported by every driven local subagent.
    pub subagent_usage: crate::model::usage::Usage,
    /// Aggregate token usage reported by every driven managed external agent.
    pub external_usage: crate::model::usage::Usage,
    /// Artifacts (patches/diffs/files/test results) reported by external
    /// delegates, in the order their delegations completed.
    pub artifacts: Vec<ArtifactRef>,
    /// The ordered normalized events for the run.
    pub events: Vec<RunEvent>,
    /// Whether any managed external delegate was denied before it started by the
    /// approval policy (§9.2). The Agent facade folds this into a run-level
    /// [`FacadeError::ApprovalDenied`].
    pub external_approval_denied: bool,
    /// The last-known data-only session facts for each managed external delegate
    /// driven this run, keyed by delegate name, for snapshot retention (§15.2).
    pub external_sessions: HashMap<String, RetainedExternalSession>,
}

/// Projects the drained tool notifications into per-call traces and UI events,
/// splitting delegation calls out from ordinary tool calls.
///
/// A [`Notification::ToolCallStarted`] carries the tool name and framework call
/// id. When that call id was recorded as a delegation by the
/// [`DelegationToolHandler`], it seeds a [`DelegationTrace`] in `delegations`
/// (its child usage folded into `subagent_usage` for a local subagent or
/// `external_usage` for a managed external agent) and a
/// [`RunEvent::DelegationStarted`]; otherwise it seeds a [`ToolTrace`] and a
/// [`RunEvent::ToolStarted`]. A [`Notification::ToolCallFinished`] carries only
/// the call id, so its role is recovered from the same recorder / started map to
/// emit the matching finished (or failed) event; an external delegation that
/// completed also emits one [`RunEvent::DelegationArtifact`] per reported
/// artifact and folds those artifacts into the run output.
///
/// A `ToolCallFinished` whose call id was never seen as a `ToolCallStarted`
/// (and is not a delegation) is a call the approval gate denied before it ever
/// started: it emits **no** `ToolFinished`, so a denied tool leaves no tool
/// lifecycle event on the non-streaming path — exactly as on the streaming path,
/// where a denied call never reaches the tool handler. The paused approval is
/// still surfaced separately by [`weave_approval_events`].
pub(crate) fn collect_traces(
    notifications: &[Notification],
    recorder: &DelegationRecorder,
) -> CollectedTraces {
    let recorded = recorder
        .lock()
        .expect("delegation recorder poisoned")
        .clone();
    let mut tool_calls = Vec::new();
    let mut delegations = Vec::new();
    let mut subagent_usage = crate::model::usage::Usage::default();
    let mut external_usage = crate::model::usage::Usage::default();
    let mut artifacts = Vec::new();
    let mut events = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut external_approval_denied = false;
    let mut external_sessions: HashMap<String, RetainedExternalSession> = HashMap::new();

    for record in recorded.values() {
        if !record.is_external {
            continue;
        }
        if record.approval_denied {
            external_approval_denied = true;
        }
        let status = match record.trace.status {
            DelegationStatus::Completed => ExternalDelegateStatus::Completed,
            DelegationStatus::Failed => ExternalDelegateStatus::Failed,
        };
        external_sessions.insert(
            record.trace.delegate.clone(),
            RetainedExternalSession {
                status,
                session: record.session.clone(),
                artifacts: record.artifacts.clone(),
            },
        );
    }

    for notification in notifications {
        match notification {
            Notification::ToolCallStarted(started) => {
                let call_id = started.call_id().to_string();
                if let Some(record) = recorded.get(&call_id) {
                    delegations.push(record.trace.clone());
                    if record.is_external {
                        external_usage.merge(record.trace.usage.clone());
                    } else {
                        subagent_usage.merge(record.trace.usage.clone());
                    }
                    events.push(RunEvent::DelegationStarted(record.trace.clone()));
                } else {
                    let name = started.call().name.clone();
                    names.insert(call_id.clone(), name.clone());
                    let trace = ToolTrace { name, call_id };
                    tool_calls.push(trace.clone());
                    events.push(RunEvent::ToolStarted(trace));
                }
            }
            Notification::ToolCallFinished(finished) => {
                let call_id = finished.call_id().to_string();
                if let Some(record) = recorded.get(&call_id) {
                    match record.trace.status {
                        DelegationStatus::Completed => {
                            for artifact in &record.artifacts {
                                artifacts.push(artifact.clone());
                                events.push(RunEvent::DelegationArtifact(artifact.clone()));
                            }
                            events.push(RunEvent::DelegationFinished(record.trace.clone()));
                        }
                        DelegationStatus::Failed => {
                            events.push(RunEvent::DelegationFailed(record.trace.clone()));
                        }
                    }
                } else if let Some(name) = names.get(&call_id).cloned() {
                    events.push(RunEvent::ToolFinished(ToolTrace { name, call_id }));
                }
                // A `ToolCallFinished` with no recorded `ToolCallStarted` name
                // (and no delegation record) belongs to a call the approval gate
                // denied before it ever started: it produced no `ToolStarted`, so
                // it emits no `ToolFinished` either, keeping the non-streaming
                // path's tool lifecycle identical to the streaming path (which
                // never invokes the tool handler for a denied call). The paused
                // approval itself is still surfaced by `weave_approval_events`.
            }
            _ => {}
        }
    }

    CollectedTraces {
        tool_calls,
        delegations,
        subagent_usage,
        external_usage,
        artifacts,
        events,
        external_approval_denied,
        external_sessions,
    }
}

/// The outcome of one rules-routed delegation drive (`docs/facade-api.md` §13.2).
///
/// Shared by [`Agent::run_full`] and the streaming path: the [`RunOutput`] is the
/// terminal result to return (or yield as `Done`), while the
/// [`RecordedDelegation`] lets the caller retain an external delegate's session
/// facts (§15.2).
pub(crate) struct RulesRoutedDrive {
    /// The terminal run output assembled from the drive.
    pub output: RunOutput,
    /// The recorded delegation (trace, artifacts, session, denial flag).
    pub record: RecordedDelegation,
}

/// Drives one rules-routed delegation and assembles its terminal output.
///
/// The delegate is driven through the shared [`DelegationToolHandler`] using a
/// framework call id minted from `ids`, then its recorded trace, usage, and
/// artifacts are projected into a single-delegation [`RunOutput`]. A managed
/// external delegate the approval policy denied fails with
/// [`FacadeError::ApprovalDenied`] (§9.2).
pub(crate) async fn drive_rules_routed(
    handler: &DelegationToolHandler,
    recorder: &DelegationRecorder,
    ids: &FacadeIds,
    target: &RulesRoutedTarget,
    task: String,
    ctx: &RunContext,
) -> Result<RulesRoutedDrive, FacadeError> {
    let (record, summary) = run_one_delegation(handler, recorder, ids, target, task, ctx).await?;

    // A denied external delegate surfaces as a run-level error, matching the
    // model-routed path (§9.2).
    if record.approval_denied {
        return Err(FacadeError::ApprovalDenied);
    }

    let output = build_rules_routed_output(&record, summary);
    Ok(RulesRoutedDrive { output, record })
}

/// Drives one delegate through the shared [`DelegationToolHandler`] and returns
/// its recorded trace plus the folded summary text.
///
/// The delegate is fulfilled under a framework call id minted from `ids`; the
/// resulting [`RecordedDelegation`] is read back from `recorder` and the summary
/// is extracted from the tool result (or the classified error on failure). The
/// caller decides how to treat an approval denial or a failed status; this
/// helper never short-circuits so a dispatcher loop can inspect every run.
async fn run_one_delegation(
    handler: &DelegationToolHandler,
    recorder: &DelegationRecorder,
    ids: &FacadeIds,
    target: &RulesRoutedTarget,
    task: String,
    ctx: &RunContext,
) -> Result<(RecordedDelegation, String), FacadeError> {
    let call_id = ids.fresh_tool_call_id();
    let key = call_id.to_string();
    let result = handler
        .fulfill_rules_routed(call_id, target, task, ctx)
        .await;

    let record = recorder
        .lock()
        .expect("delegation recorder poisoned")
        .get(&key)
        .cloned()
        .ok_or_else(|| {
            FacadeError::InvalidState("facade-routed delegation was not recorded".to_owned())
        })?;

    let summary = rules_routed_summary(&result);
    Ok((record, summary))
}

/// Projects a single recorded rules-routed delegation into a [`RunOutput`].
///
/// The supervisor took no LLM step, so its usage is zero and the delegate's
/// usage is attributed to the subagent or external slice; the delegation trace,
/// artifacts, and bracketing events mirror a model-routed delegation exactly.
fn build_rules_routed_output(record: &RecordedDelegation, summary: String) -> RunOutput {
    let mut events = vec![RunEvent::DelegationStarted(record.trace.clone())];
    let mut usage = UsageSummary::from_supervisor(crate::model::usage::Usage::default());
    if record.is_external {
        usage.add_external(record.trace.usage.clone());
    } else {
        usage.add_subagent(record.trace.usage.clone());
    }
    match record.trace.status {
        DelegationStatus::Completed => {
            for artifact in &record.artifacts {
                events.push(RunEvent::DelegationArtifact(artifact.clone()));
            }
            events.push(RunEvent::DelegationFinished(record.trace.clone()));
        }
        DelegationStatus::Failed => {
            events.push(RunEvent::DelegationFailed(record.trace.clone()));
        }
    }
    RunOutput {
        reply: Reply::from_parts(summary, Some(crate::model::usage::Usage::default()), None),
        response: None,
        usage,
        tool_calls: Vec::new(),
        delegations: vec![record.trace.clone()],
        artifacts: record.artifacts.clone(),
        events,
    }
}

/// Extracts the delegate's summary text (or, on failure, its classified error
/// message) from a fulfilled rules-routed delegation.
fn rules_routed_summary(result: &RequirementResult) -> String {
    match result {
        RequirementResult::Tool(Ok(response)) => response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        RequirementResult::Tool(Err(error)) => error.to_string(),
        _ => String::new(),
    }
}

/// A shared capability tag for the two workers in a dispatcher roster.
///
/// The facade dispatcher is a fixed two-tier cheap→strong loop rather than a
/// capability-routed roster, so both workers advertise the same provider-neutral
/// [`Capability::Custom`] tag; the escalation decision turns purely on cost tier
/// and the primary worker's configured escalation target.
fn dispatcher_capability() -> Capability {
    Capability::Custom("dispatch".to_owned())
}

/// The outcome of one dispatcher-routed drive (`docs/facade-api.md` §13.3).
///
/// Shared by [`Agent::run_full`] and the streaming path: `output` is the
/// terminal result to return (or yield as `Done`), while `records` carries every
/// worker/verifier [`RecordedDelegation`] so the caller can retain each external
/// delegate's session facts (§15.2).
pub(crate) struct DispatcherDrive {
    /// The terminal run output assembled from the loop.
    pub output: RunOutput,
    /// Every delegation recorded during the loop, in run order.
    pub records: Vec<RecordedDelegation>,
}

/// Accumulates the ordered [`RunOutput`] pieces of a dispatcher loop.
#[derive(Default)]
struct DispatcherAccumulator {
    events: Vec<RunEvent>,
    delegations: Vec<DelegationTrace>,
    artifacts: Vec<ArtifactRef>,
    usage: UsageSummary,
    records: Vec<RecordedDelegation>,
}

impl DispatcherAccumulator {
    /// Folds one recorded delegation into the accumulator, appending its
    /// bracketing events, trace, artifacts, usage, and record exactly as a
    /// model- or rules-routed delegation would report them.
    fn record(&mut self, record: &RecordedDelegation) {
        self.events
            .push(RunEvent::DelegationStarted(record.trace.clone()));
        if record.is_external {
            self.usage.add_external(record.trace.usage.clone());
        } else {
            self.usage.add_subagent(record.trace.usage.clone());
        }
        match record.trace.status {
            DelegationStatus::Completed => {
                for artifact in &record.artifacts {
                    self.artifacts.push(artifact.clone());
                    self.events
                        .push(RunEvent::DelegationArtifact(artifact.clone()));
                }
                self.events
                    .push(RunEvent::DelegationFinished(record.trace.clone()));
            }
            DelegationStatus::Failed => {
                self.events
                    .push(RunEvent::DelegationFailed(record.trace.clone()));
            }
        }
        self.delegations.push(record.trace.clone());
        self.records.push(record.clone());
    }
}

/// Drives one task through the dispatcher cheap→verify→strong escalation loop
/// and assembles its terminal output (`docs/facade-api.md` §13.3).
///
/// The primary worker runs first; when a verifier is configured its verdict (or
/// a worker's own failure) escalates to the stronger worker, capped at
/// `config.max_attempts`. The escalation *decision* is delegated to
/// `agent::external::Escalator` (§19). A managed external delegate the approval
/// policy denies fails with [`FacadeError::ApprovalDenied`] (§9.2).
///
/// `evaluator` and `verifier` are the optional host-injected AI-routing /
/// AI-verification seams (§19). When `verifier` is present it both backs the
/// [`Escalator`] and is consulted after each worker run as an additional verdict
/// source (rejecting composes with worker failure and the verifier delegate's
/// token). When `evaluator` is present it chooses the escalation target instead
/// of the built-in roster logic. Both absent reproduces Milestone 5 exactly.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_dispatcher_routed(
    handler: &DelegationToolHandler,
    recorder: &DelegationRecorder,
    ids: &FacadeIds,
    config: &DispatcherConfig,
    targets: &HashMap<String, RulesRoutedTarget>,
    task: String,
    ctx: &RunContext,
    evaluator: Option<SharedTaskEvaluator>,
    verifier: Option<SharedVerifier>,
) -> Result<DispatcherDrive, FacadeError> {
    // An injected Verifier replaces the built-in inert ScriptedVerifier::passing()
    // seam inside the Escalator (§19); absent one the engine behaves as M5.
    let escalation_verifier: SharedVerifier = verifier
        .clone()
        .unwrap_or_else(|| Arc::new(ScriptedVerifier::passing()));
    let escalator = Escalator::new(escalation_verifier).with_budget_headroom(0);
    let roster = build_dispatcher_roster(config, ids);

    let mut acc = DispatcherAccumulator::default();
    let mut final_summary = String::new();
    let mut current = config.primary().to_owned();

    for attempt in 1..=config.max_attempts() {
        let worker = fetch_target(targets, &current)?;
        let (record, summary) =
            run_one_delegation(handler, recorder, ids, worker, task.clone(), ctx).await?;
        if record.approval_denied {
            return Err(FacadeError::ApprovalDenied);
        }
        acc.record(&record);
        final_summary = summary.clone();
        let worker_failed = record.trace.status == DelegationStatus::Failed;

        // A clean worker run that the verifier delegate (if any) and any injected
        // verifier both accept ends the loop.
        let rejected = worker_failed
            || run_verifier(
                handler, recorder, ids, config, targets, &task, &summary, ctx, &mut acc,
            )
            .await?
            || injected_verifier_rejects(verifier.as_ref(), &current, worker_failed);
        if !rejected {
            break;
        }

        // Rejected: escalate to the stronger worker while attempts remain and the
        // routing decision offers a target.
        if attempt >= config.max_attempts() {
            break;
        }
        let next = match evaluator.as_ref() {
            Some(evaluator) => {
                injected_escalation_target(evaluator.as_ref(), roster.as_ref(), targets, &current)
            }
            None => dispatcher_escalation_target(&escalator, roster.as_ref(), &current, ctx, ids)?,
        };
        let Some(next) = next else {
            break;
        };
        acc.events.push(RunEvent::Escalated(EscalationTrace {
            from: current.clone(),
            to: next.clone(),
        }));
        current = next;
    }

    let output = RunOutput {
        reply: Reply::from_parts(
            final_summary,
            Some(crate::model::usage::Usage::default()),
            None,
        ),
        response: None,
        usage: acc.usage,
        tool_calls: Vec::new(),
        delegations: acc.delegations,
        artifacts: acc.artifacts,
        events: acc.events,
    };
    Ok(DispatcherDrive {
        output,
        records: acc.records,
    })
}

/// Runs the configured verifier (if any) against a worker's `summary`, folding
/// its delegation into `acc`, and returns whether it requests an escalation.
///
/// A verifier rejects when its delegation fails or its reply carries the
/// [`DISPATCHER_ESCALATE_MARKER`] token (§13.3). With no verifier configured a
/// clean worker run is always accepted.
#[allow(clippy::too_many_arguments)]
async fn run_verifier(
    handler: &DelegationToolHandler,
    recorder: &DelegationRecorder,
    ids: &FacadeIds,
    config: &DispatcherConfig,
    targets: &HashMap<String, RulesRoutedTarget>,
    task: &str,
    worker_summary: &str,
    ctx: &RunContext,
    acc: &mut DispatcherAccumulator,
) -> Result<bool, FacadeError> {
    let Some(verifier_name) = config.verifier() else {
        return Ok(false);
    };
    let verifier = fetch_target(targets, verifier_name)?;
    let brief = verifier_brief(task, worker_summary);
    let (record, summary) =
        run_one_delegation(handler, recorder, ids, verifier, brief, ctx).await?;
    if record.approval_denied {
        return Err(FacadeError::ApprovalDenied);
    }
    let failed = record.trace.status == DelegationStatus::Failed;
    acc.record(&record);
    Ok(failed || verifier_requests_escalation(&summary))
}

/// Looks up a dispatcher target by name, erroring defensively if it is missing
/// (names are validated at build time, §13.3).
fn fetch_target<'a>(
    targets: &'a HashMap<String, RulesRoutedTarget>,
    name: &str,
) -> Result<&'a RulesRoutedTarget, FacadeError> {
    targets.get(name).ok_or_else(|| {
        FacadeError::InvalidState(format!("dispatcher delegate `{name}` is not registered"))
    })
}

/// Builds the verifier's task brief: the original task plus the worker's output
/// and the escalation-token protocol the facade interprets (§13.3).
fn verifier_brief(task: &str, worker_summary: &str) -> String {
    format!(
        "Review the following worker output for the task and decide whether it is acceptable.\n\n\
         Task:\n{task}\n\nWorker output:\n{worker_summary}\n\n\
         If the work is insufficient and must be redone by a stronger worker, reply with the word \
         ESCALATE; otherwise approve it."
    )
}

/// Reports whether a verifier's reply requests an escalation, i.e. contains the
/// case-insensitive [`DISPATCHER_ESCALATE_MARKER`] token (§13.3).
fn verifier_requests_escalation(summary: &str) -> bool {
    summary.to_lowercase().contains(DISPATCHER_ESCALATE_MARKER)
}

/// Builds the two-worker escalation roster for a dispatcher config, or `None`
/// when no escalation target is configured (nothing to escalate to).
///
/// The primary is registered [`CostTier::Cheap`] with an escalation rule pointing
/// at the stronger worker; the stronger worker is [`CostTier::Premium`] and
/// terminal. This is exactly the shape `agent::external::Escalator::assess`
/// resolves an upward escalation from.
fn build_dispatcher_roster(config: &DispatcherConfig, ids: &FacadeIds) -> Option<WorkerRoster> {
    let strong = config.escalate_to()?;
    let mut roster = WorkerRoster::new();
    let capability = dispatcher_capability();
    let spec = AgentSpecRef(ids.agent_id());

    roster.register(
        WorkerProfile::new(
            strong,
            [capability.clone()],
            CostTier::Premium,
            EscalationRules::none(),
        ),
        spec,
    );
    roster.register(
        WorkerProfile::new(
            config.primary(),
            [capability],
            CostTier::Cheap,
            EscalationRules::new(
                [
                    EscalationTrigger::ReviewRejected,
                    EscalationTrigger::TestFailure,
                    EscalationTrigger::Timeout,
                    EscalationTrigger::LowConfidence,
                ],
                Some(WorkerProfileRef::new(strong)),
                false,
            ),
        ),
        spec,
    );
    Some(roster)
}

/// Builds the provider-neutral [`TaskDescriptor`] the facade uses when it asks
/// the escalation engine or an injected hook to weigh a dispatcher task.
fn dispatcher_task_descriptor() -> TaskDescriptor {
    TaskDescriptor::new(
        dispatcher_capability(),
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    )
}

/// Asks an injected [`Verifier`] whether the worker `current` just produced an
/// output that should be rejected (§19), returning `false` when no verifier is
/// injected so the Milestone 5 verdict is preserved.
///
/// The verifier is consulted directly (not gated on
/// [`TaskDescriptor::warrants_verification`]) because the host injected it
/// deliberately. A `worker_failed` run is reported as a failing
/// [`WorkerReport`] so a verifier can key off the failure; otherwise a clean
/// report is passed and the verdict is entirely the verifier's.
fn injected_verifier_rejects(
    verifier: Option<&SharedVerifier>,
    current: &str,
    worker_failed: bool,
) -> bool {
    let Some(verifier) = verifier else {
        return false;
    };
    let descriptor = dispatcher_task_descriptor();
    let worker = WorkerProfileRef::new(current);
    let report = if worker_failed {
        WorkerReport::failed(worker, EscalationTrigger::ReviewRejected)
    } else {
        WorkerReport::succeeded(worker)
    };
    verifier.verify(&descriptor, &report).is_some()
}

/// Asks an injected [`TaskEvaluator`] which worker a rejected task escalates to
/// (§19), returning the target delegate name or `None` to decline.
///
/// The evaluator picks from the dispatcher roster (primary plus the configured
/// escalation target). A `None` roster (no escalation configured), an evaluator
/// that declines, or one that names the `current` worker or a delegate that is
/// not registered all mean "do not escalate".
fn injected_escalation_target(
    evaluator: &(dyn TaskEvaluator + Send + Sync),
    roster: Option<&WorkerRoster>,
    targets: &HashMap<String, RulesRoutedTarget>,
    current: &str,
) -> Option<String> {
    let roster = roster?;
    let descriptor = dispatcher_task_descriptor();
    let choice = evaluator.evaluate(&descriptor, roster)?;
    let name = choice.id();
    if name == current || !targets.contains_key(name) {
        return None;
    }
    Some(name.to_owned())
}

/// Asks `agent::external::Escalator` which stronger worker to escalate to after
/// `current` was rejected, returning the target delegate name or `None`.
///
/// A `None` roster (no escalation configured), an escalation the engine declines
/// (`Accept` / `Human` / `Exhausted`), or a `current` worker the roster does not
/// know all mean "do not escalate".
fn dispatcher_escalation_target<V: Verifier>(
    escalator: &Escalator<V>,
    roster: Option<&WorkerRoster>,
    current: &str,
    ctx: &RunContext,
    ids: &FacadeIds,
) -> Result<Option<String>, FacadeError> {
    let Some(roster) = roster else {
        return Ok(None);
    };
    let report = WorkerReport::failed(
        WorkerProfileRef::new(current),
        EscalationTrigger::ReviewRejected,
    );
    let descriptor = dispatcher_task_descriptor();
    let gate = HumanGate::new(ids.step_id(), ids.agent_id());
    match escalator.assess(&descriptor, &report, roster, ctx, &gate) {
        Ok(EscalationOutcome::Reassign(choice)) => Ok(Some(choice.worker().id().to_owned())),
        Ok(_) => Ok(None),
        // The `current` worker is not in the roster (e.g. already the strong
        // worker, whose profile is terminal): nothing further to escalate to.
        Err(EscalationError::UnknownWorker { .. }) => Ok(None),
        Err(error) => Err(FacadeError::InvalidState(error.to_string())),
    }
}

/// Concatenates the text of every [`ContentBlock::Text`] block in a user
/// message, so a rules-routed delegation can match keywords against it (§13.2).
pub(crate) fn user_message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extracts the final assistant text, aggregated usage, and last stop reason of
/// the most recently committed turn.
pub(crate) fn final_turn_summary(
    conversation: &Conversation,
) -> (
    String,
    crate::model::usage::Usage,
    Option<crate::model::normalized::StopReason>,
) {
    let Some(turn) = conversation.turns().last() else {
        return (String::new(), crate::model::usage::Usage::default(), None);
    };
    let text = turn
        .messages()
        .last()
        .map(|message| {
            message
                .payload()
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let usage = turn.meta().usage().clone();
    let stop_reason = turn
        .meta()
        .responses()
        .last()
        .map(|response| response.stop_reason().value);
    (text, usage, stop_reason)
}

#[cfg(test)]
mod tests;
