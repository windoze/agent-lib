# Current task: M1-2 — `AgentRunStream` drop-time cleanup of an unfinished run

First incomplete task in TODO.md = **M1-2** (line 68): fix `AgentRunStream`
(`src/facade/agent/stream.rs`) so an early drop does not strand an unfinished run on the
agent's held `DefaultAgentMachine`. After an early drop the same `Agent` must `run` again.

## Root cause
- `Agent::stream` -> `stream::start` builds a boxed drive `future` capturing
  `&mut agent.machine` and calling `drain(machine, ...)`. `AgentRunStream` has no other
  handle to the machine and no `Drop`, so an early drop leaves the machine's in-flight
  turn uncommitted with an outstanding requirement on the loop cursor.
- The machine already exposes the sans-io cleanup: `StepInput::Abandon(id)` closes the
  in-flight turn and settles the cursor to `Idle` (feedable). The cursor keeps the
  outstanding requirement id(s) after the drive future is dropped
  (`LoopCursor::pending_requirement_ids`).
- Blocker: `&mut machine` is buried in the opaque `drain` future, so `Drop` can't reach
  it. Re-driving the future is not viable (a run parked in an approval handler never
  resolves).

## Design
Share the machine via `Rc<RefCell<&mut DefaultAgentMachine>>` (`MachineCell`). A custom
`drive_streamed` mirrors `drain`'s loop but holds the RefCell borrow only across the sync
`machine.step()` calls, releasing it before each `fulfill_batch(...).await`. So `Drop`
can `try_borrow_mut` and `Abandon` while the future is parked.

- Replace `drain(...)` in the normal `start` path with `drive_streamed`. Reuse
  `fulfill_batch`, `Resolved`, `record_requirement`, `record_requirement_resolution`,
  `is_terminal` from drive.rs so streamed turns stay equivalent to `run_full`.
- Store a `MachineCell` clone in `AgentRunStream` for all three start paths. Rules/
  dispatcher futures never step the machine, so their cursor stays `Idle` -> drop no-op.
- `impl Drop` -> `abandon()`: if `state != Done`, mark `Done` and, via the cell, feed
  `StepInput::Abandon(first_outstanding_id)` when the cursor still has an outstanding
  requirement. Idempotent; committed/errored drives have no outstanding requirement so
  abandon never rolls back a committed turn.

## drive.rs changes
Make `pub(crate)`: `fulfill_batch`, `Resolved` (+ fields), `record_requirement`,
`record_requirement_resolution`, `is_terminal`.

## Tests (src/facade/agent/tests.rs)
`DropTestClient` scripts both `chat` (recovery) and `chat_stream` (streamed turn,
optionally parking after its events) and records recovery request message counts.
1. stream created, never polled, dropped -> next `run` succeeds.
2. stream read a partial `TextDelta` (parking stream) then dropped -> next `run` succeeds
   and the discarded partial turn is not in committed history.
3. stream parked awaiting approval (parking interaction handler) then dropped -> next
   `run` succeeds.

## Validation
cargo fmt --all; cargo clippy --all-targets -- -D warnings;
cargo test -p agent-lib --lib facade::agent::stream; cargo test -p agent-lib --lib facade::agent::;
cargo test --all --all-targets; RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace.

## Status
- [x] drive.rs: expose helpers pub(crate)
- [x] stream.rs: MachineCell + drive_streamed + Drop
- [x] tests (4: never-polled, partial-events, awaiting-approval, awaiting-tool)
- [x] validation (fmt, clippy default + external features, full suite 841 lib + integration, doc)
- [x] docs/refine.md issue #1 status updated for M1-2
- [x] TODO.md [DONE] + completion record
- [ ] commit (final step)
