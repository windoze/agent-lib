> **归档说明(2026-07-13)**:本计划对应 push / 自驱的 `AgentLoop::feed → AgentEvent
> stream` 契约,M1–M3 已落地(`AgentSpec`/`RunContext`/`AgentState`/`LoopCursor`/
> `DefaultAgentLoop`/pivot/reconfig/approval/cancel)。设计已转向 sans-io + effect-handler
> 模型(见 [`docs/agent-effect-model.md`](../../agent-effect-model.md) 与迁移方案
> [`docs/agent-effect-migration.md`](../../agent-effect-migration.md))。当前活动计划见仓库根
> `PLAN.md` / `TODO.md`。本文与同目录 `TODO.md` 仅作历史记录,不再更新。

# 实施计划：Agent Layer

> 本计划以 [`docs/agent-layer.md`](docs/agent-layer.md) 为规范性设计输入，并承接
> [`DESIGN.md`](DESIGN.md) §1.3 的 Agent Management / Orchestration 方向。
> Conversation Core 已完成并归档于
> [`docs/archive/2026-07-13-conversation/`](docs/archive/2026-07-13-conversation/)；
> 逐任务要求与完成记录见 [`TODO.md`](TODO.md)。

## 范围与非目标

**范围**：实现单 Agent Runtime 的静态配置、运行时状态、`RunContext`、可步进
`AgentLoop`、`AgentEvent` stream、tool 执行编排、暂停/恢复、pivot、turn-boundary
reconfig、approval、cancel 贯穿，以及 API-first 的 skill/tool、plan、blackboard 和最小
multi-agent orchestration 原语。Agent 层复用已完成的 Client `LlmClient`/`StreamEvent`/
`Response` 与 Conversation 的 committed log、pending transaction、`Boundary`、
projection/compaction、fork、snapshot/restore 和 cancel 闭合能力。

**非目标**：本计划不发明通用 DAG/workflow/scheduler，不把 plan 或 blackboard 做成隐藏
executor，不重新实现 Conversation 的 I1--I4、tool pairing、Boundary 校验或持久化 restore
门，不引入 provider 特判来规避 Client/Conversation 公共模型，不支持一个 Agent 同时持有
多个活动 conversation。多路径探索通过 fork Conversation 后创建新 Agent 承载；复杂拓扑由
宿主用普通 Rust、tokio task、channel 和 join 组合。

## 规范优先级与已定关键决策

`docs/agent-layer.md` 是本阶段的权威设计输入；若它与 `DESIGN.md` 的方向性文字冲突，以
前者更细的边界为准。同一文档内部以 §4.1/§4.2 对 pivot/reconfig 的精确定义为准：
pivot 注入 `user` 消息，system prompt、tool set 与 skill 变更走 turn 边界 reconfig。

1. **Agent 由三层组成**：`AgentSpec` 是可 serde 的静态 identity/config；`AgentState`
   持有唯一活动 `Conversation`、active skills、可恢复 `LoopCursor` 与可重建 runtime 句柄；
   `AgentLoop` 是不 serde 的推进引擎。顶层 `Agent` 只是组合壳，不额外承载语义。
2. **RunContext 一等贯穿**：`RunContext` 从一开始进入 loop、tool 和子 agent 调用路径，
   承载 cancellation、budget 与 trace。子 agent 必须从父 context 派生 cancel/budget/trace，
   不能成为游离进程。
3. **Agent 只持有一个活动 Conversation**：不引入会话池或挂起会话列表。fork 后的新路径由
   新 Agent 承载，复用 Conversation 已实现的 O(1) immutable prefix sharing。
4. **feed 是一步一段的 stream 契约**：一次 `feed` 可跨多个 LLM call → tool result →
   LLM call 往返；返回 `AgentEvent` stream，stream 消费完之前不能开始下一段 feed。背压即
   节奏控制，避免另造并发锁。
5. **StepBoundary 是统一求值点**：预算、trace step、pivot 生效、compaction trigger 观察、
   loop 可暂停点均挂在 Agent 产生的 `StepBoundary(Boundary)` 事件上。Conversation 的
   `Boundary` 仍是受检 Turn boundary；Agent 层在每个 LLM/tool step 结束时重新取得合法 token
   并携带事件，不伪造或缓存 stale boundary。
6. **Conversation 新增 step-boundary user 注入入口**：当前 `Conversation::begin_turn`
   以一条 user payload 开启 pending turn。Agent pivot 需要在未来合法 step boundary 向
   pending 追加第二条或后续 `Role::User` message，仍由 Conversation 校验 role sequence 与
   open tool calls。该入口不得用于 system/reconfig。
7. **pivot 与 reconfig 两级边界分工**：pivot 是消息注入，只能注入 `user` role，来源记录在
   Agent/Conversation metadata；reconfig 包括 skill 启停、tool set 变更和 system prompt 叠加，
   只在 turn boundary 生效，保证一个 turn 内工具集恒定。
8. **Tool 发起在 Agent，配对事实在 Conversation**：Agent loop 决定 tool 是否并行、失败如何
   回灌、是否需要审批；Conversation 只负责 `ToolCallId`/provider call id/result message 的
   配对记账和最终 commit，不暴露第二条 bypass 路径。
9. **Approval 是 stream 中的可恢复挂起点**：需要人工确认的 tool 以 `AwaitingApproval`
   事件携带 responder，事件流就地等待回灌而不结束；批准、拒绝和超时都转换为完整
   `ToolResponse`，再走同一 Conversation append path。
10. **Cancel 贯穿但只闭合 pending**：`CancellationToken` 可立即打断 LLM streaming、tool
    execution 和子 agent；Conversation cancel 仍只处理 pending，按已落地的
    `CancelDisposition` 合成 cancelled tool results 或丢弃 pending，成功后必须可继续 feed。
11. **LoopCursor + Conversation 一起恢复**：暂停、审批等待和中断续跑都保存 data-only
    `LoopCursor` 与 Conversation snapshot；runtime handles、client、registry、responder、
    stream、token 和 task handle 不 serde，恢复时由 resolver 重建。
12. **垂直功能 API-first**：skill/mcp、plan、blackboard、agent 调度先是一等 Rust API；
    tool registry 中的工具只是薄 adapter。宿主程序必须能直接调用同一 API 进行编排与测试。
13. **plan 与 blackboard 分工**：plan 是带版本/CAS 的任务板，支持 task 状态、claim、依赖和
    更新但没有 executor；blackboard 是 append-only 群聊式消息流，带 sender/topic/read cursor，
    无锁、无认领、best-effort。
14. **每阶段必须 Review**：每个里程碑末尾有独立 `Mx-R` 审阅公共 API、状态边界、serde 边界、
    测试与 rustdoc；Review 不替代实现任务。

## 里程碑总览

| 里程碑 | 目标 | 主要产出 |
|---|---|---|
| **M1 Agent 基础数据与 RunContext** | 建立静态配置、运行状态和 run 级横切上下文 | `AgentId`、`AgentSpec`、`AgentState`、`LoopCursor`、`RunContext`、预算/trace/cancel trait |
| **M2 AgentLoop 步进模型** | 定义 feed→`AgentEvent` stream 契约并打通基础 LLM/tool 往返 | `AgentInput`、`AgentEvent`、`AgentOutcome`、loop driver、tool execution adapter |
| **M3 边界干预与恢复** | 支持 pivot、turn-boundary reconfig、approval、cancel 与 pause/resume | Conversation user 注入入口、pivot queue、reconfig queue、approval responder、snapshot restore |
| **M4 垂直功能 API-first** | 提供 skill/mcp、plan、blackboard 和 agent 调度最小原语 | `ToolRegistry`、`SkillBundle`、`PlanBoard`、`Blackboard`、`AgentSpawner` |
| **M5 横切运行时设施** | 预算、trace、hook/中间件与 compaction trigger 集成 | budget accounting、trace tree、runtime hooks、boundary-triggered compaction |
| **M6 跨功能验收与文档** | 完成端到端验收、示例、README/crate docs 与总 Review | 离线 Agent 示例、多 agent 验收、文档更新、Agent 层总 Review |

依赖顺序固定为：M1 → M2 → M3 → M4 → M5 → M6。后续里程碑只能依赖前序已暴露的受检
API，不得通过公开裸状态、unchecked serde 或 provider/task 私有特判绕开依赖。

## 建议目录与公共 API 边界

```text
src/
  agent/
    mod.rs                 # Agent facade、公共 prelude、统一错误
    id.rs                  # AgentId、RunId、StepId、PlanId、BlackboardId 等强类型 id
    spec.rs                # AgentSpec、WorktreeRef、ModelRef、ToolSetRef、SystemPrompt
    state.rs               # AgentState、LoopCursor、serde/runtime handle 分离
    context.rs             # RunContext、BudgetHandle、TraceHandle、CancellationToken adapter
    event.rs               # AgentInput、AgentEvent、AgentOutcome、StepBoundary payload
    loop_driver.rs         # AgentLoop trait 与默认推进器
    tool/
      mod.rs               # ToolRegistry、ToolExecutor、ToolAdapter、approval policy
      skill.rs             # SkillBundle、SkillId、turn-boundary activation
    intervention/
      mod.rs               # pivot/reconfig queue 与边界应用
      approval.rs          # AwaitingApproval、Responder、恢复数据
      cancel.rs            # RunContext cancel 与 Conversation cancel glue
    vertical/
      plan.rs              # PlanBoard、task 状态、claim CAS
      blackboard.rs        # append-only topic/message/read cursor
      orchestration.rs     # spawn/stop/send/await 最小原语
    runtime/
      budget.rs            # usage/step/wall-clock budget accounting
      trace.rs             # run→step→llm/tool/sub-agent trace tree
      hook.rs              # before/after llm/tool/step hooks
tests/
  agent_*.rs               # 单 Agent loop、边界干预、多 Agent/垂直功能验收
```

公共 API 只暴露受检操作和只读查询：创建 `AgentSpec`、从 spec/conversation 构造
`AgentState`、通过 `AgentLoop::feed` 推进、订阅 `AgentEvent`、排队 pivot/reconfig、
响应 approval、触发 cancel、snapshot/restore data-only state、直接调用 vertical API 或把
同一 API 注册为 tools。内部不得公开 `Conversation` raw commit、pending mutable container、
unchecked `LoopCursor`、可变 tool registry 快照或 bypass approval 的 tool 执行入口。

## 测试策略与完成门

- **数据/serde 单测**：强类型 id、`AgentSpec`、`AgentState`、`LoopCursor`、plan、blackboard、
  trace records 和 budget records 往返；runtime handles、client、registry、stream、responder
  与 cancellation handles 不被序列化。
- **loop 状态机测试**：覆盖 text-only、single tool、parallel tool、tool failure self-heal、
  max steps、budget exhausted、stop reason、stream backpressure 和 feed reentrancy 拒绝。
- **边界干预测试**：pivot 只在未来 step boundary 注入 user message；reconfig 只在 turn
  boundary 生效；approval stream 就地挂起并可恢复；cancel 后 Conversation 仍可继续 feed。
- **pause/restore 测试**：保存 `LoopCursor` + Conversation snapshot 后恢复，恢复后的事件、
  pending phase、tool mappings、budget 和 trace parentage 与未中断路径一致。
- **垂直 API 测试**：skill activation 重建 tool registry，MCP/tool adapter 只薄封装 API；
  plan claim 使用版本/CAS 防双重认领；blackboard append-only、topic 隔离和 read cursor 正确；
  子 agent 继承父 RunContext budget/cancel/trace。
- **跨 provider 回归**：Agent loop 只依赖 Client/Conversation provider-neutral API，能用离线
  fake `LlmClient` 覆盖 Anthropic/OpenAI 共同语义；真实 endpoint 测试仍默认 ignored。
- **命令顺序**：每个任务先运行 `cargo fmt --all`，再运行
  `cargo clippy --all-targets -- -D warnings`，随后运行聚焦测试和
  `cargo test --all --all-targets`，最后运行
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与 `git diff --check`。完整测试最长
  30 分钟，单个测试必须少于 1 分钟。

## Serde / 恢复边界

可持久化事实包括 `AgentSpec`、`AgentState` 的 data 部分、唯一活动 Conversation snapshot、
active skill ids、queued pivot/reconfig 请求、`LoopCursor`、plan/blackboard 数据、budget
剩余额度和 trace record。恢复必须重建 client、tool registry、skill resources、MCP session、
approval responder、stream、task handles 和 cancellation handles，并重新绑定到新的
`RunContext`。

不得持久化 live `LlmClient`、`ToolExecutor` trait object、tokio task、channel receiver、
`CancellationToken` 内部状态、approval responder closure、active stream 或锁。任何恢复后的
Conversation 都必须通过既有 `Conversation::restore` 校验；Agent 层不能从 snapshot 中直接
构造 unchecked pending 或 closed history。

## 每阶段结束的 Review

每个里程碑末尾必须有独立 `Mx-R` Review，核对本阶段是否遵守 `docs/agent-layer.md` 与
`DESIGN.md` §1.3 的边界：三层拆分、单活动 conversation、RunContext 贯穿、feed stream 背压、
step/turn 两级边界、API-first verticals、serde/runtime 分离、公共 API 封装、错误分类、测试
和 rustdoc。M6-R 额外回溯本计划与 `TODO.md` 全文，确认 Agent 层没有重新实现或弱化
Conversation Core 已落地的不变量。
