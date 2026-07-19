# Execution Plan

## Current Status

- Invocation started: 2026-07-19.
- First action: create this progress plan before running project commands.
- Private reasoning is not recorded here; this file contains the actionable execution plan and progress log.
- First incomplete task identified: `M7-2 [TODO] StreamEvent::Usage 语义契约文档化并断言（M-ADP-1）`.

## Plan

1. Read `TODO.md` to identify the first incomplete task, where only headings prefixed with `[DONE]` count as complete.
2. Check the latest commit message for an explicitly unfinished issue relevant to that task.
3. Inspect the task requirements and any directly relevant code or docs.
4. Implement the task as written, or add the minimum prerequisite task to `TODO.md` if a concrete blocker makes implementation impossible.
5. Run required validation in the prescribed order: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, then relevant/full tests as required.
6. Update `TODO.md` completion record and prefix the completed task heading with `[DONE]` if the task is fully complete.
7. Commit all task-related changes with a clear task-scoped message.
8. Stop after completing exactly one task.

## Progress Log

- Created initial execution plan.
- Read `TODO.md` and selected `M7-2` as the current execution unit. Earlier tasks through `M7-1` are marked `[DONE]`; `M7-2` is the first heading still marked `[TODO]`.
- Latest commit checked: `ec86516 [M7-1] Update execution log`. It does not mention an unfinished issue directly relevant to `M7-2`.
- Current implementation direction: inspect `StreamEvent::Usage`, adapter stream normalizers, accumulator behavior, and existing cassette/fixture tests; then document one consistent consumption contract and add tests proving direct event consumption matches `collect`.
- Implemented the selected contract: every `StreamEvent::Usage` is an additive segment. Updated rustdoc for `StreamEvent::Usage` and `Usage::merge`, and added Anthropic/OpenAI stream fixture tests that merge usage events directly and compare them to accumulator output.
- Task-specific adapter/stream tests, `cargo fmt --all`, default clippy, and `cargo test --all --all-targets` passed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` failed on a redundant explicit intra-doc link in `src/stream/mod.rs`; fix the link and rerun documentation validation.
- Fixed the redundant rustdoc link and reran `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`; documentation validation passed.
- Updated `docs/review-2026-07.md` to mark M-ADP-1 fixed in M7-2.
- Updated `TODO.md`: M7-2 is now `[DONE]` with completion record, validation summary, and compatibility note.
- Inspected git status, full diff, and recent log; change set is scoped to M7-2. `git diff --check` passed.
- Next step: commit the M7-2 changes and stop.
