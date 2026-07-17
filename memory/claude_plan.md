# M3-2 Plan — model-routed delegation

Task: **M3-2** in `TODO.md` (first incomplete). Expose each registered subagent
as an `ask_<name>(task)` tool; on a model call, fulfil via the existing
`SubagentHandler` (`DrivingSubagentHandler`), drive the child machine, fold the
child summary back as the supervisor tool result, collect a `DelegationTrace`
into `RunOutput.delegations`, and emit `RunEvent::DelegationStarted/Finished/
Failed`. Child approval-requiring tools still trigger approval (§9.2).

## Key architectural facts
- `DefaultAgentMachine` only ever emits `NeedTool`, never `NeedSubagent`. So a
  delegation is intercepted at the `NeedTool` boundary by a custom `ToolHandler`
  that internally drives the child via `DrivingSubagentHandler` +
  `SubagentSpawner` — the established codebase bridge (see
  `tests/agent_tool_adapter.rs`, `tests/agent_complex_subagent.rs`). This is the
  faithful "reuse SubagentHandler" path.
- LLM request tools come from machine state (`current_tool_set().tools()`), so
  `ask_<name>` declarations MUST be appended to the supervisor's `AgentSpec`
  tool set at `AgentBuilder::build` time.
- `summarize(&TurnDone)` cannot see child machine state; a `RecordingChildMachine`
  wrapper captures the child's `final_turn_summary` (text + usage + stop_reason)
  into a shared slot when the child cursor reaches `Done`.

## Steps
1. run.rs: add `DelegationStatus{Completed,Failed}`; extend `DelegationTrace`
   with `status` + `usage`. Re-export `DelegationStatus` from `facade/mod.rs`.
2. agent.rs: make `assemble_machine` + `final_turn_summary` `pub(crate)`.
   Append delegation declarations to the spec tool set in `build`. Change
   `FacadeAgentScope.tool` to `DelegationToolHandler`. Build recorder + handler
   per-run in `run_full`. Replace `collect_tool_traces` with `collect_traces`
   that also yields delegations + subagent usage using the recorder.
3. delegate.rs: `delegation_tool_name`/`delegation_declaration`,
   `DEFAULT_MAX_DELEGATION_DEPTH`, `ChildSummary`, `RecordingChildMachine`,
   `ChildAgentScope`, `FacadeSubagentSpawner`, `DelegationToolHandler`,
   `EmptyScope`, `DelegationRecorder`.
4. stream.rs: route delegation through `DelegationToolHandler`, emit Delegation
   live events, assemble delegations + subagent usage from the recorder.
5. Tests in delegate.rs (offline).
6. Validate seq 1-6, mark [DONE], commit, STOP.

## Status
- [x] Investigation + design complete.
- [x] Implement run.rs / agent.rs / delegate.rs / stream.rs — compiles clean.
- [x] Tests (delegate.rs offline: declaration unit + `model_routed_tests` A/B) + validation seq 1–6 all green.
- [x] Marked M3-2 `[DONE]` in TODO.md with completion record.
- [ ] Commit + STOP (in progress).
