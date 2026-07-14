//! Convenience re-exports for agent-layer test authors.
//!
//! Test modules can `use agent_testkit::prelude::*;` to pull in the most common
//! `agent-lib` agent-effect types alongside the kit's own helpers as later
//! milestones fill in the [`crate`] modules. For now this surfaces the machine
//! and step contract so downstream tests can name the effect boundary without a
//! deep import path, plus the deterministic id source.

pub use crate::fixtures::{
    agent_spec, agent_spec_with_tools, agent_state, assistant_text, assistant_tool_use,
    calendar_tool, default_machine, root_context, text_block, tool_call, tool_error_response,
    tool_ok, tool_response, usage, user_input, user_message, weather_tool,
};
pub use crate::handlers::{
    InteractionCallLog, InteractionDecision, LlmCallLog, MisalignedHandler, ReconfigCallLog,
    ScriptedInteractionHandler, ScriptedLlmHandler, ScriptedReconfigHandler, ScriptedToolHandler,
    ScriptedToolRegistry, ToolCallLog,
};
pub use crate::ids::{RequirementAllocation, SeqIds};
pub use crate::script::{
    CallLog, CallRecord, CallTicket, InteractionStep, LlmStep, ReconfigStep, Script, ScriptError,
    ScriptStep, StrictMode, ToolStep,
};
pub use agent_lib::agent::{
    AgentMachine, DefaultAgentMachine, LlmStepMode, Requirement, RequirementKind, StepInput,
    StepOutcome,
};
