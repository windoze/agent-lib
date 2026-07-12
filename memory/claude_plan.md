## Execution Plan

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit only for unfinished work directly relevant to that selected task.
3. Read the selected task requirements, dependencies, and validation instructions.
4. Inspect the relevant code and tests for that task only.
5. Implement the task completely, or add the minimum prerequisite task to `TODO.md` if a concrete blocker makes completion impossible.
6. Run formatting, linting, and the relevant tests in the required order, escalating to the full suite when needed.
7. Update `TODO.md` by prefixing the completed task title with `[DONE]` and filling in its completion record, or document the blocker/prerequisite if blocked.
8. Update this file when key milestones are reached or the plan changes.
9. Commit all task-related changes with a descriptive message and stop without starting the next task.

## Current Progress

- Selected first incomplete task: `M1-3 [TODO] Usage`.
- Latest commit only records `M1-2` completion and does not mention unfinished work relevant to `M1-3`.
- Implementation target: fill `src/model/usage.rs` with the provider-neutral `Usage` model, provider-field normalization for Anthropic/OpenAI usage fragments, merge/total helpers, and focused unit tests.
- Implemented `Usage`, marked `M1-3` complete in `TODO.md`, and validated with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
