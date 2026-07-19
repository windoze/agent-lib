# 执行计划：M4-2 `AwaitingReconfig` 期间的新 reconfigure 不再静默丢失（H-STATE-5）

## 状态：✅ 已完成（待提交）

## 执行结果

1. 探索：完成（explore subagent）。定位 `reconfigure`（mod.rs:314）、`AwaitingReconfig` cursor、
   resume 路径（`apply_reconfig_application` 无条件 clear）、`AgentStateError`、测试模式。
2. 选型：方案 (a)——park 期间拒绝（resume 重 plan 会让已确认的 registry swap 与实际应用不符）。
3. 实现：完成。
   - `src/agent/state.rs`：新增 `AgentStateError::ReconfigWhileAwaitingRegistry` +
     单一来源准入规则 `ensure_reconfig_admission()`；`queue_reconfig`（pub 入口，class-wide）
     同步接守卫。
   - `src/agent/machine/default/mod.rs`：`reconfigure` 在 plan 前调准入守卫；rustdoc `# Errors` 同步。
   - M4-4 衔接：M4-4 未落地，走既有 `AgentError` 通道（`AgentErrorKind::AgentState`），已记录。
4. 测试：`reconfigure_during_awaiting_reconfig_is_rejected_and_can_be_retried`
   （复现报告场景：R2 被拒、队列不动、resume 应用 A1、R2 可重提交）；
   `agent::` 444 条全过。
5. 文档：`docs/agent-layer.md` §4.2 补拒绝+重试口径；`docs/review-2026-07.md` H-STATE-5 标注 ✅。
6. 验证全过：fmt、clippy（默认 + external features）、`cargo test --all --all-targets`（exit 0，33s）、doc。
7. TODO.md：M4-2 标 [DONE] + 完成记录（含 breaking change 记录：AgentStateError 新增变体）。
   下一步：commit 后停止。
