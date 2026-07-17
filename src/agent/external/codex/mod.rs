//! Managed Codex runtime adapter (feature `external-codex`).
//!
//! This module backs the OpenAI Codex CLI as an
//! [`ExternalRuntimeAdapter`](crate::agent::external::ExternalRuntimeAdapter),
//! gated behind the non-default `external-codex` feature so the core crate stays
//! free of CLI-adapter machinery unless a host opts in. It is filled in across
//! milestone 7:
//!
//! - **M7-1 (this task):** [`CodexConfig`] launch configuration and the [`probe`]
//!   capability probe. The probe never assumes Codex is installed or usable — it
//!   classifies a missing/broken binary as
//!   [`Launch`](crate::agent::external::ExternalAgentError::Launch) and a binary
//!   lacking the structured `codex exec --json` event stream as
//!   [`UnsupportedCapability`](crate::agent::external::ExternalAgentError::UnsupportedCapability),
//!   returning a conservatively-detected
//!   [`ExternalRuntimeCapabilities`](crate::agent::external::ExternalRuntimeCapabilities)
//!   otherwise.
//! - **M7-2 (this task):** the private [`codex exec --json` decoder`](decoder)
//!   turning raw CLI frames into sequenced
//!   [`ExternalObservedEvent`](crate::agent::external::ExternalObservedEvent)
//!   observations and per-turn [`CodexDecision`]s.
//! - **M7-3 (later):** the live
//!   [`ExternalRuntimeSession`](crate::agent::external::ExternalRuntimeSession)
//!   process management that wraps the decoder into start/resume/advance.
//!
//! The [`CodexConfig`] captures the CLI's split-flag layout: the approval policy
//! is a top-level flag placed before the `exec` subcommand, while the sandbox
//! policy and the `--json` stream are `exec` flags after it (design §12). Nothing
//! here parses or re-exports Codex's private wire schema as stable public API
//! (design 非目标): the probe reads only `--version` / `--help` / `exec --help`.

mod config;
mod decoder;
mod probe;

pub use config::CodexConfig;
pub use decoder::{CodexDecision, CodexDecodeContext, CodexStreamDecoder};
pub use probe::{CodexProbeExec, CodexProbeOutput, SystemCodexExec, probe, probe_with_exec};
