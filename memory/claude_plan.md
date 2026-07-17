# M5-R Review: Dispatcher / Escalator correctness + doc consistency

Task: `TODO.md` §Milestone 5 → **M5-R** (first incomplete task). Review + convergence only.
Spec anchors: `docs/facade-api.md` §13.2–§13.3, §6.3 (`RunEvent::Escalated`/`EscalationTrace`),
§18.5, §19 (no new scheduler runtime).

## Goal
Certify M5-1 (rules-routed) + M5-2 (dispatcher-routed) match §13.2–§13.3:
- rules-routed: model unaware (no delegate tools advertised).
- dispatcher-routed maps onto existing `agent::external::{Dispatcher, Escalator}` (no new runtime, §19).
- escalation path + `DelegationTrace` / `RunEvent::Escalated` complete.
- dispatcher is never a first-version default.
Fix small deviations; record gaps as follow-up tasks (per rules). Produce §13 promise vs
M5-impl comparison table. Run full validation sequence 1–6 (+ external-features clippy).

## Review findings (code read)
- `delegate.rs`: `DelegationMode::{Rules,Dispatcher}` → `declarations()`/`route()`/
  `external_tool_names()` all return empty ⇒ model never sees delegates. ✓ (§13.2/§13.3)
- `route_task`: first-match-wins, case-insensitive substring. ✓
- Build-time validation: `first_unknown_rule_delegate` / `first_unknown_dispatcher_delegate` +
  empty-primary check ⇒ `FacadeError::Config`. ✓
- `agent.rs drive_dispatcher_routed`: primary→verify→escalate loop capped by `max_attempts`;
  escalation *decision* via real `Escalator::assess` over a `WorkerRoster`
  (primary=Cheap+EscalationRules→strong; strong=Premium terminal), `with_budget_headroom(0)`.
  Emits `RunEvent::Escalated(EscalationTrace{from,to})`; per-attempt worker/verifier →
  `DelegationTrace`. No supervisor LLM; usage=0; not folded into Conversation. ✓ (§19)
- Uses `Escalator` (escalation engine) but not `Dispatcher` (initial budget-aware router):
  faithful, since the primary is an explicitly-named fixed worker (no ambiguous routing to
  dispatch). Documented in M5-2 record. → note in comparison table, no code change.
- prelude / facade §3: §3's list has no `RoutingRule`/`DispatcherConfig`/`EscalationTrace`;
  current prelude already matches §3 (has `Delegation`, `RunEvent`, `RunOutput`, `ManagedExternalAgent`).
  ⇒ no prelude change needed (unlike M4-R which had a pending prelude add).

## Decision
No source deviation found requiring a fix. Pure review: run full validation, write completion
record + comparison table in TODO.md, mark M5-R `[DONE]`, commit, stop.

## Validation (sequence 1–6 + external clippy)
1. cargo fmt --all -- --check
2. cargo clippy --all-targets -- -D warnings
3. cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
4. cargo test -p agent-lib facade::delegate  (focused)
5. cargo test --all --all-targets  (full)
6. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace ; git diff --check

## Status: DONE

All validation green (fmt, clippy default + 4 external features, focused 46 delegate tests,
full `cargo test --all --all-targets`, doc, git diff --check). No source deviation found;
review-only. M5-R marked [DONE] in TODO.md with §13 comparison table. PLAN.md unchanged
(no phase-level change). Committing and stopping (do not start M6-1).
