//! Managed ACP (Agent Client Protocol) runtime adapter (feature `external-acp`).
//!
//! ACP is a *single standard* protocol (JSON-RPC 2.0 over stdio, wire version
//! [`ACP_WIRE_VERSION`]) stewarded by the neutral `agentclientprotocol` org, not
//! a per-vendor variant: one client implementation talks to every ACP agent
//! (Gemini CLI and OpenCode natively; Claude and Codex through Zed bridge
//! processes). This crate therefore ships **one** adapter rather than one per
//! vendor — the launch line in [`AcpConfig`] selects the agent, and the wire
//! layer is fully shared. Everything here is gated behind the non-default
//! `external-acp` feature, which is the only adapter feature that pulls in real
//! optional dependencies (`agent-client-protocol` and its
//! `agent-client-protocol-schema` protocol types); a default build links neither.
//!
//! This module is filled in across milestone 10:
//!
//! - **M10-1 (this task):** the [`AcpConfig`] launch recipe and the pure
//!   capability-negotiation mapping ([`capabilities_from_initialize`]) that turns
//!   the neutral [`AcpNegotiatedCapabilities`] projection of an `initialize`
//!   handshake into an
//!   [`ExternalRuntimeCapabilities`](crate::agent::external::ExternalRuntimeCapabilities).
//! - **M10-2 (this task):** the client [`connection`] layer ([`AcpLauncher`] /
//!   [`SpawnedAcpAgent`] / [`TokioProcessLauncher`]) and the private
//!   [`session/update` decoder`](decoder) — [`AcpStreamDecoder`] — that
//!   normalizes agent→client messages into sequenced
//!   [`ExternalObservedEvent`](crate::agent::external::ExternalObservedEvent)
//!   observations and per-turn [`AcpDecision`]s while caching the client requests
//!   ([`PendingClientRequest`]) M10-3 must service.
//! - **M10-3 (later):** the live `AcpAdapter` / session that first truly drives
//!   the host-pausable permission bridge.
//!
//! The official crate's raw protocol types are used only inside the adapter and
//! are **never** re-exported as stable `agent-lib` API (design 非目标, mirroring
//! the "no raw frame types" discipline of the three CLI adapters). The capability
//! mapping deliberately takes the neutral [`AcpNegotiatedCapabilities`] projection
//! rather than a schema type so the crate boundary stays inside the adapter; the
//! projection is built from the handshake by the connection layer in later tasks.

mod adapter;
mod config;
mod connection;
mod decoder;

pub use adapter::AcpAdapter;
pub use config::AcpConfig;
pub use connection::{AcpLauncher, SpawnedAcpAgent, TokioProcessLauncher};
pub use decoder::{
    AcpDecision, AcpPermissionOption, AcpPermissionOptionKind, AcpStreamDecoder,
    PendingClientRequest,
};

use crate::agent::external::{ExternalRuntimeCapabilities, ExternalRuntimeKind};

/// ACP wire protocol version negotiated by the pinned crate releases.
///
/// The `agent-client-protocol` / `agent-client-protocol-schema` versions this
/// crate depends on advertise `ProtocolVersion::LATEST == V1`, i.e. wire version
/// `1`. The value is recorded here as a probe/negotiation datum; drift is handled
/// by upgrading the crates and refreshing cassettes rather than by hard-coding an
/// assumption elsewhere.
pub const ACP_WIRE_VERSION: u16 = 1;

/// The free-form [`ExternalRuntimeKind::Custom`] label carried by ACP sessions.
///
/// ACP reuses the existing `Custom(String)` runtime kind rather than introducing
/// a new enum variant (the machine, driver, and state stay unchanged), so every
/// ACP capability set and session reference is tagged with this stable label.
pub const ACP_RUNTIME_LABEL: &str = "acp";

/// Returns the [`ExternalRuntimeKind`] used for ACP-backed sessions.
///
/// This is [`ExternalRuntimeKind::Custom`] carrying [`ACP_RUNTIME_LABEL`]; using a
/// `Custom` label keeps ACP off the named-runtime enum while still routing through
/// the shared capability and registry machinery.
#[must_use]
pub fn acp_runtime_kind() -> ExternalRuntimeKind {
    ExternalRuntimeKind::Custom(ACP_RUNTIME_LABEL.to_owned())
}

/// A provider-neutral projection of the facts an ACP `initialize` handshake
/// settles that affect managed capabilities.
///
/// The official schema types (`AgentCapabilities`, `ClientCapabilities`) stay
/// inside the adapter; the connection layer (M10-2/M10-3) projects a completed
/// handshake down to this small struct, and [`capabilities_from_initialize`] maps
/// it to an [`ExternalRuntimeCapabilities`]. Keeping the mapping input neutral is
/// what stops the crate's raw protocol types from leaking into stable API.
///
/// Only fields that influence a managed capability bit are captured. Note that
/// `fs`/`terminal` are **recorded** because the client advertises them as
/// environment services, but they do **not** imply host-tool injection: the first
/// ACP version reports [`host_tools`](ExternalRuntimeCapabilities::host_tools) as
/// `false` (there is no client MCP bridge), and the adapter fulfils `fs/*` and
/// `terminal/*` directly against the worktree instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AcpNegotiatedCapabilities {
    /// The agent advertised support for `session/load` (resumable sessions).
    pub load_session: bool,
    /// The client advertised file-system (`fs/*`) environment services.
    pub fs: bool,
    /// The client advertised terminal (`terminal/*`) environment services.
    pub terminal: bool,
}

impl AcpNegotiatedCapabilities {
    /// A minimal handshake advertising none of the negotiable capabilities.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            load_session: false,
            fs: false,
            terminal: false,
        }
    }

    /// Marks `session/load` (resume) support.
    #[must_use]
    pub const fn with_load_session(mut self, load_session: bool) -> Self {
        self.load_session = load_session;
        self
    }

    /// Marks advertised `fs/*` client environment services.
    #[must_use]
    pub const fn with_fs(mut self, fs: bool) -> Self {
        self.fs = fs;
        self
    }

    /// Marks advertised `terminal/*` client environment services.
    #[must_use]
    pub const fn with_terminal(mut self, terminal: bool) -> Self {
        self.terminal = terminal;
        self
    }
}

/// Maps a negotiated ACP `initialize` handshake to managed capabilities.
///
/// The result starts from the conservative all-unsupported baseline
/// ([`ExternalRuntimeCapabilities::none`]) and only turns on bits that are either
/// **guaranteed by the ACP protocol** for any conformant agent or **confirmed by
/// negotiation**:
///
/// - `streaming` — always `true`: `session/update` notifications stream progress.
/// - `permission_bridge` — always `true`: ACP defines `session/request_permission`
///   as an agent→client request, the first adapter to light the host-pausable arm.
/// - `graceful_shutdown` — always `true`: `session/cancel` plus a clean connection
///   close end a session without residual side effects.
/// - `resume` — mirrors [`load_session`](AcpNegotiatedCapabilities::load_session):
///   on only when the agent advertised `session/load`.
///
/// Everything else stays `false` in this first version:
/// `host_tools`/`host_subagents` (no client MCP bridge — advertised `fs`/`terminal`
/// are recorded on the projection but never widen these), and `artifacts`/`usage`
/// (the live adapter in M10-3 turns these on only if the crate actually surfaces
/// them). `reconfigure` (mid-turn live tool-swap) also stays `false`.
///
/// This is a pure function: it performs no IO and does not touch the process
/// environment, so the handshake→capability contract is unit-testable offline.
#[must_use]
pub fn capabilities_from_initialize(
    negotiated: &AcpNegotiatedCapabilities,
) -> ExternalRuntimeCapabilities {
    let mut capabilities = ExternalRuntimeCapabilities::none(acp_runtime_kind());
    // Protocol-guaranteed bits for any conformant ACP agent.
    capabilities.streaming = true;
    capabilities.permission_bridge = true;
    capabilities.graceful_shutdown = true;
    // The single negotiated bit that maps to a managed capability.
    capabilities.resume = negotiated.load_session;
    // `fs`/`terminal` are recorded on the projection but intentionally do not
    // imply host-tool injection in this version.
    capabilities
}

#[cfg(test)]
mod tests {
    use super::{
        ACP_RUNTIME_LABEL, ACP_WIRE_VERSION, AcpNegotiatedCapabilities, acp_runtime_kind,
        capabilities_from_initialize,
    };
    use crate::agent::external::{ExternalCapability, ExternalRuntimeKind};

    #[test]
    fn acp_wire_version_and_runtime_label_are_stable() {
        assert_eq!(ACP_WIRE_VERSION, 1);
        assert_eq!(ACP_RUNTIME_LABEL, "acp");
        assert_eq!(
            acp_runtime_kind(),
            ExternalRuntimeKind::Custom("acp".to_owned())
        );
    }

    #[test]
    fn acp_capabilities_from_initialize() {
        // A handshake advertising loadSession + fs: resume follows loadSession,
        // the three protocol-guaranteed bits are on, and advertised fs does NOT
        // widen host_tools.
        let negotiated = AcpNegotiatedCapabilities::none()
            .with_load_session(true)
            .with_fs(true);
        let caps = capabilities_from_initialize(&negotiated);

        assert_eq!(caps.runtime, acp_runtime_kind());
        assert!(caps.supports(ExternalCapability::Resume));
        assert!(caps.supports(ExternalCapability::PermissionBridge));
        assert!(caps.supports(ExternalCapability::Streaming));
        assert!(caps.supports(ExternalCapability::GracefulShutdown));
        assert!(!caps.supports(ExternalCapability::HostTools));
        assert!(!caps.supports(ExternalCapability::HostSubagents));
        assert!(!caps.supports(ExternalCapability::Artifacts));
        assert!(!caps.supports(ExternalCapability::Usage));
        assert!(!caps.supports(ExternalCapability::Reconfigure));

        // An empty/minimal handshake: only the protocol-guaranteed bits are true;
        // resume is off because loadSession was not advertised.
        let minimal = capabilities_from_initialize(&AcpNegotiatedCapabilities::none());
        assert!(minimal.supports(ExternalCapability::Streaming));
        assert!(minimal.supports(ExternalCapability::PermissionBridge));
        assert!(minimal.supports(ExternalCapability::GracefulShutdown));
        assert!(!minimal.supports(ExternalCapability::Resume));
        for capability in [
            ExternalCapability::HostTools,
            ExternalCapability::HostSubagents,
            ExternalCapability::Artifacts,
            ExternalCapability::Usage,
            ExternalCapability::Reconfigure,
        ] {
            assert!(
                !minimal.supports(capability),
                "minimal handshake must not claim {capability}"
            );
        }

        // Advertised terminal alone still does not imply host tools.
        let terminal_only =
            capabilities_from_initialize(&AcpNegotiatedCapabilities::none().with_terminal(true));
        assert!(!terminal_only.supports(ExternalCapability::HostTools));
    }
}
