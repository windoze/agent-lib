//! Session-level capability model for managed external runtimes.
//!
//! The dispatcher already routes work by task-shaped [`Capability`](super::Capability)
//! (search, shell, review, …). That is about *what kind of task* a worker is good
//! at. This module answers a different question: *which managed features a
//! concrete runtime session can actually fulfill* — live streaming, session
//! resume, host-tool injection, subagent bridging, and so on.
//!
//! A managed runtime is never assumed to support a feature. Capabilities start
//! from a conservative all-unsupported baseline
//! ([`ExternalRuntimeCapabilities::none`]) and are only turned on by a real probe
//! or adapter declaration (design §15, PLAN 非目标 "能力差异通过 capability model
//! 显式暴露，不能静默假装支持"). When the machine reaches a decision point a
//! runtime cannot serve, it raises the classified
//! [`ExternalAgentError::UnsupportedCapability`](super::ExternalAgentError::UnsupportedCapability)
//! rather than silently degrading, so a scheduler can avoid dispatching that
//! worker again.

use serde::{Deserialize, Serialize};

use super::{ExternalAgentError, ExternalRuntimeKind};

/// One managed feature an external runtime session may or may not support.
///
/// Each variant names a decision point or bypass the
/// [`ExternalAgentMachine`](super::ExternalAgentMachine) can reach. It is a
/// coarse, provider-neutral selector used for capability gating, classified
/// errors, and test assertions — not a wire protocol. The set mirrors the review
/// checklist for milestone 4 (streaming, resume, permission bridge, host tools,
/// host subagents, artifacts, usage, graceful shutdown).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalCapability {
    /// Forwarding fine-grained events to a live [`ExternalEventSink`](super::ExternalEventSink).
    Streaming,
    /// Resuming a prior session from a stored reference/token.
    Resume,
    /// Bridging runtime permission/interaction pauses to host approvals.
    PermissionBridge,
    /// Injecting host-provided tools the runtime can call.
    HostTools,
    /// Bridging runtime spawn requests to host-managed subagents.
    HostSubagents,
    /// Reporting produced artifacts (patches, files) back to the host.
    Artifacts,
    /// Reporting token/cost usage for budget charging.
    Usage,
    /// Shutting the session down cleanly without residual side effects.
    GracefulShutdown,
}

impl ExternalCapability {
    /// Every capability, in a stable order, for exhaustive iteration.
    ///
    /// Building a capability matrix or asserting round-trips over the full set
    /// should use this rather than hand-listing variants, so a newly added
    /// capability is covered automatically once this array is extended.
    pub const ALL: [ExternalCapability; 8] = [
        ExternalCapability::Streaming,
        ExternalCapability::Resume,
        ExternalCapability::PermissionBridge,
        ExternalCapability::HostTools,
        ExternalCapability::HostSubagents,
        ExternalCapability::Artifacts,
        ExternalCapability::Usage,
        ExternalCapability::GracefulShutdown,
    ];

    /// Returns the stable, human-readable label for this capability.
    ///
    /// The label matches the serde representation and carries no runtime output,
    /// so it is safe to embed in diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ExternalCapability::Streaming => "streaming",
            ExternalCapability::Resume => "resume",
            ExternalCapability::PermissionBridge => "permission_bridge",
            ExternalCapability::HostTools => "host_tools",
            ExternalCapability::HostSubagents => "host_subagents",
            ExternalCapability::Artifacts => "artifacts",
            ExternalCapability::Usage => "usage",
            ExternalCapability::GracefulShutdown => "graceful_shutdown",
        }
    }
}

impl std::fmt::Display for ExternalCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The managed features a concrete external runtime session can fulfill.
///
/// A capability set is attached to a runtime so a host can gate features before
/// or during a managed run. The baseline is conservative:
/// [`none`](Self::none) reports every feature as unsupported, and probes or
/// adapters flip on only what they have verified. This keeps the crate from
/// silently pretending a runtime supports host-tool injection or resume when it
/// does not (design §15).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRuntimeCapabilities {
    /// Runtime these capabilities describe.
    pub runtime: ExternalRuntimeKind,
    /// Forwards fine-grained events to a live sink as they arrive.
    pub streaming: bool,
    /// Resumes a prior session from a stored reference/token.
    pub resume: bool,
    /// Bridges runtime permission/interaction pauses to host approvals.
    pub permission_bridge: bool,
    /// Accepts host-provided tools the runtime can call.
    pub host_tools: bool,
    /// Bridges runtime spawn requests to host-managed subagents.
    pub host_subagents: bool,
    /// Reports produced artifacts back to the host.
    pub artifacts: bool,
    /// Reports token/cost usage for budget charging.
    pub usage: bool,
    /// Shuts the session down cleanly without residual side effects.
    pub graceful_shutdown: bool,
}

impl ExternalRuntimeCapabilities {
    /// Builds a conservative capability set that reports **no** managed feature
    /// as supported.
    ///
    /// This is the safe starting point before a probe runs: every feature is
    /// off, so nothing is assumed until it is explicitly enabled. See
    /// [`ExternalRuntimeKind::conservative_capabilities`].
    #[must_use]
    pub fn none(runtime: ExternalRuntimeKind) -> Self {
        Self {
            runtime,
            streaming: false,
            resume: false,
            permission_bridge: false,
            host_tools: false,
            host_subagents: false,
            artifacts: false,
            usage: false,
            graceful_shutdown: false,
        }
    }

    /// Reports whether `capability` is supported by this runtime.
    #[must_use]
    pub fn supports(&self, capability: ExternalCapability) -> bool {
        match capability {
            ExternalCapability::Streaming => self.streaming,
            ExternalCapability::Resume => self.resume,
            ExternalCapability::PermissionBridge => self.permission_bridge,
            ExternalCapability::HostTools => self.host_tools,
            ExternalCapability::HostSubagents => self.host_subagents,
            ExternalCapability::Artifacts => self.artifacts,
            ExternalCapability::Usage => self.usage,
            ExternalCapability::GracefulShutdown => self.graceful_shutdown,
        }
    }

    /// Builds a classified [`ExternalAgentError::UnsupportedCapability`] naming
    /// this runtime and `capability`.
    ///
    /// External errors in this crate are carried as values into
    /// [`ExternalSessionResult::Failed`](super::ExternalSessionResult::Failed) or
    /// a machine cursor rather than returned as `Result` errors, so a caller that
    /// reaches an unsupported decision point uses this to produce the failure
    /// value it will surface. `detail` should be a stable diagnostic describing
    /// *why* the capability was needed; it must not embed raw prompt text or tool
    /// input, since the error is shown to the host and logs.
    #[must_use]
    pub fn unsupported(
        &self,
        capability: ExternalCapability,
        detail: impl Into<String>,
    ) -> ExternalAgentError {
        ExternalAgentError::UnsupportedCapability {
            runtime: self.runtime.clone(),
            capability,
            detail: detail.into(),
        }
    }
}

impl ExternalRuntimeKind {
    /// Returns a conservative capability set for this runtime with every managed
    /// feature reported as unsupported.
    ///
    /// Adapters override individual fields once a probe confirms support; until
    /// then nothing is assumed (design §15).
    #[must_use]
    pub fn conservative_capabilities(&self) -> ExternalRuntimeCapabilities {
        ExternalRuntimeCapabilities::none(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{ExternalCapability, ExternalRuntimeCapabilities};
    use crate::agent::external::{ExternalAgentError, ExternalRuntimeKind};

    #[test]
    fn external_capabilities_roundtrip() {
        // A conservative baseline supports nothing and survives a JSON round-trip.
        let baseline = ExternalRuntimeKind::ClaudeCode.conservative_capabilities();
        assert_eq!(
            baseline,
            ExternalRuntimeCapabilities::none(ExternalRuntimeKind::ClaudeCode)
        );
        for capability in ExternalCapability::ALL {
            assert!(
                !baseline.supports(capability),
                "conservative baseline must not claim {capability}"
            );
        }
        let encoded = serde_json::to_value(&baseline).expect("serialize baseline");
        let decoded: ExternalRuntimeCapabilities =
            serde_json::from_value(encoded).expect("deserialize baseline");
        assert_eq!(decoded, baseline);

        // A fully populated set round-trips too, and `supports` reflects each
        // field exactly.
        let full = ExternalRuntimeCapabilities {
            runtime: ExternalRuntimeKind::Custom("bespoke-cli".to_owned()),
            streaming: true,
            resume: true,
            permission_bridge: true,
            host_tools: true,
            host_subagents: true,
            artifacts: true,
            usage: true,
            graceful_shutdown: true,
        };
        for capability in ExternalCapability::ALL {
            assert!(
                full.supports(capability),
                "full set must support {capability}"
            );
        }
        let encoded = serde_json::to_value(&full).expect("serialize full");
        let decoded: ExternalRuntimeCapabilities =
            serde_json::from_value(encoded).expect("deserialize full");
        assert_eq!(decoded, full);

        // The capability enum serializes as its stable snake_case label.
        assert_eq!(
            serde_json::to_value(ExternalCapability::HostSubagents).expect("serialize capability"),
            serde_json::json!("host_subagents"),
        );
        assert_eq!(ExternalCapability::HostSubagents.as_str(), "host_subagents");
    }

    #[test]
    fn unsupported_builds_classified_capability_error() {
        // A supported field is honored by `supports`; an unsupported one produces
        // a classified error value naming the runtime and capability.
        let mut caps = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::Codex);
        caps.streaming = true;

        assert!(caps.supports(ExternalCapability::Streaming));
        assert!(!caps.supports(ExternalCapability::HostTools));

        let error = caps.unsupported(ExternalCapability::HostTools, "needs apply_patch bridge");
        match error {
            ExternalAgentError::UnsupportedCapability {
                runtime,
                capability,
                detail,
            } => {
                assert_eq!(runtime, ExternalRuntimeKind::Codex);
                assert_eq!(capability, ExternalCapability::HostTools);
                assert_eq!(detail, "needs apply_patch bridge");
            }
            other => panic!("expected UnsupportedCapability, got {other:?}"),
        }
    }
}
