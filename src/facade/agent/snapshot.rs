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
//! The delegate, mailbox, blackboard, plan, and artifact slices of an
//! [`AgentSnapshot`] are reserved for later milestones; the base agent path
//! produces empty slices here so the struct shape is stable now.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::{AgentState, PlanSnapshot, ToolRegistry};
use crate::client::LlmClient;
use crate::conversation::ConversationSnapshot;
use crate::facade::approval::{ApprovalPolicy, FacadeApproval};
use crate::facade::chat::client_for_provider;
use crate::facade::config::ProviderConfig;
use crate::facade::delegate::LocalSubagent;
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::ArtifactRef;
use crate::facade::tool::{Tool, ensure_unique_tool_names};
use crate::model::tool::Tool as ToolDecl;

use super::{Agent, assemble_machine, build_facade_approval};

/// A serializable, data-only snapshot of an [`Agent`]'s supervisor state.
///
/// The base agent path fills only [`supervisor`](Self::supervisor) and
/// [`agent_state`](Self::agent_state); the delegate, mailbox, blackboard, plan,
/// and artifact slices are reserved for later milestones and are empty here.
///
/// The type is `Clone`/`PartialEq`/`Serialize`/`Deserialize` but deliberately not
/// `Eq`: [`AgentStateSnapshot`] captures the state as a `serde_json::Value`,
/// whose numeric leaves are not totally comparable.
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
    /// Local subagent delegate snapshots (reserved; empty on the base path).
    pub delegates: Vec<DelegateSnapshot>,
    /// In-flight delegation snapshots (reserved; empty on the base path).
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
    /// Captures a data-only snapshot of `state`.
    ///
    /// The supervisor conversation is snapshotted first so an in-flight
    /// (uncommitted) turn surfaces as a clean [`FacadeError::Conversation`]
    /// before the whole state is serialized.
    pub(super) fn capture(state: &AgentState) -> Result<Self, FacadeError> {
        let supervisor = state.conversation().snapshot()?;
        let agent_state = AgentStateSnapshot::capture(state)?;
        Ok(Self {
            supervisor,
            agent_state,
            delegates: Vec::new(),
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

/// Placeholder snapshot for one local subagent delegate.
///
/// Reserved for the subagent milestone; the base agent path never produces one.
/// The field set is empty for now (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DelegateSnapshot {}

/// Placeholder snapshot for one in-flight delegation.
///
/// Reserved for the subagent milestone; the base agent path never produces one.
/// The field set is empty for now (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DelegationSnapshot {}

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
            delegates: Vec::new(),
        })
    }
}
