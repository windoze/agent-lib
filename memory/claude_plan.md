# M3-4 Review：interaction/subagent parity 正确性检查

**当前执行 = TODO.md 第一个未完成任务 = M3-4**（M1-*、M2-*、M3-1/2/3 已 `[DONE]`）。
这是 **review 任务**（不可跳过、不可拆分）。

## 做什么（来自 TODO.md M3-4）
1. 手工检查 `src/agent/external/machine.rs`:resume 各相位 id/family 校验；NeedSubagent 只 reify；
   spawn_agent 特判不落入普通 ToolRegistry；interaction 经 accepts_response 校验后回灌。
2. 检查 `drain` 无 external 特判（有则解释 + trace 测试）。
3. 更新 `docs/managed-external-agent.md` 中 M2/M3 状态或命名差异。

## 验证条件
- `cargo test -p agent-lib external_agent` / `cargo test -p agent-lib drive`
- 完整验证序列 1-6 全过。
- 完成记录中给出 M3 能力 parity 摘要。

## 发现记录（review findings）
- machine.rs 四点核对全部满足：
  1. resume 各相位 id+family 校验齐全（session/interaction/tool-batch-route+dup+kind/subagent）。
  2. NeedSubagent 只 reify（pause_for_subagent / spawn_agent 分支只 emit Requirement），child 由
     driver DrivingSubagentHandler 驱动。
  3. spawn_agent 经 SpawnAgentRequest::matches 桥成 NeedSubagent（畸形→runtime-visible error），
     绝不发 NeedTool；machine 本身无 ToolRegistry。
  4. resume_interaction 先 interaction.accepts_response 校验再 RespondInteraction，失败进 error cursor，
     不转发非法答案。
- drive.rs drain/fulfill_batch/resolve_requirement 全泛型按 RequirementKindTag 路由，无 External 特判，
  external emit 的 NeedTool/NeedSubagent/NeedInteraction 与 DefaultAgentMachine 同形，无需新增 trace 测试。
- 文档同步 docs/managed-external-agent.md：§1 现状表 2 行、§3 parity 表 4 行、§21 编号说明 +
  M2 命名差异（无 AwaitingToolApproval / ToolApprovalPolicy / ToolFailurePolicy）+ M3 落地状态标注。
- 焦点测试：external_agent 132 passed、drive 27 passed；fmt/clippy(-D warnings)/doc(-D warnings)/
  git diff --check 全过；full suite 复用 M3-3 绿快照（本任务仅 .md 改动，无 .rs 变更）。

## 状态：已完成（M3-4 [DONE]）
- 纯 review sign-off + 文档同步，无 .rs 改动。
- TODO.md 已标 [DONE] 并补完成记录（含 M3 parity 摘要表）。下一任务 = M4-1，本轮不启动。
