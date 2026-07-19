## Execution Plan

This file records the actionable plan and progress updates for the current invocation. It intentionally contains a concise rationale and step-by-step execution plan, not private hidden reasoning.

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit only for unfinished work directly relevant to that task.
3. Inspect the code and documentation needed for that task, avoiding unrelated historical triage.
4. Implement the task exactly as specified, or add the minimum prerequisite task if a concrete blocker makes completion impossible.
5. Run required formatting, linting, and tests in the requested order, using the task's validation requirements plus relevant targeted checks.
6. Update `TODO.md` completion record and prefix the task title with `[DONE]` only after implementation and validation are complete.
7. Commit all intended changes with a clear task-scoped commit message, then stop without starting the next task.

## Progress

- Initial plan recorded before reading the task list.
- First incomplete task identified: `M2-2 [TODO] reconfig handler 接线（流式 + 非流式）与 ReplaceToolSet 一致性`.
- Task-specific plan:
  1. Check current git status and recent commit for directly relevant unfinished M2-2 context.
  2. Inspect facade non-streaming and streaming drive paths, current reconfig queue handling, and tool registry abstractions.
  3. Implement reconfig requirement handling in both drive paths with explicit declaration/registry consistency checks for `ReplaceToolSet` and `PatchToolSet`.
  4. Ensure applied reconfig is observable through trace/run behavior as required by the task documentation.
  5. Add tests for non-streaming, streaming, removed-tool error, and declaration/registry mismatch failure.
  6. Run validation in order: `cargo fmt --all`, relevant targeted tests, `cargo clippy --all-targets -- -D warnings`, full tests/doc checks as needed.
  7. Mark M2-2 `[DONE]` with completion notes, commit the task-scoped changes, and stop.
- Implemented facade reconfig wiring in progress:
  - Added shared registry-slot helper for `ToolRegistryHandler` + `ReconfigRegistryHandler`.
  - Added facade resolver/active registry filtering so queued `ToolSetRef` values must map to registered facade tool names and removed tools stop executing across later runs.
  - Wired non-streaming and streaming facade scopes to expose `reconfig()`.
  - Added tests for non-streaming replace, non-streaming patch, streaming replace, and unbacked declaration failure.
- Validation phase started with `cargo fmt --all`.
- Validation completed:
  - `cargo fmt --all`
  - `cargo test -p agent-lib --lib facade::agent`
  - `cargo test -p agent-lib --lib facade::`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test --all --all-targets`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
- Marked M2-2 `[DONE]` in `TODO.md` with completion notes. Diff/status review is complete; preparing the task-scoped commit.
