//! Machine-local configuration for an [`ExternalAgentMachine`].
//!
//! [`ExternalSessionPolicy`](super::ExternalSessionPolicy) is the *runtime-facing*
//! half of managed configuration: it travels inside every
//! [`ExternalSessionRequest`](super::ExternalSessionRequest) as a hint the
//! handler forwards to the backing runtime (permission mode, worktree isolation,
//! turn cap, stream policy). This module holds the *machine-local* half:
//! [`ExternalAgentMachineConfig`] gathers the policy knobs the
//! [`ExternalAgentMachine`](super::ExternalAgentMachine) itself enforces while it
//! bridges runtime pauses to host requirements. Splitting the two keeps the
//! machine constructor from ballooning and keeps runtime hints from leaking
//! machine-only decisions (design §7).
//!
//! The config is *plain data*: it carries no live handler, sink, id source, or
//! task handle, so it round-trips through serde and never enters the serializable
//! [`ExternalAgentState`](super::ExternalAgentState) — the live identity sources
//! ([`RequirementIds`](crate::agent::RequirementIds) /
//! [`ToolExecutionIds`](crate::agent::ToolExecutionIds)) stay behind their own
//! builder injections. The default is deliberately permissive so a machine built
//! without a config behaves exactly as it did before this milestone.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::ExternalCapability;

/// How the machine reacts when a bridged host tool call fails to execute.
///
/// A runtime pause for tool calls is bridged into host
/// [`NeedTool`](crate::agent::RequirementKind::NeedTool) requirements; when one
/// resolves with a [`ToolRuntimeError`](crate::agent::ToolRuntimeError) this
/// policy decides whether the failure is handed back to the runtime as a failed
/// tool result or stops the host turn (design §8.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalToolFailurePolicy {
    /// Relay the failure to the runtime as a failed
    /// [`ExternalToolResult`](super::ExternalToolResult) and let the runtime
    /// decide how to react. This is the default and matches the pre-M4-3
    /// behavior: the external runtime, not the host, owns tool-loop control.
    #[default]
    ReturnErrorToRuntime,
    /// Stop the host turn on a classified error cursor instead of relaying the
    /// failure, discarding the pending turn. Use this for strict runs that must
    /// abort rather than continue after a host tool failure.
    StopRun,
}

/// Machine-local policy knobs applied while driving one external-agent machine.
///
/// This is the external-agent counterpart of the internal machine's policy
/// bundle. It is pure data — no live handler, sink, or id source — so it is a
/// serde DTO that round-trips and stays out of
/// [`ExternalAgentState`](super::ExternalAgentState). Injected through
/// [`ExternalAgentMachine::with_external_config`](super::ExternalAgentMachine::with_external_config)
/// (or the focused `with_*` setters), it covers:
///
/// - [`tool_failure`](Self::tool_failure): how a failed host tool call is handled.
/// - [`required_capabilities`](Self::required_capabilities): the managed features
///   this run depends on, expressed as an [`ExternalCapability`] set so a
///   decision point the host cannot service fails loudly with
///   [`UnsupportedCapability`](super::ExternalAgentError::UnsupportedCapability)
///   rather than a generic error (design §15).
/// - [`max_decision_loops`](Self::max_decision_loops): a bound on how many times
///   the machine may hand control back to the runtime, so an unbounded
///   pause/respond loop fails with
///   [`LimitExceeded`](super::ExternalAgentError::LimitExceeded) (design §6.3).
///
/// [`Default`] is permissive: `ReturnErrorToRuntime`, no required capabilities,
/// and no loop bound, so a machine built without a config behaves as it did
/// before this configuration was introduced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalAgentMachineConfig {
    /// How a failed bridged host tool call is handled.
    #[serde(default)]
    tool_failure: ExternalToolFailurePolicy,
    /// Managed features this run requires the runtime/host to support.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    required_capabilities: BTreeSet<ExternalCapability>,
    /// Optional cap on the number of runtime decision loops (session
    /// round-trips) for the machine's whole lifetime; `None` is unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_decision_loops: Option<u32>,
}

impl ExternalAgentMachineConfig {
    /// Creates a permissive machine config equal to [`Default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets how a failed bridged host tool call is handled.
    #[must_use]
    pub fn with_tool_failure_policy(mut self, policy: ExternalToolFailurePolicy) -> Self {
        self.tool_failure = policy;
        self
    }

    /// Sets the cap on runtime decision loops; `None` clears the bound.
    #[must_use]
    pub const fn with_max_decision_loops(mut self, max: Option<u32>) -> Self {
        self.max_decision_loops = max;
        self
    }

    /// Adds one capability this run requires the runtime/host to support.
    #[must_use]
    pub fn require_capability(mut self, capability: ExternalCapability) -> Self {
        self.required_capabilities.insert(capability);
        self
    }

    /// Requires host-tool bridging support
    /// ([`ExternalCapability::HostTools`]).
    #[must_use]
    pub fn require_host_tools(self) -> Self {
        self.require_capability(ExternalCapability::HostTools)
    }

    /// Requires host-subagent bridging support
    /// ([`ExternalCapability::HostSubagents`]).
    #[must_use]
    pub fn require_subagents(self) -> Self {
        self.require_capability(ExternalCapability::HostSubagents)
    }

    /// Returns the configured tool-failure behavior.
    #[must_use]
    pub const fn tool_failure(&self) -> ExternalToolFailurePolicy {
        self.tool_failure
    }

    /// Reports whether `capability` is required by this run.
    #[must_use]
    pub fn requires(&self, capability: ExternalCapability) -> bool {
        self.required_capabilities.contains(&capability)
    }

    /// Returns the set of capabilities this run requires.
    #[must_use]
    pub const fn required_capabilities(&self) -> &BTreeSet<ExternalCapability> {
        &self.required_capabilities
    }

    /// Returns the configured decision-loop bound, if any.
    #[must_use]
    pub const fn max_decision_loops(&self) -> Option<u32> {
        self.max_decision_loops
    }
}

#[cfg(test)]
mod tests {
    use super::{ExternalAgentMachineConfig, ExternalToolFailurePolicy};
    use crate::agent::external::ExternalCapability;

    #[test]
    fn external_machine_config_defaults_are_permissive() {
        // The default config keeps the pre-configuration behavior: relay tool
        // failures, require nothing, and never bound the decision loop.
        let config = ExternalAgentMachineConfig::default();
        assert_eq!(
            config.tool_failure(),
            ExternalToolFailurePolicy::ReturnErrorToRuntime
        );
        assert!(config.max_decision_loops().is_none());
        assert!(config.required_capabilities().is_empty());
        for capability in ExternalCapability::ALL {
            assert!(!config.requires(capability));
        }
    }

    #[test]
    fn external_machine_config_roundtrip() {
        // A populated config carries every machine-local knob and survives a
        // serde round-trip as a plain DTO.
        let config = ExternalAgentMachineConfig::new()
            .with_tool_failure_policy(ExternalToolFailurePolicy::StopRun)
            .with_max_decision_loops(Some(16))
            .require_host_tools()
            .require_subagents();

        assert_eq!(config.tool_failure(), ExternalToolFailurePolicy::StopRun);
        assert_eq!(config.max_decision_loops(), Some(16));
        assert!(config.requires(ExternalCapability::HostTools));
        assert!(config.requires(ExternalCapability::HostSubagents));
        assert!(!config.requires(ExternalCapability::Streaming));

        let encoded = serde_json::to_value(&config).expect("serialize config");
        assert_eq!(encoded["tool_failure"], serde_json::json!("stop_run"));
        assert_eq!(encoded["max_decision_loops"], serde_json::json!(16));
        assert_eq!(
            encoded["required_capabilities"],
            serde_json::json!(["host_tools", "host_subagents"])
        );
        let decoded: ExternalAgentMachineConfig =
            serde_json::from_value(encoded).expect("deserialize config");
        assert_eq!(decoded, config);

        // The permissive default serializes to an empty object: absent knobs are
        // skipped so the common shape stays compact.
        let default_encoded =
            serde_json::to_value(ExternalAgentMachineConfig::default()).expect("serialize default");
        assert_eq!(
            default_encoded,
            serde_json::json!({ "tool_failure": "return_error_to_runtime" })
        );
    }
}
