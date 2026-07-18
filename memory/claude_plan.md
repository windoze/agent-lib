# Current task: M1-1 — RunStream drop-time pending-turn cleanup

TODO.md was rewritten (commit aae2997) with a new "refine" task set M1–M6, all [TODO].
First incomplete task = **M1-1** (TODO.md line 15): fix `ChatSession::stream` leaving a
pending turn when the returned `RunStream` is dropped early.

## Root cause
- `ChatSession::stream` (src/facade/chat.rs:517) calls `begin_turn` then hands back a
  `RunStream` borrowing the conversation. `RunStream` (src/facade/chat/stream.rs) has NO
  `Drop` impl, so an early drop leaves the pending turn open.
- Consequences: next `snapshot()` fails (SnapshotError::PendingTurn); next `send`/`stream`
  hits `begin_turn` on an already-pending conversation.
- The `stream()` rustdoc already PROMISES drop discards the in-flight turn — doc/impl mismatch.

## Fix plan (src/facade/chat/stream.rs)
1. Replace `rollback()` with a single idempotent `abandon()` helper:
   if `state != Done` -> `cancel_pending(DiscardTurn)` + set `state = Done`.
   This converges the error-path rollback and drop rollback into one helper (spec req).
2. `absorb` / `finish` error paths call `self.abandon()` instead of `self.rollback()`.
3. poll_next: absorb-error + stream-error branches use `abandon()` (drop redundant state sets);
   Finishing branch keeps `state = Done` for the commit-success case.
4. Add `impl Drop for RunStream` calling `self.abandon()`.
   - Normal completion => state already Done => drop is a no-op (no double rollback).
   - Errored => already abandoned => no-op.
   - Non-terminal drop => rolls back pending turn.

## Tests (src/facade/chat/tests.rs)
- stream created, not polled, dropped -> subsequent `send` succeeds.
- stream read >=1 delta, dropped -> `snapshot` succeeds AND next `send` request count
  proves no half assistant turn committed.
- stream drained fully then dropped -> committed assistant turn NOT rolled back.
Use existing StreamingFakeClient / text_stream_events harness.

## Validation
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::chat::
- cargo test -p agent-lib --lib facade::agent::  (per TODO verify block)
- full: cargo test --all --all-targets (code changed)
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

## Status
- [x] implement stream.rs (abandon() helper + Drop guard)
- [x] add 3 tests + DualFakeClient
- [x] validate (fmt/clippy/focused chat 19 passed/full suite green/doc clean)
- [x] docs/refine.md #1 status note
- [x] mark M1-1 [DONE] in TODO.md
- [ ] commit (in progress)
