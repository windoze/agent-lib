//! Managed OpenCode runtime adapter (feature `external-opencode`).
//!
//! This module backs the OpenCode CLI as an
//! [`ExternalRuntimeAdapter`](crate::agent::external::ExternalRuntimeAdapter),
//! gated behind the non-default `external-opencode` feature so the core crate
//! stays free of CLI-adapter machinery unless a host opts in. It is filled in
//! across milestone 8:
//!
//! - **M8-1 (this task):** [`OpenCodeConfig`] launch configuration and the
//!   [`probe`] capability probe. Because OpenCode ships in more deployment shapes
//!   than the other runtimes, nothing is assumed: the probe classifies a
//!   missing/broken binary as
//!   [`Launch`](crate::agent::external::ExternalAgentError::Launch), a binary
//!   lacking the structured `opencode run --format json` event stream as
//!   [`UnsupportedCapability`](crate::agent::external::ExternalAgentError::UnsupportedCapability),
//!   and otherwise reports a conservatively-detected
//!   [`ExternalRuntimeCapabilities`](crate::agent::external::ExternalRuntimeCapabilities)
//!   set whose every flag defaults to `false` until the help text advertises the
//!   backing feature.
//! - **M8-2 (this task):** the private [`opencode run --format json` decoder`](decoder)
//!   turning raw CLI frames into sequenced
//!   [`ExternalObservedEvent`](crate::agent::external::ExternalObservedEvent)
//!   observations and per-turn [`OpenCodeDecision`]s. Like `codex exec --json`,
//!   `run --format json` runs autonomously — its permission prompts are resolved
//!   against the `--auto` launch flag rather than bridged back to the host — so a
//!   turn only ever completes or fails.
//! - **M8-3 (this task):** the live
//!   [`ExternalRuntimeSession`](crate::agent::external::ExternalRuntimeSession)
//!   process management ([`OpenCodeAdapter`]) that wraps the decoder into
//!   start/resume/advance. `opencode run` is one-shot per turn (the prompt is a
//!   CLI positional argument, not a stdin frame), so a follow-up turn is a fresh
//!   `opencode run --session <id> <message>` process; the adapter reports
//!   host-tool, host-subagent, and permission bridging as unsupported because the
//!   stream never pauses for the host (M8-2).
//!
//! The [`OpenCodeConfig`] captures the CLI's `run`-subcommand layout: the
//! structured stream is selected with `--format json`, the model with
//! `-m/--model provider/model`, a preset agent with `--agent`, and permission
//! bypass with `--auto` — mapped conservatively so only
//! [`BypassPermissions`](crate::agent::external::ExternalPermissionMode::BypassPermissions)
//! passes `--auto` (design §14). Nothing here parses or re-exports OpenCode's
//! private wire schema as stable public API (design 非目标): the probe reads only
//! `--version` / `--help` / `run --help`.

mod adapter;
mod config;
mod decoder;
mod probe;

pub use adapter::OpenCodeAdapter;
pub use config::OpenCodeConfig;
pub use decoder::{OpenCodeDecision, OpenCodeDecodeContext, OpenCodeStreamDecoder};
pub use probe::{
    OpenCodeProbeExec, OpenCodeProbeOutput, SystemOpenCodeExec, probe, probe_with_exec,
};
