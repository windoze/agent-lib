# M6-1 — `Collaboration` config + topology-driven collab substrate

Task: `TODO.md` §Milestone 6 → **M6-1** (first incomplete task).
Spec anchors: `docs/facade-api.md` §14 (default table + explicit `Collaboration`), §18.6, §19;
`PLAN.md` R8 (only promise landed collab; don't fake). `agent::collab` primitives (Mailbox/
Blackboard/Plan) are landed and public.

## §14 default table (topology -> substrate)
- no delegate -> none
- 1 delegate, model-routed -> mailbox optional, default OFF -> none
- multiple delegates -> mailbox
- dispatcher / verifier -> plan + blackboard + mailbox
- managed external agent -> artifact store (additive)

Explicit `.collaboration(Collaboration::new().plan().blackboard().mailbox().artifacts())`
overrides the derived default (full replacement).

## Scope decision (honest, non-faking, per R8)
- collab Mailbox/Blackboard/Plan and `ArtifactRef` all landed -> all four §14 substrates map to
  real primitives. No auto-tier faked/skipped.
- "Wire into scope/state" = provision live, shared primitives on `Agent` (real state), exposed
  via public accessors so callers / delegates / the M6-2 external bridge can use them.
- §14's named populate mechanism is the external runtime collab-event bridge = M6-2. So M6-1 does
  NOT give the supervisor LLM collab tools / auto-route coordination (would over-reach/fake).
- Snapshot: reserved collab fields stay reserved in M6-1 (serialization + restore lands w/ bridge,
  M6-2). Provisioning is live-state only. Base-path snapshot test stays green (base provisions none).
- Prelude: §3 has NO `Collaboration` -> export from `facade` only, NOT prelude (M6-R check green).

## Steps
1. FacadeIds: add blackboard_id() + plan_id().
2. New src/facade/collab.rs: Collaboration (Copy, serde) + builders/accessors; RoutingKind +
   derive_default(count,kind,has_external) per §14; resolve(explicit,...); pub(crate) CollabState
   {config,mailbox,blackboard,plan} + provision(config,&ids); #[cfg(test)] mod tests.
3. facade/mod.rs: pub mod collab; pub use collab::Collaboration;
4. AgentBuilder: collaboration field + setter; build() resolves+provisions -> Agent.collab;
   Agent accessors collaboration()/mailbox()/blackboard()/plan(); Debug prints collaboration.
5. Rustdoc + honest notes.

## Validation (1-6 + external clippy)
1 fmt; 2 clippy; 3 clippy --features external-*; 4 cargo test -p agent-lib facade::collab;
5 cargo test --all --all-targets (<30min); 6 RUSTDOCFLAGS=-D warnings cargo doc; git diff --check

## Status: DONE

All validation green (fmt, clippy default + 4 external features, focused facade::collab 12,
full cargo test --all --all-targets exit 0, doctests incl. 3 collab, doc, git diff --check).
M6-1 marked [DONE] in TODO.md with completion record. PLAN.md unchanged (no phase-level change).
Committing and stopping (do not start M6-2).
