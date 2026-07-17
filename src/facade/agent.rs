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
use std::sync::Arc;

use crate::agent::{
    AgentError, AgentInput, AgentSpec, AgentState, BudgetLimits, DefaultAgentMachine, HandlerScope,
    InteractionHandler, LlmClientHandler, LlmHandler, LlmStepMode, LoopCursor, LoopPolicy,
    ModelRef, Notification, RequirementIds, RunContext, ToolApprovalPolicy, ToolExecutionIds,
    ToolFailurePolicy, ToolHandler, ToolRegistry, ToolRegistryHandler, ToolSetRef, WorktreeRef,
    drain,
};
use crate::client::LlmClient;
use crate::conversation::{Conversation, ConversationConfig};
use crate::facade::approval::{ApprovalPolicy, FacadeApproval};
use crate::facade::chat::client_for_provider;
use crate::facade::config::{ModelConfig, ProviderConfig};
use crate::facade::delegate::{
    AgentWorkerBuilder, DelegationRecorder, DelegationToolHandler, LocalSubagent,
    delegation_declaration, delegation_tool_name, new_delegation_recorder,
};
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::{
    DelegationStatus, DelegationTrace, IntoUserMessage, Reply, RunEvent, RunOutput, ToolTrace,
    UsageSummary,
};
use crate::facade::tool::{FacadeToolRegistry, Tool, ToolContextParts, ensure_unique_tool_names};
use crate::model::content::ContentBlock;
use crate::model::tool::Tool as ToolDecl;

mod snapshot;
mod stream;

pub use snapshot::{
    AgentParts, AgentRestoreBuilder, AgentSnapshot, AgentStateSnapshot, BlackboardSnapshot,
    DelegateSnapshot, DelegationSnapshot, MailboxSnapshot,
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
    ids: FacadeIds,
    delegates: Vec<LocalSubagent>,
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
        let scope = FacadeAgentScope {
            llm: LlmClientHandler::new(self.client.clone()),
            tool: DelegationToolHandler::new(
                ToolRegistryHandler::new(registry),
                self.delegate_table(),
                self.client.clone(),
                self.supervisor_model(),
                self.ids.clone(),
                recorder.clone(),
            ),
            interaction: self.approval.clone(),
        };

        let agent_input = AgentInput::user_message(
            self.ids.turn_id(),
            self.ids.message_id(),
            input.into_user_message(),
            self.ids.message_id(),
            self.ids.step_id(),
        )?;

        let done = drain(&mut self.machine, agent_input, &scope, None, &ctx).await?;
        let collected = collect_traces(done.notifications(), &recorder);

        match done.cursor() {
            LoopCursor::Done(_) => {
                let (text, usage, stop_reason) =
                    final_turn_summary(self.machine.state().conversation());
                let mut usage_summary = UsageSummary::from_supervisor(usage.clone());
                usage_summary.add_subagent(collected.subagent_usage);
                Ok(RunOutput {
                    reply: Reply::from_parts(text, Some(usage), stop_reason),
                    response: None,
                    usage: usage_summary,
                    tool_calls: collected.tool_calls,
                    delegations: collected.delegations,
                    artifacts: Vec::new(),
                    events: collected.events,
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

    /// Builds the per-run delegation lookup keyed by each delegate's synthesized
    /// `ask_<name>` tool name (§10.1).
    ///
    /// Shared behind an [`Arc`] so the run-scoped [`DelegationToolHandler`] and
    /// the streaming tap can consult the same table without re-cloning the
    /// delegate recipes on every tool call.
    fn delegate_table(&self) -> Arc<HashMap<String, LocalSubagent>> {
        Arc::new(
            self.delegates
                .iter()
                .map(|delegate| (delegation_tool_name(delegate.name()), delegate.clone()))
                .collect(),
        )
    }

    /// Returns the supervisor's own model, substituted into any inheriting child
    /// spec when a delegation is fulfilled (R4).
    fn supervisor_model(&self) -> ModelRef {
        self.machine.state().spec().model().clone()
    }

    /// Runs one agent turn as an incremental [`AgentRunStream`].
    ///
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
    /// persist and later feed to [`Agent::restore`]. Delegate, mailbox,
    /// blackboard, plan, and artifact slices are reserved for later milestones
    /// and are empty here (`docs/facade-api.md` §15.2).
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Conversation`] if an uncommitted turn is in flight
    /// (a [`ConversationSnapshot`](crate::conversation::ConversationSnapshot) is
    /// only available at a committed consistency point). In normal use each
    /// [`run`](Agent::run) commits before returning, so the agent rests at a
    /// snapshot-able point.
    pub fn snapshot(&self) -> Result<AgentSnapshot, FacadeError> {
        AgentSnapshot::capture(self.machine.state())
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
    /// declarations, the shared approval bridge, and the identity source to an
    /// advanced caller who needs to drive the layers directly
    /// (`docs/facade-api.md` §8.2). The facade never reclaims these parts, so the
    /// caller owns the assembled state after this call.
    #[must_use]
    pub fn into_parts(self) -> AgentParts {
        AgentParts {
            state: self.machine.into_state(),
            client: self.client,
            tools: self.tools,
            custom_registry: self.custom_registry,
            extra_declarations: self.extra_declarations,
            approval: self.approval,
            ids: self.ids,
            delegates: self.delegates,
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
    max_steps: Option<u32>,
    max_tool_rounds: Option<u32>,
    tool_failure_policy: Option<ToolFailurePolicy>,
    worktree: Option<WorktreeRef>,
    ids: Option<FacadeIds>,
    delegates: Vec<LocalSubagent>,
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

    /// Finalizes the builder into an [`Agent`], assembling the §8.3 machine stack.
    ///
    /// # Errors
    ///
    /// - [`FacadeError::Config`] when no model was set, or when neither an
    ///   explicit client nor a provider was supplied.
    /// - [`FacadeError::DuplicateTool`] when a tool name is declared more than
    ///   once across the typed tools, the escape-hatch declarations, and the
    ///   custom registry.
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
        // then append one synthesized delegation tool per registered subagent so
        // the supervising model can route work to a child (§10.1).
        let mut declarations: Vec<ToolDecl> = self.tools.iter().map(Tool::declaration).collect();
        declarations.extend(self.extra_declarations.iter().cloned());
        if let Some(custom) = &self.custom_registry {
            declarations.extend(custom.declarations());
        }
        for delegate in &self.delegates {
            declarations.push(delegation_declaration(
                delegate.name(),
                delegate.description(),
            ));
        }

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
        // pending-decision map through a single Arc.
        let approval = build_facade_approval(self.approval.unwrap_or_default(), &self.tools);

        let machine = assemble_machine(state, &ids, approval.clone());

        Ok(Agent {
            machine,
            client,
            tools: self.tools,
            custom_registry: self.custom_registry,
            extra_declarations: self.extra_declarations,
            approval,
            ids,
            delegates: self.delegates,
        })
    }
}

/// Builds the shared [`FacadeApproval`] bridge from an agent-level policy and the
/// per-tool overrides carried on each typed [`Tool`].
///
/// A tool-level [`Approval`](crate::facade::Approval) override wins over the
/// agent-level entry for the same name (`docs/facade-api.md` §9.1). The returned
/// value is shared behind one [`Arc`] so the machine (as
/// [`ToolApprovalPolicy`]) and the drive scope (as [`InteractionHandler`])
/// observe the same pending-decision map.
fn build_facade_approval(policy: ApprovalPolicy, tools: &[Tool]) -> Arc<FacadeApproval> {
    let mut approval = FacadeApproval::new(policy);
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
/// and the shared [`FacadeApproval`] interaction handler.
///
/// The three accessors [`drain`] consults are provided; every other handler
/// family defaults to `None` because the facade never emits those requirements
/// (no reconfiguration, subagents, or host permissions on the base agent path).
struct FacadeAgentScope {
    llm: LlmClientHandler,
    tool: DelegationToolHandler,
    interaction: Arc<FacadeApproval>,
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
    /// Aggregate token usage reported by every driven child.
    pub subagent_usage: crate::model::usage::Usage,
    /// The ordered normalized events for the run.
    pub events: Vec<RunEvent>,
}

/// Projects the drained tool notifications into per-call traces and UI events,
/// splitting delegation calls out from ordinary tool calls.
///
/// A [`Notification::ToolCallStarted`] carries the tool name and framework call
/// id. When that call id was recorded as a delegation by the
/// [`DelegationToolHandler`], it seeds a [`DelegationTrace`] in `delegations`
/// (its child usage folded into `subagent_usage`) and a
/// [`RunEvent::DelegationStarted`]; otherwise it seeds a [`ToolTrace`] and a
/// [`RunEvent::ToolStarted`]. A [`Notification::ToolCallFinished`] carries only
/// the call id, so its role is recovered from the same recorder / started map to
/// emit the matching finished (or failed) event.
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
    let mut events = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();

    for notification in notifications {
        match notification {
            Notification::ToolCallStarted(started) => {
                let call_id = started.call_id().to_string();
                if let Some(trace) = recorded.get(&call_id) {
                    delegations.push(trace.clone());
                    subagent_usage.merge(trace.usage.clone());
                    events.push(RunEvent::DelegationStarted(trace.clone()));
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
                if let Some(trace) = recorded.get(&call_id) {
                    match trace.status {
                        DelegationStatus::Completed => {
                            events.push(RunEvent::DelegationFinished(trace.clone()));
                        }
                        DelegationStatus::Failed => {
                            events.push(RunEvent::DelegationFailed(trace.clone()));
                        }
                    }
                } else {
                    let name = names.get(&call_id).cloned().unwrap_or_default();
                    events.push(RunEvent::ToolFinished(ToolTrace { name, call_id }));
                }
            }
            _ => {}
        }
    }

    CollectedTraces {
        tool_calls,
        delegations,
        subagent_usage,
        events,
    }
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
