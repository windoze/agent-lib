# 执行计划

## 约束理解

- 本轮只处理 `TODO.md` 中第一个标题未带 `[DONE]` 的任务，完成后停止。
- `TODO.md` 是任务排序、验收要求和完成记录的唯一权威来源；`PLAN.md` 只在阶段级计划变化时更新。
- 在选择当前任务前不做开放式问题扫查；只处理会阻塞当前任务、使当前任务行为无效，或由当前任务引入的直接回归。
- 任何观察到且未被明确排期的测试失败都必须修复，或在 `TODO.md` 中加入最小必要的前置任务，且不能把当前任务标记完成。
- 不接受 workaround；如发现规格不匹配且阻塞当前任务，先修复或加入前置任务并提交后停止。
- 输出、进度记录和最终答复使用中文。

## 步骤计划

1. 读取 `TODO.md`，按标题顺序找出第一个未带 `[DONE]` 的任务。
2. 检查最近一次提交信息；若它明确提到与当前任务直接相关的未完成问题，将其纳入当前任务或作为前置项记录到 `TODO.md`。
3. 只读取当前任务所需的相关代码、测试和文档，确认验收要求、依赖和现有实现边界。
4. 如任务可直接完成，按仓库现有结构实施；如发现必须先补的具体前置问题，更新 `TODO.md` 后提交并停止。
5. 在编辑前记录即将修改的范围；使用小而聚焦的补丁更新代码、测试和必要文档。
6. 按要求先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，通过后运行相关测试和必要的完整测试套件；完整测试套件超时不超过 30 分钟。
7. 若测试失败，判断是否与当前任务相关或未被明确排期；修复或在 `TODO.md` 中加入前置任务，不能忽略。
8. 验证通过后，在 `TODO.md` 当前任务标题加 `[DONE]`，并更新完成记录，记录实现摘要和验证命令。
9. 检查工作区变更，提交本轮所有相关未提交文件，提交信息包含任务 id 和清晰说明。
10. 停止，不继续处理下一个任务。

## 当前状态

- 已读取 `TODO.md` 并定位到本轮唯一任务：`M5-1 [TODO] Boundary 一致点 ConversationSnapshot`。
- 该任务要求新增 versioned `ConversationSnapshot` schema 与 `Conversation::snapshot()`，只在无 pending 的 committed boundary 成功，保存恢复有效视图所需 data-only facts，并排除 pending、Accumulator、ToolCallIndex、runtime strategy/trigger/client 等运行时资源。
- 已检查最新提交：`4ac511f [M4-R] Review projection compaction boundaries`，没有直接相关的未完成事项。
- 工作区存在未跟踪 `docs/agent-layer.md`，当前判断与 M5-1 无关，本轮不改动也不纳入提交。
- 设计决策：新增 `conversation::persistence` 模块，snapshot 公开 versioned data shape 与只读 getter，内部使用 validator-facing `TurnData` 复制 live closed Turn facts；记录 raw turn facts 一次、当前 lineage turn ids、head turn count、fork ceiling turn count、structural version、origin、projection/artifacts。

## M5-1 细化计划

1. [DONE] 检查最近提交信息，确认是否有直接关联 M5-1 的未完成事项。
2. [DONE] 阅读当前 conversation/history/projection/boundary 模块和相关测试，找出 snapshot 应使用的内部事实来源与公开/私有边界。
3. [DONE] 设计并实现 `ConversationSnapshot` 数据结构：显式 schema version，包含 id/config、raw closed turns、active lineage/head、structural version、fork origin/ceiling、projection 与 artifacts/provenance。
4. [DONE] 实现 `Conversation::snapshot()` 与分类错误：有 pending 时拒绝且不改变原状态；snapshot 不序列化 pending、Accumulator、ToolCallIndex、Arc/lock、client/registry/strategy/trigger object，只保存 data-only `StrategyRef` 等事实。
5. [DONE] 添加聚焦测试：线性历史、revert 后 detached suffix、fork origin、projection/artifacts serde round-trip、共享事实不重复，以及 text/tool/open/ready pending 下 snapshot 拒绝与 runtime-only 字段 JSON 缺席。
6. [DONE] 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、snapshot 聚焦测试、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。
7. [DONE] 验证通过后更新 `TODO.md`：将 `M5-1` 标题标为 `[DONE]` 并补充完成记录。
8. [DONE] 检查工作区并提交本轮相关变更后停止。
