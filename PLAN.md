# 实施计划：Facade API（batteries-included 装配层）

> 本计划以 [`docs/facade-api.md`](docs/facade-api.md) 为唯一设计输入，并引用
> [`docs/conversation-core.md`](docs/conversation-core.md)（Conversation 不变量与 snapshot/restore）、
> [`docs/agent-layer.md`](docs/agent-layer.md)（Agent sans-io 分层：machine / handler / driver）、
> [`docs/agent-effect-model.md`](docs/agent-effect-model.md)（Requirement / HandlerScope / Pop）、
> [`docs/external-agent.md`](docs/external-agent.md) 与
> [`docs/managed-external-agent.md`](docs/managed-external-agent.md)（受管 external agent）。
>
> 上一轮根目录计划「Managed External Agent」（Milestone 1–10：核心协议扩展、三家 CLI adapter、
> ACP adapter、worktree/budget/reconfig/docs 收尾）已完成并归档到
> [`docs/archive/2026-07-17-managed-external-agent/`](docs/archive/2026-07-17-managed-external-agent/)。
> 更早的 Client / Conversation / Agent Layer / Effect Migration / Testability / Complex-tests /
> Effect-refine / External-agent 记录在 `docs/archive/2026-07-*` 下。逐任务实现清单见
> [`TODO.md`](TODO.md)。
>
> 标注约定：`docs/facade-api.md` 中的类型/方法名是**建议的** facade 形状，未必已在代码中存在；本计划把它们
> 落成实际 API，命名以文档为准、以现有底层类型为锚。

## 目标

在已有 Client / Conversation / Agent 三层之上，新增一层 batteries-included 的 **facade** API
（`agent_lib::facade` + `agent_lib::prelude`），让常见的聊天、工具 agent、subagent、managed external
agent 场景**不必手写** `Conversation` pending 事务、`AgentMachine`、`RequirementIds`/`ToolExecutionIds`、
`HandlerScope` 与 driver wiring（见 `docs/facade-api.md` §0–§2）。具体：

- **渐进式使用**：从 one-shot `Chat::ask`，到 stateful `ChatSession::send`，再到 `Agent` 工具调用、
  local subagent、managed external agent，逐层增加概念，不要求用户一开始理解完整 effect 模型。
- **保留强不变量**：facade 内部仍用 `Conversation` 推进 turn、`DefaultAgentMachine` reify effect、
  `HandlerScope`/`drain`/`Pop` 兑现 requirement；**不**绕过底层重写一套轻量状态机（§2.1、§19）。
- **默认可用**：默认生成稳定 identity（内建 `RequirementIds`/`ToolExecutionIds` id source）、默认创建
  session、默认处理 pending 失败、默认接好 tool registry、默认给 headless/attended 审批一个明确行为。
- **可恢复**：`ChatSession` / `Agent` / delegating session 支持 snapshot/restore，snapshot 只存 data-only
  facts，不存凭据、闭包、进程句柄、client handle（§15、§19）。
- **可观测 + 逃生舱清楚**：简单路径只拿 `Reply.text()`；产品/调试路径拿完整 `RunOutput`（response、
  tool trace、delegation trace、artifact、usage、raw events），并能退回底层模块（§6、§19）。

## 非目标

（严格照搬 `docs/facade-api.md` §2.2、§19）

- 不隐藏 provider 能力差异：provider-specific extras 仍显式绑定 `ProviderId`（`model::extras`）。
- 不把所有能力塞进一个 `EasyClient`；Chat / stateful conversation / tool agent / delegating agent 语义
  不同，API 上区分。
- 不让 `Reply` 只等于 `String`；主路径返回结构化 `Reply` / `RunOutput`，`ask_text` 只是便捷。
- 不把 API key、base URL token、运行期闭包、live process handle 写进 snapshot。
- 不让 managed external agent 伪装成普通函数工具；它有 session/artifact/worktree/权限/cancel/attach 语义，
  一等建模为 external delegate。
- **不新增 effect family**：subagent/external 的 tool/interaction/subagent 决策点仍映射到现有
  `NeedTool`/`NeedInteraction`/`NeedSubagent`，external 经 `NeedExternalSession` 回灌 runtime。

## 现有代码锚点（facade 的精确接入面）

- **模块**：`src/lib.rs` 现有 `pub mod adapter/agent/client/conversation/model/stream`；新增
  `pub mod facade;` 与 `pub mod prelude;`。facade 子模块建议：`config`、`chat`、`run`（Reply/RunOutput/
  RunEvent/RunStream）、`tool`、`agent`、`delegate`、`error`、`ids`（内建 id source）。
- **client**：`client::{EndpointConfig, AuthScheme, ChatRequest, Response, LlmClient}`；
  `model::extras::{ProviderId, ProviderExtras}`。`ProviderConfig` 是 `EndpointConfig` + `ProviderId`
  的易用包装。
- **conversation**：`Conversation::{new, begin_turn, start_assistant_response, finish_assistant,
  commit_pending, effective_view, snapshot}`、`ConversationSnapshot`、`ConversationConfig`、ids
  （`ConversationId/TurnId/MessageId/ToolCallId`）、`AssistantFinish`。
- **agent**：`AgentSpec/AgentState/DefaultAgentMachine/LlmStepMode`、`RequirementIds`+`ToolExecutionIds`
  （id source）、`ModelRef/LoopPolicy/ToolFailurePolicy/ToolSetRef/WorktreeRef`、
  `RunContext::new_root`+`BudgetLimits`、`drive::{drain, HandlerScope, ReferenceScope}`
  （`ReferenceScope::new(client, registry).with_interaction(..)`）、
  `ToolRegistry/ToolExecutor/ToolApprovalPolicy/ApprovalRequirement/Interaction/InteractionHandler/
  InteractionKind`、`NestedMachine/SubagentHandler`、`collab`（plan/blackboard/mailbox）。
- **external**：`agent::external::{ExternalRuntimeKind, ExternalRuntimeCapabilities,
  ExternalAgentMachine, ExternalSessionHandler, ExternalSessionRegistry, runtime adapters}`；ACP 侧
  `AcpAdapter`（feature `external-acp`）。这些是 `ManagedExternalAgent` / `ExternalRunMode` 的地基。
- **驱动样板**：`examples/agent_chat.rs` 展示 id source（`RequirementIds`+`ToolExecutionIds`）、
  `AgentSpec`→`AgentState`→`DefaultAgentMachine`、自建 `HandlerScope`、`drain`、`RunContext` 的完整
  wiring——facade 要把这段样板收进内部。`examples/tool_round_trip.rs` / `examples/managed_*.rs` 同为参考。

## 里程碑

承接 `docs/facade-api.md` §18「建议落地顺序」，逐层增加概念、每层可独立验证。

| 里程碑 | 主题 | 主要产出 | 默认测试形态 |
|---|---|---|---|
| M1 | Chat facade | `facade`/`prelude` 模块、`ProviderConfig`/`ModelConfig`、`Reply`/`RunOutput`/`RunEvent`/`FacadeError`、`Chat`/`ChatSession`（ask/send/stream/snapshot/restore） | 单元（内建 fake `LlmClient`，离线） |
| M2 | 基础 Agent facade | typed function `Tool`、`Approval` 三档、内建 id source、`Agent`（`run`/`run_full`/`stream`/`snapshot`）、loop policy | 单元（fake client + 脚本工具，离线） |
| M3 | Local subagent | `Agent::worker()`、`.subagent(..)`、model-routed delegation（每 delegate 一个工具）、`DelegationTrace` | 单元（父子 machine 离线 drain） |
| M4 | Managed external agent | `.external_agent(..)`、`ManagedExternalAgent`（含 `::acp` 预设）、`ExternalRunMode`/`ExternalAgentCapabilities` 分级、approval defaults、artifact trace、restore policy | 单元（scripted/registry-backed external，离线） |
| M5 | Dispatcher / Escalator | rules-routed + dispatcher-routed delegation（primary→verify→escalate）、升级路径入 `DelegationTrace` | 单元（离线 delegate 拓扑） |
| M6 | Collaboration convenience | 按 delegate 拓扑自动启用 mailbox/blackboard/plan/artifact store，`Collaboration` 显式配置，桥接 external collab 能力 | 单元（离线拓扑） |
| M7 | 宿主嵌入接入面 | interaction handler 注入、`RunEvent` 可序列化投影、`ApprovalRequest` 富化、生产 `ExternalSessionHandler`、AI 决策接缝透出 | 单元（离线 fake/scripted） |

每个 milestone 末尾有一个独立 `M<n>-R` review 任务，检查正确性、完整性、文档一致性。

## 关键设计约束（照搬 §19，落地时必须守）

- Facade 是装配层，不是第二套 runtime；`ChatSession`/`Agent` 内部必须用 `Conversation`，不直接拼 message Vec。
- `Agent` 必须内部用 `AgentMachine` + effect handler，不绕过 `Requirement`。
- 简单 API 默认 cancel failed pending（回到上一个 committed 一致点）；高级 API 可保留 pending 供检查
  （`PendingFailurePolicy::{Cancel, KeepForInspection}`）。
- Snapshot 不保存 secret / 闭包 / client / live process handle。
- Local subagent 默认作为 local delegate；managed external agent 默认作为 external delegate，且更保守
  （启动/resume/写工作区需审批或显式 opt-in）。
- Model-routed delegation 默认每 delegate 一个工具（`ask_<name>`）；统一 `delegate` 工具是高级选项。
- `RunOutput` 必须能同时表达 LLM response、tool trace、delegation trace、artifact、raw events。
- 所有 provider-specific 行为继续经 provider extras / capability model 显式表达。
- **（M7）facade 必须为宿主提供依赖注入口**（interaction handler / external session handler / task evaluator / permission decider），而非把决策后端写死在 facade 内。每个注入口都有一个保持现有保守行为的默认值：不注入时行为与 M1–M6 完全一致，注入才改变；注入口只是把底层已就绪能力（`InteractionHandler`/`ExternalSessionHandler`/`TaskEvaluator`/`Verifier`/`InteractionKind::Permission` 通道）透出，不新增 effect family、不改底层语义。

## 验证策略

默认完整验证序列（cheap → expensive，任务另有放宽时以任务为准）：

1. `cargo fmt --all -- --check`
2. 聚焦测试：任务中给出精确过滤名（如 `cargo test -p agent-lib facade::chat`）
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`（完整套件，超时 ≤ 30 分钟）
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

补充约束：

- 触碰 external adapter 的任务，额外跑一遍
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`。
- facade 单元测试必须**离线**：用内建/伪造 `LlmClient` 与脚本化 handler，不依赖网络、凭据、CLI 或本地登录态；
  每个测试须在 1 分钟内完成，卡住即为 bug。
- 真实 provider / 真实 CLI 的端到端验证一律 `#[ignore]`，缺环境时干净跳过（绿），不输出 secret。
- 新增公开 API 必须带 rustdoc（`#![warn(missing_docs)]` 已开）；宏/泛型生成项的 rustdoc 需可编译。

## 风险与待确认（含 `docs/facade-api.md` §20 未定问题）

facade-api.md §20 列了 7 个未定问题；落地时按下述取向推进，遇到硬约束再在 `TODO.md` 追加前置任务：

- **R1 typed tool 与 `schemars`（§20.3，实证缺口）**：`schemars` **当前不是** `agent-lib` 的依赖
  （已核对 `Cargo.toml`）。`Tool::function` 需要 `Args -> JSON schema`。取向：优先把 schema 派生放在
  **可选 feature 或 companion 支持**，避免给核心 crate 强加 `schemars`；若 M2 证明无 feature 无法保证
  易用性，则在 `TODO.md` 追加「引入 schema 依赖」前置任务，明确 feature 边界后再实现 typed tool。
  - **决策（M2-1 已落地）**：新增 **off-by-default** feature `facade-schema = ["dep:schemars"]`。
    开启后 `facade::Tool::function(name, desc, handler)`（`Args: schemars::JsonSchema`）派生 schema（并去掉
    顶层 `$schema` 元键）；不开启时用**始终可用**的 `facade::Tool::function_with_schema(..)` 显式传
    `input_schema`。默认 `cargo build` 不链接 `schemars`；核心 crate 无强加依赖。无需追加前置任务。
- **R2 Chat::send 语义（§20.1）**：为避免 stateful/stateless 含混，采用文档倾向——只提供 `Chat::ask`
  （one-shot）+ `ChatSession::send`（多轮），不提供易混的 `Chat::send`。
- **R3 `Agent` vs `AgentSession` 命名（§20.2）**：第一版以 `Agent` 承载运行态（`run` 取 `&mut self`），
  builder 产 `Agent`；文档仍按三层讲。若后续需要区分 spec/session，再拆。
- **R4 subagent 是否继承 supervisor provider/model（§20.4）**：`Agent::worker()` 同时支持
  `.model(..)`（显式）与 `.inherit_model()`（继承），默认继承，保持易用。
- **R5 DelegationTrace task brief redaction（§20.5）**：`DelegationTrace` 若进 snapshot/日志，需 redact
  策略；M3/M4 落地时 task brief 默认不写入持久 snapshot，敏感字段提供 redact 钩子。
- **R6 External restore 默认 `MarkInterrupted`（§20.6）**：coding agent 默认 `MarkInterrupted`（安全）；
  只读 external agent 允许显式 `AttachOrFail`。
- **R7 `RunEvent` 可序列化（§20.7）**：`RunEvent` 的归一化变体尽量可序列化；`RawStream`/`RawNotification`
  逃生舱可能含不易序列化 source，标注为「非序列化承诺」变体，序列化能力不作为稳定契约。
- **R8 底层能力对齐**：facade 只承诺 `docs/facade-api.md` 有依据、且底层已落地的能力；文档里尚无底层支撑的
  形状（如 `DelegateBackend` trait、部分 collab 自动拓扑）先不公开，作为后续 milestone 或待确认项，不假装支持。
- **R9 external 能力档真实性**：`ExternalRunMode`/`ExternalAgentCapabilities` 必须如实反映
  `ExternalRuntimeCapabilities`（8 项）与 ACP `initialize` 协商结果，未验证的档位不假装可用（承接 M10）。

## Milestone 7 — 宿主嵌入接入面（host embedding surface）

> 本 milestone 承接 M1–M6：facade 六个 milestone 完成后，第一个真实宿主（`mag`——一个基于 agent-lib、
> Tauri GUI + web 的编码 agent app）在接入时暴露出一组 facade 层的**依赖注入缺口**。底层 effect 模型
> （`InteractionHandler`/`ExternalSessionHandler`/`TaskEvaluator`/`Verifier`/`InteractionKind::Permission`）
> 已完全就绪，但 facade 把决策后端**写死**了：审批 handler 硬编码为同步 `FacadeApproval`，`RunEvent` 整体
> 不可序列化，`ApprovalRequest` 只有 tool name，无生产级 external session handler，AI 路由/审批接缝未透出。
> 结果是任何"把审批请求跨进程发给前端、await 用户点按钮再折回"的宿主都被迫**绕过 facade、下沉到 agent 层自组
> `HandlerScope`+`drain` 重搭 driver**。这违背 facade「batteries-included 装配层」的初衷。

**动机**：让真实宿主能留在 facade 里嵌入 agent-lib，而不是被迫下沉重写 driver。

**目标**：把底层已就绪的能力通过 facade 的**依赖注入口**透出。严格遵守本轮既有约束——**不新增 effect family**，
不改底层状态机语义，每个注入口都有保持现有保守行为的默认值（不注入时与 M1–M6 完全一致）。

**子目标（对应 TODO M7-1..M7-5）**：

1. **interaction handler 注入**（核心，M7-1）：`AgentBuilder::interaction_handler(Arc<dyn InteractionHandler>)`。
   替换 `FacadeAgentScope.interaction` / `TapInteractionHandler.inner` 的硬编码 `FacadeApproval`；底层
   `InteractionHandler::fulfill` 本就是 async 暂停点，注入后宿主可在 `fulfill` 内 await 一个跨进程 channel
   实现"发前端 → 等回答 → 折回"。同步路径与流式路径都接上；与 `.approval(..)` 的优先级写清。
2. **`RunEvent` 可序列化投影**（M7-2）：新增 `WireRunEvent`（或 `to_wire()`），可序列化变体如实转发，
   `RawStream`/`RawNotification` 降级为明确 opaque 标记。保持 R7「`RunEvent` 本身不 serde」的既有决定不变。
3. **富化 `ApprovalRequest`**（M7-3）：补 `call_id`/`reason`/工具输入摘要（`#[non_exhaustive]`，加字段兼容），
   从底层 `InteractionKind::Approval` 的 `call_id`+`ApprovalRequirement` 填充，让 UI 能渲染有意义的审批框。
4. **生产级 registry-backed `ExternalSessionHandler`**（M7-4，feature-gated）：把 `ExternalSessionRegistry`
   + live adapter（`ClaudeCodeAdapter` 等）接成官方 handler，宿主 `.session_handler(default_..)` 直接用，
   补上"live adapter → 可注入 handler"的最后一公里（当前全库只有 test double）。
5. **AI 决策接缝透出**（M7-5）：`Delegation::dispatcher()` 接受自定义 `TaskEvaluator`/`Verifier`（替换写死的
   `ScriptedVerifier`）；审批策略提供自定义 permission decider 钩子（处理 `InteractionKind::Permission`，替换
   默认 deny）。

**明确非目标**：本 milestone **不实现任何 AI 决策逻辑**（不写 LLM-backed evaluator、不写 AI 权限判定器），
只**开放注入口**并保持默认行为。AI-based routing / AI-based permission 的具体实现由宿主或后续 milestone 承担。
