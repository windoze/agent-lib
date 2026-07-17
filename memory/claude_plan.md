# M4-1 `ManagedExternalAgent` constructor + `ExternalRunMode` + capability grading

Task (TODO.md line 1063). First incomplete task. Offline-only construction +
capability validation. NO real CLI launch, NO delegate fulfillment (that is M4-2).

## Deliverables
- New module `src/facade/external.rs`.
- `ExternalRunMode` enum: BlackBox / Managed / ManagedWithTools / Attachable.
  - required_capabilities() mapping:
    - BlackBox -> {}
    - Managed -> {Streaming}
    - ManagedWithTools -> {Streaming, HostTools}
    - Attachable -> {Streaming, Resume}
- `ExternalAgentCapabilities`: facade view over `ExternalRuntimeCapabilities`.
  - from_runtime_capabilities / supports(cap) / supports_mode / missing_for_mode /
    highest_supported_mode / runtime() / as_runtime_capabilities().
  - `#[cfg(external-acp)] from_acp_negotiation(&AcpNegotiatedCapabilities)`.
- `ManagedExternalAgent` (data-first): runtime, mode, capabilities, worktree,
  binary, model, args, permission_mode. Accessors only; no live handles.
- `ManagedExternalAgentBuilder` + presets:
  - `::claude_code()`, `::codex()`, `::opencode()` (always available).
  - `#[cfg(external-acp)] ::acp(binary,args)`, `::claude_agent_acp()`,
    `::codex_acp()`, `::opencode_acp()`, `::gemini_acp()`.
  - builder: .worktree/.mode/.binary/.model/.arg/.args/.permission_mode/
    (`.acp_negotiated(..)` under feature) + .build().
- `build()` validates mode vs capabilities -> fail fast `FacadeError` when unmet.
- New `FacadeError::UnsupportedExternalMode { runtime, mode, missing }`.
- Baseline capabilities per runtime mirror adapter `implemented_capabilities()`:
  - ClaudeCode: streaming,resume,permission_bridge,artifacts,usage,graceful (host_tools/subagents/reconfigure=false)
  - Codex/OpenCode: same but permission_bridge=false
  - ACP: capabilities_from_initialize(none) = streaming,permission_bridge,graceful (resume via negotiation)
  Documented as refined by probe/negotiation at run time; not hard-coded truth.
- Export from `facade::mod`. rustdoc complete (note negotiation-filled caps).

## Validation
- Unit tests (offline): each preset -> correct kind + default caps; unsupported
  mode -> fail fast; ACP negotiation mapping (resume via load_session) — ACP tests
  `#[cfg(feature="external-acp")]`.
- Focus: `cargo test -p agent-lib facade::external` (+ `--features external-acp`).
- Full seq: fmt; clippy --all-targets -D warnings; clippy with all 4 external
  features; test --all --all-targets; RUSTDOCFLAGS doc; git diff --check.

## Status
- [ ] Implement module + FacadeError variant + exports
- [ ] Tests
- [ ] Validation seq
- [ ] Mark [DONE] in TODO.md + commit

## Result (2026-07-18) — DONE
- Implemented src/facade/external.rs (ExternalRunMode, ExternalAgentCapabilities,
  ManagedExternalAgent + builder + presets), FacadeError::UnsupportedExternalMode,
  facade::mod exports.
- facade::external tests: 8 (default) / 10 (external-acp) pass.
- Validation seq 1-6 all green (fmt; clippy default + 4 features; test --all
  --all-targets; doc default + 4 features; diff --check).
- TODO.md M4-1 marked [DONE] with completion record. Committed.
