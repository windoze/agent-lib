# genai 能力探测报告

> 目的:实测 `genai` crate(v0.4.4)能否作为 agent-lib 的 LLM API Client 层。
> 方法:用真实 Foundry 代理 endpoint(Anthropic wire format,`databricks-claude-haiku-4-5`)
> 跑非流式 / 流式 / tool call,观察它暴露什么、丢什么。探测代码见 `probes/genai-probe/`。

## 结论速览

**不建议直接把 genai 作为 client 层地基;建议作为"内部可替换后端"或纯参考实现。**
核心原因:genai 的定位是"ergonomics/commonality first, depth secondary",它在多处**主动抹平了 agent-lib 明确需要的底层信息**。下面是实测证据。

## 实测发现

### ✅ 能做到的
1. **基本连通**:非流式 / 流式 / tool call 三条路径都能跑通真实 Foundry endpoint。
2. **流式聚合**:`ChatStreamEvent::{Start, Chunk, ReasoningChunk, ToolCallChunk, End}`,`End` 带 `captured_usage`。
3. **tool call 解析**:流式里 tool call 直接给出解析好的 `fn_name` + `fn_arguments`(已是 `serde_json::Value`)。
4. **usage 结构**:`Usage` 有 `prompt_tokens_details.{cache_creation_tokens, cached_tokens}`、`completion_tokens_details.reasoning_tokens` 字段位。

### ❌ 关键缺陷(对 agent-lib 是硬伤)

**1. tool call 的流式增量被抹平 —— 违反 StreamEvent 纪律 2。**
genai 的 `ToolCallChunk` 直接吐**已累积并 parse 好的完整 `ToolCall`**(实测:一个事件就给出 `args=Object {"city": "Tokyo"}`)。
- 拿不到原始 `input_json_delta` 片段,拿不到累积过程。
- 我们 DESIGN.md 明确要求"tool 参数流式 JSON 累积后再 parse",且要能把 partial JSON 暴露给上层(用于打字机式 UI、或自定义容错)。genai 在它那层就替我们做完了,**信息在源头丢失,无法找回**。

**2. 没有 block-level 的 index / start / stop 事件 —— 违反 StreamEvent 纪律 1。**
genai 事件里**没有 content block 的 `index`**,也没有 BlockStart/BlockStop。
- 并行 tool call 场景下,无法把 delta 归位到正确的块(它靠内部聚合规避了这个问题,但也就不暴露 index)。
- 我们的 Accumulator 设计(`HashMap<index, PartialBlock>`)在 genai 之上无从实现——没有 index 可用。

**3. thinking/reasoning 被降级成纯文本 —— 有损。**
`ReasoningChunk(String)` 和 `captured_reasoning_content: Option<String>` 都是**纯字符串**。
- Anthropic 的 thinking block 带 `signature`(用于多轮校验)等结构化字段,genai 丢弃了。
- 对需要回传 thinking signature 的多轮推理场景不够用。

**4. 自定义 auth 极其笨拙 —— 方言适配能力弱。**
genai 的 Anthropic adapter 用 `AuthData::Key` 时**写死发 `x-api-key`**;而 Foundry 需要 `Authorization: Bearer`。
- 唯一出路是 `AuthData::RequestOverride`,但它**全量替换 url + headers**(all-or-nothing):要改一个 auth header,就得手动重建完整 URL 和所有必要 header(`anthropic-version`、`content-type`…)。
- model 名不匹配内置前缀规则时(如 `databricks-*`),genai **默认猜成 Ollama adapter**,必须用 `ServiceTargetResolver` 显式指定 `AdapterKind`。
- 这些都印证:genai 对"厂商方言/非标准 endpoint"的适配是打补丁式的,而这恰是 agent-lib 的核心关注点。

**5. 无 raw provider 响应逃生舱。**
未发现暴露原始 JSON 响应的入口。我们 DESIGN.md 的逃生舱 (B) flatten extra、(C) `Normalized<T>` 都依赖能看到 provider 原始字段。
- 实测:Foundry 的 Anthropic 响应里含 `cache_creation.{ephemeral_5m,ephemeral_1h}` 等细分字段;OpenAI Response 含 `content_filters`(Azure 特有)。这些方言字段 genai 全部吞掉,拿不到。

**6. stop_reason 归一化粒度不明 / 不可控。**
我们要求 stop_reason 细分到 `tool_use / end_turn / max_tokens / refusal` 且保留 raw(`Normalized<T>`)。genai 未暴露可控的、带 raw 的 stop_reason。

## 对"方案 B(genai 当内部后端)"的裁决

方案 B 的前提是"genai 的输出能喂饱我们的 StreamEvent"。实测证明**喂不饱**:tool 流式增量、block index、thinking 结构、raw 字段——这些我们要的信息在 genai 那层已被抹除,是"无米之炊"(见 DESIGN 讨论里预判的风险)。

因此:
- **genai 不能作为 client 层的内部实现**(方案 B 否决)——它丢的正是我们要的。
- **genai 作为参考实现价值很高**:它的 `ChatMessage`/`MessageContent`/`ContentPart` 分类、`Tool` 定义、多 provider adapter 组织方式,值得直接借鉴。
- **结论倒向方案 C(自研)**:直接用 `async-openai`(OpenAI Response 类型权威)+ 手写 Anthropic adapter,自己掌控 message/stream/usage/逃生舱。这是唯一能满足 DESIGN.md 全部约束的路径。

## 附:实测环境
- endpoint:Foundry 代理,Anthropic wire format,`Authorization: Bearer`,model `databricks-claude-haiku-4-5`
- genai 0.4.4(crates.io 最新 0.6.x/0.7-beta,但核心 stream 事件模型一致,缺陷是设计取向而非版本问题)
- 三条路径实测输出见 git 历史 / 探测代码运行结果
