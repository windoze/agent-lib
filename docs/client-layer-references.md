# Client 层参考实现与设计对照

> 结论(见 `genai-probe-findings.md`):client 层自研(方案 C)。本文档沉淀"抄谁、抄哪一部分",
> 作为起草 `Message` / `ContentBlock` / `StreamEvent` / `Usage` 类型骨架的直接依据。

## 参考分工总表

| 目标类型 | 主参考 | 抄什么 | 不抄什么 |
|---|---|---|---|
| `StreamEvent` | **Vercel AI SDK v5**(首要) | 事件分类学、id 关联机制、tool 三段式、step/approval/abort | 传输格式(SSE 编码)、纯 UI part(`data-*`/`source-url`) |
| `ContentBlock` | Anthropic SDK | 完整态块分类(text/image/tool_use/tool_result/thinking) | provider 专有序列化细节 |
| OpenAI Response 适配器 | `async-openai` | Response API 的 SSE 事件 → 我们 StreamEvent 的映射 | 它的类型直接对外(仅内部适配用) |
| OpenAI Chat/Completions 适配器 | `async-openai`(chat 模块) | classic `/v1/chat/completions` 的 `messages`/`tool_calls`/`stream` 结构、SSE chunk(含 `[DONE]` 哨兵、`include_usage` 终态 chunk) → StreamEvent 映射 | 它的类型直接对外(仅内部适配用);不为 DeepSeek/vLLM 方言建 quirk 类型(方言经 `ProviderExtras` 逃生舱) |
| `Message` / `Tool` 组织 | `genai` | Rust 化的 message/tool 结构组织方式 | 它的 stream 事件模型(抹平了增量,见探测报告) |

## 为什么 Vercel AI SDK 是流式的首要参考

几个成熟 SDK 里,它是唯一把**流式作为一等设计**的,而流式恰是本项目最关键、genai 最不足的地方。
更重要的是:它用生产级 SDK **验证了我们此前独立推导出的几乎所有设计**(step 边界、审批挂起、tool JSON 累积、逃生舱分层),是很强的设计信心背书。

## Vercel AI SDK v5 stream part ↔ 本项目 DESIGN.md 对照

| DESIGN.md 要求 | Vercel v5 对应 part | 契合 |
|---|---|---|
| StreamEvent 纪律 1:delta 带 index/id 关联到块 | 所有 part 用 **`id`** 关联(`text-start/delta/end` 共享 id) | ✅ |
| StreamEvent 纪律 2:tool JSON 累积后 parse | `tool-input-start` → `tool-input-delta`(原始 `inputTextDelta`)→ `tool-input-available`(parse 好的 `input`) | ✅ 完美 |
| StreamEvent:BlockStart/Delta/Stop | `text-start/delta/end`、`reasoning-start/delta/end` 三段式 | ✅ |
| thinking/reasoning 一等 | `reasoning-start/delta/end` + `reasoning-file` | ✅ |
| agent loop 的 StepBoundary | `start-step` / `finish-step`(完成一次 LLM API call) | ✅ |
| human-in-loop 审批(AwaitingApproval) | `tool-approval-request` / `tool-approval-response` / `tool-output-denied` | ✅ 原生支持 |
| cancel/abort | `abort`(带 `reason`) | ✅ |
| 逃生舱 (A) provider-scoped extras | `custom`(带 `kind`)/ `data-*` | ✅ |
| ToolResponse 非正常结果 | `tool-output-available` / `tool-output-denied` | ✅ |

### Vercel v5 stream part 清单(供起草时查阅)
- **消息控制**:`start`(messageId)、`start-step`、`finish-step`、`finish`、`abort`(reason)、`error`(errorText)
- **文本**:`text-start`(id)、`text-delta`(id, delta)、`text-end`(id)
- **推理**:`reasoning-start`(id)、`reasoning-delta`(id, delta)、`reasoning-end`(id)、`reasoning-file`
- **工具输入**:`tool-input-start`(toolCallId, toolName)、`tool-input-delta`(toolCallId, inputTextDelta)、`tool-input-available`(toolCallId, input)
- **工具执行**:`tool-approval-request`、`tool-approval-response`、`tool-output-available`(output)、`tool-output-denied`
- **资源/自定义**:`file`、`source-url`、`source-document`、`custom`(kind)、`data-*`

## 关键取舍:抄"分类学"不抄"传输层"

Vercel 这套 part 是 **wire/UI streaming protocol**(服务器 → 浏览器的 SSE),偏传输。
我们的 `StreamEvent` 是**库内部的归一化事件**。两者神似但层级不同:

- **抄**:事件分类(taxonomy)、id 关联机制、tool 三段式、step/approval/abort 的存在。
- **不抄**:SSE 编码格式、`data-*`/`source-url` 等纯前端 part。

### 两个直接影响类型设计的决定

1. **块用稳定 `id` 关联,而非位置 `index`。**
   Anthropic 用 `index`(位置),Vercel 用 `id`(稳定标识)。**采用 id 方案**——它对本项目的持久化(见 `conversation-core.md` 的 MessageId 体系)更友好:id 跨序列化/fork 稳定,index 会随投影/截断错位。
   > 适配器职责:Anthropic 的 `content_block_*` 事件带 index,适配器负责把 index 映射成稳定 id 再吐给上层 StreamEvent。

2. **tool input 三段式是 Accumulator 的消费契约。**
   ```
   ToolInputStart     { id, tool_name }          // 对应 BlockStart
   ToolInputDelta     { id, json_fragment }      // 原始 JSON 片段(genai 丢的就是这个)
   ToolInputAvailable { id, parsed_input }       // parse 好的对象
   ```
   Accumulator 消费 start → delta* → available:delta 阶段只累积原始文本,available 阶段(或自己在 stop 时 parse)才产出 `Value`。**绝不在 delta 阶段 parse**(纪律 2)。

## 起草 StreamEvent 的初步形态(基于以上)

```rust
enum StreamEvent {
    MessageStart { message_id: MessageId, role: Role },

    // 块的三段式,统一用 id 关联(text/reasoning/tool 同构)
    BlockStart { id: BlockId, kind: BlockKind },        // BlockKind = Text | Reasoning | ToolInput{name} | ...
    BlockDelta { id: BlockId, delta: Delta },           // Delta = Text(String) | Json(String) | Reasoning(String)
    BlockStop  { id: BlockId },

    ToolInputAvailable { id: BlockId, input: serde_json::Value },  // 累积完成后的 parse 结果

    // agent loop 边界与干预(部分可能只在 agent 层出现,client 层最小集先不含 approval)
    StepFinish { usage: Usage, stop_reason: Normalized<StopReason> },

    Usage(Usage),          // 中途/末尾
    MessageStop { stop_reason: Normalized<StopReason> },
    Error(ClientError),
}
```
> 注:approval / abort / pivot 属 agent 层的 `AgentEvent`(见 DESIGN.md §1.3),不下沉到 client 层 StreamEvent。
> client 层 StreamEvent 只归一化"LLM wire 上真实发生的事件"。

## 下一步
用 Vercel taxonomy + Anthropic 块分类,起草 `StreamEvent` + `ContentBlock` + `Delta` + `Usage` + `Normalized<T>` 的类型骨架(主项目 `src/`)。
