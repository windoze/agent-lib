# 执行计划

## 约束

- 输出、记录与最终说明使用中文。
- `TODO.md` 是本轮任务顺序与验收要求的唯一权威来源。
- 本轮只完成 `TODO.md` 中第一个标题未带 `[DONE]` 的任务，完成后提交并停止。
- 在没有具体阻塞之前，不拆分任务、不跳到后续任务、不做开放式历史问题扫描。
- 若发现当前任务被具体前置缺陷阻塞，则在 `TODO.md` 中插入最小必要前置任务并提交后停止。
- 若运行测试发现未被后续任务明确排期的失败，必须修复或在 `TODO.md` 中排入当前任务完成前的前置任务。
- 编辑代码前先说明将改动的范围；使用小而聚焦的补丁。
- 验证顺序为 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`，完整测试超时不超过 30 分钟；若本轮只改文档且已有可复用的绿色完整测试结果，则记录跳过原因。

## 步骤

1. 读取 `TODO.md`，按标题 `[DONE]` 前缀识别第一个未完成任务，并记录任务编号、范围、依赖和验收要求。
2. 查看最新提交信息；只在它明确提到与当前任务直接相关的未完成问题时，把该问题纳入当前任务或作为前置任务写入 `TODO.md`。
3. 阅读当前任务涉及的设计文档、源码和测试，确认现有实现边界。
4. 如果任务可直接实现，按现有代码风格做最小完整实现，并补充或调整覆盖当前行为的测试。
5. 如果遇到无法按规格完成的具体阻塞，更新 `TODO.md` 以插入最小必要前置任务，保留当前任务未完成，提交后停止。
6. 运行规定验证；如有失败，按测试失败策略处理，直到没有未排期失败。
7. 在 `TODO.md` 中给当前任务标题加 `[DONE]`，补充完成记录、验证命令和结果；仅当阶段计划实际改变时更新 `PLAN.md`。
8. 检查工作区差异，确保未误改无关用户内容。
9. 用清晰提交信息提交本轮所有相关改动。
10. 最终回复说明完成的任务、关键改动、验证结果和提交哈希，然后停止。

## 进度

- 已创建本轮执行计划。
- 已读取 `TODO.md`，本轮第一个未完成任务为 `M4-2 [TODO] effective_view、head clipping 与 pending 隔离`。
- 最新提交为 `[M4-1] Implement projection model`，未明确提到与 `M4-2` 直接相关的未完成阻塞。
- 已阅读 `PLAN.md`、`docs/conversation-core.md` §6/§6.1、`Conversation`、`Projection`、`Artifact`、`PendingTurn` 与 head/fork 实现。
- 实现方案：
  1. 在 `conversation::projection` 中新增 `EffectiveView`，持有单列 system prompt 与按 projection/head 渲染出的 Client `Message` 列表。
  2. 新增 `PendingContext`，只包含 pending 中已经冻结的完整 Client `Message` payload，不暴露 active `PendingMessage` 或 partial。
  3. 实现 `Conversation::effective_view()`：遍历当前 projection spans；raw span 渲染 head 以内 raw Turn messages；完整位于 head 前的 compacted span 渲染 artifact；若 head 落在 compacted cover 内，则对可见前缀回退为 raw turns，避免摘要泄漏未来 Turn。
  4. 实现 `Conversation::pending_context()`：无 pending 返回 `None`，有 pending 返回已冻结 payload clone；active partial 不进入结果。
  5. 补充 projection 聚焦测试覆盖 raw、compacted 混排、head clipping、revert/redo、zero head、fork ceiling 与 pending 隔离。
  6. 按要求运行 `cargo fmt --all`、严格 clippy、聚焦测试、全量测试、rustdoc 和 diff check。
- 已在 `projection` 模块新增 `EffectiveView`、`PendingContext`、`Conversation::effective_view()` 与 `Conversation::pending_context()`，并补充 raw/compacted/head clipping/fork/pending 聚焦测试。
- 已修正聚焦测试中复用 stale boundary 的测试错误，并同步 README、crate rustdoc 与 conversation 模块 rustdoc。
- 文档同步后验证已通过：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test conversation::projection -- --nocapture`（14 passed）以及 1800 秒上限内 `cargo test --all --all-targets`（258 个库测试 + 3 个离线集成测试 passed，7 ignored）。
- 后续验证已通过：`cargo test --doc`（1 个正向与 10 个 compile-fail passed）、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与 `git diff --check`。
- 已更新 `TODO.md`：M4-2 标题改为 `[DONE]`，补充实现摘要、测试覆盖和完整验证记录；`PLAN.md` 未变化。
- 最终状态：M4-2 已实现并完成验证；下一步只剩检查差异并提交。
