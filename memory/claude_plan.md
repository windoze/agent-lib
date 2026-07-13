# 执行计划 — M4-3 前置：发现 reconfig 迁移缺口，插入 M4-2a 并停止

## 选中的任务
`TODO.md` 第一个未完成任务 = **M4-3**（M1..M4-2 全 `[DONE]`）。M4-3 = 删除
`respond_approval` + `ApprovalWaiters`、降级/删除 `AgentFeedGuard`，并做**决策 E**
（`DefaultAgentLoop` 去留），保证 loop 测试迁移或随实现删除且被参考 driver 等价覆盖。

## 调查结论
1. `DefaultAgentLoop`/`AgentLoop`/`AgentEventStream`/`AgentFeedGuard`/`respond_approval`/
   `ApprovalWaiters` 只被 `src/agent/loop_driver/` 自身及其测试使用；`examples/`、`tests/`
   集成测试都不用它。即 legacy loop 及其 streaming 事件流**没有外部消费者**。
2. sans-io 机器（`DefaultAgentMachine`）已完全接管审批策略路径（按 `ApprovalRequirement`
   拆分、`NeedInteraction`、`AwaitingApproval` park、`RequirementResult::Interaction` 回程）。
   参考 driver（`drive`/`drive/reference`）是效果模型继任者，其 `*_matches_default_loop`
   测试等价覆盖 text/单工具/并行工具/工具失败/审批批准/审批拒绝/headless/cancel。
3. 因此**决策 E = 删除 legacy loop** 是工程上正确的终局（loop 无消费者，机器+参考 driver
   等价覆盖，且移除审批 responder 会让 loop 的审批 await 路径无法闭合，半删的 loop 是
   不自洽且能力更弱的重复实现）。

## 阻塞点（关键）：reconfig turn-boundary 应用未迁移，且未被任何任务追踪
- **reconfig-at-turn-boundary 应用只有 legacy loop 实现**：`LoopRuntime`
  `apply_queued_reconfigs_before_turn`（turn 起始前应用）与
  `prepare_queued_reconfig_application`（commit 处应用并写 `reconfigs` metadata）。
- sans-io 机器（`src/agent/machine/`）与参考 driver（`src/agent/drive/`）**零 reconfig
  代码、零 reconfig 测试**（已用 `git grep` 在 HEAD 上核实）。
- `TODO.md` 与 `docs/agent-effect-migration.md` **完全没有 reconfig 迁移任务**（已核实；
  仅 M4-2 完成记录里附带提到 reconfig metadata 链保留与一个已删测试名）。
- 若按决策 E 删除 loop 而不先迁移 reconfig，会造成三个真实问题：
  1. **功能回归**：`AgentState::queue_reconfig` 仍在（`pub`），但没有任何代码在 turn 边界
     应用 queued reconfig → 队列累积却永不生效。
  2. **测试等价覆盖缺失**：loop 的 3 个 reconfig 测试
     （`reconfig_queued_during_text_turn_applies_at_turn_boundary_and_next_request_changes`、
     `reconfig_during_tool_turn_keeps_current_turn_registry_snapshot`、
     `conflicting_reconfig_requests_are_rejected_atomically`）随 loop 删除后，参考 driver
     没有等价 reconfig 测试 → 违反 M4-3 bullet 3 的“等价覆盖”要求。
  3. **dead code → clippy -D warnings 失败**：`plan_reconfig_with`、
     `queue_prevalidated_reconfig`、`apply_reconfig_application`、
     `ReconfigApplication::current_tool_set`、`ReconfigQueue::clear` 将无生产调用者。
- **难点**：reconfig 应用依赖 `LoopRuntime::resolve_reconfig_registry`，它调用
  `tool_registry_resolver.resolve_tool_set(ToolSetRef)` —— 这是 **host I/O effect**。
  sans-io 机器不能内联执行，必须像 `NeedTool` 一样把 registry 解析 reify 成一个
  requirement/effect，由 driver 解析后回喂。这是一块设计密集、milestone 量级的真实工作。

## 决定（遵循 roadblock / No-Workaround 政策）
reconfig 迁移是 M4-3（决策 E = 删除 loop）的**具体、未追踪前置依赖**。不能通过换建模/
缩小范围/私有特例绕过。按工作流：
1. 已把投机性的 M4-3 loop 删除改动**全部回退**（`git checkout HEAD -- .`，工作树干净），
   因为 loop 必须存活到 reconfig 迁移完成之前。
2. 在 `TODO.md` 的 M4-2 与 M4-3 之间插入新前置任务 **M4-2a**（reconfig 迁移进 sans-io
   机器 / 参考 driver，含 registry 解析 effect），并把 M4-3 的“前置依赖”改为
   `M4-2、M4-2a`。
3. 更新本计划文件。
4. 提交 `TODO.md` + 本计划，**停止**（不实现 M4-2a，也不实现 M4-3）。下一次调用会把
   M4-2a 作为第一个未完成任务实现。

## M4-2a 实现指引（供下一次调用）
- reify registry 解析为 sans-io effect（新 `RequirementKind::NeedReconfigRegistry { tool_set }`
  或等价）：机器在 turn 边界发现 queued reconfig 改变 tool set 时发出该 requirement，driver
  用 `ToolRegistryResolver` 解析并回喂 registry（校验 declarations 匹配 `ToolSetRef`，不匹配
  fail）；tool set 未变则短路无 effect（复用 `resolve_reconfig_registry` 逻辑）。
- 机器 `begin_user_turn`（turn 边界）应用 queued reconfig：`queued_reconfig_application()` →
  （必要时）registry effect → `apply_reconfig_application()` → 切换 current tool set/registry →
  step boundary metadata 写 `reconfigs`（复用 `reconfig_records`/`reconfig_metadata` 语义）。
- 提供机器 reconfigure/队列入口（对应已移除的 `AgentLoop::reconfigure`）：`plan_reconfig_with`
  校验 → 解析 registry 校验 declarations → `queue_prevalidated_reconfig`。入口形态实现时定。
- 参考 driver（`drive`）串接新 effect（`ReferenceScope` 加 resolver，`drive_turn` 处理）。
- 保持 `apply_reconfig_application`/`current_tool_set`/`ReconfigQueue::clear`/`plan_reconfig_with`/
  `queue_prevalidated_reconfig` 全部有真实调用者（无 dead code）。
- 迁移 loop 的 3 个 reconfig 测试到机器/参考 driver 路径，等价覆盖；每个 <1min。
- 不删除 legacy loop（留到 M4-3）。机器与 loop 各操作自己的 `AgentState` 实例，互不干扰。

## 本次调用产物
- 回退全部 M4-3 投机改动（工作树回到 HEAD = M4-2）。
- `TODO.md`：插入 M4-2a，更新 M4-3 前置依赖。
- `memory/claude_plan.md`：本文件。
- 提交并停止。
