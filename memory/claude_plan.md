# 当前执行计划

## 目标与边界

- 本次调用只处理 `TODO.md` 中标题未带 `[DONE]` 的第一个任务。
- `TODO.md` 是任务顺序、依赖、验收条件和完成记录的唯一事实来源；仅当阶段级计划发生变化时才修改 `PLAN.md`。
- 不做开放式历史问题扫描。只检查当前任务、最新提交中与当前任务直接相关的未完成事项，以及验证过程中实际暴露的失败。
- 不以缩小范围、特殊分支或替代表示规避规范问题。若出现阻塞当前任务的真实前置缺口，则按要求在 `TODO.md` 中增加最少的前置任务、提交并停止。

## 执行步骤

1. 读取 `TODO.md`，按标题的 `[DONE]` 前缀确定第一个未完成任务，并完整提取其需求、依赖和验证要求。
2. 检查工作区状态和最新提交；保留用户已有改动，并只把最新提交中明确提到且直接影响当前任务的未完成问题纳入范围。
3. 阅读当前任务涉及的设计文档和实现/测试代码，建立需求到代码与测试的对应关系；若发现具体阻塞前置条件，立即更新本文件和 `TODO.md`，不继续绕过问题。
4. 以小而聚焦的补丁完成实现，并补充覆盖正常路径、边界情况、错误路径和序列化/兼容性要求的测试；每完成关键步骤即更新本文件。
5. 按指定顺序验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，再运行任务要求的测试及 `cargo test --all --all-targets`（完整测试最长 30 分钟），最后按任务要求运行文档构建等检查。
6. 对任何测试失败进行根因处理：修复整类缺陷；若确属已明确排期的后续任务则核对记录，否则修复或插入最少的前置/跟进任务。在存在未处理、未排期失败时不标记完成。
7. 验收全部通过后，在 `TODO.md` 的任务标题前加 `[DONE]` 并填写可复核的完成记录（实现、测试命令及结果）；仅在阶段级依赖或完成标准变化时更新 `PLAN.md`。
8. 复查差异和工作区状态，确保没有遗漏本次恢复任务应纳入的未提交文件；创建一个清晰描述该任务的 Git 提交。
9. 确认提交成功、工作区状态符合预期后停止，不开始下一个任务。若本次恰好完成全部任务，则额外执行最终审查并按要求创建 `endtag`。

## 进度记录

- 已建立初始执行计划。
- 已完整读取 `TODO.md` 并锁定本次唯一任务：`M4-R [TODO] Milestone 4 Review`；后续 `M5-1` 及之后任务不在本次范围。
- 当前任务的明确验收点为：同一 prompt 的非流式结果与流式折叠结果一致；Anthropic index 到稳定 block id 的映射正确；tool JSON 只在完整累积后解析；thinking signature 保留；Foundry cache 明细及其他方言字段进入 `extra`；环境可用时真实非流式/流式/tool 集成测试全部通过。

## M4-R 专项审阅路径

1. 检查 Git 工作区与最新提交，只识别与 M4-R 直接相关的遗留事项，并保留所有已有未提交改动。
2. 对照 M4-1 至 M4-3 的实现、单元测试、真实 fixture 和集成测试，逐项建立 M4-R 验收证据。
3. 重点核对是否已有“同一 prompt”下非流式 `Response` 与流式经统一 `Accumulator` 折叠结果的直接一致性断言；若没有，补充不依赖网络且可稳定复现的端到端回归测试，避免只凭两组独立测试推断一致性。
4. 核对稳定 block id、tool JSON 完整边界、thinking signature、顶层/块级/usage 级 `extra` 的测试覆盖；只修复实际缺口，不触碰 OpenAI 适配器任务。
5. 按顺序运行格式化、严格 clippy、完整测试和文档构建；若 `.envrc` 能提供 Anthropic 测试所需变量，则加载后执行全部 ignored Anthropic 真实集成测试，并确保单项均在一分钟内结束。
6. 全部通过后将标题改为 `M4-R [DONE]`，填写详细完成记录，复查差异并提交本任务的全部未提交文件，然后停止。

## 审阅发现

- Git 初始状态仅有本文件的本次计划更新；最新提交 `eed8f81 [M4-3] Implement Anthropic streaming SSE adapter` 未声明与 M4-R 直接相关的未完成问题。
- 文本录制流测试 `real_text_sse_maps_events_and_matches_complete_response_shape` 将真实 SSE 规范化、折叠后，与同一输出对应的完整 Anthropic JSON 解析结果做 `assert_eq!`，覆盖 content、usage、stop reason 和 response extra 的全结构一致性。
- 工具录制流测试 `real_tool_sse_keeps_raw_fragments_and_publishes_complete_input_at_stop` 同样与完整响应做全结构相等断言，并明确断言原始 JSON fragments、完整 `ToolInputAvailable` 与 `BlockStop` 的发布顺序。
- `interleaved_provider_indices_keep_stable_ids_and_start_order` 覆盖非连续且交错的 provider index `2/7` 映射为稳定 id `anthropic-block-2/7`，并验证按 block start 顺序折叠。
- `partial_tool_json_is_not_parsed_until_block_stop` 证明残缺 JSON delta 到达时不会解析，只有完整边界 `content_block_stop` 才返回协议错误；不存在边流边 parse 的绕行实现。
- `thinking_signature_deltas_survive_normalization_and_folding` 证明分片 signature 经 `ReasoningSignature` 拼接后完整落入 `ContentBlock::Thinking.signature`。
- 完整响应测试覆盖顶层、content block 与 usage 三层逃生舱；流式文本 fixture 覆盖 `usage.cache_creation.ephemeral_5m_input_tokens` / `ephemeral_1h_input_tokens` 以及顶层 `amazon-bedrock-invocationMetrics` 合并到最终 `Response.extra`，且流式/非流式完整结果相等。
- 真实集成测试共有三项：非流式文本、流式文本、流式工具调用；每项均有 55 秒超时，满足单测试少于一分钟的要求。
- 当前未发现阻塞 M4-R 或要求新增前置任务的规范偏差；进入验证阶段。

## 验证与完成状态

- `cargo fmt --all`：通过，未产生源码格式改动。
- `cargo clippy --all-targets -- -D warnings`：通过，无 warning。
- `cargo test --all --all-targets`：通过；101 项单元测试通过，0 失败；真实 Anthropic 测试按默认配置 3 项 ignored。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：通过，无文档 warning。
- 安全检查确认 `.envrc` 中 `ANTHROPIC_BASE_URL` 与 `ANTHROPIC_AUTH_TOKEN` 均可用，未输出凭据值。
- 加载 `.envrc` 后执行 `cargo test --test integration_anthropic -- --ignored --nocapture`：3 项全部通过，0 失败，总耗时 2.73 秒；每项均远低于一分钟上限。
- 已将 `TODO.md` 中唯一当前任务标题更新为 `M4-R [DONE] Milestone 4 Review`，并写入逐项审阅证据与全部验证结果。
- 阶段顺序、依赖和完成标准均未变化，因此按约束不修改 `PLAN.md`。
- 最终 diff 与空白错误检查通过；已创建 `[M4-R] Complete Anthropic milestone review` 提交，纳入 `TODO.md` 和本进度文件。
- 剩余步骤：把本条最终状态并入同一任务提交，确认提交和工作区状态后停止，不进入 M5。
