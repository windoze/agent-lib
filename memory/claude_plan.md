# M2-3 `Agent` / `AgentBuilder` + `run` / `run_full` (assemble machine + drive)

## Task (TODO.md M2-3)
Build `src/facade/agent.rs`: `Agent` + `AgentBuilder`. builder collects
provider/model/system/tools/approval/loop-policy; `build()` assembles the §8.3 chain
(AgentBuilder -> AgentSpec -> AgentState(Conversation::new) -> DefaultAgentMachine ->
RequirementIds+ToolExecutionIds -> HandlerScope(llm+tool+interaction) -> RunContext ->
drain). `run`/`run_full`: per turn `AgentInput::user_message` + `drain`; final assistant
text -> Reply; RunOutput fills response/usage/tool_calls (ToolTrace from notifications);
`LoopCursor::Error` -> FacadeError (LoopLimitExceeded / Agent(..)). Loop policy defaults
max_steps=8, max_tool_rounds=4, tool_failure_policy=ReturnErrorToModel + overrides.
pending failure -> cancel (machine's fail path already discards pending). rustdoc + doctest.

## Key findings (verified against code)
- `LoopPolicy` fields = { max_steps, max_parallel_tools, tool_failure_policy } — there is
  NO `max_tool_rounds` in the core. Faithful mapping: underlying `LoopPolicy.max_steps =
  min(facade.max_steps, facade.max_tool_rounds + 1)` (a successful run needs tool_rounds+1
  LLM steps). max_parallel_tools = 1 (core default; facade doesn't expose it; core machine
  doesn't consume it anyway).
- Step-limit reached => machine `fail_with_notifications` => `LoopCursor::Error(msg)` with
  msg = "agent loop step limit {N} reached before a final assistant response". `drain`
  returns `Ok(TurnDone{cursor: Error})` for machine errors, `Err(AgentError)` only for
  driver/handler failures. Classify Error cursor: msg.contains("loop step limit") =>
  LoopLimitExceeded, else Agent(AgentError::Other(msg)). A facade test drives the limit and
  asserts LoopLimitExceeded, locking the contract.
- Approval Deny does NOT abort: machine synthesizes a denial tool response, emits
  ToolCallFinished, continues loop (tool closure never called). So auto_deny test asserts
  tool-not-executed + final text; no ApprovalDenied abort (M2-3 error mapping only lists
  LoopLimitExceeded / Agent).
- Usage: committed turn's `meta().usage()` IS the aggregate (pending turn accumulates each
  response's usage; merged at commit even with TurnMeta::default()). stop_reason from
  `meta().responses().last()`. Reply built from (final assistant text, turn usage, last
  stop_reason). RunOutput.response = None (drain consumes raw Response; synthesizing one
  would be misleading). tool_calls/events from TurnDone notifications
  (ToolCallStarted/Finished).
- ReferenceScope::with_interaction only accepts ApprovalInteractionHandler (fixed decision),
  so build a custom `HandlerScope` (like examples/agent_chat.rs ChatScope) carrying
  LlmClientHandler + ToolRegistryHandler(FacadeToolRegistry) + Arc<FacadeApproval> as the
  interaction handler. Same Arc<FacadeApproval> is the machine approval policy (via
  with_approval_policy) AND the scope interaction handler (shared pending map).
- FacadeToolRegistry needs per-run ToolContextParts (run_id/agent_id/worktree/cancel/trace)
  from RunContext; so registry + scope are rebuilt per run. Machine (state+approval+loop) is
  built once and persists across run() calls (multi-turn history).
- ToolContextParts: run_id=ctx.run_id(), agent_id=spec.id(), worktree=spec.worktree().clone(),
  cancel=ctx.cancellation().clone(), trace=ctx.trace().clone().

## Edits
1. `error.rs`: add `Agent(#[from] AgentError)` and `LoopLimitExceeded` variants (§16).
2. `run.rs`: add `pub(crate) fn Reply::from_parts(text, usage, stop_reason)`.
3. `tool.rs`: extract `pub(crate) fn ensure_unique_tool_names(tools, extra, custom)` used by
   both `FacadeToolRegistry::new` and `Agent::build` (build-time conflict check).
4. `agent.rs` (new): Agent + AgentBuilder + custom HandlerScope + drive + classifier.
5. `mod.rs`: `pub mod agent;` + export `Agent`, `AgentBuilder`. (prelude deferred to M2-R.)

## Tests (offline, in agent.rs) — scripted sequenced FakeClient
- run: tool-use then final text -> returns final text; tool closure ran once.
- run_full: tool_calls records the call; events contain ToolStarted/Finished.
- auto_deny: tool closure NOT called; final text still returned.
- max_tool_rounds=1 with always-tool-use client -> LoopLimitExceeded.
- (extra) build fails on duplicate tool name; run advertises tool declarations.

## Validation
fmt --all -- --check; clippy --all-targets -D warnings (+ --features facade-schema);
cargo test -p agent-lib facade::agent (focused); cargo test --all --all-targets;
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace; git diff --check.

## Status: DONE

- Implemented `src/facade/agent.rs` (`Agent` + `AgentBuilder` + `FacadeAgentScope` + `run`/`run_full`
  + helpers `build_loop_policy`/`classify_error`/`collect_tool_traces`/`final_turn_summary`).
- Added `src/facade/agent/tests.rs` — 7 offline tests, all green.
- Wired `pub mod agent; pub use agent::{Agent, AgentBuilder};` into `src/facade/mod.rs`.
- Support edits: `error.rs` (`Agent(#[from] AgentError)` + `LoopLimitExceeded`), `run.rs`
  (`Reply::from_parts`), `tool.rs` (`ensure_unique_tool_names`), `chat.rs` (`client_for_provider` → `pub(crate)`).
- Validation all green: fmt; clippy default + `facade-schema` (0 warnings); focused `facade::agent` 7/7;
  `cargo test --all --all-targets` 983 passed / 0 failed; rustdoc `-D warnings` default + `facade-schema`;
  doctests 12/12; `git diff --check` clean.
- M2-3 marked `[DONE]` in `TODO.md` with completion record. Prelude additions + stream/snapshot/restore/into_parts
  deferred to M2-4 / M2-R per plan.
