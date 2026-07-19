# Execution Plan

## Scope

- Work from `TODO.md` as the authoritative source.
- Select exactly the first incomplete task whose heading is not prefixed with `[DONE]`.
- Complete that one task only, then stop after committing.
- Update this file whenever the plan changes or a key step completes.

## Plan

1. Read `TODO.md` first and identify the first incomplete task.
2. Inspect the latest commit only for directly relevant unfinished work tied to that task.
3. Read the task's referenced files and requirements, avoiding unrelated historical triage.
4. Implement the smallest complete change that satisfies the task without workarounds or scope narrowing.
5. Add or update focused tests and documentation required by the task.
6. Run validation in the required order: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets` with a timeout under 30 minutes; include feature-specific validation if touched.
7. If an unscheduled failing test or blocking spec mismatch appears, either fix it or add the minimum prerequisite task before the blocked task in `TODO.md`, then commit and stop.
8. Mark the completed task by prefixing its title with `[DONE]` and update its completion record.
9. Commit all task-related changes with a descriptive message and stop without starting the next task.

## Progress Log

- Initial execution plan written before reading project task files or running commands.
- Identified first incomplete task: `M2-1 [TODO] facade reconfig 入口 API 与校验`.
- Current scope: expose and validate a facade reconfiguration entry point, document its chosen timing semantics, and add the required focused tests for `facade::agent`.
- Implemented initial M2-1 code path: `Agent::reconfigure`, facade reconfig re-exports, request-family validation, between-run cursor validation, declaration-level tool-set admission, and focused facade tests.
- Updated docs describing the M2-1 facade timing choice and the remaining M2-2/M2-3 boundaries.
- Validation completed: `cargo fmt --all`, `cargo test -p agent-lib --lib facade::agent`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all --all-targets`, and `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` all passed.
- Marked `M2-1` as `[DONE]` in `TODO.md` with a completion record. Preparing final git commit for this task only.
- Corrected one post-validation Markdown-only wording issue in `docs/mag-gaps.md` so its A2 requirement text matches the M2-1 facade `InvalidState` timing choice; no code changed after validation.
