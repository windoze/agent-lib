# 当前执行计划

## 约束确认

- 输出使用中文。
- `TODO.md` 是唯一任务顺序与完成状态依据。
- 本轮只完成 `TODO.md` 中第一个标题未带 `[DONE]` 的任务，然后停止。
- 在读取和执行任务前，先在本文件记录可公开的计划、决策依据和进度。
- 不进行开放式历史问题扫描；只处理当前任务需要的上下文、阻塞项和测试失败策略要求的问题。
- 若发现当前任务被具体前置问题阻塞，则在 `TODO.md` 中加入最小必要前置任务，提交后停止。
- 完成任务后需要更新 `TODO.md` 的标题 `[DONE]` 前缀和完成记录，并提交所有相关改动。

## 初始执行步骤

1. 读取 `TODO.md`，按文件顺序找出第一个标题未显式带 `[DONE]` 的任务。
2. 读取该任务相关的 `PLAN.md` 片段和必要代码/测试上下文，避免扩大范围。
3. 检查最新提交信息是否明确提到与该任务直接相关的未完成问题；若相关，将其纳入当前任务或作为前置任务写入 `TODO.md`。
4. 根据任务要求实现代码或文档改动；若需要拆分，只在存在不可一起落地的明确前置条件时进行。
5. 在关键实现步骤完成后更新本文件的进度记录。
6. 按要求先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，最后在有代码影响时运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
7. 对发现的未计划测试失败：要么修复，要么在 `TODO.md` 中安排最小必要前置/后续任务，且不得把当前任务标记为完成。
8. 任务完成后更新 `TODO.md`：标题加 `[DONE]`，补全完成记录和验证结果；仅在阶段级计划变化时更新 `PLAN.md`。
9. 查看 git 状态，确保不回滚用户已有改动；把本轮相关改动和任何需要一起提交的未提交恢复工作纳入一次清晰 commit。
10. 提交后停止，不继续下一个任务。

## 当前进度

- 已创建本执行计划文件。
- 已读取 `TODO.md` 并识别第一个未完成任务：`M2-3 Tool use 执行编排与 result 回灌`。
- 已查看最新提交摘要：`[M2-2] Add default text-only LLM loop driver`，未发现需要在执行
  M2-3 前单独处理的、提交信息明确指出的相关未完成 issue。
- 已读取 M2-3 相关 Agent loop、事件、Conversation pending tool API、cursor、tool model 与
  trace/budget API。
- 设计选择：
  - 新增最小 runtime tool 模块，包含 `ToolRegistry`、`ToolExecutor`、tool runtime error 和
    外部注入的 `ToolExecutionIds`。
  - `DefaultAgentLoop::new` 保持现有 text-only 行为；新增带 tool registry/id source 的构造入口。
  - 第一次 assistant 的 ids 继续来自 `AgentInput`；工具映射、tool-result message id、后续
    assistant message id 和 step id 由 `ToolExecutionIds` 提供，库不生成随机/时钟 id。
  - 默认 loop 遇到 tool-use 后调用 Conversation 的 `register_tool_calls` 与
    `append_tool_response`，不复制 pairing 校验。
  - tool executor 的正常返回必须是完整 `ToolResponse`；执行错误在
    `ReturnErrorToModel` 策略下转换为 `ToolStatus::Error` 结果，在 `StopRun` 策略下中止并丢弃
    pending。
  - 先支持串行与按 policy 并行执行；流式路径在 LLM event 透传后进入同一工具循环。
- 下一步：增量添加 `agent::tool` 模块、导出类型，然后重构 default loop。
- 已新增 `agent::tool` runtime 边界并导出。
- 已重构 `DefaultAgentLoop`，保留既有 text-only 构造入口，新增
  `DefaultAgentLoop::with_tool_registry`，并把非流式/流式路径接入工具循环。
- 已运行 `cargo fmt --all` 和 `cargo test agent::loop_driver --all-targets`，现有 loop_driver
  测试通过。
- 已补充工具编排聚焦测试，覆盖单 tool、并行 tool、tool error/denied、Conversation
  拒绝重复/未知 call id、tool 执行失败后模型继续自愈，以及 committed Turn 的 pairing。
- 已更新 crate 根文档与 `README.md` 中关于 `DefaultAgentLoop` 和 tool runtime 边界的当前能力说明。
- 已运行 `cargo fmt --all` 与 `cargo clippy --all-targets -- -D warnings`，均通过。
- 已运行 M2-3 聚焦测试、完整测试、rustdoc、doctest 和 `git diff --check`，均通过。
- 已更新 `TODO.md`，将 M2-3 标记为 `[DONE]` 并补全完成记录。
- 下一步：检查 git 状态和 diff，确认改动范围后提交。
