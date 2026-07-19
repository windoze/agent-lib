# Execution Plan

I will execute exactly the first incomplete task from `TODO.md` and then stop. This file records the actionable plan and progress updates; it does not include private reasoning.

## Selected Task

`M7-4 [TODO] 线缆容错批量修复（adapter L2/L3、external L-1、facade L1）`

## Required Outcomes

1. Treat empty OpenAI Responses tool-call `arguments` as `{}` in both streaming and non-streaming paths.
2. Make Anthropic streaming tolerant of missing optional usage/stop metadata while preserving raw evidence and normalized output.
3. Make managed CLI decoders tolerate a bounded number of non-JSON noise lines before failing with a protocol error; tolerated diagnostics must not include raw line content.
4. Make usage detail parsing skip non-object detail fields such as `null` instead of failing the whole usage payload.
5. Add one focused regression test per required tolerance path.
6. Update the owning docs/review entry and mark only M7-4 as `[DONE]` after validation passes.
7. Commit all intended changes with an M7-4-specific message, then stop.

## Execution Steps

1. Check latest commit for directly relevant unfinished M7-4 work and inspect current git status without altering unrelated changes.
2. Inspect the implementation points listed in `TODO.md`: OpenAI Responses convert/stream normalizer, Anthropic stream wire/normalizer, Claude/Codex/OpenCode decoders, and `Usage` deserialization.
3. Apply small targeted patches for each tolerance class, re-reading affected sections before edits.
4. Add/adjust unit tests for the four required behavior classes.
5. Run targeted tests first where useful, then required validation in order: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, external-feature clippy, `cargo test -p agent-lib --lib`, external feature tests, full suite if needed by observed changes, and rustdoc.
6. If any failing test appears and is not already explicitly scheduled, fix it or add the minimum prerequisite/follow-up task before marking M7-4 complete.
7. Update `TODO.md` completion record and `docs/review-2026-07.md` M7-4-related entries after implementation and validation.
8. Commit all changed files, including this plan file, and stop without starting M7-5.

## Progress Log

- Read `TODO.md` and selected first incomplete task: `M7-4 [TODO] 线缆容错批量修复`.
- Wrote this updated execution plan before running build, test, or Git commands.
- Latest commit checked: `[M7-3] Relax OpenAI stream sequence validation`; it does not identify unfinished work that blocks M7-4.
- Implemented the four M7-4 tolerance classes: empty OpenAI function arguments parse as `{}`, Anthropic missing usage/stop metadata no longer aborts streams, CLI decoders tolerate up to 8 consecutive non-JSON noise lines, and non-object usage details stay in `extra` instead of failing deserialization.
- Targeted validation passed: `cargo test -p agent-lib --lib adapter::openai_resp`, `cargo test -p agent-lib --lib adapter::anthropic`, `cargo test -p agent-lib --lib model::usage`, and `cargo test -p agent-lib --features "external-claude-code external-codex external-opencode" --lib agent::external`.
- Full validation passed: `cargo fmt --all`, default clippy, external-feature clippy including ACP, `cargo test -p agent-lib --lib`, `cargo test --all --all-targets`, `cargo test --features "external-claude-code external-codex external-opencode external-acp" --all-targets`, and rustdoc with `-D warnings`.
- `TODO.md` now marks M7-4 as `[DONE]`; `docs/review-2026-07.md` marks the four M7-4 protocol parsing bullets fixed while leaving the `ContentBlock` Unknown item for M7-5.
- After final cassette-test adjustments, default clippy, external-feature clippy, and rustdoc were rerun and passed.
