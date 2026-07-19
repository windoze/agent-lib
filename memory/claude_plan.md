# 执行计划

## 当前状态

- 已确定本轮首个未完成任务：`M6-3 [TODO] M6 review：预算接线收口`。

## 约束

- 以 `TODO.md` 为任务顺序和完成状态的唯一权威来源。
- 只完成首个标题未带 `[DONE]` 的任务，完成后停止。
- 完成任务后更新 `TODO.md`、运行必要验证、提交 Git commit。
- 如遇阻塞问题，添加最小必要前置任务并提交后停止。

## 步骤

1. 检查最新提交是否明确提到与 `M6-3` 直接相关的未完成问题。
2. 核对 `docs/review-2026-07.md` 中 `M-PROM-1`、`L-8`、`L-9` 的最终标注。
3. 核对 `BudgetExhausted` / `BudgetExceeded` 相关变体已有生产构造路径，不是死代码。
4. 按 M6 review 要求运行全量门禁：`cargo fmt --all`、两组 clippy、`cargo test --all --all-targets`、`cargo doc`。
5. 更新 `TODO.md`：将 `M6-3` 标题改为 `[DONE]` 并追加完成记录。
6. 仅在发现阶段级计划变化时更新 `PLAN.md`；当前预期不需要修改。
7. 检查 Git diff 和状态，提交本轮变更，然后停止。

## 进度日志

- 已读取 `TODO.md` 并定位首个未完成任务为 `M6-3`。
- 最新提交 `[M6-2] Expose facade budget controls` 与本 review 直接相关，但提交信息未声明未完成项。
- 已核对 `docs/review-2026-07.md`：`M-PROM-1` 已标注 M6-1/M6-2 修复，`L-9` 已标注 M6-2 修复；`L-8` 的预算预检/charge 非原子窗口已作为可接受且已文档化的现状记录。
- 已核对生产路径：`drain`/`drive_streamed` 预算预检与 LLM response 后 `charge_step`/`charge_usage` 已接线，默认机器与 external 机器会构造 `BudgetExhausted`，默认机器预算恢复路径会构造 `BudgetExceeded`，facade 会映射 `BudgetExhausted`，dispatch 零预算硬出口已存在。
- M6 review 要求的全量门禁已通过：`cargo fmt --all`、两组 clippy、`cargo test --all --all-targets`、`cargo doc`。
- 已将 `TODO.md` 中 `M6-3` 标题改为 `[DONE]` 并追加完成记录。
- 下一步检查 diff/status 并提交本轮变更。
