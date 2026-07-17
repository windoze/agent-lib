# M7-1 — AgentBuilder::interaction_handler(..) inject custom async InteractionHandler

TODO.md first incomplete task = **M7-1** (Milestone 7 host-embedding injection points).

## Goal
Expose an injection point so a host can supply an async `InteractionHandler`
(a true pause point that can `await` a cross-process oneshot) instead of the
hardcoded synchronous `FacadeApproval`. Un-injected behavior == M2 exactly.

## Design (Option A — swap only the InteractionHandler role)
- The machine gate (`ToolApprovalPolicy`) stays `FacadeApproval` — it still
  decides pause-or-not from the `ApprovalPolicy` tiers, and still `record_pending`
  (so the stream path can peek the tool name).
- Only the scope's `interaction()` handler is swapped:
  injected handler if present, else fall back to `FacadeApproval`.
- Priority vs `.approval(..)`: when injected, the custom handler is the sole
  authority for *answering* paused interactions (FacadeApproval::fulfill's
  ask/deny logic is overridden). The ApprovalPolicy still governs which calls
  pause; to route every call through the handler use an ask/deny default.
  Document this in rustdoc.

## Edits
1. `src/facade/agent.rs`
   - `AgentBuilder`: add field `interaction_handler: Option<Arc<dyn InteractionHandler>>`
     + builder method `interaction_handler(self, handler) -> Self` (rustdoc: priority/caveat).
   - `AgentBuilder` Debug: add `has_interaction_handler`.
   - `Agent`: add field `interaction_handler: Option<Arc<dyn InteractionHandler>>`
     + private helper `fn interaction_handler(&self) -> Arc<dyn InteractionHandler>`
       returning injected or `self.approval.clone()` (coerce Arc<FacadeApproval>).
   - sync `run` path: `FacadeAgentScope.interaction = self.interaction_handler()`.
   - `FacadeAgentScope.interaction` field type -> `Arc<dyn InteractionHandler>`.
   - `build()`: pass `interaction_handler: self.interaction_handler`.
2. `src/facade/agent/stream.rs`
   - `TapInteractionHandler`: fields become `approval: Arc<FacadeApproval>`
     (peek pending_tool_name) + `inner: Arc<dyn InteractionHandler>` (delegate) + sink.
   - `start()`: wire `approval: agent.approval.clone()`, `inner: agent.interaction_handler()`.
3. `src/facade/agent/snapshot.rs`
   - restore `Agent { .. }`: add `interaction_handler: None` (conservative M2 fallback).

## Tests (src/facade/agent/tests.rs)
- sync: scripted InteractionHandler awaits a oneshot; approval-requiring turn
  (policy auto_deny -> gate pauses); assert future does NOT complete before
  test resolves; then approve -> tool runs; deny -> tool skipped. Cover both.
- stream: injected handler still emits ApprovalRequested with tool_name, and
  the injected decision (approve) runs the tool (vs default deny).
- Confirm existing no-injection tests still pass (fallback == M2).

## Validation (full sequence 1-6; no external adapter touched)
1 cargo fmt --all -- --check
2 cargo test -p agent-lib facade::agent
3 cargo clippy --all-targets -- -D warnings
4 cargo test --all --all-targets (<=30min)
5 RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6 git diff --check

## Status: DONE
- All edits landed; full validation 1-6 green (+ doctests). TODO.md M7-1 marked [DONE].
- No external adapter touched -> all-external clippy not required for this task.
