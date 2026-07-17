# M1-1 建 facade 模块骨架 + 内建 id source + ProviderConfig + ModelConfig + FacadeError

**当前任务 = TODO.md 首个未完成 = M1-1**（`### [TODO] M1-1`）。
上一工作线（Managed External Agent，M1..M10 + H-1）已全部 `[DONE]` 并归档。
本轮开始新的 Facade API 落地任务单（`docs/facade-api.md` 为唯一设计输入）。

## 目标（TODO.md M1-1「做什么」）

1. `src/lib.rs` 加 `pub mod facade;` + `pub mod prelude;`。
2. 建 `src/facade/{mod.rs, config.rs, ids.rs, error.rs}` + `src/prelude.rs`。
3. `ProviderConfig`（config.rs）：包 `EndpointConfig` + `ProviderId`；构造器
   `anthropic_from_env()` / `openai_from_env()` / `anthropic()` / `openai()` builder / `custom(..)`；
   凭据不 debug/log/persist（手写脱敏 Debug）。
4. `ModelConfig`（config.rs）：`new(model).max_tokens(u32).temperature(f32)`；`to_model_ref()`→`ModelRef`；
   `apply_to_request(&mut ChatRequest)` helper。
5. `FacadeError`（error.rs）：M1 变体 `Config(String)`、`Client(ClientError)`、`Conversation(ConversationError)`、
   `UnexpectedToolUse`、`InvalidState(String)`；`#[non_exhaustive]`；impl Error+Display 保留 source；
   rustdoc 注明后续 milestone 增补。
6. `FacadeIds`（ids.rs）：内建单调计数器 → `uuid::Uuid::from_u128`（从 1 起），实现 `RequirementIds`+
   `ToolExecutionIds`，并生成 `ConversationId/TurnId/MessageId/ToolCallId/StepId/AgentId/RunId/ToolSetId/TraceNodeId`。
7. prelude 先只重导已存在类型：`ProviderConfig, ModelConfig`（Chat/ChatSession/Reply/RunOutput/RunEvent 后续补）。
8. 全部公开项带 rustdoc（lib.rs 已开 `#![warn(missing_docs)]`）。

## 已核实代码锚点

- `client::{EndpointConfig, AuthScheme}`（src/client/config.rs）。`EndpointConfig{base_url, auth, query_params, extra_headers}`。
- `client::ChatRequest`（src/client/request.rs）字段 model/messages/tools/system/max_tokens/temperature/stream/provider_extras。
- `client::ClientError`（src/client/error.rs）。
- `model::extras::{ProviderId, ProviderExtras}`（src/model/extras.rs）；`ProviderId::{Anthropic, OpenAiResp}`（`#[non_exhaustive]`）。
- `agent::ModelRef::new(model, NonZeroU32 max_tokens, Option<f32>, Option<ProviderExtras>)`（src/agent/spec.rs）。
- `conversation::ConversationError`（src/conversation/error.rs）。
- id 构造器 `X::new(uuid::Uuid)`；`TraceNodeId::new(impl Into<String>)`。
- id trait：`RequirementIds::next_requirement_id(&self, RequirementKindTag)->Result<RequirementId,RequirementError>`；
  `ToolExecutionIds::{tool_call_id, tool_result_message_id, next_assistant_message_id, next_step_id}`。
- 现有样板 `examples/agent_chat.rs` 的 `DemoIds`（本轮抽成库内 `FacadeIds`）。
- env 约定（对齐 examples/support）：
  - Anthropic：ANTHROPIC_BASE_URL(必) ANTHROPIC_AUTH_TOKEN(必, Bearer) ANTHROPIC_VERSION(可选 def 2023-06-01)→ header anthropic-version。
  - OpenAI：OPENAI_BASE_URL(必) OPENAI_API_KEY(必, Header api-key) OPENAI_API_VERSION(可选 def 2025-04-01-preview)→ query api-version。
- `uuid`、`thiserror` 均为正式依赖（非 dev-only）。

## 验证（TODO.md M1-1）

- 单元测试：ProviderConfig::custom/builder 生成正确 EndpointConfig+ProviderId；env 缺变量→FacadeError::Config
  （用临时 env，不落真凭据）；ModelConfig::to_model_ref 与 ChatRequest 字段映射正确；ProviderConfig/凭据 Debug 无明文 key。
- 聚焦：`cargo test -p agent-lib facade::config`。
- 序列 1(cargo fmt --all -- --check)、3(cargo clippy --all-targets -- -D warnings)、
  5(RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace)、6(git diff --check)；步骤 2 用聚焦名；步骤 4 视改动运行。

## 执行步骤

1. [x] 读 TODO M1-1、锚点类型、facade-api.md §3/§4/§16。
2. [x] 建 4 个 facade 文件 + prelude.rs，改 lib.rs。
3. [x] 写单元测试（config：builder/custom/env-missing/debug 脱敏/model-ref/chatrequest 映射）。
4. [x] fmt → clippy → 聚焦测试 → doc → git diff --check。
5. [x] TODO.md 把 M1-1 标 [DONE] + 完成记录。
6. [~] commit（进行中） [M1-1] ...，停。
