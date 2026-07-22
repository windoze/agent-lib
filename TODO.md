# TODO：openai_chat 适配器任务单（chat/completions + DeepSeek/vLLM 方言）

本任务单对应 [PLAN.md](PLAN.md) 和 [docs/openai-chat-api.md](docs/openai-chat-api.md)
（唯一设计输入，下称「设计文档」，按 §号引用）。
旧任务单已归档（最近一轮）：[docs/archive/2026-07-20-mag-gaps/TODO.md](docs/archive/2026-07-20-mag-gaps/TODO.md)。

执行规则：

- 严格按编号顺序实现，除非当前任务明确要求先补充前置信息。
- 每个标题中的 `[TODO]` 表示尚未完成。完成后把 `[TODO]` 改成 `[DONE]`，并在任务下方追加
  "完成记录"，写明关键实现决策、验证结果和（如有）breaking change。
- 不要跳过每个 milestone 末尾的 review 任务。
- 修改行为时同步修改拥有该行为的文档；M5 集中收口文档，但 M1–M4 触及既有文档口径的
  改动随任务同步。
- 默认测试必须离线可跑，不依赖真实 provider、网络或用户本机配置。真实端点测试一律
  `#[ignore]`，缺环境干净跳过。
- 行号引用自评估时点（2026-07-21），随后续修复可能漂移，以符号名为准。
- 1.0 前 API 稳定性不作为约束，但优先向后兼容形状（新增类型/变体/静态），breaking
  change 必须在完成记录显式注明。
- 模板对照：`src/adapter/openai_resp/` 整套（模块骨架、测试分层、fixture 惯例）照抄；
  `src/adapter/common/` 的 helper 全部原样复用，不在新适配器里重写。

全量门禁命令（每个 milestone review 必跑）：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

---

## M1：适配器骨架与请求侧

### M1-1 [DONE] 库内前置触点：ProviderId 变体 + capability 静态 + 模块注册

上下文：

- `ProviderId`（`src/model/extras.rs:14`）是 `#[non_exhaustive]` enum，现有变体含
  `Anthropic`、`OpenAiResp`；`ProviderExtras::merge_into`（`src/model/extras.rs:42-60`）
  用它做 provider 匹配。
- `OPENAI_RESP_DEFAULT_CAPABILITY`（`src/client/capability.rs:77`）是既有静态样板：
  `LazyLock<Capability>`，`max_context_tokens: None`（模型相关，克隆后再覆盖），文件内
  `set()` helper 构造 `BTreeSet`，底部 `mod tests` 有序列化稳定性测试。
- `src/adapter/mod.rs` 当前为 `pub mod anthropic; mod common; pub mod openai_resp;`；
  LLM wire 适配器按惯例全量编译，**不加 feature gate**（设计文档 §4.1）。
- chat/completions 的能力轮廓（设计文档 §6）：text+image 输入、streaming、tool_calling、
  parallel_tool_calls、reasoning；stop_reasons 含 `StopSequence`（chat/completions 可用
  `stop` 参数表达）。与 Responses 的差异：无 audio/file 输入输出、无 prompt_caching
  声明、无 structured_output 声明。

实现要求：

- `ProviderId` 新增 `OpenAiChat` 变体（serde 命名与现有变体风格一致）；检查
  `src/model/extras.rs` 内对 `ProviderId` 的 match/测试是否需要连带更新。
- `src/client/capability.rs` 新增 `pub static OPENAI_CHAT_DEFAULT_CAPABILITY: LazyLock<Capability>`：
  `max_context_tokens: None`；`input_modalities: {Text, Image}`；
  `output_modalities: {Text}`；`streaming / tool_calling / parallel_tool_calls /
  reasoning = true`；`prompt_caching / structured_output = false`；`stop_reasons:
  {ToolUse, EndTurn, MaxTokens, StopSequence, Refusal}`。rustdoc 注明「协议级默认值，
  context limit 模型相关，克隆后覆盖」（比照既有静态）。
- `src/adapter/mod.rs` 加 `pub mod openai_chat;`（占位空模块即可，M1-2 填充），并保持
  模块文档注释的协议清单同步。
- capability 静态加进 `capability.rs` 底部既有测试的覆盖（序列化/集合内容断言，照
  既有测试形状）。

验证条件：

- `cargo test -p agent-lib --lib client::capability` 与
  `cargo test -p agent-lib --lib model::extras` 通过。
- `cargo clippy --all-targets -- -D warnings` 通过。

完成记录（2026-07-23）：

- `ProviderId::OpenAiChat` 新增（`src/model/extras.rs`），serde `rename_all="snake_case"` →
  `open_ai_chat`，与既有 `anthropic`/`open_ai_resp` 一致。`provider_extras_round_trip_for_every_provider_id`
  测试表追加 `(OpenAiChat, "open_ai_chat")`，钉住新变体的序列化往返。
- `OPENAI_CHAT_DEFAULT_CAPABILITY`（`src/client/capability.rs`，比照 `OPENAI_RESP_DEFAULT_CAPABILITY`
  的 full struct literal）落地：`max_context_tokens: None`；
  `input_modalities: {Text, Image}`；`output_modalities: {Text}`；
  `streaming/tool_calling/parallel_tool_calls/reasoning = true`；
  `prompt_caching/structured_output = false`（显式，与既有静态的关键差异）；
  `stop_reasons: {ToolUse, EndTurn, MaxTokens, StopSequence, Refusal}`（含 `StopSequence`——chat/completions
  支持 `stop` 参数；含 `Refusal`——`content_filter`）。rustdoc 注明协议级默认值、context 模型相关、克隆后覆盖。
  新增测试 `openai_chat_default_describes_protocol_capabilities` 逐字段断言（含 `prompt_caching/structured_output=false`
  与完整 `stop_reasons` 集合），并补进 tests 的 `use` 导入。
- `src/adapter/mod.rs` 注册 `pub mod openai_chat;`（按字母序排在 `common` 与 `openai_resp` 之间）；模块文档注释
  无协议清单，无需同步列表。
- `src/client/mod.rs` 将 `OPENAI_CHAT_DEFAULT_CAPABILITY` 一并 `pub use` 出去，供适配器/集成引用。
- **不可避免的编译耦合（已最小正确处理，非 workaround）**：`ProviderId` 虽 `#[non_exhaustive]`，定义 crate 内仍要求
  exhaustive match。新增变体让两处 facade exhaustive match 编译断裂，而 M1-1 的验证条件要求 `cargo clippy --all-targets
  -- -D warnings` 全绿，故这两处必须在本任务内收口：
  - `src/facade/config.rs` `ProviderConfigBuilder::build()` 加 `ProviderId::OpenAiChat => openai_chat_endpoint(base_url, api_key)`
    分支，并新增 `openai_chat_endpoint()` helper（Bearer 直连、无 `api-key` 头/无 `api-version` query，设计文档 §5.3/§6 的
    正确传输形态）。env 读取构造器 `openai_chat_from_env()`、vLLM 无 auth 的 None 路径仍留给 **M4-1**。
  - `src/facade/chat.rs` `client_for_provider()` 加 `ProviderId::OpenAiChat => Arc::new(OpenAiChatAdapter::new(endpoint))`
    分支。因适配器结构体要到 **M1-2** 才落地，为此在 `src/adapter/openai_chat/mod.rs` 放**最小编译桩**：结构体形状
    `{ http_client, endpoint }` + `#[derive(Clone, Debug)]`（Debug 经 `EndpointConfig` 脱敏，与 `openai_resp` 同款）+
    `new(endpoint)` 构造函数 + `#[async_trait] impl LlmClient`，其中 `capability()` 已返回最终静态；`chat()`/`chat_stream()`
    返回 `ClientError::Other("…implemented in M1-2")` 占位（非 `unimplemented!` panic，避免潜伏崩溃）。
- **显式留给后续任务**（避免 M1-2/M4-1 重复或漏做）：
  - M1-2：补 `with_http_client(endpoint, http_client)` 构造函数 + `endpoint()` 访问器；`chat()`/`chat_stream()` 的
    stream 标志互斥校验（本任务钉死的校验逻辑/错误类型）并替换占位 body 为委托；建 §4.1 子模块空壳（request/response/stream）；
    补模块级 rustdoc；加 stream=true/chat_stream stream=false 报错单测。
  - M4-1：`openai_chat_from_env()` env 读取构造器、`src/lib.rs:13-14` 协议清单文档加 chat/completions、facade rustdoc 示例。
- 验证结果（全绿）：
  - `cargo fmt --all`（无 diff）；
  - `cargo clippy --all-targets -- -D warnings`；
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  - `cargo test -p agent-lib --lib client::capability`（6 通过，含新增 openai_chat 用例）；
  - `cargo test -p agent-lib --lib model::extras`（4 通过，round-trip 已含 OpenAiChat）；
  - `cargo test --all --all-targets`（全绿，含 facade/集成测试，无回归）；
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- 无 breaking change：`#[non_exhaustive]` enum 新增变体 + 新增 `pub static` + 新增 `pub mod`，均为向后兼容的形状新增。

### M1-2 [TODO] 适配器骨架：OpenAiChatAdapter 结构体与 LlmClient 契约

上下文：

- 模板 `src/adapter/openai_resp/mod.rs`（112 行）：`OpenAiRespAdapter { http_client:
  reqwest::Client, endpoint: EndpointConfig }`，`new(endpoint)` / `with_http_client(endpoint,
  http_client)`，`Clone + Debug`（密钥经 `EndpointConfig` 的脱敏 `Debug`），
  `#[async_trait] impl LlmClient`。
- `LlmClient` trait（`src/client/mod.rs:29`）：`capability()`、`chat()`、`chat_stream()`。
- stream 标志互斥校验先例（`src/adapter/openai_resp/response.rs:53-57`）：`chat()` 拒绝
  `stream=true`，`chat_stream()` 拒绝 `stream=false`，返回 `ClientError` 的配置类错误。
- 复用 helper：`default_http_client`（`src/adapter/common/http.rs`）作为 `new()` 的默认
  client；`endpoint_url(&["chat", "completions"])` 与 `endpoint_headers`
  （`src/adapter/common/request.rs`）留给 M1-3 使用，本任务只需建好结构。
- 目录骨架（设计文档 §4.1）：`mod.rs`、`request.rs`、`request/input.rs`、`response.rs`、
  `response/convert.rs`、`stream/{mod.rs,decoder.rs,wire.rs,normalizer.rs}`；wire 类型
  全部 crate-private。本任务只建 `mod.rs` 与空壳子模块，其余任务填充。

实现要求：

- 新建 `src/adapter/openai_chat/mod.rs`：`OpenAiChatAdapter` 结构体、两个构造函数、
  `Clone + Debug`、`#[async_trait] impl LlmClient`，其中 `capability()` 返回
  `OPENAI_CHAT_DEFAULT_CAPABILITY.clone()`；`chat()`/`chat_stream()` 先做 stream 标志
  互斥校验，主体可暂 `unimplemented!` 或直接委托给空壳 `response.rs`/`stream/mod.rs`
  （下一任务填充），但校验逻辑与错误类型本任务钉死。
- 子模块按 §4.1 骨架建空壳（`pub(crate)` 或私有，随 `openai_resp` 惯例），保证
  `cargo check` 通过。
- 模块级 rustdoc：一句话说明覆盖 classic `POST /v1/chat/completions`（含 SSE 流式），
  方言策略指向设计文档 §5（统一回放 `reasoning_content` + extras 兜底，不建 quirk 类型）。

验证条件：

- 单元测试（`mod.rs` 内或 `request/tests.rs` 暂挂）：`chat()` 收到 `stream=true` 报错、
  `chat_stream()` 收到 `stream=false` 报错，错误类型/消息与 `openai_resp` 同款。
- `cargo test -p agent-lib --lib adapter::openai_chat` 通过；
  `cargo clippy --all-targets -- -D warnings` 通过。

完成记录（2026-07-23）：

- `src/adapter/openai_chat/mod.rs`：替换 M1-1 的最小编译桩为完整骨架，结构体形状与
  `OpenAiRespAdapter` 完全一致（`{ http_client: reqwest::Client, endpoint: EndpointConfig }`，
  `#[derive(Clone, Debug)]`，密钥经 `EndpointConfig` 脱敏）。两个构造函数 `new(endpoint)`
  （默认 client 走 `common::default_http_client`）+ `with_http_client(endpoint, http_client)` +
  `endpoint()` 访问器齐备；rustdoc 沿用 openai_resp 的超时口径。
- `LlmClient` impl：`capability() → &OPENAI_CHAT_DEFAULT_CAPABILITY`（trait 返回引用；
  任务文案「`.clone()`」为口语化措辞，trait 签名 `&Capability` 决定必须返回引用，与 M1-1 桩一致）。
  `chat()`/`chat_stream()` 按模板委托给子模块 inherent 方法（`OpenAiChatAdapter::chat` /
  `OpenAiChatAdapter::chat_stream`，inherent 优先于 trait 同名方法，UFCS 无歧义，openai_resp 同款）。
- stream 互斥校验（本任务钉死，与 openai_resp 同款）：
  - `response.rs::chat()` 首句 `if request.stream { Err(invalid_response("…stream to be false")) }`；
  - `stream/mod.rs::chat_stream()` 首句 `if !request.stream { Err(invalid_stream("…stream to be true")) }`；
  - 两者均为 `ClientError::Protocol`，helper 文案 `invalid OpenAI Chat/Completions {response,stream}: …`。
- 校验通过后的主体为桩：返回 `ClientError::Other("…implemented in M1-3/M2-1 / M1-3/M3")` 占位，
  **非 `unimplemented!` panic**（延续 M1-1「避免潜伏崩溃」原则）。build_request（M1-3）/
  transport+parse（M2-1）/ SSE 解码+归一化（M3-1/M3-2）后续填充。
- §4.1 子模块空壳已建：`mod.rs`（全量）+ `request.rs`（纯 rustdoc 壳，M1-3 填 build_request）+
  `response.rs`（chat 桩 + `pub(super) fn invalid_response`）+ `stream/mod.rs`（chat_stream 桩 +
  `fn invalid_stream`）。`request/input.rs`、`response/convert.rs`、`stream/{decoder,wire,normalizer}.rs`
  不在本任务创建——它们只在各自任务（M1-3/M2-1/M3-1/M3-2）被父模块 `mod` 声明引用时才落地，
  避免产生无引用的 dead 文件。
- 模块级 rustdoc：一句话覆盖 classic `POST /v1/chat/completions`（含 SSE），方言策略指向
  设计文档 §5（统一回放 `reasoning_content` + extras 兜底，不建 quirk 类型）。
- 过渡性 `#[allow(dead_code)]`：`http_client` 字段在 M1-2 桩里尚未被读取（M1-3 `build_request`/
  transport 才读），加逐字段 `#[allow(dead_code)]` + 注释标注接线任务；`endpoint` 已被 `endpoint()`
  读取无需标注。M1-3 接线后该 allow 自然失效、届时移除。
- 验证结果（全绿）：
  - `cargo fmt --all`（无 diff，仅 fmt 重排 stream/response 的 import 顺序与 format! 换行）；
  - `cargo clippy --all-targets -- -D warnings`；
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  - `cargo test -p agent-lib --lib adapter::openai_chat`（3 通过：Debug 脱敏 + chat 拒 stream=true +
    chat_stream 拒 stream=false，错误类型 `Protocol`、文案含 "stream to be false/true"，与 openai_resp 同款）；
  - `cargo test -p agent-lib --lib`（1065 全绿，+3 新增，facade 依赖的桩替换无回归）。
- 无 breaking change：公开 API（结构体形状、`new`、`capability`）与 M1-1 桩完全一致；新增 `with_http_client`/
  `endpoint()` 为向后兼容的形状新增。

### M1-3 [TODO] 请求侧映射：build_request + Message/Tool → messages/tools

上下文：

- 模板 `src/adapter/openai_resp/request.rs`（108 行，URL/headers/body 组装）与
  `request/input.rs`（362 行，输入映射）；chat/completions 比 Responses 少 item 层级，
  映射更扁。
- 复用：`endpoint_url(&endpoint.base_url, &["chat", "completions"])`（proxy 前缀安全）、
  `endpoint_headers(&endpoint)`（`AuthScheme::{Bearer,Header,None}` + extra headers）、
  `insert_preserving_collision`（`src/adapter/common/json.rs`）、
  `ProviderExtras::merge_into` 最后合并、mismatch 报错（设计文档 §4.2 末行）。
- 映射表（设计文档 §4.2，逐行实现）：
  - `ChatRequest.system` → 首条 `{"role":"system","content":…}`（system 不进 `messages`
    的既有约定不变）；
  - user/assistant 文本 → `{"role","content"}`；
  - assistant 的 `ContentBlock::Thinking` → 该消息的 `reasoning_content` 字段，**无条件
    原样回放**（§5.1 推论：不需要时被忽略，需要时必须在场，统一回放永远安全）；
  - assistant 的 `ContentBlock::ToolUse` → `tool_calls: [{id, type:"function",
    function:{name, arguments}}]`，`arguments` 是序列化后的 JSON **字符串**；
  - `ContentBlock::ToolResult` → 独立 `{"role":"tool","tool_call_id","content"}` 消息；
    `Vec<ContentBlock>` 扁平化为文本（图像结果有损，第一期接受）；非 `Ok` 状态拼入文本
    （比照 anthropic `is_error` 降级，`src/adapter/anthropic/request.rs:204-209`）；
  - `ChatRequest.tools` → `tools: [{type:"function", function:{name, description,
    parameters}}]`（比 Responses 多一层 `function` 嵌套）；
  - `stream=true` → 自动注入 `stream_options: {"include_usage": true}`，否则流式没有
    usage；
  - `max_tokens` 在 `ChatRequest` 中非可选，直接对应；采样参数扩充一律走
    `provider_extras`（§2.2）。
- `ChatRequest`/`Message`/`ContentBlock`/`Tool` 定义见 `src/model/`，字段名以代码为准。

实现要求：

- `request.rs`：`build_request`（或照 `openai_resp` 命名）输出 method + URL + headers +
  序列化 body；`POST`、JSON content-type、auth 由 `endpoint_headers` 处理。
- `request/input.rs`：上表逐条的纯函数映射；assistant 多 block 消息按「一条 assistant
  消息携带 `content` + 可选 `reasoning_content` + 可选 `tool_calls`」聚合（chat/
  completions 的消息模型是一条 message 上挂这些字段，不是多条）。
- extras 合并在所有映射完成之后执行，允许覆盖任何 body 字段；provider mismatch 报错
  与既有适配器一致。
- wire 类型（messages/tools/tool_calls 的 serde 视图）crate-private，放 `request.rs` 或
  `request/input.rs` 内，不泄露到 `adapter::openai_chat` 模块外。

验证条件：

- `request/tests.rs`（模板 `openai_resp/request/tests.rs`，599 行）用 `json!` 精确比对
  **完整请求 body**，并断言 method、URL path+query、headers。关键用例逐个覆盖：
  1. system 渲染为首条 system 消息；
  2. tools 的 `function` 嵌套形状；
  3. tool_result 扁平化 + 非 `Ok` 状态拼入文本；
  4. `stream=true` 注入 `stream_options.include_usage`；
  5. extras 合并覆盖既有字段 + mismatch 报错；
  6. **带工具调用的多轮历史中，assistant 消息完整携带 `reasoning_content` +
     `tool_calls`**（§5.1 规则，DeepSeek 400 防线）。
- `cargo test -p agent-lib --lib adapter::openai_chat` 通过。

### M1-R [TODO] M1 review：骨架与请求侧正确性核对

目的：独立核对 M1 的正确性与完整性，不新增功能。

核对清单：

- 设计文档 §4.2 映射表逐行对照实现与测试，确认无遗漏行（尤其 `reasoning_content` 无条件
  回放与 `stream_options` 注入）。
- `ProviderId::OpenAiChat`、`OPENAI_CHAT_DEFAULT_CAPABILITY`、模块注册三处触点的形状与
  既有先例一致；capability 各字段与设计文档 §6 描述一致。
- wire 类型无泄漏（`pub(crate)`/私有）；`Debug` 不泄露密钥。
- 请求单测覆盖 §7.1 列出的全部关键用例，断言是 `json!` 精确比对而非字段抽查。
- 跑全量门禁命令（见文件头），全部通过。

产出：在本任务下方追加 review 记录（核对结论 + 门禁输出摘要 + 发现的问题及处置）。

---

## M2：非流式响应侧

### M2-1 [TODO] 响应解析：wire 类型 + parse_response + finish_reason 映射 + chat()

上下文：

- 模板 `src/adapter/openai_resp/response.rs`（142 行，含 `object=="response"` 校验
  :69）与 `response/convert.rs`（368 行，含 `finish_reason` 映射惯例 :306）。
- 复用：`execute_json_response`、`map_transport_error`（`src/adapter/common/http.rs`）；
  `ClientError::from_http_response`（`src/client/error.rs:61-105`）的错误分类**已覆盖
  OpenAI 拼写**（429/Retry-After、408/504、401/403、context-length、content-filter），
  零改动。
- `Usage` 的自定义 `Deserialize`（`src/model/usage.rs:83-138`）**已认识**
  `prompt_tokens`/`completion_tokens`/`total_tokens`、`prompt_tokens_details.cached_tokens`、
  `completion_tokens_details.reasoning_tokens`——usage 零改动，直接反序列化。
- 归一化落点：`message.content` → `ContentBlock::Text`；`message.reasoning_content` →
  `ContentBlock::Thinking { text, signature: None }`；`message.tool_calls[]` →
  `ContentBlock::ToolUse`（`function.arguments` 字符串解析为 `Value`，解析失败保留原文
  进 extra，设计文档 §4.3）。
- `finish_reason` 映射表（设计文档 §4.3，适配器本地函数）：`stop`→`EndTurn`、
  `length`→`MaxTokens`、`tool_calls`→`ToolUse`、`content_filter`→`Refusal`、其它/缺失→
  `Normalized::Other`。
- 未建模字段（`created`、`system_fingerprint`、`logprobs` 等）进 `Response.extra`
  （响应侧 flatten 惯例，`insert_preserving_collision` 可用于冲突保护）。

实现要求：

- `response.rs`：chat/completions 响应的 crate-private wire 类型（`object`、`choices[]`
  的 `message`/`finish_reason`、`usage`），`parse_response` 校验 `object ==
  "chat.completion"`，取 `choices[0]`（`n > 1` 只取第一条，§2.2）。
- `response/convert.rs`：`message` → `Vec<ContentBlock>` + `finish_reason` 映射表函数；
  block 顺序保持 wire 出现顺序的合理归一（reasoning 在 text 前，与 anthropic 惯例一致
  ——以 `openai_resp/response/convert.rs` 的实际顺序先例为准）。
- `mod.rs` 的 `chat()`：stream 互斥校验（M1-2 已钉）→ `build_request` →
  `execute_json_response` → `parse_response`；错误经 `map_transport_error` /
  `ClientError::from_http_response` 分类。

验证条件：

- `response/tests/` + `fixtures/*.json`（模板 `openai_resp/response/tests/`，含
  `mod.rs`/`parsing.rs`/`transport.rs` 分层与 fixtures 目录）：
  1. 三种响应 fixture：纯文本、工具调用、含 `reasoning_content`；
  2. `finish_reason` 全表（`stop`/`length`/`tool_calls`/`content_filter`/未知/缺失）；
  3. 未知字段落 `Response.extra`；`usage`（含 cached/reasoning details）正确解析；
  4. arguments 非法 JSON 字符串时保留原文进 extra 而非报错；
  5. `object` 不符报错。
- `cargo test -p agent-lib --lib adapter::openai_chat` 通过。

### M2-2 [TODO] 非流式 transport 测试：状态码/内容类型/错误映射

上下文：

- 模板 `src/adapter/openai_resp/stream/tests/transport.rs:17-40`（一次性 `TcpListener`
  本地服务器，手搓 HTTP 响应）；`openai_resp/response/tests/transport.rs` 有非流式同款。
- 目的：钉住 `chat()` 的传输层行为——HTTP 状态码分类、错误 body 标记（context-length /
  content-filter 的 OpenAI 拼写已被 `ClientError::from_http_response` 覆盖）、非 JSON
  响应处理。

实现要求：

- `response/tests/transport.rs`：本地 `TcpListener` 起一次性服务器，覆盖：
  1. 200 + 合法 body → 正常 `Response`；
  2. 429 带 `Retry-After` → 限流错误；
  3. 401 → 认证错误；
  4. 400 + OpenAI context-length 错误 body → context-length 分类；
  5. 400 + content-filter body → content-filter 分类；
  6. 非 2xx 其它 → transport/http 错误。
- 全部离线，不起真实网络；端口用 `bind("127.0.0.1:0")` 取临时端口。

验证条件：

- 上述用例逐个断言 `ClientError` 的分类变体（对照 `src/client/error.rs:61-105`）。
- `cargo test -p agent-lib --lib adapter::openai_chat` 通过，且测试秒级完成（本地
  回环，无超时等待）。

### M2-R [TODO] M2 review：非流式响应侧正确性核对

核对清单：

- 设计文档 §4.3 逐条对照：`object` 校验、`choices[0]`、三种 content 落点、arguments
  解析失败降级、`finish_reason` 全表、extra 兜底。
- 确认 `Usage` 零改动（`src/model/usage.rs` 无 diff）且 cached/reasoning details 有测试
  钉住。
- 确认 `src/adapter/common/` 与 `src/client/error.rs` 零改动（本里程碑只允许新增
  `openai_chat/` 内文件）。
- fixtures 与 `openai_resp` 惯例一致（`include_str!` 加载、脱敏）。
- 跑全量门禁命令，全部通过。

产出：本任务下方追加 review 记录。

---

## M3：SSE 流式

### M3-1 [TODO] 流式骨架：stream/wire.rs + decoder.rs（[DONE] 哨兵）

上下文：

- 模板 `src/adapter/openai_resp/stream/{mod.rs(57行),decoder.rs(39行),wire.rs(269行)}`；
  chat/completions 的 chunk 结构更扁，wire 视图应明显短于 269 行。
- 复用：`SseNormalizer` trait + `normalize_sse`（`src/adapter/common/sse.rs`，字节流 →
  SSE frame → `StreamEvent`，错误终止、EOF 处理）、`execute_sse_response`
  （`src/adapter/common/http.rs`）。
- chunk 形态（设计文档 §4.4）：`choices[0].delta = {role?, content?, reasoning_content?,
  tool_calls?}`；`finish_reason` 在末个非空 chunk；usage 在 `include_usage` 后由**空
  `choices` 的独立 chunk** 携带。
- `[DONE]` 哨兵（设计文档 §4.4.1）：`data: [DONE]` 非 JSON，JSON 解析前特判直接终止；
  `SseNormalizer` 的 `is_terminal`/`incomplete_error` 已支持适配器自控终止。SSE
  `event:` 字段恒为 `message`，既有 event/type 一致性检查自然通过。

实现要求：

- `stream/wire.rs`：chunk 的 crate-private serde 视图（`choices[].delta`、
  `choices[].finish_reason`、顶层 `usage`），字段全部按 §4.4 列出的形态建模；多余字段
  不建模（进不了 extra 的流式 chunk 不需要）。
- `stream/decoder.rs`：约 30 行，照 `openai_resp/stream/decoder.rs` 绑定 `SseNormalizer`；
  在 frame → chunk 的入口处特判 `data == "[DONE]"` → terminal。
- `stream/mod.rs`：`chat_stream()` 入口：stream 互斥校验（M1-2）→ `build_request`
  （`stream=true`，自动带 `include_usage`）→ `execute_sse_response` → `normalize_sse`
  挂 decoder + normalizer（normalizer 在 M3-2 实现，本任务可用最小桩让编译通过）。

验证条件：

- 单元测试：含 `[DONE]` 行的最小 SSE 字节流喂 decoder → 正常 terminal 收尾，无 JSON
  解析错误；`event: message` 行不触发一致性错误。
- `cargo test -p agent-lib --lib adapter::openai_chat` 通过。

### M3-2 [TODO] 流式状态机：normalizer.rs（文本/reasoning/工具增量/终态）

上下文：

- 模板 `src/adapter/openai_resp/stream/normalizer/`（多文件：item/part/reasoning/
  terminal）；chat/completions 无 item/part 层级、无终态快照，状态机应单文件可收。
- `StreamEvent` 形态（`src/stream/mod.rs:50-97`）：`BlockStart`/`Delta`/`BlockStop`/
  `MessageStop`/`Usage` 等；`BlockKind::Reasoning` + `Delta::Reasoning` 是
  `reasoning_content` 的落点（无 signature）。
- 关键差异（设计文档 §4.4.2-4.4.4，逐条实现）：
  1. 工具调用增量：`tool_calls[]` 按 `index` 键控；`id`/`function.name` 只在首 chunk
     出现，`function.arguments` 是字符串片段 → `BlockStart(ToolInput{tool_name,
     tool_call_id})` + 后续 `Delta::Json` + `BlockStop`，**绝不中途解析 JSON**；`BlockId`
     用位置派生稳定 id（先例 `anthropic-block-{index}`，
     `src/adapter/anthropic/stream/normalizer.rs:423-424`）；
  2. `delta.reasoning_content` → `BlockKind::Reasoning` 的 start/delta/stop；
  3. 终态：无 Responses 那样的终态快照事件；末 chunk 的 `finish_reason` →
     `MessageStop`（复用 M2-1 的 finish_reason 映射表）；空 `choices` 的 usage chunk →
     单段加性 `Usage`；EOF 而无 `[DONE]` → `incomplete_error`。

实现要求：

- `stream/normalizer.rs`：chunk → `Vec<StreamEvent>`（或照 `openai_resp` 的返回形状）的
  状态机；内部按 `index` 维护 tool_call 的打开状态（首个带 `id` 的 chunk 开 block，
  后续同 `index` chunk 只发 `Delta::Json`）；文本/reasoning block 的开关随字段出现/
  消失切换。
- `finish_reason` 映射函数从 `response/convert.rs` 复用（抽到两边都能用的位置，
  crate-private）。
- 多 `index` 交错（并行工具调用）必须正确处理，不假设单 tool_call。

验证条件：

- 单测直接构造 chunk 序列喂 normalizer（不依赖 fixture 文件），覆盖：
  1. 纯文本流 → `BlockStart(Text)` + `Delta::Text`* + `BlockStop` + `MessageStop`；
  2. `reasoning_content` 流 → `BlockKind::Reasoning` + `Delta::Reasoning`；
  3. 单工具调用：首 chunk 出 `BlockStart(ToolInput{name,id})`，arguments 片段逐个
     `Delta::Json`，末尾 `BlockStop`；中途不出现 JSON 解析；
  4. 两个 `index` 交错的并行工具调用；
  5. 空 `choices` usage chunk → 加性 `Usage`；`finish_reason` 各值 → `MessageStop`
     的 stop_reason 正确；
  6. EOF 无 `[DONE]` → `incomplete_error`。
- `cargo test -p agent-lib --lib adapter::openai_chat` 通过。

### M3-3 [TODO] 流式 fixtures + 端到端折叠对照 + transport

上下文：

- 模板 `src/adapter/openai_resp/stream/tests/`：`mod.rs`/`parsing.rs`/`transport.rs`/
  `errors.rs` + `fixtures/*.sse`（脱敏录屏，`include_str!` 加载）。
- `Accumulator`（`src/stream/`，归一化折叠器）把 `StreamEvent` 序列折回 `Response`，
  是流式 vs 非流式一致性的对照工具（设计文档 §7.1）。
- 不规则字节分块（设计文档 §7.1）：fixture 按尺寸 `[1,2,7,3,19,5,11]` 循环切块喂
  normalizer，钉住 UTF-8/行边界跨块的健壮性。

实现要求：

- `stream/tests/fixtures/*.sse`（脱敏，无任何真实 key/账号信息）：
  1. 纯文本流（含 `[DONE]`）；
  2. 工具调用流（多 `index` 并行）；
  3. `reasoning_content` 流；
  4. `include_usage` 终态流（末个空 `choices` usage chunk + `[DONE]`）。
- `stream/tests/parsing.rs`：每个 fixture 用不规则字节分块喂完整
  `chat_stream` 管线（或 normalize_sse + decoder + normalizer），断言精确
  `StreamEvent` 序列；再用 `Accumulator` 折叠，与同一语义响应的非流式
  `parse_response` 结果对照一致（文本/reasoning/tool_use/stop_reason/usage）。
- `stream/tests/errors.rs`：`[DONE]` 哨兵终止、EOF 无 `[DONE]` 报错、SSE 错误帧/非
  2xx 的错误传播。
- `stream/tests/transport.rs`：一次性 `TcpListener` 返回 SSE body，覆盖 200 流式成功
  与非 2xx 错误映射（模板 `openai_resp/stream/tests/transport.rs:17-40`）。

验证条件：

- 四个 fixture 的精确事件序列断言全部通过；Accumulator 折叠 == 非流式解析对照通过；
  错误路径用例通过。
- `cargo test -p agent-lib --lib adapter::openai_chat` 通过，秒级完成。

### M3-R [TODO] M3 review：流式正确性核对

核对清单：

- 设计文档 §4.4 四个关键差异逐条对照实现：哨兵特判在 JSON 解析前、`index` 键控增量
  不中途解析、reasoning 落点正确、终态双源（finish_reason + usage chunk）无重复
  `MessageStop`。
- 与 M2 的一致性：finish_reason 映射表两处共用同一份代码（无复制粘贴漂移）；
  Accumulator 折叠对照测试确实存在且通过。
- fixtures 脱敏检查：无真实 key、token、账号、内网地址。
- 状态机对乱序/缺失字段的健壮性：缺 `id` 的后续 chunk、空 delta、未知字段不 panic。
- 跑全量门禁命令，全部通过。

产出：本任务下方追加 review 记录。

---

## M4：facade 接线与集成

### M4-1 [TODO] facade 接线：client_for_provider 分支 + openai_chat_from_env + lib.rs 文档

上下文：

- `client_for_provider`（`src/facade/chat.rs:387`）按 `ProviderConfig` 变体构造
  `Arc<dyn LlmClient>`，现有 Anthropic/OpenAiResp 分支可照抄。
- `ProviderConfig`（`src/facade/config.rs`）：现有 `openai_from_env`（:109-117）是
  **Azure 风格**（`api-key` 头 + `api-version` query），对 chat/completions 直连
  **不适用**（设计文档 §6）；需要 Bearer 风格构造器。
- env 约定（设计文档 §6）：`OPENAI_CHAT_BASE_URL` / `OPENAI_CHAT_API_KEY`；DeepSeek 既
  有 env 约定（`DEEPSEEK_API_KEY` / `DEEPSEEK_BASE_URL`）已在混合 e2e 中使用
  （AGENTS.md「Required environment」表），facade 构造器是否一并提供 DeepSeek 便捷入口
  在本任务定夺（最小方案：只加 `openai_chat_from_env`，DeepSeek 由用户把 base_url 指到
  `https://api.deepseek.com`）。
- `src/lib.rs:16-17` 的模块文档协议清单当前只列 Anthropic Messages 与 OpenAI
  Responses。

实现要求：

- `ProviderConfig` 新增 chat/completions 变体（或复用现有变体 + 协议判别字段，以
  `config.rs` 现有形状为准，选最小改动）+ `openai_chat_from_env()` 构造器：
  `OPENAI_CHAT_BASE_URL` 必需、`OPENAI_CHAT_API_KEY` 可选（vLLM 可无 auth，对应
  `AuthScheme::None`）；错误走 `FacadeError` 既有风格。
- `client_for_provider` 加分支构造 `OpenAiChatAdapter`。
- `src/lib.rs:16-17` 协议清单加 chat/completions。
- facade 层 rustdoc 示例（`chat.rs` 顶部 doc 引用 `openai_from_env` 处）按需补一条
  chat/completions 用法，不改既有示例语义。

验证条件：

- 单测：env 缺 `OPENAI_CHAT_BASE_URL` → 明确错误；env 齐备 → 构造成功且
  `client_for_provider` 返回的 client `capability()` 与
  `OPENAI_CHAT_DEFAULT_CAPABILITY` 一致（env 隔离用既有测试惯例处理，不污染并行
  测试）。
- `cargo test -p agent-lib --lib facade` 通过；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。

### M4-2 [TODO] 归一化矩阵：tests/normalization/config.rs 注册新 Provider

上下文：

- `tests/normalization/config.rs:20` 的 `Provider` enum 当前为 `{Anthropic,
  OpenAiResponses}`；`configured_targets()` 按确定性顺序构建有完整凭据的 target；
  `build_openai_target()`（:60 起）是 `OpenAiRespAdapter::with_http_client` 样板。
- normalization 矩阵的 scenario 是 provider-neutral 的，新增 provider 只需注册构造
  分支，scenario 不改。

实现要求：

- `Provider` 新增 `OpenAiChat` 变体（保持矩阵顺序确定性，追加在末尾）；
  `build_target` 加分支：env 齐备（base_url + token + 可用 model 名）才构造
  `OpenAiChatAdapter` target，否则 `None` 跳过；model 名走既有 env 惯例或常量。
- 若 normalization 下另有按 provider 数量的断言/快照，连带更新。

验证条件：

- 无 env 时 `cargo test --test integration_normalization` 照常通过（新分支静默
  跳过）；有 env 时本地手验新 provider 入矩阵（可在完成记录注明手验结果）。

### M4-3 [TODO] #[ignore] 真实端点测试：tests/integration_openai_chat.rs（DeepSeek + vLLM）

上下文：

- 模板 `tests/integration_openai_resp.rs:24-54`（Option 模式：env 缺省返回 `None`
  干净跳过，`#[ignore]` 默认不跑）。
- 两套配置（设计文档 §7.3）：
  - DeepSeek：`DEEPSEEK_API_KEY` 必需，可选 `DEEPSEEK_BASE_URL`（默认
    `https://api.deepseek.com`）/`DEEPSEEK_MODEL`；
  - vLLM：`VLLM_BASE_URL` 必需，可选 `VLLM_API_KEY`（缺省 `AuthScheme::None`）/
    `VLLM_MODEL`。
- 手搓 DeepSeek 客户端的行为参照（`finish_reason` 映射、system 渲染、bearer 直连）：
  `tests/agent_external_managed_real_e2e.rs:245-442`、
  `tests/agent_external_real_e2e.rs:144-184`。
- DeepSeek 方言规则（设计文档 §5.1）：思考模式开关 `{"thinking":{"type":"enabled/
  disabled"}}` 与 `reasoning_effort` 经 `provider_extras` 传递；有工具调用时
  `reasoning_content` 必须完整回传，否则 API 400。

实现要求：

- `tests/integration_openai_chat.rs`，全部 `#[ignore]`，每套配置缺 env 干净跳过：
  1. DeepSeek 非流式：基础问答，断言 text 与 usage；
  2. DeepSeek 流式：断言事件流含 text delta + 终态 usage；
  3. DeepSeek 思考模式（`provider_extras` 开 thinking）：响应含 `Thinking` block；
  4. **DeepSeek 思考模式多轮 + 工具调用**：第一轮带 tool 定义触发 tool_calls → 回放
     完整历史（assistant 消息携带 `reasoning_content` + `tool_calls`，由适配器请求侧
     自动完成）+ tool 结果 → 第二轮不 400 且正常收尾（§5.1 的 400 规则验证）；
  5. vLLM 非流式 + 流式基础用例；若端点开 `--reasoning-parser`，顺带验证
     `reasoning_content` 回放是否被接受（§5.2 待验证项）。
- 测试内不打印 key；model 名从 env 读，无默认值硬编码真实账号模型。

验证条件：

- 无 env：`cargo test --test integration_openai_chat` 全部跳过、exit 0。
- 有 DeepSeek key：人工跑 `cargo test --test integration_openai_chat -- --ignored
  --nocapture`，DeepSeek 用例（尤其第 4 条 400 规则）通过；结果与 vLLM 实测结论记录
  在完成记录中（环境缺失则如实标注「未实测」，留给 M5-1 的 capability-matrix 引用）。

### M4-R [TODO] M4 review：接线与集成正确性核对

核对清单：

- facade 构造器错误路径与既有 `*_from_env` 一致；Azure 风格 `openai_from_env` 未被误
  改语义。
- 归一化矩阵顺序确定性保持；无 env 时默认测试树全绿。
- 真实端点测试全部 `#[ignore]`、无 key 泄漏、缺 env 干净跳过。
- M4-3 的实测结论（或未实测标注）已写入完成记录，M5-1 可直接引用。
- 跑全量门禁命令（含 external features 的 clippy），全部通过。

产出：本任务下方追加 review 记录。

---

## M5：文档同步与收尾

### M5-1 [TODO] 文档同步：DESIGN.md 决策反转 + capability-matrix + README + AGENTS.md + client-layer-references

上下文：

- 设计文档 §8 的同步清单是本任务的定义；其中 `DESIGN.md` §1.1 修订是**决策反转，必须
  做**（设计文档 §1）：原决策「不支持 chat/completions」（`DESIGN.md:18`）与 DeepSeek、
  vLLM 归在 Anthropic 协议下（`DESIGN.md:15`）均须修订。
- capability-matrix 的「DeepSeek/vLLM 实测」一节引用 M4-3 完成记录的实测结论；若环境
  缺失未实测，如实标注「待实测」而不是留空或编造。

实现要求：

- `DESIGN.md` §1.1：协议清单加 chat/completions；删除/修订「不支持」段，改写为「经
  `openai_chat` 适配器支持，方言策略见 `docs/openai-chat-api.md`」；DeepSeek、vLLM 的
  协议归类从 Anthropic 移到 chat/completions。
- `docs/capability-matrix.md`：协议级默认值表加 chat/completions 列（与
  `OPENAI_CHAT_DEFAULT_CAPABILITY` 一致）；新增 DeepSeek/vLLM 实测一节（思考模式、
  400 规则、vLLM 回放兼容性结论）。
- `README.md`：provider 选择段落加 chat/completions；ignored 测试命令加
  `cargo test --test integration_openai_chat -- --ignored --nocapture`。
- `AGENTS.md`：`src/` 布局的 `adapter/` 描述加 openai_chat；「Required environment」
  类表格加 `OPENAI_CHAT_BASE_URL`/`OPENAI_CHAT_API_KEY`/`VLLM_*`（注明可选/跳过语义）。
- `docs/client-layer-references.md`：参考分工总表加一行（可参考 `async-openai` 的
  chat 模块）。
- 顺手核对 `src/lib.rs` 与 `src/adapter/mod.rs` 的协议清单注释与实际一致。

验证条件：

- 文档中的命令、env 变量名、文件路径与代码实际一致（逐条对照，不凭记忆）。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过；`cargo fmt --all`
  无 diff。

### M5-2 [TODO] 可选收尾：e2e 手搓 DeepSeek 客户端替换为适配器

上下文：

- 两份手搓的、非流式 DeepSeek chat/completions 客户端：
  `tests/agent_external_managed_real_e2e.rs:245-442`、
  `tests/agent_external_real_e2e.rs:144-184`（设计文档 §1、§7.5）。
- 这两个测试是 `#[ignore]` 的真实 CLI e2e；替换后行为必须等价（finish_reason 映射、
  system 渲染、bearer 直连、不打印 key）。

实现要求：

- 两个 e2e 的 DeepSeek 调用改为构造 `OpenAiChatAdapter`（或经 facade），删除手搓
  HTTP 代码；若 e2e 需要的字段超出归一化 `Response`（如原始 JSON 调试输出），用
  `Response.extra` 逃生舱而非保留手搓客户端。
- 若替换过程中发现适配器缺口（缺字段/缺行为），**停止替换**，在完成记录中登记缺口，
  保留手搓客户端，不做适配器 scope 蔓延。

验证条件：

- `cargo test --all --all-targets` 通过（两 e2e 默认 `#[ignore]`，编译通过即可）；
- 有 `DEEPSEEK_API_KEY` 与 CLI 环境时人工跑通替换后的 e2e 并在完成记录注明；环境缺失
  则标注「编译验证，未实跑」。
- 若判定不替换（收益/风险不划算），在完成记录写明理由并把本任务标为 `[DONE]`（降级
  完成）。

### M5-R [TODO] M5 review + 最终收口

核对清单：

- 设计文档 §8 文档同步清单逐条销号；`DESIGN.md` 中不存在与本适配器矛盾的「不支持」
  表述（全文 grep `chat/completions`、`DeepSeek`、`vLLM` 核对口径）。
- 设计文档 §2.1 第一期目标三条逐条验收：适配器 + 两方言、三层测试、归一化矩阵。
- 设计文档 §2.2 非目标确认未被偷渡（无 logprobs 建模、无 quirk 配置类型等）。
- 规模核对：实现/测试行数与 §9 估算（1200–1500 + 800–1000）是否量级相符，严重超标
  需说明原因。
- 全部任务已 `[DONE]` 或显式降级；最终跑一遍全量门禁命令（含 external features 的
  clippy）。
- 在 PLAN.md 追加最终收口结论（比照归档 PLAN 的体例），然后把 PLAN.md 与 TODO.md
  归档到 `docs/archive/<日期>-openai-chat/`。

产出：本任务下方追加最终 review 记录；PLAN.md 追加收口结论；完成归档。
