//! Managed **Codex** external agent (design doc §13, capability matrix "Managed
//! External Runtime").
//!
//! Drives a real `codex` CLI session the managed way — through an
//! [`ExternalAgentMachine`](agent_lib::agent::ExternalAgentMachine) and a scoped
//! [`ExternalSessionHandler`](agent_lib::agent::ExternalSessionHandler) backed by
//! an [`ExternalSessionRegistry`](agent_lib::agent::external::ExternalSessionRegistry) —
//! never by calling the adapter directly. See `examples/support/managed.rs` for
//! the shared wiring.
//!
//! # Feature flag
//!
//! Requires `external-codex`. The example is a no-op unless that feature is
//! enabled (Cargo's `required-features` skips it otherwise).
//!
//! # Environment
//!
//! - `CODEX_BIN` (optional): path to the `codex` binary (default: `codex` on
//!   `PATH`). Its value is used but never printed.
//! - `CODEX_MODEL` (optional): pin a cheaper model.
//!
//! The spawned CLI inherits this process's environment and its own stored login.
//! No credential is ever logged.
//!
//! # Run it
//!
//! ```text
//! cargo run --example managed_codex --features external-codex
//! ```
//!
//! A missing `codex` binary or a failed capability probe turns the run into a
//! **skip** (a non-secret message + exit 0), so an unconfigured machine stays
//! green.

#[path = "support/managed.rs"]
mod managed;

use agent_lib::agent::ExternalRuntimeKind;

#[tokio::main]
async fn main() {
    println!("Managed Codex external agent example");
    let result = managed::drive_managed_child(
        ExternalRuntimeKind::Codex,
        "Summarise, in one sentence, what an agent-lib ExternalAgentMachine does.",
    )
    .await;
    managed::report(&result);
}
