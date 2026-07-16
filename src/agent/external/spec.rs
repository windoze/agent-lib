//! Static configuration data for an external-agent machine.
//!
//! [`ExternalAgentSpec`] is the external-agent counterpart of
//! [`AgentSpec`](crate::agent::AgentSpec): a data-only recipe that records stable
//! identity, the backing runtime, worktree boundary, initial tool declarations,
//! and per-session policy. It holds no live [`Conversation`](crate::conversation::Conversation),
//! runtime process, SDK client, or task handle; those stay behind the
//! [`ExternalRuntimeHandles`](super::ExternalRuntimeHandles) boundary (design
//! §4.1/§4.3).

use crate::agent::{
    AgentId,
    external::{ExternalRuntimeKind, ExternalSessionPolicy, WorkerProfileRef},
    spec::{ToolSetRef, WorktreeRef},
};
use serde::{Deserialize, Serialize};

/// Data-only recipe for constructing or restoring an external-agent runtime.
///
/// `ExternalAgentSpec` is a template that records stable identity, the backing
/// [`ExternalRuntimeKind`], the worktree boundary, an optional
/// [`WorkerProfileRef`] into the mixed-agent scheduler's
/// [`WorkerProfileRegistry`](super::WorkerProfileRegistry), the initial tool
/// declarations exposed to the runtime, and the per-session policy. It does not
/// hold a live session, process, SDK client, tool registry, or task handle. The
/// profile is an `Option` so external-agent execution stays decoupled from
/// scheduling when no profile applies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalAgentSpec {
    id: AgentId,
    runtime: ExternalRuntimeKind,
    worktree: WorktreeRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<WorkerProfileRef>,
    initial_tools: ToolSetRef,
    session_policy: ExternalSessionPolicy,
}

impl ExternalAgentSpec {
    /// Creates a static external-agent recipe from caller-supplied data.
    #[must_use]
    pub const fn new(
        id: AgentId,
        runtime: ExternalRuntimeKind,
        worktree: WorktreeRef,
        profile: Option<WorkerProfileRef>,
        initial_tools: ToolSetRef,
        session_policy: ExternalSessionPolicy,
    ) -> Self {
        Self {
            id,
            runtime,
            worktree,
            profile,
            initial_tools,
            session_policy,
        }
    }

    /// Returns the externally supplied agent identity.
    #[must_use]
    pub const fn id(&self) -> AgentId {
        self.id
    }

    /// Returns the runtime that backs this external agent.
    #[must_use]
    pub const fn runtime(&self) -> &ExternalRuntimeKind {
        &self.runtime
    }

    /// Returns the worktree boundary configured for this agent.
    #[must_use]
    pub const fn worktree(&self) -> &WorktreeRef {
        &self.worktree
    }

    /// Returns the reserved worker-profile reference, if one was configured.
    #[must_use]
    pub const fn profile(&self) -> Option<&WorkerProfileRef> {
        self.profile.as_ref()
    }

    /// Returns the initial tool-set declaration reference.
    #[must_use]
    pub const fn initial_tools(&self) -> &ToolSetRef {
        &self.initial_tools
    }

    /// Returns the per-session policy applied to this agent's sessions.
    #[must_use]
    pub const fn session_policy(&self) -> &ExternalSessionPolicy {
        &self.session_policy
    }
}
