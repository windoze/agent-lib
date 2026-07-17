//! Managed **Claude Code** external agent (design doc §12, capability matrix
//! "Managed External Runtime").
//!
//! Drives a real `claude` CLI session the managed way — through an
//! [`ExternalAgentMachine`](agent_lib::agent::ExternalAgentMachine) and a scoped
//! [`ExternalSessionHandler`](agent_lib::agent::ExternalSessionHandler) backed by
//! an [`ExternalSessionRegistry`](agent_lib::agent::external::ExternalSessionRegistry) —
//! never by calling the adapter directly. See `examples/support/managed.rs` for
//! the shared wiring.
//!
//! # Feature flag
//!
//! Requires `external-claude-code`. The example is a no-op unless that feature is
//! enabled (Cargo's `required-features` skips it otherwise).
//!
//! # Environment
//!
//! - `CLAUDE_CODE_BIN` (optional): path to the `claude` binary (default:
//!   `claude` on `PATH`). Its value is used but never printed.
//! - `CLAUDE_CODE_MODEL` (optional): pin a cheaper model.
//!
//! The spawned CLI inherits this process's environment and its own stored login,
//! exactly as an interactive shell would. No credential is ever logged.
//!
//! # Run it
//!
//! ```text
//! cargo run --example managed_claude_code --features external-claude-code
//! ```
//!
//! A missing `claude` binary or a failed capability probe turns the run into a
//! **skip** (a non-secret message + exit 0), so an unconfigured machine stays
//! green.

#[path = "support/managed.rs"]
mod managed;

use agent_lib::agent::ExternalRuntimeKind;

#[tokio::main]
async fn main() {
    println!("Managed Claude Code external agent example");
    let result = managed::drive_managed_child(
        ExternalRuntimeKind::ClaudeCode,
        "Summarise, in one sentence, what an agent-lib ExternalAgentMachine does.",
    )
    .await;
    managed::report(&result);
}
