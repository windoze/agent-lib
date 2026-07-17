//! **Mixed** managed external agents: drive a Claude Code child and a Codex
//! child through the same managed stack, one after another (design doc §3
//! capability parity, §11 runtime adapter abstraction).
//!
//! Each child is a distinct [`ExternalAgentMachine`](agent_lib::agent::ExternalAgentMachine)
//! wired to its own probed adapter behind an
//! [`ExternalSessionRegistry`](agent_lib::agent::external::ExternalSessionRegistry),
//! driven through a scoped [`ExternalSessionHandler`](agent_lib::agent::ExternalSessionHandler)
//! — the same unified managed path regardless of runtime. See
//! `examples/support/managed.rs` for the shared wiring and the milestone-9 real
//! e2e (`tests/agent_external_managed_real_e2e.rs`) for a DeepSeek coordinator
//! that fans these out through `NeedSubagent`.
//!
//! # Feature flags
//!
//! Requires both `external-claude-code` and `external-codex`. The example is a
//! no-op unless both are enabled (Cargo's `required-features` skips it
//! otherwise).
//!
//! # Environment
//!
//! - `CLAUDE_CODE_BIN` / `CODEX_BIN` (optional): binary overrides (default:
//!   `claude` / `codex` on `PATH`). Their values are used but never printed.
//! - `CLAUDE_CODE_MODEL` / `CODEX_MODEL` (optional): pin cheaper models.
//!
//! Each spawned CLI inherits this process's environment and its own stored
//! login. No credential is ever logged.
//!
//! # Run it
//!
//! ```text
//! cargo run --example managed_mixed --features "external-claude-code external-codex"
//! ```
//!
//! Any runtime whose CLI is missing or whose capability probe fails is
//! individually **skipped** (a non-secret message), so a partially-configured
//! machine still exercises whatever is available and stays green.

#[path = "support/managed.rs"]
mod managed;

use agent_lib::agent::ExternalRuntimeKind;

#[tokio::main]
async fn main() {
    println!("Mixed managed external agents example (Claude Code + Codex)");

    for runtime in [ExternalRuntimeKind::ClaudeCode, ExternalRuntimeKind::Codex] {
        println!("- driving {runtime:?} child");
        let result = managed::drive_managed_child(
            runtime,
            "Summarise, in one sentence, what an agent-lib ExternalAgentMachine does.",
        )
        .await;
        managed::report(&result);
    }
}
