//! Shared support for the complex agent-effect mock tests.
//!
//! These tests exercise realistic combinations of the agent effect boundary —
//! multi-turn conversations, tool approval/deny, subagents, plan/blackboard
//! side effects, cancellation, and pivots — on top of `agent-testkit`. The
//! support layer here holds the pieces reused across those scenarios.
//!
//! Milestone 1 lands the mock plan/blackboard vertical feature in
//! [`plan_blackboard`], the complex tool adapter, tool declarations, and
//! approval policy in [`tools`], and the read-only assertion helpers in
//! [`assertions`]; later milestones build the scenario suites on top of them.
//!
//! The support layer is grown one milestone at a time and is compiled fresh into
//! each complex-test binary, so any given test crate only exercises a subset of
//! the API. `dead_code` and `unused_imports` are allowed here (and propagate to
//! the child modules) so helpers and re-exports staged for a later milestone do
//! not warn in the crates that do not use them yet.
#![allow(dead_code, unused_imports)]

pub mod assertions;
pub mod plan_blackboard;
pub mod tools;

// ----- re-exports ----------------------------------------------------------
//
// The scenario suites reach for these names constantly; surfacing them at the
// support-layer root keeps their `use` lists short and stable.

pub use assertions::{
    assert_board_messages, assert_interaction_decisions, assert_no_task_owner,
    assert_pivot_after_tool_result, assert_task_depends_on, assert_task_owner, assert_task_status,
    assert_tool_executions, role_sequence,
};
pub use plan_blackboard::{
    BoardMessage, MockPlanBlackboardStore, OpKind, PlanState, StoreError, StoreOp, TaskState,
    TaskStatus,
};
pub use tools::{
    BLACKBOARD_POST, BLACKBOARD_READ, ComplexToolHandler, DANGEROUS_WRITE, PLAN_ADD_TASK,
    PLAN_CLAIM, PLAN_CLAIM_FIRST_AVAILABLE, PLAN_CREATE, PLAN_UPDATE,
    RequireDangerousWriteApprovalPolicy, SAFE_READ, SPAWN_REVIEWER, ToolInvocation,
    complex_agent_machine, complex_scope, complex_tool_handler, tool_declarations,
};
