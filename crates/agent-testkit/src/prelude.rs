//! Convenience re-exports for agent-layer test authors.
//!
//! Test modules can `use agent_testkit::prelude::*;` to pull in the most common
//! `agent-lib` agent-effect types alongside the kit's own helpers: deterministic
//! ids, provider-neutral fixtures, scripted and cassette-backed handlers, the
//! [`TestScope`](crate::scope::TestScope) builder, the step/drain harnesses, the
//! assertion entry points, the concurrency/cancellation tools, and the
//! [`scenario`](crate::scenario) runner — so downstream tests can name the
//! effect boundary without a deep import path.

pub use crate::assertions::{
    BudgetAssertions, CallAssertions, ConversationAssertions, NotificationAssertions,
    RequirementAssertions, RequirementTraceView, RequirementView, TraceAssertions, TraceNodeView,
    TurnDoneAssertions, assert_budget, assert_budget_snapshot, assert_calls, assert_conversation,
    assert_done, assert_notifications, assert_requirements, assert_trace, assert_trace_records,
};
pub use crate::cassette::{
    CASSETTE_SCHEMA_VERSION, Cassette, CassetteEntry, CassetteError, CassetteInteractionHandler,
    CassetteLlmHandler, CassetteMetadata, CassetteObservations, CassettePlayer,
    CassetteReconfigHandler, CassetteRecorder, CassetteToolError, CassetteToolHandler,
    DefaultRedactor, EntryDrift, InteractionEntry, LlmEntry, LlmOutcome, RECORD_ENV_VAR,
    ReconfigEntry, ReconfigOutcome, RecorderError, RecorderMode, RecorderReport,
    RecordingInteractionHandler, RecordingLlmHandler, RecordingReconfigHandler,
    RecordingToolHandler, Redactor, ReplayMismatch, ReplayMismatchKind, ToolEntry, ToolOutcome,
    UPDATE_ENV_VAR, request_fingerprint,
};
pub use crate::concurrency::{
    Barrier, BarrierWait, CancelEvent, CancelLog, CancelOnCall, CancelTiming, Delay,
    DelayingToolHandler, InFlightGuard, PanicOnCall, PeakInFlight, YieldTicks,
};
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
pub use crate::harness::{
    DrainHarness, DrainObservation, HandlerCallCounts, HandlerLogSummary, StepHarness,
    StepHarnessError, StepObservation,
};
pub use crate::ids::{RequirementAllocation, SeqIds};
pub use crate::machine::{ScriptMachine, ScriptMachineBuilder, ScriptMachineLog};
pub use crate::scenario::{
    ApprovalPolicySpec, Scenario, ScenarioEffectScript, ScenarioError, ScenarioExpectation,
    ScenarioInput, ScenarioInteractionStep, ScenarioLlmStep, ScenarioSummary, ScenarioToolCall,
    ScenarioToolStep, ScenarioUsage, ToolResultExpectation, ToolResultObservation,
    TurnRolesExpectation, run_scenario,
};
pub use crate::scope::{TestScope, TestScopeBuilder};
pub use crate::script::{
    CallLog, CallRecord, CallTicket, InteractionStep, LlmStep, ReconfigStep, Script, ScriptError,
    ScriptStep, StrictMode, ToolStep,
};
pub use crate::subagent::{
    ScriptedSubagentSpawner, ScriptedSubagentSpawnerBuilder, SpawnedChildBuilder,
    attended_child_scope, headless_child_scope, parent_scope_with_subagent,
};
pub use agent_lib::agent::{
    AgentMachine, AgentSpecRef, DefaultAgentMachine, DrivingSubagentHandler, Interaction,
    LlmStepMode, Requirement, RequirementKind, SpawnedChild, StepInput, StepOutcome,
    SubagentOutput, SubagentSpawner, TurnDone,
};
