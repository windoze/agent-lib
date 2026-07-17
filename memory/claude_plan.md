# M5-2 dispatcher-routed delegation — execution plan

Task: `TODO.md` §Milestone 5 → **M5-2 dispatcher-routed delegation (primary → verify → escalate)**.
Spec: `docs/facade-api.md` §13.3, §17.4, §6.3 (`RunEvent::Escalated`), §19.

## Goal
`Delegation::dispatcher().primary("cheap").verify_with("verifier").escalate_to("strong").max_attempts(2)`
maps onto `agent::external::{Dispatcher, Escalator}` semantics (no new scheduler runtime, §19).
Loop: primary runs → verifier checks product → on reject, escalate to strong, capped by
`max_attempts`. Attempts + escalation path recorded into `DelegationTrace`s; emit `RunEvent::Escalated`.

## Design decisions
- New `DispatcherConfig { primary, verifier: Option, escalate_to: Option, max_attempts }` +
  `DelegationMode::Dispatcher`. Builder mirrors rules-routed switch behavior.
- Dispatcher mode exposes **no** delegate tool to the model (like Rules): declarations/route/
  external_tool_names empty. Facade drives delegates directly via the existing
  `DelegationToolHandler::fulfill_rules_routed` path (reuses subagent/external drive, §9.2 gate).
- Escalation **decision** uses the real `agent::external::Escalator` + `WorkerRoster`: primary
  registered `CostTier::Cheap` with `EscalationRules{escalate_to: strong}`, strong `Premium`
  terminal. `assess()` on `WorkerReport::failed(current, ReviewRejected)` returns `Reassign(strong)`
  → read `choice.worker().id()` back to delegate name. Escalator `.with_budget_headroom(0)`.
- Verifier verdict contract (documented): verifier requests escalation when its reply contains the
  case-insensitive token `ESCALATE`, or its delegation fails. Otherwise passes. A primary/worker
  delegation that itself fails also triggers escalation.
- Final reply text = last worker summary (not verifier). Supervisor usage = 0; child usage on
  subagent/external slices. Not folded into supervisor Conversation.

## Steps / Status
1. [config] delegate.rs config+builder+accessors+validation, RulesRoutedTarget Clone, drive_one helper — done
2. [drive] agent.rs drive_dispatcher_routed + run_dispatcher_routed + run_full branch + retention + build validation — done
3. [stream] stream.rs start_dispatcher_routed — done
4. [tests] offline unit + drive + stream tests (10 cases) — done
5. [validate] fmt/clippy(+external)/focused/full/doc/diff — done, all green

## Class-wide bug fixed during M5-2
- `FacadeSubagentSpawner::child_ids` (delegate.rs) and `FacadeExternalSpawner::child_ids`
  (external.rs) used a fixed `subagent:{name}` / `external:{name}` trace node id. Driving the same
  delegate twice in one run (dispatcher re-runs the verifier per attempt) triggered
  `duplicate trace node id`. Fixed by folding the freshly-minted `run_id` into the trace node id,
  guaranteeing uniqueness across repeated drives for BOTH subagent and external delegates.

## Result
M5-2 complete and marked `[DONE]` in TODO.md. Full validation sequence 1–6 green. Stopping (do
not start M5-R).
