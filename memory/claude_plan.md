# 本轮执行计划

## 约束与当前状态

- 本轮只完成 `TODO.md` 中按顺序出现的第一个标题未带 `[DONE]` 的任务，完成后立即停止，不进入下一任务。
- `TODO.md` 是任务顺序、依赖、验收条件和完成记录的唯一事实来源；只有阶段级计划发生实质变化时才修改 `PLAN.md`。
- 当前尚未读取仓库状态或任务内容；这是本轮的第一个文件操作。以下先记录可审计的执行计划，不记录模型的私有逐字思维链。
- 不为方便而拆分任务。只有发现阻断当前任务的具体、未跟踪前置条件时，才在 `TODO.md` 中加入最少数量的前置任务，提交该结构性变更后停止。
- 已存在的用户改动必须保留；若这是一次中断后的续作，最终提交应原子地包含当前任务遗留的全部未提交文件。

## 执行步骤

1. 首先完整读取 `TODO.md`，按标题是否显式带 `[DONE]` 判断完成状态，定位第一个未完成任务并摘录其范围、依赖、测试要求和完成记录要求。
2. 检查 `git status` 与最新一次提交信息；只分析与当前任务直接相关的未提交改动和最新提交所明确提及的未完事项，不进行开放式历史缺陷扫描。
3. 阅读当前任务直接涉及的设计、源码、测试及仓库级说明文件（包括适用的 `AGENTS.md`）；确认预期行为、现有实现边界和验收路径。
4. 若发现具体阻断项，判断它是否会阻止任务正确实现、使规定行为无效，或属于本轮直接引入的回归：
   - 可以在当前任务范围内完整修复时，修复同一根因影响的整个已识别类别并补测试；
   - 必须新增前置任务时，以最小粒度更新 `TODO.md` 的顺序与依赖，必要时更新阶段级 `PLAN.md`，提交后停止；
   - 不通过缩小表示、专用特例、弱化断言或其他 workaround 绕过问题。
5. 采用小而聚焦的补丁完成实现，并在每个关键步骤后重新读取相关区段；为公开行为补齐正常、边界、错误和序列化/流式状态等与任务要求相符的测试。
6. 按规定顺序验证：先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，然后在不超过 30 分钟的超时下运行 `cargo test --all --all-targets`，最后按任务要求运行文档构建或其他专项检查。任何未被后续明确任务精确覆盖的失败都必须立即修复或排入正确的前置位置。
7. 验证通过后，仅在任务标题前加 `[DONE]`，并在 `TODO.md` 的完成记录中写明实现内容、测试命令和结果；只有阶段结构确实变化才更新 `PLAN.md`。
8. 再次检查 diff、格式、测试结果与工作树，确认没有遗漏或意外改动；用包含任务编号的清晰信息提交当前任务的全部相关改动。若 `PROMPT.md` 意外变化，也按要求纳入提交而不擅自还原。
9. 将本文件更新为最终进度摘要（所选任务、关键决定、验证结果、提交号），确认提交后停止，不读取或实施下一个任务。

## 进度

- [x] 在运行任何仓库检查或命令前建立本轮计划。
- [x] 定位首个未完成任务并确认工作树上下文。
- [x] 完成实现与测试。
- [x] 通过全部规定验证。
- [x] 更新 `TODO.md` 完成记录。
- [x] 提交并停止。

## 已选任务：M5-1

- 标题：`OpenAI Response 请求构造与非流式响应`。
- 请求侧验收：将统一 `ChatRequest` 显式映射为 Responses API 的 `input`、`instructions`、`tools`、`max_output_tokens` 等字段，并完整应用 `EndpointConfig`、Azure `api-key` 和 `api-version` query；OpenAI 专属 extras 只能在最终序列化阶段合并。
- 响应侧验收：把 `output[]` 中的 message、reasoning、function_call item 归一为 `Response.message.content`；把 input/output/reasoning token 分列；把状态/失败信息映射为保留 raw 的 `Normalized<StopReason>`；Azure `content_filters` 及其他未知字段必须进入合适的 `extra`，不得丢失。
- 测试验收：无网络请求体单元测试、基于真实探测 JSON 的解析测试，以及默认忽略且有严格超时的真实 `gpt-5.5` 非流式集成测试。
- 仓库上下文：最新提交 `a58e380e11b0658fe66cf7f1aca3bd06c2a07dc4`（`[M4-R] Complete Anthropic milestone review`）未声明与 M5-1 相关的未完问题；本轮开始时无既有未提交改动。

## M5-1 细化步骤

1. 阅读 `PLAN.md`、`DESIGN.md`、OpenAI Responses 参考/探测记录、现有模块树与 Anthropic 适配器实现，明确 wire 形态和可复用的 HTTP/错误处理边界。
2. 建立聚焦的 `adapter/openai_resp` 子模块，将请求编码、完整响应解析和 adapter 门面分开，避免形成过长源文件。
3. 实现请求 URL/header/query/body 构造，覆盖完整内容块、工具 schema、provider extras、错误输入和 `stream=false` 非流式约束。
4. 实现完整响应解析与 `LlmClient::chat`，覆盖 message 文本/refusal、reasoning、function_call、usage、stop reason、Azure 方言字段及 HTTP/协议错误分类；本任务不提前实现 M5-2 的 SSE。
5. 增加真实 fixture 驱动的单元测试、本地 HTTP 传输测试和忽略的真实 endpoint 测试；发现 wire 与既有中立模型之间的阻断性缺口时，按无 workaround 原则修复根因或写入最小前置任务并停止。

## 调研结论与实现决定

- `openai-docs` 官方 MCP 已注册，但当前会话无法热加载；已改用同一官方域名的 Responses migration、function calling 与 vision 指南核对字段。官方资料确认 Responses 顶层使用 `instructions`/`input`，上下文为 typed Item；function tool 使用扁平 `type/name/description/parameters`；函数结果通过 `call_id` 关联；图片通过 `input_image.image_url`（base64 为 data URL）传入。
- 已对配置中的 Foundry endpoint 完成两次脱敏、无状态非流式探测：
  - 文本响应包含顶层 Azure `content_filters`、空 `reasoning` item、`message` item 的 `output_text`，状态为 `completed`；
  - 工具响应包含 `function_call` item，`arguments` 为完整 JSON 字符串，`call_id` 是应映射到中立 `ToolUse.id` 的关联标识；
  - usage 使用 `input_tokens_details.cached_tokens` 和 `output_tokens_details.reasoning_tokens`。
- 请求转换采用 item 级映射：普通文本/图片组成 message item，assistant tool use 组成 `function_call` item，tool result 组成 `function_call_output` item，thinking 组成 reasoning item；不合法的 role/block 组合返回协议错误，不静默改形。
- 响应转换把 message content、reasoning、function_call 依序折叠成一个 assistant `Message`；item/content 层尚未建模的元数据放入块 `extra` 的 OpenAI 命名空间，未知 output item 原值放入顶层 `Response.extra`，顶层 `content_filters` 等字段直接保留。
- stop reason 由 status、`incomplete_details.reason` 和实际 output 分类共同确定：function call 优先归一为 `ToolUse`，正常 completed 为 `EndTurn`，max-output incomplete 为 `MaxTokens`，refusal/content-filter 为 `Refusal`，其他状态保留 raw 并归 `Other`。
- 现有 `Usage` 未把真实 Responses 字段 `input_tokens_details.cached_tokens` 归入 `cache_read`。这是当前任务直接依赖的同类 alias 缺口，将在 M5-1 内完整补齐并测试。

## 实施进度

- 已实现 `OpenAiRespAdapter` 门面、`POST /responses` 请求构造和完整态 `chat` 传输；尚未加入 `LlmClient` 实现，因为 trait 要求的 SSE 方法属于紧随其后的 M5-2，当前不放置未实现 shim。
- 请求转换已覆盖 text、URL/base64 image、reasoning、function call、function call output、JSON Schema tools、instructions、max output tokens、temperature、stream 与 OpenAI provider extras；不合法 role/block 组合和跨 provider extras 均返回可观测错误。
- 响应转换已覆盖 message/output_text/refusal、reasoning、function_call，完整 JSON arguments 只解析一次；item/content 元数据进入块级 `openai_response` extra，未知 output item/part 进入顶层逃生舱，Azure `content_filters` 原样保留。
- 通用 `Usage` 已补齐 `input_tokens_details.cached_tokens`/cache creation aliases，并保留 details 中其他未知计数。
- 第一次聚焦测试暴露空 query 列表也会生成尾随 `?`；同一根因影响 Anthropic 与 OpenAI 两个请求构造器，现已做类级修复并为两边添加回归断言。
- 当前聚焦结果：`cargo test adapter::openai_resp:: -- --nocapture` 14/14 通过；OpenAI Responses usage alias 聚焦测试通过。
- 真实验证：加载 `.envrc` 后，`cargo test --test integration_openai_resp -- --ignored --nocapture` 1/1 通过，耗时 2.32 秒；拿到非空文本、usage、`completed` stop 和 Azure `content_filters`。
- 已用 `async-openai 0.41.1` 的 Responses 类型复核：reasoning `id` 可选且 summary/content/encrypted_content 形态匹配；function call output 支持字符串或 input content 数组及可选 completed/incomplete status；当前请求表示与权威类型一致。

## 最终验证结果

- `cargo fmt --all`：通过。
- `cargo clippy --all-targets -- -D warnings`：通过，无 warning。
- `gtimeout 1800 cargo test --all --all-targets`：通过，115 passed、0 failed、4 ignored；所有实际运行的单测均远低于 1 分钟。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：通过。
- 加载 `.envrc` 后 `gtimeout 120 cargo test --test integration_openai_resp -- --ignored --nocapture`：通过，1 passed，3.09 秒。
- `PLAN.md` 未修改：M5 阶段顺序、依赖和完成标准没有变化，本轮只完成既定 M5-1 执行单元。
- `TODO.md` 已将首个未完成任务 `M5-1` 的标题显式改为 `[DONE]` 并补充完成记录；未开始 M5-2。
- 已创建并复核任务提交：`[M5-1] Implement OpenAI Responses non-streaming adapter`；本文件的最终状态将 amend 进同一提交，随后只读确认并停止。
