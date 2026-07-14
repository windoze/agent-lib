//! Convenience re-exports for agent-layer test authors.
//!
//! Test modules can `use agent_testkit::prelude::*;` to pull in the most common
//! `agent-lib` agent-effect types alongside the kit's own helpers as later
//! milestones fill in the [`crate`] modules. For now this surfaces the machine
//! and step contract so downstream tests can name the effect boundary without a
//! deep import path.

pub use agent_lib::agent::{
    AgentMachine, DefaultAgentMachine, LlmStepMode, Requirement, RequirementKind, StepInput,
    StepOutcome,
};
