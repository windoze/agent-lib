# H-1 归档当前计划并为 facade API 生成新的 PLAN/TODO

**当前任务 = TODO.md 首个未完成 = H-1**（`### [TODO] H-1`, 交接任务）。
M1..M10 全 `[DONE]`。H-1 是 Managed External Agent 工作线的最后一个（交接）任务。

## 目标（TODO.md H-1「做什么」）

1. 归档当前根目录 `PLAN.md` + `TODO.md`（完整内容，不能只摘要）到既有归档惯例位置。
2. 阅读 `docs/facade-api.md`，为其写新的落地计划到根 `PLAN.md`（milestone / 风险 / 验证）。
3. 重写根 `TODO.md`：facade API 落地任务单，按实现顺序编号（M1-1…），每个 `[TODO]`，
   含上下文/做什么/验证条件，每个 milestone 末尾一个独立 review 任务，开头写通用执行规则。

## 关键事实（已核实）

- **归档惯例**：`docs/archive/YYYY-MM-DD-<topic>/{PLAN.md,TODO.md}`。已有 8 个归档目录
  （client-layer / conversation / agent-layer / agent-effect-migration / agent-testability /
  complex-tests / effect-refine / external-agent）。今天是 2026-07-17，已存在
  `2026-07-17-effect-refine`、`2026-07-17-external-agent`；本次用
  `docs/archive/2026-07-17-managed-external-agent/`（主题唯一）。
- **facade-api.md §18 落地顺序**（6 阶段）：1 Chat facade → 2 基础 Agent facade →
  3 Local subagent → 4 Managed external agent → 5 Dispatcher/Escalator → 6 Collaboration。
- **§20 未定问题**（7 项）→ 作为 PLAN 的「待确认风险/文档缺口」：
  1 Chat::send 是否 stateful（文档倾向只留 ask + ChatSession::send）；
  2 Agent vs AgentSession 命名；3 typed tool 是否核心依赖 schemars；
  4 subagent 是否继承 supervisor provider/model；5 DelegationTrace task brief redaction；
  6 External restore 默认 MarkInterrupted 是否过保守；7 RunEvent 是否需可序列化。
- **代码锚点**（facade 装配层要接的现有类型）：
  - 新模块：`src/facade/`（`agent_lib::facade`）+ `agent_lib::prelude`（lib.rs 现有
    `pub mod adapter/agent/client/conversation/model/stream`，加 `pub mod facade; pub mod prelude;`）。
  - client：`client::{EndpointConfig, AuthScheme, ChatRequest, Response, LlmClient}`；
    `model::extras::{ProviderId, ProviderExtras}`。
  - conversation：`Conversation::{new, begin_turn, start_assistant_response, finish_assistant,
    commit_pending, effective_view, snapshot}`、`ConversationSnapshot`、`ConversationConfig`、
    ids（`ConversationId/TurnId/MessageId/ToolCallId`）。
  - agent：`AgentSpec/AgentState/DefaultAgentMachine/LlmStepMode`、`RequirementIds`+
    `ToolExecutionIds`（id source）、`ModelRef/LoopPolicy/ToolFailurePolicy/ToolSetRef/WorktreeRef`、
    `RunContext::new_root/BudgetLimits`、`drive::{drain, HandlerScope, ReferenceScope}`
    （`ReferenceScope::new(client, registry).with_interaction(..)`）、
    `ToolRegistry/ToolExecutor/ToolApprovalPolicy/Interaction/InteractionHandler`、
    `NestedMachine/SubagentHandler`、`external::{ExternalRuntimeKind, ExternalRuntimeCapabilities,
    ExternalAgentMachine, ExternalSessionHandler, runtime adapters, AcpAdapter(feature external-acp)}`、
    `collab`（plan/blackboard/mailbox）。
  - **schemars 当前不是依赖**（Cargo.toml 已确认）→ typed function tool 的 JSON schema 派生是
    真实文档缺口/风险，PLAN 明确记为待确认（feature/ companion crate 方案）。
  - 现有驱动样板见 `examples/agent_chat.rs`（DemoIds 实现 RequirementIds+ToolExecutionIds、
    AgentSpec/AgentState/DefaultAgentMachine、HandlerScope、drain、RunContext）。

## 执行步骤

1. [x] 读 TODO H-1、PLAN.md、facade-api.md 全文、codebase 锚点。
2. [x] 先在根 TODO.md 把 H-1 标 `[DONE]` + 完成记录（让归档副本反映该工作线已收尾）。
3. [x] 建 `docs/archive/2026-07-17-managed-external-agent/`，把（已标 DONE 的）PLAN.md、TODO.md
   完整复制进去（PLAN 192 行 / TODO 3682 行，H-1 [DONE] 已含）。
4. [x] 用 facade 计划**覆盖**根 `PLAN.md`（目标/非目标/锚点/6 milestone/风险含 §20 未定问题/验证策略）。
5. [x] 用 facade 任务单**覆盖**根 `TODO.md`（通用执行规则 preamble + M1..M6，25 个任务，每 milestone
   末尾 `M<n>-R` review；每任务含上下文/做什么/验证条件）。
6. [x] 验证：归档文件存在且完整；新 PLAN 引用 facade-api.md（7 处）；新 TODO 全 `[TODO]`、每 milestone
   有 review、三段结构齐全；`git diff --check` 通过。纯文档改动 → 跳过完整测试套件（复用 M10-3 绿测）。
7. [x] commit `[H-1] ...`，停。

## 备注

- 本任务纯文档（*.md），不改编译产物 → 复用上次绿测结果，跳过 `cargo test --all`，在完成记录注明。
- 新 TODO.md 的 facade milestone 划分严格只承诺 facade-api.md 有依据的内容；缺口列为待确认风险。
