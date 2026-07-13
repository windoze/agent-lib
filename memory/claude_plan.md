# 执行计划

## 约束说明

- 本文件记录可公开检查的执行计划、关键决策和进度更新。
- 我不会写入私有逐步思考内容；会用明确的计划摘要和决策依据替代。
- 本轮只完成 `TODO.md` 中第一个标题未带 `[DONE]` 的任务，然后停止。

## 初始计划

1. 读取 `TODO.md`，按文件顺序识别第一个未完成任务，并确认其依赖、验收条件和 completion record 要求。
2. 查看最近一次提交信息；仅当它明确提到与当前任务直接相关的未完成问题时，将其纳入当前任务或作为前置任务写入 `TODO.md`。
3. 根据当前任务读取必要的设计、计划、源码和测试上下文；避免无边界历史问题扫查。
4. 实现当前任务要求；若发现阻塞当前任务的规格缺口或测试失败，优先修复，或在 `TODO.md` 中插入最小必要前置任务后停止。
5. 运行验证：先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
6. 验证通过后，将当前任务标题加上 `[DONE]`，更新 `TODO.md` completion record；仅当阶段计划发生真实变化时才更新 `PLAN.md`。
7. 提交本轮所有相关变更，提交信息包含任务编号和清晰描述。
8. 停止，不继续下一个任务。

## 当前状态

- 状态：已读取 `TODO.md` 并确认首个未完成任务为 `M4-4 [TODO] Compaction strategy/trigger 扩展点与数据/行为分离`。
- 最近提交：`fe6065a [M4-3] Implement atomic compaction apply`，未点名与 M4-4 直接相关的未完成阻塞。
- 已读取上下文：`PLAN.md`、`docs/conversation-core.md` projection/compaction 章节、`projection::{mod,artifact,compaction}`、`Conversation` 与错误枚举。

## M4-4 实施计划

1. 新增 `conversation::projection::strategy` 模块：
   - `CompactionStrategy`：`#[async_trait]` dyn-safe trait，按 `StrategyRef` 解析运行时实例。
   - `CompactionStrategyResolver`：只读解析接口，缺失或返回错误引用时产生分类错误，不 fallback。
   - `CompactionInput`/`CompactCtx`：只读 source spans、effective context 与目标 artifact/strategy 数据。
   - `ArtifactDraft`：策略返回的未校验 artifact draft，最终由 ctx 组装成带 provenance 的 `Artifact`。
   - `CompactionTrigger`、`CompactionTriggerOutcome`、`DeferredUntilBoundary`：同步 trigger 只观察 `&Conversation` 和 `Usage`，返回 data-only plan 或 deferred。
2. 扩展错误类型：
   - 新增 `CompactionError`，覆盖 unresolved strategy、resolver 返回错误 strategy ref、strategy failed。
   - 接入 `ConversationError` 并从 crate/conversation 导出。
3. 扩展现有 compaction 数据 API：
   - 为 `CompactionPlan` 增加保留 header/steps、替换 artifacts 的 helper，便于 trigger 先返回 plan intent，strategy 后填入 artifacts。
4. 给 `Conversation` 增加 trigger evaluation 方法：
   - pending 时直接返回 `DeferredUntilBoundary`，不调用 trigger。
   - boundary 状态下以 immutable `&Conversation` 调用 trigger，trigger 不能直接修改 projection。
5. 添加聚焦测试：
   - mock async strategy 通过 trait object/resolver 生成 artifact，plan serde round-trip 后可按相同 `StrategyRef` 填入 artifacts 并 apply。
   - 两个 trigger 分别产生 tiered raw plan 与 consolidated span plan，证明不同 `StrategyRef` 可用。
   - pending 状态返回 deferred 且不调用 trigger。
   - 无 registry、缺失 strategy、错误 strategy reference 明确失败。
   - serde 输出只包含 data plan/artifact/provenance，不包含 mock runtime/client handle。
6. 更新 README、crate/conversation rustdoc 与 `TODO.md` completion record；除非阶段计划变化，否则不改 `PLAN.md`。
7. 验证顺序：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → M4-4 聚焦测试 → `cargo test --all --all-targets`（1800 秒内）→ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` → `git diff --check`。

## 进度更新

- 已完成代码实现：新增 `projection::strategy` 模块、`CompactionError`、trigger evaluation 方法、`CompactionPlan::with_artifacts` 和顶层导出。
- 已完成聚焦测试：`cargo test conversation::projection::tests::strategy -- --nocapture` 通过（5 passed）。
- 已完成文档与台账：README、crate/conversation rustdoc 和 `TODO.md` 已更新，M4-4 标记为 `[DONE]`。
- 已完成验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、M4-4 聚焦测试、完整 projection 聚焦测试、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check` 均通过。
- 提交：已创建 `[M4-4] Add compaction strategy trigger extension points`，本轮任务完成后停止。
