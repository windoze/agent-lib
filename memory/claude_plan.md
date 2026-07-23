## Execution Plan — M5-1：文档同步（DESIGN.md 决策反转 + capability-matrix + README + AGENTS.md + client-layer-references）

TODO.md 第一个未完成任务：**M5-1 [TODO]**（M1-1~M4-R 全部 [DONE]）。
这是**纯文档同步任务**，定义来源是设计文档 `docs/openai-chat-api.md` §8 的同步清单。
不新增功能、不改生产代码，只改 `*.md` 文档（+ 顺手核对 `src/lib.rs`/`src/adapter/mod.rs` 注释一致性，纯注释核对）。

### 任务范围（逐条对 TODO M5-1 实现要求）
1. **`DESIGN.md` §1.1 决策反转（必须做）**：协议清单加 chat/completions；删除/修订「不支持」段 →「经 `openai_chat` 适配器支持，方言策略见 `docs/openai-chat-api.md`」；DeepSeek、vLLM 协议归类从 Anthropic 移到 chat/completions。
2. **`docs/capability-matrix.md`**：协议级默认值表加 chat/completions 列（与 `OPENAI_CHAT_DEFAULT_CAPABILITY` 一致）；新增 DeepSeek/vLLM 实测一节（思考模式、400 规则、vLLM 回放兼容性——引用 M4-3 实测结论，vLLM 未实测如实标注）。
3. **`README.md`**：provider 选择段落加 chat/completions；ignored 测试命令加 `cargo test --test integration_openai_chat -- --ignored --nocapture`。
4. **`AGENTS.md`**：`src/` 布局 `adapter/` 描述加 openai_chat；「Required environment」表加 `OPENAI_CHAT_BASE_URL`/`OPENAI_CHAT_API_KEY`/`VLLM_*`（注明可选/跳过语义）。
5. **`docs/client-layer-references.md`**：参考分工总表加一行（可参考 `async-openai` 的 chat 模块）。
6. **顺手核对** `src/lib.rs` 与 `src/adapter/mod.rs` 的协议清单注释与实际一致（M4-1 已改 lib.rs，需确认 mod.rs）。

### 验证条件
- 文档中的命令、env 变量名、文件路径与代码实际**逐条对照，不凭记忆**。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过；`cargo fmt --all` 无 diff。
- 因纯 `.md` 改动（+注释核对），**无需重跑全量测试套件**（沿用 M4-R 绿基线，注明 skip）。

### 关键数据源（实测结论引用，来自 M4-3 完成记录 TODO.md:1252-1301）
- DeepSeek 实测：4 用例全过（非流式 text+usage / 流式 text delta+usage / thinking 模式 Thinking block / **thinking 多轮+工具调用 round-2 回放 reasoning_content+tool_calls 不 400**）。
- 真实 spec 细节 2 条：① chat/completions `tool_choice` 必须嵌套 `{"type":"function","function":{"name":...}}`（非 Responses 扁平形）；② DeepSeek 思考模式拒 `tool_choice` 字段 → 用强指令 system prompt 自然触发工具调用。
- §5.1 400 规则验证成立；thinking_extras passthrough 确认。
- vLLM 未实测（无 `VLLM_BASE_URL`/凭据，2 测试干净 skip）→ 如实标注「待实测」。
- env 约定：DeepSeek `DEEPSEEK_API_KEY`(必需)/`DEEPSEEK_BASE_URL`(默认 https://api.deepseek.com)/`DEEPSEEK_MODEL`；vLLM `VLLM_BASE_URL`(必需)/`VLLM_API_KEY`(缺省 None)/`VLLM_MODEL`；facade `OPENAI_CHAT_BASE_URL`(必需)/`OPENAI_CHAT_API_KEY`(可选)。

### OPENAI_CHAT_DEFAULT_CAPABILITY 字段（capability-matrix 列必须与此一致，来自 M1-1）
`max_context_tokens: None`；`input_modalities: {Text, Image}`；`output_modalities: {Text}`；
`streaming/tool_calling/parallel_tool_calls/reasoning = true`；`prompt_caching/structured_output = false`；
`stop_reasons: {ToolUse, EndTurn, MaxTokens, StopSequence, Refusal}`。

### 执行步骤
1. 读取所有目标文档 + 设计文档 §8 同步清单 + capability 静态实际值（逐条对照，不凭记忆）。
2. 逐文件编辑（小而精准的 patch，每个文件一组改动）。
3. 跑 `cargo fmt --all --check` + `cargo doc --no-deps --workspace`（注释核对可能触及 src/lib.rs/adapter/mod.rs 但 M4-1 已确认 lib.rs，若 mod.rs 需改属注释）。
4. 标记 TODO M5-1 [TODO]→[DONE] + 完成记录。
5. commit。
6. stop。

### 进度日志
- [x] 读取文档 + 设计 §8 + 代码实际值（capability.rs:106-123 确认 OPENAI_CHAT_DEFAULT_CAPABILITY；config.rs/normalization/integration 测试确认 env 名；lib.rs/adapter/mod.rs 注释核对一致无需改）。
- [x] 逐文件编辑（DESIGN.md §1.1 决策反转 / capability-matrix 加列+实测节 / README 三处口径+provider段+ignored命令 / AGENTS adapter+env表 / client-layer-references 加行）。
- [x] 门禁：`cargo fmt --all --check` exit0；`cargo doc --no-deps --workspace -D warnings` Finished+Generated exit0；逐条对照 env/路径/命令 vs 代码全一致。全量 test 套件未重跑（纯 .md，沿用 M4-R 绿基线）。
- [x] TODO M5-1 [TODO]→[DONE] + 完成记录（逐文件+逐条对照+门禁摘要）。commit + stop。

---

## Execution Plan — M5-2：e2e 手搓 DeepSeek 客户端替换为 OpenAiChatAdapter

TODO.md 第一个未完成任务：**M5-2 [TODO]**（M1-1~M5-1 全部 [DONE]）。
两个 `#[ignore]` 真实 CLI e2e 各有一份**手搓**非流式 DeepSeek chat/completions 客户端：
- `tests/agent_external_real_e2e.rs:144-388`（DeepSeekConfig/chat_url/wire 类型/chat_messages/normalize_finish_reason/DeepSeekLlmHandler）
- `tests/agent_external_managed_real_e2e.rs:217-442`（同款，整个文件 `#![cfg(all(feature=external-claude-code, feature=external-codex))]` 门控）

### 替换策略
保留 `DeepSeekLlmHandler`（实现 `LlmHandler` trait，返回 `RequirementResult::Llm`，是 e2e 包装层 + logging），
**只把手搓 reqwest POST + wire 反序列化替换为委托 `OpenAiChatAdapter::chat()`**。
- `DeepSeekLlmHandler` 字段 `http: reqwest::Client` → `adapter: OpenAiChatAdapter`。
- `new(config, log)`：用 `EndpointConfig { base_url, auth: AuthScheme::Bearer(api_key), query_params:Vec::new(), extra_headers:Vec::new() }` 构造 `OpenAiChatAdapter::new(endpoint)`（Bearer 直连，与手搓 `bearer_auth` 等价；adapter 内部 `endpoint_url(&["chat","completions"])` 取代 `chat_url()`）。
- `chat(&self, request: &ChatRequest)`：clone request（adapter.chat 消费 ChatRequest）→ 委托 → logging。

### 行为等价（TODO 明列 4 维度：finish_reason 映射 / system 渲染 / bearer 直连 / 不打印 key）
adapter 全部满足：finish_reason 用 §4.3 映射表（比手搓更完整，coordinator 场景均为 stop→EndTurn 一致）；system 渲染为首条 system 消息（与手搓 chat_messages 一致）；bearer 直连（EndpointConfig）；不打印 key（adapter + logging 均不打印）。

### 保留的 e2e wrapper 逻辑（非手搓 HTTP，保留在 wrapper 以维持等价）
1. **response_format 注入**：手搓在 `system.contains("JSON_OBJECT")` 时 `body["response_format"]={"type":"json_object"}`。改走 **provider_extras 逃生舱**：clone request 后设 `provider_extras=Some(ProviderExtras{provider:OpenAiChat, fields:{"response_format":{"type":"json_object"}}})`，adapter `merge_into` 注入 body（M1-3 已支持 extras 覆盖任意顶层字段）。
2. **model fallback**：`request.model.is_empty()` → `config.model`（coordinator 已恒非空，防御性等价）。
3. **empty-content → Protocol error**：adapter 返回后用 `response_text` 取文本，空则 `ClientError::Protocol("...empty assistant message")`（手搓语义）。
4. **logging**：文件1 `DeepSeekCallLog` 记 prompt/response 文本（`record_prompt` 在 chat 开头、`record_response` 在 empty 检查**之后**）；文件2 只记调用次数（`record()`）。两文件各自保留原 logging 形态。

### 删除的手搓代码（两文件）
`DeepSeekConfig::chat_url()`、`DeepSeekChatResponse`/`DeepSeekChoice`/`DeepSeekMessage` wire 类型、`chat_messages()`、`normalize_finish_reason()`、`http` 字段 + reqwest POST 逻辑；连带清理不再使用的 import（`serde::Deserialize`、`Normalized`/`StopReason`/`Usage`、`reqwest::*`、视情况的 `Value`/`json`）。保留：`DeepSeekConfig`(from_env+字段)、`DeepSeekCallLog`、`request_text`(文件1)、`message_text`/`content_text`/`response_text`(coordinator+logging 用)。

### 关键 API（已确认公开路径）
- `agent_lib::adapter::openai_chat::OpenAiChatAdapter`（`new(endpoint)`，实现 `LlmClient::chat`）
- `agent_lib::client::{EndpointConfig, AuthScheme, ChatRequest, ClientError, Response}`
- `agent_lib::model::{ProviderExtras, ProviderId}`（`ProviderId::OpenAiChat`）
- `ChatRequest` derive Clone（wrapper 可 clone）

### 执行步骤
1. 改 `tests/agent_external_real_e2e.rs`（无 feature gate，默认编译）。
2. 改 `tests/agent_external_managed_real_e2e.rs`（feature-gated）。
3. 每个文件改完查 unused import，保持 `cargo clippy --all-targets -- -D warnings` 全绿（含 external features clippy）。
4. 验证：`cargo test --test agent_external_real_e2e`（默认 ignored/skip exit0）；带 feature `cargo test --features "external-claude-code external-codex" --test agent_external_managed_real_e2e`（ignored/skip exit0）；全量 `cargo test --all --all-targets`；`cargo fmt --all`。
5. 实跑：环境无 DEEPSEEK_API_KEY/CLI → 标注「编译验证，未实跑」（TODO 验证条件允许）。
6. TODO M5-2 [TODO]→[DONE] + 完成记录。commit + stop。

### 进度日志
- [x] 读取两文件手搓客户端 + extras.rs + adapter mod.rs + 确认公开 API 路径/ChatRequest Clone + 确认 `OpenAiChatAdapter` 有 inherent `pub async fn chat`（response.rs:53），wrapper 调用无需 import trait。
- [x] 改文件1（real_e2e）：import（加 adapter/client/extras，删 normalized/usage/Deserialize）→ 删 HTTP_TIMEOUT → 删 chat_url → 改 DeepSeekLlmHandler（http→adapter，new 构造 EndpointConfig+Bearer，chat 委托 adapter + provider_extras 注入 response_format + model fallback + empty-content 检查 + record_prompt/response）→ 删 wire 类型/chat_messages/normalize_finish_reason（保留 request_text）。`cargo check` 通过。
- [x] 改文件2（managed_real_e2e，feature-gated）：对称改动，logging 形态为只记次数 record()，无 request_text。
- [x] 门禁全绿：`cargo fmt --all`（无 diff）；`cargo clippy --all-targets -- -D warnings` exit0；`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings` exit0（覆盖文件2）；`cargo test --test agent_external_real_e2e` 3 ignored exit0；`cargo test --features "external-claude-code external-codex" --test agent_external_managed_real_e2e` 3 ignored exit0；`cargo test --all --all-targets` 全部 `test result: ok.` 0 failed（lib 1123，无回归）。doc 未跑（未改 src/pub item，沿用 M5-1 绿基线）。
- [x] TODO 标 DONE + 完成记录 + commit + stop。（commit cf528cf）
