# 执行计划

## 说明

用户要求在执行任何代码或命令前先写入计划文件。当前计划是在尚未读取仓库文件的前提下形成的公开执行计划；读取 `TODO.md` 后会立即更新本文件，补充实际选中的第一个未完成任务、验证要求和执行步骤。

我不会在此文件中写入不可公开的内部推理链；本文件记录可审计的判断依据、步骤、进度和结果。

## 初始步骤

1. 读取 `TODO.md`，按标题是否带 `[DONE]` 判断第一个未完成任务。
2. 只围绕第一个未完成任务工作，不做开放式历史问题扫描。
3. 查看该任务相关的设计、代码和测试上下文；必要时检查最近提交是否明确提到与该任务直接相关的未完成事项。
4. 若任务可直接完成，按现有项目风格实现，并补充或调整针对性测试。
5. 先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
6. 若发现未被安排且会导致测试失败或阻塞当前任务的真实问题，先修复；若无法在当前任务内正确修复，则在 `TODO.md` 中插入最小必要前置任务并停止。
7. 完成任务后，在 `TODO.md` 中给任务标题加 `[DONE]`，更新 completion record。
8. 仅当阶段级计划或依赖变化时更新 `PLAN.md`。
9. 提交本轮所有相关变更到 Git，然后停止，不继续下一个任务。

## 当前状态

- 已读取 `TODO.md` 并定位首个未完成任务：
  `M4-1 [TODO] Projection、Span、Artifact 与受检覆盖范围`。
- 前置依赖 `M3-R` 已在 `TODO.md` 中标记 `[DONE]`。

## M4-1 执行计划

1. 检查 `git status` 与最近提交，确认是否有与 M4-1 直接相关的未完成事项或脏工作区。
2. 阅读 `PLAN.md`、`docs/conversation-core.md` 中 Projection/Compaction 相关规范，以及现有
   `src/conversation` 模块结构。
3. 设计并实现 `conversation/projection/`：
   - `Projection`
   - `Span::Raw`
   - `Span::Compacted`
   - `Artifact`
   - `StrategyRef`
   - token accounting / provenance
   - `CheckedTurnRange`
4. 将 Projection 挂接到 `Conversation` 的持久状态与公开 API，保持 raw history 不变；所有
   构造入口必须受检，serde DTO 不能绕过校验。
5. 实现范围解析与重验证：
   - start/end Boundary 必须同 owner、当前有效、无 pending、按顺序、非空。
   - 内部保存稳定 Turn anchor/id，后续使用时按当前 lineage 重新校验，不能依赖旧
     structural version token 自证合法。
6. 添加聚焦测试：
   - raw、单层 compacted、多层/tiered artifact、provenance 和 serde round-trip。
   - version 改变后按 Turn anchors 重验证。
   - 跨 owner、越 head、pending、反向/重叠 span、未知 Turn/artifact、detached branch 等负例。
7. 更新 `TODO.md` 完成记录；仅在阶段级计划变化时更新 `PLAN.md`。
8. 按要求运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、projection 聚焦测试、
   `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
   `git diff --check`。
9. 提交本轮所有相关变更，然后停止。

## 进度更新

- 已确认最近提交为 `[M3-R] Review boundary branching invariants`，没有发现与 M4-1 直接相关的未完成提交说明。
- 当前脏工作区仅包含本计划文件。
- 已读取 Projection 相关规范与现有 `Conversation`/`Boundary`/`History`/`Turn`/错误模块。
- 实现边界调整为：本轮只新增 projection 数据模型、受检 `CheckedTurnRange`、受检 span/projection/artifact 构造和 `Conversation` 只读挂接；不提前实现 M4-2 的 `effective_view` 或 M4-3 的 `apply_compaction`。
- 已新增 `conversation::projection` 模块，包含 `Projection`、`Span`、`CheckedTurnRange`、
  `Artifact`、`ArtifactProvenance`、`StrategyRef` 与 `TokenAccounting`。
- 已将 `Projection` 挂到 `Conversation`；新建/commit/fork 默认维护 all-raw overlay，失败路径不应改动 projection。
- 已新增 `ProjectionError` 并接入 `ConversationError`。
- 验证进度：
  - `cargo fmt --all`：通过。
  - `cargo clippy --all-targets -- -D warnings`：通过。
  - `cargo test conversation::projection -- --nocapture`：8 passed。
  - `cargo test --all --all-targets`（1800 秒上限）：252 个库测试与 3 个离线集成测试 passed，7 ignored。
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：通过。
  - `git diff --check`：通过。
- 已更新 `TODO.md`，将 M4-1 标记为 `[DONE]` 并补完成记录。
- 已更新 README 与 crate/conversation rustdoc，说明 M4-1 已完成的数据模型边界；未修改 `PLAN.md`。
