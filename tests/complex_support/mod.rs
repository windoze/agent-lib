//! Shared support for the complex agent-effect mock tests.
//!
//! These tests exercise realistic combinations of the agent effect boundary —
//! multi-turn conversations, tool approval/deny, subagents, plan/blackboard
//! side effects, cancellation, and pivots — on top of `agent-testkit`. The
//! support layer here holds the pieces reused across those scenarios.
//!
//! Milestone 1 lands the mock plan/blackboard vertical feature in
//! [`plan_blackboard`]; later milestones add tool adapters, approval policies,
//! and assertion helpers alongside it.
//!
//! The support layer is grown one milestone at a time and is compiled fresh into
//! each complex-test binary, so any given test crate only exercises a subset of
//! the API. `dead_code` is allowed here (and propagates to the child modules) so
//! helpers staged for a later milestone do not warn in the crates that do not
//! use them yet.
#![allow(dead_code)]

pub mod plan_blackboard;
