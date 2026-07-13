# 执行计划

## 当前约束

- 输出与进度记录使用中文。
- `TODO.md` 是任务顺序、完成状态、依赖和验证要求的唯一权威来源。
- 本轮只完成第一个标题未带 `[DONE]` 的任务，完成后提交 Git commit 并停止。
- 在执行实现前先记录计划；后续如果计划变化或完成关键步骤，会继续更新本文件。
- 不记录私有逐字思考过程；本文件记录可审计的判断依据、步骤和状态。

## 初始步骤

1. 阅读 `TODO.md`，定位第一个标题未带 `[DONE]` 的任务。
2. 查看最新提交信息，若其中明确提到与当前任务直接相关的未完成问题，则纳入当前任务或作为前置任务写入 `TODO.md`。
3. 只阅读完成当前任务必需的上下文文件，包括相关源码、测试、`PLAN.md` 中必要的阶段说明。
4. 判断当前任务能否作为一个执行单元完成；只有遇到具体且必须先修复的前置问题时，才修改 `TODO.md` 增加最小前置任务并停止。

## 实施步骤

1. 按现有代码结构和项目约定实现当前任务。
2. 为新增或变更行为补充有针对性的测试。
3. 先运行 `cargo fmt --all`。
4. 再运行 `cargo clippy --all-targets -- -D warnings`。
5. clippy 通过后运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
6. 若发现未被明确排期的失败测试，优先修复；若该失败阻塞当前任务且无法在本轮直接修复，则在 `TODO.md` 中插入最小前置任务并停止。
7. 验证通过后，在 `TODO.md` 将当前任务标题加上 `[DONE]`，并填写完成记录。
8. 仅当阶段计划或依赖结构确实变化时更新 `PLAN.md`。
9. 检查工作区变更，提交包含本轮所有相关改动的 Git commit。
10. 提交后停止，不处理下一个任务。

## 当前状态

- 已创建本执行计划。
- 已阅读 `TODO.md` 并确认第一个未完成任务是 `M5-2 [TODO] 受检 restore 与派生索引重建`。
- 已查看最新提交，最近提交为文档更新，没有明确指出与 M5-2 直接相关且未完成的修复项。
- 当前工作区包含 restore 实现、测试、文档和本计划文件修改。
- 已实现 restore 主流程、`RestoreError`、history runtime 重建入口、projection restore-time
  校验，并补充 persistence 聚焦测试。
- 已通过 `cargo fmt --all` 和 `cargo test conversation::persistence -- --nocapture`（12
  passed）。
- 已通过 `cargo clippy --all-targets -- -D warnings`。
- 已通过 `cargo test --all --all-targets`（281 个库测试与 3 个离线集成测试 passed、7
  ignored、0 failed）。
- 已通过 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。
- 已将 `TODO.md` 中 M5-2 标记为 `[DONE]` 并补充完成记录；`PLAN.md` 无阶段级变化，未修改。
- 下一步：提交本轮所有相关改动，然后停止。

## M5-2 具体执行计划

1. 阅读现有 `conversation::persistence`、`history`、`boundary`、`projection`、`validation`
   相关源码和测试，确认 snapshot data shape、history 重建入口和 index 重建方式。
2. 实现 `Conversation::restore(snapshot)` 与 `TryFrom<ConversationSnapshot>`，新增或扩展
   `RestoreError`，错误需要能定位 schema、id、parent、turn、lineage/head、fork、
   projection/artifact 等数据路径。
3. 按顺序校验：schema version、conversation id/config、raw Turn 全局 id 唯一、parent 存在
   且无环、每个 Turn 通过 I1--I4 validator、active lineage/head/ceiling/fork origin 合法、
   projection spans/artifact provenance 合法。
4. 只在所有校验通过后构建 runtime `Conversation`：恢复结构共享 history、logical head、
   structural version、fork origin/ceiling、projection/artifacts，并从事实数据重建
   `ToolCallIndex`；不恢复 pending。
5. 加测试覆盖 snapshot→JSON→snapshot→restore 的正向等价，以及 duplicate id、missing/cyclic
   parent、非法 Turn、head 不在 lineage、错误 fork point、重叠 span、missing artifact、
   错误 covers、未知 schema version 等损坏数据拒绝。
6. 按项目要求运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦
   restore/persistence 测试、`cargo test --all --all-targets`、rustdoc 和 `git diff --check`。
7. 验证通过后把 `TODO.md` 中 M5-2 标题改为 `[DONE]` 并写完成记录，最后提交本轮所有改动。
