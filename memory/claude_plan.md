# M2-1: Record `ApprovalRequested` events in `Agent::run_full`

## Task (first incomplete in TODO.md)
`### M2-1 [TODO]` — the non-streaming `run_full` path must surface
`RunEvent::ApprovalRequested` in `RunOutput.events`, matching the streaming
path (which emits them live via `TapInteractionHandler`).

## Root cause
`run_full` builds events only from `collect_traces(done.notifications(), ..)`,
which covers tool/delegation lifecycle but NOT approval interactions. Streaming
emits approvals live through `TapInteractionHandler`; non-streaming has no
recorder.

## Design
1. Extract shared field-mapping helper `enriched_approval_request(approval,
   call_id, requirement)` into `src/facade/approval.rs` (peek `pending_request`,
   re-bind call_id + reason). Reuse it in stream.rs `TapInteractionHandler`.
2. Add `RecordingInteractionHandler` in `src/facade/agent.rs`: wraps the
   resolved interaction handler (injected or FacadeApproval fallback), records
   each paused approval into an ordered `Arc<Mutex<Vec<ApprovalRequest>>>`
   BEFORE delegating (so it never changes approve/deny/fallback behavior).
3. Add `weave_approval_events(events, approvals)`: insert `ApprovalRequested`
   immediately before the matching `ToolStarted` (by call_id); flush denied /
   never-started approvals in recorded order (tail or before next start).
4. Wire into `run_full`: build recorder + wrap `scope.interaction`, then
   `events: weave_approval_events(collected.events, recorded_approvals)`.
   Streaming path untouched (already emits live; collect_traces still approval-free).

## Tests (src/facade/agent/tests.rs)
- ask+approve (FacadeApproval fallback): ApprovalRequested precedes ToolStarted/Finished.
- injected handler deny: ApprovalRequested still present, tool never runs.
- headless ask, no handler (ApprovalPolicy::ask_tool): ApprovalRequested still recorded.

## Validation
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::agent::
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
Then mark M2-1 [DONE] in TODO.md + completion record, commit, stop.

## Status: DONE — all validation green, M2-1 marked [DONE] in TODO.md
