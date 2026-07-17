# M7-5 — facade 透出 AI 决策注入口（不实现 AI 逻辑）

TODO.md first incomplete task = **M7-5** (line 2041). M7-1..M7-4 are `[DONE]`.

## Goal
Open two injection seams in the facade **without implementing any AI logic**, keeping
M5/M2 default behavior byte-for-byte when nothing is injected:

1. `Delegation::dispatcher()` accepts a caller-injected `TaskEvaluator` and/or
   `Verifier`, replacing the hardwired `ScriptedVerifier::passing()` + keyword/`ESCALATE`
   seam used inside `drive_dispatcher_routed`.
2. `ApprovalPolicy` provides a caller-injectable **permission decider** hook that answers
   `InteractionKind::Permission` (returning `PermissionResponse`); default stays "deny".

## Key facts
- `Delegation` (delegate.rs) derives Clone/Debug/PartialEq/Eq/Serialize/Deserialize and is
  serialized in `AgentSnapshot` (agent/snapshot.rs). Hooks are runtime handlers → store as
  `#[serde(skip)]` field, dropped on snapshot (consistent with §15.2 dropping runtime handlers).
- Seams exported from `crate::agent`: `TaskEvaluator`, `Verifier`, `ScriptedTaskEvaluator`,
  `ScriptedVerifier`, `Escalator<V: Verifier>`, `WorkerProfileRef`, `WorkerReport`, `TaskDescriptor`.
- Facade dispatcher loop = `drive_dispatcher_routed` (agent.rs), called from `run_dispatcher_routed`
  (agent.rs) and `start_dispatcher_routed` (agent/stream.rs).
- `resolve_dispatcher_targets` resolves only config-referenced delegates (primary/verifier/
  escalate_to); roster from `build_dispatcher_roster`. Evaluator picks among those.
- Permission default-deny is `FacadeApproval::fulfill` Permission arm (approval.rs).
- Dispatcher e2e tests: delegate.rs tests (RoutingClient harness). Permission tests: approval.rs.

## Design
### Dispatcher injection (delegate.rs + agent.rs + stream.rs + escalation.rs)
- `impl<V: Verifier + ?Sized> Verifier for Arc<V>` blanket (escalation.rs) so an
  `Arc<dyn Verifier+Send+Sync>` can be the Escalator's `V`.
- `Delegation`: `#[serde(skip)] dispatcher_hooks: DispatcherHooks`
  `{ evaluator: Option<Arc<dyn TaskEvaluator+Send+Sync>>, verifier: Option<Arc<dyn Verifier+Send+Sync>> }`.
  DispatcherHooks: derive Clone+Default; manual Debug (presence bools); manual PartialEq(always true)+Eq
  so Delegation keeps its derives (config identity ignores runtime hooks).
- Builder: `dispatcher_evaluator(..)`, `dispatcher_verifier(..)` (switch to dispatcher mode).
  Accessors `dispatcher_evaluator()`/`dispatcher_verifier()`.
- `drive_dispatcher_routed(+ evaluator, verifier: Option<Arc<..>>)`:
  - Escalator uses injected verifier else `ScriptedVerifier::passing()`.
  - Verdict = `worker_failed || run_verifier(..) || injected_verifier_rejects(..)`.
  - Escalation target: evaluator injected → `injected_escalation_target` (evaluate vs roster,
    resolve name via targets, decline/unknown/current → None) else `dispatcher_escalation_target` (M5).
- Both callers read `agent.delegation.dispatcher_{evaluator,verifier}()` cloned Options.

### Permission decider (approval.rs)
- `type PermissionDecider = Arc<dyn Fn(&PermissionRequest) -> PermissionResponse + Send + Sync>`.
- ApprovalPolicy: `permission_decider: Option<PermissionDecider>`; builder `on_permission(F)`;
  init None in From<Approval>; Debug shows presence.
- FacadeApproval: carry it; `new` copies; `fulfill` Permission arm → decider (re-stamp action_id
  for correlation) else default deny.

## Tests
- delegate.rs: dispatcher_injected_verifier_forces_escalation (escalate where default wouldn't);
  dispatcher_injected_evaluator_declines_escalation (suppress escalation default would do);
  dispatcher_injection_hooks_stored_and_serde_drops_them.
- approval.rs: permission_decider_can_approve (+ existing default-deny remains).

## Validation
1. cargo fmt --all  2. cargo test -p agent-lib facade::delegate facade::approval
3. cargo clippy --all-targets -- -D warnings (+ full external features clippy — touches escalation seam)
4. cargo test --all --all-targets  5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6. git diff --check

## Progress
- [x] Arc<V> Verifier blanket impl (escalation.rs)
- [x] delegate.rs hooks + builder + accessors
- [x] drive_dispatcher_routed + 2 callers
- [x] approval permission decider
- [x] tests (delegate.rs: 3 new; approval.rs: 2 new — all green)
- [x] docs/rustdoc (defaults + injection points, §19; fixed 4 redundant intra-doc links) + TODO.md [DONE]
- [x] full validation (fmt, focused, clippy default + external features, full suite exit 0, doc, doctests, git diff --check) — committing next
