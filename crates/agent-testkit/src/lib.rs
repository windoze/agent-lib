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
//!   or OpenAI wire JSON. **It never mocks a provider HTTP/SSE transport**:
//!   there is no fake endpoint, no request/response body replay, and no
//!   header/auth simulation. Protocol-level behaviour (HTTP shape, SSE folding,
//!   provider JSON) stays in the adapter/client tests; the kit only mocks the
//!   agent effect boundary — the handler traits that fulfil a
//!   [`Requirement`](agent_lib::agent::Requirement).
//! - It is not a dependency of `agent-lib` itself; it is wired in as a dev-only
//!   consumer.
//!
//! # Quickstart
//!
//! Script the model, wire a headless [`scope`], and drain one turn. Only the
//! LLM family is wired here, so a stray tool or interaction would surface as an
//! `UnhandledRequirement` rather than being silently served:
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use agent_lib::agent::drain;
//! use agent_testkit::prelude::*;
//!
//! # async fn quickstart() {
//! let ids = SeqIds::new();
//! let ctx = root_context(&ids);
//! let spec = agent_spec(&ids);
//! let mut machine = default_machine(&ids, agent_state(&ids, spec));
//!
//! // One scripted text generation that closes the turn.
//! let llm = ScriptedLlmHandler::from_steps([
//!     LlmStep::text("It is sunny in Shanghai.").with_usage(usage(4, 3)),
//! ]);
//! let scope = TestScope::builder().llm(Arc::new(llm)).build();
//!
//! let done = drain(&mut machine, user_input(&ids, "weather?"), &scope, None, &ctx)
//!     .await
//!     .expect("the scripted text turn drains to completion");
//!
//! assert_done(&done);
//! assert_conversation(machine.state().conversation())
//!     .committed_turns(1)
//!     .last_assistant_text("It is sunny in Shanghai.");
//! # }
//! ```
//!
//! For manual, step-by-step control use a
//! [`StepHarness`](harness::StepHarness); for a data-only turn description that
//! round-trips through serde, see the [`scenario`] runner spike.
//!
//! # Recording cassettes
//!
//! [`cassette`] replay is offline by default: a committed cassette JSON is
//! served by [`CassettePlayer`](cassette::CassettePlayer)-backed handlers with
//! no network, credentials, or live provider. The two *writing* modes drive the
//! real handlers, so they are gated behind explicit environment opt-ins and a
//! normal `cargo test` run can never overwrite a committed fixture:
//!
//! - `AGENT_TESTKIT_RECORD_CASSETTES=1`
//!   ([`RECORD_ENV_VAR`](cassette::RECORD_ENV_VAR)) enables
//!   [`CassetteRecorder::record`](cassette::CassetteRecorder::record), writing a
//!   fresh cassette.
//! - `AGENT_TESTKIT_UPDATE_CASSETTES=1`
//!   ([`UPDATE_ENV_VAR`](cassette::UPDATE_ENV_VAR)) enables
//!   [`CassetteRecorder::update`](cassette::CassetteRecorder::update),
//!   overwriting an existing cassette.
//! - With neither set, a recorder returns
//!   [`RecorderReport::Skipped`](cassette::RecorderReport::Skipped) and replay
//!   reads only the on-disk fixture.
//!
//! ```bash
//! # Re-capture a cassette after a spec or script change, then confirm offline lock-step:
//! AGENT_TESTKIT_UPDATE_CASSETTES=1 cargo test -p agent-testkit --test agent_replay_tool regenerate
//! cargo test -p agent-testkit --test agent_replay_tool
//! ```
//!
//! # Module map
//!
//! All modules below are implemented; the topology has been stable since the
//! kit's skeleton milestone:
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
//! - [`scenario`]: the data-only scenario model draft and runner spike.
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
pub mod scenario;
pub mod scope;
pub mod script;
pub mod subagent;

pub mod prelude;
