# M1-3 Review: 流式生命周期恢复

## Task
Review task (`### M1-3 [TODO] Review：流式生命周期恢复`) — verify the M1-1/M1-2
streaming-lifecycle drop fixes are correct and complete. Review-only: no
behavioral code changes expected unless a defect is found.

## Review checklist (from TODO.md)
- [x] `ChatSession::stream` / `RunStream`: normal/ error/ early-drop paths.
  - `RunStream` has idempotent `abandon()` shared by error branches + `Drop`;
    normal `Done` and error mark `state=Done` so drop is a no-op.
- [x] `Agent::stream` / `AgentRunStream`: normal/ error/ early-drop paths.
  - `abandon()` via shared `MachineCell` feeds `StepInput::Abandon(first id)`
    only when `state != Done` and cursor has an outstanding requirement.
  - rules-/dispatcher-routed starts never step the machine (cursor Idle) → no-op.
- [x] Other facade stream types opening pending state without drop cleanup?
  - grep: only `RunStream` + `AgentRunStream` are facade streams that open
    conversation/machine pending state. Both have `Drop`. Adapter/client streams
    are pure wire streams (no facade pending state).
- [x] New tests only use fake client / scripted handler / testkit.
  - chat: `DualFakeClient` (offline). agent: `DropTestClient`,
    `ParkingInteractionHandler`, `parking_weather_tool` (offline).
- [x] Docs describe drop/close behavior.
  - `RunStream`, `AgentRunStream`, `ChatSession::stream`, `Agent::stream` docs
    all state early-drop auto-discards the in-flight turn. `docs/refine.md`
    issue #1 has fix status for M1-1 + M1-2.

## Validation (M1-3 conditions)
- [x] cargo fmt --all (no source changes)
- [x] cargo clippy --all-targets -- -D warnings (clean)
- [x] cargo test -p agent-lib --lib facade::chat:: (19 passed)
- [x] cargo test -p agent-lib --lib facade::agent:: (30 passed)
- [x] manual recheck docs/refine.md issue #1 status (already current)

## Status
- Review found no defects. Only TODO.md gets the [DONE] mark + completion record.
- No code changed → full suite rerun not required (only docs/TODO edits).
