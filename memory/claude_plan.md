# M7-F1 — `AgentRestoreBuilder::interaction_handler(..)` injection seam

TODO.md first incomplete task = **M7-F1** (line 2175). Everything M1–M7-R is `[DONE]`.
M7-F1 aligns the restore path with `AgentBuilder::interaction_handler` (M7-1).

## Gap
- `AgentRestoreBuilder` (`src/facade/agent/snapshot.rs`) has no `interaction_handler`
  field/method; `build()` hardcodes `interaction_handler: None` (snapshot.rs:753),
  so a restored `Agent` always falls back to the synchronous `FacadeApproval`.
- `AgentBuilder::interaction_handler(Arc<dyn InteractionHandler>)` (agent.rs:1015)
  already provides the injection seam. The two are misaligned.

## Plan
1. `src/facade/agent/snapshot.rs`:
   - import `InteractionHandler` from `crate::agent`.
   - add field `interaction_handler: Option<Arc<dyn InteractionHandler>>` to
     `AgentRestoreBuilder` (derives `Default`, Option → None OK).
   - add `has_interaction_handler` to its `Debug` impl.
   - add builder method `interaction_handler(self, handler) -> Self` with rustdoc
     mirroring `AgentBuilder::interaction_handler` (priority vs `.approval`,
     both run + stream routed through it, not carried in snapshot / must re-inject).
   - in `build()`, replace hardcoded `interaction_handler: None` with
     `self.interaction_handler` (keep the comment: un-injected → FacadeApproval).
2. `src/facade/agent/tests.rs`: add offline test symmetric to M7-1's
   `injected_interaction_handler_pauses_until_approved` but driving a *restored*
   agent: snapshot a committed turn → `Agent::restore()...interaction_handler(gated)
   .build()` → drive an approval-gated turn → assert machine does not advance until
   the scripted handler resolves; approve → tool runs exactly once. Plus assert the
   un-injected restore still falls back (existing `snapshot_then_restore_continues_history`
   already covers default fallback behavior).

## Validation
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. all-external-features clippy (touch facade only; run to be safe)
4. `cargo test -p agent-lib facade::agent` (focused) then `cargo test --all --all-targets`
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `cargo test --doc -p agent-lib`

## Status
- [x] implement snapshot.rs
- [x] add test
- [x] validate (all 6 steps + all-external clippy green)
- [x] mark M7-F1 [DONE] in TODO.md + commit
