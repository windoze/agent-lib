# Execution Plan

## Constraints

- Use `TODO.md` as the authoritative task source and complete exactly the first task whose heading is not prefixed with `[DONE]`.
- Do not perform broad issue triage before selecting the current task.
- If a concrete blocker prevents the current task, add the minimum prerequisite task in `TODO.md`, commit that bookkeeping, and stop.
- Mark a completed task by prefixing its `TODO.md` heading with `[DONE]` and updating its completion record.
- Run validation in the required order: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, then relevant/full tests as appropriate.
- Commit all intended changes for the task before stopping.

## Step-by-Step Plan

1. Read `TODO.md` to identify the first incomplete task and its exact requirements.
2. Check the latest commit only for directly relevant unfinished work tied to that selected task.
3. Inspect the minimal code and documentation needed for that task.
4. Implement the task with small targeted patches, updating this plan file at key milestones.
5. Add or adjust tests and documentation required by the task.
6. Run formatting, linting, and tests according to the validation requirements.
7. Update `TODO.md` completion status and completion record for the selected task.
8. Inspect git status and diff, then commit the task changes with a descriptive message.
9. Stop without starting the next task.

## Progress Log

- Initialized execution plan before reading task details.
- Selected current task: `M5-1 [TODO] run_full 增加 drop/timeout 安全防护（H-STATE-3）`.
- Task goal: make non-streaming facade runs recover the machine after the run future is dropped or times out, then verify the next run can complete normally.
- Implemented the first pass: added a non-streaming `run_full` drop guard, shared the synchronous abandon helper with `AgentRunStream`, documented timeout/drop recovery, and added an offline timeout regression test.
- Validation completed: `cargo test -p agent-lib --lib facade::agent`, `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all --all-targets`, and `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` all passed.
- Updated `TODO.md` to mark M5-1 done and `docs/review-2026-07.md` to mark H-STATE-3 fixed.
