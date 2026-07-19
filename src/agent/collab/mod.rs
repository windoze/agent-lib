//! Collaboration vertical features and their bridge tool adapters (Milestone 6-3).
//!
//! The mixed-agent scheduler needs internal, external (Claude Code / Codex /
//! OpenCode), and host-driven agents to coordinate through **one** protocol so
//! their work is observable, testable, and replayable across runtimes
//! (`external-agent.md` §3.5). This module provides that protocol as three
//! first-class, API-first vertical features plus the thin tool adapters that
//! expose them to a model (`agent-layer.md` §5, §6.2–§6.4):
//!
//! - [`Plan`] — a stateful task board with `depends_on` edges, CAS-guarded claim
//!   with dependency-completion checks, a claim-first entry, and status updates
//!   (§6.2).
//! - [`Blackboard`] — an append-only, namespaced, monotonic message log (§6.4).
//! - [`Mailbox`] — an optional directed agent-to-agent message layer (§3.5).
//!
//! [`bridge_tool_set`] packages the model-facing declarations for injection as an
//! agent's `initial_tools`, [`CollabToolHandler`] executes the inline tools under
//! the host's [`RunContext`](crate::agent::RunContext) guards and the injected
//! agent identity, and [`SpawnAgentRequest`] translates a `spawn_agent` call into
//! a [`RequirementKind::NeedSubagent`](crate::agent::RequirementKind::NeedSubagent)
//! so the existing subagent path derives the child (§3.4, §6.3).

pub mod blackboard;
pub mod mailbox;
pub mod plan;
pub mod tools;

#[cfg(test)]
mod tests;

pub use blackboard::{Blackboard, BlackboardSnapshot, BoardMessage, DEFAULT_CHANNEL};
pub use mailbox::{MailMessage, Mailbox, MailboxSnapshot};
pub use plan::{Plan, PlanError, PlanSnapshot, TaskSnapshot, TaskStatus};
pub use tools::{
    ArtifactSink, BLACKBOARD_POST, BLACKBOARD_READ, CollabToolHandler, MAILBOX_READ, PLAN_ADD_TASK,
    PLAN_CLAIM, PLAN_CLAIM_FIRST_AVAILABLE, PLAN_READ, PLAN_UPDATE, REPORT_ARTIFACT, RUN_HOST_TOOL,
    RecordingArtifactSink, SEND_MESSAGE, SPAWN_AGENT, SpawnAgentRequest, ToolAdapterError,
    bridge_tool_declarations, bridge_tool_set,
};
