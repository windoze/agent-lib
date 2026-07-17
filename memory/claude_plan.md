# M4-2 `.external_agent(..)` external delegate fulfillment + artifact/delegation trace

Task: TODO.md M4-2 (first incomplete). Wire a registered `ManagedExternalAgent`
as an **external delegate** exposed as `ask_<name>`, fulfilled by driving an
`ExternalAgentMachine` (via `NeedSubagent` → `DrivingSubagentHandler`, scoped
`ExternalSessionHandler`), collecting external observations into
`DelegationTrace` (usage/status) + `RunOutput.artifacts` (`ArtifactRef`), and
emitting `RunEvent::DelegationStarted`/`DelegationArtifact`/`DelegationFinished`.
Offline only: scripted `ExternalSessionHandler` in tests + doctest. No real CLI.

## Architecture (mirrors local subagent path in `facade/delegate.rs`)
- IO seam = injectable `Arc<dyn ExternalSessionHandler>` on `ManagedExternalAgent`
  (sans-io machine + handler). Presets leave it `None`; unconfigured delegate
  fails fast with a clear `FacadeError`. Tests/doctest inject a scripted handler.
- `FacadeExternalSpawner` (impl `SubagentSpawner`): builds an
  `ExternalAgentMachine` from the mea data (runtime/worktree/policy/id) wrapped in
  a `RecordingExternalMachine` that snapshots final summary+usage+artifacts+
  cleanup flag; child scope provides `external()` = the injected handler.
- `DrivingSubagentHandler::fulfill(...)` drives it (derive_child → cancel/budget/
  trace/depth for free). Cancelled ctx → child drain abandons NeedExternalSession
  → external machine `mark_cleanup_required()` (cleanup marker asserted).

## Deliverables
1. `facade/external.rs`:
   - `ManagedExternalAgent` + builder gain `session_handler(Arc<dyn ...>)` escape
     hatch (manual Debug; handler opaque, not data). pub(crate) accessor.
   - `ManagedExternalDelegate { name, agent }` (+ `name()`/`agent()`), `with_name`.
   - `pub(crate) fn build_external_machine(...)`, `RecordingExternalMachine`,
     `FacadeExternalSpawner`, `ExternalDriveOutcome`, `drive_external(...)`.
2. `facade/delegate.rs`:
   - `Delegation::expose_external_agents_as_tools()` (no-op refinement).
   - `declarations(subagents, external)` + `route(subagents, external)` include
     external `ask_<name>` tools.
   - `DelegationRoute` carries local+external maps; `Resolved` gains `External`.
   - `DelegationRecorder` value → `RecordedDelegation { trace, artifacts,
     is_external }`. `DelegationToolHandler` drives external via `drive_external`.
3. `facade/run.rs`: (types already exist: `ArtifactRef`, `DelegationArtifact`).
4. `facade/agent.rs`:
   - `AgentBuilder::external_agent(name, mea)`, store `Vec<ManagedExternalDelegate>`.
   - build(): thread external delegates into declarations/route/collision check.
   - `Agent::external_agents()`. `collect_traces` → add `artifacts`,
     `external_usage`; emit `DelegationArtifact`; external usage → external slice.
   - run_full: `usage.add_external`, `artifacts: collected.artifacts`.
5. `facade/agent/stream.rs`: use `collected.artifacts`/`external_usage`; tap emits
   `DelegationArtifact` for external; read `RecordedDelegation.trace`.
6. `facade/mod.rs` + `prelude`? (prelude add is M4-R) export `ManagedExternalDelegate`.

## Validation
- Unit tests (offline scripted handler): supervisor `ask_coder` → Start→Completed;
  RunOutput.delegations has external (usage/status); RunOutput.artifacts has patch;
  events DelegationStarted/DelegationArtifact/DelegationFinished; cancel→cleanup.
- `cargo test -p agent-lib facade::external` + facade::delegate + facade::agent.
- Full seq: fmt; clippy --all-targets -D warnings (default + 4 ext features);
  test --all --all-targets; RUSTDOCFLAGS doc; git diff --check.

## Status
- [x] external.rs handler seam + delegate + drive helpers
- [x] delegate.rs route/recorder/handler external branch
- [x] agent.rs builder/collect_traces/run_full
- [x] stream.rs parity
- [x] tests + doctest (incl. drive_external_marks_cleanup_on_cancel)
- [x] validation seq + mark [DONE] + commit

## Result
M4-2 complete. All validation green: fmt, clippy (default + managed external
features), rustdoc (-D warnings), full `cargo test --all --all-targets` (0
failures), lib tests with managed features (855 passed), focused `facade::` (102
passed). TODO.md M4-2 marked [DONE] with a completion record. Cancel→cleanup
verification condition covered by an offline `drive_external` unit test asserting
the cleanup marker on a pre-cancelled RunContext.
