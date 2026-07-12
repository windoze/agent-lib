# 当前执行计划

## 目标与约束

- 以 `TODO.md` 为唯一任务顺序与验收依据，识别并完成首个标题未带 `[DONE]` 的任务。
- 不做开放式历史问题扫描；仅检查当前任务、其直接依赖、最新提交中与该任务直接相关的未完成事项，以及验证过程中暴露的失败。
- 若发现阻塞当前任务且尚未跟踪的真实前置问题，只在 `TODO.md` 中加入最少量前置任务、记录依赖、提交后停止；不以缩小范围或特殊处理规避规范。
- 若可完成，则完整实现、测试、更新 `TODO.md` 的标题与完成记录、提交全部相关未提交改动，然后停止，不进入下一任务。
- `PLAN.md` 仅在阶段级顺序、依赖、假设或完成标准确实变化时更新。

## 分步计划

1. 读取 `TODO.md`，从头定位首个标题未显式标记 `[DONE]` 的任务，提取其需求、依赖、测试与完成记录要求。
2. 检查 Git 工作区与最新提交，区分既有用户改动/上次中断遗留，并确认最新提交是否明确提到与当前任务直接相关的未完成问题。
3. 只读取当前任务所需的设计、计划和源码/测试上下文，建立实现边界；如发现具体阻塞，按规则更新任务依赖并停止。
4. 以小而聚焦的补丁实现当前任务，同时补齐覆盖正常路径、边界、错误路径和兼容性的测试及必要文档。
5. 每完成关键步骤或计划发生变化时更新本文件，记录已完成事项、发现的问题与下一步。
6. 按指定顺序验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，再运行任务指定测试与最长不超过 30 分钟的完整 `cargo test --all --all-targets`；最后运行任务要求的文档构建或其他检查。
7. 若出现未被后续任务明确覆盖的测试失败，立即修复，或在 `TODO.md` 中加入最小前置/跟进任务；未解决前不把当前任务标为完成。
8. 完成后在 `TODO.md` 任务标题前加 `[DONE]` 并填写可复核的完成记录；仅在阶段计划实际变化时修改 `PLAN.md`。
9. 复查 diff、测试结果和任务范围，更新本文件为最终状态；使用清晰的任务编号提交所有相关未提交文件（包括恢复任务遗留文件），确认提交成功后停止。

## 当前状态

- 状态：已读取 `TODO.md`，首个未完成任务确定为 `M6-2 [TODO] 能力矩阵与逃生舱实证`；没有跳过 review 任务或后续任务。
- 当前任务交付物：新增 `docs/capability-matrix.md`，记录 Anthropic Messages 与 OpenAI Responses 的默认 `Capability` 及真实 endpoint 差异；新增/强化自动化测试，明确断言 Anthropic/Foundry 的 `cache_creation.ephemeral_*` 和 Azure/OpenAI 的 `content_filters` 落入 `extra` 且不丢失。
- 当前任务验收：能力矩阵必须与代码默认表和已录制真实 fixture 一致；聚焦逃生舱测试、格式化、严格 clippy、全量测试与文档构建全部通过；随后将 `M6-2` 标为 `[DONE]`、填写完成记录并提交。
- 已完成检查：任务开始前 Git 工作区无项目遗留改动（仅本文件是本次新增改动）；最新提交 `7844640 Update doc` 只加入后续 `NEXT-1`，未声明与 `M6-2` 直接相关的未完成问题，因此无需新增前置任务。
- 实现判断：两家完整响应解析器已具备正确逃生舱行为；Anthropic 单元测试已全值断言 `usage.extra.cache_creation`，OpenAI 测试目前主要断言 `content_filters` 键存在。当前任务无需改生产逻辑，但需要新增跨 provider 的公开 API 验收，比较原始 fixture 与归一化 `extra` 的完整 JSON 值，并验证 `Response` serde 往返后仍保持一致。
- 文档判断：矩阵将分成“代码中的协议级默认表”和“当前 Foundry 部署实测证据”两层；未在真实 endpoint 验证的模态、并行工具、结构化输出等只记录为默认能力，不伪装成部署实测结论。
- 已完成实现：新增 `docs/capability-matrix.md`，逐字段列出两家协议默认值、当前 Foundry 实测范围、未实测边界和方言字段归宿；新增 `tests/capability_escape_hatches.rs`，通过公开 adapter API 验证默认 capability 绑定，并对两份脱敏真实响应做原始 JSON 全值比较及 `Response` serde 往返。
- 生产代码未修改：现有解析行为满足任务要求；本次通过独立跨 provider 验收把此前分散/较弱的断言固化为明确证据。
- 已通过验证：`cargo fmt --all`；新增验收测试 3/3；`cargo clippy --all-targets -- -D warnings`；`cargo test --all --all-targets`（库单元测试 130 项 + 新增验收 3 项通过，7 项真实 endpoint 测试按预期忽略）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`cargo fmt --all -- --check`；`git diff --check`。
- 真实 endpoint 复验通过：加载 `.envrc` 后 Anthropic 非流式/流式文本/tool 3 项、OpenAI Responses 非流式/流式文本/tool 3 项全部通过；跨 provider normalization 矩阵完整执行纯文本、多轮和 tool 往返并通过（1 项，18.17 秒）。
- 已将 `TODO.md` 的任务标题更新为 `M6-2 [DONE]` 并写入文档、测试和验证记录；阶段级计划与依赖未变化，因此未修改 `PLAN.md`，也未开始 `M6-3`。
- 下一步：执行最终格式/diff/工作区审查，暂存本次全部四个文件并检查 staged diff；确认无误后创建单一 `[M6-2]` 提交并停止。
