//! High-level, read-only assertions over the observable results of an agent
//! turn: the committed [`Conversation`](agent_lib::conversation::Conversation),
//! the [`Requirement`](agent_lib::agent::Requirement) batch a step emitted, the
//! [`Notification`](agent_lib::agent::Notification) stream, the
//! [`RunContext`](agent_lib::agent::RunContext) trace and budget, the scripted
//! handler [`CallLog`](crate::script::CallLog), and the terminal
//! [`TurnDone`](agent_lib::agent::TurnDone) cursor.
//!
//! These helpers collapse the hand-written navigation that agent-layer tests
//! would otherwise repeat — `conversation.turns()[0].messages()[3].payload()`,
//! notification `match` arms, trace-record scans — into a small set of fluent
//! `assert_*` entry points. Every builder holds a *shared* reference (or an
//! owned snapshot) and never mutates the machine or context, so an assertion can
//! never change the behaviour it is checking.
//!
//! # Fluent and diagnostic
//!
//! Each `assert_*` returns a small, [`Copy`] builder whose methods return
//! `Self`, so checks chain:
//!
//! ```no_run
//! use agent_testkit::prelude::*;
//! # fn demo(conversation: &agent_lib::conversation::Conversation) {
//! assert_conversation(conversation)
//!     .committed_turns(1)
//!     .pending_none()
//!     .last_assistant_text("It is sunny in Shanghai.");
//! # }
//! ```
//!
//! A failed check panics with a message that names *what* was expected, *what*
//! was actually observed, and enough of the surrounding shape (turn/message
//! indices, requirement families, notification kinds, trace nodes) to locate the
//! problem without re-running under a debugger. The panic payload is always a
//! `String`, so a meta-test can capture it with
//! [`std::panic::catch_unwind`] and assert on the diagnostic itself.
//!
//! # Boundaries
//!
//! The helpers observe; they do not drive. Build the turn with a
//! [`StepHarness`](crate::harness::StepHarness) or
//! [`DrainHarness`](crate::harness::DrainHarness), then point these assertions at
//! the results.

mod budget;
mod calls;
mod conversation;
mod done;
mod external;
mod notifications;
mod requirements;
mod trace;

pub use budget::{BudgetAssertions, assert_budget, assert_budget_snapshot};
pub use calls::{CallAssertions, assert_calls};
pub use conversation::{ConversationAssertions, assert_conversation};
pub use done::{TurnDoneAssertions, assert_done};
pub use external::{
    ExternalAgentCallAssertions, ExternalInputKind, ExternalResultKind, assert_external_calls,
};
pub use notifications::{NotificationAssertions, assert_notifications};
pub use requirements::{RequirementAssertions, RequirementView, assert_requirements};
pub use trace::{
    RequirementTraceView, TraceAssertions, TraceNodeView, assert_trace, assert_trace_records,
};
