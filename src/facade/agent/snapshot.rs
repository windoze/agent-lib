//! Data-only snapshot / restore / escape-hatch surface for the [`Agent`] facade.
//!
//! [`Agent::snapshot`](super::Agent::snapshot) captures an [`AgentSnapshot`]: the
//! accumulated supervisor [`Conversation`](crate::conversation::Conversation)
//! plus the serializable [`AgentState`] (spec, active tool-set declarations,
//! model, loop policy, and loop cursor). A snapshot is *data only* — it never
//! carries the LLM client, provider credentials, tool closures, or the approval
//! handler — so it is safe to persist and later feed to
//! [`Agent::restore`](super::Agent::restore), which re-injects those runtime
//! handles through an [`AgentRestoreBuilder`] (`docs/facade-api.md` §15.2).
//!
//! [`Agent::into_parts`](super::Agent::into_parts) is the complementary escape
//! hatch: it consumes the agent and hands ownership of the assembled parts to a
//! caller that wants to drive the lower layers directly (`docs/facade-api.md`
//! §8.2).
//!
//! The delegate slice of an [`AgentSnapshot`] carries the registered local
//! subagents as data-only recipes and the delegation routing mode, so a restored
//! agent re-advertises and re-routes to the same subagents. The mailbox,
//! blackboard, plan, and artifact slices are reserved for later milestones; the
//! base agent path produces empty slices there so the struct shape is stable
//! now.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::external::{ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionRef};
use crate::agent::{AgentSpec, AgentState, PlanSnapshot, ToolRegistry, ToolSetRef, WorktreeRef};
use crate::client::LlmClient;
use crate::conversation::{Conversation, ConversationSnapshot};
use crate::facade::approval::{ApprovalPolicy, FacadeApproval};
use crate::facade::chat::client_for_provider;
use crate::facade::config::ProviderConfig;
use crate::facade::delegate::{Delegation, LocalSubagent};
use crate::facade::error::FacadeError;
use crate::facade::external::{
    ExternalDelegateStatus, ExternalRunMode, ManagedExternalAgent, ManagedExternalDelegate,
    RestoreExternal, RetainedExternalSession,
};
use crate::facade::ids::FacadeIds;
use crate::facade::run::ArtifactRef;
use crate::facade::tool::{Tool, ensure_unique_tool_names};
use crate::model::tool::Tool as ToolDecl;

use super::{Agent, assemble_machine, build_facade_approval};

/// A serializable, data-only snapshot of an [`Agent`]'s supervisor state.
///
/// Beyond the [`supervisor`](Self::supervisor) conversation and the
/// [`agent_state`](Self::agent_state), the snapshot carries the registered local
/// subagent [`delegates`](Self::delegates) (data-only recipes), the registered
/// managed [`external_delegates`](Self::external_delegates) (data-only recipes
/// plus their last-known session facts), and the
/// [`delegation`](Self::delegation) routing mode, so a restored agent
/// re-advertises and re-routes to the same delegates (`docs/facade-api.md`
/// §15.2). The [`pending_delegations`](Self::pending_delegations) slice captures
/// any in-progress child conversation; the synchronous one-shot delegation drive
/// never rests with a child in flight at a committed snapshot point, so it is
/// empty in ordinary capture (see [`DelegationSnapshot`]). The mailbox,
/// blackboard, plan, and artifact slices are reserved for later milestones and
/// are empty here.
///
/// The type is `Clone`/`PartialEq`/`Serialize`/`Deserialize` but deliberately not
/// `Eq`: [`AgentStateSnapshot`] and [`DelegateSnapshot`] capture model settings
/// whose `f32` leaves are not totally comparable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    /// The supervisor [`Conversation`](crate::conversation::Conversation)'s own
    /// data-only snapshot, useful for inspection without deserializing the whole
    /// [`AgentState`].
    pub supervisor: ConversationSnapshot,
    /// The serialized [`AgentState`], the authoritative source
    /// [`Agent::restore`](super::Agent::restore) rebuilds from (it preserves the
    /// spec, active declarations, model, loop policy, and loop cursor).
    pub agent_state: AgentStateSnapshot,
    /// The registered local subagent delegates, as data-only recipes (their
    /// runtime approval handlers are omitted, §15.2).
    pub delegates: Vec<DelegateSnapshot>,
    /// The registered managed external delegates, as data-only recipes plus
    /// their last-known session facts (their runtime session handlers and
    /// credentials are omitted, §15.2).
    pub external_delegates: Vec<ExternalDelegateSnapshot>,
    /// The delegation routing mode, so a restored agent re-routes delegation
    /// calls exactly as it did before the snapshot.
    pub delegation: Delegation,
    /// In-flight delegation snapshots (empty in ordinary capture; see
    /// [`DelegationSnapshot`]).
    pub pending_delegations: Vec<DelegationSnapshot>,
    /// Shared mailbox snapshot (reserved; `None` on the base path).
    pub mailbox: Option<MailboxSnapshot>,
    /// Shared blackboard snapshot (reserved; `None` on the base path).
    pub blackboard: Option<BlackboardSnapshot>,
    /// Plan-board snapshot (reserved; `None` on the base path).
    pub plan: Option<PlanSnapshot>,
    /// Artifact references produced by delegates (reserved; empty on the base
    /// path).
    pub artifacts: Vec<ArtifactRef>,
}

impl AgentSnapshot {
    /// Captures a data-only snapshot of `state`, its registered `delegates`,
    /// `external` delegates (folding in their retained `external_sessions`), and
    /// the `delegation` routing mode.
    ///
    /// The supervisor conversation is snapshotted first so an in-flight
    /// (uncommitted) turn surfaces as a clean [`FacadeError::Conversation`]
    /// before the whole state is serialized. Each local delegate is captured as a
    /// data-only [`DelegateSnapshot`] (its approval handler, a runtime handle, is
    /// deliberately dropped); each external delegate is captured as a data-only
    /// [`ExternalDelegateSnapshot`] (its session handler, credentials, and any
    /// process handle are dropped, §15.2). `pending_delegations` is empty: a
    /// delegation is driven to completion within one supervisor turn, so no child
    /// is in flight at the committed point a snapshot requires. No task brief is
    /// written to the snapshot (`PLAN.md` R5).
    pub(super) fn capture(
        state: &AgentState,
        delegates: &[LocalSubagent],
        external: &[ManagedExternalDelegate],
        external_sessions: &HashMap<String, RetainedExternalSession>,
        delegation: &Delegation,
    ) -> Result<Self, FacadeError> {
        let supervisor = state.conversation().snapshot()?;
        let agent_state = AgentStateSnapshot::capture(state)?;
        Ok(Self {
            supervisor,
            agent_state,
            delegates: delegates.iter().map(DelegateSnapshot::capture).collect(),
            external_delegates: external
                .iter()
                .map(|delegate| {
                    ExternalDelegateSnapshot::capture(
                        delegate,
                        external_sessions.get(delegate.name()),
                    )
                })
                .collect(),
            delegation: delegation.clone(),
            pending_delegations: Vec::new(),
            mailbox: None,
            blackboard: None,
            plan: None,
            artifacts: Vec::new(),
        })
    }
}

/// A data-only capture of an [`AgentState`].
///
/// [`AgentState`] owns the live [`Conversation`](crate::conversation::Conversation)
/// and is intentionally neither `Clone` nor `PartialEq`. This newtype captures it
/// as a serialized `serde_json::Value`, so the snapshot is
/// `Clone`/`PartialEq`/`Serialize`/`Deserialize` and can be persisted and later
/// restored. It is `#[serde(transparent)]`, so it serializes exactly as the
/// underlying [`AgentState`] record — no wrapper object is introduced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentStateSnapshot(serde_json::Value);

impl AgentStateSnapshot {
    /// Serializes a live [`AgentState`] into a data-only snapshot.
    pub(super) fn capture(state: &AgentState) -> Result<Self, FacadeError> {
        serde_json::to_value(state)
            .map(Self)
            .map_err(|error| FacadeError::InvalidState(format!("agent state snapshot: {error}")))
    }

    /// Deserializes the captured snapshot back into a live [`AgentState`].
    pub(super) fn into_state(self) -> Result<AgentState, FacadeError> {
        serde_json::from_value(self.0)
            .map_err(|error| FacadeError::InvalidState(format!("agent state restore: {error}")))
    }
}

/// A data-only snapshot of one registered local subagent delegate.
///
/// It captures the delegate's stable data — its registration `name`, advertised
/// `description`, child [`AgentSpec`], advertised [`ToolSetRef`], and whether it
/// inherits the supervisor model — but never its
/// [`ApprovalPolicy`](crate::facade::ApprovalPolicy), which may carry a runtime
/// closure and is re-supplied on restore (§15.2). It is `PartialEq` but not `Eq`
/// because the captured [`AgentSpec`] records `f32` model settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DelegateSnapshot {
    /// The delegate's registration name.
    pub name: String,
    /// The delegate's advertised description.
    pub description: String,
    /// The child [`AgentSpec`] rebuilt when the delegation is fulfilled.
    pub spec: AgentSpec,
    /// The delegate's advertised tool declarations.
    pub tools: ToolSetRef,
    /// Whether the delegate inherits the supervisor's model (R4).
    pub inherit_model: bool,
}

impl DelegateSnapshot {
    /// Captures a data-only snapshot of `delegate` (approval handler dropped).
    #[must_use]
    pub(super) fn capture(delegate: &LocalSubagent) -> Self {
        Self {
            name: delegate.name().to_owned(),
            description: delegate.description().to_owned(),
            spec: delegate.spec().clone(),
            tools: delegate.tools().clone(),
            inherit_model: delegate.inherits_model(),
        }
    }

    /// Rebuilds a data-first [`LocalSubagent`] from the snapshot, re-supplying
    /// the `approval` policy a snapshot cannot carry.
    #[must_use]
    pub(super) fn into_delegate(self, approval: ApprovalPolicy) -> LocalSubagent {
        LocalSubagent::from_parts(
            self.name,
            self.description,
            self.spec,
            self.tools,
            approval,
            self.inherit_model,
        )
    }
}

/// A data-only snapshot of one registered managed external delegate.
///
/// It captures the delegate's stable recipe data — its registration `name`, the
/// backing `runtime` kind, validated run `mode`, optional `worktree`, pinned
/// `model`, launch `args`, and `permission_mode` — plus the delegate's last-known
/// data-only session facts: a coarse [`status`](Self::status), any resumable
/// [`session`](Self::session) reference, and the [`artifacts`](Self::artifacts)
/// its last drive reported. It never carries the runtime session handler, the SDK
/// client, a process handle, credentials, or the raw task brief (`PLAN.md` R5,
/// §15.2). It is `PartialEq` but not `Eq` only for symmetry with the rest of the
/// snapshot tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalDelegateSnapshot {
    /// The delegate's registration name (the `ask_<name>` stem).
    pub name: String,
    /// The backing external runtime kind.
    pub runtime: ExternalRuntimeKind,
    /// The validated run mode.
    pub mode: ExternalRunMode,
    /// The worktree the runtime was confined to, if one was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeRef>,
    /// The pinned model, if one was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The extra launch arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// The permission mode applied to gated actions.
    pub permission_mode: ExternalPermissionMode,
    /// The delegate's last-known coarse session status.
    pub status: ExternalDelegateStatus,
    /// The resumable session facts the delegate's last drive reported, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ExternalSessionRef>,
    /// Artifacts the delegate's last completed drive reported, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
}

impl ExternalDelegateSnapshot {
    /// Captures a data-only snapshot of `delegate`, folding in its last-known
    /// `session` facts (session handler and credentials dropped, §15.2).
    #[must_use]
    pub(super) fn capture(
        delegate: &ManagedExternalDelegate,
        session: Option<&RetainedExternalSession>,
    ) -> Self {
        let agent = delegate.agent();
        Self {
            name: delegate.name().to_owned(),
            runtime: agent.runtime().clone(),
            mode: agent.mode(),
            worktree: agent.worktree().cloned(),
            model: agent.model().map(ToOwned::to_owned),
            args: agent.args().to_vec(),
            permission_mode: agent.permission_mode(),
            status: session.map(|retained| retained.status).unwrap_or_default(),
            session: session.and_then(|retained| retained.session.clone()),
            artifacts: session
                .map(|retained| retained.artifacts.clone())
                .unwrap_or_default(),
        }
    }

    /// Rebuilds a data-first [`ManagedExternalDelegate`] recipe from the snapshot,
    /// without the runtime session handler a snapshot cannot carry (§15.2).
    ///
    /// The rebuilt delegate re-advertises and re-routes exactly like the original
    /// but cannot be driven until a runtime session handler is re-supplied through
    /// [`AgentRestoreBuilder::external_agent`](super::AgentRestoreBuilder::external_agent).
    #[must_use]
    pub(super) fn to_delegate(&self) -> ManagedExternalDelegate {
        let agent = ManagedExternalAgent::from_restored_parts(
            self.runtime.clone(),
            self.mode,
            self.worktree.clone(),
            self.model.clone(),
            self.args.clone(),
            self.permission_mode,
        );
        ManagedExternalDelegate::new(self.name.clone(), agent)
    }
}

/// A data-only snapshot of one in-progress delegation to a local subagent.
///
/// Per `docs/facade-api.md` §15.2 a pending delegation is preserved as the
/// child's [`ConversationSnapshot`] plus the delegate `name`, so restore can
/// rebuild the child machine and resume it. The synchronous one-shot delegation
/// drive runs a child to completion inside a single supervisor turn and only
/// permits a snapshot at a committed point, so ordinary capture never records
/// one; the type exists so the capability is in place (and exercised by tests)
/// for future interruptible delegations.
///
/// It carries no separate task-brief field: the brief lives only inside the
/// child conversation it is part of, never duplicated into a trace (`PLAN.md`
/// R5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationSnapshot {
    /// The registration name of the delegate handling the in-flight task.
    pub delegate: String,
    /// The child's data-only conversation snapshot, from which the child machine
    /// is rebuilt on restore.
    pub conversation: ConversationSnapshot,
}

impl DelegationSnapshot {
    /// Captures an in-progress child `conversation` under `delegate`.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Conversation`] if the child conversation has an
    /// uncommitted turn in flight (a snapshot is only available at a committed
    /// consistency point).
    pub fn capture(
        delegate: impl Into<String>,
        conversation: &Conversation,
    ) -> Result<Self, FacadeError> {
        Ok(Self {
            delegate: delegate.into(),
            conversation: conversation.snapshot()?,
        })
    }

    /// Rebuilds the child's live [`Conversation`] from the snapshot, from which a
    /// child machine can be reassembled to resume the delegation.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::InvalidState`] if the captured conversation cannot
    /// be restored.
    pub fn restore_conversation(&self) -> Result<Conversation, FacadeError> {
        Conversation::restore(self.conversation.clone()).map_err(|error| {
            FacadeError::InvalidState(format!("pending delegation restore: {error}"))
        })
    }
}

/// Placeholder snapshot for the shared mailbox.
///
/// Reserved for the collaboration milestone; the base agent path never produces
/// one. The field set is empty for now (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MailboxSnapshot {}

/// Placeholder snapshot for the shared blackboard.
///
/// Reserved for the collaboration milestone; the base agent path never produces
/// one. The field set is empty for now (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BlackboardSnapshot {}

/// The internal parts of an [`Agent`], handed out by
/// [`Agent::into_parts`](super::Agent::into_parts).
///
/// This is an advanced escape hatch: it gives a caller ownership of the
/// assembled [`AgentState`] (which owns the live
/// [`Conversation`](crate::conversation::Conversation)), the LLM client, the
/// registered tools and escape-hatch declarations, the shared approval bridge,
/// and the identity source, so the caller can drive the sans-io layers directly.
pub struct AgentParts {
    /// The assembled agent state, owning the live conversation.
    pub state: AgentState,
    /// The LLM client the run drives.
    pub client: Arc<dyn LlmClient>,
    /// The typed tools registered with the agent.
    pub tools: Vec<Tool>,
    /// The optional escape-hatch [`ToolRegistry`].
    pub custom_registry: Option<Arc<dyn ToolRegistry>>,
    /// Extra tool declarations advertised but executed elsewhere.
    pub extra_declarations: Vec<ToolDecl>,
    /// The shared approval bridge (policy plus interaction handler).
    pub approval: Arc<FacadeApproval>,
    /// The identity source the agent mints run/turn/message ids from.
    pub ids: FacadeIds,
    /// The registered local subagent delegates (data-first recipes).
    pub delegates: Vec<LocalSubagent>,
    /// The delegation routing strategy configured on the agent.
    pub delegation: Delegation,
}

impl std::fmt::Debug for AgentParts {
    /// Prints structural fields while treating runtime handles as opaque.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentParts")
            .field("state", &self.state)
            .field(
                "tools",
                &self.tools.iter().map(Tool::name).collect::<Vec<_>>(),
            )
            .field("has_custom_registry", &self.custom_registry.is_some())
            .field("extra_declarations", &self.extra_declarations.len())
            .finish_non_exhaustive()
    }
}

/// A fluent builder that rebuilds an [`Agent`] from an [`AgentSnapshot`].
///
/// A snapshot is data-only, so the builder re-injects the runtime handles it
/// deliberately omits: the LLM client (through a [`provider`](Self::provider) or
/// an explicit [`client`](Self::client)), the executable [`tool`](Self::tool)s,
/// and the [`approval`](Self::approval) policy. The rebuilt agent continues the
/// snapshotted conversation, so its next run appends to that history.
#[derive(Default)]
pub struct AgentRestoreBuilder {
    snapshot: Option<AgentSnapshot>,
    provider: Option<ProviderConfig>,
    client: Option<Arc<dyn LlmClient>>,
    tools: Vec<Tool>,
    custom_registry: Option<Arc<dyn ToolRegistry>>,
    extra_declarations: Vec<ToolDecl>,
    approval: Option<ApprovalPolicy>,
    ids: Option<FacadeIds>,
    subagent_overrides: Vec<LocalSubagent>,
    external_overrides: Vec<ManagedExternalDelegate>,
    restore_external: RestoreExternal,
}

impl std::fmt::Debug for AgentRestoreBuilder {
    /// Prints structural fields while treating the client as opaque.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentRestoreBuilder")
            .field("has_snapshot", &self.snapshot.is_some())
            .field("provider", &self.provider)
            .field("has_client", &self.client.is_some())
            .field(
                "tools",
                &self.tools.iter().map(Tool::name).collect::<Vec<_>>(),
            )
            .field("has_custom_registry", &self.custom_registry.is_some())
            .field("approval", &self.approval)
            .field(
                "subagent_overrides",
                &self
                    .subagent_overrides
                    .iter()
                    .map(LocalSubagent::name)
                    .collect::<Vec<_>>(),
            )
            .field(
                "external_overrides",
                &self
                    .external_overrides
                    .iter()
                    .map(ManagedExternalDelegate::name)
                    .collect::<Vec<_>>(),
            )
            .field("restore_external", &self.restore_external)
            .finish_non_exhaustive()
    }
}

impl AgentRestoreBuilder {
    /// Sets the [`AgentSnapshot`] to restore from (required).
    #[must_use]
    pub fn snapshot(mut self, snapshot: AgentSnapshot) -> Self {
        self.snapshot = Some(snapshot);
        self
    }

    /// Sets the provider used to construct the client when none is injected.
    ///
    /// Ignored when an explicit [`client`](Self::client) is also set.
    #[must_use]
    pub fn provider(mut self, provider: ProviderConfig) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Injects a concrete client, bypassing provider-based construction.
    ///
    /// This is the recommended path for offline tests.
    #[must_use]
    pub fn client(mut self, client: Arc<dyn LlmClient>) -> Self {
        self.client = Some(client);
        self
    }

    /// Re-injects one typed function [`Tool`].
    ///
    /// The restored [`AgentState`] carries only tool *declarations*; the
    /// executable closures must be re-supplied here and are correlated with the
    /// advertised declarations by name.
    #[must_use]
    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Re-injects an escape-hatch [`ToolRegistry`] whose tools the facade does
    /// not own.
    #[must_use]
    pub fn tool_registry(mut self, registry: Arc<dyn ToolRegistry>) -> Self {
        self.custom_registry = Some(registry);
        self
    }

    /// Re-advertises extra tool declarations executed elsewhere.
    #[must_use]
    pub fn tool_declarations(mut self, declarations: Vec<ToolDecl>) -> Self {
        self.extra_declarations = declarations;
        self
    }

    /// Sets the agent-level approval policy for the restored agent.
    #[must_use]
    pub fn approval(mut self, approval: impl Into<ApprovalPolicy>) -> Self {
        self.approval = Some(approval.into());
        self
    }

    /// Overrides the identity source (mainly for deterministic tests).
    ///
    /// When unset, a fresh source is derived with
    /// [`FacadeIds::continuing_after`], seeded past every id in the restored
    /// history so it cannot re-mint an id that already exists there.
    #[must_use]
    pub fn ids(mut self, ids: FacadeIds) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Re-registers a local subagent delegate, overriding the persisted recipe
    /// of the same `name`.
    ///
    /// A snapshot restores each delegate's data (spec, tools, description) but
    /// not its [`ApprovalPolicy`](crate::facade::ApprovalPolicy) — a possibly
    /// closure-bearing runtime handle. Restored delegates therefore default to
    /// [`ApprovalPolicy::default`]; pass a freshly built
    /// [`Agent::worker`](super::Agent::worker) here to re-supply an approval
    /// policy (or otherwise replace the persisted recipe). The `name` is stamped
    /// onto `worker` exactly as [`AgentBuilder::subagent`](super::AgentBuilder::subagent)
    /// does.
    #[must_use]
    pub fn subagent(mut self, name: impl Into<String>, worker: LocalSubagent) -> Self {
        self.subagent_overrides.push(worker.with_name(name));
        self
    }

    /// Re-registers a managed external delegate, re-supplying the runtime
    /// attachment (session handler, credentials) a snapshot deliberately omits.
    ///
    /// A snapshot restores each external delegate's data-only recipe and its
    /// last-known session facts, but never its runtime session handler (§15.2).
    /// Pass a freshly built [`ManagedExternalAgent`] here — with a
    /// [`session_handler`](crate::facade::ManagedExternalAgentBuilder::session_handler)
    /// when the delegate must be driven or attached — to re-supply that runtime
    /// attachment. The `name` is stamped onto `agent` exactly as
    /// [`AgentBuilder::external_agent`](super::AgentBuilder::external_agent) does.
    /// Re-registration is **required** for
    /// [`RestoreExternal::AttachOrFail`](crate::facade::RestoreExternal::AttachOrFail).
    #[must_use]
    pub fn external_agent(mut self, name: impl Into<String>, agent: ManagedExternalAgent) -> Self {
        self.external_overrides
            .push(ManagedExternalDelegate::new(name, agent));
        self
    }

    /// Sets the policy that reconciles each managed external delegate's recorded
    /// session on restore (`docs/facade-api.md` §15.3).
    ///
    /// Defaults to [`RestoreExternal::MarkInterrupted`], which marks each restored
    /// delegate interrupted without touching any external runtime — the safe
    /// default, since a coding agent may already have changed a worktree.
    #[must_use]
    pub const fn restore_external(mut self, policy: RestoreExternal) -> Self {
        self.restore_external = policy;
        self
    }

    /// Finalizes the builder, rebuilding the [`Agent`] from the snapshot.
    ///
    /// # Errors
    ///
    /// - [`FacadeError::Config`] when no snapshot was set, or when neither an
    ///   explicit client nor a provider was supplied.
    /// - [`FacadeError::DuplicateTool`] when a re-injected tool name collides
    ///   with another tool, an escape-hatch declaration, or the custom registry.
    /// - [`FacadeError::InvalidState`] when the captured
    ///   [`AgentStateSnapshot`] cannot be deserialized back into an
    ///   [`AgentState`].
    pub fn build(self) -> Result<Agent, FacadeError> {
        let snapshot = self
            .snapshot
            .ok_or_else(|| FacadeError::Config("agent restore requires a `snapshot`".to_owned()))?;
        let client = match (self.client, self.provider) {
            (Some(client), _) => client,
            (None, Some(provider)) => client_for_provider(provider),
            (None, None) => {
                return Err(FacadeError::Config(
                    "agent restore needs either a `client` or a `provider`".to_owned(),
                ));
            }
        };

        ensure_unique_tool_names(
            &self.tools,
            &self.extra_declarations,
            self.custom_registry.as_ref(),
        )?;

        // The restored state is authoritative: it preserves the spec, active
        // declarations, model, loop policy, and loop cursor of the snapshotted
        // agent, so a restored run resumes exactly where the snapshot left off.
        let state = snapshot.agent_state.into_state()?;

        // A snapshot carries no runtime id counter, so continue past every id in
        // the restored history unless the caller pins an explicit source.
        let ids = self
            .ids
            .unwrap_or_else(|| FacadeIds::continuing_after(state.conversation()));

        // Rebuild the registered external delegates from their data-only
        // snapshots (a snapshot cannot carry the runtime session handler, §15.2),
        // then apply the caller's re-registrations (which re-supply that handler),
        // replacing the persisted recipe of the same name in place — symmetric to
        // the local-subagent restore above.
        let mut external_agents: Vec<ManagedExternalDelegate> = snapshot
            .external_delegates
            .iter()
            .map(ExternalDelegateSnapshot::to_delegate)
            .collect();
        for override_delegate in self.external_overrides {
            match external_agents
                .iter_mut()
                .find(|existing| existing.name() == override_delegate.name())
            {
                Some(existing) => *existing = override_delegate,
                None => external_agents.push(override_delegate),
            }
        }

        // Reconcile each snapshotted delegate's recorded session under the chosen
        // `restore_external` policy (§15.3).
        let mut last_external_sessions: HashMap<String, RetainedExternalSession> = HashMap::new();
        for snap in &snapshot.external_delegates {
            let retained = match self.restore_external {
                RestoreExternal::MarkInterrupted => RetainedExternalSession {
                    status: ExternalDelegateStatus::Interrupted,
                    session: snap.session.clone(),
                    artifacts: snap.artifacts.clone(),
                },
                RestoreExternal::RestartFromBrief => RetainedExternalSession {
                    status: ExternalDelegateStatus::Pending,
                    session: None,
                    artifacts: Vec::new(),
                },
                RestoreExternal::AttachOrFail => {
                    let attachable = external_agents
                        .iter()
                        .find(|delegate| delegate.name() == snap.name)
                        .is_some_and(|delegate| delegate.agent().session_handler().is_some());
                    if !attachable || snap.session.is_none() {
                        return Err(FacadeError::InvalidState(format!(
                            "restore_external(attach_or_fail): external delegate `{}` cannot be \
                             attached; re-register it with `.external_agent(name, ..)` carrying a \
                             session handler and ensure the snapshot has a resumable session",
                            snap.name
                        )));
                    }
                    RetainedExternalSession {
                        status: snap.status,
                        session: snap.session.clone(),
                        artifacts: snap.artifacts.clone(),
                    }
                }
            };
            last_external_sessions.insert(snap.name.clone(), retained);
        }

        let external_tool_names = snapshot.delegation.external_tool_names(&external_agents);
        let approval = build_facade_approval(
            self.approval.unwrap_or_default(),
            &self.tools,
            external_tool_names,
        );
        let machine = assemble_machine(state, &ids, approval.clone());

        // Rebuild the registered delegates from their data-only snapshots,
        // defaulting each approval policy (a runtime handle the snapshot cannot
        // carry, §15.2). A caller may re-register a delegate through
        // `.subagent(..)` to re-supply an approval policy; a re-registration
        // replaces the persisted recipe of the same name in place.
        let mut delegates: Vec<LocalSubagent> = snapshot
            .delegates
            .into_iter()
            .map(|delegate| delegate.into_delegate(ApprovalPolicy::default()))
            .collect();
        for override_delegate in self.subagent_overrides {
            match delegates
                .iter_mut()
                .find(|existing| existing.name() == override_delegate.name())
            {
                Some(existing) => *existing = override_delegate,
                None => delegates.push(override_delegate),
            }
        }

        Ok(Agent {
            machine,
            client,
            tools: self.tools,
            custom_registry: self.custom_registry,
            extra_declarations: self.extra_declarations,
            approval,
            ids,
            delegates,
            // External delegates are a runtime attachment (their session handler
            // is never serialized). The snapshot carries only data-only recipes
            // plus last-known session facts; the caller re-supplies each runtime
            // through `.external_agent(name, ..)`, and the recorded sessions are
            // reconciled into `last_external_sessions` under the `restore_external`
            // policy above (§15.2, §15.3).
            external_agents,
            delegation: snapshot.delegation,
            last_external_sessions,
        })
    }
}
