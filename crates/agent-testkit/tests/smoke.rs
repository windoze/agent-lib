//! Smoke test proving `agent-testkit` can reference `agent-lib`'s public agent
//! effect types, both directly and through the kit's prelude.

use agent_testkit::prelude::*;

/// Compiles only if `M` satisfies the public [`AgentMachine`] contract, so it
/// witnesses that the trait bound is nameable from the testkit crate.
fn assert_agent_machine<M: AgentMachine>() {}

#[test]
fn testkit_references_agent_public_types() {
    // The default machine must satisfy the public machine trait.
    assert_agent_machine::<DefaultAgentMachine>();

    // Provider-neutral step modes are addressable and comparable by name.
    assert_ne!(LlmStepMode::NonStreaming, LlmStepMode::Streaming);
    assert_eq!(LlmStepMode::Streaming, LlmStepMode::Streaming);
}

#[test]
fn prelude_and_direct_paths_agree() {
    // Both the prelude re-export and the fully qualified path name the same type.
    fn direct(_: agent_lib::agent::LlmStepMode) {}
    direct(LlmStepMode::NonStreaming);
}
