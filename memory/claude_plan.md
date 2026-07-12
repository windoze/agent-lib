# 当前任务执行计划

## 决策摘要

- `TODO.md` 是任务顺序、需求、依赖、验证要求和完成记录的唯一权威来源。
- 本次调用只处理首个标题未带 `[DONE]` 的任务；完成后立即停止，不进入后续任务。
- 在读取 `TODO.md` 前不做开放式缺陷排查。选定任务后，只检查与该任务直接相关的实现、最新提交和验证状态。
- 若发现会阻塞当前任务的真实前置缺陷，则按规则在 `TODO.md` 中加入最少量的前置任务、保持当前任务未完成、提交任务表变更并停止。
- 若能够完成任务，则实现全部要求，按规定顺序执行格式化、严格 lint、完整测试和文档验证，更新 `TODO.md` 的标题及完成记录，最后提交所有本次任务范围内以及任何遗留的未提交文件。
- 不把 `PLAN.md` 当作日常进度日志；只有阶段级顺序、依赖、假设或完成标准变化时才更新。

## 分步执行计划

1. 读取 `TODO.md`，从上到下找出首个标题未带 `[DONE]` 的任务，摘录其需求、依赖、验收条件和完成记录要求。
2. 查看最新一次提交的主题与必要详情，判断是否明确提到与当前任务直接相关的未完成问题。
3. 检查工作区状态；区分用户既有改动、上次中断遗留改动和当前任务所需改动，避免覆盖或丢失任何内容。
4. 仅读取当前任务直接涉及的设计、代码和测试，建立需求到实现及验证的对应关系；若遇到阻塞性规格缺口，按“最少前置任务”规则处理并停止。
5. 用小而聚焦的补丁完成实现和测试；每完成关键步骤即更新本文件的进度和必要的计划调整。
6. 按顺序运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`（完整测试最长 30 分钟）以及任务要求的其他验证；修复所有未被后续明确任务覆盖的失败。
7. 必要时运行 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`，并核对当前任务的全部验收项。
8. 在 `TODO.md` 中给任务标题加 `[DONE]`，填写准确的完成记录和验证结果；仅在阶段计划确实变化时修改 `PLAN.md`。
9. 复查 diff 和工作区，提交本次完整任务；提交信息包含任务编号和清晰描述。
10. 确认提交成功、工作区状态符合预期，然后停止，不开始下一个任务。

## 当前进度

- [x] 在执行其他命令前创建本计划文件。
- [x] 识别首个未完成任务：`M4-3 Anthropic 流式(SSE) → StreamEvent`。
- [x] 核对最新提交与工作区状态：最新提交为完整的 M4-2；未声明 M4-3 阻塞项；初始工作区无代码遗留，仅本计划文件被本次修改。
- [x] 完成实现与针对性测试：Anthropic 流聚焦测试 15 项、统一 Accumulator 聚焦测试 13 项均通过。
- [x] 完成格式化、lint、完整测试及文档验证；真实 endpoint 文本/工具流式测试也已通过。
- [x] 更新 `TODO.md`：仅 `M4-3` 标为 `[DONE]`，`M4-R` 保持未完成；阶段计划未变化，未修改 `PLAN.md`。
- [x] 已提交本次完整任务；最终进度状态将 amend 到同一提交后停止，不进入下一任务。

## M4-3 任务边界与验收映射

- 解析 Anthropic SSE 的 `message_start`、`content_block_start`、`content_block_delta`、`content_block_stop`、`message_delta`、`message_stop`，产出统一 `StreamEvent`。
- 在适配器内部维护 Anthropic `index` 到稳定 `BlockId` 的映射；text、thinking、tool_use 分别映射为 `BlockKind::Text`、`Reasoning`、`ToolInput`。
- text/input_json/thinking delta 分别映射为 `Delta::Text`、`Json`、`Reasoning`；工具 JSON 只累积，完成边界发出 `ToolInputAvailable`。
- 用真实探测 SSE fixture 覆盖事件顺序、跨分片解析和 id 关联；把事件交给唯一 `Accumulator`，断言折叠结果与非流式响应结构一致。
- 增加默认忽略的真实流式集成测试，覆盖文本 `count 1..5` 与 `get_weather(Tokyo)` 工具调用，并限制单测试运行时间低于一分钟。
- 实现完整的 `LlmClient::chat_stream` 路径，正确应用 endpoint/auth/query/header，非 2xx 和传输/协议错误继续使用统一 `ClientError` 分类。
- 只有全部验收和仓库级验证通过后，才把 `M4-3` 标题改成 `[DONE]` 并填写完成记录；`M4-R` 保持未完成。

## 实现设计（完成相关源码审阅后细化）

1. 添加轻量 `eventsource-stream` 依赖，把任意 HTTP 字节分片按 SSE 标准解成完整 event；不手写易漏掉 UTF-8 跨分片、多行 data、CRLF 等边界的传输解析器。
2. 新建模块化的 `adapter/anthropic/stream/`：
   - serde 解码 Anthropic 事件 envelope；只忽略明确的 `ping`，未知/错序/重复事件均返回 `ClientError::Protocol`。
   - 状态机维护 message 生命周期、`index -> anthropic-block-{index}`、各 block kind、tool JSON 原始片段和最终 stop reason。
   - `message_start` 与 `message_delta` 的 Anthropic usage 是累计快照；先转成非负增量再发 `Usage`，避免统一 `Accumulator::merge` 重复计算 output tokens。
   - 工具块只在 stop 边界解析完整 JSON，依次发 `ToolInputAvailable` 与 `BlockStop`；非法 JSON直接形成协议错误。
   - 正常 `message_stop` 后结束流；HTTP body/SSE/生命周期中途终止均形成可观察错误。
3. 补齐 thinking signature：Anthropic 实际协议含 `signature_delta`，而当前模型不能把它折叠进 `ContentBlock::Thinking.signature`。为 `Delta` 增加 provider-neutral 的 `ReasoningSignature`，并在唯一 `Accumulator` 中按 reasoning block 累积；同时增加回归测试。该修复是 M4-3 正确实现和 M4-R 验收的直接前提，不另行拆任务。
4. 补齐流式响应 metadata 逃生舱：真实 Foundry `message_stop` 含 `amazon-bedrock-invocationMetrics`，现有事件模型会令折叠后的 `Response.extra` 永远为空。新增通用 `ResponseMetadata` 事件，并让 Accumulator 合并 message start/delta/stop 的未建模顶层字段，避免把响应 metadata 错塞进 usage 或直接丢弃。
5. 在 `AnthropicAdapter` 上提供 inherent `chat_stream`，并实现 `LlmClient`（capability、非流式、流式均转发到既有路径）。成功响应校验 SSE content type；非 2xx 保留 body、HTTP 分类和 `Retry-After`。
6. 测试分层：真实形态 SSE fixture 的任意字节分片解析；text/tool/thinking/signature/id/usage/metadata/event-order 错误；统一 Accumulator 折叠；本地 HTTP 流式传输与错误分类；默认 ignored 的真实文本及工具集成测试。

## 完成证据

- 真实 Foundry 探测确认 text 流、工具流和累计 usage 形态；脱敏 fixture 已覆盖 `count 1..5` 与拆分后的 `{"city": "Tokyo"}`。
- 流式生产代码已按职责拆为 103 行传输入口、87 行 SSE 解码驱动、455 行事件状态机、55 行 usage 快照转换和 173 行 wire 类型，测试另拆 parsing/errors/transport 三个模块。
- `cargo fmt --all`：通过。
- `cargo clippy --all-targets -- -D warnings`：通过，无 warning。
- `cargo test --all --all-targets`：101 passed、0 failed、3 ignored。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：通过。
- 加载 `.envrc` 后运行 `cargo test --test integration_anthropic -- --ignored --nocapture`：3 passed（非流式 hi、流式 count、流式 get_weather Tokyo），总耗时 2.30 秒。
- staged diff 检查后为三份 SSE fixture 增加合法的注释终止行,既保留最后事件所需的空行分隔又消除 trailing blank line；随后重新运行 Anthropic 流聚焦测试(15 passed)和全量测试(101 passed,3 ignored),结果仍为绿色。
