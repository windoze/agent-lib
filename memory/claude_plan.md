# 执行计划

说明：本文件记录可公开的执行计划、关键决策和进度更新；不记录私密推理链。

## 当前目标

- 按 `TODO.md` 的顺序识别第一个标题未以 `[DONE]` 标记的任务。
- 完整实现该任务，运行要求的格式化、lint 和测试验证。
- 在 `TODO.md` 中将该任务标题标记为 `[DONE]` 并补充 completion record。
- 提交本次任务涉及的所有变更，然后停止，不继续下一个任务。

## 初始步骤

1. 读取 `TODO.md`，只定位第一个未完成任务，不做开放式历史问题扫查。
2. 查看该任务相关的 `PLAN.md`、源码、测试和最近提交信息，确认任务边界与直接依赖。
3. 若发现阻塞当前任务的具体前置问题，按要求把最小前置任务插入 `TODO.md`、提交并停止。
4. 若无阻塞，按现有代码结构实现任务，优先沿用本仓库模式。
5. 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`，通过后运行相关测试和必要的完整测试。
6. 更新 `TODO.md` completion record；仅当阶段计划发生真实变化时才更新 `PLAN.md`。
7. 检查 git diff，提交清晰的任务 commit。

## 进度

- 已读取 `TODO.md` 并定位首个未完成任务：`M5-4 [TODO] 存盘→恢复→effective_view 端到端一致性`。
- 已检查最近提交：`[M5-3] Implement DB-neutral persistence rows`，未发现直接阻塞 M5-4 的未完成事项。
- 已新增 `src/conversation/persistence/tests/e2e.rs` 并在 persistence tests 中挂载模块。
- 已实现两组 M5-4 端到端验收：
  - snapshot/rows 两条路径恢复复杂 multi-tool + compaction + revert/redo + fork 父子会话，并比较 effective view、raw facts、boundaries、projection/provenance、usage 与 rebuilt index。
  - pending snapshot 拒绝后分别经 cancel discard 与 cancel commit 回到 committed consistency point，再执行 snapshot/rows restore。
- 已运行 `cargo fmt --all` 与 `cargo test conversation::persistence -- --nocapture`，结果通过（18 passed）。
- 已运行严格 clippy、全量测试、rustdoc 和 diff check，均通过。
- 已将 `TODO.md` 的 `M5-4` 标记为 `[DONE]` 并写入完成记录。
- 下一步检查最终 diff 与 git 状态，然后提交本次任务变更。

## M5-4 执行计划

1. 检查 `git status` 与最近提交，确认是否存在直接影响 M5-4 的未完成事项或未提交状态。
2. 阅读 `src/conversation/persistence`、projection/effective view 相关测试与 helper，复用现有 fixture 风格。
3. 新增模块化端到端 persistence integration 聚焦测试，覆盖：
   - 多 Turn、serial/parallel tools、tiered/consolidated compaction、revert/redo、fork 父子分别推进。
   - JSON snapshot 与 DB-neutral rows 两条路径 restore 后，对 system、effective messages、raw facts、head/boundaries、origin、projection/provenance、usage、ToolCallIndex 做一致性断言。
   - pending snapshot 拒绝后，分别经 cancel→commit 或 discard 到达可 snapshot 状态。
   - 全部 fixture 使用显式 id/timestamp，无网络、随机源、时钟或 runtime registry。
4. 运行 `cargo fmt --all`、严格 clippy、聚焦 persistence 测试、全量测试、rustdoc 和 `git diff --check`。
5. 将 `M5-4` 标记为 `[DONE]` 并补充完成记录，提交本次变更后停止。
