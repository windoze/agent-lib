# Task: M3-1 `Agent::worker()` → `LocalSubagent` spec + `.subagent(..)` 注册

First incomplete task in `TODO.md` (M1-*, M2-* all `[DONE]`; M3-1 is first `[TODO]`).

## Spec anchors
- `docs/facade-api.md` §10.1/§10.3: `Agent::worker()` produces a **data-first**
  `LocalSubagent { name, description, spec: AgentSpec, tools: ToolSetRef, approval:
  ApprovalPolicy }`. No live client/closures. Child runtime built later at
  `NeedSubagent` (M3-2).
- R4 (PLAN.md §140): worker defaults to **inherit** supervisor provider/model;
  also supports explicit `.model(..)` and `.inherit_model()`.

## Design decisions
- New module `src/facade/delegate.rs`.
- `LocalSubagent`: private fields + accessors (codebase convention). Fields:
  name, description, spec, tools, approval, `inherit_model`. `inherit_model`
  is a data flag recording R4 so M3-2 can substitute supervisor model at
  delegation time. Derives Clone, Debug (no closure fields).
- Inherit mode: `spec.model` uses a documented placeholder ModelRef that M3-2
  replaces with the supervisor model.
- `AgentWorkerBuilder` (from `Agent::worker()`): description, model (explicit,
  clears inherit), inherit_model, max_tokens, temperature, system, approval,
  tool_declarations (data escape hatch), worktree, loop knobs, ids,
  build() -> Result<LocalSubagent, FacadeError>.
- Reuse build_loop_policy + DEFAULT_* consts from agent.rs (pub(crate)).
- AgentBuilder::subagent(name, LocalSubagent) sets name + pushes to
  `delegates: Vec<LocalSubagent>`; carried into Agent (+ AgentParts).
  `Agent::subagents() -> &[LocalSubagent]`. Restore -> empty (M3-3).
  Name-conflict erroring deferred to M3-3. Executable child tools -> M3-2.

## Steps
1. [done] Read TODO/spec/anchors.
2. delegate.rs: LocalSubagent + AgentWorkerBuilder + tests.
3. agent.rs: Agent::worker(), delegates field, subagents(), builder .subagent(),
   wire build/restore/into_parts; loop helpers pub(crate).
4. mod.rs: pub mod delegate + re-export.
5. Validation 1-6.
6. Mark M3-1 [DONE] + commit.

## Status: done (validation 1-6 green; committing)
