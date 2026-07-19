# Claude Execution Plan

## Scope

- Follow `TODO.md` as the authoritative task list.
- Identify and complete exactly the first task whose heading is not prefixed with `[DONE]`.
- Stop after committing that one task, or after committing any required prerequisite/blocker bookkeeping if the task cannot proceed.

## Step-by-Step Plan

1. Read `TODO.md` first to identify the first incomplete task and its validation requirements.
2. Check the latest commit message only for directly relevant unfinished work tied to that selected task.
3. Inspect the task-related code and documentation needed to implement the selected task.
4. Implement the smallest correct change that satisfies the task, avoiding workarounds or scope narrowing.
5. Add or update focused tests and documentation required by the task.
6. Run validation in the required order: `cargo fmt --all`, then `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets` with a timeout no greater than 30 minutes. Run feature-specific clippy if the touched code requires it.
7. If any unscheduled test failure appears, fix it if in scope or add the minimum prerequisite task to `TODO.md` before the blocked task, then stop after committing.
8. Mark the selected task as `[DONE]` in `TODO.md` only after implementation and required validation are complete.
9. Update this file with key progress changes and final validation results.
10. Inspect git status/diff/log before committing, then commit all intended task changes with a descriptive task-scoped commit message.

## Current Progress

- New invocation initialized. This file records the actionable plan and progress summary; private reasoning is intentionally not recorded.
- Selected first incomplete task: `M1-3` (`external 委派路径：NeedInteraction 路由到父级 handler（替换 EmptyExternalScope）`).
- Planned implementation focus: inspect the external facade drive/scope wiring, preserve existing no-parent-handler failure semantics with a clearer message, route external `NeedInteraction` to the parent-injected async `InteractionHandler` with `InteractionOrigin { delegate, depth }`, map the answer back to `RequirementResult::Interaction`, add offline `external-acp` tests, update managed external/capability docs, validate, mark `TODO.md`, commit, and stop.
- Latest commit checked: `dad7482 [M1-2] Route local child interactions to parent handler`; it does not state unfinished `M1-3` work.
- Implementation in progress: replaced the external child drive's empty outer scope with an optional parent-handler interaction route, added delegate/depth origin attribution, preserved explicit no-handler failure, and threaded the parent interaction handler from `DelegationToolHandler` into `drive_external`.
- Added `external-acp` gated offline facade external tests for parent-handler permission routing and no-parent-handler failure, using the scripted runtime handler rather than a real CLI/network.
- Documentation draft updated in `docs/facade-api.md`, `docs/managed-external-agent.md`, `docs/capability-matrix.md`, and `docs/mag-gaps.md`.
- Targeted validation progress: `cargo fmt --all` passed; `cargo test --features external-acp -p agent-lib --lib facade::external` passed without warnings after gating CLI-only process helpers away from ACP-only builds.
- Validation completed successfully: `cargo clippy --all-targets -- -D warnings`; `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`; `cargo test --all --all-targets`; `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`.
- `TODO.md` now marks `M1-3` as `[DONE]` with implementation notes, no-breaking-change note, adapter coverage notes, and validation results.
- Git status/diff/log inspected before commit; changed files are limited to the M1-3 implementation, tests, docs, TODO, and this progress record.
- Next step: create the task-scoped git commit and stop.
