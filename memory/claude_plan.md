## Execution Plan — M4-2：归一化矩阵注册 OpenAiChat provider

TODO.md 第一个未完成任务：**M4-2 [TODO]**（M1/M2/M3 + M4-1 全部 [DONE]）。
目标：在 `tests/normalization/config.rs` 注册 `OpenAiChat` provider，使 OpenAI
Chat/Completions（DeepSeek/vLLM 方言）进入跨 provider 归一化矩阵。

### 现状盘点（已读源码核实）

- `tests/normalization/config.rs`：`Provider` enum = `{Anthropic, OpenAiResponses}`；
  `configured_targets()` 按确定性顺序 + `filter_map(build_target)`；每 provider 一个
  `build_*_target()`（env 缺失返回 `None` 静默跳过）；模型名是**硬编码常量**
  （`"databricks-claude-haiku-4-5"` / `"gpt-5.5"`），env 只读 base_url + token 两项。
- `scenarios.rs` / `assertions.rs` / `mod.rs`：**全部 provider-neutral**，不按 provider
  分支，无按 provider 数量的断言/快照（grep 确认；`calls.len()` 是 tool call 计数，非 provider 计数）。
- `OpenAiChatAdapter`：`pub struct`，`with_http_client(endpoint, http_client)` 已就绪
  （`src/adapter/openai_chat/mod.rs:64`），经 `agent_lib::adapter::openai_chat` 可访问。
- `OPENAI_CHAT_DEFAULT_CAPABILITY.tool_calling = true`（capability.rs:111）→ scenario 的
  `capability().tool_calling` 检查会通过。
- transport 形态（§6/M4-1）：**Bearer 直连**，无 `api-key` 头、无 `api-version` query
  （区别于 Azure 风格的 `build_openai_target`）。
- design doc §7.2 item 4：「归一化矩阵：`config.rs:20` 注册新 `Provider` 分支」，无特定
  model env 约定。
- ACP fs 沙箱漏洞（C1）属不同子系统，不阻塞本任务（不抢占 TODO 顺序）。

### 实施（靶向 patch）

#### 1. `tests/normalization/config.rs`

- import：`adapter::{anthropic::AnthropicAdapter, openai_chat::OpenAiChatAdapter, openai_resp::OpenAiRespAdapter}`。
- `Provider` enum 末尾追加 `OpenAiChat`（保持矩阵顺序确定性）。
- `configured_targets()` 的数组末尾追加 `Provider::OpenAiChat`。
- `build_target` match 加 arm：`Provider::OpenAiChat => build_openai_chat_target()`。
- 新增 `build_openai_chat_target()`：env 三件套门禁，Bearer 直连，`with_http_client`。
  - env（与 facade `openai_chat_from_env` 一致 + `*_MODEL` 惯例，比照 M4-3 的
    `DEEPSEEK_MODEL`/`VLLM_MODEL`）：`OPENAI_CHAT_BASE_URL`（必需）、
    `OPENAI_CHAT_API_KEY`（必需）、`OPENAI_CHAT_MODEL`（必需——模型名 provider 相关、
    DeepSeek/vLLM 各异，用 env 而非常量，符合 M4-2「可用 model 名」「env 惯例」要求）。
  - model 走 `integration_env`（非密钥，复用统一 skip-message 行为）。
  - EndpointConfig：`auth: AuthScheme::Bearer(token)`，`query_params: Vec::new()`，
    `extra_headers: Vec::new()`。

#### 2. `tests/integration_normalization.rs`

- `#[ignore]` 文案补 OpenAI Chat/Completions（口径准确化，属 normalization 子系统连带更新）。

### 验证

- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`（编译 + lint 含 integration_normalization 全部 binary）
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
- `cargo test --test integration_normalization`（编译 + ignored 测试干净跳过，exit 0）
- **不跑全量 `cargo test --all --all-targets`**：本任务只改 `tests/normalization/config.rs`
  + 一个 `#[ignore]` 字符串，唯一受影响的 binary 是 `integration_normalization`，已被
  clippy --all-targets + 其自身 test run 覆盖；无生产代码、无其他 test binary 改动
  （全量门禁留给 M4-R review）。
- env-present 路径需真实端点，留手动验证 / M5-1 引用（M4-2 验证条件允许「手验或注明未实测」）。

### 边界 / 不做

- 不改 `scenarios.rs`/`assertions.rs`/`mod.rs`（provider-neutral，无需动）。
- 不加 `tests/integration_openai_chat.rs`（M4-3 范围）。
- 不动 facade / 生产代码。
- 无 breaking change：纯新增测试 provider 分支 + ignore 文案。

### 执行顺序

1. [x] 上下文读取（config.rs/mod.rs/scenarios.rs/assertions.rs/integration_normalization.rs
   + openai_chat/mod.rs + capability.rs + design doc §6/§7.2 + M4-1 完成记录）。
2. [ ] config.rs：import + enum + configured_targets + build_target arm + build_openai_chat_target。
3. [ ] integration_normalization.rs：ignore 文案。
4. [ ] 跑门禁（fmt → clippy ×2 → test --test integration_normalization）。
5. [ ] TODO.md M4-2 [TODO]→[DONE] + 完成记录；commit + stop。

### 进度日志

- [x] 上下文读取完成；计划定稿。
- [x] config.rs：import + enum + configured_targets + build_target arm + build_openai_chat_target。
- [x] 连带：`IntegrationTarget.model` `&'static str`→`String`（2 常量站 `.to_owned()` +
      scenarios.rs `.clone()`）——env-sourced model 名的 class-wide 必要修法。
- [x] integration_normalization.rs：ignore 文案补 OpenAI Chat/Completions。
- [x] 门禁全绿：fmt 无 diff / clippy 默认 exit0 / clippy external exit0 /
      `test --test integration_normalization` exit0（ignored 干净跳过，新文案生效）。
- [x] 未跑全量套件（唯一受影响 binary = integration_normalization 已被 clippy 覆盖，
      无生产代码改动）；env-present 路径无凭据未实跑（注明未实测，留 M4-3/M5-1）。
- [x] TODO M4-2 [TODO]→[DONE] + 完成记录。
- [ ] commit + stop。

