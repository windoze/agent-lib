# 本轮执行计划

## 目标

按 `TODO.md` 的顺序完成第一个标题未标记 `[DONE]` 的任务，完成后更新任务记录、运行要求的验证、提交 Git commit，然后停止。

## 执行原则

- `TODO.md` 是任务排序和验收要求的唯一权威来源；`PLAN.md` 仅在阶段级计划变化时更新。
- 任务只有标题带 `[DONE]` 才视为完成；已有完成记录但标题未标记的任务仍按未完成处理。
- 不做开放式历史问题扫描；只处理当前任务直接要求的问题、阻塞当前任务的问题，以及验证中新观察到且未被明确排期的失败。
- 不通过缩小范围或绕过规格来完成任务；若发现必要前置条件缺失，则在 `TODO.md` 中插入最小必要前置任务并提交后停止。
- 任何计划变化或关键步骤完成时，更新本文件。

## 步骤

1. 读取 `TODO.md`，定位第一个未完成任务，并记录任务编号、范围、依赖与验证要求。
2. 查看最新提交信息，判断是否明确提到与当前任务直接相关的未完成事项；若有，将其纳入当前任务或作为前置任务写入 `TODO.md`。
3. 只读取当前任务需要的相关设计、源码和测试，确认现有实现边界。
4. 按任务要求实现代码或文档变更；编辑前先说明将修改的文件和目的。
5. 添加或调整聚焦测试，覆盖任务指定行为和发现的相关边界。
6. 按要求运行验证：先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
7. 若验证发现未排期失败，立即修复；若无法在当前任务内正确修复，则在 `TODO.md` 插入最小必要前置任务，保持当前任务未完成，提交后停止。
8. 任务完成后，在 `TODO.md` 给任务标题加 `[DONE]` 并更新完成记录；仅在阶段计划实际变化时更新 `PLAN.md`。
9. 检查工作区变更，提交本轮所有相关未提交文件，提交信息包含任务编号和清晰描述。
10. 停止，不继续下一个任务。

## 当前状态

- 状态：已读取 `TODO.md`，首个未完成任务为 `M3-R [TODO] Milestone 3 Review`。
- 最新提交：`8002649 [M3-4] Implement O(1) conversation fork`，与当前 Review 直接相关；本轮将把 M3-4 的 fork 实现作为审查对象之一，不新增前置任务，除非发现会阻塞 Review 验收的具体缺陷。

## 当前任务计划：M3-R Milestone 3 Review

1. 读取 `docs/conversation-core.md` 中 §7--§9，以及 M3 相关 `PLAN.md` 摘要，确认 Review 的规范检查点。（已完成）
2. 审查 `conversation` 中 history、boundary、revert/head、fork 与 `ToolCallIndex` 的公开 API 和 crate-private 边界，确认没有 raw mutation、unchecked boundary 消费或 index 事实化。（已完成，未发现需要前置修复的阻塞问题）
3. 检查已有 M3 测试覆盖，定位 branch/revert/fork 组合矩阵是否已有缺口。（已完成：已有单点覆盖充分，但缺少跨 branch/revert/fork/pending 的一体化 Review 矩阵）
4. 若覆盖不足，新增聚焦 Review 回归测试，组合验证 parent tree、raw retention、active view、fork ceiling、stale/ABA、pending boundary 禁止和 index 隔离。（已完成：新增 `src/conversation/boundary/tests/review.rs` 并挂载模块）
5. 必要时补充 rustdoc/README 中 M3 Review 发现的边界说明；若阶段计划无变化，不更新 `PLAN.md`。（当前未发现需要修改阶段计划或公共文档的规格缺口）
6. 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、M3 聚焦测试、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与 `git diff --check`。（已完成：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test conversation::boundary::tests::review -- --nocapture`、`cargo test conversation::boundary -- --nocapture`、1800 秒上限内 `cargo test --all --all-targets`、`cargo test --doc`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`）
7. 更新 `TODO.md`：将 `M3-R` 标题改为 `[DONE]` 并写入完成记录。（已完成；之后只有 Markdown 记录变化，不需要重跑 Rust 测试）
8. 检查工作区并提交本轮变更，提交信息使用 `[M3-R] ...`。（已完成：创建 `[M3-R] Review boundary branching invariants` 提交；提交后工作区干净）
