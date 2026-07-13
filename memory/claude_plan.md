# 当前执行计划

## 约束

- 使用中文记录和汇报。
- 以 `TODO.md` 为唯一任务顺序和完成状态来源。
- 只完成第一个标题未带 `[DONE]` 的任务，完成后停止。
- 如果发现阻塞当前任务的真实前置问题，先在 `TODO.md` 中插入最小必要前置任务并提交，然后停止。
- 不能通过缩小范围、改变表示或临时 workaround 绕过规格不匹配。
- 代码变更完成后按要求运行 `cargo fmt`、`cargo clippy --all-targets -- -D warnings`，再运行必要测试；完整测试超时不超过 30 分钟。
- 完成任务时必须在 `TODO.md` 标题前加 `[DONE]` 并更新完成记录，然后提交 Git。

## 步骤计划

1. 读取 `TODO.md`，按标题 `[DONE]` 前缀识别第一个未完成任务。
2. 检查最新提交信息，若其中明确提到与当前任务直接相关的未完成问题，将其纳入当前任务或作为前置任务写入 `TODO.md`。
3. 只阅读当前任务所需的设计、计划、源码和测试上下文，避免开放式历史问题排查。
4. 根据当前任务要求实施代码、测试或文档变更；如遇阻塞规格问题，更新 `TODO.md` 后提交并停止。
5. 运行格式化、lint 和相关测试；若出现未被显式排期的失败测试，修复或把最小修复任务排到当前任务前。
6. 更新 `TODO.md`：给当前任务标题添加 `[DONE]`，补充完成记录、验证命令和结果。仅当阶段级计划变化时更新 `PLAN.md`。
7. 复查 Git diff，确认没有误改或遗漏。
8. 提交本次任务相关的所有变更，提交信息包含任务编号和清晰说明。
9. 停止，不继续处理下一个任务。

## 进度

- 已创建本计划文件。
- 已读取 `TODO.md`，首个未完成任务确定为 `M1-2 RunContext、取消、预算与 trace handle 边界`。
- 已检查最新提交 `e6f0bd8 [M1-1] Add agent static spec model`，未发现直接指向 `M1-2` 的未完成事项。

## 当前任务执行计划

1. 阅读 `docs/agent-layer.md` 中 RunContext/cancel/budget/trace 相关章节、`DESIGN.md` Agent 约束，以及现有 `src/agent` 模块结构。
2. 设计并实现 `agent::context`：字段私有 `RunContext`、取消传播、预算检查/扣减、trace 节点记录、可 serde 的 budget/trace DTO，以及不可 serde 的 live handle。
3. 从 `src/agent/mod.rs` 导出必要 public API，并保持 runtime handle 不进入 serde data shape。
4. 添加聚焦测试，覆盖取消传播、预算 step/token/cost/wall-clock 扣减和超限分类、trace parent 链、子 context 继承、`RunContext` 不可 serde、record DTO 可 serde。
5. 运行格式化、严格 clippy、聚焦 context 测试、全量测试、rustdoc 和 diff check。
6. 根据验证结果修复问题，随后更新 `TODO.md` 完成记录并提交。

## 当前进度更新

- 已新增 `src/agent/context.rs`，包含 `RunContext`、`CancellationToken`、`BudgetHandle`、`TraceHandle`、预算/trace record DTO 与分类错误。
- 已在 `src/agent/mod.rs` 导出 context API。
- 已更新 `src/lib.rs` 和 `README.md` 的 Agent 当前能力描述。
- 已完成格式化、严格 clippy、聚焦测试、全量测试、doctest、rustdoc 和 diff check，均通过。
- 已将 `TODO.md` 中 `M1-2` 标记为 `[DONE]` 并补充完成记录。
- 复查发现初版 `context.rs` 过长，已拆分为 `context.rs` 聚合模块和 `context/cancel.rs`、
  `context/budget.rs`、`context/trace.rs`、`context/tests.rs`。
- 拆分后已重新通过格式化、clippy、聚焦测试、全量测试、doctest 和 rustdoc。
- 下一步运行最终 `git diff --check`，然后提交。
