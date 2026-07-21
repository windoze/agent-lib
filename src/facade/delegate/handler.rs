//! The delegation-driving machinery of [`crate::facade::delegate`]: the
//! run-scoped [`DelegationToolHandler`] and its [`DelegationRoute`] table, the
//! per-run [`DelegationRecorder`], and the child machine/scope plumbing a
//! delegation call drives to completion.
//!
//! The configuration surface ([`Delegation`](crate::facade::delegate::Delegation),
//! [`LocalSubagent`], the worker builder) stays in the parent module; everything
//! here is the runtime side a fulfilled delegation exercises.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use crate::agent::external::ExternalSessionRef;
use crate::agent::{
    AgentError, AgentInput, AgentMachine, AgentSpec, AgentSpecRef, AgentState, ApprovalDecision,
    ApprovalRequirement, ApprovalResponse, CancellationToken, DefaultAgentMachine,
    DrivingSubagentHandler, HandlerScope, Interaction, InteractionHandler, InteractionKind,
    InteractionOrigin, InteractionResponse, LlmClientHandler, LlmHandler, LoopCursor, ModelRef,
    PermissionResponse, RequirementResult, RunContext, RunId, ScopePop, SpawnedChild, StepInput,
    StepOutcome, SubagentHandler, SubagentOutput, SubagentSpawner, ToolHandler, ToolRegistry,
    ToolRegistryHandler, ToolRuntimeError, TraceHandle, TraceNodeId, TurnDone,
};
use crate::client::LlmClient;
use crate::conversation::{Conversation, ConversationConfig, ToolCallId};
use crate::facade::agent::{assemble_machine, final_turn_summary};
use crate::facade::approval::FacadeApproval;
use crate::facade::collab::CollabBridge;
use crate::facade::external::{ManagedExternalDelegate, drive_external};
use crate::facade::ids::FacadeIds;
use crate::facade::run::{ArtifactRef, DelegationStatus, DelegationTrace};
use crate::facade::tool::{FacadeToolRegistry, ToolContextParts};
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::tool::Tool as ToolDecl;
use crate::model::tool::{ToolCall, ToolResponse, ToolStatus};
use crate::model::usage::Usage;

use super::{DEFAULT_MAX_DELEGATION_DEPTH, LocalSubagent, delegation_tool_name};

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

/// Routes a delegated child's paused interaction to the supervisor's injected
/// handler.
///
/// Shared by the local-subagent path ([`ChildAgentScope`]) and the managed
/// external path (`ExternalInteractionScope` in `crate::facade::external`): the
/// child machine keeps its own approval gate; this router only decides where the
/// already-paused interaction is answered. It annotates the forwarded request
/// with display-only delegate attribution so the parent UI can render which
/// worker asked.
pub(crate) struct DelegationInteractionRouter {
    pub(crate) delegate: String,
    pub(crate) parent: Arc<dyn InteractionHandler>,
}

#[async_trait]
impl InteractionHandler for DelegationInteractionRouter {
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult {
        let routed = request
            .clone()
            .with_origin(InteractionOrigin::new(self.delegate.clone(), ctx.depth()));
        tokio::select! {
            biased;
            _ = ctx.cancellation().cancelled() => cancelled_delegation_interaction_result(&routed),
            result = self.parent.fulfill(&routed, ctx) => result,
        }
    }
}

/// Builds an in-family interaction result for a delegated interaction abandoned
/// by cancellation before the parent handler answered.
pub(crate) fn cancelled_delegation_interaction_result(request: &Interaction) -> RequirementResult {
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

/// Mints the run id and trace node id for one delegated child drive.
///
/// The run id is freshly minted per drive, so folding it into the trace node id
/// keeps the id unique even when the same delegate is driven more than once in a
/// single run (e.g. a dispatcher verifier re-run per attempt, §13.3); a fixed
/// `{prefix}:{name}` would collide.
pub(crate) fn delegation_child_ids(
    ids: &FacadeIds,
    prefix: &str,
    name: &str,
) -> (RunId, TraceNodeId) {
    let run_id = ids.run_id();
    let node = TraceNodeId::new(format!("{prefix}:{name}:{run_id}"));
    (run_id, node)
}

/// Folds a call-local capture slot into the [`SubagentOutput`] summary a
/// [`SubagentSpawner::summarize`] returns, defaulting to an empty summary when
/// the child never committed its facts. `payload` projects the captured facts
/// into the summary text.
pub(crate) fn summarize_delegation_slot<T>(
    slot: &Mutex<Option<T>>,
    payload: impl Fn(&T) -> String,
) -> SubagentOutput {
    let summary = slot
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .as_ref()
        .map(payload)
        .unwrap_or_default();
    SubagentOutput { summary }
}

/// Builds the opening user-turn [`AgentInput`] a delegated child drive starts
/// from: the task brief as a single user text block.
pub(crate) fn delegation_opening_input(
    ids: &FacadeIds,
    task: &str,
) -> Result<AgentInput, AgentError> {
    let user = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: task.to_owned(),
            extra: Map::new(),
        }],
    };
    AgentInput::user_message(
        ids.turn_id(),
        ids.message_id(),
        user,
        ids.message_id(),
        ids.step_id(),
    )
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
        Ok(delegation_child_ids(
            &self.ids,
            "subagent",
            self.subagent.name(),
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
            Some(parent) => Arc::new(DelegationInteractionRouter {
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

        let opening = delegation_opening_input(&self.ids, &self.task)?;

        Ok(SpawnedChild {
            machine: Box::new(recording),
            scope: Box::new(scope),
            opening,
        })
    }

    fn summarize(&self, _done: &TurnDone) -> SubagentOutput {
        summarize_delegation_slot(&self.slot, |captured| captured.text.clone())
    }
}

/// The run-scoped routing table a [`DelegationToolHandler`] consults to
/// recognize a delegation tool call and select the target subagent.
///
/// Built once per run from the agent's
/// [`Delegation`](crate::facade::delegate::Delegation) config and its registered
/// delegates (see [`Delegation::route`](crate::facade::delegate::Delegation::route)).
/// Two shapes mirror the two delegation
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
/// Since the B1 hardening the facade's `Agent::reconfigure` re-derives
/// delegation declarations from the registered delegates on every tool-set
/// reconfig, so this path can no longer remove one; the gate remains the
/// in-run backstop for tool sets that lost a delegation declaration any other
/// way (for example a restored state pruned by B4).
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
            _ = ctx.cancellation().cancelled() => cancelled_delegation_interaction_result(&request),
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
/// Resolved by the facade from a
/// [`Delegation::route_task`](crate::facade::delegate::Delegation::route_task)
/// match against the agent's registered delegates. It owns the delegate recipe
/// (both variants are cheap, data-only clones) so the drive holds no borrow of
/// the agent across an
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
