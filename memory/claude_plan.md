# 当前任务执行计划

## 目标与约束

- 以 `TODO.md` 为唯一的逐任务事实来源，只处理其中标题未带 `[DONE]` 的第一个任务，然后停止。
- 在选择当前任务前不做开放式历史缺陷排查；只检查与当前任务直接相关的最新提交信息、依赖和既有实现。
- 不以缩小范围、改变既定模型、私有特判或测试规避来绕过规格问题；若出现确切阻塞项，按要求在 `TODO.md` 中加入最少的前置任务并提交后停止。
- 保护工作区中已有的用户改动；若这是上次中断后恢复的同一任务，完成时将当前未提交文件一并纳入原子提交。
- 本文件记录可审阅的计划、事实、决策依据和进度，不记录不可公开的内部逐字推理。

## 分步计划

1. 读取 `TODO.md`，严格按标题是否含 `[DONE]` 识别第一个未完成任务，并摘录其要求、依赖、验收条件和完成记录格式。
2. 查看工作树状态和最新提交摘要；只判断未提交内容或最新提交中是否存在与当前任务直接相关的续作/阻塞信息，不做无边界历史问题扫描。
3. 读取当前任务直接涉及的设计文档、源码与测试，核对既定接口和实现边界；如需修改执行方案，先更新本文件。
4. 按任务原定执行单元完整实现，采用小而聚焦的补丁，并在关键实现完成后更新本文件的进度与实际变更。
5. 增补或调整覆盖正常路径、边界条件和错误路径的测试；若发现规格不匹配，修复完整问题类别，不能规避。
6. 按规定顺序验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，再运行任务要求的测试及 `cargo test --all --all-targets`（完整测试最长 30 分钟），最后运行任务要求的文档检查。任何未被明确排期的失败都必须在本任务修复或成为前置任务。
7. 完成后在 `TODO.md` 的任务标题前加 `[DONE]`，填写准确的完成记录与验证结果；仅当阶段级计划确实变化时才修改 `PLAN.md`。
8. 复查差异、工作树和任务完成条件，更新本文件为已完成状态，然后用包含任务编号的清晰消息提交所有应纳入的改动。
9. 确认提交成功且没有遗漏本任务文件；立即停止，不开始下一任务。若这是最后一个任务，则按说明执行最终审查、必要调整并创建 `endtag`。

## 当前进度

- 已完成：在任何仓库读取或命令执行前建立本计划。
- 已完成：完整读取 `TODO.md`，按当时标题状态确认第一个未完成任务为 `M5-R [TODO] Milestone 5 Review`；本次不会开始 `M6-1`。
- 当前任务验收重点：
  - 对比 Anthropic 与 OpenAI Responses 的 `StreamEvent` 形态，确认均由同一个 `Accumulator` 折叠。
  - 核对 Azure `content_filters` 等方言字段在完整态与流式终态均进入 `extra`。
  - 核对 reasoning 与 tool arguments 的增量累积、完整边界解析及错误处理与 Anthropic 纪律一致。
  - 在环境变量可用时执行 OpenAI Responses 的真实非流式、流式文本和 tool-call 集成测试。
- 已完成：检查工作树、最新提交、M5 实现及测试覆盖，未发现直接相关的遗留阻塞。

## 审阅记录

- 工作树基线：开始时除本文件外无未提交改动；最新提交为 `[M5-2] Implement OpenAI Responses streaming adapter`，提交信息未指出仍未完成且与 M5-R 直接相关的问题。
- 同构事件：Anthropic 与 OpenAI Responses 都只向公共 `StreamEvent` 发出 `MessageStart`、带稳定 `BlockId` 的块三段式、`ToolInputAvailable`、`Usage`、`ResponseMetadata`、`MessageStop`/`Error`；两者测试均调用 `stream::accumulator::Accumulator`，没有 provider 私有折叠器。
- 工具输入纪律：OpenAI `function_call_arguments.delta` 仅追加并发出 `Delta::Json`，到 `function_call_arguments.done` 才核对完整字符串、一次性解析并发出 `ToolInputAvailable`，随后在 item done 发出 `BlockStop`；测试覆盖真实五段 JSON、两个交错 tool item 和残缺 JSON 只在完成边界失败。该行为与 Anthropic 在 content block stop 边界解析一致。
- reasoning 纪律：OpenAI reasoning item 使用 `BlockStart(Reasoning)`、`Delta::Reasoning`、可选 `ReasoningSignature`、`BlockStop`；raw reasoning 优先、summary 回退规则与完整响应转换一致，测试覆盖 reasoning token 与 encrypted content 折叠。
- 逃生舱：OpenAI 流式 terminal snapshot 复用非流式 `parse_response_value`，再通过 `ResponseMetadata` 合并顶层 `extra`；录制 fixture 与真实集成断言均覆盖 Azure `content_filters`，未知未来流事件也保存在 `openai_unmodeled_stream_events`。
- 当前结论：未发现需要改变 M5 阶段设计或新增前置任务的规格缺口；下一步按规定顺序执行格式化、严格 lint、完整测试、文档构建及可用环境下的真实集成测试。

## 验证结果

- `cargo fmt --all`：通过。
- `cargo clippy --all-targets -- -D warnings`：通过，无 warning。
- `cargo test --all --all-targets`：通过；129 个单元测试通过，0 失败；6 个真实 endpoint 测试按默认配置忽略。
- `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`：通过，无文档 warning。
- 加载 `.envrc` 后运行 `cargo test --test integration_openai_resp -- --ignored --nocapture`：3 个真实 OpenAI Responses 测试全部通过，覆盖非流式文本、流式文本与流式工具调用，总耗时 2.41 秒。
- 所有已运行测试均远低于单测 1 分钟限制，未观察到卡住或未排期失败。

## 收尾计划

1. 将 `M5-R` 标题改为 `[DONE]` 并写入上述审阅证据与验证记录；阶段顺序、依赖和完成标准未变化，因此不修改 `PLAN.md`。
2. 检查最终差异、Markdown 完成状态和空白错误；仅文档在最后一次完整测试后变化，无需重复运行编译测试。
3. 更新本文件为完成状态，提交 `TODO.md` 与本文件，然后确认提交及工作树状态并停止，不进入 `M6-1`。

## 完成状态

- `M5-R` 已在 `TODO.md` 标记为 `[DONE]` 并填写完成记录。
- `PLAN.md` 未修改：本次审阅没有改变阶段顺序、依赖、假设或完成标准。
- 最终差异仅包含 `TODO.md` 与本进度文件；`git diff --check` 已通过。
- 待执行：创建 `[M5-R]` 提交并确认提交后的工作树，然后停止。
