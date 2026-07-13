# 本轮执行计划

## 约束确认
- 以 `TODO.md` 为唯一任务顺序和完成状态来源。
- 只处理第一个标题未带 `[DONE]` 的任务，完成后提交并停止。
- 不做开放式历史问题扫描；只处理会阻塞当前任务或由当前任务引入的缺陷。
- 若发现未被明确排期的测试失败，必须修复或在 `TODO.md` 中加入最小前置任务，不能把当前任务标记完成。
- 常规任务进度只更新 `TODO.md`，只有阶段级计划变化才更新 `PLAN.md`。

## 步骤计划
1. 读取 `TODO.md`，按文档顺序识别第一个标题未带 `[DONE]` 的任务，并记录任务编号、要求、依赖和验证标准。
2. 检查最近一次提交信息，只有当它明确提到与当前任务直接相关的未完成事项时，才纳入当前任务或作为前置任务写入 `TODO.md`。
3. 针对当前任务读取必要代码、测试和文档，限定范围到完成该任务所需内容。
4. 实现任务要求；如遇到阻塞当前任务的规格不匹配或缺失前置能力，先更新 `TODO.md` 加入最小前置任务并停止。
5. 添加或更新聚焦测试，覆盖当前任务要求和关键边界。
6. 按要求运行验证：先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`（完整测试超时不超过 30 分钟）。
7. 若验证通过，在 `TODO.md` 将当前任务标题前缀改为 `[DONE]`，更新完成记录、实现摘要和验证命令。
8. 查看工作区差异，确认只包含当前任务相关改动；提交所有当前未提交改动，提交信息包含任务编号和简洁说明。
9. 停止，不继续处理下一个任务。

## 进度日志
- 已创建本计划文件，下一步读取 `TODO.md` 定位第一个未完成任务。
- 已识别首个未完成任务：`M5-R [TODO] Milestone 5 Review`。
- 最近提交 `1d4d76e [M5-4] Add persistence effective view e2e acceptance` 未明确声明与 M5-R 直接相关的未完成阻塞项。
- 本轮将聚焦审查 persistence snapshot/rows/restore/effective_view 信任边界、运行 corruption 与端到端恢复测试，并在通过后只标记 M5-R 完成。
- 已完成 M5-R 代码边界初审：`Conversation::snapshot` 拒绝 pending，`Conversation::restore` 先 schema gate 再校验 raw Turn/I1--I4、parent graph、lineage/head/fork origin、projection，并重建派生 index。
- 已完成 rows 边界初审：`ConversationRows::into_snapshot` 只做 row-level PK/FK/sequence/projection data-shape 检查并返回 data snapshot，不构造 live Conversation；insert-only diff 对同 PK 不同 immutable fact 返回 conflict。
- 验证进度：`cargo fmt --all` 通过；`cargo clippy --all-targets -- -D warnings` 通过，无 warning。
- 聚焦验证通过：`cargo test conversation::persistence -- --nocapture`，18 passed，覆盖 corruption、pending snapshot gate、rows、fork/compaction/effective_view e2e。
- 全量验证通过：`perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`，287 个库测试与 3 个离线集成测试 passed，7 ignored，0 failed。
- 文档与 diff 验证通过：`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过；`git diff --check` 通过。
- 下一步更新 `TODO.md`，把 M5-R 标记为 `[DONE]` 并写入 review 完成记录。
- 已更新 `TODO.md`：M5-R 标题改为 `[DONE]`，完成记录包含审查结论和验证结果。更新后 `git diff --check` 仍通过。
- 下一步检查 diff/status，然后提交本轮所有未提交改动并停止。
