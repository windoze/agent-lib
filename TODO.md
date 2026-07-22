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

### M1-2 [DONE] 适配器骨架：OpenAiChatAdapter 结构体与 LlmClient 契约

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

### M1-3 [DONE] 请求侧映射：build_request + Message/Tool → messages/tools

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

完成记录（2026-07-23）：

- `request.rs`：`build_request` 组装 `POST /chat/completions`（`common::endpoint_url(&["chat","completions"])`
  + `common::endpoint_headers` + `.json(&body)`）。`serialize_body` 用 `OpenAiChatRequestBody` 结构体
  序列化为 `Value`，再在**所有映射完成后** `provider_extras.merge_into(body, ProviderId::OpenAiChat)`
  最后合并（可覆盖任意顶层字段，mismatch → `ClientError::Protocol` 报错，与既有适配器同款）。
  错误分类：`invalid_request` → `ClientError::Protocol`（请求映射），`invalid_endpoint` →
  `ClientError::Other`（端点配置，照 `openai_resp`）。
- 顶层 body 字段（设计文档 §4.2）：`model` / `messages` / `max_tokens`（**非** Responses 的
  `max_output_tokens`，直接对应 `ChatRequest.max_tokens`）/ `stream`；`temperature` `Option` omit-None；
  `tools` omit-empty；`stream_options: {include_usage:true}` 仅 `stream=true` 注入（`StreamOptions` 结构体）。
- `request/input.rs`（纯函数映射，逐行对 §4.2 表）：
  - `system` → 首条 `{"role":"system","content":…}`（在 `serialize_body` 注入，先于 messages 循环）。
  - user：纯文本 → `content` 字符串（多 Text 块拼接）；含 image/unknown → 多模态 array form
    （`{type:text,text}` / `{type:image_url,image_url:{url}}` / raw），vision 输入保真。
  - **assistant 多 block 聚合成一条 chat 消息**（chat/completions 消息模型是一条 message 挂 content +
    reasoning_content + tool_calls）：Text → `content`（拼接，空则 `null`）；Thinking → `reasoning_content`
    （拼接，**无条件原样回放** §5.1 推论，无 signature）；ToolUse → `tool_calls:[{id,type:"function",
    function:{name,arguments:<JSON 字符串>}}]`。
  - Tool 角色：每 `ToolResult` 块 → 一条 `{"role":"tool","tool_call_id","content"}`；`Vec<ContentBlock>`
    扁平化文本（image/unknown 有损丢弃，第一期接受，`parts.join("\n")`）；非 `Ok` 状态拼入文本前缀
    `[tool error/denied/cancelled]`（`tool_result_status_marker`，Anthropic `is_error` 类比，chat 无该字段）。
  - `tool_to_wire`：`{type:"function",function:{name,description,parameters}}`（比 Responses 多一层
    `function` 嵌套）。
  - System 角色 → 报错（用 `ChatRequest.system`）；role/block 不匹配（如 user 带 ToolUse、assistant 带
    Image、tool 带 Text）→ `ClientError::Protocol`「not valid for {Role} role」；空 Tool 消息 → 报错
    「contains no tool results」。
- wire 类型全部 crate-private（`input.rs` 内私有 fn，`tool_to_wire`/`message_to_wire` 为 `pub(super)`）；
  顶层 body 结构体 `OpenAiChatRequestBody`/`StreamOptions` 私有；无泄漏到 `adapter::openai_chat` 外。
- `mod.rs`：移除 M1-2 过渡性 `#[allow(dead_code)]`（`http_client` 现被 `build_request` 读取，M1-2 完成
  记录要求接线后移除）。
- **不接线 `chat()`/`chat_stream()`**：response.rs / stream/mod.rs 桩保持不变（`ClientError::Other`
  占位），transport+parse 留 M2-1，SSE 解码+归一化留 M3。
- 验证结果（全绿）：
  - `cargo fmt --all`（无 diff）；
  - `cargo clippy --all-targets -- -D warnings`；
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  - `cargo test -p agent-lib --lib adapter::openai_chat`（12 通过：3 M1-2 既有 + 9 M1-3 新增）；
  - `cargo test --all --all-targets`（全绿，lib 1065→1074 +9，无回归；其余套件 0 失败）；
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p agent-lib`。
- 9 个新增请求单测（`json!` 精确比对完整 body + method/path/query/headers）覆盖 TODO 列出的 6 个关键
  用例：① system 首条消息 ② tools function 嵌套 ③ tool_result 扁平化 + 非 Ok 拼入 ④ stream_options
  注入 ⑤ extras 覆盖 + mismatch ⑥ assistant 一条消息携带 reasoning_content + tool_calls（§5.1）；
  另含 image 多模态、invalid role/block、auth 变体/可选字段/malformed endpoint。
- 无 breaking change：纯新增 `build_request`（pub 方法）+ 私有映射函数；移除的是过渡 `#[allow(dead_code)]`
  而非公开 API。浮点：temperature 经 serde f32→f64，测试用精确值（0.25/0.5）规避 f32↔f64 不等。

### M1-R [DONE] M1 review：骨架与请求侧正确性核对

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

完成记录（2026-07-23）：

核对结论（逐条对 checklist）：

1. **设计文档 §4.2 映射表逐行核对，无遗漏行**：
   - `ChatRequest.system` → 首条 `{"role":"system","content":…}`（`request.rs:67-69`）✓
   - user/assistant 文本 → `{"role","content"}`（`input.rs` `user_message_to_wire`/`assistant_message_to_wire`）✓
   - assistant `ContentBlock::Thinking` → 该消息 `reasoning_content` 字段，**无条件原样回放**
     （`input.rs:95-97`、`117-119`：拼接到 `reasoning` 字符串，非空即插入，不判断本轮是否有
     tool_call——符合 §5.1「统一原样回放永远安全」推论）✓
   - assistant `ContentBlock::ToolUse` → `tool_calls:[{id,type:"function",function:{name,arguments:<JSON 字符串>}}]`
     （`input.rs:202-216` `tool_call_to_wire`，arguments 经 `serde_json::to_string` 序列化为字符串）✓
   - `ContentBlock::ToolResult` → 独立 `{"role":"tool","tool_call_id","content"}` 消息；`Vec<ContentBlock>`
     扁平化文本（image/unknown 有损丢弃，`parts.join("\n")`）；非 `Ok` 状态拼入文本前缀
     `[tool error/denied/cancelled]`（`input.rs:132-164`，Anthropic `is_error` 类比）✓
   - `ChatRequest.tools` → `tools:[{type:"function",function:{name,description,parameters}}]`，比 Responses
     多一层 `function` 嵌套（`input.rs:220-229` `tool_to_wire`）✓
   - `stream=true` → 自动注入 `stream_options:{"include_usage":true}`，`stream=false` 省略
     （`request.rs:83-85` + `StreamOptions` 私有结构体 + `skip_serializing_if=Option::is_none`）✓
   - `provider_extras` → 所有映射完成后 `merge_into` 最后合并，可覆盖任意顶层字段；mismatch 返回
     `IgnoredProviderMismatch` outcome，`serialize_body` 据此报 `ClientError::Protocol`（`request.rs:90-103`）✓
   - `max_tokens` 非可选，直接对应（`request.rs:49,79`，**非** Responses 的 `max_output_tokens`）✓
   - 关键防线确认：`reasoning_content` 无条件回放 + `stream_options` 注入两条均命中且有 `json!` 精确
     比对单测钉死（`assistant_message_aggregates…`、`stream_flag_controls_include_usage…`）。

2. **三处触点形状与既有先例一致；capability 字段与设计文档 §6 一致**：
   - `ProviderId::OpenAiChat`（`extras.rs:21`）：serde `rename_all="snake_case"` → `open_ai_chat`，与
     `anthropic`/`open_ai_resp` 同风格；round-trip 测试表已追加 `(OpenAiChat,"open_ai_chat")`（`extras.rs:170`）✓
   - `OPENAI_CHAT_DEFAULT_CAPABILITY`（`capability.rs:106-123`，full struct literal 比照
     `OPENAI_RESP_DEFAULT_CAPABILITY`）：`max_context_tokens:None`；
     `input_modalities:{Text,Image}`；`output_modalities:{Text}`；
     `streaming/tool_calling/parallel_tool_calls/reasoning=true`；
     `prompt_caching/structured_output=false`（显式，与既有静态的关键差异，对应 §6「无 prompt_caching
     /structured_output 声明」）；`stop_reasons:{ToolUse,EndTurn,MaxTokens,StopSequence,Refusal}`
     （`StopSequence`=chat 的 `stop` 参数，`Refusal`=`content_filter`）✓ 逐字段断言测试
     `openai_chat_default_describes_protocol_capabilities`（`capability.rs:230-261`）钉死。
   - 模块注册 `src/adapter/mod.rs:5` `pub mod openai_chat;`（字母序排在 `common` 与 `openai_resp` 之间）✓；
     `src/client/mod.rs:14-16` 已 `pub use` 出 `OPENAI_CHAT_DEFAULT_CAPABILITY`。
   - facade 耦合（M1-1 因 exhaustive match 触及）：`config.rs:256/289`（Bearer 直连分支 +
     `openai_chat_endpoint` helper）、`chat.rs:395`（`client_for_provider` 分支）均在位，形状一致。

3. **wire 类型无泄漏；Debug 不泄露密钥**：
   - `request/input.rs` 全部映射函数产出 `serde_json::Value`（无 wire struct 泄漏）；
     `message_to_wire`/`tool_to_wire` 为 `pub(super)`（仅 `request` 模块可见，不外泄到 `adapter::openai_chat`
     之外），其余 fn 私有；`request.rs` 顶层 `OpenAiChatRequestBody`/`StreamOptions` 私有 ✓
   - `Debug` 脱敏：`mod.rs:122-140` `adapter_debug_redacts_endpoint_credentials` 钉住——构造含
     `sk-ant-secret`（Bearer）+ `extra_headers` 同密钥的 adapter，断言 `format!("{adapter:?}")` 既不含
     `sk-ant-secret` 又含 `[REDACTED]`（密钥经 `EndpointConfig` 脱敏 `Debug`，与 `openai_resp` 同款）✓

4. **请求单测覆盖 §7.1 全部关键用例，`json!` 精确比对**：
   - 6 个关键用例逐一覆盖，且均用 `assert_eq!(…, json!{…})` 对**完整请求 body** 精确比对（非字段抽查）：
     ① system 首条消息 ② tools `function` 嵌套形状 ③ tool_result 扁平化 + 非 `Ok` 拼入
     ④ `stream_options` 注入（true/false 两态）⑤ extras 覆盖既有字段 + mismatch 报错
     ⑥ 多轮历史 assistant 一条消息携带 `reasoning_content` + `tool_calls`（§5.1 DeepSeek 400 防线）。
   - 额外覆盖：image 多模态 array form（含 URL/Base64）、invalid role/block 一族报错、auth 变体
     （Bearer/None）+ 可选字段 omit + malformed endpoint（`ClientError::Other`）。transport/auth 类用例
     因天然是字段观察（method/path/query/headers）而用字段抽查，属合理，非 body-shape 用例。
   - `openai_chat` 模块共 12 用例（3 M1-2 + 9 M1-3）全绿。

门禁输出摘要（全绿）：

- `cargo fmt --all`：无 diff。
- `cargo clippy --all-targets -- -D warnings`：通过。
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`：通过。
- `cargo test --all --all-targets`：全部 `test result:` 行 `0 failed`（lib 1074 通过，集成/replay/smoke 套件全绿，无回归）。
- `cargo test -p agent-lib --lib adapter::openai_chat`：12 通过。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`：通过。

发现的问题及处置：

- 无 spec 偏差、无 workaround、无未调度测试失败。M1 实现与设计文档 §4.2/§5.1/§6/§7.1 完全一致，
  M2-1（非流式 transport+parse）/M3（SSE）的桩状态符合本里程碑边界（`ClientError::Other` 占位，非 panic），
  已在对应任务排期，不构成 M1 的阻塞或遗留问题。
- 微小观察（非问题，不阻断）：`input.rs:182` `flatten_tool_result_text` 的错误文案为「must be text
  or image」，而实际 `Image`/`Unknown` 均被静默接受（仅丢弃），文案略宽于实际接受的 `Text|Image|Unknown`；
  这是面向开发者的诊断文案精度问题，不影响任何 spec 行为或测试，留待将来顺手收口，不在本 review
  范围内单列任务。

---

## M2：非流式响应侧

### M2-1 [DONE] 响应解析：wire 类型 + parse_response + finish_reason 映射 + chat()

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

完成记录（2026-07-23）：

- `response.rs`：替换 M1-2/M1-3 的 `chat()` 桩。`parse_response(body)` 反序列化字节 →
  `parse_response_value(value)`。`parse_response_value` 校验 `object == "chat.completion"`
  （三态 match：Some(String) 匹配 / Some(String) 不匹配报错 / Some(_) 非字符串报错 / None 缺失报错，
  比照 `openai_resp/response.rs:69`）；`take_usage` 从 wire 移除 usage（消费进 `Usage` 字段，
  unmodeled usage 字段自然落 `Usage.extra`，与 openai_resp 一致）；`read_choice` 读 `choices[0]`
  的 `message` 与 `finish_reason`（**借用不消费**，见下）；`convert_message` + `normalize_finish_reason`
  完成归一；`extra = wire`（剩余顶层）。
- `chat()`：`if request.stream` 守卫（M1-2 已钉，先于 transport）→ `self.build_request(&request)?` →
  `common::execute_json_response(&self.http_client, request, Self::parse_response)`。错误分类完全复用
  `common::map_transport_error` / `ClientError::from_http_response`（已覆盖 429/Retry-After、408/504、
  401/403、context-length、content-filter 的 OpenAI 拼写），`common/` 与 `error.rs` 零改动。
- `response/convert.rs`（新建，crate-private）：
  - `convert_message(&Value) -> Result<Vec<ContentBlock>>`：**借用 message 不消费**，便于 choices 完整留 extra。
    校验 `role == "assistant"`（防御，与 openai_resp 一致）；block 顺序 = reasoning(Thinking) →
    text(Text) → tool_calls(ToolUse)（anthropic reasoning-before-text 惯例 + wire 字段序，设计文档 §4.3）。
  - `convert_content`：string→Text；null/缺失/空→无 block；multimodal array 防御性支持（拼接 type=="text"
    的 text 字段，非 text 部分丢弃，有损但 robust，不 panic）；非 string/null/array → Protocol 报错。
  - `convert_tool_call`：取 `id` / `function.name` / `function.arguments` 为 required string（缺失→Protocol
    报错，不引入空值风险，与 review M-STATE-1 指出的「accumulator 不检查空 id/name」相反方向的安全选择）。
  - `parse_arguments`：空 arguments `""`→`json!({})`（与 openai_resp `empty_function_arguments` 一致）；
    **解析失败 → `input=Value::Null` + `extra[RESPONSE_EXTRA_KEY]["raw_arguments"]=原文`**（设计文档 §4.3
    「解析失败保留原文进 extra」；input=null 表示无有效输入、不伪造，原文可经 extra 恢复）。
  - `normalize_finish_reason(Option<&str>)`：finish_reason 为权威终止信号，映射表 `stop`→EndTurn /
    `length`→MaxTokens / `tool_calls`→ToolUse / `content_filter`→Refusal / 其它→unknown(Other) /
    缺失/null→`without_raw(Other)`。**不引入 has_tool_call 兜底**（避免与 openai_resp 的 status+evidence
    复杂逻辑漂移；chat 的 finish_reason 是单一权威字段）。
- **`RESPONSE_EXTRA_KEY` 常量**：声明于 `mod.rs`（`const RESPONSE_EXTRA_KEY: &str = "openai_chat";`，比照
  openai_resp 的 `openai_response`），convert.rs 经 `crate::adapter::openai_chat::RESPONSE_EXTRA_KEY` 引用，
  仅用于非法 arguments 的 raw_arguments 命名空间。
- **choices 不从 extra 移除**（关键设计决策，与 openai_resp「remove output + 按 block 重建 evidence」不同）：
  只 remove `usage`；`choices`（含 `choices[0].logprobs`/`message`/`finish_reason`）、`object`、`id`、`created`、
  `model`、`system_fingerprint` 等**全部保留在 `Response.extra`**。理由：设计文档 §2.2 明确「logprobs 归一化模型
  无处安放，**只能进 extra**」，而 logprobs 在 `choices[0]` 内——保 choices 是满足该约束最简洁的形态，同时顺带
  满足 §4.3「未建模字段（created/system_fingerprint 等）进 extra」。message 因此在 extra 中与归一化 block 并存
  （冗余但无损，利于 forward-compat；M3 流式 Accumulator 折叠对照按文本/reasoning/tool_use/stop_reason/usage
  字段比对，不受 extra 形状影响）。
- `response/tests/{mod.rs, parsing.rs}` + 3 个脱敏 fixtures（`text_response.json`/`tool_response.json`/
  `reasoning_response.json`，无真实 key/账号）：
  - parsing.rs 10 用例：① text fixture（content + usage 含 cached/reasoning details + extra 含 object/model/
    created/system_fingerprint/choices.logprobs、不含 usage）② tool fixture（id/name/arguments 解析 + ToolUse
    stop）③ reasoning fixture（Thinking 在 Text 前 + signature=None + reasoning_tokens）④ **finish_reason 全表**
    （stop/length/tool_calls/content_filter/未知/缺失→对应 stop_reason，缺失 without_raw）⑤ null finish_reason→Other
    ⑥ 未知顶层字段 + 结构化 logprobs 留 extra（usage 不留）⑦ 空 arguments→json!({}) ⑧ **非法 arguments→input=null +
    extra[RESPONSE_EXTRA_KEY]["raw_arguments"]=原文** ⑨ 并行 tool_calls→有序 ToolUse blocks ⑩ object 不符/缺失
    choices/空 choices/缺 message/非 assistant role/缺 function/非 string content/非 string finish_reason/非法 usage
    一族 Protocol 报错 + malformed JSON 报错。
  - mod.rs 含 `minimal_request`/`local_endpoint`（加 `#[allow(dead_code)]` + 注释标注 M2-2 transport 接线，
    沿用 M1-2 过渡 allow 惯例）；fixtures 经 `include_str!` 加载。
- 验证结果（全绿）：
  - `cargo fmt --all`（无 diff）；
  - `cargo clippy --all-targets -- -D warnings`；
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  - `cargo test -p agent-lib --lib adapter::openai_chat`（22 通过：12 既有 request/mod + 10 新增 response/parsing）；
  - `cargo test --all --all-targets`（全部 `test result:` 行 `0 failed`，lib 1062→1072 +10，集成/replay/smoke 套件全绿，无回归）；
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- 无 breaking change：纯新增 `parse_response`（pub 方法）+ `parse_response_value`（pub(super)）+ 私有 convert
  模块/函数 + 新增 RESPONSE_EXTRA_KEY 私有常量；`chat()`/`invalid_response` 签名不变（仅替换占位 body 为真实
  transport+parse 委托）。transport.rs（状态码/内容类型/错误映射的本地服务器测试）留 **M2-2**，符合本任务边界。

### M2-2 [DONE] 非流式 transport 测试：状态码/内容类型/错误映射

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

完成记录（2026-07-23）：

- `response/tests/transport.rs`（新建）：一次性 `TcpListener`（`bind("127.0.0.1:0")`
  取临时端口）本地服务器 + `chat_with_timeout`（5s 外层超时包裹 `adapter.chat(request)`，
  防 transport 回归卡死测试）。6 个 `#[tokio::test]` 精确对应 TODO 6 用例，逐个断言
  `ClientError` 分类变体（对照 `error.rs:61-105`）：
  1. **200 + 合法 body**（复用既有 `REAL_TEXT_RESPONSE` fixture）→ 正常 `Response`，
     断言 `role == Assistant` + usage `input=13`/`output=26`（transport→parse 接线贯通）；
  2. **429 + `Retry-After: 3`** → `assert_eq!(…, ClientError::RateLimited { retry_after:
     Some(Duration::from_secs(3)) })`（seconds 形式确定性）；
  3. **401** → `matches!(…, ClientError::Auth)`；
  4. **400 + OpenAI context-length body**（`maximum context length … context_length_exceeded`，
     含 CONTEXT_LENGTH_MARKERS 双重命中，且不含 content-filter marker）→
     `matches!(…, ClientError::ContextLengthExceeded)`；
  5. **400 + content-filter body**（`code:"content_filter"`，含 CONTENT_FILTER_MARKERS，
     且不含任何 context-length marker，避免被先判成 ContextLengthExceeded）→
     `matches!(…, ClientError::ContentFiltered)`；
  6. **500 非 2xx**（绕开 4xx marker 分支）→ `ClientError::Api { status: 500, body }`，
     断言 status + body 保留原文（含 `server_error`）。
- 服务器请求行断言 `POST /chat/completions HTTP/1.1`（每用例都钉，确认 `endpoint_url(&["chat","completions"])`
  的 path 组装；比照 openai_resp transport 模板断言 Responses path）。
- `response/tests/mod.rs`：`mod parsing;` 后加 `mod transport;`；移除 `minimal_request`/
  `local_endpoint` 上 M2-1 预留的过渡 `#[allow(dead_code)]`（transport.rs 已消费，allow 失效，
  沿用 M1-2→M1-3「接线后移除过渡 allow」惯例），并收紧 doc 注释。
- **零改动**：`src/adapter/common/`、`src/client/error.rs`、`response.rs`/`convert.rs` 均未动——
  chat() 的 transport→parse 接线在 M2-1 已完成（`execute_json_response` → `parse_response`，
  非 2xx 经 `ClientError::from_http_response`），本任务只在其上钉测试。fixtures 沿用 M2-1 既有
  三个脱敏录制（无真实 key/账号）。
- **范围克制**：不复制 openai_resp transport 的 invalid-success-body 用例（parsing.rs 已充分
  覆盖 `parse_response` 错误路径）、不复制 stream-guard 用例（mod.rs tests M1-2 已钉）；严格
  按 TODO 6 用例，避免冗余。200 成功用例已确认 transport→parse 完整接线。
- 验证结果（全绿）：
  - `cargo fmt --all`（无 diff）；
  - `cargo clippy --all-targets -- -D warnings`；
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  - `cargo test -p agent-lib --lib adapter::openai_chat`（28 通过：22 既有 request/response/mod +
    6 新增 transport，0.02s 秒级完成）；
  - `cargo test --all --all-targets`（全部 `test result:` 行 `0 failed`，lib 1072→1078 +6，
    集成/replay/smoke 套件全绿，无回归）；
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- 无 breaking change：纯新增测试文件 + 测试模块声明；移除的是过渡 `#[allow(dead_code)]` 而非公开 API。

### M2-R [DONE] M2 review：非流式响应侧正确性核对

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

完成记录（2026-07-23）：

核对结论（逐条对 checklist）：

1. **设计文档 §4.3 逐条对照，无遗漏行**：
   - **`object == "chat.completion"` 校验**（`response.rs:74-91` 四态 match：Some(String) 匹配放行 /
     Some(String) 不符报错 / Some(非 string) 报错 / None 缺失报错，比照 `openai_resp/response.rs:69`
     的 `object=="response"`）✓
   - **取 `choices[0]`**（`read_choice` `response.rs:122-156`：`n>1` 只取第一条，逐层校验 choices 缺失/
     非数组/空数组/`choices[0]` 非 object/缺 message/finish_reason 类型）✓
   - **三种 content 落点**（`convert.rs:28-73` `convert_message`，借用 message 不消费以保 choices 留 extra）：
     - `message.content` → `ContentBlock::Text`（`convert_content:98-134`，string→Text；null/缺失/空→无 block；
       多模态 array 防御性拼接 text 部分，非 text 丢弃）✓
     - `message.reasoning_content` → `ContentBlock::Thinking { text, signature: None, extra: 空 }`
       （`convert.rs:44-52`，**signature 恒 None** 符合 §4.3；空串不产出）✓
     - `message.tool_calls[]` → `ContentBlock::ToolUse`（`convert_tool_call:141-172`）✓
   - **arguments 解析失败降级**（`parse_arguments:175-192`）：空串→`json!({})`；合法→解析为 Value；
     **非法 JSON→`input=Value::Null` + `extra[RESPONSE_EXTRA_KEY]["raw_arguments"]=原文`**（§4.3「解析失败保留原文
     进 extra」；input=null 表示无有效输入、不伪造，原文可经 extra 恢复，与伪造/丢弃相反方向的安全选择）✓
   - **`finish_reason` 全表**（`normalize_finish_reason:81-90`，§4.3 映射表逐行）：`stop`→EndTurn / `length`→MaxTokens /
     `tool_calls`→ToolUse / `content_filter`→Refusal / `Some(其它)`→`Normalized::unknown(raw)`（
     `StopReason::unknown_value()==Other`，`normalized.rs:92-96` 钉死）→ value=Other / `None`→
     `Normalized::without_raw(Other)`；二者 value 均为 Other，吻合表「其它/缺失→Other」，raw 仅在有原文时保留 ✓
   - **extra 兜底**（`response.rs:105` `extra = wire`，只 `remove("usage")`）：`choices`（含 `choices[0].logprobs`、
     `message`、`finish_reason`）、`object`、`id`、`created`、`model`、`system_fingerprint` 及一切未建模顶层字段
     **全部保留**（§4.3「未建模字段进 extra」+ §2.2「logprobs 只能进 extra」——logprobs 在 `choices[0]` 内，保 choices
     是最简满足约束的形态）✓
   - block 顺序：reasoning(Thinking) → text(Text) → tool_calls(ToolUse)（anthropic reasoning-before-text 惯例 +
     wire 字段序，`recorded_reasoning_response_maps_reasoning_block_before_text` 钉死）✓

2. **`Usage` 零改动且 cached/reasoning details 有测试钉住**：
   - `git diff 401bdd8(M1-R)→HEAD -- src/model/usage.rs` **空**（零改动）✓
   - `Usage` 自定义 Deserialize 已认识 `prompt_tokens`/`completion_tokens`/`total_tokens`、
     `prompt_tokens_details.cached_tokens`、`completion_tokens_details.reasoning_tokens`（M2-1 上下文确认）。
     cached/reasoning details 由 fixture 测试钉住：
     - text fixture（`cached_tokens=4`/`reasoning_tokens=0`）→ `cache_read==4`/`reasoning==0`
       （`parsing.rs:29-30`）✓
     - reasoning fixture（`reasoning_tokens=35`/`cached_tokens=6`）→ `reasoning==35`/`cache_read==6`
       （`parsing.rs:95-96`）✓
   - `take_usage`（`response.rs:111-117`）将 usage 从 wire 移除并消费进 `Usage` 字段，未建模 usage 字段自然落
     `Usage.extra`，与 openai_resp 一致。

3. **`src/adapter/common/` 与 `src/client/error.rs` 零改动**：
   - `git diff 401bdd8→HEAD -- src/adapter/common/` **空**；`-- src/client/error.rs` **空** ✓
   - M2 两个提交（`d01f398` M2-1 / `ea52ff6` M2-2）`--stat` 仅触及 `src/adapter/openai_chat/` 内文件
     （+ TODO.md / memory/claude_plan.md），未越界到 common/error/usage ✓
   - chat() 的错误分类完全复用既有 `common::execute_json_response` + `ClientError::from_http_response`
     （已覆盖 429/Retry-After、408/504、401/403、context-length、content-filter 的 OpenAI 拼写），
     M2 transport 测试（6 用例）逐个断言该分类，零改动底层。

4. **fixtures 与 `openai_resp` 惯例一致**：
   - 目录结构同构：`response/tests/{mod.rs, parsing.rs, transport.rs}` + `response/tests/fixtures/*.json`，
     与 `openai_resp/response/tests/` 一一对应（openai_resp 仅 text/tool 两个 fixture；openai_chat 多一个
     reasoning fixture，对应 §7.1「含 `reasoning_content`」要求）✓
   - `include_str!` 加载（`tests/mod.rs:42-48`，与 openai_resp 同款）✓
   - **脱敏检查**：fixtures 均为 demo 值（model `deepseek-chat`/`deepseek-reasoner`、id `chatcmpl-recorded-*`、
     `system_fingerprint: fp_demo_*`、`call_recorded_weather`），`grep -iE 'sk-|Bearer |secret|...'` 仅命中
     `*_tokens` 字段名（含子串 "token"），**无真实 key/token/账号** ✓

门禁输出摘要（全绿，2026-07-23 独立重跑）：

- `cargo fmt --all`：无 diff。
- `cargo clippy --all-targets -- -D warnings`：exit 0。
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`：exit 0。
- `cargo test --all --all-targets`：全部 50 个 `test result:` 行 `0 failed`（lib 1090 通过；集成/replay/smoke 套件全绿，无回归）。
- `cargo test -p agent-lib --lib adapter::openai_chat`：28 通过（12 request/mod + 10 response/parsing + 6 transport），0.02s。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`：exit 0。

发现的问题及处置：

- 无 spec 偏差、无 workaround、无未调度测试失败。M2 实现（§4.3 响应解析 + chat() 接线 + transport 测试）
  与设计文档完全一致；M3（SSE 流式）桩状态（`stream/mod.rs::chat_stream` 返回 `ClientError::Other` 占位，非 panic）
  符合本里程碑边界，已在 M3-1/M3-2 排期，不构成 M2 的阻塞或遗留问题。
- 微小观察（非问题，不阻断）：`convert_content`（`convert.rs:98-134`）对多模态 array form 仅拼接 `type=="text"`
  部分、丢弃非 text 部分（有损但 robust，第一期 assistant content 为 string/null，array 是防御性 forward-compat），
  与 §4.3「`message.content` → Text」的 phase-one 范围一致；若将来 chat/completions assistant 输出多模态，
  需扩 `convert_content`——属 M5 之后的能力扩展，不在 M2 review 范围内单列任务。

---

## M3：SSE 流式

### M3-1 [DONE] 流式骨架：stream/wire.rs + decoder.rs（[DONE] 哨兵）

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

完成记录（2026-07-23）：

- `stream/wire.rs`（新建，crate-private serde 视图）：5 个视图类型 + `decode` 函数。
  - `DecodedChunk { choices: Vec<Choice>, usage: Option<Value> }`（两者 `#[serde(default)]`：
    空 `choices` 的 usage-only chunk 与缺 `usage` 的内容 chunk 都能解析）；
  - `Choice { delta: Delta, finish_reason: Option<String> }`；
  - `Delta { role, content, reasoning_content, tool_calls }`（全 `Option`，逐字段 `#[serde(default)]`）；
  - `ToolCallDelta { index: u64, id: Option<String>, function: Option<FunctionDelta> }`（`index` 键控，§4.4.2）；
  - `FunctionDelta { name, arguments }`（`arguments` 是字符串片段，绝不中途解析）。
  - 多余字段（`id`/`object`/`created`/`model`/`system_fingerprint`/`choices[].index`/`tool_calls[].type`）
    **不建模**，serde 默认忽略——设计文档「进不了 extra 的流式 chunk 不需要」（无 raw 保留，与
    `openai_resp` wire 的保留 `raw` 不同）。
  - **过渡性 `#[allow(dead_code)]`**（每个结构体一个 + 模块文档说明）：M3-1 仅 `decode` 验证形态，字段
    未被 lib 路径读取（`pub(super)` 在 crate 内**不**豁免 dead_code，沿用 M1-2/M2-1 过渡 allow 惯例）；
    M3-2 状态机读取字段后移除。
- `stream/normalizer.rs`（新建，`StreamNormalizer` 桩，`#[derive(Default)]`）：
  - `translate`：先判 `terminal` → 报「received a chunk after the [DONE] sentinel」；再
    **`event.data.trim() == "[DONE]"` 特判 → 置 `terminal=true` + 返回空事件**（在 `wire::decode` 之前，
    §4.4.1「JSON 解析前特判」，非 JSON 哨兵永不触发解析错误）；否则 `decode(&event.data)` 验证可解析
    （M3-2 替换为状态机产出，本任务产出 0 事件）。
  - `is_terminal → self.terminal`；`incomplete_error → "SSE body ended before the [DONE] sentinel"`
    （EOF 无哨兵报错；`is_terminal` 让 `common::normalize_sse` 的 unfold 自然终止流，无需额外机制）。
  - **不做 event/type 一致性检查**（与 `openai_resp` 的 type 校验相反）：chat/completions 无 `type` 判别
    字段、`event: message` 是常态，normalizer 直接忽略 `event.event` 字段。
- `stream/decoder.rs`（新建，照 `openai_resp/stream/decoder.rs`）：`normalize_sse` 包装
  `common::normalize_sse::<StreamNormalizer, …>` + `impl SseNormalizer for StreamNormalizer`
  （逐方法委托 + `invalid_sse → invalid_stream`）。
- `stream/mod.rs`（替换 M1-2 占位桩为真实接线，照 `openai_resp/stream/mod.rs`）：`mod decoder; mod normalizer;
  mod wire;` + `use decoder::normalize_sse;` + `chat_stream`（`stream` 守卫 → `build_request`（自动带
  `include_usage`）→ `common::execute_sse_response` → `normalize_sse`）+ `invalid_stream` +
  `#[cfg(test)] mod tests;`。rustdoc 写 10min connect+headers 限定、body 无总超时、`[DONE]` 正常终止 / EOF 报错。
- `stream/tests/mod.rs`（新建，最小骨架；M3-3 扩 parsing/transport/errors 子模块复用其 helper）：
  `decode_fixture` + `irregular_chunks` helper + 4 个测试：① `[DONE]` 正常 terminal 收尾、events 空、
  无 JSON 解析错误；② `event: message` 不触发一致性错误；③ EOF 无 `[DONE]` → `ClientError::Protocol`
  含「[DONE]」；④ `wire_decodes_each_delta_shape` 直接钉住 wire.rs（text/reasoning/tool_call/usage-only
  四种 chunk 的字段解析，覆盖所有建模字段，不让 wire 正确性推给 M3-2）。
- 验证结果（全绿）：
  - `cargo fmt --all`（无 diff，仅 fmt 重排 `normalizer.rs` 的 import 为单行）；
  - `cargo clippy --all-targets -- -D warnings`；
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  - `cargo test -p agent-lib --lib adapter::openai_chat`（32 通过：28 既有 request/response/mod +
    6 transport + 4 新增 stream，0.02s 秒级完成）；
  - `cargo test --all --all-targets`（全部 `test result:` 行 `0 failed`，lib 1090→1094 +4，
    集成/replay/smoke 套件全绿，无回归）；
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- **显式留给 M3-2**（避免重复或漏做）：把 `translate` 的占位 `decode(...)?; Ok(Vec::new())` 替换为状态机
  产出——文本/reasoning block 随字段开关、tool_calls 按 `index` 键控增量（位置派生 block id，先例
  `anthropic-block-{index}`，`src/adapter/anthropic/stream/normalizer.rs:423-424`）、末 chunk
  `finish_reason` → `MessageStop`（复用 `response/convert.rs::normalize_finish_reason`，抽到 crate-private
  共用位置）、空 `choices` usage chunk → 加性 `Usage`；并移除 wire.rs 的过渡 `#[allow(dead_code)]`。
- 无 breaking change：`mod.rs` 替换的是 M1-2 的 `ClientError::Other` 占位桩，`chat_stream` 签名不变；
  其余为纯新增 `pub(super)`/私有文件。`common/` 零改动。

### M3-2 [DONE] 流式状态机：normalizer.rs（文本/reasoning/工具增量/终态）

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

完成记录（2026-07-23）：

核心设计决策（逐条对 §4.4 关键差异 + 验证条件）：

1. **MessageStop 延迟到 `[DONE]`**（关键，解决 wire 顺序矛盾）：usage chunk 在 `finish_reason`
   之后到达（空 `choices` 独立 chunk），而 accumulator 契约要求 `Usage` 不能落在 `MessageStop`
   之后（`EventAfterMessageStop`）。故 `finish_reason` chunk **只缓存 `stop_reason`**（复用
   `normalize_finish_reason`），usage chunk 到达即发 `Usage`，`[DONE]` 时 flush：关闭所有打开的
   block + 发 `MessageStop`。`stop_reason` 仍来自 `finish_reason`（符合 §4.4.4「由末 chunk 的
   finish_reason 发 MessageStop」语义），且 `Usage` 严格在 `MessageStop` 前（accumulator 不报错）。
   EOF 无 `[DONE]` 走 `incomplete_error`，不 flush（不完整流）。
2. **每 kind 一个活跃 block + 统一 `[DONE]` 关闭**：`active_text`/`active_reasoning: Option<BlockId>`。
   `content` 续接同一 text block，`reasoning_content` 续接同一 reasoning block（DeepSeek 实际流
   reasoning 全→content 全，各只开一次，折叠后 content 顺序 = reasoning→text，与非流式 convert 一致）。
   block id：`text`/`reasoning`/`tool-call-{wire_index}`（位置派生稳定 id，先例 `anthropic-block-{index}`）。
   统一在 `[DONE]` 关闭（固定序 reasoning→text→tools by index），不在中途关，避免字段切换的开关复杂度。
3. **不发 `ToolInputAvailable`**（严格遵循 §4.4.2「`BlockStart(ToolInput)+Delta::Json+BlockStop`，
   绝不中途解析 JSON」）：适配器**零 JSON 解析**，让 accumulator 在 `BlockStop` 时自己解析 accumulated
   arguments（合法 fixture 下与非流式 `ToolUse` 一致）。与 `openai_resp` 不同（Responses 有
   `function_call_arguments.done` 显式边界才发 `ToolInputAvailable`），chat/completions 无此边界。
   测试 `single_tool_call_streams_argument_fragments_without_parsing` 显式断言事件序列无 `ToolInputAvailable`。
4. **tool_call 首片**：`id`/`function.name` 只在首 chunk（§4.4.2），`index` 首次出现 = 首片，必须带
   `id`+`function.name`（缺任一→`ClientError::Protocol`），开 `BlockStart(ToolInput{tool_name,tool_call_id})`；
   后续片（同 `index`）只发 `Delta::Json`（arguments 非空时）。多 `index` 并行按 wire `index` 独立维护
   `Vec<ToolCallState{index, block_id}>`，交错到达各自续接（测试 `parallel_tool_calls_interleave_by_index`）。
5. **role**：固定 `Role::Assistant`（chat/completions assistant 响应），但读 `delta.role` 字段验证
   若存在须为 `"assistant"`（移除 wire.rs role 的过渡 `#[allow(dead_code)]`，对齐 M2-1 `convert_message`
   的 role 验证）。

- **finish_reason 复用（无漂移）**：`normalize_finish_reason`（`response/convert.rs`）从 `pub(super)` 改
  `pub(crate)`；`response.rs` 的 `mod convert;` 改 `pub(crate) mod convert;`（其内部其余 `pub(super)`/私有
  helper 仍只在 response 可见，仅 `normalize_finish_reason` 对 crate 暴露）。stream 经
  `crate::adapter::openai_chat::response::convert::normalize_finish_reason` 引用，响应侧与流式终态共用同一份
  §4.3 映射表，无复制粘贴漂移。
- **wire.rs**：移除 5 个 struct 的过渡 `#[allow(dead_code)]`（M3-2 状态机读取所有字段），更新模块文档
  （删 M3-1 过渡说明）。
- **normalizer.rs**：`StreamNormalizer` 状态 `{ terminal, message_started, active_text, active_reasoning,
  tool_calls: Vec<ToolCallState>, stop_reason: Option<Normalized<StopReason>> }`。`translate`：terminal→报错；
  `[DONE]`→`ensure_message_started`+`close_open_blocks`+`MessageStop`(cached/`without_raw(Other)`)；否则 `decode`
  →usage 即时发 `Usage`→`choices[0]` `translate_choice`（`MessageStart` + role 验证 + content/reasoning/tool_calls
  增量 + `finish_reason` 缓存）。`is_terminal`/`incomplete_error` 沿用 M3-1。
- **decoder.rs / stream/mod.rs 零改动**：`translate`/`is_terminal`/`incomplete_error` 签名不变，
  `impl SseNormalizer` 委托照旧，`chat_stream` 接线（M3-1）不动。

验证结果（全绿）：

- `cargo fmt --all`（无 diff）；
- `cargo clippy --all-targets -- -D warnings`；
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
- `cargo test -p agent-lib --lib adapter::openai_chat`（40 通过：28 既有 request/response/mod/transport +
  12 stream，0.02s 秒级）；
- `cargo test --all --all-targets`（全部 `test result:` 行 `0 failed`，lib 1094→1102 +8，集成/replay/smoke
  套件全绿，无回归）；
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。

12 个 stream 测试（inline SSE 字符串经 `decode_fixture` 端到端，**不依赖 `.sse` fixture 文件**，断言精确
`StreamEvent` 序列）覆盖 6 场景 + finish_reason 全表 + role/空流：① 纯文本流（text block + Delta::Text* +
BlockStop + MessageStop）② reasoning 流（BlockKind::Reasoning + Delta::Reasoning，reasoning→text 顺序）③
单工具调用（首片 BlockStart(ToolInput{name,id})、arguments 逐片 Delta::Json、末尾 BlockStop、**中途无 JSON
解析/无 ToolInputAvailable**）④ 两个 index 交错并行工具调用 ⑤ 空 choices usage chunk → 加性 `Usage`
（finish_reason 后到达却在 MessageStop 前）⑥ finish_reason 全表（stop/length/tool_calls/content_filter/未知
→unknown/缺失→without_raw(Other)）⑦ `[DONE]` 关闭 block + MessageStop ⑧ `event: message` 不触发一致性错
⑨ 空流（仅 `[DONE]`）→ MessageStart + MessageStop ⑩ EOF 无 `[DONE]` → `Protocol` 含 "[DONE]" ⑪ 非 assistant
role → `Protocol` 含 "assistant" ⑫ wire 各 delta shape 解析。`decode_fixture` 签名改 `impl AsRef<str>`
（String/&str 都传，消除 needless_borrow）。

- **显式留给 M3-3**（避免重复或漏做）：`stream/tests/fixtures/*.sse`（脱敏录屏，`include_str!`）+
  `parsing.rs`（不规则字节分块 `[1,2,7,3,19,5,11]` 喂完整管线 + `Accumulator` 折叠与 M2 非流式 `parse_response`
  对照：文本/reasoning/tool_use/stop_reason/usage）+ `errors.rs`（哨兵终止/EOF 报错/SSE 错误帧）+ `transport.rs`
  （一次性 TcpListener SSE）。M3-2 的 inline 测试已钉死状态机正确性（单 chunk 序列 → 精确事件），M3-3 在其上加
  录屏级端到端 + 折叠一致性对照。
- 无 breaking change：`mod convert;`→`pub(crate) mod convert;` 与 `normalize_finish_reason` `pub(super)`→`pub(crate)`
  均为**放宽可见性**（向后兼容）；`wire.rs` 移除的是过渡 `#[allow(dead_code)]` 而非 API；其余纯新增 `pub(super)`/私有
  字段与方法 + 测试。

### M3-3 [DONE] 流式 fixtures + 端到端折叠对照 + transport

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

完成记录（2026-07-23）：

- **fixtures**（`stream/tests/fixtures/`，脱敏 demo，无真实 key/账号/内网地址——grep 已确认）：
  4 个 `.sse` + 4 个**配对** `.json`（同语义非流式 body，供折叠对照）。每 `.sse` 覆盖一类 §4.4 场景：
  ① `text_stream`（stop→EndTurn，cached_tokens）② `tool_stream`（双 `index` 并行，tool_calls→ToolUse）
  ③ `reasoning_stream`（reasoning→text，reasoner，reasoning_tokens）④ `usage_terminal`（`length`→MaxTokens，
  fixture 层补 finish_reason 覆盖，区别于 ①）。每个 `.sse` 都带「末个空 `choices` usage chunk + `[DONE]`」
  的真实 `include_usage` 终态，并以 `: end of recorded fixture` 注释行收尾（比照 `openai_resp` fixtures 惯例，
  且为 `data: [DONE]` 提供必需的尾随空行使 eventsource_stream 正确 dispatch 哨兵）。
- **`stream/tests/mod.rs`**（保留 M3-2 的 12 个 inline 状态机测试不动）：补 `fold_events`（共享 accumulator
  折叠）/`comparable`（清空 response-level extra）helper、4 个 `.sse` 的 `include_str!` 常量、
  `mod errors/parsing/transport;` 声明，更新模块 doc。transport-specific import（`AuthScheme`/`EndpointConfig`/
  `LlmClient`/`Message`/`ContentBlock`/`Map`）下沉 `transport.rs`，比照 `openai_resp` 的 import 分工，避免 mod.rs
  unused-import。
- **`stream/tests/parsing.rs`**（新建）：每个 fixture 一测——不规则字节分块 `[1,2,7,3,19,5,11]` 喂完整
  `normalize_sse` 管线 → `assert_eq!(events, vec![精确全序列])`（逐事件钉死 MessageStart/BlockStart/Δ/
  Usage/BlockStop/MessageStop，含 finish_reason→stop_reason 与 usage 细字段）→ `fold_events` 折叠 →
  与配对 `.json` 的 `OpenAiChatAdapter::parse_response` 经 `comparable`（清空 extra）对照 `assert_eq!`。
  另一 `usage_events_are_single_additive_segments…` 测试遍历 4 fixture 断言**恰好 1 个**加性 Usage 段且聚合 ==
  非流式 usage（§4.4.4/§7.1）。
- **折叠对照的 extra 差异处理**（关键）：流式 normalizer 不发 `ResponseMetadata` → folded `extra` 空；非流式
  `parse_response` 把 `choices`（含 `logprobs`）/`object`/`id`/`model`/`system_fingerprint` 全留 `extra`。故
  `comparable` 两边清空 response-level `extra`，只比 message(content blocks)/stop_reason/usage——per-block extra
  两边皆空（fixture 用合法 JSON arguments），content 可整体比对。
- **`stream/tests/errors.rs`**（新建，6 用例）：①直接 normalizer——`[DONE]` 后再喂 chunk → Protocol「after the
  [DONE] sentinel」（terminal guard，§4.4.1）②管线 EOF 无 `[DONE]` → Protocol「[DONE]」③`data: {not valid json`
  → `wire::decode` 失败 → Protocol（chat/completions 无 `type:"error"` 事件建模，畸形帧即最接近的「SSE 错误帧」）
  ④非法 UTF-8(`0xff`) → Protocol「valid UTF-8」⑤直接 normalizer——tool 首片缺 `id` → Protocol「must carry `id`」
  （§4.4.2，M3-R 健壮性清单）⑥空 `delta`+未知字段(`id`/`object`/`system_fingerprint`)+`[DONE]` → 干净
  MessageStart/MessageStop，不 panic。
- **`stream/tests/transport.rs`**（新建，6 用例，照 `openai_resp/stream/tests/transport.rs`）：一次性
  `TcpListener`（`bind("127.0.0.1:0")`）+ `collect_with_timeout`（5s 外层超时防回归卡死）。逐例断言：
  ①200+SSE（复用 `REAL_TEXT_STREAM`）→ 折叠 role=Assistant + usage 13/26（transport→decode→normalizer→fold 贯通），
  服务器侧断言 `POST /chat/completions` + `"stream":true` ②429+`Retry-After:4` → `RateLimited{Some(4s)}` 
  ③500 → `Api{status:500}`（含 `server_error`）④200+`application/json` → Protocol「application/json」（SSE
  content-type 守卫）⑤200+无 `[DONE]` 截断体 → 消费期 Protocol「[DONE]」（transport 层 EOF）⑥`request(false)` →
  Protocol「stream to be true」（stream 守卫先于 transport）。
- **零改动**：`stream/{mod.rs,decoder.rs,normalizer.rs,wire.rs}`、`response.rs`/`convert.rs`、`common/`、
  `error.rs` 均未动——M3-3 纯测试新增，状态机与传输接线在 M3-1/M3-2/M2-1 已完成，本任务只在其上钉录屏级端到端 +
  折叠一致性 + transport。
- 验证结果（全绿）：
  - `cargo fmt --all`（无 diff）；
  - `cargo clippy --all-targets -- -D warnings`；
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  - `cargo test -p agent-lib --lib adapter::openai_chat`（57 通过：40 既有 request/response/mod/transport/stream +
    5 parsing + 6 errors + 6 transport，0.03s 秒级）；
  - `cargo test --all --all-targets`（全部 `test result:` 行 `0 failed`，lib 1102→1119 +17，集成/replay/smoke 套件全绿，无回归）；
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- 无 breaking change：纯新增测试文件 + 测试模块声明 + 4 对脱敏 fixture；`stream/tests/mod.rs` 的改动仅为测试
  基础设施（helper + 常量 + 子模块声明 + import/doc），不动任何生产代码或公开 API。

### M3-R [DONE] M3 review：流式正确性核对

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

完成记录（2026-07-23）：

核对结论（逐条对 checklist）：

1. **设计文档 §4.4 四个关键差异逐条对照实现，全部命中**：
   - **§4.4.1 `[DONE]` 哨兵特判在 JSON 解析前**：`normalizer.rs:81` 的 `if event.data.trim() == "[DONE]"`
     分支**先于** `decode(&event.data)`（`:94`）执行——非 JSON 哨兵永不进入 `wire::decode`，故永不触发解析错误。
     模块级 rustdoc（`:76-80`）与 `wire.rs:10-11` 注释均显式标注该顺序。`done_sentinel_closes_open_blocks_and_emits_message_stop`
     + errors.rs `trailing_chunk_after_done_sentinel_is_rejected`（断言文案含 "after the [DONE] sentinel"）钉死 ✓
   - **§4.4.2 `index` 键控增量、绝不中途解析 JSON**：`push_tool_call`（`:229-256`）按 `delta.index` 在
     `tool_calls` Vec 里 `find` 已开 block；首片→`start_tool_call`（`:260-297`）要求 `id`+`function.name`
     缺一报 `Protocol`，发 `BlockStart(ToolInput{tool_name,tool_call_id})`；后续片只把 `function.arguments`
     原样 `Delta::Json(String)`（`:250-253`），**零 JSON 解析**。BlockId 位置派生 `tool-call-{index}`
     （`:284`，先例 `anthropic-block-{index}`）。`single_tool_call_streams_argument_fragments_without_parsing`
     显式断言事件序列无 `ToolInputAvailable`；`parallel_tool_calls_interleave_by_index` 钉双 `index` 并行交错 ✓
   - **§4.4.3 `reasoning_content` 落点正确**：`push_reasoning_delta`（`:191-197`）发 `BlockDelta{Delta::Reasoning}`
     + `reasoning_block_id`（`:214-225`）发 `BlockStart{BlockKind::Reasoning}`。`Delta::Reasoning(String)`
     仅携带文本（`src/stream/mod.rs:11`，无 signature 字段；`ReasoningSignature` 是 Anthropic 专用独立变体，
     chat/completions normalizer 从不产生）——符合「无 signature」。reasoning→text 顺序由
     `close_open_blocks` 固定序（reasoning 先于 text）保证，`reasoning_stream_emits_reasoning_block_before_text` 钉死 ✓
   - **§4.4.4 终态双源（finish_reason + usage chunk）无重复 MessageStop**：`finish_reason` chunk **只缓存**
     `stop_reason`（`translate_choice` 末尾 `:174-176`，复用 `normalize_finish_reason`），usage chunk 到达即发 `Usage`
     （`:98-102`），`MessageStop` **唯一**在 `[DONE]` 分支发一次（`:85-91`，带缓存/`without_raw(Other)` stop_reason）。
     grep 确认 `MessageStop` 全文件仅此一处产出 → 无重复。`terminal_usage_chunk_emits_additive_usage_before_stop`
     钉死「finish_reason 在前 chunk、usage 在后 chunk、`[DONE]` 才发 MessageStop」且 Usage 严格在 MessageStop 前
     （满足 accumulator `Usage` 不能落在 `MessageStop` 之后契约）。`stop_reason` 仍源自 `finish_reason`，语义未变 ✓
   - **关键设计决策核实（非 spec 偏差）**：§4.4.4 字面「由末 chunk 的 finish_reason 发 MessageStop」被实现为
     「finish_reason 缓存 stop_reason、MessageStop 延迟到 `[DONE]` flush」。这是为解决 usage chunk 在 finish_reason
     **之后**到达与 accumulator「Usage 必须在 MessageStop 前」契约的矛盾而做的**必要排序修正**——observable 行为
     （最终 Response 的 stop_reason 来自 finish_reason、usage 来自 usage chunk）与 spec 一致，且双源无重复 MessageStop
     完全满足 checklist 第 1 条。已在 normalizer 模块 doc（`:16-22`）与 M3-2 完成记录充分说明，非 workaround。

2. **与 M2 的一致性（finish_reason 单一映射 + 折叠对照存在且通过）**：
   - **映射表无漂移**：`response.rs:22 pub(crate) mod convert;`；`normalize_finish_reason`（`convert.rs:85`，`pub(crate)`）
     被非流式 `response.rs:99` 与流式 `normalizer.rs:175` 共用同一份 §4.3 映射表（`stop`→EndTurn / `length`→MaxTokens /
     `tool_calls`→ToolUse / `content_filter`→Refusal / 其它→unknown / 缺失→without_raw(Other)）。grep 确认全 crate
     无第二份 finish_reason 映射实现。流式 `finish_reason_maps_each_value_to_stop_reason` 覆盖全表（含 unknown/缺失），
     与 M2 非流式全表测试同源 ✓
   - **折叠对照测试存在且通过**：`stream/tests/parsing.rs` 4 个 fixture 各一测——不规则字节分块 `[1,2,7,3,19,5,11]`
     喂完整 `normalize_sse` 管线 → `assert_eq!(events, vec![精确全序列])` → `fold_events`（共享 `Accumulator`）折叠 →
     经 `comparable`（清空 response-level extra，因流式不发 `ResponseMetadata` 故 folded extra 空、非流式留
     `choices`/`object`/`id` 等）与配对 `.json` 的 `parse_response` 逐字段 `assert_eq!`。4 fixture 全部通过
     （text/tool/reasoning/usage_terminal）；另 `usage_events_are_single_additive_segments_matching_non_streaming_usage`
     断言每 fixture 恰好 1 个加性 Usage 段且聚合 == 非流式 usage（§4.4.4/§7.1）✓

3. **fixtures 脱敏检查（无真实 key/token/账号/内网地址）**：
   - stream fixtures（4 `.sse` + 4 配对 `.json`）全部 demo 值：id `chatcmpl-demo-*`、tool_call_id `call_demo_a`/`call_demo_b`、
     model `deepseek-chat`/`deepseek-reasoner`、`created` 为固定时间戳；`grep -E '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+'` 在 fixtures
     内**零命中**（无内网/公网 IP）✓
   - `grep -niE 'sk-|bearer |secret|api[_-]?key|password|token='` 命中均在**测试代码**而非 fixture：`mod.rs:131-139`
     的 Debug 脱敏断言（`sk-ant-secret` 是**断言被脱敏**的假密钥）、`request/tests.rs` 的假占位密钥 `sk-deepseek-secret`/
     `Bearer token`（验证 auth 头组装）、`response/tests/transport.rs:112` 的模拟错误 body 文案 "Incorrect API key"
     （非真实凭据）。与 M2-R 脱敏结论一致，无新泄漏 ✓

4. **状态机对乱序/缺失字段的健壮性（不 panic）**：
   - `normalizer.rs` **零 panic 路径**：grep `unwrap()/expect(/panic!/unreachable!` 零命中，全部走 `?` + 显式
     `ok_or_else`→`ClientError::Protocol` ✓
   - **缺 `id` 的后续 chunk**：`push_tool_call` 按 `index` 查已开 block，后续片不读 `id`（只读 `function.arguments`），
     故「后续 chunk 缺 id」是正常路径，不发错误更不 panic ✓
   - **首片缺 `id`**：`first_tool_call_fragment_without_id_is_rejected` 钉死 → `Protocol`「must carry `id`」（非 panic）✓
   - **空 delta + 未知字段**：`empty_delta_and_unknown_fields_terminate_cleanly`（errors.rs）——chunk 带 `delta:{}`
     + 未建模顶层字段（`id`/`object`/`system_fingerprint`）+ `[DONE]` → 干净 `MessageStart`+`MessageStop`，不 panic
     （wire.rs serde 默认忽略未建模字段）✓
   - **非法 UTF-8 / 畸形 JSON / 非法 usage**：分别 errors.rs `invalid_utf8_is_protocol_error`、`malformed_chunk_json_is_protocol_error`
     与 normalizer.rs:99-100 `invalid usage object` → 全部 `Protocol`，不 panic ✓

门禁输出摘要（全绿，2026-07-23 独立重跑）：

- `cargo fmt --all`：无 diff。
- `cargo clippy --all-targets -- -D warnings`：exit 0（PASS）。
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`：exit 0（PASS）。
- `cargo test -p agent-lib --lib adapter::openai_chat`：57 通过（12 request/mod + 10 response/parsing + 6 response/transport
  + 12 stream/mod inline + 5 stream/parsing + 6 stream/errors + 6 stream/transport），0.02s。
- `cargo test --all --all-targets`：全部 `test result:` 行 `0 failed`（lib 1119 通过；集成/replay/smoke 套件全绿，无 FAILED、无 panic）。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`：exit 0（PASS）。

发现的问题及处置：

- **无 spec 偏差、无 workaround、无未调度测试失败**。M3 流式实现（wire/decoder/normalizer + fixtures/折叠对照/errors/transport）
  与设计文档 §4.4 四个关键差异、§7.1 折叠对照要求完全一致。MessageStop 延迟到 `[DONE]` 是经核实的必要排序修正
  （解决 usage-after-finish_reason 与 accumulator 契约矛盾），非 spec 偏差。
- **范围外观察（非 M3 缺陷，不阻断，记备查）**：2026-07-23 全库安全审查（`docs/review-2026-07-23.md`）发现共享
  `Accumulator::apply_unknown_delta` 对 `stream_deltas` 非 Array 字段会 `expect` panic（H-ROB-1）。**chat/completions
  normalizer 只产生 Text/Reasoning/ToolInput/Usage/MessageStart/MessageStop 事件，从不产生 `ContentBlock::Unknown`**，
  故该 panic 经 chat/completions 流式路径**不可达**，与本 M3 review 正交；属独立全库审查线，不在 openai_chat M1–M5
  任务范围内，按任务规则不抢占当前 TODO 顺序，留给单独的安全修复批次处置。

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
