# OpenAI Chat/Completions 适配器设计

> 本文档记录新增 `openai_chat` 适配器(classic `POST /v1/chat/completions`,含 SSE 流式)的设计。
> 第一期范围:**基础 API + DeepSeek 与 vLLM 方言**;其余厂商以后再说。
> 本设计推翻了 `DESIGN.md` §1.1 原先"不支持 OpenAI chat/completion"的决策(见 §1),落地时须同步修订该节。

## 1. 背景与决策反转

`DESIGN.md:18` 原决策:不支持 chat/completions,理由是"新服务端普遍支持 response 或 anthropic;且 chat API 被各家定制过多,兼容性成本高";并将 DeepSeek、vLLM 归在 Anthropic 协议下(`DESIGN.md:15`)。

反转理由:
- DeepSeek 官方主力接口就是 chat/completions 形态(`https://api.deepseek.com`,兼容 OpenAI chat/completions);vLLM 自建服务的生产入口同样是 `/v1/chat/completions`。这两家的 Anthropic 协议兼容层覆盖不全(尤其思考模式、工具调用的组合)。
- 库内已有两份手搓的、非流式的 DeepSeek chat/completions 客户端(`tests/agent_external_managed_real_e2e.rs:245-442`、`tests/agent_external_real_e2e.rs:144-184`),说明需求真实存在,且重复实现应当收回库里。
- 架构侦察结论:模型层(`Usage` 反序列化、`ContentBlock::Thinking`、extras 逃生舱)已经消化了 chat/completions 的大半差异,新增成本低于原决策时的估计。

落地时 `DESIGN.md` §1.1 的协议清单与"不支持"一节须同步修订。

## 2. 目标与非目标

### 2.1 第一期目标

- 新增 `src/adapter/openai_chat/` 适配器 `OpenAiChatAdapter`,实现 `LlmClient`(`src/client/mod.rs:29`),非流式 + SSE 流式。
- 方言:DeepSeek(官方 API,含思考模式 `reasoning_content`)与 vLLM(自建 OpenAI 兼容服务)。**一个适配器覆盖三家**,不为方言建新类型(见 §5)。
- 测试三层齐备(模块内单测 / transport 测试 / `#[ignore]` 真实端点测试),并纳入跨 provider 归一化矩阵。

### 2.2 第一期非目标

- `logprobs`(归一化模型无处安放,只能进 extra)。
- `n > 1` 多 choice(`Response` 只建模一条 message,取 `choices[0]`)。
- audio content、`ChatRequest` 采样参数扩充(`top_p`/`stop`/`seed`/`response_format`/`tool_choice`/`max_completion_tokens` 等一律走 `provider_extras`)。
- quirk 配置体系(`DESIGN.md:21` 设想的方言开关);第一期用"统一默认行为 + extras 兜底",装不下再加。
- OpenAI 官方账号侧的新字段适配。

## 3. 可复用基础(侦察结论)

架构对新增适配器有利,以下内容**原样复用,零改动**:

- `src/adapter/common/`(`pub(crate)`,全部协议无关):
  - `default_http_client`、`execute_json_response`、`execute_sse_response`、`map_transport_error`(`common/http.rs`);
  - `endpoint_url`(proxy 前缀安全的路径拼接,本适配器用 `&["chat", "completions"]`)、`endpoint_headers`(`AuthScheme::{Bearer,Header,None}` + extra headers,`common/request.rs`);
  - `SseNormalizer` trait + `normalize_sse`(字节流 → SSE frame → `StreamEvent`,错误终止、EOF 处理,`common/sse.rs`);
  - `insert_preserving_collision`(`common/json.rs`);
  - 错误分类 `ClientError::from_http_response`(`src/client/error.rs:61-105`),429/Retry-After、408/504、401/403、context-length 与 content-filter 的 body 标记**已覆盖 OpenAI 拼写**。
- 归一化模型:
  - `Usage` 的自定义 `Deserialize` **已认识** `prompt_tokens`/`completion_tokens`/`total_tokens`、`prompt_tokens_details.cached_tokens`、`completion_tokens_details.reasoning_tokens`(`src/model/usage.rs:83-138`)——usage 零改动。
  - `ContentBlock::Thinking { text, signature: None }` 是 `reasoning_content` 的天然归宿;`StreamEvent` 的 `BlockKind::Reasoning` + `Delta::Reasoning` 同理(`src/stream/mod.rs:50-97`)。
  - extras 逃生舱:请求侧 `ProviderExtras::merge_into` 最后合并(`src/model/extras.rs:42-60`),响应侧 `Response.extra` / `Usage.extra` / `ContentBlock::extra` 的 `flatten` 惯例。
- 模板:`src/adapter/openai_resp/` 的模块骨架、测试分层、fixture 惯例整套照抄。
- DeepSeek 方言参照:两个 e2e 测试里的手搓客户端(上文 §1),含 `finish_reason` 映射、system 渲染、bearer 直连形态。

## 4. 适配器设计

### 4.1 模块骨架

沿用 `openai_resp` 布局,全部 wire 类型 crate-private,不加 feature gate(LLM wire 适配器按惯例全量编译,`src/adapter/mod.rs:1-5`):

```
src/adapter/openai_chat/
├── mod.rs            # OpenAiChatAdapter 结构体 + LlmClient 实现 + 构造函数
├── request.rs        # build_request:URL/headers/body 序列化 + extras 合并
├── request/input.rs  # Message/Tool → messages/tools 数组映射
├── request/tests.rs
├── response.rs       # parse_response + 非流式 chat()
├── response/convert.rs  # choices[0].message → ContentBlock,finish_reason 映射
├── response/tests/{…}   # + fixtures/*.json
└── stream/
    ├── mod.rs        # chat_stream() 入口
    ├── decoder.rs    # SseNormalizer 绑定(约 30 行,照 openai_resp/stream/decoder.rs)
    ├── wire.rs       # chunk 类型视图 + [DONE] 哨兵
    ├── normalizer.rs # chunk → StreamEvent 状态机
    └── tests/{…}     # + fixtures/*.sse
```

适配器结构同 `OpenAiRespAdapter`:`{ http_client: reqwest::Client, endpoint: EndpointConfig }`,`new()` / `with_http_client()`,`Clone + Debug`(密钥经 `EndpointConfig` 的脱敏 `Debug`)。`capability()` 返回新增的 `OPENAI_CHAT_DEFAULT_CAPABILITY`(`src/client/capability.rs`,比照 :77)。

`chat()` 拒绝 `stream=true`、`chat_stream()` 拒绝 `stream=false`(既有契约,`openai_resp/response.rs:53-57` 同款)。

### 4.2 请求侧映射

| 归一化输入 | chat/completions wire |
|---|---|
| `ChatRequest.system` | 首条 `{"role":"system", "content": …}` 消息(system 不进 `messages` 的既有约定不变) |
| user/assistant 文本 | `{"role", "content"}` |
| assistant 的 `ContentBlock::Thinking` | 该消息的 `reasoning_content` 字段(**原样回放**,规则见 §5.1) |
| assistant 的 `ContentBlock::ToolUse` | `tool_calls: [{id, type:"function", function:{name, arguments}}]`,arguments 为序列化后的 JSON **字符串** |
| `ContentBlock::ToolResult` | 独立 `{"role":"tool", "tool_call_id", "content"}` 消息;`Vec<ContentBlock>` 扁平化为文本(图像结果有损,第一期接受);非 `Ok` 状态拼入文本(比照 anthropic 的 `is_error` 降级,`src/adapter/anthropic/request.rs:204-209`) |
| `ChatRequest.tools` | `tools: [{type:"function", function:{name, description, parameters}}]`(注意比 Responses 多一层 `function` 嵌套) |
| `stream=true` | 自动注入 `stream_options: {"include_usage": true}`,否则流式没有 usage |
| `provider_extras` | `ProviderExtras::merge_into` 最后合并,可覆盖任何 body 字段;mismatch 报错(同既有适配器) |

`max_tokens` 在 `ChatRequest` 中非可选,直接对应;DeepSeek 思考模式下 `temperature` 等采样参数被服务端静默忽略(§5.1),照传无害。

### 4.3 响应侧映射(非流式)

- 校验 `object == "chat.completion"`(比照 `openai_resp/response.rs:69` 的 `object=="response"` 校验),取 `choices[0]`。
- `message.content` → `ContentBlock::Text`;`message.reasoning_content` → `ContentBlock::Thinking { text, signature: None }`;`message.tool_calls[]` → `ContentBlock::ToolUse`(arguments 字符串解析为 `Value`,解析失败保留原文进 extra)。
- `finish_reason` → `Normalized<StopReason>`,适配器本地映射(惯例见 `openai_resp/response/convert.rs:306`):

| wire | 归一化 |
|---|---|
| `stop` | `EndTurn` |
| `length` | `MaxTokens` |
| `tool_calls` | `ToolUse` |
| `content_filter` | `Refusal` |
| 其它/缺失 | `Normalized::Other` |

- 未建模字段(`created`、`system_fingerprint`、`logprobs` 等)进 `Response.extra`。

### 4.4 流式侧

chunk 形态:`choices[0].delta = {role?, content?, reasoning_content?, tool_calls?}`,`finish_reason` 在末个非空 chunk;usage 在 `include_usage` 后由**空 `choices` 的独立 chunk**携带。

与两个既有适配器的关键差异,normalizer 必须自己处理:

1. **`data: [DONE]` 哨兵**:非 JSON,JSON 解析前特判,直接终止(`SseNormalizer` 的 `is_terminal`/`incomplete_error` 已支持适配器自控终止)。SSE `event:` 字段恒为 `message`,既有的 event/type 一致性检查自然通过。
2. **工具调用增量**:`tool_calls[]` 按 `index` 键控,`id`/`function.name` 只在首 chunk 出现,`function.arguments` 是字符串片段 → `BlockStart(ToolInput{tool_name, tool_call_id})` + `Delta::Json` + `BlockStop`,**绝不中途解析 JSON**。`BlockId` 用位置派生稳定 id(先例 `anthropic-block-{index}`,`src/adapter/anthropic/stream/normalizer.rs:423-424`)。
3. **`delta.reasoning_content`** → `BlockKind::Reasoning` + `Delta::Reasoning`(无 signature)。
4. **终态**:无 Responses 那样的终态快照事件;由末 chunk 的 `finish_reason` 发 `MessageStop`,由 usage chunk 发单段加性 `Usage`。EOF 而无 `[DONE]` → `incomplete_error`。

## 5. 方言设计

### 5.1 DeepSeek(以[官方思考模式文档](https://api-docs.deepseek.com/zh-cn/guides/thinking_mode/)为准)

- 思考模式开关 `{"thinking": {"type": "enabled/disabled"}}`(默认 enabled)、`reasoning_effort: high/max` —— 均经 `provider_extras` 传递,不进 `ChatRequest`。
- 思考模式下 `temperature`/`top_p`/`presence_penalty`/`frequency_penalty` 被静默忽略(不报错)。
- **`reasoning_content` 回传规则(条件性,本设计已按此修正)**:
  - 两轮 `user` 消息之间**无工具调用**:中间 assistant 的 `reasoning_content` 无需参与拼接;传了被忽略(不报错)。
  - **有工具调用**:`reasoning_content` **必须在后续所有 user 交互轮次中完整回传**,否则 API 返回 400(`The reasoning_content in the thinking mode must be passed back to the API`)。
  - 推论:**统一原样回放是永远安全的默认策略**(不需要时被忽略,需要时必须在场),实现也最简单——请求侧把 `Thinking` 块序列化为 `reasoning_content` 即可,无须判断该轮是否有工具调用。官方样例的做法(`messages.append(response.choices[0].message)` 整条回放)与此等价。
- base_url `https://api.deepseek.com`,Bearer 认证。

### 5.2 vLLM

- base_url 自建 `http://host:port/v1`,Bearer 或 `AuthScheme::None`。
- `reasoning_content` 取决于启动参数 `--reasoning-parser`:开启时出现在 message/delta 中,未开启时思考内容混在 `content` 里(适配器无法也无需区分)。
- **待验证项**:消息里携带 `reasoning_content` 的回放是否被所有目标版本接受(较新版本的消息模型已内置该字段;老的或严格校验的端点可能拒绝未知字段)。第一期默认回放,实测确认;若确有端点拒绝,再引入 quirk 开关(此时才需要 `DESIGN.md:21` 的方言机制)。

### 5.3 统一策略

一个 `OpenAiChatAdapter` 覆盖三家,差异仅体现在:

1. `EndpointConfig`(base_url / auth)不同;
2. `reasoning_content` 统一回放(§5.1 推论);
3. 其余差异全部经 `provider_extras` 兜底。

第一期**不建** quirk 配置类型;§5.2 的待验证项是它唯一的候选触发器。

## 6. 库内触点(适配器之外的小改动)

| 位置 | 改动 |
|---|---|
| `src/model/extras.rs:14` | `ProviderId` 新增 `OpenAiChat` 变体(enum 本就 `#[non_exhaustive]` 并注明可扩展) |
| `src/client/capability.rs` | 新增 `OPENAI_CHAT_DEFAULT_CAPABILITY` 静态(text+image 输入、streaming、tool_calling、parallel_tool_calls、reasoning;stop_reasons 含 `StopSequence`,chat/completions 可用 `stop` 参数表达) |
| `src/facade/chat.rs:391` | `client_for_provider` 加分支 |
| `src/facade/config.rs` | `ProviderConfig` 新增构造/env 分支;注意现有 `openai_from_env`(:109-117)是 Azure 风格(`api-key` 头 + `api-version` query),**不适用**,需要 Bearer 风格构造器(如 `openai_chat_from_env`,读 `OPENAI_CHAT_BASE_URL`/`OPENAI_CHAT_API_KEY` 或 DeepSeek 的既有 env 约定) |
| `src/lib.rs:16-17` | 协议列表文档 |
| `src/adapter/mod.rs` | `pub mod openai_chat;` |

## 7. 测试计划

照抄既有适配器的三层惯例,**不用 agent-testkit**(它明确不 mock provider 传输层):

1. **模块内单测**
   - `request/tests.rs`:断言 method、URL path+query、headers、**完整请求 body JSON**(`json!` 精确比对)。关键用例:system 渲染、tools 嵌套形状、tool_result 扁平化、`stream_options` 注入、extras 合并覆盖、**带工具调用的多轮历史中 assistant 消息完整携带 `reasoning_content` + `tool_calls`**(§5.1 规则)。
   - `response/tests/` + `fixtures/*.json`:文本、工具调用、含 `reasoning_content` 三种响应;`finish_reason` 全表;未知字段落 `extra`。
   - `stream/tests/` + `fixtures/*.sse`(脱敏录屏,`include_str!` 加载):纯文本流、工具调用流(多 `index` 并行)、`reasoning_content` 流、`include_usage` 终态 chunk。fixture 用**不规则字节分块**(尺寸 `[1,2,7,3,19,5,11]`)喂 normalizer;断言精确 `StreamEvent` 序列;再用 `Accumulator` 折叠后与非流式 `parse_response` 结果对照。`[DONE]` 哨兵、EOF 无 `[DONE]` 的错误路径单独覆盖。
2. **transport 测试**:一次性 `TcpListener` 本地服务器,覆盖状态码/内容类型/错误映射(模板 `openai_resp/stream/tests/transport.rs:17-40`)。
3. **`#[ignore]` 真实端点测试** `tests/integration_openai_chat.rs`:env 缺省跳过(Option 模式,模板 `tests/integration_openai_resp.rs:24-54`);两套配置 —— DeepSeek(`DEEPSEEK_API_KEY`,可选 `DEEPSEEK_BASE_URL`/`DEEPSEEK_MODEL`)与 vLLM(`VLLM_BASE_URL`,可选 `VLLM_API_KEY`/`VLLM_MODEL`)。DeepSeek 用例须含:思考模式多轮 + 工具调用(验证 400 规则)。
4. **归一化矩阵**:`tests/normalization/config.rs:20` 注册新 `Provider` 分支。
5. **可选收尾**:把两个 e2e 里的手搓 DeepSeek 客户端换成本适配器,删除重复代码。

## 8. 文档同步清单

- `DESIGN.md` §1.1:修订协议清单与"不支持"决策(**决策反转,必须做**)。
- `docs/capability-matrix.md`:协议级默认值表加 chat/completions 列 + DeepSeek/vLLM 实测一节。
- `README.md`:provider 选择、示例与 ignored 测试命令。
- `AGENTS.md`:`src/` 布局、新增 env 变量。
- `docs/client-layer-references.md`:参考分工总表加一行(可参考 `async-openai` 的 chat 模块)。

## 9. 规模估算

参照 `openai_resp`(实现约 2000 行 + 测试约 1400 行);chat/completions 协议更简单(无 item/part 层级、无终态快照、usage 模型现成),估计**实现 1200–1500 行 + 测试 800–1000 行**,约 80% 工作量在 `src/adapter/openai_chat/` 内部,其余为 §6 的几行级触点。
