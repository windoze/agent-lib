## Execution Plan — M4-1：facade 接线（client_for_provider 分支 + openai_chat_from_env + lib.rs 文档）

TODO.md 第一个未完成任务：**M4-1 [TODO]**（M1/M2/M3 含 review 全部 [DONE]）。
目标：把 OpenAI Chat/Completions 协议接入 facade 层，补齐 M1-1 为满足 exhaustive match
而留下的「env 读取构造器 + vLLM 无 auth 路径」欠账，并同步顶层协议清单文档。

### 现状盘点（M1-1 已落地，本任务在其上加薄层）

- `ProviderId::OpenAiChat` 变体 ✓（`src/model/extras.rs`）
- `OPENAI_CHAT_DEFAULT_CAPABILITY` 静态 ✓（`src/client/capability.rs`，已在 `client/mod.rs` `pub use`）
- `openai_chat_endpoint(base_url, api_key)` 私有 helper ✓（`src/facade/config.rs:289`）
- `ProviderConfigBuilder::build()` 已处理 `ProviderId::OpenAiChat => openai_chat_endpoint(...)` ✓（`config.rs:256`）
- `client_for_provider()` 已有 `ProviderId::OpenAiChat => Arc::new(OpenAiChatAdapter::new(endpoint))` 分支 ✓（`chat.rs:395`）

### 待补（本任务产出）

#### 1. config.rs：`openai_chat_from_env()` 构造器（核心）

- 读 `OPENAI_CHAT_BASE_URL`（**必需**，缺失/空 → `FacadeError::Config`，复用 `required_env`）。
- 读 `OPENAI_CHAT_API_KEY`（**可选**）：有值 → `AuthScheme::Bearer`；缺失/空 → `AuthScheme::None`
  （vLLM 等无 auth OpenAI 兼容端点）。
- 错误风格与 `openai_from_env`/`anthropic_from_env` 一致（`FacadeError::Config`，文案点名变量不点值）。
- DeepSeek：最小方案，只加 `openai_chat_from_env`；用户把 base_url 指到 `https://api.deepseek.com`
  （DeepSeek 专用 env 入口留给 M4-3 `#[ignore]` 测试直接读 `DEEPSEEK_*`）。

#### 2. config.rs：泛化 `openai_chat_endpoint(base_url, auth: AuthScheme)`

- 现签名 `(base_url, api_key) -> 总是 Bearer`。改为 `(base_url, auth: AuthScheme)`，
  让 None 路径复用同一 helper，消除 EndpointConfig 字面量重复。
- 影响 builder 调用点：`openai_chat_endpoint(base_url, AuthScheme::Bearer(api_key))`。
- 私有 helper，零公开 API 影响。

#### 3. config.rs：`ProviderConfig::openai_chat()` builder 入口

- 与 `anthropic()`/`openai()` 对称的 fluent builder 入口（`ProviderConfigBuilder::new(OpenAiChat)`）。
- builder 路径恒带 api_key（Bearer）；无 auth（vLLM）走 `openai_chat_from_env` 或 `custom`。
- 顺手在 `api_version()` rustdoc 注明 chat/completions 忽略该字段（build 的 OpenAiChat arm 不读它）。

#### 4. config/tests.rs：env 隔离单测（复用既有 `ENV_LOCK` + `EnvGuard`）

按 TODO 验证条件两条：
- a) env 缺 `OPENAI_CHAT_BASE_URL` → 明确 `FacadeError::Config`。
- b) env 齐备（base_url + api_key）→ 构造成功，且 `client_for_provider(config)` 返回的 client
  `capability()` 与 `OPENAI_CHAT_DEFAULT_CAPABILITY` 一致（经 `crate::facade::chat::client_for_provider`，
  import `LlmClient`）。
- 补：无 api_key → `AuthScheme::None`（vLLM 路径）；`openai_chat()` builder 产 Bearer + provider==OpenAiChat。

#### 5. lib.rs：协议清单文档（§6 要求）

- 行 3：`translates Anthropic Messages and OpenAI Responses wire formats` → 加 `and OpenAI Chat/Completions`。
- 行 16-17：`adapter implements the Anthropic Messages and OpenAI Responses HTTP and SSE protocols`
  → 加 `and OpenAI Chat/Completions`。

#### 6. chat.rs：facade rustdoc 示例（§6 要求）

- `Chat` struct doc 示例（行 61 引用 `openai_from_env` 处）后补一条 chat/completions 用法提示，
  指向 `ProviderConfig::openai_chat_from_env`；不改既有示例语义、不新增可编译 doctest 负担。

### 验证条件（TODO）

- `cargo test -p agent-lib --lib facade` 通过。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- 全量门禁：`cargo fmt --all` / `cargo clippy --all-targets [--features external-*] -- -D warnings`
  / `cargo test --all --all-targets` 全绿。

### 执行顺序（小而靶向的 patch，每步间 re-read 受影响区）

1. [x] 上下文读取（TODO §M4-1 + 设计文档 §5.3/§6 + config.rs/config tests + chat.rs/chat tests + lib.rs + openai_chat/mod.rs）
2. [ ] config.rs：泛化 `openai_chat_endpoint` 签名 + 改 builder 调用点。
3. [ ] config.rs：加 `openai_chat_from_env()` + `openai_chat()` builder + `api_version()` rustdoc 注记。
4. [ ] config/tests.rs：加 env 隔离单测（含 client_for_provider capability 对照）。
5. [ ] lib.rs：协议清单两处。
6. [ ] chat.rs：rustdoc 示例补注。
7. [ ] 跑门禁（fmt → clippy ×2 → facade 测试 → doc）→ 全量 test。
8. [ ] TODO.md 标 [DONE] + 完成记录；commit + stop。

### 边界 / 不做

- 不动 `client_for_provider` 分支（M1-1 已在）。
- 不动 `tests/normalization/config.rs`（M4-2 范围）。
- 不加 `tests/integration_openai_chat.rs`（M4-3 范围）。
- 不加 DeepSeek 专用 facade 构造器（最小方案；DEEPSEEK_* 由 M4-3 ignored 测试直读）。
- 无 breaking change：新增构造器/builder + 放宽私有 helper 签名 + 文档。

### 进度日志

- [x] 上下文读取完成；计划定稿。
- [x] config.rs：泛化 `openai_chat_endpoint(base_url, auth: AuthScheme)` + builder 调用点改传 `AuthScheme::Bearer`。
- [x] config.rs：加 `openai_chat_from_env()`（必需 base_url / 可选 api_key→Bearer|None）+ `openai_chat()` builder +
      `optional_owned_env` helper + `api_version()`/struct doc 注记。
- [x] config/tests.rs：4 个 env 隔离单测（缺 base_url→错；齐备→Bearer+capability==默认；无 key→None；builder→Bearer 忽略 version）。
- [x] lib.rs：协议清单两处补 OpenAI Chat/Completions。
- [x] chat.rs：`Chat` 示例后补 chat/completions rustdoc 指引。
- [x] 门禁全绿：fmt 无 diff / clippy 默认 PASS / clippy external PASS / facade 282 通过 /
      test --all 全 0 failed（lib 1119→1123 +4）/ doc PASS（修 redundant-explicit-link）。
- [x] TODO M4-1 [TODO]→[DONE] + 完成记录。
- [x] commit（c9b082a）+ stop。

