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

### [TODO] M1-3 `Chat` / `ChatBuilder` + `ask` / `ask_full`（one-shot，无 tool-use）

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

### [TODO] M1-4 `ChatSession` + `send` / `send_full` + `conversation()` + snapshot/restore

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

### [TODO] M1-5 `ChatSession::stream` + `RunStream`（基于 `Accumulator`）

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

### [TODO] M1-R Review：Chat facade 正确性与文档一致性检查

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

---

## Milestone 2 — 基础 Agent facade

目标：`docs/facade-api.md` §7–§9、§8.3。typed function `Tool` + `ToolContext`、`Approval` 三档 +
`ApprovalPolicy`、`Agent`（`run`/`run_full`/`stream`/`snapshot`/`into_parts`）。内部装配
`AgentSpec`→`AgentState`→`DefaultAgentMachine`→`ReferenceScope`/`HandlerScope`→`RunContext`→`drain`
（参照 `examples/agent_chat.rs`）。用户不直接看到 `Requirement`。

### [TODO] M2-1 typed function `Tool` + `ToolContext` + 内部 `ToolRegistry` 桥接

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

### [TODO] M2-2 `Approval` 三档 + `ApprovalPolicy` → `ToolApprovalPolicy`/`InteractionHandler`

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

### [TODO] M2-3 `Agent` / `AgentBuilder` + `run` / `run_full`（装配 machine + drive）

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

### [TODO] M2-4 `Agent::stream` + `snapshot`/`restore` + `into_parts`

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

### [TODO] M2-R Review：基础 Agent facade 正确性与文档一致性检查

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

---

## Milestone 3 — Local subagent

目标：`docs/facade-api.md` §10、§13.1、§18.3。`Agent::worker()` 产 data-first `LocalSubagent` spec、
`.subagent(name, worker)`、model-routed delegation（默认每 subagent 一个工具 `ask_<name>`）、
`DelegationTrace`。完全复用 `NeedSubagent` / `SubagentHandler` / `NestedMachine`。

### [TODO] M3-1 `Agent::worker()` → `LocalSubagent` spec + `.subagent(..)` 注册

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

### [TODO] M3-2 model-routed delegation：subagent 暴露为工具 + `NeedSubagent` 兑现 + `DelegationTrace`

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

### [TODO] M3-3 `Delegation` 配置（model-routed 选项）+ 多 delegate + pending delegation snapshot

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

### [TODO] M3-R Review：Local subagent 正确性与文档一致性检查

**上下文**：M3-1..M3-3 落地 local subagent delegation。仅审查+收敛。

**做什么**：

- 核对与 `docs/facade-api.md` §10、§13.1 一致：`Agent::worker()` 产 data-first spec；child 在 `NeedSubagent`
  兑现时才建（复用 `SubagentHandler`/`NestedMachine`，未另造机制，§19）；model-routed 默认每 delegate 一工具；
  `DelegationTrace`/`RunEvent::Delegation*` 完整；snapshot/restore 覆盖 delegate 字段且不含 secret。
- `prelude` 增补 `Delegation`（若公开）。修正小范围偏离；需新功能按规则插前置任务。

**验证条件**：

- 完整验证序列 1–6 全绿。
- 对照表：M3 已实现 vs §10 承诺项，缺口记为后续任务。

---

## Milestone 4 — Managed external agent

目标：`docs/facade-api.md` §11、§15.2–§15.3、§18.4。`ManagedExternalAgent` 构造器（含 `::acp` 预设）+
`ExternalRunMode`/`ExternalAgentCapabilities` 能力分级 + `.external_agent(name, mea)` external delegate +
approval defaults（比 local 更保守）+ artifact trace + external restore policy。复用已落地的
`ExternalAgentMachine` / `ExternalSessionHandler` / runtime adapters（含 `AcpAdapter`）。

### [TODO] M4-1 `ManagedExternalAgent` 构造器 + `ExternalRunMode` + 能力分级校验

**上下文**：

- `docs/facade-api.md` §11.1、§11.3：`ManagedExternalAgent::codex().worktree(..).mode(ExternalRunMode)
  .build()`；能力档 `ExternalRunMode::{BlackBox, Managed, ManagedWithTools, Attachable}`。构建时按
  `ExternalAgentCapabilities` 校验，不支持的档 fail fast 或明确降级。
- 承接 M10（`PLAN.md` R9）：runtime→能力现状——三家 CLI adapter（Claude Code/Codex/OpenCode）
  `permission_bridge`/`host_tools=false`（Managed 无权限桥）；**ACP adapter**（feature `external-acp`）
  `permission_bridge=true`，`resume` 取决 `loadSession` 协商。facade 需提供 `::acp(binary, args)` 及便捷预设
  （如 `::claude_agent_acp()`/`::gemini_acp()`），能力档由 `initialize` 协商结果填充，不假装未验证档位。
- 锚点：`agent::external::{ExternalRuntimeKind, ExternalRuntimeCapabilities(8 项), ExternalSessionRegistry,
  runtime adapters}`；`AcpAdapter`（`external-acp`）。

**做什么**：

- 建 `src/facade/external.rs`：`ManagedExternalAgent` + builder + 预设构造器
  （`::claude_code()`/`::codex()`/`::opencode()`/`::acp(..)`/便捷 ACP 预设），`ExternalRunMode` 枚举、
  `ExternalAgentCapabilities`（facade 视图，映射自 `ExternalRuntimeCapabilities` + ACP 协商结果）。
- `build()` 按目标 runtime 的能力校验请求的 `mode`：请求超出能力的档 → `FacadeError`（fail fast），
  或按文档明确降级并记录。ACP 预设在 feature `external-acp` 下可用，未开 feature 时给清晰编译/构造错误。
- rustdoc 完整，标注能力档由协商填充、不硬编码。

**验证条件**：

- 单元测试（离线，不启真实 CLI）：各预设产正确 `ExternalRuntimeKind`/默认能力；请求不支持档位 → fail fast；
  ACP 预设的能力映射正确（`permission_bridge=true` 档可表达）。ACP 相关测试 `#[cfg(feature = "external-acp")]`。
- 聚焦：`cargo test -p agent-lib facade::external`（及 `--features external-acp` 的聚焦跑）。
- 完整验证序列 1–6，**并**跑
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`。

### [TODO] M4-2 `.external_agent(..)` external delegate 兑现 + artifact/delegation trace

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

### [TODO] M4-3 external approval defaults + restore policy + `AgentSnapshot` external 字段

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

### [TODO] M4-R Review：Managed external agent 正确性与文档一致性检查

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

---

## Milestone 5 — Dispatcher / Escalator

目标：`docs/facade-api.md` §13.2–§13.3、§18.5。rules-routed 与 dispatcher-routed delegation，对应
`agent::external::Dispatcher` / `Escalator`：cheap→verify→strong 升级闭环，升级路径进 `DelegationTrace`。

### [TODO] M5-1 rules-routed delegation

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

### [TODO] M5-2 dispatcher-routed delegation（primary → verify → escalate）

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

### [TODO] M5-R Review：Dispatcher / Escalator 正确性与文档一致性检查

**上下文**：M5-1..M5-2 落地 rules/dispatcher 路由。仅审查+收敛。

**做什么**：

- 核对与 `docs/facade-api.md` §13.2–§13.3 一致：rules-routed 模型可无感；dispatcher-routed 映射到现有
  `Dispatcher`/`Escalator`（未另造调度，§19）；升级路径与 `DelegationTrace`/`RunEvent::Escalated` 完整；
  dispatcher 非默认。
- 修正小范围偏离；需新功能按规则插前置任务。

**验证条件**：

- 完整验证序列 1–6 全绿。
- 对照表：M5 已实现 vs §13 承诺项，缺口记为后续任务。

---

## Milestone 6 — Collaboration convenience

目标：`docs/facade-api.md` §14、§18.6。按 delegate 拓扑自动启用 mailbox/blackboard/plan/artifact store，
提供 `Collaboration` 显式配置，并把 external runtime 的 collab 能力桥接到本库 `agent::collab` primitives。

### [TODO] M6-1 `Collaboration` 配置 + 按拓扑自动启用协作原语

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
