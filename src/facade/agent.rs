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
    Notification, RequirementIds, RunContext, ToolApprovalPolicy, ToolExecutionIds,
    ToolFailurePolicy, ToolHandler, ToolRegistry, ToolRegistryHandler, ToolSetRef, WorktreeRef,
    drain,
};
use crate::client::LlmClient;
use crate::conversation::{Conversation, ConversationConfig};
use crate::facade::approval::{ApprovalPolicy, FacadeApproval};
use crate::facade::chat::client_for_provider;
use crate::facade::config::{ModelConfig, ProviderConfig};
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::{IntoUserMessage, Reply, RunEvent, RunOutput, ToolTrace, UsageSummary};
use crate::facade::tool::{FacadeToolRegistry, Tool, ToolContextParts, ensure_unique_tool_names};
use crate::model::content::ContentBlock;
use crate::model::tool::Tool as ToolDecl;

/// Default per-turn LLM-step budget when a builder does not set one (§8.4).
const DEFAULT_MAX_STEPS: u32 = 8;
/// Default number of tool-call rounds allowed per turn when unset (§8.4).
const DEFAULT_MAX_TOOL_ROUNDS: u32 = 4;

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
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// Starts a fluent [`AgentBuilder`].
    #[must_use]
    pub fn builder() -> AgentBuilder {
        AgentBuilder::default()
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

        let scope = FacadeAgentScope {
            llm: LlmClientHandler::new(self.client.clone()),
            tool: ToolRegistryHandler::new(registry),
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
        let (tool_calls, events) = collect_tool_traces(done.notifications());

        match done.cursor() {
            LoopCursor::Done(_) => {
                let (text, usage, stop_reason) =
                    final_turn_summary(self.machine.state().conversation());
                Ok(RunOutput {
                    reply: Reply::from_parts(text, Some(usage.clone()), stop_reason),
                    response: None,
                    usage: UsageSummary::from_supervisor(usage),
                    tool_calls,
                    delegations: Vec::new(),
                    artifacts: Vec::new(),
                    events,
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
    /// The full snapshot / restore / escape-hatch surface is added by a later
    /// milestone; this accessor exposes the assembled state for inspection.
    #[must_use]
    pub const fn state(&self) -> &AgentState {
        self.machine.state()
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
        // FacadeToolRegistry reports, so build it from the same three sources.
        let mut declarations: Vec<ToolDecl> = self.tools.iter().map(Tool::declaration).collect();
        declarations.extend(self.extra_declarations.iter().cloned());
        if let Some(custom) = &self.custom_registry {
            declarations.extend(custom.declarations());
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
        let mut approval = FacadeApproval::new(self.approval.unwrap_or_default());
        for tool in &self.tools {
            if let Some(tool_approval) = tool.approval_override() {
                approval = approval.with_tool_override(tool.name(), tool_approval.clone());
            }
        }
        let approval = Arc::new(approval);

        let requirement_ids: Arc<dyn RequirementIds> = Arc::new(ids.clone());
        let tool_ids: Arc<dyn ToolExecutionIds> = Arc::new(ids.clone());
        let approval_policy: Arc<dyn ToolApprovalPolicy> = approval.clone();
        let machine = DefaultAgentMachine::new(state, LlmStepMode::NonStreaming, requirement_ids)
            .with_tool_execution_ids(tool_ids)
            .with_approval_policy(approval_policy);

        Ok(Agent {
            machine,
            client,
            tools: self.tools,
            custom_registry: self.custom_registry,
            extra_declarations: self.extra_declarations,
            approval,
            ids,
        })
    }
}

/// One total drain layer carrying the LLM client, the run-scoped tool registry,
/// and the shared [`FacadeApproval`] interaction handler.
///
/// The three accessors [`drain`] consults are provided; every other handler
/// family defaults to `None` because the facade never emits those requirements
/// (no reconfiguration, subagents, or host permissions on the base agent path).
struct FacadeAgentScope {
    llm: LlmClientHandler,
    tool: ToolRegistryHandler,
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
fn build_loop_policy(
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

/// Projects the drained tool notifications into per-call traces and UI events.
///
/// A [`Notification::ToolCallStarted`] carries the tool name and framework call
/// id, so it seeds both a [`ToolTrace`] in `tool_calls` and a
/// [`RunEvent::ToolStarted`]. A [`Notification::ToolCallFinished`] carries only
/// the call id, so its name is recovered from the started map to emit the
/// matching [`RunEvent::ToolFinished`].
fn collect_tool_traces(notifications: &[Notification]) -> (Vec<ToolTrace>, Vec<RunEvent>) {
    let mut tool_calls = Vec::new();
    let mut events = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();

    for notification in notifications {
        match notification {
            Notification::ToolCallStarted(started) => {
                let call_id = started.call_id().to_string();
                let name = started.call().name.clone();
                names.insert(call_id.clone(), name.clone());
                let trace = ToolTrace { name, call_id };
                tool_calls.push(trace.clone());
                events.push(RunEvent::ToolStarted(trace));
            }
            Notification::ToolCallFinished(finished) => {
                let call_id = finished.call_id().to_string();
                let name = names.get(&call_id).cloned().unwrap_or_default();
                events.push(RunEvent::ToolFinished(ToolTrace { name, call_id }));
            }
            _ => {}
        }
    }

    (tool_calls, events)
}

/// Extracts the final assistant text, aggregated usage, and last stop reason of
/// the most recently committed turn.
fn final_turn_summary(
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
