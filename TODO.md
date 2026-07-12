# TODO:Client 层实现任务列表

> 依据 `PLAN.md`。任务按实现顺序编号(M<里程碑>-<序号>)。
> 标题含 `[TODO]` = 未完成;完成后由 coding agent 改为 `[DONE]`。
> 每个任务含足够上下文,实现时无需反复搜索代码库。
> 通用约定见 `PLAN.md` 的"已定关键决策"——所有任务都必须遵守,不再逐条重复。

参考文档:`DESIGN.md`、`docs/conversation-core.md`、`docs/client-layer-references.md`、`docs/genai-probe-findings.md`。
真实 endpoint 参数见 `PLAN.md` 的"测试与真实环境"与仓库 `.envrc`。

---

## Milestone 1 — 基础数据模型(完整态类型)

### M1-1 [DONE] 建立 crate 骨架与依赖
**上下文**:当前 `src/lib.rs` 基本为空,`Cargo.toml` 无依赖,edition 2024。按 `PLAN.md` 的目录结构建立模块树。
**做什么**:
- 在 `Cargo.toml` 添加依赖:`serde`(derive)、`serde_json`、`tokio`(full)、`async-trait`、`thiserror`、`futures`。HTTP 客户端(`reqwest`,rustls)可留到 M4 再加。
- 建立 `PLAN.md` 目录结构里的模块文件与 `mod` 声明(空模块 + 文档注释即可):`client/`、`model/`、`stream/`、`adapter/`。
- `lib.rs` 用 `//!` 写 crate 级文档,列出三层架构定位(本 crate 目前只做 Client 层)。
**验证**:
- `cargo build` 通过,无 warning。
- `cargo doc --no-deps` 生成成功。
- 模块树与 `PLAN.md` 一致。
**完成记录**:
- 2026-07-12: 添加基础依赖,建立 `client`/`model`/`stream`/`adapter` 模块树及文档注释。
- 验证通过:`cargo fmt`,`cargo clippy --all-targets -- -D warnings`,`cargo build`,`cargo doc --no-deps`,`cargo test --all --all-targets`。

### M1-2 [DONE] `Role` 与 `Normalized<T>` + `StopReason`
**上下文**:逃生舱 (C),见 `DESIGN.md` §4(C)。`Normalized<T>` = 归一化枚举值 + 保留 provider 原始字符串,映射不上时 value=Unknown/Other 且 raw 留证据。这是全项目最基础的防御性类型,先做。
**做什么**:
- `model/normalized.rs`:`struct Normalized<T> { value: T, raw: Option<String> }`,`serde` 派生,`T: Serialize+Deserialize`。提供构造:`from_mapped(value, raw)`、`unknown(raw)`。
- `model/message.rs`:`enum Role { User, Assistant, System, Tool }`(serde rename 到小写)。
- `model/normalized.rs`:`enum StopReason { ToolUse, EndTurn, MaxTokens, StopSequence, Refusal, Other }`;约定 `Normalized<StopReason>` 为标准用法。
**验证**:
- 单元测试:`Normalized<StopReason>` 从 `"tool_use"` → `ToolUse` 且 raw=`Some("tool_use")`;从未知 `"weird"` → `Other` 且 raw 保留。
- serde round-trip 测试通过。
**完成记录**:
- 2026-07-13: 实现 `Normalized<T>`、`StopReason` 原始字符串归一化与 `Role` 小写 wire name,并添加 focused serde/unit tests。
- 验证通过:`cargo fmt --all`,`cargo clippy --all-targets -- -D warnings`,`cargo test --all --all-targets`。

### M1-3 [DONE] `Usage`
**上下文**:`DESIGN.md` §5 Usage 一等公民;探测实证 Anthropic 返回 `input_tokens`/`output_tokens`/`cache_creation_input_tokens`/`cache_read_input_tokens`,OpenAI 用 `prompt_tokens`/`completion_tokens` + `completion_tokens_details.reasoning_tokens`。cache_read/cache_write/reasoning **必须单列**,不得揉进 input。
**做什么**:
- `model/usage.rs`:`struct Usage { input: u32, output: u32, cache_read: u32, cache_write: u32, reasoning: u32, total: Option<u32> }`,字段用 `#[serde(default)]`。
- 加 `#[serde(flatten)] extra: Map<String, Value>`(逃生舱 B)兜底未建模字段。
- 提供 `merge`(流式累加)与 `total_computed()` 辅助。
**验证**:
- 单元测试:分别用 Anthropic 与 OpenAI 的真实 usage JSON 片段反序列化,断言 cache/reasoning 落到正确字段。
- 未知字段进入 `extra` 而非丢失。
**完成记录**:
- 2026-07-13: 实现 provider-neutral `Usage` 类型,支持 Anthropic/OpenAI usage 字段归一化、cache_read/cache_write/reasoning 单列、`extra` 逃生舱、流式 `merge` 与 `total_computed()`。
- 验证通过:`cargo fmt --all`,`cargo clippy --all-targets -- -D warnings`,`cargo test --all --all-targets`。

### M1-4 [DONE] `ContentBlock` 与多模态承载
**上下文**:`DESIGN.md` §5;参考 Anthropic 块分类(`docs/client-layer-references.md`)。这是"完整态"块(区别于 M2 的流式增量态)。
**做什么**:
- `model/content.rs`:`enum ContentBlock { Text{text}, Image{source}, ToolUse{id,name,input:Value}, ToolResult{tool_use_id,content,is_error}, Thinking{text,signature:Option<String>} }`。
- `Image.source`:`enum ImageSource { Url(String), Base64{media_type,data} }`(承载两种方式,见 DESIGN.md 多模态承载)。
- `ToolResult.content` 用 `Vec<ContentBlock>`(ToolResponse 也是多模态的,见 §2.1)。
- 每个变体加逃生舱 (B) 兜底(视需要)。`ContentBlock` 用 `#[serde(tag="type")]` 贴近 wire。
- `thinking` 保留 `signature`(探测发现 genai 丢了它,我们不丢)。
**验证**:
- serde round-trip 覆盖每个变体。
- 反序列化一段含 text+tool_use 的真实 Anthropic content 数组成功。
**完成记录**:
- 2026-07-13: 实现完整态 `ContentBlock` 与 `ImageSource`,覆盖 text/image/tool_use/tool_result/thinking,支持 URL/base64 图片、多模态 tool result、thinking signature 保留,并为块与图片 source 保留 flatten `extra` 逃生舱。
- 验证通过:`cargo fmt --all`,`cargo clippy --all-targets -- -D warnings`,`cargo test --all --all-targets`。

### M1-5 [DONE] `Message` 与 `Tool`(schema)/ `ToolCall` / `ToolResponse`
**上下文**:`Message` 是 Turn 的内容物(见 `conversation-core.md` §2.3,中立层)。`Tool` 定义含 JSON schema。`ToolCall`/`ToolResponse` 是统一 data model(`DESIGN.md` §2.1),ToolResponse 要能表达非正常结果(需审批/被拒/出错)。
**做什么**:
- `model/message.rs`:`struct Message { role: Role, content: Vec<ContentBlock> }`(content 是 Vec,不是 String)。**本层不含 MessageId**——id 归 Conversation 层(见 conversation-core.md),client 层 Message 保持 wire-neutral 纯数据。
- `model/tool.rs`:`struct Tool { name, description, input_schema: Value }`(schema 先用 `serde_json::Value`,`schemars` 派生留后续)。
- `struct ToolCall { id, name, input: Value }`;`struct ToolResponse { tool_call_id, content: Vec<ContentBlock>, status: ToolStatus }`;`enum ToolStatus { Ok, Error, Denied, Cancelled }`(对应 DESIGN.md 非正常结果 + Vercel `tool-output-denied`)。
**验证**:
- serde round-trip。
- 构造一个含 tool 的 `Message` 序列并断言结构。
**完成记录**:
- 2026-07-13: 实现不含 Conversation `MessageId` 的完整态 `Message`,以及 JSON schema `Tool`、`ToolCall`、支持多模态内容与 Ok/Error/Denied/Cancelled 状态的 `ToolResponse`。
- 验证通过:`cargo fmt --all`,`cargo test model::message::tests`,`cargo test model::tool::tests`,`cargo clippy --all-targets -- -D warnings`,`cargo test --all --all-targets`(26 passed)。

### M1-6 [DONE] `ProviderExtras`(逃生舱 A)与 `ProviderId`
**上下文**:`DESIGN.md` §4(A);请求侧方言口袋,绑定 ProviderId,只在序列化最后一步 merge。优先级低但类型先立。
**做什么**:
- `model/extras.rs`:`enum ProviderId { Anthropic, OpenAiResp, /* 可扩展 */ }`;`struct ProviderExtras { provider: ProviderId, fields: Map<String,Value> }`。
- 提供 `merge_into(&self, body: &mut Value, target: ProviderId)`:仅当 target 匹配时合并,不匹配返回可观测的忽略(日志/错误按 DESIGN.md 约定)。
**验证**:
- 单元测试:provider 匹配时字段合并进 body;不匹配时不合并。
**完成记录**:
- 2026-07-13: 实现可 serde 的 `ProviderId` 与 `ProviderExtras`,支持最终请求序列化阶段的字段合并、同名字段覆盖、provider 不匹配的可观测 no-op,并对非对象请求体返回明确错误。
- 验证通过:`cargo fmt --all`,`cargo clippy --all-targets -- -D warnings`,`cargo test model::extras::tests`(4 passed),`cargo test --all --all-targets`(30 passed)。

### M1-R [TODO] Milestone 1 Review
**做什么**:核对 M1 全部产出。
**验证清单**:
- 所有 M1 类型 `serde` round-trip 测试齐全且通过。
- Normalized/Usage/ContentBlock 与 DESIGN.md §4/§5 描述一致;cache/reasoning 单列;thinking 保留 signature。
- Message 不含 id(id 归 Conversation 层)这一决定已落实并注释。
- 逃生舱 B(flatten extra)在 Usage/ContentBlock 上生效;逃生舱 A/C 类型就位。
- `cargo build` / `cargo test` / `cargo doc` 全绿,无 warning,公共类型有文档注释。
- 更新本文件:M1 任务标记 `[DONE]`。

---

## Milestone 2 — 流式事件与聚合

### M2-1 [TODO] `BlockId` / `BlockKind` / `Delta`
**上下文**:`docs/client-layer-references.md` 决策 4/5——块用稳定 id、三段式同构。Delta 区分 text/json/reasoning。
**做什么**:
- `stream/mod.rs`:`struct BlockId(String)`(稳定 id,可由适配器从 index 映射生成)。
- `enum BlockKind { Text, Reasoning, ToolInput { tool_name: String, tool_call_id: String } }`。
- `enum Delta { Text(String), Json(String), Reasoning(String) }`(Json = tool 参数的原始片段,累积用)。
**验证**:serde round-trip;类型文档注释说明"Json delta 不可边流边 parse"。

### M2-2 [TODO] `StreamEvent`
**上下文**:`docs/client-layer-references.md` 的 StreamEvent 草案 + 决策 7(只归一化 wire 真实事件,不含 approval/abort/pivot)。
**做什么**:
- `stream/mod.rs`:
```
enum StreamEvent {
  MessageStart { role: Role },
  BlockStart { id: BlockId, kind: BlockKind },
  BlockDelta { id: BlockId, delta: Delta },
  BlockStop  { id: BlockId },
  ToolInputAvailable { id: BlockId, input: serde_json::Value },
  Usage(Usage),
  MessageStop { stop_reason: Normalized<StopReason> },
  Error(ClientError),   // ClientError 若此时未定义,先用占位/String,M3 回填
}
```
- 文档注释标注每个变体对应的 Vercel v5 part(可追溯)。
**验证**:serde round-trip;编译通过(ClientError 占位可接受,M3 回填)。

### M2-3 [TODO] `Accumulator`:StreamEvent → 完整 Response
**上下文**:`DESIGN.md` §5 streaming 纪律 3(流可折叠)、`conversation-core.md` §5(PartialBlock/HashMap<index/id>)。这是 streaming 归一化的心脏,逻辑只写一份,两适配器复用。
**做什么**:
- 先定义 `Response`(若 M3 未定义):`struct Response { message: Message, usage: Usage, stop_reason: Normalized<StopReason>, extra: Map }`。
- `stream/accumulator.rs`:`struct Accumulator { blocks: HashMap<BlockId, PartialBlock>, order: Vec<BlockId>, usage, stop_reason }`。
- `PartialBlock`:按 kind 累积;Text/Reasoning 累加字符串;ToolInput 累加 Json 片段字符串,在 `ToolInputAvailable`(优先)或 `BlockStop` 时 parse 成 `Value`(parse 失败要产出明确错误,不 panic)。
- `fn push(&mut self, ev: StreamEvent) -> Result<()>` 与 `fn finish(self) -> Result<Response>`。
- 提供便捷:`async fn collect(stream) -> Result<Response>` 消费整条流。
**验证**:
- 单元测试:手工构造事件序列(含交错的两个 block id、tool JSON 分片)→ 折叠出正确 Response。
- tool 参数分 3 个 Json delta 累积后正确 parse。
- partial JSON(缺尾)→ finish 返回错误而非 panic。
- 空流、仅 usage、错误事件的边界处理。

### M2-R [TODO] Milestone 2 Review
**验证清单**:
- 三段式同构在 text/reasoning/tool 上都成立;Accumulator 只有一份折叠逻辑。
- 纪律 1(id 关联)、纪律 2(累积后 parse)、纪律 3(可折叠)均由测试覆盖。
- 交错 block、并行 tool call 的折叠正确。
- `cargo test` 全绿;更新本文件 M2 标记 `[DONE]`。

---

## Milestone 3 — Client 抽象(trait / capability / error / config)

### M3-1 [TODO] `ClientError` 分类
**上下文**:`DESIGN.md` §5 统一 error model;retry/backoff 依赖分类。回填 M2-2 的占位。
**做什么**:
- `client/error.rs`(`thiserror`):`enum ClientError { RateLimited { retry_after: Option<Duration> }, Timeout, ContextLengthExceeded, ContentFiltered, Network(..), Protocol(..), Auth, Api { status: u16, body: String }, Other(..) }`。
- 提供从 HTTP status + body 分类的构造辅助(429→RateLimited 且解析 retry-after;探测见 Foundry 401/404/content_filter 形态)。
**验证**:单元测试:各 HTTP 状态/响应体 → 正确分类;429 的 retry-after 解析。

### M3-2 [TODO] `Capability`(结构化)
**上下文**:`DESIGN.md` §5 Capability 非布尔标志。来源=默认表 + 可覆盖。
**做什么**:
- `client/mod.rs`:`struct Capability { max_context_tokens: Option<u32>, input_modalities: Set<Modality>, output_modalities: Set<Modality>, streaming: bool, tool_calling: bool, parallel_tool_calls: bool, prompt_caching: bool, reasoning: bool, structured_output: bool, stop_reasons: Set<StopReason> }`。
- `enum Modality { Text, Image, Audio, File }`。
**验证**:serde round-trip;构造 Anthropic/OpenAI 各一个默认 Capability 常量并断言字段。

### M3-3 [TODO] `EndpointConfig` 与请求参数类型
**上下文**:`DESIGN.md` §1.1 endpoint config(base_url/auth/方言开关),独立于 wire protocol。探测证明认证方式因 endpoint 而异(Bearer vs api-key vs x-api-key)、需自定义 query param。
**做什么**:
- `client/mod.rs`:`struct EndpointConfig { base_url: String, auth: AuthScheme, query_params: Vec<(String,String)>, extra_headers: Vec<(String,String)> }`。
- `enum AuthScheme { Bearer(String), Header { name: String, value: String }, None }`(覆盖 Foundry 的 Bearer / api-key)。
- `struct ChatRequest { model: String, messages: Vec<Message>, tools: Vec<Tool>, system: Option<String>, max_tokens, temperature, stream: bool, provider_extras: Option<ProviderExtras>, ... }`(system 单列,归一化两家差异,见 conversation-core §1.2)。
**验证**:serde round-trip;构造两个真实 endpoint 的 config 并断言。

### M3-4 [TODO] `LlmClient` trait
**上下文**:`DESIGN.md` 一律 `#[async_trait]` + dyn-safe;两种消费姿势(流式 / collect 完整)。
**做什么**:
- `client/mod.rs`:
```
#[async_trait]
trait LlmClient: Send + Sync {
  fn capability(&self) -> &Capability;
  async fn chat(&self, req: ChatRequest) -> Result<Response, ClientError>;             // 内部可走流式+Accumulator
  async fn chat_stream(&self, req: ChatRequest)
      -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError>;
}
```
- 确认 `Box<dyn LlmClient>` 可用(dyn-safe)。
**验证**:写一个 mock 实现 + 断言可 `Box<dyn LlmClient>`;`chat` 默认实现(基于 chat_stream + Accumulator)可选。

### M3-R [TODO] Milestone 3 Review
**验证清单**:
- error 分类覆盖 retry 所需;Capability 结构化无布尔堆砌;EndpointConfig 能表达两个真实 endpoint 的认证/query 差异。
- trait dyn-safe,`Box<dyn LlmClient>` 编译通过。
- M2-2 的 ClientError 占位已回填为真类型。
- `cargo test` 全绿;更新本文件 M3 标记 `[DONE]`。

---

## Milestone 4 — Anthropic 适配器

### M4-1 [TODO] 接入 HTTP 客户端与 Anthropic 请求构造
**上下文**:真实 endpoint 见 `PLAN.md`:base `ANTHROPIC_BASE_URL`,`Authorization: Bearer $ANTHROPIC_AUTH_TOKEN`,`anthropic-version: 2023-06-01`,`content-type: application/json`,model `databricks-claude-haiku-4-5`,路径 `POST {base}/v1/messages`。探测代码 `probes/genai-probe/` 有可用写法。
**做什么**:
- `Cargo.toml` 加 `reqwest`(rustls、json、stream 特性)。
- `adapter/anthropic/`:把 `ChatRequest` 序列化成 Anthropic body(system 单列字段、messages、tools 的 Anthropic schema 格式、max_tokens 必填)。
- 应用 `EndpointConfig`(base_url + AuthScheme + 额外 header + query)。
**验证**:单元测试:`ChatRequest` → Anthropic body JSON 结构正确(快照/字段断言),不实际联网。

### M4-2 [TODO] Anthropic 非流式响应 → `Response`
**上下文**:探测实测响应含 `content[]`(text/tool_use blocks)、`stop_reason`、`usage`(含 cache_creation/cache_read 细分与 `cache_creation.ephemeral_5m/1h`)。方言字段走逃生舱 B。
**做什么**:
- 解析 Anthropic 响应 → `Response`:content blocks → `Vec<ContentBlock>`;`stop_reason` → `Normalized<StopReason>`(保留 raw);usage 映射到单列字段,cache 细分正确归位;未知字段进 extra。
**验证**:
- 单元测试:用探测记录的真实响应 JSON 反序列化 → 断言 text/tool_use、stop_reason 归一化 + raw、usage 各字段。
- `#[ignore]` 集成测试:真实调用 `databricks-claude-haiku-4-5` 说 "hi",断言拿到文本与 usage。

### M4-3 [TODO] Anthropic 流式(SSE)→ `StreamEvent`
**上下文**:Anthropic SSE 事件:`message_start`/`content_block_start`/`content_block_delta`(text_delta / input_json_delta / thinking_delta)/`content_block_stop`/`message_delta`(带 stop_reason+usage)/`message_stop`。**决策 4**:把 Anthropic 的 `index` 映射成稳定 `BlockId`。**纪律 2**:input_json_delta 只累积。
**做什么**:
- `adapter/anthropic/` 流式:解析 SSE,产出归一化 `StreamEvent`:
  - `content_block_start` → `BlockStart{ id=map(index), kind }`(kind 依 block 类型:text→Text、thinking→Reasoning、tool_use→ToolInput{name,tool_call_id})。
  - `content_block_delta`:text_delta→`BlockDelta{Delta::Text}`;input_json_delta→`BlockDelta{Delta::Json}`;thinking_delta→`BlockDelta{Delta::Reasoning}`。
  - `content_block_stop` → `BlockStop`;tool_use 块在 stop 时(或积累完)发 `ToolInputAvailable`。
  - `message_delta`/`message_stop` → `Usage` + `MessageStop{stop_reason}`。
- index→BlockId 映射在适配器内维护。
**验证**:
- 单元测试:喂探测记录的真实 SSE 分片 → 断言 StreamEvent 序列 + id 关联正确。
- 经 Accumulator 折叠 → Response 与非流式结果结构一致。
- `#[ignore]` 集成测试:真实流式 "count 1..5" 与 tool call(get_weather Tokyo),断言事件序列 + 折叠结果(对照探测输出:tool 参数 `{"city":"Tokyo"}`)。

### M4-R [TODO] Milestone 4 Review
**验证清单**:
- 非流式与流式折叠结果一致(同一 prompt)。
- index→稳定 id 映射正确;tool JSON 仅累积后 parse;thinking signature 保留。
- Foundry 方言字段(cache_creation.ephemeral_*、其他)未丢失(进 extra)。
- 真实集成测试(非流式/流式/tool)通过(有环境变量时)。
- 更新本文件 M4 标记 `[DONE]`。

---

## Milestone 5 — OpenAI Response 适配器

### M5-1 [TODO] OpenAI Response 请求构造与非流式响应
**上下文**:真实 endpoint:base `OPENAI_BASE_URL`,header `api-key: $OPENAI_API_KEY`,query `?api-version=2025-04-01-preview`,model `gpt-5.5`,路径 `POST {base}/responses`。探测实测响应含 `content_filters`(Azure 特有,走逃生舱 B)、Response API 的 `output[]` 结构。
**做什么**:
- `adapter/openai_resp/`:`ChatRequest` → Response API body(`input`/`instructions`/`tools`/`max_output_tokens`)。
- 非流式响应 `output[]`(message/reasoning/function_call items)→ `Response`;usage(`input_tokens`/`output_tokens`/`output_tokens_details.reasoning_tokens`)→ 单列;`content_filters` 等入 extra;stop 状态 → `Normalized<StopReason>`。
**验证**:
- 单元测试:请求 body 结构;用探测记录的真实响应 JSON 解析 → 断言。
- `#[ignore]` 集成测试:真实 `gpt-5.5` 说 "hi"。

### M5-2 [TODO] OpenAI Response 流式(SSE)→ `StreamEvent`
**上下文**:Response API 事件 `response.output_item.added` / `response.*.delta`(text/function_call arguments/reasoning) / `response.output_item.done` / `response.completed`(usage)。映射到统一 StreamEvent(见 `docs/client-layer-references.md` 对照:added→BlockStart、delta→BlockDelta、done→BlockStop)。function_call 的 `arguments` delta 只累积(纪律 2)。用稳定 BlockId 关联(item_id/output_index)。
**做什么**:
- 解析 Response SSE → `StreamEvent`,与 Anthropic 适配器产出**同构**的事件流(以便 Accumulator 复用)。
- item_id/index → BlockId 映射;arguments delta→`Delta::Json`;reasoning delta→`Delta::Reasoning`;完成时 `ToolInputAvailable`。
**验证**:
- 单元测试:真实 SSE 分片 → StreamEvent 序列;经 Accumulator 折叠一致。
- `#[ignore]` 集成测试:真实流式文本 + tool call,断言事件序列与折叠结果。

### M5-R [TODO] Milestone 5 Review
**验证清单**:
- 两适配器产出的 StreamEvent **同构**,同一 Accumulator 均可折叠。
- Azure 方言字段(content_filters 等)进 extra 未丢。
- reasoning/tool 累积规则与 Anthropic 一致。
- 真实集成测试通过(有环境变量时)。
- 更新本文件 M5 标记 `[DONE]`。

---

## Milestone 6 — 跨 provider 验收

### M6-1 [TODO] 归一化一致性集成测试
**上下文**:两 provider 经统一 `LlmClient` 应产出结构一致的归一化结果。
**做什么**:
- `tests/`:参数化测试,对 Anthropic 与 OpenAI Response 各跑:纯文本、多轮、tool call 往返(执行 tool 后回灌 result 再请求),断言归一化结构一致(role/content/stop_reason/usage 字段存在且合理)。
- 通过 `Box<dyn LlmClient>` 调用,证明 dyn 抽象可用。
**验证**:两 provider 均通过同一套断言(有环境变量时);无 provider 特判逻辑泄漏到测试断言层。

### M6-2 [TODO] 能力矩阵与逃生舱实证
**做什么**:
- 文档 `docs/capability-matrix.md`:记录两 provider 的 Capability 默认值与实测差异。
- 测试:断言各 provider 的方言字段确实落入 extra(Foundry cache_creation.ephemeral_*、Azure content_filters),证明逃生舱 B 生效、无信息丢失。
**验证**:能力矩阵与实测一致;逃生舱测试通过。

### M6-3 [TODO] 使用示例与 crate 文档
**做什么**:
- `examples/`:非流式、流式打字机、tool call 往返各一个可运行示例(读环境变量选 provider)。
- 完善 `lib.rs` crate 文档与公共 API 文档注释;README 增加 Client 层用法与配置说明。
**验证**:`cargo run --example ...` 全部可运行(有环境变量时);`cargo doc` 无缺失文档 warning。

### M6-R [TODO] Milestone 6 / Client 层总 Review
**验证清单**:
- 全量 `cargo test`(含 `-- --ignored` 真实集成)通过。
- 归一化一致性、逃生舱、能力矩阵、示例齐备。
- 回溯 DESIGN.md §5 Client 层约束逐条满足:Capability 结构化 / error 分类 / usage 一等 / ContentBlock / streaming 三纪律 / 三逃生舱。
- 确认 Client 层为 Conversation 层提供了完备类型(Message/ContentBlock/StreamEvent/Usage),可开始 Conversation 层实现。
- 更新本文件 M6 标记 `[DONE]`;在 PLAN.md 记录 Client 层完成。
