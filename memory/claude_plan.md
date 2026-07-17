# M7-4 — Production-grade registry-backed `ExternalSessionHandler` (feature-gated)

TODO.md first incomplete task = **M7-4** (line 1968). M7-1..M7-3 are `[DONE]`.

## Gap / goal
Running a real local CLI agent needs an injected `Arc<dyn ExternalSessionHandler>`,
but the whole library only ships **test doubles** (Fixed/Scripted/Counting). The
"last mile" — wiring a live adapter (`ClaudeCodeAdapter` etc.) + `ExternalSessionRegistry`
(`get_or_start`/`cleanup`) into an injectable handler — is copy-pasted in each
host (`examples/support/managed.rs`, `agent-testkit` runtime.rs). Consolidate it
into one official library-provided piece.

## Design
### Part A — runtime-agnostic library handler (no feature gate)
New module `src/agent/external/handler.rs` (+ `handler/tests.rs`):
- `pub struct RegistryExternalSessionHandler { registry: Arc<ExternalSessionRegistry>, sink: Option<Arc<dyn ExternalEventSink>> }`
- `new(registry)`, `with_sink(registry, sink)`, `registry()` accessor.
- private `advance()` = `get_or_start(request, ctx, sink.clone())` → `session.advance(input, ctx)` → fold via `.into()` (family-aligned `ExternalSessionResult`). A `get_or_start` failure folds to `Failed` (fail-fast, no wrong family).
- `impl ExternalSessionHandler::fulfill` = `RequirementResult::ExternalSession(Box::new(advance))`.
- Holds NO machine state (mirrors example/testkit shape) — this is the production one.
Re-export `RegistryExternalSessionHandler` from `src/agent/external/mod.rs`.

### Part B — feature-gated convenience constructor (facade)
In `src/facade/external.rs`:
- `pub async fn default_external_session_handler(agent: &ManagedExternalAgent) -> Result<Arc<RegistryExternalSessionHandler>, FacadeError>`
  - returns concrete `Arc<RegistryExternalSessionHandler>` so host keeps `.registry()` for `cleanup_agent`; coerces to `Arc<dyn ExternalSessionHandler>` at `.session_handler(..)`.
- private `build_default_registry(agent)` matches `agent.runtime()`:
  - `#[cfg(feature="external-claude-code")] ClaudeCode` → build `ClaudeCodeConfig` (working_dir/binary/model/permission_mode/timeout), `probe(&config)`, `ClaudeCodeAdapter::with_probed_capabilities`, wrap in registry.
  - `#[cfg(feature="external-codex")] Codex` → same with `CodexConfig`/`codex_probe`/`CodexAdapter`.
  - `#[cfg(feature="external-opencode")] OpenCode` → same with `OpenCodeConfig`/`opencode_probe`/`OpenCodeAdapter`.
  - `#[cfg(feature="external-acp")] Custom(label) if label==ACP_RUNTIME_LABEL` → rebuild `AcpConfig` from binary/args/working_dir/permission_mode, `AcpAdapter::new` (live-negotiated; no probe).
  - fallback `other =>` → `FacadeError::ExternalAgent` "runtime not enabled; rebuild with matching external-* feature" (fail-fast, no silent degrade).
  - probe failure → `FacadeError::ExternalAgent` (fail-fast). Non-secret message via `{error:?}` (classified errors carry no creds).
- Default IO/probe timeout constant (120s) applied for a generous live-session bound; documented.
- Re-export `default_external_session_handler` + `RegistryExternalSessionHandler` from facade mod / prelude as appropriate.

## Tests (offline)
- `src/agent/external/handler/tests.rs`: in-crate scripted adapter/session double drives
  start → PausedForInteraction → RespondInteraction → Completed (pause/resume on the
  same live handle), then `registry().cleanup_agent(..)` closes it (shutdown counted);
  plus a start-error case folds to `ExternalSessionResult::Failed` (fail-fast). No testkit (src convention).
- facade: `default_external_session_handler` with a bogus binary → `Err(FacadeError::ExternalAgent)` (probe fail-fast, offline). Under default build (all features off) any runtime → feature-disabled error.

## Validation
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. focus: `cargo test -p agent-lib --features external-claude-code facade::external` and `--lib external::handler`
4. `cargo test --all --all-targets` (<=30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`
+ extra: `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`

## Status
- [x] Part A handler + tests (2 lib tests pass)
- [x] Part B constructor + tests (2 facade tests pass, offline fail-fast)
- [x] re-exports (agent/external/mod.rs + facade/mod.rs)
- [x] validation (fmt, clippy default + 4-feature, doc default + 4-feature, full suite, git diff --check — all green)
- [x] TODO.md [DONE] + completion record + commit
