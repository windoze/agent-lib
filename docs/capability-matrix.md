# Capability 能力矩阵与逃生舱实证

本文记录 `agent-lib` 当前两种 wire protocol 的 `Capability` 默认值，并把这些协议级
默认值与 2026-07-13 在 Microsoft Foundry 部署上的实测范围分开说明。这样，调用方既能
看到库公开的默认能力，也不会把尚未针对具体 model/deployment 验证的能力误认为运行时
保证。

默认值的唯一代码来源是
[`src/client/capability.rs`](../src/client/capability.rs)。`AnthropicAdapter` 与
`OpenAiRespAdapter` 的 `LlmClient::capability()` 分别返回对应默认表项。当前没有运行时
能力探测；部署、模型或 API 版本有差异时，调用方应克隆默认值并应用自己的覆盖。

## 协议级默认值

| `Capability` 字段 | Anthropic Messages 默认值 | OpenAI Responses 默认值 |
|---|---|---|
| `max_context_tokens` | `None` | `None` |
| `input_modalities` | `Text`, `Image`, `File` | `Text`, `Image`, `Audio`, `File` |
| `output_modalities` | `Text` | `Text`, `Audio` |
| `streaming` | `true` | `true` |
| `tool_calling` | `true` | `true` |
| `parallel_tool_calls` | `true` | `true` |
| `prompt_caching` | `true` | `true` |
| `reasoning` | `true` | `true` |
| `structured_output` | `true` | `true` |
| `stop_reasons` | `ToolUse`, `EndTurn`, `MaxTokens`, `StopSequence`, `Refusal` | `ToolUse`, `EndTurn`, `MaxTokens`, `Refusal` |

`max_context_tokens` 有意保持未知：context window 属于具体模型和部署，而不是 wire
protocol 的固定属性。集合使用 `BTreeSet`，因此序列化与测试顺序稳定。这里的 modality
集合描述协议能力上界；当前公共 `ContentBlock` 和 adapter 映射是否已经为某种输入形态
提供一等类型，应以对应公共 API 为准。尚未实测的默认值也不是特定 Foundry 部署的服务
等级承诺。

## 当前 Foundry 部署的实测范围

真实集成测试使用已归档
[`Client 层 PLAN.md`](archive/2026-07-13-client-layer/PLAN.md) 所列的两个 endpoint：
Anthropic Messages wire 的
`databricks-claude-haiku-4-5`，以及 OpenAI Responses wire 的 `gpt-5.5`。测试不会记录
认证值。下表中的“未实测”表示默认表仍声明协议支持，但本轮验收没有据此推断具体部署
一定支持。

| 能力 | Anthropic Messages / Foundry | OpenAI Responses / Foundry |
|---|---|---|
| 文本输入与输出 | 非流式、流式和多轮均已实测 | 非流式、流式和多轮均已实测 |
| 图片、音频、文件 | 本轮真实 endpoint 未实测 | 本轮真实 endpoint 未实测 |
| tool calling | 单次 tool call 与 tool result 回灌已实测；原始 `tool_use` stop reason 保留 | 单次 function call 与 result 回灌已实测；终态 `completed` 保留并归一为 `ToolUse` |
| parallel tool calls | 交错 block 的归一化与折叠由 fixture 测试覆盖；真实 endpoint 未实测 | 交错 item 的归一化与折叠由 fixture 测试覆盖；真实 endpoint 未实测 |
| streaming | text 与 tool SSE 已实测，Anthropic `index` 映射为稳定 `BlockId` | text 与 tool SSE 已实测，`item_id`/`output_index` 映射为稳定 `BlockId` |
| prompt caching | 实际响应含 cache creation/read 计数及 `cache_creation` 明细；本次样本的 creation/read 计数为 0 | 实际响应含 `input_tokens_details.cached_tokens`，样本归一为 `cache_read = 4` |
| reasoning/thinking | thinking、signature 和增量映射由协议 fixture 覆盖；当前真实 endpoint 场景未要求 thinking | 实际响应含 reasoning item，并报告 `reasoning_tokens = 18` |
| structured output | 协议默认值为支持；当前真实 endpoint 场景未实测 | 协议默认值为支持；当前真实 endpoint 场景未实测 |
| 已观察终止原因 | `end_turn`, `tool_use` | `completed`，根据输出内容归一为 `EndTurn` 或 `ToolUse` |

真实连通与归一化矩阵位于
[`tests/integration_normalization.rs`](../tests/integration_normalization.rs)，默认标记为
`#[ignore]`，仅在提供 `.envrc` 所述配置时访问 endpoint。协议边界、错误路径以及尚未由
真实调用覆盖的 stop reason 由录制 fixture 和合成单元测试验证。

## 响应侧逃生舱实证

响应侧逃生舱遵循 `DESIGN.md` 的机制 B：已建模字段进入 provider-neutral 字段，未建模
字段保留在最接近语义位置的 `extra: Map<String, Value>` 中。保留原始 JSON 值，而不是
只留下“曾出现过该字段”的布尔证据。

| Provider 方言字段 | 原始位置 | 归一化后位置 | 已建模的相关字段 |
|---|---|---|---|
| Foundry Anthropic cache creation 明细 | `usage.cache_creation.ephemeral_5m_input_tokens` / `ephemeral_1h_input_tokens` | `Response.usage.extra["cache_creation"]`，完整对象保留 | `cache_creation_input_tokens` → `Usage.cache_write`; `cache_read_input_tokens` → `Usage.cache_read` |
| Azure OpenAI 内容过滤结果 | 顶层 `content_filters[]`，含 prompt/completion 的分类、offset 与原始结果 | `Response.extra["content_filters"]`，完整数组保留 | 被阻断的响应同时归一为 `StopReason::Refusal`，但原始过滤证据不删除 |

[`tests/capability_escape_hatches.rs`](../tests/capability_escape_hatches.rs) 使用脱敏的真实响应
fixture，通过公开的 adapter 解析 API 做以下断言：

1. 原始 Anthropic `usage.cache_creation` 对象与 `Usage.extra` 中的值全等。
2. 原始 Azure `content_filters` 数组与 `Response.extra` 中的值全等。
3. 两种响应经过 `Response` 的 serde 序列化与反序列化后，上述值仍全等。

因此，归一化字段可供跨 provider 逻辑直接使用，调用方同时仍能检查具体 endpoint 的
方言证据；新增 provider 字段不会因为当前模型尚未认识它们而静默丢失。
