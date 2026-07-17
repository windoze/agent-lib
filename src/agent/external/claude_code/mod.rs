//! Managed Claude Code runtime adapter (feature `external-claude-code`).
//!
//! This module is the first real [`ExternalRuntimeAdapter`](crate::agent::external::ExternalRuntimeAdapter)
//! backing, gated behind the non-default `external-claude-code` feature so the
//! core crate stays free of CLI-adapter machinery unless a host opts in. It is
//! filled in across milestone 6:
//!
//! - **M6-1 (this task):** [`ClaudeCodeConfig`] launch configuration and the
//!   [`probe`] capability probe. The probe never assumes Claude Code is
//!   installed or usable — it classifies a missing/broken binary as
//!   [`Launch`](crate::agent::external::ExternalAgentError::Launch) and a binary
//!   lacking the structured `stream-json` protocol as
//!   [`UnsupportedCapability`](crate::agent::external::ExternalAgentError::UnsupportedCapability),
//!   returning a conservatively-detected
//!   [`ExternalRuntimeCapabilities`](crate::agent::external::ExternalRuntimeCapabilities)
//!   otherwise.
//! - **M6-2 (this task):** the private [`stream-json` decoder`](decoder) turning
//!   raw CLI frames into sequenced
//!   [`ExternalObservedEvent`](crate::agent::external::ExternalObservedEvent)
//!   observations and per-turn [`ClaudeDecision`]s.
//! - **M6-3 (later):** the live
//!   [`ExternalRuntimeSession`](crate::agent::external::ExternalRuntimeSession)
//!   process management that wraps the decoder into start/resume/advance.
//!
//! Nothing here parses or re-exports Claude Code's private wire schema as stable
//! public API (design 非目标): the probe reads only `--version` / `--help`, and
//! the decoder navigates the raw frames through [`serde_json::Value`] without
//! ever exposing a raw frame type — its outputs are the provider-neutral
//! observation and decision DTOs.

mod config;
mod decoder;
mod probe;

pub use config::ClaudeCodeConfig;
pub use decoder::{ClaudeDecision, ClaudeDecodeContext, ClaudeStreamDecoder};
pub use probe::{ClaudeCodeProbeExec, ProbeOutput, SystemClaudeCodeExec, probe, probe_with_exec};
