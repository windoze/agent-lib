# Task: M2-R Review — 基础 Agent facade 正确性与文档一致性检查

First incomplete task in `TODO.md` (M1-*, M2-1..M2-4 all `[DONE]`; M2-R is `[TODO]`).

## Scope (review + converge only; docs/facade-api.md §6.2, §7–§9, §8.3–§8.4, §19)

Verify `src/facade/{tool,approval,agent}.rs` against the spec, fix small
deviations, and record gaps as follow-up tasks.

## Review findings

- §8.3 internal mapping: `assemble_machine` builds `DefaultAgentMachine`; `run_full`
  drives via `drain(&mut machine, input, &scope, None, &ctx)`. Not bypassed. ✓ (§19)
- §7.1 typed tool schema (R1): `Tool::function` gated behind off-by-default
  `facade-schema` feature (`dep:schemars`); `function_with_schema` always available.
  Feature boundary documented in module docs + Cargo.toml. ✓
- §7.1 return types: `impl<T: Serialize> IntoToolResult` + `impl IntoToolResult for
  ToolResult` cover String/Value/Serialize/ToolResult. ✓
- §7.2 ToolContext: run_id/agent_id/tool_call_id/worktree/cancel/trace, no &mut Conv. ✓
- §7.3 escape hatch: `tool_registry` + `tool_declarations` builder methods; build-time
  `ensure_unique_tool_names` conflict check. ✓
- §9.1 three tiers (auto_allow/auto_deny/ask) + per-tool override; §9.2 default
  permission semantics: headless `ask` with no handler denies (not blocks);
  external-agent/worktree flags recorded for M4. ✓
- §8.4 loop policy defaults: max_steps=8, max_tool_rounds=4,
  tool_failure_policy=ReturnErrorToModel, non-streaming unless stream, pending
  failure cancels uncommitted work. ✓
- §6.2/§19 RunOutput: `collect_tool_traces` fills `tool_calls` + ToolStarted/Finished
  events; RunEvent enum covers full tool/delegation trace surface. ✓

## Concrete deviation to fix

- **prelude gap**: `src/prelude.rs` exports only M1 types. M2-R requires adding
  `Agent, Tool, Approval, ApprovalPolicy, ToolContext`. Fix + update module doc.

## Gaps vs §7–§9 (recorded as follow-up; already scheduled)

- `Agent::worker()` (§8.2) → M3-1 (scheduled).
- prelude `AgentSession`/`Delegation`/`ManagedExternalAgent` → later milestones
  (M3/M4/M5). `AgentSession` type-name is open question #2; §3 sanctions single
  `Agent` entry for v1, so no new task needed.

No unscheduled gaps or test failures found → no new prerequisite tasks needed.

## Steps
1. [done] Review facade tool/approval/agent vs spec.
2. [done] Fix prelude: add Agent/Tool/Approval/ApprovalPolicy/ToolContext + doc.
3. Validation sequence 1–6 (fmt, focused facade tests, clippy -D warnings,
   full test suite, rustdoc -D warnings, git diff --check).
4. Mark M2-R [DONE] in TODO.md with completion record + comparison table; commit.

## Status: done (validation 1-6 green; committing)
