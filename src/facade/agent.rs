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
//! [`HandlerScope`](crate::agent::HandlerScope) carrying the LLM client, the [`crate::facade::FacadeToolRegistry`], and the
//! [`FacadeApproval`] interaction handler (`docs/facade-api.md` §19).
//!
//! # Loop policy mapping
//!
//! The facade exposes two ergonomic knobs, `max_steps` (default `8`) and
//! `max_tool_rounds` (default `4`), while the underlying [`LoopPolicy`](crate::agent::LoopPolicy) has a
//! single per-turn step budget. A successful run needs one LLM step per tool
//! round plus one final response, so the effective budget is
//! `min(max_steps, max_tool_rounds + 1)` (§8.4). When that budget is exhausted
//! before a final assistant message the run fails with
//! [`FacadeError::LoopLimitExceeded`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::agent::{
    AgentError, AgentInput, AgentMachine, AgentState, Blackboard, BudgetLimits, CancellationToken,
    DefaultAgentMachine, InteractionHandler, LlmClientHandler, LoopCursor, LoopDoneReason, Mailbox,
    ModelRef, Plan, ReconfigRegistryHandler, ReconfigRequest, RunContext, RunId, StepInput,
    ToolRegistry, ToolRegistryHandler, drain,
};
use crate::client::LlmClient;
use crate::conversation::Conversation;
use crate::facade::approval::FacadeApproval;
use crate::facade::collab::{CollabBridge, CollabState, Collaboration};
use crate::facade::delegate::{
    AgentWorkerBuilder, Delegation, DelegationRecorder, DelegationRoute, DelegationToolHandler,
    DispatcherConfig, LocalSubagent, RulesRoutedTarget, new_delegation_recorder,
};
use crate::facade::error::FacadeError;
use crate::facade::external::{
    ExternalDelegateStatus, ManagedExternalDelegate, RetainedExternalSession,
};
use crate::facade::ids::FacadeIds;
use crate::facade::run::{DelegationStatus, IntoUserMessage, Reply, RunOutput, UsageSummary};
use crate::facade::tool::{Tool, ToolContextParts};
use crate::model::tool::Tool as ToolDecl;

mod builder;
mod dispatch;
mod reconfig;
mod snapshot;
mod stream;

use self::reconfig::FacadeToolRegistryResolver;

pub use builder::AgentBuilder;
pub(crate) use builder::{
    ApprovalRecorder, FacadeAgentScope, RecordingInteractionHandler, assemble_machine,
    build_agent_tool_declarations, build_facade_approval, build_loop_policy, classify_error,
    ensure_facade_reconfig_request_supported, ensure_facade_reconfig_rest_boundary,
    ensure_facade_set_model_valid, merge_facade_delegation_declarations, weave_approval_events,
};
pub(crate) use dispatch::{
    CollectedTraces, DispatcherDrive, RulesRoutedDrive, collect_traces, drive_dispatcher_routed,
    drive_rules_routed, final_turn_summary, user_message_text,
};
pub use snapshot::{
    AgentParts, AgentRestoreBuilder, AgentSnapshot, AgentStateSnapshot, BlackboardSnapshot,
    DelegateSnapshot, DelegationSnapshot, ExternalDelegateSnapshot, MailboxSnapshot,
};
pub use stream::AgentRunStream;

/// Default per-turn LLM-step budget when a builder does not set one (§8.4).
pub(crate) const DEFAULT_MAX_STEPS: u32 = 8;
/// Default number of tool-call rounds allowed per turn when unset (§8.4).
pub(crate) const DEFAULT_MAX_TOOL_ROUNDS: u32 = 4;

/// A cooperative cancellation handle for one facade Agent run.
///
/// Pass a clone to [`Agent::run_with_cancel`] or
/// [`Agent::run_full_with_cancel`], then call [`cancel`](Self::cancel) from the
/// host task that decides the run should stop. Tools invoked during that run see
/// the same token through [`ToolContext::cancel`](crate::facade::ToolContext::cancel).
#[derive(Clone, Debug, Default)]
pub struct CancelHandle {
    token: CancellationToken,
}

impl CancelHandle {
    /// Creates a fresh, not-yet-cancelled handle.
    #[must_use]
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    /// Requests cooperative cancellation of the associated run.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Returns whether cancellation has already been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

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
///
/// # Managed external sessions and teardown (M3-2)
///
/// A drive of a managed external delegate that ends cancelled or failed is
/// force-closed automatically by the facade — session cancel, transport close,
/// process termination, and the ephemeral-worktree policy all run — so a host
/// that does nothing extra leaks no subprocess. A *completed* drive keeps its
/// live session for reuse.
///
/// Dropping the `Agent` drops the delegate's session handler and its registry
/// with it. Every managed runtime child was spawned with `kill_on_drop`, so
/// the direct child process is reaped even then; however no `session/cancel`
/// notification or classified shutdown disposition runs, grandchildren the
/// child spawned are **not** reaped (process-group termination only runs on
/// the explicit close path), and an ephemeral worktree is left behind. A host
/// that needs a classified teardown — or must not leave grandchildren or
/// worktrees — must sweep **before** dropping, through the handler's registry
/// ([`RegistryExternalSessionHandler::registry`](crate::facade::RegistryExternalSessionHandler::registry)
/// →
/// [`ExternalSessionRegistry::cleanup_agent`](crate::agent::external::ExternalSessionRegistry::cleanup_agent)).
/// Dropping without that sweep is a best-effort backstop, never a silent clean
/// teardown.
pub struct Agent {
    machine: DefaultAgentMachine,
    client: Arc<dyn LlmClient>,
    tools: Arc<[Tool]>,
    custom_registry: Option<Arc<dyn ToolRegistry>>,
    extra_declarations: Arc<[ToolDecl]>,
    tool_registry_resolver: Arc<FacadeToolRegistryResolver>,
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
    /// Per-run budget limits used to create each [`RunContext`].
    budget: BudgetLimits,
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
            .field("budget", &self.budget)
            .field("collaboration", &self.collab.config)
            .finish_non_exhaustive()
    }
}

/// Synchronously closes a stranded facade turn by feeding one never-resume input.
///
/// Abandoning any outstanding requirement closes the whole in-flight turn on the
/// default machine: an LLM step discards its pending turn, while a tool phase
/// folds cancelled tool results and returns to a feedable cursor.
fn abandon_in_flight_turn(machine: &mut DefaultAgentMachine) {
    if let Some(id) = machine
        .cursor()
        .pending_requirement_ids()
        .into_iter()
        .next()
    {
        let _ = machine.step(StepInput::Abandon(id));
    }
}

/// Drop guard for non-streaming facade drives.
///
/// `run_full` cannot perform async cleanup when its future is dropped by a host
/// timeout, so this guard performs the same synchronous abandon step used by the
/// streaming drop path.
struct RunFullDropGuard {
    machine: std::ptr::NonNull<DefaultAgentMachine>,
    armed: bool,
}

impl RunFullDropGuard {
    fn new(machine: &mut DefaultAgentMachine) -> Self {
        Self {
            machine: std::ptr::NonNull::from(machine),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RunFullDropGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        // SAFETY: the guard is created inside `Agent::run_full` from the same
        // `&mut Agent` that owns the machine, so the pointer remains valid for
        // the lifetime of the future. The `drain` future is declared after this
        // guard and is therefore dropped before the guard when the run future is
        // cancelled, releasing its temporary `&mut DefaultAgentMachine` borrow
        // before this synchronous recovery step runs.
        unsafe {
            abandon_in_flight_turn(self.machine.as_mut());
        }
    }
}

/// Clears approval decisions recorded during a facade run at every exit path.
struct ApprovalPendingGuard {
    approval: Arc<FacadeApproval>,
}

impl ApprovalPendingGuard {
    fn new(approval: Arc<FacadeApproval>) -> Self {
        approval.clear_pending();
        Self { approval }
    }
}

impl Drop for ApprovalPendingGuard {
    fn drop(&mut self) {
        self.approval.clear_pending();
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
        self.run_with_cancel(input, CancelHandle::new()).await
    }

    /// Runs one agent turn with a caller-owned cancellation handle and returns
    /// the minimal [`Reply`].
    ///
    /// Call [`CancelHandle::cancel`] from another task to request cooperative
    /// cancellation. The same token is passed to tools through
    /// [`ToolContext::cancel`](crate::facade::ToolContext::cancel).
    ///
    /// # Errors
    ///
    /// Returns any [`FacadeError`] produced by
    /// [`run_full_with_cancel`](Agent::run_full_with_cancel).
    pub async fn run_with_cancel(
        &mut self,
        input: impl IntoUserMessage,
        cancel: CancelHandle,
    ) -> Result<Reply, FacadeError> {
        Ok(self.run_full_with_cancel(input, cancel).await?.reply)
    }

    /// Runs one agent turn and returns the full [`RunOutput`].
    ///
    /// The turn is driven by [`drain`]ing the held [`DefaultAgentMachine`]: the
    /// machine loops through LLM steps, tool calls, and approvals until it
    /// reaches a final assistant response or exhausts its loop budget. Tool
    /// execution, the approval policy, and the LLM client are all supplied
    /// through a per-run [`HandlerScope`](crate::agent::HandlerScope); the run-scoped
    /// [`crate::facade::FacadeToolRegistry`] is rebuilt each call so each tool sees the current
    /// run id, worktree, cancellation token, and trace handle.
    ///
    /// On success the committed turn's aggregated token usage and final stop
    /// reason are folded into the returned [`Reply`], and every
    /// [`Notification::ToolCallStarted`](crate::agent::Notification::ToolCallStarted) / [`Notification::ToolCallFinished`](crate::agent::Notification::ToolCallFinished) is
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
    /// `Agent` stays usable and its committed history is unchanged. If the run
    /// future is dropped before completion — for example because the host wraps
    /// it in `tokio::time::timeout` — a synchronous guard abandons the stranded
    /// requirement before control returns to the caller, leaving the same
    /// committed consistency point available for the next run or snapshot.
    pub async fn run_full(
        &mut self,
        input: impl IntoUserMessage,
    ) -> Result<RunOutput, FacadeError> {
        self.run_full_with_cancel(input, CancelHandle::new()).await
    }

    /// Runs one agent turn with a caller-owned cancellation handle and returns
    /// the full [`RunOutput`].
    ///
    /// This is the cancellable form of [`run_full`](Agent::run_full). The handle
    /// is cooperative: cancellation is observed at the same bounded points as the
    /// lower-level Agent driver — including pre-empting the wait on a blocked
    /// tool/interaction batch (M3-3), where a still-blocked fulfill future is
    /// detached after a bounded unwind grace — and an interrupted turn is
    /// abandoned so the `Agent` remains usable.
    ///
    /// # Errors
    ///
    /// Returns the same variants as [`run_full`](Agent::run_full). A cancelled
    /// run currently surfaces as [`FacadeError::Agent`] with a cancellation
    /// diagnostic while preserving the committed consistency point.
    pub async fn run_full_with_cancel(
        &mut self,
        input: impl IntoUserMessage,
        cancel: CancelHandle,
    ) -> Result<RunOutput, FacadeError> {
        let _approval_guard = ApprovalPendingGuard::new(self.approval.clone());
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
                return self.run_rules_routed(delegate_name, task, cancel).await;
            }
        }

        // Dispatcher-routed delegation routes *every* task through the facade
        // cheap→verify→strong escalation loop, again without exposing any
        // delegate to the model (§13.3).
        if self.delegation.is_dispatcher_routed() {
            let task = user_message_text(&message);
            return self.run_dispatcher_routed(task, cancel).await;
        }

        let run_id = self.ids.run_id();
        let ctx = RunContext::new_root_with_cancellation(
            run_id,
            self.budget,
            self.ids.trace_root("agent-run"),
            cancel.token(),
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
        let (tool_handler, reconfig_handler) = self.tool_handlers_for_run(context)?;

        let recorder = new_delegation_recorder();
        // Records each approval the drive pauses on so the non-streaming
        // `RunOutput.events` can surface an `ApprovalRequested`, matching what
        // the streaming path emits live (M2-1).
        let approvals: ApprovalRecorder = Arc::new(Mutex::new(Vec::new()));
        let scope = FacadeAgentScope {
            llm: LlmClientHandler::new(self.client.clone()),
            tool: DelegationToolHandler::new(
                tool_handler,
                self.delegation_route(),
                self.client.clone(),
                self.supervisor_model(),
                self.interaction_handler.clone(),
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
            reconfig: reconfig_handler,
        };

        let agent_input = AgentInput::user_message(
            self.ids.turn_id(),
            self.ids.message_id(),
            message,
            self.ids.message_id(),
            self.ids.step_id(),
        )?;

        let mut drop_guard = RunFullDropGuard::new(&mut self.machine);
        let done = {
            let drive = drain(&mut self.machine, agent_input, &scope, None, &ctx);
            drive.await?
        };
        drop_guard.disarm();
        let collected: CollectedTraces = collect_traces(done.notifications(), &recorder);
        // Recovered in fulfill order so a paused approval sits before the tool
        // lifecycle it gated (or at the tail when the tool never started).
        let recorded_approvals = approvals
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
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
            // A cancelled drain rests on the machine's post-cancel rest state
            // (`Idle`), not a terminal cursor (M4-5): surface an honest cancel
            // error instead of the misleading "non-terminal cursor" one. A
            // dedicated facade-level cancellation surface lands with the cancel
            // entry points in M5-4.
            cursor if done.cancelled() => Err(FacadeError::Agent(AgentError::Other(format!(
                "agent run cancelled (cursor: {:?})",
                cursor.kind()
            )))),
            // A per-turn step-limit stop is a normal terminal on the machine
            // (M4-4); the facade surfaces it as its structured limit error.
            LoopCursor::Done(done_cursor)
                if done_cursor.reason() == LoopDoneReason::StepLimitReached =>
            {
                Err(FacadeError::LoopLimitExceeded)
            }
            LoopCursor::Done(done_cursor)
                if done_cursor.reason() == LoopDoneReason::BudgetExhausted =>
            {
                Err(FacadeError::BudgetExhausted)
            }
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
            LoopCursor::Error(error) => Err(classify_error(error)),
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

    /// Queues a turn-boundary reconfiguration for this facade agent.
    ///
    /// Accepted requests are validated eagerly and applied only at the next turn
    /// boundary. Calls are allowed only while the facade is between runs (the
    /// machine is resting on `Idle`, `Done`, `Error`, or `CancelRecovery`); an
    /// active or parked turn returns [`FacadeError::InvalidState`] instead of
    /// making the change visible mid-turn. The stream API borrows the agent
    /// mutably for its whole lifetime, so this same rule also prevents
    /// reconfiguration while a stream is live.
    ///
    /// The facade supports model, tool-set declaration, system-prompt overlay,
    /// and loop-policy requests. Skill activation requests are rejected with
    /// [`FacadeError::Config`] because the facade does not yet expose a skill
    /// registry or skill-to-prompt/tool expansion layer. Every supported
    /// request can be constructed from facade
    /// re-exports alone ([`ModelRef`], [`ToolSetId`](crate::facade::ToolSetId),
    /// [`ToolSetRef`](crate::facade::ToolSetRef), [`ToolSetPatch`](crate::facade::ToolSetPatch),
    /// [`ToolDecl`], [`LoopPolicy`](crate::facade::LoopPolicy)) — no `agent::` internal imports are needed:
    ///
    /// ```
    /// # fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// use agent_lib::facade::{ReconfigRequest, ToolDecl, ToolSetId, ToolSetRef};
    ///
    /// let tool_set = ToolSetRef::new(
    ///     ToolSetId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890e1")?,
    ///     vec![ToolDecl {
    ///         name: "get_weather".to_owned(),
    ///         description: "Look up the current weather for a city.".to_owned(),
    ///         input_schema: serde_json::json!({ "type": "object" }),
    ///     }],
    /// );
    /// let request = ReconfigRequest::ReplaceToolSet { tool_set };
    /// # let _ = request;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// A `SetModel` request is validated with the same checks
    /// [`AgentBuilder::build`] applies to an initial model — a non-blank model
    /// name, a finite temperature, and provider extras consistent with the
    /// provider the current model targets when one is inferable — so an invalid
    /// model can never be queued and rendered into the next request.
    ///
    /// `ReplaceToolSet` / `PatchToolSet` manage only the **non-delegation**
    /// surface. The delegation tool declarations (one `ask_<name>` per
    /// model-routed delegate, or the unified single-tool name) are never taken
    /// from the caller: they are synthesized `pub(crate)`-side and are always
    /// re-derived from the delegates currently registered on this agent, then
    /// merged into the queued set, so a replacement can never silently drop a
    /// still-registered delegate from the model-visible surface (mag gap B1).
    /// A caller-supplied `ReplaceToolSet` declaration that collides with a
    /// synthesized delegation name, and a `PatchToolSet` that removes or
    /// shadows one, are rejected with [`FacadeError::Config`] — delegation
    /// declarations are derived state the caller must not manage; drop the
    /// delegate registration instead of editing its tool.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::InvalidState`] when a turn is in progress.
    /// Returns [`FacadeError::Config`] when the request content is invalid or
    /// unsupported: a skill-variant request (no facade skill registry exists),
    /// a `SetModel` payload that fails the builder-level
    /// model checks (blank name, non-finite temperature, mismatched provider
    /// extras), or a tool-set request that tries to manage a synthesized
    /// delegation declaration directly. Returns [`FacadeError::Agent`] when the
    /// underlying agent state rejects the request, for example because a
    /// system-prompt overlay version is stale, a tool-set patch targets a
    /// non-current tool-set id, or the requested tool set is not backed by the
    /// registered facade tool surface. On any admission failure nothing is
    /// queued.
    pub fn reconfigure(&mut self, request: ReconfigRequest) -> Result<(), FacadeError> {
        ensure_facade_reconfig_request_supported(&request)?;
        ensure_facade_reconfig_rest_boundary(self.machine.cursor())?;
        if let ReconfigRequest::SetModel { model } = &request {
            ensure_facade_set_model_valid(model, self.machine.state().current_model())?;
        }
        let request = merge_facade_delegation_declarations(
            request,
            &self.delegation,
            &self.delegates,
            &self.external_agents,
        )?;
        self.machine.reconfigure(request)?;
        Ok(())
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

    /// Returns the supervisor's effective model, substituted into any inheriting
    /// child spec when a delegation is fulfilled (R4).
    fn supervisor_model(&self) -> ModelRef {
        self.machine.state().current_model().clone()
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

    /// Builds the active tool handler and its matching reconfig handler for one
    /// run-scoped [`ToolContextParts`].
    fn tool_handlers_for_run(
        &self,
        context: ToolContextParts,
    ) -> Result<(ToolRegistryHandler, ReconfigRegistryHandler), FacadeError> {
        self.tool_registry_resolver.bind_context(context);
        let registry = self
            .tool_registry_resolver
            .resolve_active_registry(self.machine.state().current_tool_set())
            .map_err(AgentError::from)?;
        Ok(ToolRegistryHandler::with_reconfig_resolver(
            registry,
            self.machine.tool_registry_resolver(),
        ))
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
        let (tool_handler, _) = self.tool_handlers_for_run(context)?;
        Ok(DelegationToolHandler::new(
            tool_handler,
            self.delegation_route(),
            self.client.clone(),
            self.supervisor_model(),
            self.interaction_handler.clone(),
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
        cancel: CancelHandle,
    ) -> Result<RunOutput, FacadeError> {
        let run_id = self.ids.run_id();
        let ctx = RunContext::new_root_with_cancellation(
            run_id,
            self.budget,
            self.ids.trace_root("agent-run"),
            cancel.token(),
        );
        let recorder = new_delegation_recorder();
        let handler = self.build_delegation_handler(run_id, &ctx, recorder.clone())?;
        let target = self.resolve_rules_target(&delegate_name)?;

        let drive: RulesRoutedDrive =
            drive_rules_routed(&handler, &recorder, &self.ids, &target, task, &ctx).await?;

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
    async fn run_dispatcher_routed(
        &mut self,
        task: String,
        cancel: CancelHandle,
    ) -> Result<RunOutput, FacadeError> {
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
        let ctx = RunContext::new_root_with_cancellation(
            run_id,
            self.budget,
            self.ids.trace_root("agent-run"),
            cancel.token(),
        );
        let recorder = new_delegation_recorder();
        let handler = self.build_delegation_handler(run_id, &ctx, recorder.clone())?;
        let targets = self.resolve_dispatcher_targets(&config)?;
        let evaluator = self.delegation.dispatcher_evaluator_hook().cloned();
        let verifier = self.delegation.dispatcher_verifier_hook().cloned();

        let drive: DispatcherDrive = drive_dispatcher_routed(
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
    /// each incremental [`RunEvent::TextDelta`](crate::facade::RunEvent::TextDelta) as the assistant text arrives and
    /// each [`RunEvent::ToolStarted`](crate::facade::RunEvent::ToolStarted) / [`RunEvent::ToolFinished`](crate::facade::RunEvent::ToolFinished) /
    /// [`RunEvent::ApprovalRequested`](crate::facade::RunEvent::ApprovalRequested) as the drive reaches it, then yields
    /// exactly one terminal [`RunEvent::Done`](crate::facade::RunEvent::Done) carrying the complete
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
        self.stream_with_cancel(input, CancelHandle::new()).await
    }

    /// Starts a streamed run with a caller-owned cancellation handle.
    ///
    /// The returned [`AgentRunStream`] also exposes [`AgentRunStream::cancel`]
    /// and [`AgentRunStream::interject`] for hosts that prefer controlling the
    /// run through the stream value itself.
    ///
    /// # Errors
    ///
    /// Returns the same immediate setup errors as [`stream`](Agent::stream).
    pub async fn stream_with_cancel(
        &mut self,
        input: impl IntoUserMessage,
        cancel: CancelHandle,
    ) -> Result<AgentRunStream<'_>, FacadeError> {
        stream::start(self, input.into_user_message(), cancel)
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
    /// A reconfiguration queued through [`reconfigure`](Agent::reconfigure) but
    /// not yet applied at a turn boundary **is** captured: the pending queue is
    /// part of the serialized [`AgentState`], and a restored agent re-plans it,
    /// so the queued change applies at the restored agent's next turn boundary
    /// exactly as if the snapshot had never been taken. Restore additionally
    /// requires the re-injected tool surface to cover both the snapshot's
    /// current tool set and any tool set a queued reconfig would apply; a
    /// surface that cannot fails the restore explicitly rather than stranding
    /// the agent (see [`AgentRestoreBuilder::build`]).
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
    /// run budget limits, the last-known external session facts, and the live
    /// collaboration substrate (config plus the shared mailbox / blackboard /
    /// plan handles) to an
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
            tools: self.tools.iter().cloned().collect(),
            custom_registry: self.custom_registry,
            extra_declarations: self.extra_declarations.iter().cloned().collect(),
            approval: self.approval,
            interaction_handler: self.interaction_handler,
            ids: self.ids,
            delegates: self.delegates,
            external_agents: self.external_agents,
            delegation: self.delegation,
            budget: self.budget,
            retained_external_sessions: self.last_external_sessions,
            collaboration: collab.config,
            mailbox: collab.mailbox,
            blackboard: collab.blackboard,
            plan: collab.plan,
        }
    }
}

#[cfg(test)]
mod tests;
