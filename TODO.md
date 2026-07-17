# TODO：Facade API 落地任务单

> 依据 [`PLAN.md`](PLAN.md) 与唯一设计输入 [`docs/facade-api.md`](docs/facade-api.md)。
> 上一轮任务单（Managed External Agent，M1–M10 + 交接任务 H-1）已归档到
> [`docs/archive/2026-07-17-managed-external-agent/`](docs/archive/2026-07-17-managed-external-agent/)。

## 通用执行规则

- **一次一个任务**：每次只执行「首个标题带 `[TODO]` 的任务」。完成后把该标题的 `[TODO]` 改为 `[DONE]`，
  并在任务末尾补「完成记录」，然后停止，等待下一次调用。
- **完成的定义**：只有标题带 `[DONE]` 才算完成。仅填了完成记录、日志或摘要而标题仍是 `[TODO]`，一律按未完成处理。
  review 任务（`M<n>-R`）是真实任务，不得跳过。
- **编号**：任务按实现顺序编号 `M<里程碑>-<序号>`；每个 milestone 末尾有一个独立 review 任务 `M<n>-R`。
- **不新增 effect family**：facade 是装配层，只能复用现有 `Conversation` / `DefaultAgentMachine` /
  `HandlerScope` / `drain` / `Pop` / `NeedTool` / `NeedInteraction` / `NeedSubagent` / `NeedExternalSession`，
  不得绕过底层重写状态机（`docs/facade-api.md` §2.1、§19）。
- **离线测试纪律**：facade 单元测试必须离线——用内建/伪造 `LlmClient` 与脚本化 handler，不依赖网络、凭据、
  CLI、本地登录态。每个测试须 1 分钟内完成，卡住即为 bug，须立刻修。真实 provider/CLI e2e 一律 `#[ignore]`，
  缺环境干净跳过（绿），不输出 secret。
- **不容忍 workaround / spec 偏离**：遇到底层缺口（缺 API、类型不匹配、能力不支持）不得papering over；要么修，
  要么在本文件正确依赖位置插入最小前置任务并让被阻塞任务显式依赖它，然后提交并停止。
- **默认完整验证序列**（任务另有放宽以任务为准）：
  1. `cargo fmt --all -- --check`
  2. 聚焦测试（任务给出精确过滤名）
  3. `cargo clippy --all-targets -- -D warnings`
  4. `cargo test --all --all-targets`（超时 ≤ 30 分钟）
  5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  6. `git diff --check`
  - 触碰 external adapter 的任务额外跑
    `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`。
  - 纯文档改动（只改 `*.md` 且不影响编译产物）可复用上一轮绿测结果，跳过步骤 4，并在完成记录注明。
- **公开 API 必须带 rustdoc**（`src/lib.rs` 已开 `#![warn(missing_docs)]`）。
- **schemars 风险（`PLAN.md` R1）**：typed function tool 的 JSON schema 派生涉及是否引入依赖；`schemars`
  **当前不是** crate 依赖。见 M2-1，优先 feature/companion 方案，不给核心 crate 强加依赖。

---

## Milestone 1 — Chat facade

目标：新增 `agent_lib::facade` + `agent_lib::prelude`，落地 `ProviderConfig`/`ModelConfig`、
`Reply`/`RunOutput`/`RunEvent`/`FacadeError`、`Chat`/`ChatSession`（ask/send/stream/snapshot/restore）。
Chat facade **不执行工具**：模型返回 tool-use 时报 `FacadeError::UnexpectedToolUse`（`docs/facade-api.md`
§5.3）。M1 直接驱动 `Conversation`（`begin_turn`→`start_assistant_response`→`finish_assistant`→
`commit_pending`），**不**引入 `DefaultAgentMachine`（那属于 M2）。

### [DONE] M1-1 建 facade 模块骨架 + 内建 id source + `ProviderConfig` + `ModelConfig` + `FacadeError`

**上下文**：

- `src/lib.rs` 现有 `pub mod adapter/agent/client/conversation/model/stream`；facade 是全新顶层模块。
- 配置锚点：`client::{EndpointConfig, AuthScheme}`、`model::extras::{ProviderId, ProviderExtras}`、
  `agent::ModelRef`（`ModelRef::new(model, max_tokens: NonZeroU32, temperature, ...)`，见
  `examples/agent_chat.rs`）、`client::ChatRequest` 字段（`model/messages/tools/system/max_tokens/
  temperature/stream/provider_extras`）。
- 库从不自己造 id：需要一个内建 `RequirementIds`+`ToolExecutionIds` 实现（参照
  `examples/agent_chat.rs` 的 `DemoIds`：单调计数器 → `uuid::Uuid::from_u128`，从 1 起）。放
  `src/facade/ids.rs`，供 Chat/Agent 复用（生成 `ConversationId/TurnId/MessageId/ToolCallId/StepId` 等）。

**做什么**：

- 在 `src/lib.rs` 加 `pub mod facade;` 与 `pub mod prelude;`。
- 建 `src/facade/mod.rs` 及子模块 `config.rs`、`ids.rs`、`error.rs`，并起 `prelude`（`src/facade/prelude.rs`
  或 `src/prelude.rs`，与 §3 列表一致，先只重导 M1 已存在的类型：`Chat, ChatSession, ProviderConfig,
  ModelConfig, Reply, RunOutput, RunEvent`；后续 milestone 逐步补 `Agent/AgentSession/Approval/...`）。
- `ProviderConfig`（`config.rs`）：包装 `EndpointConfig` + 目标 `ProviderId`。构造器：
  `anthropic_from_env()`、`openai_from_env()`（从常见 env 读 base_url/api_key/version，读不到给
  `FacadeError::Config`）、`openai()`/`anthropic()` builder（`.base_url(..).api_key(..).api_version(..)
  .build()`）、`custom(EndpointConfig, ProviderId)`。标注凭据不应 debug/log/persist（`Debug` 手写脱敏，
  不打印 key）。
- `ModelConfig`（`config.rs`）：`ModelConfig::new(model).max_tokens(u32).temperature(f32)`；提供
  `to_model_ref()`（→ `agent::ModelRef`）与把公共字段套进 `ChatRequest` 的 helper。
- `FacadeError`（`error.rs`）：先落 M1 需要的变体 `Config(..)`、`Client(client::ClientError)`、
  `Conversation(conversation::...Error)`、`UnexpectedToolUse`、`InvalidState(String)`；`impl std::error::Error`
  + `Display`，保留 source。后续 milestone 再按 `docs/facade-api.md` §16 追加
  （`Agent/Tool/ApprovalDenied/PermissionDenied/LoopLimitExceeded/UnhandledRequirement/Delegate/
  ExternalSession/Restore`）——本任务不必全加，但要在 rustdoc 注明会增补。
- 全部公开项带 rustdoc。

**验证条件**：

- 单元测试：`ProviderConfig::custom` / builder 生成正确 `EndpointConfig`+`ProviderId`；env 构造器缺变量时
  返回 `FacadeError::Config`（用临时 env，不落真凭据）；`ModelConfig::to_model_ref` 与 ChatRequest 字段
  映射正确；`ProviderConfig`/凭据的 `Debug` 不含明文 key。
- 聚焦：`cargo test -p agent-lib facade::config`。
- 完整验证序列 1、3、5、6（M1-1 不含跨包行为，步骤 2 用上面的聚焦名；步骤 4 视改动运行）。

**完成记录**：

- `src/lib.rs` 新增 `pub mod facade;` 与 `pub mod prelude;`（保持模块声明字母序）。
- 新建 `src/facade/mod.rs`（层级/职责 rustdoc + 重导 `ProviderConfig/ProviderConfigBuilder/ModelConfig/
  FacadeError/FacadeIds`）。
- `src/facade/config.rs`：
  - `ProviderConfig`（包 `EndpointConfig`+`ProviderId`）构造器 `custom` / `anthropic_from_env` /
    `openai_from_env` / `anthropic()` / `openai()` builder（`ProviderConfigBuilder`，
    `.base_url/.api_key/.api_version/.build`，缺 `base_url`/`api_key` → `FacadeError::Config`）。
    env 约定对齐 `examples/support`（Anthropic：`ANTHROPIC_BASE_URL` 默认 `https://api.anthropic.com`、
    必填 `ANTHROPIC_AUTH_TOKEN` Bearer、`ANTHROPIC_VERSION` 默认 `2023-06-01`→`anthropic-version` header；
    OpenAI：必填 `OPENAI_BASE_URL`、`OPENAI_API_KEY`→`api-key` header、`OPENAI_API_VERSION`
    默认 `2025-04-01-preview`→`api-version` query）。
  - 手写脱敏 `Debug`（`RedactedAuth`/`RedactedPairs`：只显示 auth 种类/header/query 键名，值一律
    `<redacted>`；不派生 `Serialize`），凭据不落明文；rustdoc 注明不应 log/persist、不入 snapshot。
  - `ModelConfig::new(model).max_tokens(u32).temperature(f32)`（`max_tokens` 默认 1024，传 0 保留默认并
    注明）；`.provider_extras(..)`；`to_model_ref()`→`agent::ModelRef`；`apply_to_request(&mut ChatRequest)`
    只覆盖 `model/max_tokens/temperature/provider_extras`。
- `src/facade/error.rs`：`FacadeError`（`thiserror`，`#[non_exhaustive]`）变体 `Config(String)`/
  `Client(#[from] ClientError)`/`Conversation(#[from] ConversationError)`/`UnexpectedToolUse`/
  `InvalidState(String)`，保留 source；rustdoc 注明后续 milestone 按 §16 增补。
- `src/facade/ids.rs`：`FacadeIds`（`Arc<AtomicU64>` 从 1 起 → `uuid::Uuid::from_u128`，`Clone` 共享计数器）
  实现 `RequirementIds`+`ToolExecutionIds`，并提供 `agent_id/run_id/tool_set_id/conversation_id/turn_id/
  message_id/step_id/trace_root` 便捷生成器（去掉与 trait 冲突的同名 `tool_call_id` 便捷方法，`ToolCallId`
  经 trait 方法生成）。
- `src/prelude.rs`：先只重导已存在的 `ProviderConfig, ModelConfig`（rustdoc 注明后续补 Chat/ChatSession/
  Reply/RunOutput/RunEvent 等）。
- 单元测试（17 个，全离线）：`config`（custom/builder/anthropic+openai env-读取与默认/env 缺变量→`Config`/
  builder 缺字段→`Config`/`Debug` 脱敏 bearer+header 值/`ModelConfig` 默认+builder+`max_tokens(0)` 保默认/
  `to_model_ref` 全字段映射/`apply_to_request` 只覆盖公共字段；env 测试用进程级 `Mutex`+`EnvGuard` 串行、
  不落真凭据），`ids`（跨 family 唯一非 nil/克隆共享计数器/trait 方法各自出新 id）。
- 验证：`cargo fmt --all -- --check` ✅；`cargo test -p agent-lib --lib facade::` 17 passed ✅；
  `cargo clippy --all-targets -- -D warnings` ✅；`cargo test --all --all-targets` 全绿（50 组 test result: ok，
  0 failed）✅；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✅；`git diff --check` 干净 ✅。


### [DONE] M1-2 `Reply` / `RunOutput` / `UsageSummary` / `RunEvent` / `IntoUserMessage`

**上下文**：

- 结果类型见 `docs/facade-api.md` §6：`Reply{text,usage,stop_reason}`、
  `RunOutput{reply,response,usage,tool_calls,delegations,artifacts,events}`、`RunEvent`（枚举，含
  `TextDelta/ToolStarted/ToolFinished/ApprovalRequested/Delegation*/Escalated/Done(RunOutput)/
  RawStream(StreamEvent)/RawNotification(Notification)`）。
- 底层锚点：`client::Response`、`model::usage::Usage`/`TokenUsage`、`model::normalized::StopReason`、
  `stream::StreamEvent`、`agent::event::Notification`（确认实际类型名）。
- `IntoUserMessage`（§5.2）先支持 `&str`/`String`/`model::message::Message`/`Vec<ContentBlock>`。

**做什么**：

- 建 `src/facade/run.rs`：`Reply`（`text()/usage()/stop_reason()`；`text()` 聚合 `Response` 的 text block）、
  `RunOutput`、`UsageSummary`（聚合 supervisor/subagent/external usage，M1 只填 supervisor）、`RunEvent`。
  M1 只需 `TextDelta`/`Done`/`RawStream`/`RawNotification` 有实义，其余 delegation/tool 变体先定义占位
  （M2/M3/M4 填充），但类型现在就定，避免后续破坏性改枚举。
- 为 delegation/artifact 相关字段定义最小占位类型（`ToolTrace`/`DelegationTrace`/`ArtifactRef` 等）放
  `src/facade/run.rs` 或 `delegate.rs`，M1 里 `RunOutput` 对应 Vec 默认空；标注后续 milestone 填充。
- `IntoUserMessage` trait + 上述 4 个 impl，产出 facade 内部统一的 user `Message`。
- `RunEvent` 尽量 `#[derive(...)]` 可序列化的归一化变体；`RawStream`/`RawNotification` 标注非序列化承诺
  （`PLAN.md` R7）。
- 全部公开项带 rustdoc。

**验证条件**：

- 单元测试：`Reply::text()` 从多 text block 的 `Response` 聚合正确、非文本 content 不丢（保留在
  `RunOutput.response`）；`IntoUserMessage` 四种输入产出等价 `Message`；`UsageSummary` 聚合求和正确。
- 聚焦：`cargo test -p agent-lib facade::run`。
- 完整验证序列 1、3、5、6（步骤 2 用聚焦名）。

**完成记录**：

- 新建 `src/facade/run.rs`：
  - `Reply { text, usage: Option<Usage>, stop_reason: Option<StopReason> }` + `text()/usage()/stop_reason()`。
    spec §6.1 写 `TokenUsage`，但本 crate 无该类型（TODO 已授权「确认实际类型名」）→ 采用
    `model::usage::Usage`；`stop_reason` 取 `Response.stop_reason.value`（`Normalized<StopReason>` 的归一值）。
    文本聚合 `aggregate_text` 只拼接 `ContentBlock::Text`，其余（tool-use/image/thinking）不进 `text` 但
    完整保留在 `RunOutput.response`。构造走 `impl From<&Response> for Reply`（公开、惯用、避免 dead_code）。
  - `RunOutput { reply, response: Option<Response>, usage: UsageSummary, tool_calls: Vec<ToolTrace>,
    delegations: Vec<DelegationTrace>, artifacts: Vec<ArtifactRef>, events: Vec<RunEvent> }`，
    `impl From<Response> for RunOutput`（M1 只填 supervisor usage，其余 Vec 空）。
  - `UsageSummary { supervisor, subagents, external: Usage }` + `from_supervisor/total/add_supervisor/
    add_subagent/add_external`（`total()` 用 `Usage::merge` 求和；M1 只填 supervisor）。
  - `RunEvent` 枚举全变体现在就定死：`TextDelta/ToolStarted/ToolFinished/ApprovalRequested/
    DelegationStarted/DelegationProgress/DelegationMessage/DelegationArtifact/DelegationFinished/
    DelegationFailed/Escalated/Done(Box<RunOutput>)/RawStream(StreamEvent)/RawNotification(Notification)`；
    M1 只有 `TextDelta/Done/RawStream/RawNotification` 有实义，其余占位待 M2–M5 填。
  - 最小占位类型（`#[non_exhaustive]`，rustdoc 注明后续 milestone 填充）：`ToolTrace{name,call_id}`、
    `ApprovalRequest{tool_name}`、`DelegationTrace{delegate}`、`DelegationProgress{delegate,message}`、
    `DelegationMessage{delegate,message}`、`ArtifactRef{path}`、`EscalationTrace{from,to}`。
  - `IntoUserMessage` trait + 4 impl（`&str`/`String`/`Message`/`Vec<ContentBlock>` → user `Message`）。
- **R7（序列化承诺）**：`RunEvent`/`RunOutput` 不 derive serde（`RawStream`/`RawNotification` 逃生舱不作稳定
  序列化契约，且 `RunOutput` 内含 `Vec<RunEvent>`）；归一化叶子类型（`Reply`/`UsageSummary`/各 trace 占位）
  仍 derive `Serialize+Deserialize`。rustdoc 已注明该取舍。
- **spec 取舍（供 M1-R 记录）**：①`Reply.usage` 用 `Usage` 而非 spec 的 `TokenUsage`（后者不存在）；
  ②`RunEvent::Done` 采 `Box<RunOutput>`（clippy `large_enum_variant`；spec §6.3 写不带 Box，Deref 后
  字段访问一致，示例不受影响）。
- `src/facade/mod.rs` 重导 `run::{Reply, RunOutput, UsageSummary, RunEvent, IntoUserMessage, ToolTrace,
  ApprovalRequest, DelegationTrace, DelegationProgress, DelegationMessage, ArtifactRef, EscalationTrace}`；
  `src/prelude.rs` 补 `Reply, RunEvent, RunOutput`（对齐 `docs/facade-api.md` §3 prelude 列表）。
- 单元测试（`src/facade/run/tests.rs`，5 个，全离线）：Reply 多 text block 顺序聚合 + usage/stop_reason 映射；
  `RunOutput.response` 保留非文本 tool-use 且 M1 各 Vec 空、supervisor usage 正确；`IntoUserMessage` 四输入
  产等价 `Message`；`UsageSummary::total` 三片求和；`from_supervisor`+`add_*` 累加。
- 验证：`cargo fmt --all -- --check` ✅；`cargo test -p agent-lib --lib facade::run` 5 passed ✅；
  `cargo clippy --all-targets -- -D warnings` ✅；`cargo test --all --all-targets` 全绿（50 组 test result: ok，
  0 failed）✅；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✅；`git diff --check` 干净 ✅。

### [DONE] M1-3 `Chat` / `ChatBuilder` + `ask` / `ask_full`（one-shot，无 tool-use）

**上下文**：

- `docs/facade-api.md` §5.1–§5.3：`Chat` = 可共享配置 + `LlmClient` 装配入口；`Chat::ask` 是 one-shot，
  不保留历史。内部映射（§5.3）：生成 ids → `Conversation::begin_turn` → 从 `effective_view()` 构造
  `ChatRequest` → `LlmClient::chat` → `start_assistant_response` → 无 tool-use 则 `finish_assistant`
  （`AssistantFinish::ReadyToCommit`）+ `commit_pending` → 返回 `RunOutput`/`Reply`。
- 若 `Response` 含 tool-use content，报 `FacadeError::UnexpectedToolUse`（Chat facade 不执行工具）。
- 需要一个具体 `LlmClient` 才能装配；builder 用 `ProviderConfig` 造 adapter（`adapter::anthropic` /
  `adapter::openai`，按 `ProviderId` 选），或允许 `.client(Arc<dyn LlmClient>)` 直接注入（便于离线测试）。

**做什么**：

- 建 `src/facade/chat.rs`：`Chat` + `ChatBuilder`（`.provider(ProviderConfig).model(..).system(..)
  .max_tokens(..).temperature(..).client(..).build()`）。builder 依 `ProviderId` 选 adapter 构造
  `Arc<dyn LlmClient>`；也接受显式 `.client(..)` 覆盖（离线测试用）。
- `Chat::ask(input: impl IntoUserMessage) -> Result<Reply, FacadeError>` 与
  `ask_full(..) -> Result<RunOutput, FacadeError>`：每次新建临时 `Conversation`（one-shot 不保历史），
  按 §5.3 驱动；tool-use → `UnexpectedToolUse`；pending 失败默认 cancel（回到一致点）。
- `Chat::session(&self) -> ChatSessionBuilder`（M1-4 落地 `ChatSession`；本任务先留 builder 入口或在 M1-4 加）。
- 全部公开项带 rustdoc + 一个 `no_run` 或离线 doctest。

**验证条件**：

- 单元测试（离线，注入 fake `LlmClient` 返回固定 `Response`）：`ask` 返回文本正确、`ask_full` 的
  `RunOutput.response` 与 usage 正确；模型返回 tool-use → `FacadeError::UnexpectedToolUse`；连续两次 `ask`
  互不保留历史。
- 聚焦：`cargo test -p agent-lib facade::chat`。
- 完整验证序列 1–6（含 `cargo test --all --all-targets`，因为新增可编译代码与测试）。

**完成记录**：

- 新建 `src/facade/chat.rs`：
  - `Chat`（`Clone`；持 `Arc<dyn LlmClient>` + `ModelConfig` + `Option<system>` + `FacadeIds`；手写脱敏
    `Debug` 把 client 打成 `<dyn LlmClient>`，不泄漏内部）。方法 `builder()/ask/ask_full` 及只读
    `client()/model()/system()`。
  - `ChatBuilder`（`Clone, Default`）：`.provider(ProviderConfig)`、`.client(Arc<dyn LlmClient>)`（离线测试直
    接注入，优先于 provider）、`.model(impl Into<String>)`（必填，缺则 `FacadeError::Config`）、`.system(..)`、
    `.max_tokens(u32)`、`.temperature(f32)`、`.ids(FacadeIds)`（确定性测试用）、`.build()`。client 解析：显式
    client 优先；否则按 `ProviderId` 造 adapter（`Anthropic`→`AnthropicAdapter`、`OpenAiResp`→`OpenAiRespAdapter`）；
    两者皆无 → `FacadeError::Config`。
  - `ask_full` 每次新建临时 `Conversation`（`ConversationConfig::new(system)`），按 `docs/facade-api.md` §5.3
    驱动：`begin_turn` → `effective_view().into_parts()` + `pending_context().into_messages()` 构 `ChatRequest`
    （`ModelConfig::apply_to_request` 覆盖 model/max_tokens/temperature/provider_extras；`stream=false`；`tools`
    恒空——Chat 不执行工具）→ `LlmClient::chat` → `start_assistant_response` → `finish_assistant`。
    `AssistantFinish::ReadyToCommit` → `commit_pending(TurnMeta::default())`（response usage 已由 pending 自动
    `merge_pending` 进 meta，避免重复计数）；`RequiresToolCallMappings`（即 tool-use）→ `FacadeError::UnexpectedToolUse`。
    共享私有 `drive_turn` 在 `begin_turn` 之后的任意错误路径都兜底 `cancel_pending(CancelDisposition::DiscardTurn)`
    回到上一提交一致点（供 M1-4 `ChatSession` 复用）。`ask` = `ask_full().reply`。
  - `Chat::session()` 入口按 TODO 授权延后到 M1-4（避免引入尚未落地的 `ChatSessionBuilder`）。
- `src/facade/mod.rs` 重导 `chat::{Chat, ChatBuilder}` 并更新模块 rustdoc；`src/prelude.rs` 补 `Chat`
  （对齐 §5.1 示例）。
- 单元测试（`src/facade/chat/tests.rs`，6 个，全离线，`FakeClient` 返回固定 `Response` 且记录收到的 request）：
  `ask` 文本+stop_reason 聚合、请求只含当轮 user 消息；`ask_full` 保留 `response` 且 supervisor usage 正确、
  request 带 system/model、无 tools、非流式；tool-use `Response` → `UnexpectedToolUse`；连续两次 `ask` 请求消息数
  均为 `[1, 1]`（一次性会话不保留历史）；builder 缺 model / 缺 client+provider → `Config`。
- **spec 取舍**：`.model(..)` 取字符串（对齐 §5.1 `.model("gpt-5.5")`），内部据 `.max_tokens`/`.temperature`
  组装 `ModelConfig`；tool-use 判定采用 Conversation 权威信号 `AssistantFinish::RequiresToolCallMappings`
  而非手扫 content（等价，且避免 dead code）。
- 验证：`cargo fmt --all -- --check` ✅；`cargo test -p agent-lib --lib facade::chat` 6 passed ✅；
  `cargo clippy --all-targets -- -D warnings` ✅；`cargo test --all --all-targets` 全绿（50 组 test result: ok，
  947 passed，0 failed）✅；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✅；`git diff --check` 干净 ✅。

### [DONE] M1-4 `ChatSession` + `send` / `send_full` + `conversation()` + snapshot/restore

**上下文**：

- `docs/facade-api.md` §5.1–§5.3、§15.1：`ChatSession` = 有状态 `Conversation` session，多轮保留历史。
  `send`/`send_full` 逻辑同 §5.3，但复用同一 `Conversation`（每轮 `begin_turn` 续接）。
- snapshot 用 `conversation::ConversationSnapshot`（`Conversation::snapshot()` 只在 committed 一致点导出）；
  `ChatSession::restore(snapshot, chat)` 重新注入 provider/client（§15.1）。

**做什么**：

- 在 `src/facade/chat.rs`（或 `chat_session.rs`）落 `ChatSession` + `ChatSessionBuilder`（`chat.session()
  .system(..).build()`，可继承 `Chat` 的 provider/model/system）。
- `send`/`send_full`（`&mut self`）：复用内部 `Conversation` + id source 续接多轮；无 tool-use 才 commit；
  tool-use → `UnexpectedToolUse`；pending 失败默认 cancel。
- `conversation(&self) -> &Conversation`、`snapshot() -> Result<ConversationSnapshot, FacadeError>`、
  `restore(snapshot, chat) -> Result<Self, FacadeError>`。
- 全部公开项带 rustdoc。

**验证条件**：

- 单元测试（离线 fake client）：两轮 `send` 后 `conversation().effective_view()` 含前一轮历史（多轮上下文
  正确累积）；`snapshot()` 只在 committed 点成功；`restore()` 后继续 `send` 能接上历史；snapshot 不含
  client/凭据（类型层面即不可能，断言字段）。
- 聚焦：`cargo test -p agent-lib facade::chat`（含 session 用例）。
- 完整验证序列 1–6。

**完成记录**：

- 在 `src/facade/chat.rs` 落地 `ChatSession` + `ChatSessionBuilder`，并加 `Chat::session(&self) -> ChatSessionBuilder`：
  - `ChatSession` 持 `Conversation` + `Arc<dyn LlmClient>` + `ModelConfig` + `FacadeIds`；手写脱敏 `Debug`（client 打成 `<dyn LlmClient>`）。
  - `send_full(&mut self, input)` / `send(&mut self, input)` 复用同一 `Conversation` + 同一 id source，直接调用已有共享
    `drive_turn`——每轮 `begin_turn` 续接committed 历史，无 tool-use 才 commit；tool-use → `UnexpectedToolUse`；失败兜底
    `cancel_pending(DiscardTurn)`。`send` = `send_full().reply`。
  - `conversation(&self) -> &Conversation`（`const fn`）；`snapshot(&self) -> Result<ConversationSnapshot, FacadeError>`
    （`Conversation::snapshot()`，pending 时 `ConversationError::Snapshot(PendingTurn)`）；
    `restore(snapshot, chat) -> Result<Self, FacadeError>`（`Conversation::restore` + 从 `chat` 重注入 client/model）。
  - `ChatSessionBuilder`（`chat.session().system(..).build()`）继承 Chat 的 client/model/system/ids；`.system(..)` 覆盖用
    `system_overridden` 标记区分「显式设空」与「未设」；`build()` 返回 `Result`（对齐 doc §5.1 `.build()?`，当前 infallible，
    rustdoc 注明为将来校验预留）。
- **前置缺陷修复（class-wide，非 workaround）**：内建 `FacadeIds` 用「从 1 起的单调计数器 → `Uuid::from_u128`」，
  `ConversationSnapshot` 是纯数据、不含 runtime 计数器，故用**新的**默认计数器 restore 后再 `send` 会 re-mint 与恢复历史
  相同的 id → `DuplicateMessageId`（实测 `MessageId(...-000000000003)` 冲突），会让 spec §15.1 的 restore 示例失败。修复：
  - `src/facade/ids.rs` 新增 `FacadeIds::seeded(u64)`（clamp≥1）与 `FacadeIds::continuing_after(&Conversation)`——扫描
    conversation.id/turn.id/每条 message.id 以及 tool pairing 的 call_id/call_msg/result_msg，取「落在 u64 计数器空间内」
    的最大值 +1 作为新计数器起点（真实随机/UUIDv7 id 落在高 64 位、与小计数器值不可能冲突，故忽略）。
  - `ChatSession::restore` 改用 `FacadeIds::continuing_after(&conversation)` 派生 id source，保证续接不撞已恢复历史。
- `src/facade/mod.rs` 重导 `ChatSession, ChatSessionBuilder` 并更新模块 rustdoc；`src/prelude.rs` 补 `ChatSession`
  （对齐 §3 prelude 列表）；`chat.rs` 模块 rustdoc 更新（stateful session 落地本任务，stream 留 M1-5）。
- 单元测试：`src/facade/chat/tests.rs` 追加 6 个（全离线）——两轮 `send` 请求 message 数 `[1,3]`、`effective_view` 累积 4 条；
  build 继承 Chat system / `.system(..)` 覆盖；session tool-use → `UnexpectedToolUse`；`snapshot()` 在 committed 点成功且
  serde round-trip、断言 JSON 不含 `client`/`api_key`/`LlmClient`（快照不含 client/凭据）；`restore()` 用不同 client 的 Chat
  续接、回放 `[3]` 且返回新 client 文本。`src/facade/ids.rs` 追加 `seeded` 起点+clamp 测试（`continuing_after` 由 restore
  e2e 覆盖）。
- 验证：`cargo fmt --all -- --check` ✅；`cargo test -p agent-lib --lib facade::` 35 passed ✅；
  `cargo clippy --all-targets -- -D warnings` ✅（未触碰 external adapter，无需 external features 额外 pass）；
  `cargo test --all --all-targets` 全绿（`--lib` 698 passed，其余各 suite 0 failed）✅；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✅；`git diff --check` 干净 ✅。

### [DONE] M1-5 `ChatSession::stream` + `RunStream`（基于 `Accumulator`）

**上下文**：

- `docs/facade-api.md` §5.4：流式不强制暴露 `Accumulator`，但保留 raw event。内部用
  `stream::accumulator::Accumulator` 折叠完整 `Response`，最终 `RunEvent::Done(RunOutput)` 提交 Conversation。
- 锚点：`client::LlmClient::chat_stream` 产 `stream::StreamEvent`；`stream::accumulator::{Accumulator, collect}`。

**做什么**：

- 落 `ChatSession::stream(&mut self, input) -> Result<RunStream, FacadeError>`。`RunStream` 是
  `Stream<Item = Result<RunEvent, FacadeError>>`（或 `next().await` 便捷）：转发 `TextDelta`、可选
  `RawStream(StreamEvent)`，末尾 `Done(RunOutput)`；内部 `Accumulator` 折叠出 `Response` 后 `commit_pending`。
- 若流中出现 tool-use，产 `UnexpectedToolUse`（Chat 不执行工具）。
- rustdoc + 离线 doctest（`no_run` 或 fake stream）。

**验证条件**：

- 单元测试（离线 fake `chat_stream` 产固定事件序列）：`TextDelta` 顺序正确、`Done` 的 `RunOutput` 文本/
  usage 与非流式一致、流结束后 `conversation()` 已提交该轮；tool-use 流 → `UnexpectedToolUse`。
- 聚焦：`cargo test -p agent-lib facade::chat`（含 stream 用例）。
- 完整验证序列 1–6。

**完成记录**：

- 新增 `src/facade/chat/stream.rs`（模块化，chat.rs 已 21KB）落地 `RunStream<'a>`：
  - 持 `&'a mut Conversation` + `BoxStream<'static, Result<StreamEvent, ClientError>>` + `Option<Accumulator>`
    + `FacadeIds`（clone）+ `VecDeque<RunEvent>` 缓冲 + `State{Streaming,Finishing,Done}` 状态机。
  - `impl futures::Stream`（全字段 `Unpin`，`inner.poll_next_unpin`；finish 全同步无 async）：每个上行
    `StreamEvent` 先缓冲 `RunEvent::TextDelta`（text delta 时），再 `RunEvent::RawStream(event.clone())`，然后
    push 进 `Accumulator`；inner 耗尽→`accumulator.finish()`→`Response`→与非流式 `drive_pending` **相同尾巴**
    （`start_assistant_response`→`finish_assistant`→`commit_pending`）→末尾单个 `Done(Box<RunOutput>)`。
  - 便捷 inherent `pub async fn next(&mut self)`（包 `StreamExt::next`，免导入即可 `stream.next().await`）。
  - 错误处理：inner `Err`、`Accumulator` 校验错误、tool-use（`finish_assistant` 返回 `RequiresToolCallMappings`
    → `UnexpectedToolUse`）一律 `cancel_pending(DiscardTurn)` 回滚 pending turn，会话回到最近 committed 点仍可用。
  - `AccumulatorError` 映射：`Stream(e)`→`FacadeError::Client(e)`；其余（协议/校验违规）→`Client(Protocol(..))`。
- `src/facade/chat.rs`：`ChatSession::stream` 打开 pending turn（`begin_turn`）→ `build_request(stream=true)` →
  `client.chat_stream().await`（失败即回滚 pending 并直接返回 `Err`）→ `RunStream::new`。`build_request` 增
  `stream: bool` 参数（`drive_pending` 传 `false`）。`mod stream; pub use stream::RunStream;`；模块 rustdoc 更新
  （stream 落地本任务）。
- `src/facade/mod.rs` / `src/prelude.rs` 重导 `RunStream` 并更新 rustdoc。
- 单元测试：`src/facade/chat/tests.rs` 追加 `StreamingFakeClient`（脚本化 `chat_stream` 事件序列 + 记录请求）与 4
  个离线用例：text 流 `TextDelta` 顺序正确、`RawStream` 已转发、`Done` 文本/usage 正确、流结束后 `conversation()`
  已提交（effective_view 2 条）；`Done` 的 `RunOutput` 与 `RunOutput::from(等价 Response)` **整体相等**（证与非流式
  一致）；tool-use 流 → `UnexpectedToolUse` 且无 `Done`、turn 已回滚（无 committed 历史）；连续两次 `stream` 累积
  历史（请求消息数 `[1,3]`、effective_view 4 条）。
- 验证：`cargo fmt --all` ✅；`cargo clippy --all-targets -- -D warnings` ✅（未触碰 external adapter）；
  `cargo test -p agent-lib --lib facade::chat` 16 passed ✅；`cargo test --all --all-targets` 全绿（`--lib` 702
  passed，doctest 12 passed，其余各 suite 0 failed）✅；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  ✅；`git diff --check` 干净 ✅。

### [DONE] M1-R Review：Chat facade 正确性与文档一致性检查

**上下文**：M1-1..M1-5 落地了 Chat facade。此任务只做审查与必要的收敛修正，不引入新功能。

**做什么**：

- 通读 `src/facade/{mod,config,ids,error,run,chat}.rs` 与 `prelude`，核对：与 `docs/facade-api.md`
  §3–§6 的类型/方法形状一致；`Chat::ask` one-shot 不保历史、`ChatSession` 多轮保历史；tool-use 一律
  `UnexpectedToolUse`；pending 失败默认 cancel；snapshot 不含 secret/client。
- 核对 `prelude` 只重导已存在类型；rustdoc 完整、doctest 可编译；`FacadeError` 变体与 §16 命名一致
  （允许尚未全加，但已加的名字要对）。
- 修正发现的 spec 偏离/文档不一致（小范围）；若发现需要新功能才能对齐，按「通用执行规则」插入前置任务而非在
  review 里扩功能。
- 若 `docs/facade-api.md` 与实现有取舍差异（如 R2/R3 命名），在本文件该 milestone 或 `PLAN.md` 风险处记一句。

**验证条件**：

- 完整验证序列 1–6 全绿。
- 复述式检查：列出 M1 已实现 vs `docs/facade-api.md` §5–§6 承诺项的对照，缺口（若有）记为后续任务。

**完成记录**：

- 通读 `src/facade/{mod,config,ids,error,run,chat,chat/stream}.rs` 与 `src/prelude.rs`，逐项核对
  `docs/facade-api.md` §3–§6/§16，结论：形状与语义一致，**无 spec 偏离、无缺口**。
- 语义核对（全部满足）：
  - §5.1 `Chat::ask`/`ask_full` 每次新建 throwaway `Conversation`（`ask_full` 内 `Conversation::new`），
    **one-shot 不保历史**；`ChatSession` 复用单一 `Conversation`，**多轮保历史**（测试
    `consecutive_asks_do_not_retain_history` 请求消息数 `[1,1]` vs `session_accumulates_history` `[1,3]`）。
  - tool-use 一律 `FacadeError::UnexpectedToolUse`：非流式 `drive_pending` 与流式 `finish_inner` 都在
    `finish_assistant` 返回 `RequiresToolCallMappings` 时报该错（`tool_use_response_is_rejected` /
    `session_rejects_tool_use` / `stream_rejects_tool_use_and_rolls_back`）。
  - pending 失败默认 cancel：`drive_turn` 出错走 `cancel_pending(DiscardTurn)`，`stream` 出错走
    `RunStream::rollback`（`DiscardTurn`），回到最近 committed 一致点，`Chat`/`ChatSession` 仍可用。
  - snapshot 不含 secret/client：`ChatSession::snapshot` 只产 `ConversationSnapshot`（数据）；
    `snapshot_is_data_only_and_round_trips` 断言序列化文本不含 `client`/`api_key`/`LlmClient`。
- `prelude`（`src/prelude.rs`）只重导已存在类型
  `Chat/ChatSession/ModelConfig/ProviderConfig/Reply/RunEvent/RunOutput/RunStream`；集成测试
  `tests/smoke.rs::prelude_and_direct_paths_agree` 已保证 prelude 与直接路径一致。rustdoc 完整、doctest 可编译
  （`RUSTDOCFLAGS="-D warnings" cargo doc` 绿）。
- §16 `FacadeError`：已加变体 `Config`/`Client`/`Conversation`/`UnexpectedToolUse`/`InvalidState` 命名与 §16 一致；
  `#[non_exhaustive]`，其余变体留待后续里程碑（M2+）补，符合任务「允许尚未全加」。
- **有意取舍差异（均已在代码 rustdoc 注明，非偏离）**：
  - `FacadeError::Config(String)` vs §16 `Config(ConfigError)`：本 crate 无 `ConfigError` 类型，暂用 `String`
    承载配置文案（变体名一致、payload 简化）。
  - `RunEvent::Done(Box<RunOutput>)` vs §6.3 未装箱：避免大终态变体膨胀每个 `RunEvent`（`run.rs` 已注）。
  - `Reply.usage: Option<Usage>` vs §6.1 `TokenUsage`：本 crate 具体类型为 `model::usage::Usage`（`run.rs` 已注）。
  - `RunEvent` 不派生 serde（`PLAN.md` R7）：`RawStream`/`RawNotification` 逃生舱不作序列化承诺。
- **§5–§6 承诺项 vs M1 实现对照（无缺口，无需新增后续任务）**：
  - §5.2 `Chat`：`builder`/`ask`/`ask_full`/`session` ✅；`ChatSession`：`send`/`send_full`/`stream`/
    `conversation`/`snapshot`/`restore` ✅。
  - §5.2 `IntoUserMessage`：`&str`/`String`/`Message`/`Vec<ContentBlock>` ✅。
  - §5.4 streaming：内部 `Accumulator` 折叠 `Response`、末尾 `Done` 提交、转发 `TextDelta`+`RawStream` ✅。
  - §6.1 `Reply`（`text`/`usage`/`stop_reason`）✅；§6.2 `RunOutput`（7 字段齐全）✅；§6.3 `RunEvent`（全变体定义）✅。
- 小范围文档一致性修正：`docs/facade-api.md` §3 prelude 目标示例补入 `RunStream`（doc-only，不改编译产物）。
- 验证：`cargo fmt --all -- --check` ✅；`cargo clippy --all-targets -- -D warnings` ✅（未触碰 external adapter）；
  `cargo test -p agent-lib --lib facade::` 39 passed ✅；`cargo test --all --all-targets` 全绿 ✅；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✅；`git diff --check` 干净 ✅。

---

## Milestone 2 — 基础 Agent facade

目标：`docs/facade-api.md` §7–§9、§8.3。typed function `Tool` + `ToolContext`、`Approval` 三档 +
`ApprovalPolicy`、`Agent`（`run`/`run_full`/`stream`/`snapshot`/`into_parts`）。内部装配
`AgentSpec`→`AgentState`→`DefaultAgentMachine`→`ReferenceScope`/`HandlerScope`→`RunContext`→`drain`
（参照 `examples/agent_chat.rs`）。用户不直接看到 `Requirement`。

### [DONE] M2-1 typed function `Tool` + `ToolContext` + 内部 `ToolRegistry` 桥接

**上下文**：

- `docs/facade-api.md` §7：`Tool::function(name, desc, async fn(ToolContext, Args) -> Result<T>)`。
  负责 `Args -> JSON schema -> ToolDeclaration`、`Value -> Args`、`Result<T> -> ToolResult`。返回值第一版支持
  `String`/`serde_json::Value`/`impl Serialize`/显式 `ToolResult`。
- 锚点：`agent::{ToolRegistry, ToolExecutor}`、`model::tool::{Tool as ToolDecl, ToolCall, ToolResponse,
  ToolStatus}`（见 `examples/agent_chat.rs` 的 `WeatherRegistry`）、`ToolRuntimeError`。
- **schemars 风险（`PLAN.md` R1，硬决策点）**：`Args -> JSON schema` 需要 schema 派生，`schemars`
  **当前不是依赖**。先决定方案：优先把 typed schema 派生放在**新增可选 feature**（如 `facade-schema`）或允许
  用户显式传 `input_schema`（无 feature 时的降级路径），避免给核心 crate 强加 `schemars`。若确认无依赖无法
  实现合格 typed tool，则**先停下**：在本文件 M2-1 之前插入前置任务「引入 schema 依赖并划定 feature 边界」，
  让 M2-1 显式依赖它，提交并停止（不得用 workaround 蒙混）。

**做什么**：

- 建 `src/facade/tool.rs`：`Tool`（facade 层，非 `model::tool::Tool`，注意命名/re-export 避免歧义）、
  `Tool::function(...)`、`ToolContext{run_id, agent_id, tool_call_id, worktree, cancel, trace}`
  （用现有 `RunId/AgentId/ToolCallId/WorktreeRef/CancellationToken/TraceHandle` 锚点，`ToolContext` 只给
  受控 handle，不暴露破坏 Conversation 不变量的可变引用）。
- 内部把 facade `Tool` 集合桥接成一个实现 `agent::ToolRegistry` 的适配器（`declarations()` 汇出
  `ToolDecl`，`execute()` 反序列化 args、调用闭包、序列化结果为 `ToolResponse`）。
- 逃生舱（§7.3）：`.tool_registry(my_registry)` + `.tool_declarations(..)`；与 typed tool 混用时 build 期
  检查 name 冲突并报 `FacadeError`。
- 按 R1 决策实现 schema 派生（feature 或显式 schema）；在 rustdoc 与 `PLAN.md`/本文件注明所选方案。

**验证条件**：

- 单元测试：typed `Tool::function` 的 `declarations()` schema 正确、`execute()` 对合法/非法 args 行为正确
  （非法 args → 结构化错误）、`String`/`Value`/`Serialize`/`ToolResult` 四种返回都能归一化；name 冲突 build
  期报错。
- 聚焦：`cargo test -p agent-lib facade::tool`（若引入 feature，附带 `--features <schema-feature>` 的聚焦跑）。
- 完整验证序列 1–6；若新增 feature，额外跑该 feature 的 clippy/test。

**完成记录**：

- **R1 schema 决策**：新增 off-by-default feature `facade-schema = ["dep:schemars"]`（`schemars = { version = "1",
  optional = true }`）。开启后 `Tool::function(name, desc, handler)`（`Args: schemars::JsonSchema`）派生 schema
  并去掉顶层 `$schema` 元键；**始终可用** `Tool::function_with_schema(name, desc, input_schema: Value, handler)`
  为无 feature 降级路径。默认 `cargo build` 不链接 `schemars`。已在 `src/facade/tool.rs` rustdoc、`PLAN.md` R1、
  本记录三处注明。未新增任何前置任务。
- **落地**：新增 `src/facade/tool.rs`。
  - `Tool`（facade，`Clone` + 手写 `Debug`，与 `model::tool::Tool` 用 `ToolDecl` 别名消歧）、
    `Tool::function`/`function_with_schema`、`declaration() -> model::tool::Tool`。
  - `ToolContext{run_id, agent_id, tool_call_id, worktree, cancel, trace}`：全部为受控 Clone handle
    （锚点 `RunId`/`AgentId`/`ToolCallId`/`WorktreeRef`/`CancellationToken`/`TraceHandle`），不暴露破坏
    Conversation 不变量的可变引用。
  - `ToolResult`（facade 结果，**不** derive `Serialize` 以保证下述归一化 impl 相容）+ `IntoToolResult`：
    blanket `impl<T: Serialize>`（`Value::String` → 原文本，其它 → 紧凑 JSON 文本）+ `impl for ToolResult`，
    覆盖 `String`/`Value`/`impl Serialize`/显式 `ToolResult` 四种返回。
  - `FacadeToolRegistry` 实现 `agent::ToolRegistry`：`declarations()` 汇出 typed + escape-hatch 声明，
    `execute()` 反序列化 args（非法 → 结构化 `ToolRuntimeError::ExecutionFailed`）、构造 `ToolContext`、
    调闭包、归一化结果为 `ToolResponse`；工具失败一律 `ExecutionFailed`，交给 loop 的 `ToolFailurePolicy`
    裁决（不自行 pre-empt）。逃生舱（§7.3）：可选 `custom` registry + `extra` 声明；构造期跨 typed/extra/custom
    检查 name 冲突 → 新增 `FacadeError::DuplicateTool`。
  - `FacadeToolRegistry`/`ToolContextParts` 设为 `pub`（装配 seam，M2-3 装配、进阶用户可手工复用），
    避免 dead-code 告警而无需 `#[allow(dead_code)]`。
  - `facade/mod.rs` + `facade` root 导出 `Tool/ToolContext/ToolResult/IntoToolResult/FacadeToolRegistry/
    ToolContextParts`（prelude 增补留待 M2-R）。
- **测试**（`src/facade/tool.rs` 离线单测）：declaration schema 正确；合法 args → Ok；非法 args → 结构化
  `ExecutionFailed`；handler Err → `ExecutionFailed`；unknown tool → `UnknownTool`；四种返回归一化正确；
  显式 `ToolResult` 状态贯通 `execute`；typed/extra/custom name 冲突构造期报错；custom registry 声明合并 +
  委派执行；`#[cfg(feature = "facade-schema")]` 下 `function` 从 `Args` 派生 schema（无 `$schema`）。
- **验证**：`cargo fmt --all -- --check` ✅｜`cargo test -p agent-lib --lib facade::tool` 默认 10 / `--features
  facade-schema` 11 全绿 ✅｜`cargo clippy --all-targets -- -D warnings` 默认 + `--features facade-schema` 均 0
  警告 ✅｜`cargo test --all --all-targets` 全绿（lib 152 通过）✅｜`RUSTDOCFLAGS="-D warnings" cargo doc
  --no-deps --workspace` 默认 + `--features facade-schema` 均通过（doctest 默认 1 / feature 2 通过）✅｜
  `git diff --check` 干净 ✅。

### [DONE] M2-2 `Approval` 三档 + `ApprovalPolicy` → `ToolApprovalPolicy`/`InteractionHandler`

**上下文**：

- `docs/facade-api.md` §9：`Approval::{auto_allow, auto_deny, ask(handler)}`；工具级可覆盖
  `Tool::function(..).approval(..)`；agent 级 `ApprovalPolicy::default().allow_tool(..).ask_tool(..)
  .ask_external_agents().ask_worktree_write()`。§9.2 默认权限语义表。
- 锚点：`agent::{ToolApprovalPolicy, ApprovalRequirement, Interaction, InteractionHandler, InteractionKind,
  ApprovalDecision, ApprovalResponse, InteractionResponse}`（见 `examples/agent_chat.rs` 的
  `RequireApproval`/`StdinApproval`）。headless 且无匹配 policy → deny 或 error，不静默等待（§9.2）。

**做什么**：

- 建 `src/facade/approval.rs`：`Approval` 三档 + `ApprovalPolicy` builder。
- 把 facade approval 映射为实现 `agent::ToolApprovalPolicy` 的适配器（按工具名/策略产 `ApprovalRequirement`），
  以及实现 `agent::InteractionHandler` 的适配器（`Approval::ask(handler)` 调用用户 handler；`auto_allow`/
  `auto_deny` 直接产 `ApprovalResponse`；`InteractionKind::Approval` 之外的 kind 走合理默认或 error）。
- headless（无 ask handler）遇到需审批工具 → `FacadeError::ApprovalDenied`/`PermissionDenied`，不阻塞。
- rustdoc 完整。

**验证条件**：

- 单元测试：`auto_allow` 放行、`auto_deny` 拒绝（→ `ApprovalDenied`）、`ask` 调用自定义 handler 并按其决策
  执行；工具级覆盖优先于 agent 级；headless 无匹配 policy → error 而非挂起。
- 聚焦：`cargo test -p agent-lib facade::approval`。
- 完整验证序列 1–6。

**完成记录**：

- **落地**：新增 `src/facade/approval.rs`。
  - `Approval` 三档（`auto_allow`/`auto_deny`/`ask(handler)`，`Clone` + 手写 `Debug`）。`ask` handler 为
    `Fn(&ApprovalRequest) -> ApprovalDecision + Send + Sync`；`ApprovalDecision` 从 `crate::agent` re-export。
    内部另有 `Ask(None)` 档（不对外暴露），供 `ApprovalPolicy::ask_tool` 使用（下沉到 policy default handler，
    否则 headless deny）。
  - `ApprovalPolicy`（`Clone` + 手写 `Debug`）：`{ default, per_tool, ask_external_agents, ask_worktree_write }`。
    `Default` = default 档为 `auto_allow`（typed tool 是用户自写 Rust 函数，默认放行）。builder：`new`/
    `allow_tool`/`ask_tool`/`deny_tool`/`tool(name, Approval)`/`ask_external_agents`/`ask_worktree_write` +
    两个 flag 的 getter。`impl From<Approval> for ApprovalPolicy`（AgentBuilder `.approval(..)` 可收 `Approval`
    或 `ApprovalPolicy`，M2-3 用 `Into`）。§9.2 的 external/worktree flag 先记录、M4 再消费。
  - `FacadeApproval` 同时实现 `agent::ToolApprovalPolicy` 与 `agent::InteractionHandler`（`Arc` 共享）：
    `approval_requirement` 对 `auto_allow` 产 `AutoApprove`，其余产 `RequireApproval` 并把已解析决策
    （`Deny{msg}` / `Ask{request,handler}`）写入共享 `Mutex<HashMap<ToolCallId, PendingDecision>>`；
    `fulfill` 对 `InteractionKind::Approval` 弹出决策产 `ApprovalResponse`（`auto_deny`/headless→Deny，
    `ask`→调 handler），对 `Question`/`Choice` 走 in-family 平凡默认，对 `Permission` 默认 deny（§9.2，M4 细化）。
    `InteractionKind::Approval` 只带 `call_id` 不带工具名 → 通过 policy（先）→ interaction（后）的调用序共享
    pending map 关联，避免脆弱的 reason 解析。解析优先级：tool-level override > policy per_tool > default。
    `with_tool_override(name, Approval)` 供 M2-3 注入每工具级覆盖。
- **Tool 工具级覆盖**（`src/facade/tool.rs`）：`Tool` 增 `approval: Option<Approval>` 字段 + `.approval(Approval)`
  builder + `approval_override()` getter；`Debug` 增 `has_approval_override`。
- **错误**（`src/facade/error.rs`）：新增 `FacadeError::{ApprovalDenied, PermissionDenied}` 单元变体（§16）。
  run-path 把 deny 决策映射为 `ApprovalDenied` 属 M2-3 装配职责（本任务只备好变体与适配器）。
- **导出**：`facade/mod.rs` + root 导出 `Approval/ApprovalDecision/ApprovalPolicy/FacadeApproval`（prelude
  增补留待 M2-R）。
- **测试**（`src/facade/approval.rs` 离线单测 8 个）：`auto_allow`→`AutoApprove`；`auto_deny`→`RequireApproval`
  且 `fulfill` 产 Deny；`ask`→调 handler 并按其 Approve/Deny 决策；tool-level override 覆盖 agent policy；
  `ask_tool` 无 handler（headless）→Deny 不挂起；`ask_tool` 回落 policy default handler；`Question`/`Choice`/
  `Permission` 非审批 kind 的安全默认；`From<Approval>` + flag getter。
- **验证**：`cargo fmt --all` ✅｜`cargo clippy --all-targets -- -D warnings` 默认 + `--features facade-schema`
  均 0 警告 ✅｜`cargo test -p agent-lib --lib facade::approval` 8 全绿 ✅｜`cargo test --all --all-targets`
  全绿（agent-lib lib 720 通过）✅｜`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 默认 +
  `--features facade-schema` 均通过；`cargo test --doc -p agent-lib` 默认 14 / `--features facade-schema` 15
  全绿 ✅｜`git diff --check` 干净 ✅。

### [DONE] M2-3 `Agent` / `AgentBuilder` + `run` / `run_full`（装配 machine + drive）

**上下文**：

- `docs/facade-api.md` §8：`Agent::builder().provider(..).model(..).system(..).tool(..).approval(..)
  .build()`；`run(&mut self, input) -> Reply`、`run_full(..) -> RunOutput`。§8.3 内部映射：
  `AgentBuilder -> AgentSpec -> AgentState(Conversation::new) -> DefaultAgentMachine ->
  RequirementIds+ToolExecutionIds -> ReferenceScope(client, registry, interaction) -> RunContext ->
  drive_turn/drain`。§8.4 loop policy 默认（`max_steps=8`、`max_tool_rounds=4`、
  `tool_failure_policy=ReturnErrorToModel`）。
- 精确样板：`examples/agent_chat.rs`（`AgentSpec::new` 参数、`DefaultAgentMachine::new(state, LlmStepMode,
  ids).with_tool_execution_ids(ids).with_approval_policy(..)`、`drain(&mut machine, input, &scope, None,
  &ctx)`、`AgentInput::user_message(..)`、`RunContext::new_root(run_id, BudgetLimits::unbounded(),
  trace_root)`）。`ReferenceScope::new(client, registry).with_interaction(..)` 是现成 total scope。

**做什么**：

- 建 `src/facade/agent.rs`：`Agent` + `AgentBuilder`。builder 收集 provider/model/system/tools/approval/
  loop policy，`build()` 内部装配 §8.3 全链路，把 M2-1 的 tool 桥接成 registry、M2-2 的 approval 桥接成
  `ToolApprovalPolicy`+interaction，用 `ReferenceScope`（或自建 `HandlerScope`）承载 llm+tool+interaction。
- `run`/`run_full`：每轮 `AgentInput::user_message` + `drain`；从 machine 的 `AgentState.conversation()`
  取最终 assistant 文本组 `Reply`；`RunOutput` 填 response/usage/tool_calls（从本轮 trace/notifications 收集
  `ToolTrace`）；`LoopCursor::Error` → 对应 `FacadeError`（`LoopLimitExceeded`/`Agent(..)`）。
- loop policy 默认与覆盖：`.max_steps(..).max_tool_rounds(..).tool_failure_policy(..)`（→ `LoopPolicy`）。
- pending 失败默认 cancel（§16、§8.4）。rustdoc + 离线 doctest。

**验证条件**：

- 单元测试（离线 fake `LlmClient` 脚本化「先 tool-use 后 final text」+ 脚本工具）：`run` 完成一次
  tool round-trip 并返回最终文本；`run_full` 的 `tool_calls` 记录该工具调用；`auto_deny` 时工具不执行且
  行为符合策略；`max_tool_rounds` 超限 → `LoopLimitExceeded`。
- 聚焦：`cargo test -p agent-lib facade::agent`。
- 完整验证序列 1–6。

**完成记录**：

- 新增 `src/facade/agent.rs`：`Agent` + `AgentBuilder`。`build()` 一次性装配 §8.3 全链路
  —— typed tools + escape-hatch 声明 → `ToolSetRef`；`AgentSpec`（worktree/system/model/loop policy）
  → `AgentState(Conversation::new)` → `DefaultAgentMachine::new(state, NonStreaming, FacadeIds)`
  `.with_tool_execution_ids(FacadeIds)` `.with_approval_policy(FacadeApproval)`。machine 建一次、跨 `run`
  持有（多轮历史累积）；每轮重建 run-scoped `FacadeToolRegistry` + 自建 `FacadeAgentScope: HandlerScope`
  （llm + tool + 共享 `Arc<FacadeApproval>` 作 interaction），`AgentInput::user_message` + `drain` 驱动。
- `run_full`：`LoopCursor::Done` → 从已提交 turn 取最终 assistant 文本 / 聚合 usage / stop_reason 组
  `Reply::from_parts`（`response=None`，因 drive 折叠 `Response`）；从本轮 `ToolCallStarted/Finished`
  notifications 收集 `ToolTrace` 填 `tool_calls` 与 `RunEvent::ToolStarted/ToolFinished`。`run` = `run_full().reply`。
- loop policy 映射（§8.4）：`effective_max_steps = min(max_steps, max_tool_rounds+1).max(1)`，
  `max_parallel_tools=1`；默认 `max_steps=8 / max_tool_rounds=4 / ToolFailurePolicy::ReturnErrorToModel`。
  step 限额耗尽（机器发 `"agent loop step limit N reached ..."`）→ `FacadeError::LoopLimitExceeded`，
  其余错误 cursor → `FacadeError::Agent(AgentError::Other(..))`。
- 支撑改动：`error.rs` 增 `FacadeError::{Agent(#[from] AgentError), LoopLimitExceeded}`；`run.rs` 增
  `pub(crate) Reply::from_parts`；`tool.rs` 抽出 `pub(crate) ensure_unique_tool_names` 供 build 期查重复用；
  `chat.rs` 的 `client_for_provider` 提升为 `pub(crate)`；`mod.rs` 导出 `pub mod agent; pub use agent::{Agent, AgentBuilder};`。
- 测试 `src/facade/agent/tests.rs`（全离线，脚本化 `ScriptedClient` + 计数 typed tool，各 < 1s）：
  `run` 完成一次 tool round-trip 返回最终文本且工具恰执行一次；`run_full` 记录 `tool_calls` + 事件 +
  聚合 usage；`auto_deny` 时工具不执行且返回最终文本；always-tool-use（每轮唯一 provider id）+
  `max_tool_rounds=1` → `LoopLimitExceeded`；多轮 `run` 历史累积；缺 model / 重名工具 build 报错。
  （修复的一处真实行为：脚本客户端若跨轮复用同一 provider call id，会先触发 Conversation 重复 id
  报错而非 loop limit —— 真实模型每轮 id 唯一，故 fixture 改为逐轮生成唯一 id，未绕过任何底层路径。）
- 验证：`cargo fmt --all` ✅｜`cargo clippy --all-targets -- -D warnings` 默认 + `--features facade-schema`
  均 0 警告 ✅｜`cargo test -p agent-lib facade::agent` 7 全绿 ✅｜`cargo test --all --all-targets`
  全绿（983 passed / 0 failed）✅｜`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 默认 +
  `--features facade-schema` 均通过；`cargo test --doc -p agent-lib` 12 全绿 ✅｜`git diff --check` 干净 ✅。
- 说明：`prelude` 增补 `Agent/Tool/Approval/...` 与 `stream`/`snapshot`/`restore`/`into_parts` 按计划留待
  M2-4 / M2-R；本任务仅从 `facade` 根导出 `Agent`/`AgentBuilder`。

### [DONE] M2-4 `Agent::stream` + `snapshot`/`restore` + `into_parts`

**上下文**：

- `docs/facade-api.md` §8.2、§15.2：`Agent::stream`、`state()`、`snapshot() -> AgentSnapshot`、
  `restore()`、`into_parts() -> AgentParts`。M2 的 `AgentSnapshot` 只需 supervisor `ConversationSnapshot` +
  `AgentStateSnapshot`（delegates/pending_delegations/mailbox/... 属 M3/M4/M6，先留空/Option::None）。
- 锚点：`agent::AgentState`（含 `snapshot`/restore 能力，见 `docs/agent-layer.md`）、
  `LlmStepMode::Streaming` 用于 `stream`。

**做什么**：

- `Agent::stream(&mut self, input) -> Result<RunStream, FacadeError>`：以 streaming step mode 驱动，转发
  `TextDelta`/`ToolStarted`/`ToolFinished`/`ApprovalRequested`，末尾 `Done(RunOutput)`。
- `snapshot()`/`restore()`（`AgentRestoreBuilder`，重新注入 provider/client/tools/approval）与
  `into_parts()` 逃生舱（交出内部 `AgentState`/`Conversation` 等）。M2 snapshot 只含 supervisor 部分，
  delegate 字段留待 M3/M4。
- rustdoc 完整。

**验证条件**：

- 单元测试（离线）：`stream` 事件序列正确且末尾 `Done` 与 `run_full` 结果一致；`snapshot()`→`restore()`
  后 `run` 能接上历史；snapshot 不含 client/凭据/闭包；`into_parts()` 交出可用的底层状态。
- 聚焦：`cargo test -p agent-lib facade::agent`（含 stream/snapshot 用例）。
- 完整验证序列 1–6。

**完成记录**：

- 新增 `src/facade/agent/stream.rs`：`Agent::stream(&mut self, input) -> Result<AgentRunStream<'_>, FacadeError>`。
  是 `ChatSession::stream` 的 tool/approval 版对应物。机器保持 `NonStreaming`，改由三个 *tapping* handler
  在 `drain` 过程中把实时 `RunEvent` 推进共享 sink：`StreamingTapHandler`（`LlmHandler`）无论请求 mode 一律
  走 `chat_stream`，用 `Accumulator` 折回同一个 `Response` 供机器消费，每个 `BlockDelta{Text}` 发 `TextDelta`；
  `TapToolHandler` 包裹参考 `ToolRegistryHandler`，围绕执行发 `ToolStarted`/`ToolFinished`；`TapInteractionHandler`
  包裹共享 `Arc<FacadeApproval>`，在委派前 peek 出待决工具名发 `ApprovalRequested`。未引入任何新 effect
  家族——机器跑的仍是普通循环。`AgentRunStream<'a>` 持 `Pin<Box<dyn Future<..>+'a>>`（借 `&mut machine`）+
  sink：`poll_next` 先排空 sink 再 poll future，`Ready(Ok)` 存 `RunOutput` 转 Draining、排空尾部事件后发唯一
  `Done`；`Pending` 时补发已入 sink 的事件避免停摆。终态 `RunOutput` 与 `run_full` 逐字段一致（同 `final_turn_summary`
  / `collect_tool_traces` / `classify_error`）。注册/输入校验错误在 `stream().await` 即刻返回。
- 新增 `src/facade/agent/snapshot.rs`：`AgentSnapshot`（`Clone/Debug/PartialEq/Serialize/Deserialize`，因
  `serde_json::Value` 无 `Eq` 故不派生 `Eq`）含 §15.2 的 `supervisor: ConversationSnapshot` +
  `agent_state: AgentStateSnapshot` + 预留空槽（`delegates/pending_delegations/artifacts` 空 `Vec`、
  `mailbox/blackboard/plan` `None`）。`AgentStateSnapshot` 为 `#[serde(transparent)]` 包 `serde_json::Value`
  的 newtype（`AgentState` 有 serde 但无 `Clone/PartialEq`）。`snapshot()` 先 `conversation().snapshot()?`（挂起 turn
  → 干净 `FacadeError::Conversation`）再序列化整个 state。`restore()` 返回 `AgentRestoreBuilder`：反序列化
  `agent_state` 为权威 `AgentState`（保留 spec/声明/model/loop policy/loop cursor），重注入 client（provider 或显式）
  + tools + approval，`ids = self.ids | FacadeIds::continuing_after(conversation)`。`into_parts() -> AgentParts`
  逃生舱交出 `state/client/tools/custom_registry/extra_declarations/approval/ids`。占位快照类型
  `DelegateSnapshot/DelegationSnapshot/MailboxSnapshot/BlackboardSnapshot`（空、`#[non_exhaustive]`）。
- `src/facade/approval.rs`：`PendingDecision::Deny` 增 `tool_name`（`record_pending` 两处 Deny 臂填入），
  新增 `PendingDecision::tool_name()` 与 `pub fn FacadeApproval::pending_tool_name(call_id) -> Option<String>`
  —— approval interaction 仅带 `call_id`，据此把已记录的待决决策工具名回补给 `ApprovalRequested`。
- `src/facade/agent.rs`：从 `build()` 抽出 `build_facade_approval` / `assemble_machine` 私有 helper 供 build 与
  restore 复用；声明 `mod stream; mod snapshot;` 并 `pub use` 新公有类型；`impl Agent` 增 `stream`/`snapshot`/
  `restore`/`into_parts`（`state()` 文档同步更新）。`mod.rs` 从 `facade` 根导出全部新类型。
- 测试 `src/facade/agent/tests.rs`（全离线，新增 `StreamingScriptedClient` + `text_stream`/`tool_stream` 帮手，
  各 < 1s，8 个新用例）：`stream` 文本分块重组为整段且末尾 `Done` 与 `run_full` 逐字段相等；tool round-trip 实时
  发 `ToolStarted`→`ToolFinished`（名对、序对）、尾部 text、`Done.tool_calls` 记录调用、工具恰执行一次、两步
  LLM；`auto_deny` 时发 `ApprovalRequested{get_weather}` 且工具不执行；`snapshot()`→JSON 往返相等且预留槽为空；
  `snapshot()`→`restore()`（换新 client + 重注入 tool）保留首轮并可接上第二轮；`into_parts()` 交出仍持历史的
  `state`；restore 缺 snapshot / 缺 client|provider 均 `Config` 报错。
- 验证：`cargo fmt --all` ✅｜`cargo clippy --all-targets -- -D warnings` 0 警告 ✅｜
  `cargo test -p agent-lib facade::agent` 15 全绿 ✅｜`cargo test --all --all-targets` 全绿
  （lib 735 passed / 0 failed，各集成 target 全绿）✅｜`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  通过 ✅｜`git diff --check` 干净 ✅。
- 说明：`prelude` 增补与 `RunStream` 别名收敛留待 M2-R。为避与 chat facade 既有 `RunStream` 同名冲突，agent 的
  流类型命名为 `AgentRunStream`（§8.2 中 `RunStream` 为概念名）。

### [DONE] M2-R Review：基础 Agent facade 正确性与文档一致性检查

**上下文**：M2-1..M2-4 落地了 typed tool、approval、Agent 装配与 run/stream/snapshot。仅审查+收敛。

**做什么**：

- 核对 `src/facade/{tool,approval,agent}.rs` 与 `docs/facade-api.md` §7–§9、§8.3–§8.4 一致：内部确实经
  `DefaultAgentMachine` + `drain` + `Requirement`（未绕过底层，§19）；typed tool schema 方案（R1）与所选
  feature 边界文档化；approval 三档 + 默认权限语义（§9.2）正确；loop policy 默认值对；pending 失败默认 cancel。
- 核对 `RunOutput.tool_calls`/events 能表达 tool trace（§6.2、§19）；`prelude` 增补 `Agent/Tool/Approval/
  ApprovalPolicy/ToolContext`。
- 修正小范围偏离；需要新功能对齐时按规则插前置任务。

**验证条件**：

- 完整验证序列 1–6 全绿。
- 对照表：M2 已实现 vs §7–§9 承诺项，缺口记为后续任务。

**完成记录**：

审查 `src/facade/{tool,approval,agent}.rs` 对照 `docs/facade-api.md` §6.2、§7–§9、
§8.3–§8.4、§19，结论：核心装配与语义均一致，仅 `prelude` 缺 M2 类型（唯一偏离），已修。

- **§8.3 内部映射（§19 不绕过底层）**：`assemble_machine` 建 `DefaultAgentMachine`
  （`.with_tool_execution_ids(FacadeIds)` + `.with_approval_policy(FacadeApproval)`），
  `run_full`/`stream` 经 `drain(&mut machine, AgentInput, &FacadeAgentScope, None, &ctx)`
  兑现 `Requirement`；scope 只暴露 llm/tool/interaction 三个 handler，其余 `None`。✓
- **§7.1 typed tool schema（R1）**：`Tool::function` 走 off-by-default `facade-schema`
  feature（`dep:schemars`），无 feature 时用 always-available `function_with_schema`；
  feature 边界在模块 doc + `Cargo.toml` 注释里写清。默认构建不链接 `schemars`。✓
  返回值 `String`/`Value`/`Serialize`/`ToolResult` 由 `impl<T: Serialize> IntoToolResult`
  + `impl IntoToolResult for ToolResult` 覆盖。✓
- **§7.2 ToolContext**：run_id/agent_id/tool_call_id/worktree/cancel/trace 全为受控
  clone handle，无 `&mut Conversation`。✓
- **§7.3 逃生舱**：`tool_registry` + `tool_declarations` builder 方法；build 期
  `ensure_unique_tool_names` 检查 typed/custom/declaration 三源 name 冲突。✓
- **§9.1/§9.2 approval**：三档（auto_allow/auto_deny/ask）+ 每工具 override（override >
  per-tool > default）；headless `ask` 无 handler 时 deny 而非阻塞；
  external-agent / worktree-write 标志记录待 M4 强制。`FacadeApproval` 同时实现
  `ToolApprovalPolicy` + `InteractionHandler`，共享 pending map。✓
- **§8.4 loop policy 默认值**：max_steps=8、max_tool_rounds=4、
  tool_failure_policy=ReturnErrorToModel、非流式（`LlmStepMode::NonStreaming`，`stream`
  另走 tap handler）、pending 失败丢弃未提交工作回到上一致点。`build_loop_policy` 映射为
  `min(max_steps, max_tool_rounds+1)` 单预算。✓
- **§6.2/§19 RunOutput**：`collect_tool_traces` 从 `Notification::ToolCallStarted/Finished`
  投影出 `RunOutput.tool_calls` 与 `RunEvent::ToolStarted/ToolFinished`；`RunEvent` 枚举涵盖
  tool/delegation/artifact/raw 全谱。✓

**修正**：`src/prelude.rs` 之前只导出 M1 类型；按本任务要求补齐 M2 的
`Agent / Tool / ToolContext / Approval / ApprovalPolicy`（§3 prelude 中已落地的子集），
并更新模块 doc。`AgentSession`/`Delegation`/`ManagedExternalAgent` 属后续里程碑，暂不导出。

**对照表（M2 已实现 vs §7–§9 承诺）**：

| §  | 承诺项 | 状态 |
|----|--------|------|
| 7.1 | typed function tool + schema 派生 | ✅ `function_with_schema` 常驻，`function` 由 `facade-schema` gate |
| 7.1 | 返回 String/Value/Serialize/ToolResult | ✅ |
| 7.2 | ToolContext（只读受控 handle） | ✅ blackboard/artifact/mailbox 写句柄留待后续里程碑 |
| 7.3 | 逃生到 ToolRegistry + name 冲突检查 | ✅ |
| 8.2 | run/run_full/stream/conversation/state/snapshot/restore/into_parts | ✅ |
| 8.2 | `Agent::worker()` | ⏳ 缺口 → 已排期 M3-1 |
| 8.3 | DefaultAgentMachine + drain + Requirement | ✅ |
| 8.4 | loop policy 默认值 + pending 失败 cancel | ✅ |
| 9.1 | 三档 approval + per-tool override | ✅ |
| 9.2 | 默认权限语义（headless deny、external/worktree 标志） | ✅ external/worktree 标志记录，M4 强制 |
| 3   | prelude 导出 M2 类型 | ✅ 本任务补齐；`AgentSession`/`Delegation`/`ManagedExternalAgent` → M3/M4/M5 |

未发现未排期的 spec 偏离或失败测试，无需新增前置任务。

**验证**：序列 1–6 全绿——`cargo fmt --all -- --check`；聚焦 `cargo test -p agent-lib
facade::`（72 passed）；`cargo clippy --all-targets -- -D warnings` 及
`--features facade-schema` 均 clean；`cargo test --all --all-targets` 全绿；
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` clean；`git diff --check` clean。

---

## Milestone 3 — Local subagent

目标：`docs/facade-api.md` §10、§13.1、§18.3。`Agent::worker()` 产 data-first `LocalSubagent` spec、
`.subagent(name, worker)`、model-routed delegation（默认每 subagent 一个工具 `ask_<name>`）、
`DelegationTrace`。完全复用 `NeedSubagent` / `SubagentHandler` / `NestedMachine`。

### [DONE] M3-1 `Agent::worker()` → `LocalSubagent` spec + `.subagent(..)` 注册

**上下文**：

- `docs/facade-api.md` §10.1、§10.3：`Agent::worker().model(..).system(..).build()` 产 data-first
  `LocalSubagent{name, description, spec: AgentSpec, tools: ToolSetRef, approval: ApprovalPolicy}`——
  **不是**已绑 live client 的完整 session；child `AgentState`/`AgentMachine`/`RunContext` 在 `NeedSubagent`
  兑现时才建。§20.4/`PLAN.md` R4：worker 默认继承 supervisor provider/model，也支持 `.model(..)` 显式、
  `.inherit_model()`。
- 锚点：`agent::{AgentSpec, ToolSetRef, NestedMachine, SubagentHandler}`。

**做什么**：

- 建 `src/facade/delegate.rs`（或 `subagent.rs`）：`AgentWorkerBuilder`（`Agent::worker()`）产 `LocalSubagent`。
- `AgentBuilder::subagent(name, LocalSubagent)` 登记；内部统一收进 delegate 表（为 M4 的统一 `Delegate`
  抽象预留，见 §12，但第一版可先只存 local）。
- worker 继承/显式 model 两模式（R4）。rustdoc 完整。

**验证条件**：

- 单元测试：`Agent::worker()` 产的 `LocalSubagent` 是 data-only（无 client/闭包字段）；继承与显式 model
  两模式都能构造；`.subagent(..)` 后 delegate 表含该 worker。
- 聚焦：`cargo test -p agent-lib facade::delegate`（或 `facade::subagent`）。
- 完整验证序列 1–6。

**完成记录**：

新建 `src/facade/delegate.rs`，落地 data-first 的 `AgentWorkerBuilder`（`Agent::worker()`）
与 `LocalSubagent`，并在 `AgentBuilder` 上接入 `.subagent(name, worker)` 登记，全部离线单测通过。

- **`Agent::worker()` → `LocalSubagent`（§10.3 data-first）**：`AgentWorkerBuilder`
  产 `LocalSubagent{name, description, spec: AgentSpec, tools: ToolSetRef, approval:
  ApprovalPolicy, inherit_model}`——只含数据，无 client/闭包/handler 字段；child
  `AgentState`/machine/`RunContext` 留待 `NeedSubagent` 兑现（M3-2）。字段用私有 + 访问器，
  与 `AgentSpec`/`ModelRef` 等既有数据类型的约定一致；`spec` 可经 serde 往返（测试断言）。
- **model 继承/显式两模式（R4）**：默认继承（`inherit_model=true`），`spec.model` 记
  占位模型 `<inherited>`，`inherits_model()` 报 `true`，真实 supervisor model 在兑现时替换；
  `.model(..)` 显式 pin（清除继承）、`.inherit_model()` 显式继承（清除 pin），最后一次调用生效。
- **`.subagent(name, LocalSubagent)` 登记**：`AgentBuilder` 增 `delegates: Vec<LocalSubagent>`，
  `.subagent()` 用 `with_name` 打上注册名并按序追加；build 后随 `Agent` 携带，经
  `Agent::subagents() -> &[LocalSubagent]` 暴露；`into_parts()`/`AgentParts.delegates` 一并携带。
  统一 delegate 抽象（§12）与 name 冲突 build 期报错留待 M3-3；restore 的 delegate 快照留待 M3-3
  （当前 restore 产空 delegate 表）。可执行 child tools 留待 M3-2（`LocalSubagent` 保持 data-only）。
- **复用**：worker 复用 agent.rs 的 `build_loop_policy` + `DEFAULT_MAX_STEPS`/
  `DEFAULT_MAX_TOOL_ROUNDS`（改 `pub(crate)`）；`mod.rs` 增 `pub mod delegate;` 并 re-export
  `AgentWorkerBuilder`/`LocalSubagent`。

**测试**：`src/facade/delegate.rs` 7 个单测（data-only 显式模型、默认继承、继承/显式切换 last-wins、
tool_declarations 进 spec、approval 携带、确定性 id、`with_name`）+ `src/facade/agent/tests.rs`
两个登记测试（`.subagent()` 后 delegate 表按序含 reviewer/researcher 且模型模式正确；`into_parts`
携带 delegate）。

**验证**：序列 1–6 全绿——`cargo fmt --all`；聚焦 `cargo test -p agent-lib --lib facade::delegate`
（7 passed）+ `facade::`（81 passed）；`cargo clippy --all-targets -- -D warnings` 及
`--features "external-claude-code external-codex external-opencode"` 均 clean；
`cargo test --all --all-targets` 全绿（含 16 doctests）；
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` clean；`git diff --check` clean。

### [DONE] M3-2 model-routed delegation：subagent 暴露为工具 + `NeedSubagent` 兑现 + `DelegationTrace`

**上下文**：

- `docs/facade-api.md` §10.1–§10.2、§13.1、§18.3：默认把每个 subagent 暴露成单独工具（`ask_<name>(task)`），
  模型调用后经 `NeedSubagent`→`SubagentHandler`→child `NestedMachine` drain 得结果，记入
  `RunOutput.delegations`（`DelegationTrace{worker, status, usage, ...}`）并产 `RunEvent::Delegation*`。
- 锚点：`agent::{SubagentHandler, NestedMachine, MachineTreeState}`、`drive` 的 `NeedSubagent` 路由
  （见归档 `docs/archive/.../` 与 `docs/agent-layer.md`；`drain` 对 `NeedSubagent` 走串行 + outer pop）。

**做什么**：

- 把每个注册 subagent 合成一个 delegation 工具声明（`ask_<name>`，输入含 `task`），加入 supervisor 的
  tool set；在 `HandlerScope` 里接 `SubagentHandler`，`NeedSubagent` 兑现时用 `LocalSubagent.spec` 建
  child `AgentState`/machine 并 nested drain，结果回灌为工具结果。
- 收集 `DelegationTrace` 进 `RunOutput.delegations`，沿途产 `RunEvent::DelegationStarted/Finished/Failed`。
- rustdoc + 离线 doctest。

**验证条件**：

- 单元测试（离线：supervisor fake client 脚本化「调用 `ask_reviewer`」→ child fake client 脚本化产结果）：
  子 agent 被正确 drive、结果回灌为 supervisor 工具结果并进入下一步；`RunOutput.delegations` 含一条
  `DelegationTrace`（worker 名、status、usage）；`RunEvent` 顺序含 `DelegationStarted`→`DelegationFinished`；
  子 agent 的需审批工具仍触发审批（§9.2）。
- 聚焦：`cargo test -p agent-lib facade::delegate`（含 model-routed 用例）。
- 完整验证序列 1–6。

**完成记录**：

在 `src/facade/delegate.rs` 落地 model-routed delegation 的兑现路径，并在 `agent.rs`/
`agent/stream.rs`/`run.rs`/`mod.rs` 上接线，全部离线单测通过。

- **subagent 暴露为工具（§10.1）**：`delegation_tool_name(name)="ask_<name>"` +
  `delegation_declaration(name, description)`（输入 schema 含必填 `task: string`，空描述给出
  terse 生成描述）。`AgentBuilder::build` 为每个已登记 subagent 追加一条 `ask_<name>` 声明到
  supervisor 的 `AgentSpec` tool set——因为发给模型的工具集取自 machine state 的
  `current_tool_set()`，声明必须在 build 期入 spec，run-scoped `FacadeToolRegistry` 不含它。
  name 冲突 build 期报错按 spec 留待 M3-3。
- **`NeedSubagent` 兑现（复用 `SubagentHandler`）**：`DefaultAgentMachine` 只发 `NeedTool`，
  故在 `NeedTool` 边界用 `DelegationToolHandler`（`ToolHandler`）拦截 `ask_<name>`：`is_delegation`
  命中即 `drive_delegation`，内部构 per-call `FacadeSubagentSpawner`（`SubagentSpawner`）+ 参考
  `DrivingSubagentHandler`（`DEFAULT_MAX_DELEGATION_DEPTH=8`）串行 drain child `NestedMachine`，
  非 delegation 名转发底层 `ToolRegistryHandler`。这正是「NeedSubagent→SubagentHandler→child
  drain」的忠实路径，只是在模型真正路由的工具边界做关联。
- **child 建模与回灌**：`spawn` 用 `LocalSubagent.spec` 重建 child `AgentSpec`/`AgentState`/machine；
  R4 继承时以 supervisor 具体 model 替换占位模型，否则用 worker 显式 model。`RecordingChildMachine`
  包裹具体 `DefaultAgentMachine`，在 `cursor()==Done` 时经 `final_turn_summary` 抓 `(text, usage)`
  存入共享 slot；`summarize` 读 slot 作 child 摘要，`delegation_response` 把摘要回灌为
  supervisor 的 `ToolResponse`（child 失败则折成 `ToolRuntimeError::ExecutionFailed`）。
- **DelegationTrace + 事件（§10.2/§18.3）**：新增 `DelegationStatus{Completed,Failed}` 并扩展
  `DelegationTrace{delegate, status, usage}`（`#[non_exhaustive]`）。`collect_traces` 依 recorder 的
  call_id 把通知分流为 delegation vs 普通 tool：delegation 进 `RunOutput.delegations` 且 usage 经
  `UsageSummary::add_subagent` 归入 `subagents` 切片，沿途产
  `RunEvent::DelegationStarted/Finished/Failed`（stream 路径的 `TapToolHandler` 同样分流）。
- **审批（§9.2）**：child scope 用 `LocalSubagent.approval()` 建 `FacadeApproval`，同时充当 child
  machine 的 `ToolApprovalPolicy` 与 scope 的 `InteractionHandler`，故子 agent 的需审批工具仍会暂停。

**测试**：`src/facade/delegate.rs` 新增 `delegation_declaration_advertises_ask_tool_with_task_input`
单测 + `model_routed_tests` 模块两例（离线 `RoutingClient` 按 `request.system` 标记分流 supervisor/child）：
（1）`model_routed_delegation_drives_child_and_folds_result`——supervisor 调 `ask_reviewer`→child 被
drive、摘要回灌为工具结果并推进到 supervisor 终局；`delegations` 恰一条（delegate=reviewer、
status=Completed、usage.input=11）；`usage.subagents` 折入 child usage；`tool_calls` 空；事件
`DelegationStarted`→`DelegationFinished` 有序且无普通 tool 事件。（2）
`child_approval_gated_tool_still_triggers_approval`——child 的 `shell` 工具经 ask/deny 审批仍被咨询
（`AtomicBool` 置位）。

**验证**：序列 1–6 全绿——`cargo fmt --all`；聚焦 `cargo test -p agent-lib --lib facade::delegate`
（10 passed）+ `facade::`（84 passed）；`cargo clippy --all-targets -- -D warnings` 及
`--features "external-claude-code external-codex external-opencode"` 均 clean；
`cargo test --all --all-targets` 全绿；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
clean（修正了因新 import 使 `AgentState`/`RunContext` 内链可解析而产生的 redundant-target 告警）；
`git diff --check` clean。

### [DONE] M3-3 `Delegation` 配置（model-routed 选项）+ 多 delegate + pending delegation snapshot

**上下文**：

- `docs/facade-api.md` §10.2、§13.1、§15.2：`Delegation::model_routed().expose_subagents_as_tools()`；
  高级 `Delegation::single_tool("delegate")`（统一工具）。`AgentSnapshot` 增
  `delegates`/`pending_delegations`（local subagent：存 child `AgentState`/`Conversation` snapshot，restore
  时重建 child machine，§15.2）。
- 锚点：M2-4 的 `AgentSnapshot`（本任务补 delegate 字段）。

**做什么**：

- 建 `Delegation` 配置类型（`src/facade/delegate.rs`）：`model_routed()`（默认，每 delegate 一工具）与
  `single_tool(name)`（统一 `delegate(agent, task)`）。`AgentBuilder::delegation(..)` 接入。
- 多 subagent：每个各暴露一工具（或统一工具按参数路由）；name 冲突 build 期报错。
- 扩展 `AgentSnapshot`：`delegates`（data-only spec）+ `pending_delegations`（进行中 child 的
  `ConversationSnapshot`/状态）；`restore()` 重建 child。task brief 默认不写入持久 snapshot（`PLAN.md` R5）。
- rustdoc 完整。

**验证条件**：

- 单元测试：两个 subagent 各暴露独立工具且可分别调用；`single_tool` 模式按参数路由到正确 worker；含进行中
  delegation 的 `snapshot()`→`restore()` 后能继续；snapshot 不含敏感 task brief 明文。
- 聚焦：`cargo test -p agent-lib facade::delegate`。
- 完整验证序列 1–6。

**完成记录**：

- **`Delegation` 配置类型（`src/facade/delegate.rs`）**：新增 `Delegation` + 私有 `DelegationMode`
  枚举（`PerSubagentTool`/`SingleTool{tool_name}`，`#[serde(tag="kind", rename_all="snake_case")]`）。
  构造器 `model_routed()`（默认，`Default` impl 亦指向它）与 `single_tool(name)`；
  `expose_subagents_as_tools()`/`expose_as_tools()` 为幂等精炼器（model_routed 已按工具暴露，故为
  no-op，两种拼写兼容 §13.1 与 §18.3/TODO）。`declarations(&[LocalSubagent])` 依模式产出声明
  （每 delegate 一 `ask_<name>` 或统一 `delegation_single_tool_declaration` 的 `delegate(agent, task)`，
  `agent` 为 delegate 名枚举 + `task` required），`route(&[LocalSubagent])` 产出运行期
  `DelegationRoute{PerSubagent|SingleTool}`。
- **`DelegationToolHandler` 重构**：改持 `DelegationRoute`；`fulfill` 依 `route.resolve(call)` 分流——
  `Delegate{subagent, task}` 驱动 child；`UnknownDelegate{requested, available}`（single_tool 指了未注册
  delegate）记 Failed trace 并回 `ToolRuntimeError::ExecutionFailed`（含 available 列表）；`NotDelegation`
  落基础注册表。single_tool 按 `input["agent"]` 路由到正确 worker。
- **多 delegate + name 冲突**：`AgentBuilder::build()` 把 `delegation.declarations(&delegates)` 追加进
  supervisor `AgentSpec` 工具集（LLM 请求工具来自 machine state，必须在 build 期就位），再经新增的
  `tool::ensure_unique_declaration_names(&[ToolDecl])` 扫描最终合并声明，重名（含 delegation 工具与普通
  工具/两个同名 delegate）回 `FacadeError::DuplicateTool`。
- **snapshot 扩展（`src/facade/agent/snapshot.rs`）**：`AgentSnapshot` 增 `delegates:
  Vec<DelegateSnapshot>`、`delegation: Delegation`、`pending_delegations: Vec<DelegationSnapshot>`。
  `DelegateSnapshot{name, description, spec: AgentSpec, tools: ToolSetRef, inherit_model}`（data-only，
  经 `LocalSubagent` 公开访问器抓取；`LocalSubagent` 非 Serialize/PartialEq，故 restore 经新增
  `LocalSubagent::from_parts` 重建并由调用方重注 approval）。`DelegationSnapshot{delegate,
  conversation: ConversationSnapshot}` + `capture(delegate, &Conversation)`/`restore_conversation()`
  重建 child 活动会话。同步 one-shot 兑现在单个 supervisor turn 内跑完 child，`snapshot()` 仅在提交点可用，
  故常规 capture 下 `pending_delegations` 恒空——这是架构的忠实结果而非 workaround；该类型/能力已完整实现并被单测直接验证。
  delegation 模式随 snapshot 持久化（避免脆弱推断），`AgentRestoreBuilder::subagent(name, worker)` 供
  restore 时重注 approval。R5：`DelegationTrace` 与 snapshot 均不另存 task brief 明文（brief 仅存活于
  child 会话内部）。
- **接线/导出/文档**：`Agent`/`AgentBuilder` 增 `delegation` 字段 + `.delegation(..)` builder 与访问器；
  `run_full`/`snapshot`/`into_parts` 全量接线。`mod.rs`/`prelude.rs` 导出 `Delegation`；
  `docs/facade-api.md` §15.2 的 `AgentSnapshot` 同步补 `delegation` 字段与说明。rustdoc 完整。
- **测试**：`model_routed_tests` 新增 6 例——`two_subagents_each_expose_independent_tools_and_route`
  （两 delegate 各暴露 `ask_reviewer`/`ask_researcher` 并分别路由、各自摘要回灌、两条 trace 有序）、
  `single_tool_delegation_routes_by_agent_argument`（统一 `delegate` 工具按 `agent` 参数路由到 researcher）、
  `duplicate_delegate_name_is_rejected_at_build`（两个 `reviewer` → `DuplicateTool{name:"ask_reviewer"}`）、
  `snapshot_carries_delegates_and_restore_can_delegate_again`（run→snapshot 携 delegate 数据与 model_routed
  模式→restore 后再次委派成功）、`snapshot_does_not_persist_the_task_brief_in_delegation_data`（delegates 与
  pending 序列化均不含运行期 brief）、`delegation_snapshot_round_trips_and_rebuilds_child_conversation`
  （`DelegationSnapshot` serde 往返 + `restore_conversation` 重建 child 会话且 turns 一致）。
- **验证**：序列 1–6 全绿——`cargo fmt --all`；聚焦 `cargo test -p agent-lib --lib facade::delegate`
  （16 passed，含 6 新例）；`cargo clippy --all-targets -- -D warnings` 及
  `--features "external-claude-code external-codex external-opencode"` 均 clean；
  `cargo test --all --all-targets` 全绿（lib 753 passed，全部集成二进制 0 失败）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` clean；`git diff --check` clean。

### [DONE] M3-R Review：Local subagent 正确性与文档一致性检查

**上下文**：M3-1..M3-3 落地 local subagent delegation。仅审查+收敛。

**做什么**：

- 核对与 `docs/facade-api.md` §10、§13.1 一致：`Agent::worker()` 产 data-first spec；child 在 `NeedSubagent`
  兑现时才建（复用 `SubagentHandler`/`NestedMachine`，未另造机制，§19）；model-routed 默认每 delegate 一工具；
  `DelegationTrace`/`RunEvent::Delegation*` 完整；snapshot/restore 覆盖 delegate 字段且不含 secret。
- `prelude` 增补 `Delegation`（若公开）。修正小范围偏离；需新功能按规则插前置任务。

**验证条件**：

- 完整验证序列 1–6 全绿。
- 对照表：M3 已实现 vs §10 承诺项，缺口记为后续任务。

**完成记录**：

纯审查 + 收敛任务，逐条核对 M3-1..M3-3 与 `docs/facade-api.md` §10/§13.1/§15.2/§19，未发现需修正的规范偏离，
故**无源码改动**；`prelude` 已导出 `Delegation`（`src/prelude.rs:18`），无需增补。审查结论：M3 实现忠实且完整。

- **§10.1 Local delegate** — ✅ `Agent::worker()` + `AgentBuilder::subagent(name, worker)` 已落地
  （`src/facade/delegate.rs`、`agent.rs`）。文档所示映射 `DefaultAgentMachine → ReferenceScope →
  SubagentHandler → NestedMachine child drain` 忠实实现：`DefaultAgentMachine` 仅发 `NeedTool`，故在 `NeedTool`
  边界用 `DelegationToolHandler` 拦截 `ask_<name>`，经 `FacadeSubagentSpawner` + 参考
  `DrivingSubagentHandler` 串行 drain child `NestedMachine`——复用既有 `NeedSubagent`/`SubagentHandler`/
  `NestedMachine` 机制，未另造 runtime（§19）。
- **§10.2 暴露形式** — ✅ 默认每 subagent 一工具 `ask_<name>(task)`（`DelegationMode::PerSubagentTool`）；
  高级统一工具 `delegate(agent, task)`（`Delegation::single_tool`，按 `agent` 参数路由）。
- **§10.3 Worker spec（data-first）** — ✅ `LocalSubagent{name, description, spec, tools, approval, inherit_model}`
  只含数据，无 client/闭包/handler；child `AgentState`/machine/`RunContext` 于兑现时才建。**审查确认的小偏离**：
  文档 sketch 示公开字段，实现改私有字段 + 访问器（与 `AgentSpec`/`ModelRef` 等既有数据类型约定一致，且
  `ApprovalPolicy` 含运行期 handler 不宜直接公开），并新增 `inherit_model`（R4）。属更优工程选择而非规范违背，
  完成记录已载明，不改。
- **§13.1 Model-routed** — ✅ 默认模式 = 每 delegate 一工具；`expose_as_tools()`/`expose_subagents_as_tools()`
  幂等精炼器（no-op，两拼写兼容）。
- **§10.2/§18.3 DelegationTrace + RunEvent** — ✅ `DelegationTrace{delegate, status, usage}`
  进 `RunOutput.delegations`，usage 经 `UsageSummary::add_subagent` 归入 `subagents`；同步与 stream 两路径均产
  `RunEvent::DelegationStarted → DelegationFinished/Failed`（`collect_traces` 与 stream 的 `TapToolHandler`）。
- **§15.2 Snapshot/Restore** — ✅ `AgentSnapshot` 携 `delegates: Vec<DelegateSnapshot>`（data-only，approval
  handler 剥离）、`delegation: Delegation`（路由模式）、`pending_delegations: Vec<DelegationSnapshot>`（同步
  one-shot 常规为空，类型/能力已就绪并被单测直验）。**无 secret**：client/provider 凭据/闭包/approval handler
  一律不入 snapshot，restore 时经 `AgentRestoreBuilder` 重注（R5：task brief 亦不落持久 snapshot）。
- **§9.2 child 审批** — ✅ child scope 用 `LocalSubagent.approval()` 建 `FacadeApproval`，兼作 child machine 的
  `ToolApprovalPolicy` 与 scope `InteractionHandler`，需审批 child 工具仍暂停。

**对照表 — §10（及相邻）承诺项 vs M3 实现（缺口 → 已排期任务）**：

| §    | 承诺项                                    | 状态 | 归属 |
|------|-------------------------------------------|------|------|
| 10.1 | local subagent 作为 local delegate        | ✅ 已实现 | M3-1..3 |
| 10.2 | 每 subagent 一工具 / 统一 `delegate` 工具 | ✅ 已实现 | M3-2/3 |
| 10.3 | data-first worker spec                    | ✅ 已实现 | M3-1 |
| 13.1 | model-routed（默认）                      | ✅ 已实现 | M3-2/3 |
| 15.2 | AgentSnapshot delegate 字段 + 无 secret   | ✅ 已实现 | M3-3 |
| 19   | 复用 `NeedSubagent`/`SubagentHandler`     | ✅ 已实现 | M3-2 |
| 11   | managed external agent 作为 external delegate | ⏳ 未实现（已排期）| M4-1..R |
| 12   | 统一 `Delegate`/`DelegateSpec` 抽象       | ⏳ 未实现（首版可不公开，随 external 增量）| M4 |
| 13.2 | rules-routed delegation                   | ⏳ 未实现（已排期）| M5-1 |
| 13.3 | dispatcher-routed delegation（cheap→verify→strong）| ⏳ 未实现（已排期）| M5-2 |
| 10.2 | `RunEvent::Delegation{Progress,Message,Artifact}` | ⏳ 类型已备，用于 external/streaming 委派 | M4 |

所有缺口均属后续里程碑且**已在 `TODO.md` 显式排期**（M4/M5），无需新增前置任务；M3（local subagent）承诺项
全部实现。

**验证**：序列 1–6 全绿——`cargo fmt --all`（无改动）；`cargo clippy --all-targets -- -D warnings` 及
`--features "external-claude-code external-codex external-opencode"` 均 clean；`cargo test --all --all-targets`
全绿；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` clean；`git diff --check` clean。
（本任务仅审查，未改动编译产物；测试套件复用同一 green 结果。）

---

## Milestone 4 — Managed external agent

目标：`docs/facade-api.md` §11、§15.2–§15.3、§18.4。`ManagedExternalAgent` 构造器（含 `::acp` 预设）+
`ExternalRunMode`/`ExternalAgentCapabilities` 能力分级 + `.external_agent(name, mea)` external delegate +
approval defaults（比 local 更保守）+ artifact trace + external restore policy。复用已落地的
`ExternalAgentMachine` / `ExternalSessionHandler` / runtime adapters（含 `AcpAdapter`）。

### [DONE] M4-1 `ManagedExternalAgent` 构造器 + `ExternalRunMode` + 能力分级校验

**完成记录**（2026-07-18）：

- 新增 `src/facade/external.rs`：
  - `ExternalRunMode`（`BlackBox`/`Managed`/`ManagedWithTools`/`Attachable`）+
    `required_capabilities()` 映射（BlackBox=∅；Managed={Streaming}；
    ManagedWithTools={Streaming,HostTools}；Attachable={Streaming,Resume}）+
    `as_str()`/`Display`/`ALL`/snake_case serde。
  - `ExternalAgentCapabilities`：facade 视图（wrap `ExternalRuntimeCapabilities`），
    `supports`/`supports_mode`/`missing_for_mode`/`supported_modes`/`runtime`/
    `as_runtime_capabilities`/`into_runtime_capabilities` +
    `#[cfg(external-acp)] from_acp_negotiation`（复用 `capabilities_from_initialize`）。
  - `ManagedExternalAgent`（data-first：runtime/mode/capabilities/worktree/binary/
    model/args/permission_mode，无句柄/凭证）+ 预设 `::claude_code()`/`::codex()`/
    `::opencode()`；`#[cfg(external-acp)]` 预设 `::acp(binary,args)`/
    `::claude_agent_acp()`/`::codex_acp()`/`::opencode_acp()`/`::gemini_acp()`。
  - `ManagedExternalAgentBuilder`：`.mode/.worktree/.binary/.model/.arg/.args/
    .permission_mode/.capabilities` + `#[cfg(external-acp)] .acp_negotiated`；
    `build()` 按能力校验 mode，超档 → fail fast。
- 各 runtime 默认能力如实映射对应 adapter `implemented_capabilities()`：Claude Code
  `permission_bridge=true`；Codex/OpenCode `false`；ACP 走 `capabilities_from_initialize`
  的协议保证档（streaming/permission_bridge/graceful，`resume` 由 `loadSession` 协商）。
  rustdoc 标注为「探针/协商前保守基线，运行时精化，不硬编码未验证档」。
- 新增 `FacadeError::UnsupportedExternalMode { runtime, mode, missing }`（fail fast，
  含缺失能力清单，非 secret）。`facade::mod` 导出四个新类型（prelude 补录留 M4-R）。

**验证**：

- `cargo test -p agent-lib --lib facade::external` → 8 passed（default）；
  `--features external-acp` → 10 passed（含 2 个 `#[cfg(feature="external-acp")]`
  ACP 协商用例）。覆盖：预设 runtime/默认能力、launch 数据记录、`args` 覆盖、
  ManagedWithTools 超档 fail fast（missing=`host_tools`）、BlackBox 恒支持、
  ACP `resume` 需 `loadSession` 协商方启用 Attachable。
- 完整验证序列 1–6 全绿：fmt；`clippy --all-targets -D warnings`（default）+
  4 features clippy 均 clean；`cargo test --all --all-targets` 全绿；
  `RUSTDOCFLAGS="-D warnings" cargo doc`（default + 4 features）clean；`git diff --check` clean。

### [DONE] M4-2 `.external_agent(..)` external delegate 兑现 + artifact/delegation trace

**上下文**：

- `docs/facade-api.md` §11.2：external delegate 接现有地基——`Agent facade -> Delegation policy ->
  NeedSubagent 或 NeedExternalSession -> ExternalAgentMachine -> ExternalSessionHandler -> runtime adapter`。
  推荐作为 child 时走 `NeedSubagent` 进 `ExternalAgentMachine`，其内部再 `NeedExternalSession` 推进 runtime，
  与 local subagent 共享 scope 派生/cancel/budget/trace/pop 语义。
- 锚点：`agent::external::{ExternalAgentMachine, ExternalSessionHandler, ExternalSessionRegistry}`；离线测试用
  scripted external runtime adapter（M5 归档已落地的 scripted/registry-backed handler，见
  `docs/archive/2026-07-17-managed-external-agent/`）。artifact 锚点：external observations 的 file patch/
  artifact → `RunOutput.artifacts`（`ArtifactRef`）。

**做什么**：

- `AgentBuilder::external_agent(name, ManagedExternalAgent)` 登记为 external delegate；默认经 model-routed
  暴露为工具 `ask_<name>`（§13.1）。
- 兑现路径：`NeedSubagent`→`ExternalAgentMachine`（scoped，registry-backed `ExternalSessionHandler`）→
  runtime adapter；收集 external observations 为 `DelegationTrace`（usage/status）+ `RunOutput.artifacts`
  （`ArtifactRef`），沿途产 `RunEvent::Delegation*`/`DelegationArtifact`。
- rustdoc + 离线 doctest（用 scripted adapter，不碰真实 CLI）。

**验证条件**：

- 单元测试（离线 scripted external runtime）：supervisor 调用 `ask_coder` → external session 走
  Start→…→Completed，结果回灌工具结果；`RunOutput.delegations` 含 external delegation（usage/status）；
  `RunOutput.artifacts` 含上报的 artifact；`RunEvent` 含 `DelegationStarted`/`DelegationArtifact`/
  `DelegationFinished`；cancel 时 external session 走 cleanup（cleanup 标记）。
- 聚焦：`cargo test -p agent-lib facade::external`（含 delegate 兑现用例）。
- 完整验证序列 1–6（+ external features clippy 同 M4-1）。

**完成记录**：

- `AgentBuilder::external_agent(name, ManagedExternalAgent)` 登记 external delegate；经 model-routed
  暴露为 `ask_<name>` 工具（与 local subagent 共用 declarations/route/single-tool 路径）。新增
  `ManagedExternalAgent::session_handler(..)` seam 注入 scoped `ExternalSessionHandler`。
- 兑现路径复用 local subagent 的 `DrivingSubagentHandler::fulfill`：`FacadeExternalSpawner` 构造
  `ExternalAgentMachine`（包在 `RecordingExternalMachine` 中，按步捕获 summary/usage/artifacts/
  cleanup/completed），由 `ExternalChildScope` 经注入的 handler 兑现 `NeedExternalSession`，
  外层 `EmptyExternalScope` + `ScopePop`。`drive_external` 把 `Subagent(Ok)` 映射为捕获的
  `ExternalDriveOutcome`，`Subagent(Err)` 映射为 `FacadeError::ExternalAgent`。
- external observations 折叠为 `DelegationTrace`（usage/status：`Completed` 当且仅当
  `completed && !cleanup_required`，否则 `Failed`）+ `RunOutput.artifacts`（`map_artifact`
  将 `ExternalArtifactRef` 归一为 `ArtifactRef`）；usage 走 `UsageSummary.external`（§17.3）。
  沿途产 `RunEvent::DelegationStarted`/`DelegationArtifact`/`DelegationFinished`（`run_full`
  与 streaming 两路一致）。
- 离线单测（`src/facade/delegate.rs` `model_routed_tests`，用 in-crate `FixedExternalSessionHandler`
  绕开 agent-testkit 的 crate-重复问题）：`ask_coder` Start→Completed、`RunOutput.delegations`
  含 external（usage/status）、`RunOutput.artifacts` 含上报 artifact、三个 `Delegation*` 事件、
  external delegate 广告为 `ask_` 工具、缺 session_handler 时 delegation 失败。新增
  `src/facade/external.rs` `drive_external_marks_cleanup_on_cancel`：预取消 `RunContext` 下
  external session 走 abandon→cleanup 标记（`cleanup_required` 置位、未 completed、无 artifact）。
- 验证：`cargo fmt --all` ✓；`cargo clippy --all-targets -- -D warnings` ✓；
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode" -- -D warnings` ✓；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✓；`cargo test --all --all-targets`
  ✓（0 失败）；`cargo test -p agent-lib --lib --features "external-claude-code external-codex external-opencode"`
  ✓（855 passed）。聚焦 `cargo test -p agent-lib --lib facade::` ✓（102 passed）。

### [DONE] M4-3 external approval defaults + restore policy + `AgentSnapshot` external 字段

**上下文**：

- `docs/facade-api.md` §9.2、§11、§15.2–§15.3：managed external agent 默认更保守——启动需审批、写工作区需
  审批或显式 opt-in、resume/attach 既有 session 需审批（`ApprovalPolicy::default().ask_external_agents()
  .ask_worktree_write()`）。external restore：`RestoreExternal::{AttachOrFail, MarkInterrupted,
  RestartFromBrief}`，默认 `MarkInterrupted`（`PLAN.md` R6）。`AgentSnapshot` 存 external delegate 的
  `external_session_id`/`runtime kind`/`worktree ref`/`last known status`/`task brief`/`artifact refs`/
  `transcript refs`，**不**存进程句柄/API key/client/闭包；restore 经 manager 重新 attach 或标 interrupted。
- 锚点：M2-4/M3-3 的 `AgentSnapshot`（本任务补 external 字段）、`Agent::restore()` builder（§15.3
  `.external_agent(name, manager).restore_external(RestoreExternal::..)`）。

**做什么**：

- external 审批默认接入：external delegate 的启动/写/resume 触发审批，headless 无匹配 policy → error
  （不静默等待）。工具/policy 覆盖遵循 §9。
- `RestoreExternal` 枚举 + `Agent::restore().restore_external(..)`；默认 `MarkInterrupted`；只读 external
  允许显式 `AttachOrFail`（R6）。
- 扩展 `AgentSnapshot` 的 external delegate 字段（data-only，见上）；restore 按策略 attach/mark/restart。
- rustdoc 完整。

**验证条件**：

- 单元测试（离线）：默认策略下 external 启动/写/resume 触发审批，`auto_deny`→`ApprovalDenied`；含 external
  session 的 `snapshot()` 只含 data-only 字段（断言无 handle/secret）；`restore_external(MarkInterrupted)`
  后该 delegate 标记 interrupted；`AttachOrFail` 在无法 attach 时 → 明确错误。
- 聚焦：`cargo test -p agent-lib facade::external`（含 approval/restore 用例）。
- 完整验证序列 1–6（+ external features clippy 同 M4-1）。

**完成记录**：

- **审批默认接入（drive-gate）**：`FacadeApproval` 新增 `external_tools: BTreeSet<String>` +
  `with_external_tools(..)`，并新增 `resolve_external_start(tool_name) -> bool`（同步决策：显式
  per-tool/override tier 优先；否则 `ask_external_agents` 时走 `decide_ask_deferred`（无
  handler/headless → deny），再否则用默认 tier）。external delegate 的启动在
  `drive_external_delegation` 处经 `resolve_external_start` 门控；拒绝时记 `Failed` delegation
  （`approval_denied=true`）并回灌被拒工具结果，`collect_traces` 置 `external_approval_denied`，
  `run_full`/streaming 两路返回 `FacadeError::ApprovalDenied`。model-routed 的 `ask_<name>` 工具在机器
  审批门中豁免（`external_tools` → `AutoApprove`），使 drive 成为唯一审批权威（不双重提示）。
  `ask_external_agents`/`ask_worktree_write` 由 M2-2「仅记录」升级为「强制执行」（rustdoc 同步更新）。
- **`RestoreExternal` 策略**：`src/facade/external.rs` 新增 `RestoreExternal::{AttachOrFail,
  MarkInterrupted(默认), RestartFromBrief}`（snake_case serde + `Display`）、`ExternalDelegateStatus`
  （Pending/Completed/Failed/Interrupted）、`RetainedExternalSession`（pub(crate)）。
  `Agent::restore().restore_external(..)` + `.external_agent(name, manager)` 覆盖；`build` 从
  `snapshot.external_delegates` 重建 external delegates（`ExternalDelegateSnapshot::to_delegate()`
  data-only recipe，经 `ManagedExternalAgent::from_restored_parts` 不注入 handler/不重校验 mode），再按名
  应用 override——与 local subagent restore 对称。`last_external_sessions` 依策略重建：
  MarkInterrupted→Interrupted+保留 session/artifacts；RestartFromBrief→Pending+清空；AttachOrFail 在无
  重新登记且可 attach 的 delegate（`session_handler().is_some()` 且 `snap.session.is_some()`）时 →
  `FacadeError::InvalidState`。
- **`AgentSnapshot` external 字段（data-only）**：新增 `external_delegates: Vec<ExternalDelegateSnapshot>`，
  字段仅含 external_session_id/runtime kind/worktree ref/last status/task brief/artifact refs/
  transcript refs——无进程句柄/API key/client/闭包。`capture(..)` 合并 `external_agents`（recipe）与
  `last_external_sessions`（status/session/artifacts）；`ExternalDriveOutcome` 新增
  `session: Option<ExternalSessionRef>` 并在 `RecordingExternalMachine::step` 捕获，`run_full` 于回合后
  刷新 `Agent.last_external_sessions`（streaming 路因 `&mut machine` 借用无法保留，已注明——快照在回合之间取）。
- **导出**：`RestoreExternal`/`ExternalDelegateStatus`/`ExternalDelegateSnapshot` 经 `src/facade/mod.rs`
  与 `src/facade/agent.rs` re-export。
- **离线单测**（`src/facade/delegate.rs` `model_routed_tests`，用 in-crate `FixedExternalSessionHandler`/
  `completed_external_handler`，不碰真实 CLI）：`auto_deny` → `ApprovalDenied`；`ask_external_agents` 无
  handler（headless）→ `ApprovalDenied`；`ask` handler 批准 → 跑到 Completed；driven 后 `snapshot()`
  external delegate 为 data-only 且含 session facts（断言无 handle/secret）；`restore_external(
  MarkInterrupted)` 后 delegate 标 interrupted；`AttachOrFail` 不可 attach 时 → 明确错误。另加
  `src/facade/external.rs` `RestoreExternal` doctest。
- **验证**：`cargo fmt --all` ✓；`cargo clippy --all-targets -- -D warnings` ✓；
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
  ✓；`cargo test --all --all-targets` ✓（0 失败）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✓；`git diff --check` ✓。

### [DONE] M4-R Review：Managed external agent 正确性与文档一致性检查

**上下文**：M4-1..M4-3 落地 external delegate。仅审查+收敛。

**做什么**：

- 核对与 `docs/facade-api.md` §11、§15.2–§15.3 一致：external delegate 经 `NeedSubagent`/
  `NeedExternalSession`→`ExternalAgentMachine`→`ExternalSessionHandler`→adapter（复用地基，未另造，§19）；
  能力档如实反映 `ExternalRuntimeCapabilities` + ACP 协商（R9）、不假装未验证档；external 默认更保守
  （启动/写/resume 审批）；restore 默认 `MarkInterrupted`；snapshot 不含 secret/handle/client。
- 核对 `RunOutput` 能同时表达 external delegation + artifact + events（§6.2、§19）；`prelude` 增补
  `ManagedExternalAgent`；文档（如 `docs/facade-api.md` 若需小修/或本 milestone 记差异）一致。
- 修正小范围偏离；需新功能按规则插前置任务。

**验证条件**：

- 完整验证序列 1–6 全绿，+ external features clippy 全绿。
- 对照表：M4 已实现 vs §11 承诺项，缺口记为后续任务。

**完成记录**（2026-07-18）：

审查 + 收敛任务，逐条核对 M4-1..M4-3 与 `docs/facade-api.md` §11/§9.2/§15.2–§15.3/§6.2/§19。
唯一源码改动是 **prelude 增补 `ManagedExternalAgent`**（M4-1 明确「prelude 补录留 M4-R」）：§3 prelude
清单仅列 `ManagedExternalAgent`（**不**含 `ExternalRunMode`/`RestoreExternal`），故只补此一项，忠于 §3
（`src/prelude.rs`，含 rustdoc 更新）。其余逐条核对**未发现需修正的规范偏离**。

- **§11.1 External delegate** — ✅ `AgentBuilder::external_agent(name, ManagedExternalAgent)` 登记
  external delegate，默认经 model-routed 暴露为 `ask_<name>`（`src/facade/agent.rs`、`delegate.rs`）；
  managed external agent 不压扁成普通函数工具（保留 session/artifact/worktree/权限/cancel 语义）。
- **§11.2 内部映射（复用地基，§19）** — ✅ 兑现路径 `NeedTool` 拦 `ask_<name>` →
  `FacadeExternalSpawner` 构 `ExternalAgentMachine`（`RecordingExternalMachine` 包装）→ 注入的 scoped
  `ExternalSessionHandler` 兑现 `NeedExternalSession` → runtime adapter；与 local subagent 共享
  scope 派生/cancel/budget/trace/pop（复用 `DrivingSubagentHandler`，未另造 runtime）。
  （`src/facade/external.rs` `drive_external`、`RecordingExternalMachine`。）
- **§11.3 能力分级（R9 真实性）** — ✅ `ExternalRunMode{BlackBox,Managed,ManagedWithTools,Attachable}`
  + `required_capabilities()` 映射；`ExternalAgentCapabilities` wrap `ExternalRuntimeCapabilities`
  （8 项），基线取各 adapter `implemented_capabilities()`，ACP 经 `capabilities_from_initialize`
  精化；`build()` 对超档 mode **fail fast**（`FacadeError::UnsupportedExternalMode{runtime,mode,missing}`）；
  rustdoc 标注为「探针/协商前保守基线，运行时精化，不硬编码未验证档」——不假装未验证档。
- **§9.2 external 默认更保守** — ✅ external delegate **启动**经 `FacadeApproval::resolve_external_start`
  在 drive 门控（override>per-tool>ask_external_agents?ask-deferred:default）；headless 无 handler →
  deny（`ApprovalDenied`），不静默等待。model-routed 的 `ask_<name>` 工具在机器审批门豁免，drive 为唯一
  审批权威（不双重提示）。**收敛说明**：`ask_worktree_write` 为 **advisory**（rustdoc 明载「Recorded for
  host inspection…advisory for the managed path」），因 managed child 恒在**一次性隔离 worktree**（用户经
  `.worktree()` 显式配置）运行、写入不触碰父 checkout，恰好满足 §9.2「写工作区 需要审批**或显式 opt-in**」的
  opt-in 分支，故非硬门——属规范内正确取值。M4-3 完成记录「`ask_worktree_write` 升级为强制执行」措辞对
  worktree-write 一项不精确（真正硬门的是 external-agent **启动**，经 `ask_external_agents`），此处校正，
  **不改源码**（行为已符合规范）。
- **§15.2 AgentSnapshot（data-only，无 secret）** — ✅ `external_delegates: Vec<ExternalDelegateSnapshot>`
  仅存 external_session_id/runtime kind/worktree ref/last status/task brief/artifact refs/transcript
  refs；**不**存进程句柄/API key/client/闭包（restore 时 `session_handler` 显式置 `None`，`Debug` 不透明）。
- **§15.3 External restore policy** — ✅ `RestoreExternal{AttachOrFail,MarkInterrupted,RestartFromBrief}`，
  `#[default]=MarkInterrupted`（`AgentRestoreBuilder` 派生 Default）；`AttachOrFail` 在无重登记 handler 或无
  可 resume session 时 → `FacadeError::InvalidState`（不静默 attach）。
- **§6.2/§19 RunOutput** — ✅ `RunOutput{reply,response:Option,usage,tool_calls,delegations,artifacts,
  events}` 与 §6.2 一致；external delegation 无 1:1 LLM `Response` 时仍产 `reply`/`delegations`/`artifacts`/
  events；`UsageSummary` 聚合 supervisor+subagent+external。`RunEvent::Delegation{Started,Artifact,
  Finished,Failed}` 两路（run_full/stream）一致。

**对照表 — §11（及相邻 §9.2/§15/§6.2/§19）承诺项 vs M4 实现**：

| §    | 承诺项 | 状态 | 归属 |
|------|--------|------|------|
| 11.1 | managed external agent 作为 external delegate（非普通函数工具） | ✅ 已实现 | M4-2 |
| 11.2 | 映射到地基 `NeedSubagent`→`ExternalAgentMachine`→`ExternalSessionHandler`→adapter（未另造，§19） | ✅ 已实现 | M4-2 |
| 11.3 | `ExternalRunMode` 四档 + `ExternalAgentCapabilities` 分级 | ✅ 已实现 | M4-1 |
| 11.3 | 构建时能力校验，超档 fail fast | ✅ 已实现 | M4-1 |
| 11.3 | 能力档如实反映 `ExternalRuntimeCapabilities`+ACP 协商，不假装未验证档（R9） | ✅ 已实现 | M4-1 |
| 9.2  | external 启动需审批；headless 无 policy → deny（不静默） | ✅ 已实现 | M4-3 |
| 9.2  | external 写工作区 需审批**或显式 opt-in**（worktree 隔离+`.worktree()` 显式配置满足 opt-in 分支；flag advisory） | ✅ 已实现（opt-in 分支） | M4-1/M4-3 |
| 15.2 | `AgentSnapshot` external 字段 data-only、无 secret/handle/client/闭包 | ✅ 已实现 | M4-3 |
| 15.3 | `RestoreExternal` 三档，默认 `MarkInterrupted` | ✅ 已实现 | M4-3 |
| 6.2/19 | `RunOutput` 同表 external delegation+artifact+events；`UsageSummary` 聚合 external | ✅ 已实现 | M4-2 |
| 3    | `prelude` 增补 `ManagedExternalAgent` | ✅ 已实现（本任务） | M4-R |
| 11.3 | `ManagedWithTools` 的 host-tool **运行期注入** | ⏳ 后续能力（`host_tools` 底层未落地，R8/R9；facade fail-fast 不假装） | 后续 |
| 11.3 | `Attachable` 的 **live attach/resume**（消费 retained session）+ resume/attach 审批门 | ⏳ 后续能力（`resume` 取决 ACP `loadSession` 协商，底层未落地，R8/R9；`AttachOrFail` 现仅保留 session facts） | 后续 |
| 12   | 统一 `Delegate`/`DelegateSpec` 抽象 | ⏳ 首版可不公开（随 external 增量） | 后续 |
| 13.2 | rules-routed delegation | ⏳ 未实现（已排期） | M5-1 |
| 13.3 | dispatcher-routed delegation | ⏳ 未实现（已排期） | M5-2 |

M4（managed external agent 构造/分级/兑现/审批/restore）承诺项**全部实现**。两个 ⏳「后续能力」缺口
（`ManagedWithTools` host-tool 注入、`Attachable` live resume + resume 审批门）由 `docs/facade-api.md`
§11.3 自身标注为「后续能力」/「取决于 ACP `loadSession` 协商」，且 `PLAN.md` R8/R9 明确 facade 只承诺底层
已落地的能力、未落地档位 fail-fast 不假装——现实现即如此（无 workaround、无假装档）。故属**规范自身延后的
底层门控能力**而非 M4 未兑现承诺，记入本对照表跟踪、**无需新增前置任务**（不阻塞、非 M4 承诺范围；与 M3-R
处理 §11/§12/§13 缺口方式一致）。待底层 host_tools / loadSession-resume 落地后再由对应 facade 任务承接。

**验证**（序列 1–6 全绿 + external features clippy 全绿）：

- `cargo fmt --all -- --check` ✓；聚焦 `cargo test -p agent-lib --lib facade::` ✓（108 passed）；
- `cargo clippy --all-targets -- -D warnings` ✓ + `--features "external-claude-code external-codex
  external-opencode external-acp"` ✓；
- `cargo test --all --all-targets` ✓（50 个测试二进制，0 失败）；
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✓；`git diff --check` ✓。

---

## Milestone 5 — Dispatcher / Escalator

目标：`docs/facade-api.md` §13.2–§13.3、§18.5。rules-routed 与 dispatcher-routed delegation，对应
`agent::external::Dispatcher` / `Escalator`：cheap→verify→strong 升级闭环，升级路径进 `DelegationTrace`。

### [DONE] M5-1 rules-routed delegation

**上下文**：

- `docs/facade-api.md` §13.2：`Delegation::rules().when_task_contains(["fix","test"], "coder")
  .when_task_contains(["review","audit"], "reviewer")`——由 facade/应用规则决定路由，模型可以不知道 delegate
  存在。
- 锚点：M3/M4 的 delegate 表 + `Delegation` 配置类型（M3-3）。

**做什么**：

- 扩展 `Delegation`：`rules()` builder + `when_task_contains(keywords, delegate_name)` 等规则；运行时按规则
  把任务路由到对应 delegate（local 或 external），不必把 delegate 暴露为模型工具。
- rustdoc 完整。

**验证条件**：

- 单元测试（离线）：任务文本命中规则 → 路由到正确 delegate 并执行；未命中 → 明确行为（不路由/默认）；
  规则优先级/多命中处理确定。
- 聚焦：`cargo test -p agent-lib facade::delegate`（含 rules 用例）。
- 完整验证序列 1–6。

**完成记录**：

- `src/facade/delegate.rs`：新增 `RoutingRule`（keywords + delegate，大小写不敏感子串匹配、任一关键词命中）
  与 `DelegationMode::Rules`；`Delegation` 新增 `rules()`、`when_task_contains(keywords, delegate)`（链式，
  非 rules 模式调用会切换为 rules）、`is_rules_routed()`、`route_task()`（首条命中规则胜出＝注册顺序即优先级）、
  `first_unknown_rule_delegate()`（build 期校验）。`declarations()`/`route()`/`external_tool_names()` 对 Rules
  模式返回空——即不向模型暴露任何 delegate 工具。新增 `RulesRoutedTarget`（Local/External，持有 owned clone）与
  `DelegationToolHandler::fulfill_rules_routed()`：合成 `ask_<name>(task)` 调用后复用既有
  `drive_delegation`/`drive_external_delegation`，因此 recorder、usage 归集、artifacts、§9.2 外部审批门完全一致。
- `src/facade/ids.rs`：新增 `fresh_tool_call_id()`（避免与 `ToolExecutionIds::tool_call_id` trait 方法冲突），
  为无模型工具调用的 rules 路由铸造 recorder key。
- `src/facade/agent.rs`：`run_full` 增加 rules 分支（命中即 `run_rules_routed`，未命中回落到普通 supervisor
  drive）；新增 `build_delegation_handler`/`resolve_rules_target`/`run_rules_routed` 与自由函数
  `drive_rules_routed`/`build_rules_routed_output`/`rules_routed_summary`/`user_message_text`。设计：rules 路由
  不经过 supervisor LLM，supervisor usage 为 0，且**不**把该轮折叠进 supervisor `Conversation`（保持 sans-io
  封装），delegation 完全经由 `RunOutput`+trace/events 报告；external delegate 的可续会话事实保留供 snapshot。
  `AgentBuilder::build` 增加 build 期校验：规则引用未注册 delegate → `FacadeError::Config`。
- `src/facade/agent/stream.rs`：`start()` 增加 rules 分支 `start_rules_routed`，future 驱动 delegate 后把
  `DelegationStarted`/`DelegationArtifact`/`DelegationFinished|Failed` 事件回放进 sink，末尾产 `Done`。
- 测试：`src/facade/delegate.rs` 新增 4 个单元测试（route_task 优先级/大小写/未命中、Rules 模式零工具声明、
  链式切换、未知 delegate 检测）+ 6 个离线 drive/stream 测试（本地 subagent、external delegate、未命中回落
  supervisor、首条规则胜出、build 拒绝未知 delegate、stream 事件序列）。
- 验证：`cargo fmt --all` ✓；`cargo clippy --all-targets -- -D warnings` ✓（含
  `--features external-claude-code external-codex external-opencode` ✓）；`cargo test -p agent-lib facade::delegate`
  35 passed ✓；`cargo test --all --all-targets` 全绿（50 个 test-result 组无失败）✓；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✓；`git diff --check` 无问题 ✓。


### [DONE] M5-2 dispatcher-routed delegation（primary → verify → escalate）

**上下文**：

- `docs/facade-api.md` §13.3、§17.4：`Delegation::dispatcher().primary("cheap-coder")
  .verify_with("verifier").escalate_to("strong-coder").max_attempts(2)`。语义：primary 先试 → verifier 检查
  产物 → 不过则升级 strong；最终结果与升级路径进 `DelegationTrace`，产 `RunEvent::Escalated`。
- 锚点：`agent::external::{Dispatcher, Escalator}`（现有；确认 API），facade 把 dispatcher 配置映射到它们。
  dispatcher-routed 不做第一版默认（§13.3）。

**做什么**：

- 扩展 `Delegation`：`dispatcher()` builder（primary/verify_with/escalate_to/max_attempts）→ 映射到
  `agent::external::Dispatcher`/`Escalator` 调度；实现 primary→verifier→escalate 闭环，把尝试与升级记入
  `DelegationTrace`，产 `RunEvent::Escalated`。
- rustdoc + 离线 doctest。

**验证条件**：

- 单元测试（离线：脚本化 primary 失败/verifier 判不过/strong 成功）：升级闭环按 `max_attempts` 执行；
  `DelegationTrace` 含升级路径（primary→strong）；`RunEvent::Escalated` 产出；verifier 通过时不升级。
- 聚焦：`cargo test -p agent-lib facade::delegate`（含 dispatcher 用例）。
- 完整验证序列 1–6（若涉及 external features 则附带其 clippy）。

**完成记录**：

- `src/facade/delegate.rs`：新增 `DispatcherConfig`（primary/verifier/escalate_to/max_attempts，`max_attempts`
  下限钳到 1）与 `DelegationMode::Dispatcher`；`Delegation` 新增 `dispatcher()`、`primary()`/`verify_with()`/
  `escalate_to()`/`max_attempts()`（链式，非 dispatcher 模式调用会切换为 dispatcher）、`is_dispatcher_routed()`、
  `dispatcher_config()`、`first_unknown_dispatcher_delegate()`（build 期校验）。`declarations()`/`route()`/
  `external_tool_names()` 对 Dispatcher 模式同 Rules 返回空——不向模型暴露任何 delegate。verifier 判定协议：
  回复含大小写不敏感 token `ESCALATE`（常量 `DISPATCHER_ESCALATE_MARKER`）或其自身 delegation 失败＝判不过，
  否则通过（§13.3 未指定线协议，facade 约定并写入 rustdoc）。
- `src/facade/agent.rs`：`run_full` 增 dispatcher 分支（整轮走 `run_dispatcher_routed`）；把 rules 与 dispatcher
  的单次 delegate 驱动抽出共享 `run_one_delegation()`。新增 `resolve_dispatcher_targets()`/`run_dispatcher_routed()`
  与自由函数 `drive_dispatcher_routed()`（cheap→verify→strong 闭环）、`run_verifier()`、`build_dispatcher_roster()`、
  `dispatcher_escalation_target()`（升级**决策**委托给 `agent::external::Escalator::assess`：primary 注册
  `CostTier::Cheap`+`EscalationRules{ReviewRejected/…→strong}`、strong 注册 `CostTier::Premium` 终态；
  `Escalator::with_budget_headroom(0)` 关闭预算降级，纯上行升级；current==strong 返回 `Exhausted`→停）。每次
  worker/verifier 走既有 `DelegationToolHandler::fulfill_rules_routed`，recorder/usage/artifacts/§9.2 审批门与
  model-routed 完全一致；不经 supervisor LLM（supervisor usage=0，不折叠进 `Conversation`），最终回复＝最后一次
  worker 摘要（非 verifier）。`AgentBuilder::build` 增 build 期校验：空 primary 或未注册 delegate → `FacadeError::Config`。
- `src/facade/agent/stream.rs`：`start()` 增 dispatcher 分支 `start_dispatcher_routed`，future 跑完闭环后把有序
  事件（含 `RunEvent::Escalated`）回放进 sink，末尾产 `Done`。
- **类级 bug 修复**（实现中发现的直接阻塞项）：`FacadeSubagentSpawner::child_ids`（`src/facade/delegate.rs`）与
  `FacadeExternalSpawner::child_ids`（`src/facade/external.rs`）原用固定 `subagent:{name}` / `external:{name}`
  作 trace node id，同一 delegate 在一轮内被驱动两次（dispatcher 每次尝试重跑 verifier）即触发
  `duplicate trace node id`。改为折入每次驱动新铸的 `run_id` 保证唯一，修掉整类（子代理与外部代理）重复驱动碰撞。
- 测试：`src/facade/delegate.rs` 新增 10 个离线用例：builder 配置/零工具声明、`max_attempts` 钳位、模式切换、
  未知 delegate 检测、空 primary 与未知 delegate 的 build 拒绝、primary 失败→升级 strong、verifier 判不过→升级、
  verifier 通过→不升级、`max_attempts(1)` 不升级、stream 产 `Escalated`→`Done`。
- 验证：`cargo fmt --all` ✓；`cargo clippy --all-targets -- -D warnings` ✓（含
  `--features external-claude-code external-codex external-opencode` ✓）；`cargo test -p agent-lib facade::delegate`
  （含 10 个 dispatcher 用例）全绿 ✓；`cargo test --all --all-targets` 全绿（50 组 test result: ok，0 failed）✓；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✓；`git diff --check` 干净 ✓。

### [DONE] M5-R Review：Dispatcher / Escalator 正确性与文档一致性检查

**上下文**：M5-1..M5-2 落地 rules/dispatcher 路由。仅审查+收敛。

**做什么**：

- 核对与 `docs/facade-api.md` §13.2–§13.3 一致：rules-routed 模型可无感；dispatcher-routed 映射到现有
  `Dispatcher`/`Escalator`（未另造调度，§19）；升级路径与 `DelegationTrace`/`RunEvent::Escalated` 完整；
  dispatcher 非默认。
- 修正小范围偏离；需新功能按规则插前置任务。

**验证条件**：

- 完整验证序列 1–6 全绿。
- 对照表：M5 已实现 vs §13 承诺项，缺口记为后续任务。

**完成记录**（2026-07-18）：

审查 + 收敛任务，逐条核对 M5-1（rules-routed）与 M5-2（dispatcher-routed）与
`docs/facade-api.md` §13.2–§13.3/§6.3/§18.5/§19。**未发现需修正的规范偏离，无源码改动**——
prelude 与 §3 清单已一致（§3 列 `Delegation`/`RunEvent`/`RunOutput`/`ManagedExternalAgent`，
均已在 `src/prelude.rs`；`RoutingRule`/`DispatcherConfig`/`EscalationTrace` 不在 §3 prelude
清单内，经 `pub mod delegate` / `facade::EscalationTrace` 可达，无需增补）。

- **§13.2 Rules-routed（模型可无感）** — ✅ `DelegationMode::Rules` 使
  `declarations()`/`route()`/`external_tool_names()`（`src/facade/delegate.rs:942/972/1017`）**全部返回空**，
  即不向 supervisor LLM 暴露任何 delegate 工具；`Agent::run_full`（`agent.rs:259`）与 stream `start`
  （`agent/stream.rs:96`）在进 supervisor 前用 `route_task()` 拦截并直驱 delegate。`route_task`
  （`delegate.rs:769`）大小写不敏感子串、首条命中规则胜出（注册顺序＝优先级），未命中回落普通 supervisor
  drive（同样无 delegate 工具）。build 期 `first_unknown_rule_delegate` 拒未注册 delegate（`FacadeError::Config`）。
- **§13.3 Dispatcher-routed（映射现有 `Dispatcher`/`Escalator`，未另造调度，§19）** — ✅
  `DelegationMode::Dispatcher` 同 Rules 对模型零暴露；整轮走 `run_dispatcher_routed`→`drive_dispatcher_routed`
  （`agent.rs:569/1564`）的 primary→verify→escalate 闭环，受 `max_attempts` 钳制。升级**决策**委托给真实
  `agent::external::Escalator::assess`（`agent.rs:1747/1768`），roster 用真实
  `WorkerRoster`/`WorkerProfile`/`CostTier`/`EscalationRules`/`EscalationTrigger`/`WorkerReport`
  （primary=`Cheap`+规则→strong；strong=`Premium` 终态；`with_budget_headroom(0)` 纯上行）——**未新造调度
  runtime**，符合 §19。每次 worker/verifier 复用 `DelegationToolHandler::fulfill_rules_routed`，
  recorder/usage/artifacts/§9.2 审批门与 model-routed 完全一致；不经 supervisor LLM（usage=0、不折叠进
  `Conversation`）。
- **升级路径 + `DelegationTrace`/`RunEvent::Escalated` 完整** — ✅ 每次尝试的 worker/verifier 进
  `DelegationTrace`（`RunOutput.delegations` + `Delegation{Started,Artifact,Finished,Failed}` 事件，
  经 `DispatcherAccumulator::record`，`agent.rs:1528`），每次升级产
  `RunEvent::Escalated(EscalationTrace{from,to})`（`agent.rs:1611`；§6.3 line 325 定义的专用变体）。
  verifier 判定协议（§13.3 未定线协议，facade 约定并写 rustdoc）：回复含大小写不敏感 `ESCALATE`
  或自身 delegation 失败＝判不过。run_full 与 stream 两路事件序列一致（`agent/stream.rs:272`
  `start_dispatcher_routed` 回放同序事件 + 末尾 `Done`）。
- **Dispatcher 非默认** — ✅ `Delegation::default()`＝`model_routed()`（`delegate.rs:650`）；
  dispatcher 仅经显式 `Delegation::dispatcher()` 进入，rustdoc 明标「advanced, opt-in — never a default」。

**对照表 — §13（及相邻 §6.3/§19）承诺项 vs M5 实现**：

| §    | 承诺项 | 状态 | 归属 |
|------|--------|------|------|
| 13.1 | model-routed（每 delegate 一个 `ask_<name>` 工具，默认档） | ✅ 已实现 | M3-2 |
| 13.2 | rules-routed：`rules().when_task_contains(kw, delegate)` 按规则路由 | ✅ 已实现 | M5-1 |
| 13.2 | rules-routed 模型可无感（不向 LLM 暴露 delegate 工具） | ✅ 已实现 | M5-1 |
| 13.2 | 未命中规则的明确行为（回落普通 supervisor drive） | ✅ 已实现 | M5-1 |
| 13.2 | build 期拒未注册 delegate | ✅ 已实现 | M5-1 |
| 13.3 | dispatcher-routed：`dispatcher().primary().verify_with().escalate_to().max_attempts()` | ✅ 已实现 | M5-2 |
| 13.3 | 映射到现有 `agent::external::Escalator`（升级决策），未另造调度（§19） | ✅ 已实现 | M5-2 |
| 13.3 | primary→verifier→escalate 闭环，受 `max_attempts` 钳制 | ✅ 已实现 | M5-2 |
| 13.3 | 升级路径产 `RunEvent::Escalated(EscalationTrace)`（§6.3） | ✅ 已实现 | M5-2 |
| 13.3 | 每次尝试进 `DelegationTrace`（`RunOutput.delegations` + 事件） | ✅ 已实现 | M5-2 |
| 13.3 | dispatcher-routed 非第一版默认（默认＝model-routed） | ✅ 已实现 | M5-2 |
| 13.3 | 映射到 `agent::external::Dispatcher`（初始预算感知路由器） | ➖ 无需（primary 为显式固定 worker，无歧义路由可 dispatch；仅 `Escalator` 已足够，§19 复用地基不假装） | M5-2 |
| 12   | 统一 `Delegate`/`DelegateSpec` 抽象 | ⏳ 首版可不公开（随 external 增量） | 后续 |
| 14   | 按 delegate 拓扑自动启用 collab（mailbox/blackboard/plan/artifact） | ⏳ 未实现（已排期） | M6-1 |
| 14   | external runtime collab 能力桥接本库 primitives | ⏳ 未实现（已排期） | M6-2 |

M5（rules-routed + dispatcher-routed）承诺项**全部实现**。唯一 ➖ 项（映射 `Dispatcher` 初始路由器）
属**设计选择而非缺口**：dispatcher-routed 的 primary 是显式命名的固定 worker，不存在需要 `Dispatcher`
的「模糊中段 + 预算降级」初始路由；升级决策由真实 `Escalator::assess` 承担，忠实复用地基、未另造 runtime
（§19），M5-2 完成记录已载此取向。其余 ⏳ 缺口（§12 统一抽象、§14 collab）由 §13/§14 自身排期为后续
milestone（M6）承接，**无需新增前置任务**（不阻塞、非 M5 承诺范围）。**全套测试绿、无新观察到的失败测试**
（Test Failure Policy 满足）。

**验证**（序列 1–6 全绿 + external features clippy 全绿；本任务仅文档改动，但为 review 验收门仍跑全序列）：

- `cargo fmt --all -- --check` ✓；
- `cargo clippy --all-targets -- -D warnings` ✓ +
  `--features "external-claude-code external-codex external-opencode external-acp"` ✓；
- 聚焦 `cargo test -p agent-lib facade::delegate` ✓（46 passed，含 M5-1/M5-2 的 rules/dispatcher 用例）；
- `cargo test --all --all-targets` ✓（全部 test binary 0 failed）；
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✓；`git diff --check` ✓。

---

## Milestone 6 — Collaboration convenience

目标：`docs/facade-api.md` §14、§18.6。按 delegate 拓扑自动启用 mailbox/blackboard/plan/artifact store，
提供 `Collaboration` 显式配置，并把 external runtime 的 collab 能力桥接到本库 `agent::collab` primitives。

### [DONE] M6-1 `Collaboration` 配置 + 按拓扑自动启用协作原语

**上下文**：

- `docs/facade-api.md` §14 默认表：无 delegate→不启用；单 delegate model-routed→mailbox 默认关；多 delegate→
  自动 mailbox；dispatcher/verifier→自动 plan+blackboard+mailbox；managed external agent→自动 artifact store。
  显式配置 `Collaboration::new().plan().blackboard().mailbox().artifacts()`。
- 锚点：`agent::collab`（plan/blackboard/mailbox primitives；确认公开 API）。仅承诺底层已落地的 collab 能力
  （`PLAN.md` R8：文档里尚无底层支撑的自动拓扑先不公开）。

**做什么**：

- 建 `src/facade/collab.rs`：`Collaboration` 配置 + `AgentBuilder::collaboration(..)`；`build()` 时按 delegate
  拓扑推导默认启用集合（§14 表），显式配置可覆盖。把启用的原语接入 supervisor 的 scope/state。
- 对底层尚不支持的自动档，明确不启用并在 rustdoc 标注（不假装支持）。
- rustdoc 完整。

**验证条件**：

- 单元测试（离线）：不同 delegate 拓扑推导出正确的默认协作集合；显式 `Collaboration` 覆盖生效；启用 mailbox
  时多 delegate 能收发消息（若底层支持）；未支持档不被静默启用。
- 聚焦：`cargo test -p agent-lib facade::collab`。
- 完整验证序列 1–6。

**完成记录（M6-1）**：

- 新增 `src/facade/collab.rs`：数据型 `Collaboration`（`new()/.plan()/.blackboard()/.mailbox()/.artifacts()`
  + `*_enabled()`/`any()`，`Copy`+serde），`pub(crate) derive_default(delegation, local, external)` 严格实现
  §14 表（无 delegate→空；dispatcher→plan+blackboard+mailbox；否则 ≥2 delegate→mailbox；含 external→artifacts
  叠加），`resolve()`（显式全量覆盖，否则拓扑推导），`pub(crate) CollabState{config,mailbox,blackboard,plan}`
  + `provision(config,&ids)` 仅实例化启用的 live 共享原语。
- `FacadeIds` 增 `blackboard_id()`/`plan_id()`；`facade::mod` 导出 `Collaboration`（**不**入 prelude，§3 列表未列，
  保持 M6-R prelude 一致）。
- `AgentBuilder` 增 `collaboration(Collaboration)`；`build()` 按 delegate 拓扑 `resolve`+`provision`，把启用原语接入
  `Agent` 状态；`Agent` 增 `collaboration()/mailbox()/blackboard()/plan()` 只读访问器（`Option<Arc<..>>` 共享句柄）；
  `restore()` 从恢复后的拓扑重新推导并重建原语（snapshot 不持久化显式 `Collaboration`，符合 §15.2）。
- R8 诚实边界：§14 四档均映射到已落地原语（Mailbox/Blackboard/Plan/`ArtifactRef`），无「假装支持」被静默启用；
  M6-1 仅**供给**共享底座并对外暴露/可被使用，**不**向 supervisor LLM 广告 collab 工具、**不**自动路由协作——
  §14 指名的填充机制是 external runtime collab 事件桥接，属 M6-2。AgentSnapshot 的 collab 保留字段其内容序列化
  一并留待 M6-2（M6-1 仅 live 状态供给），基线 base-path snapshot 测试保持不变（base 不供给任何原语）。
- 测试：`facade::collab`（8）覆盖拓扑表 5 档、显式覆盖、`provision` 仅建启用项、共享 mailbox 收发；`facade::agent`
  新增 4 例端到端校验 builder→agent 接线与访问器（base 全空、双 subagent→mailbox 且经 `agent.mailbox()` 收发、
  dispatcher→plan+blackboard+mailbox、显式覆盖抑制推导 mailbox）。
- 验证：`cargo fmt` ✓；`cargo clippy --all-targets -D warnings` ✓；`clippy --features
  "external-claude-code external-codex external-opencode external-acp" -D warnings` ✓；
  `cargo test -p agent-lib facade::collab` 12 ✓；`cargo test --all --all-targets` 全绿（exit 0）；
  `cargo test --doc -p agent-lib`（含 3 个 collab doctest）✓；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
  --workspace` ✓；`git diff --check` ✓。PLAN.md 无阶段级变化，未改。

### [TODO] M6-2 桥接 external runtime collab 能力到本库 primitives

**上下文**：

- `docs/facade-api.md` §14 末段：external runtime 的 `spawn_agent`/`send_message`/`plan_update`/
  `blackboard_post` 等能力应桥接到本库 collab primitives，**不**直接依赖某 runtime 私有协议。
- 锚点：external 侧 spawn/subagent bridge（归档 M3-3「external runtime spawn_agent tool bridge 特判」，见
  `docs/archive/2026-07-17-managed-external-agent/TODO.md`）+ `agent::collab`。

**做什么**：

- 把 external observations 中的 collab 事件（spawn_agent/send_message/plan_update/blackboard_post）归一化并
  桥接到 facade 启用的 `agent::collab` primitives（mailbox/plan/blackboard），保持 provider-neutral。
- rustdoc + 离线 doctest（scripted external 发 collab 事件）。

**验证条件**：

- 单元测试（离线 scripted external 产 collab 事件）：external `send_message` 进入 mailbox、`plan_update`
  更新 plan、`blackboard_post` 写 blackboard；不引入 runtime 私有类型到 facade 公开 API。
- 聚焦：`cargo test -p agent-lib facade::collab`（含 external bridge 用例，`--features` 视需要）。
- 完整验证序列 1–6（+ external features clippy 若涉及）。

### [TODO] M6-R Review：Collaboration convenience 与 facade 整体验收

**上下文**：M6-1..M6-2 落地协作便利层，也是 facade 六个 milestone 的收官 review。

**做什么**：

- 核对与 `docs/facade-api.md` §14 一致：按拓扑自动启用正确；external collab 桥接到本库 primitives（无私有协议
  泄漏，§14/§19）；只承诺底层已落地能力（R8）。
- **整体验收**：对照 `docs/facade-api.md` §2/§18/§19，逐条核 facade 是否满足：渐进式使用、保留强不变量
  （内部用 `Conversation`+`AgentMachine`+`Requirement`）、默认可用、可恢复（snapshot 无 secret）、可观测
  （`RunOutput` 全维度）、逃生舱清楚。核 `prelude` 与 §3 列表一致。核 README/文档是否需补 facade 入门示例。
- 汇总所有 milestone 遗留缺口（若有）为后续任务；确认无未调度的失败测试（Test Failure Policy）。

**验证条件**：

- 完整验证序列 1–6 全绿，+ 全 external features clippy 全绿。
- 整体对照表：facade 已实现 vs `docs/facade-api.md` §2–§17 承诺项；未覆盖项显式记为后续任务或已确认的非目标。
- 若全部 facade milestone 完成且验收通过，可按调用方约定收尾（如打 `endtag`）——由后续调用决定。
