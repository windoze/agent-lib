# 执行计划

本文件记录本次调用的可公开推理摘要与执行步骤。不会记录隐藏链路思考；后续如计划变化或关键步骤完成，会继续更新这里。

## 当前目标

按 `TODO.md` 的权威顺序识别并完成第一个标题未带 `[DONE]` 的任务，完成后更新记录、验证、提交，然后停止。

## 步骤

1. 读取 `TODO.md`，只识别第一个未完成任务；同时查看最新提交信息，判断是否有与该任务直接相关的未完事项。
2. 读取该任务涉及的计划、代码、测试和文档上下文，避免做无关历史问题扫描。
3. 如任务可直接完成，按仓库现有设计实现；如存在阻塞当前任务的明确前置问题，则在 `TODO.md` 插入最小前置任务并停止。
4. 依据任务要求补充或调整测试。
5. 先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，最后运行必要测试；若代码有变更且任务要求完整验证，则运行 `cargo test --all --all-targets`。
6. 将当前任务标题标记为 `[DONE]`，更新 completion record；仅在阶段计划真实变化时更新 `PLAN.md`。
7. 检查工作区变更并提交本次任务相关全部变更。

## 状态

- 已创建初始计划。
- 已读取 `TODO.md` 标题索引，首个未完成任务为 `M3-3 Turn-boundary reconfig：skill/tool/system 变更排队`。
- 最新提交为 `[M3-2] Implement pivot queue interject soft turning`，与当前 M3-3 顺序相邻但未发现需先处理的额外 unfinished issue。
- 当前工作区存在本次新增/更新的 `memory/claude_plan.md`，另有未跟踪 `docs/agent-effect-model.md`，后续会先判定其是否与 M3-3 直接相关，避免误覆盖用户改动。

## M3-3 实施计划

1. 扩展 `agent::state::queue`：把现有 `QueuedReconfig` 演进为公开的 `ReconfigRequest` 数据形状，并加入 `ReconfigQueue` wrapper；覆盖 skill activate/deactivate、active skill replace、tool set replace/patch、system overlay、model ref 和 loop policy 更新。
2. 在 `AgentState` 增加已生效的 system overlay、current tool set、current model、current loop policy，以及受检的 `apply_queued_reconfigs` 原子应用入口；预校验通过后一次性 drain queue，失败时不修改队列或已生效状态。
3. 在 runtime tool 边界增加可替换 registry wrapper，使 `DefaultAgentLoop` 可在 turn boundary 把新的 `ToolSetRef` 声明同步到 runtime registry；当前 turn 的工具执行仍使用该 turn 开始时的 snapshot。
4. 修改 `DefaultAgentLoop`：`build_chat_request` 读取 state 中当前 model/policy/system/tool 声明；最终 assistant commit 后、发出 final `StepBoundary` 前应用 queued reconfigs，并把结果写入 boundary metadata。
5. 补充聚焦测试：reconfig 在 pending turn 中延迟到最终 commit 后生效、下一 turn request 改变；tool-use turn 内 registry snapshot 恒定；pivot 与 reconfig 同时排队互不干扰；重复 skill、未知 tool set、system overlay 版本冲突失败保持原子性。
6. 验证顺序：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → 聚焦 reconfig/default loop 测试 → `cargo test --all --all-targets` → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` → `git diff --check`。

## M3-3 当前进展

- 已扩展 reconfig data shape：`ReconfigRequest`、`ReconfigQueue`、`ToolSetPatch`，并保留 `QueuedReconfig` 兼容别名。
- 已在 `AgentState` 增加当前 system overlay、tool set、model 和 loop policy，并实现 queued reconfig 的预览/原子应用。
- 已在 tool runtime 边界增加 `ToolRegistryResolver`、declared-only resolver 和静态 catalog resolver。
- 已让 `DefaultAgentLoop` 支持 `reconfigure` 入队，在 idle turn boundary 和 final assistant commit boundary 应用 reconfig，并在 final `StepBoundary` 写入 metadata。
- 已补充 state/default loop 聚焦测试，并更新 README、crate docs、`docs/agent-layer.md` 与 `TODO.md` 完成记录。
- 验证已通过：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test agent::state --all-targets`、`cargo test agent::loop_driver::default --all-targets`、`perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`、`cargo test --doc`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`。完整测试后只改了 Markdown 和 Rust doc comment，并已重跑 `cargo fmt --all`、rustdoc 和 `git diff --check`。
- 下一步是提交本任务变更并停止，不进入 M3-4。
