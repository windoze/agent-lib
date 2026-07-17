# M3-R Plan — Review: Local subagent correctness + doc consistency

Task: **M3-R** in `TODO.md` (first incomplete). Review-only + convergence.
Milestone 3 (M3-1..M3-3) landed local subagent delegation.

## Scope (from TODO.md)
- Verify consistency with `docs/facade-api.md` §10, §13.1:
  - `Agent::worker()` produces data-first spec.
  - child built at `NeedSubagent` fulfillment (reuses `SubagentHandler`/
    `NestedMachine`, no new mechanism, §19).
  - model-routed default: one tool per delegate.
  - `DelegationTrace` / `RunEvent::Delegation*` complete.
  - snapshot/restore covers delegate fields and contains no secrets.
- `prelude` exposes `Delegation` (if public).
- Fix small-scope deviations; new features → prerequisite tasks per rules.

## Validation
- Full validation sequence 1–6 all green:
  1. `cargo fmt --all`
  2. `cargo clippy --all-targets -- -D warnings`
  3. clippy with external features
  4. `cargo test --all --all-targets`
  5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  6. `git diff --check`
- Comparison table: M3 implemented vs §10 promised items; gaps → follow-up tasks.

## Steps
1. Read docs §10/§13.1/§15.2/§18.3/§19 + delegate.rs + agent snapshot/stream +
   prelude/mod exports. Build the §10 promise→impl matrix.
2. Note any small deviations; fix in-scope only. Record real gaps as follow-ups.
3. Run validation seq 1–6.
4. Mark [DONE] in TODO.md + completion record (incl. comparison table). Commit. STOP.

## Status
- [x] Review + matrix — M3-1..M3-3 忠实实现 §10/§13.1/§15.2/§19；无需修正的规范偏离；
      prelude 已导出 Delegation。缺口（§11/§12/§13.2/§13.3）均已在 M4/M5 排期。
- [x] Validation seq 1–6 全绿（fmt no-op; clippy default+external clean; test --all green;
      doc clean; diff --check clean）。无源码改动。
- [x] Mark [DONE] + 完成记录（含对照表）写入 TODO.md；提交，STOP。
