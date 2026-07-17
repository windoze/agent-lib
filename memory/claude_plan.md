# M6-2 — Bridge external runtime collab events to library collab primitives

Task: `TODO.md` → first incomplete = **M6-2**.
Spec: `docs/facade-api.md` §14 末段 — external runtime `spawn_agent`/`send_message`/
`plan_update`/`blackboard_post` 应桥接到本库 `agent::collab` primitives，不依赖 runtime 私有协议。

## Analysis
- External observations surface as `Notification::ExternalAgent(ExternalAgentEvent)` in each
  child machine `StepOutcome` (exactly-once, seq-deduped).
- Collab events map:
  - `send_message`  → `ExternalAgentEvent::MessageSent { to, summary }` → Mailbox.send
  - `plan_update`   → `ExternalAgentEvent::TaskUpdated { task_id, status }` → Plan reflect
  - `blackboard_post` → NEW `ExternalAgentEvent::BlackboardPosted { channel, summary }` → Blackboard.post
  - `spawn_agent`   → already bridged (NeedSubagent tool, M3-3) — not reflected here.
- Facade provisions live shared primitives in `CollabState` (M6-1). Need a bridge that routes
  external observations into the enabled ones. Provider-neutral: bridge takes agent-layer
  `ExternalAgentEvent` (no runtime private types); stays `pub(crate)` (not in public facade API).

## Steps
1. `src/agent/external/mod.rs`: add `BlackboardPosted { channel, summary }` variant to
   `ExternalAgentEvent` (symmetric w/ MessageSent/TaskUpdated; model-complete, decoder-unused —
   same precedent as MessageSent). Rustdoc. No exhaustive match breaks (only `_`-guarded match).
2. `src/facade/collab.rs`: add `pub(crate) struct CollabBridge` { mailbox/blackboard/plan Option<Arc> }
   + `from_state(&CollabState)`, `is_active()`, `absorb_notifications(from, &[Notification])`,
   `absorb_event(from, &ExternalAgentEvent)`. Plan reflection helper (add_task→claim→update,
   best-effort) + lenient status label parser (todo/pending, in_progress, completed/done, blocked,
   cancelled). Tests.
3. `src/facade/external.rs`: thread `CollabBridge` into `drive_external` → `FacadeExternalSpawner`
   → `RecordingExternalMachine`; absorb `outcome.notifications` in `step` (from = delegate name).
4. `src/facade/delegate.rs`: `DelegationToolHandler` gains `collab: CollabBridge`; pass into
   `drive_external`.
5. `src/facade/agent.rs`: `Agent::collab_bridge()` builds from `self.collab`; pass at both
   `DelegationToolHandler::new` sites (run_full + build_delegation_handler).
6. Rustdoc updates (collab.rs module: M6-2 landed). TODO.md [DONE] + record.

## Validation (1-6 + external clippy)
1 fmt; 2 clippy --all-targets -D warnings; 3 clippy --features external-*; 4
cargo test -p agent-lib facade::collab; 5 cargo test --all --all-targets (<30min); 6 doc.

## Discovered pre-existing failure (scheduled as M6-3)
- Ran `cargo test --features "external-claude-code external-codex external-opencode external-acp"`:
  948 passed / 1 failed. The 1 failure `claude_code_cassette_matches_in_code_builder` reproduces on a
  CLEAN tree (git stash) with the same feature set → PRE-EXISTING, not introduced by M6-2, and not on
  the collab-bridge path.
- Root cause: `external-acp` pulls `agent-client-protocol(-schema)` which enable
  `serde_json/preserve_order`; Cargo feature-unification flips `serde_json::Value` object maps from
  `BTreeMap` (sorted) to `IndexMap` (insertion), so the claude_code cassette `frame()` payloads
  (`json!(..).to_string()`) reorder keys and drift from the frozen fixture (sorted).
- Default-feature suite green (1070/0); single-feature cassette green. Per Test Failure Policy,
  scheduled a dedicated fix task **M6-3** BEFORE M6-R rather than folding unrelated test-infra
  serialization work into M6-2.

## Status: DONE
- Implementation + wiring + tests complete and green (see TODO.md 完成记录（M6-2）).
- M6-2 marked [DONE]; M6-3 (cassette preserve_order drift) inserted before M6-R.
- PLAN.md unchanged (no phase-level change).
