## Execution Plan — M1-2 适配器骨架：OpenAiChatAdapter 结构体与 LlmClient 契约

This file records the actionable plan and progress updates for the current invocation.

## 任务（TODO.md 第一个未完成任务）
**M1-2 [TODO] 适配器骨架：OpenAiChatAdapter 结构体与 LlmClient 契约**

把 M1-1 留下的最小编译桩升级为真正骨架，钉死 stream 互斥校验，建 §4.1 子模块空壳。

## 设计要点（对照 openai_resp 模板 + 设计文档 §4.1/§4.3/§4.4）
- 结构体 `{ http_client: reqwest::Client, endpoint: EndpointConfig }`，`new` /
  `with_http_client` / `endpoint()` 访问器，`#[derive(Clone, Debug)]`（密钥经 EndpointConfig 脱敏）。
- `LlmClient` impl：`capability() → &OPENAI_CHAT_DEFAULT_CAPABILITY`（trait 返回引用；
  任务文案 "clone()" 为口语化措辞，M1-1 桩已用引用，保持）。
- `chat()` / `chat_stream()` 按模板拆到子模块（inherent method），mod.rs 的 trait impl 委托：
  - `chat()` → `OpenAiChatAdapter::chat`（落 `response.rs`）
  - `chat_stream()` → `OpenAiChatAdapter::chat_stream`（落 `stream/mod.rs`）
  - inherent 方法优先于 trait 同名方法，UFCS 无歧义（openai_resp 同款）。
- stream 互斥校验（本任务钉死，与 openai_resp 同款）：
  - `chat()` 首句 `if request.stream { Err(invalid_response("…stream to be false")) }`
  - `chat_stream()` 首句 `if !request.stream { Err(invalid_stream("…stream to be true")) }`
  - 错误类型 `ClientError::Protocol`，helper：
    - `response.rs`: `pub(super) fn invalid_response` = `Protocol("invalid OpenAI Chat/Completions response: …")`
    - `stream/mod.rs`: `fn invalid_stream` = `Protocol("invalid OpenAI Chat/Completions stream: …")`
  - 校验通过后的主体：本任务为桩（返回 `ClientError::Other` 占位，**非 panic**，延续 M1-1 原则），
    build_request(M1-3) / execute+parse(M2-1) / SSE(M3) 后续填充。
- §4.1 子模块空壳：本任务建 mod.rs + request.rs(壳) + response.rs(chat 桩) + stream/mod.rs(桩)。
  request/input.rs、response/convert.rs、stream/{decoder,wire,normalizer}.rs 留给各自任务，
  本任务不创建无引用的空文件（避免 dead 文件）。

## 文件改动
1. `src/adapter/openai_chat/mod.rs` —— 替换 M1-1 桩为完整骨架（模块 rustdoc + 三 mod 声明 +
   结构体 + 两构造函数 + endpoint 访问器 + LlmClient impl 委托 + tests：Debug 脱敏 + 两条校验）。
2. `src/adapter/openai_chat/request.rs` —— 新建空壳（模块 rustdoc，M1-3 填 build_request）。
3. `src/adapter/openai_chat/response.rs` —— 新建：`impl OpenAiChatAdapter { chat() 校验+桩 }` +
   `pub(super) fn invalid_response`。
4. `src/adapter/openai_chat/stream/mod.rs` —— 新建：`impl OpenAiChatAdapter { chat_stream() 校验+桩 }`
   + `fn invalid_stream`。

## 验证
- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
- `cargo test -p agent-lib --lib adapter::openai_chat`
- 全量门禁留 M1-R；本任务跑目标测试足够。

## 收口
- TODO.md：M1-2 标题 `[TODO]` → `[DONE]`，追加完成记录。
- git commit：`[M1-2] 适配器骨架：OpenAiChatAdapter 结构体与 LlmClient 契约`。
- 停。

## 进度日志
- 阅读模板/设计文档，确认实现边界 ✓
- 实现完成（mod.rs/response.rs/stream/mod.rs/request.rs）✓
- 全门禁通过：fmt 无 diff；clippy base + external-features 全绿；
  `cargo test -p agent-lib --lib adapter::openai_chat` 3 通过；
  `cargo test -p agent-lib --lib` 1065 全绿无回归 ✓
- TODO.md 标 M1-2 [DONE] + 完成记录 ✓
- 提交 + 停。
