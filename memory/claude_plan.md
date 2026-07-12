# 本次执行计划

## 目标与边界

- 严格以 `TODO.md` 为任务顺序、需求、依赖、验证标准和完成记录的唯一事实来源。
- 本次只处理 `TODO.md` 中标题未带 `[DONE]` 的第一个任务；完成并提交后立即停止，不进入下一任务。
- `PLAN.md` 只在阶段级顺序、依赖、假设或完成标准确实变化时修改。
- 本文件记录可审计的计划、判断依据、进度、计划调整和验证结果；不记录私密的逐字推理过程。

## 初始执行步骤

1. 读取 `TODO.md`，按文档顺序定位第一个标题未带 `[DONE]` 的任务，并完整阅读其需求、依赖、测试与完成记录要求。
2. 检查 Git 工作区状态和最新提交，仅判断未提交改动及最新提交是否与当前任务直接相关；不开展无边界的历史问题排查。
3. 阅读当前任务直接涉及的设计文档、代码和测试，确认现状与验收边界；若发现阻塞当前任务的具体前置缺陷，按要求先修复，或在 `TODO.md` 中插入最少的前置任务并停止。
4. 将已识别的任务编号、实现方案、影响文件和具体验证命令补充到本文件，然后进行小步、聚焦的代码修改；每完成关键步骤即更新进度。
5. 增补或调整测试，覆盖任务规定的正常路径、边界情况、错误路径以及受同一根因影响的同类场景。
6. 按规定顺序验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，再运行相关测试与 `cargo test --all --all-targets`，最后按任务要求运行文档构建或其他检查。完整测试设置不超过 30 分钟的超时。
7. 所有要求满足且不存在未安排的失败后，在 `TODO.md` 的任务标题前添加 `[DONE]`，填写可复核的完成记录；只有阶段级计划发生变化时才更新 `PLAN.md`。
8. 复查差异与工作区，确保保留用户已有改动；若这是异常中断后恢复的同一任务，则按要求把当前所有未提交文件纳入本次原子提交。
9. 使用清晰、包含任务编号的提交消息提交全部本任务改动，确认提交和工作区状态，然后停止。

## 当前任务与验收边界

- 当前任务：`M3-2 [TODO] Capability（结构化）`，是 `TODO.md` 中第一个标题未带 `[DONE]` 的任务。
- 实现范围：在 Client 层加入结构化 `Capability` 与 `Modality`，包括上下文上限、输入/输出模态、streaming/tool calling/parallel tool calls/prompt caching/reasoning/structured output 支持，以及可接受的 stop reason 集合。
- 明确验证：全部类型 serde round-trip；构造 Anthropic 与 OpenAI 各一个默认 Capability 常量并断言关键字段；运行格式化、严格 clippy、完整测试和严格文档构建。
- 非范围：不提前实现 `M3-3 EndpointConfig/ChatRequest` 或 `M3-4 LlmClient`。

## 实现方案

1. 新建聚焦模块 `src/client/capability.rs`，定义可 serde 的 `Modality` 和 `Capability`；集合使用 `BTreeSet`，保证集合语义和序列化输出稳定。
2. 为 `StopReason` 补充集合键所需的全序与哈希派生，不改变现有 wire 表示或归一化行为。
3. 提供 Anthropic 与 OpenAI Responses 两份只读默认 Capability 表项。由于 Capability 属于协议/模型能力，而仓库未给出可泛化到所有模型的 context window 数值，协议级默认将 `max_context_tokens` 保持为 `None`，后续具体模型或用户配置可在克隆后覆盖。
4. Anthropic 默认覆盖文本/图片/文件输入、文本输出；OpenAI Responses 默认覆盖文本/图片/音频/文件输入和文本/音频输出。两者明确列出 streaming、tool calling、并行工具、prompt caching、reasoning 与 structured output 支持，以及各自已归一化的 stop reason 集合。
5. 在 `src/client/mod.rs` 中公开 capability 子模块并重导出公共类型及默认表项。
6. 单元测试覆盖：`Modality` 全变体 snake_case serde、含 `Some(max_context_tokens)` 的完整 Capability round-trip、两份默认表关键字段、克隆后覆盖不会改变默认表。

## 核查结论

- 工作区在本次开始时仅有本文件的新修改；没有用户遗留代码改动。
- 最新提交 `bab5bc7 [M3-1] Implement classified client errors` 已完成前一任务，未提及 M3-2 的未竟问题。
- `DESIGN.md` 与 `PLAN.md` 未提供具体模型的 context window 数值，因此不编造协议级上限；这不是缩窄能力模型，`Option<u32>` 仍完整支持具体值和覆盖。

## 当前状态

- 状态：实现、验证、TODO 完成记录、最终任务边界复核与 Git 提交均已完成；本次停止，不开始下一任务。
- 当前任务：`M3-2 Capability（结构化）`。
- 计划变更：无。
- 验证结果：`cargo fmt --all`、严格 clippy、Capability 聚焦测试、完整测试（65 passed）和严格 rustdoc 均通过。

## 进度记录

- 已新增 `src/client/capability.rs`：`Modality`、`Capability`、Anthropic/OpenAI Responses 默认表项，以及 serde/default override 测试。
- 已在 `src/client/mod.rs` 公开模块并重导出公共 API。
- 已为 `StopReason` 增加 `PartialOrd`、`Ord`、`Hash` 派生，以作为集合元素；归一化逻辑和 serde wire name 未改变。
- `cargo fmt --all` 通过。
- `cargo clippy --all-targets -- -D warnings` 通过，无 warning。
- `cargo test client::capability::tests` 通过：5 passed。
- `cargo test --all --all-targets` 在 30 分钟超时保护下通过：65 passed，0 failed。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过。
- `TODO.md` 的 M3-2 标题已改为 `[DONE]` 并写入实现与验证记录；阶段计划未变化，未修改 `PLAN.md`。
- 最终复核确认 M3-3 仍为 `[TODO]` 且未被实现；测试后仅修改 Markdown 记录，按规则复用绿色完整测试结果。
- 本次全部变更已纳入提交 `[M3-2] Implement structured capabilities`。
