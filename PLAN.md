# 实施计划：Agent Effect Model 迁移

> 本计划以 [`docs/agent-effect-model.md`](docs/agent-effect-model.md)(为什么)与
> [`docs/agent-effect-migration.md`](docs/agent-effect-migration.md)(接口形状与迁移路径)
> 为规范性设计输入。它把现有 push / 自驱的 `AgentLoop::feed → AgentEvent stream` 契约
> 翻转为 sans-io + effect-handler 的 `step → (notifications, requirements)` pull 契约。
>
> 被本计划取代的旧 Agent Layer 计划(M1–M3 已落地)归档在
> [`docs/archive/2026-07-13-agent-layer/`](docs/archive/2026-07-13-agent-layer/);
> 已完成的 Conversation Core 归档在
> [`docs/archive/2026-07-13-conversation/`](docs/archive/2026-07-13-conversation/)。
> 逐任务要求与完成记录见 [`TODO.md`](TODO.md)。

## 范围与非目标

**范围**:把已落地的单 Agent 运行时(`AgentSpec`/`RunContext`/`AgentState`/`LoopCursor`/
`DefaultAgentLoop`/pivot/reconfig/approval/cancel)重构为 sans-io 计算模型:纯 `step`
状态机(不做 IO、只吐 requirement)、库外 async driver、requirement/notification 二分、
handler scope + drain + pop 路由、cancel = never-resume 接 `Conversation::cancel_pending`、
`LoopCursor` 升格为整台机器的可序列化状态,并落地 agent hierarchy 与 subagent handler。
复用已完成的 Client `LlmClient`/`ChatRequest`/`Response`/`StreamEvent` 与 Conversation 的
committed log、pending、`Boundary`、`cancel_pending`、`fork_at`、snapshot/restore 能力。

**非目标**:不引入 continuation 复制 / multishot(多路径一律 `Conversation::fork_at` → 新
Agent 承载);不发明通用 DAG/workflow/scheduler;不重新实现 Conversation 的 I1–I4、tool
pairing、`Boundary` 校验或 restore 门;不引入 provider 特判;不支持一个 Agent 同时持有多个
活动 Conversation。driver 编排(join/select/串行)归调用者,库只提供机制与不变量。

## 规范优先级与已采纳的关键决策

`docs/agent-effect-migration.md` 是本阶段权威接口输入;若它与旧 `agent-layer.md` §1.3/§3/§4
冲突,以迁移文档的 pull 契约为准。迁移文档 §12 的开放决策,本计划**采纳其默认建议**:

1. **sans-io `step` 是核心**:`step(&mut self, StepInput) -> StepOutcome`,纯、同步、无 IO、
   无 async;`&mut self` 即天然背压。所有 await 在 driver 兑现 requirement 时发生。
2. **AgentEvent 一分为二**:`Notification`(通知,drain 可跳过)与 `Requirement`(请求,
   drain 不能跳过,带 `id + origin` 可寻址回程)。turn 结束由 `StepOutcome.quiescent + cursor
   到达 Done/Error` 表达,不再是流里的 `Done` 事件。
3. **step 推进到静止并一次吐一批 requirement**(决策 B):直接支撑 hierarchy 聚合与父子并发。
4. **RequirementId 由 host 供给 trait 分配**(决策 A):新增 `RequirementIds`,与既有
   `ToolExecutionIds` 一致,保持"库不自己造 id"的既定风格。
5. **NeedInteraction 泛化 approval**:现有 `ApprovalRequirement/Response/Policy` 成为
   interaction 的一个子类型 + interaction handler 后端。"运行模式"= 顶层 interaction handler
   挂真人 UI(attended)还是 policy(unattended)。删除 loop 上的 `respond_approval`。
6. **cancel = never-resume handler**:`step(StepInput::Abandon(id))` 不回灌结果,迁 cursor 到
   `CancelRecovery`,触发被弃子树 `Conversation::cancel_pending` 闭合,收尾后仍可 feed。
   `CancellationToken` 保留为向下的"该停了"信号,不再是 cancel 实现主体。
7. **pivot = 多喂 input**:`AgentInput::Pivot`,删除 pivot queue / `QueuedPivotTurn` /
   `interject` 的排队语义;何时插入由 driver / Session 决定。
8. **LoopCursor 升格**:现有变体已与 requirement 一一对应,补 `RequirementId`(及 `AgentPath`)
   后即"精确记住卡在哪个 requirement 上";整台机器(含子机器)可序列化,live handle 全移出。
9. **RunContext 由 drain scope 隐式派生**:cancel↓/budget↕/trace↓ 沿 hierarchy 派生,
   interaction 走 pop↑;深度上限、预算继承、cancel 传播全部在 subagent handler 强制。
10. **pop 路由库强制**:本层无 handler 则 pop 给外层(查找从发出者外层起,跳过自身防即时环);
    顶层仍无 handler 即分类报错 `UnhandledRequirement`,绝不静默跳过或挂起。
11. **每阶段必须 Review**:每个里程碑末尾有独立 `Mx-R`,审阅接口形状、serde 边界、pop 路由
    不变量、测试与 rustdoc;Review 不替代实现任务。

## 里程碑总览

| 里程碑 | 迁移文档阶段 | 目标 | 主要产出 |
|---|---|---|---|
| **M1 类型骨架** | 阶段 0 | 新增 requirement/notification/interaction 类型,不改行为 | `Requirement`/`RequirementKind`/`RequirementId`/`AgentPath`/`RequirementResult`/`Notification`/`Interaction*` |
| **M2 sans-io step** | 阶段 1 | 把 `DefaultAgentLoop` 推进逻辑抽成纯 `AgentMachine::step` | `AgentMachine`/`StepInput`/`StepOutcome`、`AgentInput` 调整、`LoopCursor` 升格 |
| **M3 driver + drain(单层)** | 阶段 2 | 库侧 handler scope + drain + pop;参考 driver 复跑现有集成测试 | `HandlerScope`/四个 handler trait/`drain`/`UnhandledRequirement`、参考 driver |
| **M4 cancel / pivot 收编** | 阶段 3 | cancel→never-resume、pivot→多喂 input,删旧机制 | `Abandon` + `cancel_pending` glue、`AgentInput::Pivot`、删 `respond_approval`/pivot queue/guard |
| **M5 hierarchy / subagent** | 阶段 4 | 嵌套状态机 + `NeedSubagent` handler + 作用域强制 | 嵌套机器 state、`SubagentHandler`、深度/预算/cancel 强制、trace resolved-by-scope |
| **M6 文档并轨与端到端验收** | 阶段 5 | 更新主文档,attended+headless 端到端验收与示例 | `agent-layer.md`/`README` 更新、多 agent 示例、Agent 层总 Review |

依赖顺序固定:M1 → M2 → M3 → M4 → M5 → M6。后续里程碑只依赖前序已暴露的受检 API,不得
公开裸机器状态、unchecked serde 或绕过 pop 路由的兑现入口。

## 受影响的目录与公共 API 边界

```text
src/agent/
  requirement.rs   # 新增:Requirement、RequirementKind、RequirementId、AgentPath、
                   #        RequirementResult、RequirementResolution、RequirementIds 供给 trait
  interaction.rs   # 由 approval.rs 演进:Interaction/InteractionKind/InteractionResponse;
                   #        旧 Approval* 类型保留并 re-export
  event.rs         # AgentEvent 拆出 Notification;调整 AgentInput(UserMessage/Pivot)
  machine.rs       # 新增:AgentMachine trait、StepInput、StepOutcome
  loop_driver.rs   # 弱化/删除 AgentFeedGuard;AgentLoop → driver 侧
  loop_driver/default.rs  # 拆分:纯推进逻辑 → step;client/tool/approval/sleep → driver + handler
  state/cursor.rs  # LoopCursor 各变体补 RequirementId / AgentPath,升格为机器状态
  state.rs         # AgentState 移出 live handle;为 hierarchy 预留子机器包含结构
  drive.rs         # 新增:HandlerScope、LlmHandler/ToolHandler/InteractionHandler/SubagentHandler、
                   #        drain 参考实现、Pop 路由、UnhandledRequirement 错误
  context.rs       # RunContext 保留;派生点从机器内部挪到 subagent handler
  context/trace.rs # 新增 TraceNodeKind::Requirement { resolved_at_scope, disposition }
```

公共 API 只暴露受检操作与只读查询:构造机器、`step` 推进、消费 `Notification`、把
`Requirement` 交给 driver 兑现、`Resume`/`Abandon` 回灌、drain/scope 组合、snapshot/restore
data-only 机器状态。内部不得公开裸 mutable 机器容器、unchecked cursor、可跳过 pop 的兑现
入口或 bypass `cancel_pending` 的丢弃入口。

## 测试策略与完成门

- **类型/serde 单测**:全部新增 requirement/notification/interaction/cursor 类型 round-trip;
  `RequirementResult` 与 `RequirementKind` 的类型对齐(NeedLlm 只接 Llm result)分类报错。
- **step 状态机测试(纯、无 async)**:喂 requirement 结果序列 → 断言 `notifications`/
  `requirements`/`cursor`/`quiescent`;覆盖 text-only、single/parallel tool、tool failure
  self-heal、approval(NeedInteraction)、max steps、budget exhausted、abandon 后 cursor 状态。
- **drain / pop 测试**:本层兑现不冒泡;本层无 handler 则 pop;顶层无 handler 报
  `UnhandledRequirement`;pop 从外层起(handler 自 perform 同类不回到自身)。
- **迁移回归**:M3 用参考 driver 复跑现有 `DefaultAgentLoop` 的 50 个集成测试
  (`src/agent/loop_driver/default/tests.rs`)语义,text/tool/approval turn 全绿。
- **cancel/pivot 测试**:"cancel 后仍可 feed" 迁到 never-resume 路径并通过;pivot 软转向。
- **hierarchy 测试**:attended 父 + headless 子(子 `NeedInteraction` pop 到父真人后端)端到端;
  父子并发兑现按完成顺序回灌;深度上限/预算继承/cancel 传播由 subagent handler 强制。
- **命令顺序**:每个任务先 `cargo fmt --all`,再 `cargo clippy --all-targets -- -D warnings`,
  随后聚焦测试与 `cargo test --all --all-targets`,最后
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与 `git diff --check`。全量测试最长 30
  分钟,单个测试必须少于 1 分钟。

## Serde / 恢复边界

可持久化事实:`AgentSpec`、机器状态(含 `LoopCursor` + 各 cursor 里的 `RequirementId` /
`AgentPath`)、唯一活动 Conversation snapshot、active skills、budget 剩余额度、trace record。
跨进程恢复时,driver 用 cursor 里的 `RequirementId` 重建未决 requirement 登记表。

不得持久化:live `LlmClient`/`ToolRegistry`/handler、tokio task、channel、
`CancellationToken` 内部状态、interaction 后端、active stream。恢复后的 Conversation 必须
通过既有 `Conversation::restore` 校验。

## 每阶段结束的 Review

每个里程碑末尾必须有独立 `Mx-R`,核对是否遵守迁移文档:sans-io `step` 不 await、
requirement/notification 二分、`id + origin` 可寻址、pop 路由与顶层 total、cancel=never-resume
接 `cancel_pending`、多路径走 `fork_at` 不做 multishot、RunContext 由 scope 派生、serde/runtime
分离、公共 API 封装、错误分类、rustdoc。M6-R 额外回溯本计划与 `TODO.md` 全文,确认没有
重新实现或弱化 Conversation Core 已落地的不变量。
