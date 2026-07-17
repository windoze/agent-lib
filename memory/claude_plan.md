# M4-R Review：Managed external agent 正确性与文档一致性检查

Task: TODO.md M4-R (first incomplete). Review-only + reconcile task over M4-1..M4-3.
Verify facade external delegate matches docs/facade-api.md §11 / §9.2 / §15.2–§15.3 /
§6.2 / §19; fix small deviations; produce comparison table; record gaps.

## Findings
- CONCRETE FIX: prelude was missing `ManagedExternalAgent` (M4-1 deferred it here,
  "prelude 补录留 M4-R"). §3 prelude list names only `ManagedExternalAgent` (NOT
  ExternalRunMode/RestoreExternal), so add exactly that one. -> src/prelude.rs.
- Background review (explore agent) + manual read confirmed NO other code deviation:
  * mapping NeedSubagent->ExternalAgentMachine->ExternalSessionHandler->adapter reused (§19).
  * capability grades honest (wrap ExternalRuntimeCapabilities + ACP negotiation, fail-fast).
  * snapshot data-only (no handle/secret/client/closure); default RestoreExternal=MarkInterrupted.
  * external start gated by resolve_external_start; headless-no-policy denies (not hang).
  * RunOutput expresses delegation+artifacts+events (§6.2).
- Reconciliation: `ask_worktree_write` is ADVISORY (rustdoc says so), satisfied via §9.2's
  "或显式 opt-in" branch (managed child runs in isolated throwaway worktree explicitly
  configured via .worktree()). M4-3 record's "升级为强制执行" phrasing was imprecise for
  worktree-write specifically; only external-agent START is hard-gated (ask_external_agents).
  Behavior is spec-compliant -> no code change; clarified in M4-R record.
- Future/deferred (foundation-gated, R8/R9; spec §11.3 marks host_tools "后续能力",
  resume depends on ACP loadSession): live host-tool injection (ManagedWithTools runtime)
  and live attach/resume (Attachable runtime) + resume approval gate are NOT M4 deliverables;
  facade honestly fail-fasts, does not pretend. Recorded in comparison table; no new task
  (not committed scope, not blocking) — same handling as M3-R for §11/§12/§13 gaps.

## Validation (seq 1-6 + ext clippy) — ALL GREEN
1. cargo fmt --all -- --check ✓
2. cargo test -p agent-lib --lib facade:: ✓ (108 passed)
3. cargo clippy --all-targets -- -D warnings ✓
   + --features "external-claude-code external-codex external-opencode external-acp" ✓
4. cargo test --all --all-targets ✓ (50 binaries, 0 failures)
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace ✓
6. git diff --check ✓

## Status
- [x] prelude fix + rustdoc
- [x] full validation
- [x] TODO.md M4-R [DONE] + comparison table + commit
