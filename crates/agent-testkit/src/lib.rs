//! Dev-only testing infrastructure for the [`agent-lib`](agent_lib) agent
//! effect layer.
//!
//! `agent-testkit` collects the fakes, fixtures, scripted effect handlers,
//! cassette record/replay helpers, scope builders, step/drain harnesses,
//! assertions, and concurrency/cancellation tools that agent-layer tests would
//! otherwise re-implement per file. It sits at the effect boundary: it fulfils
//! [`Requirement`](agent_lib::agent::Requirement) values by directly
//! implementing `agent-lib`'s public handler traits, and never mocks a provider
//! HTTP/SSE wire format.
//!
//! # Boundaries
//!
//! - The kit depends only on `agent-lib`'s public API and must not reach around
//!   its invariants.
//! - It stays provider-neutral: it constructs
//!   [`Message`](agent_lib::model::message::Message),
//!   [`Response`](agent_lib::client::Response),
//!   [`ToolCall`](agent_lib::model::tool::ToolCall) and friends, not Anthropic
//!   or OpenAI wire JSON.
//! - It is not a dependency of `agent-lib` itself; it is wired in as a dev-only
//!   consumer.
//!
//! # Module map
//!
//! The modules below are pre-declared so the crate topology is stable while each
//! milestone fills them in:
//!
//! - [`ids`]: deterministic id sources (`SeqIds`, `RequirementIds`,
//!   `ToolExecutionIds`).
//! - [`fixtures`]: provider-neutral message/response/tool/agent constructors.
//! - [`script`]: the scripted effect model, strict mode, and call log.
//! - [`handlers`]: scripted [`LlmHandler`](agent_lib::agent::LlmHandler),
//!   [`ToolHandler`](agent_lib::agent::ToolHandler), and other effect handlers.
//! - [`cassette`]: record/replay of provider-neutral effect req/resp.
//! - [`scope`]: the `TestScope` handler-scope builder.
//! - [`machine`]: the `ScriptMachine` [`AgentMachine`](agent_lib::agent::AgentMachine)
//!   double.
//! - [`harness`]: step/drain harnesses over the machine and scope.
//! - [`assertions`]: conversation/notification/trace/budget assertions.
//! - [`concurrency`]: delay/barrier/peak and cancel/panic tools.
//! - [`subagent`]: scripted subagent spawner and parent/child scope helpers.
//! - [`prelude`]: convenience re-exports for test authors.

#![warn(missing_docs)]

pub mod assertions;
pub mod cassette;
pub mod concurrency;
pub mod fixtures;
pub mod handlers;
pub mod harness;
pub mod ids;
pub mod machine;
pub mod scope;
pub mod script;
pub mod subagent;

pub mod prelude;
