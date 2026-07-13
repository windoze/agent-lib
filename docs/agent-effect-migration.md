# Agent Effect Model —— 落地设计与迁移方案

> **状态:已落地。** 本文把 [`docs/agent-effect-model.md`](agent-effect-model.md) 的抽象
> 计算模型翻译成**具体 Rust 接口形状**与**分阶段迁移路径**;这些接口与迁移(Milestone
> 1–5)均已实现,主文档 [`agent-layer.md`](agent-layer.md) §1.3 / §3 / §4 也已按新模型
> 改写。下文 §1 映射表左列的旧 push API(`AgentLoop::feed`/`DefaultAgentLoop`/`AgentEvent`
> 混装流/pivot queue/`respond_approval`/`AgentFeedGuard` 等)已删除;右列的目标形状为
> 当前代码。**注意**:本文写作时预估的部分文件路径与最终落位略有差异 —— 实际实现见
> `src/agent/machine/`(机器契约与默认/嵌套机)、`src/agent/drive.rs` 与
> `src/agent/drive/`(handler/drain/pop/subagent)、`src/agent/requirement.rs`(寻址)、
> `src/agent/event.rs`(`Notification`/`AgentInput`)、`src/agent/context.rs` 及
> `src/agent/context/`(`RunContext`/budget/cancel/trace)。历史阶段划分与验收保留供追溯。
>
> 前置阅读:[`agent-effect-model.md`](agent-effect-model.md)(为什么),
> [`agent-layer.md`](agent-layer.md) §1.3 / §3 / §4(已改写的落地契约),
> [`conversation-core.md`](conversation-core.md)(`cancel_pending` / `fork_at` 地基)。

## 0. 本文要定死的东西

1. sans-io `step` 的**函数签名**与它的**输入/输出/状态**三个数据类型。
2. `Notification` 与 `Requirement` 的**具体 enum 形状**(从现有 `AgentEvent` 一分为二)。
3. `RequirementId` / `AgentPath` / `RequirementResult` 的**寻址与回程**形状。
4. handler / drain / pop 的**trait 与路由规则**。
5. `LoopCursor` 如何**升格**为整台机器的可序列化状态。
6. cancel = never-resume 如何接 `Conversation::cancel_pending`。
7. `RunContext` 在新模型里的**线程方式**(谁派生、谁可见)。
8. hierarchy / subagent 的**嵌套状态机**形状(留阶段 4,但接口先占位)。
9. 分阶段迁移顺序 + 每阶段的验收。

文末 §12 列出**仍需拍板的决策**,每条带一个默认建议。

---

## 1. 一句话映射:现有代码 → 新模型

| 现有(push / 自驱) | 新(pull / sans-io) | 文件 |
|---|---|---|
| `AgentLoop::feed(input) -> AgentEventStream` | `Agent::step(&mut self, input) -> StepOutcome` | `agent/loop_driver.rs` |
| `DefaultAgentLoop`(自持 client/tool/approval,async 自驱) | `AgentMachine`(纯 step)+ `Driver`(库外,async,持资源) | `agent/loop_driver/default.rs` 拆分 |
| `AgentEvent`(6 变体混装) | `Notification`(通知)+ `Requirement`(请求) | `agent/event.rs` |
| `AgentEvent::AwaitingApproval` | `Requirement::NeedInteraction` | `agent/event.rs` + `agent/approval.rs` |
| `ApprovalRequirement/Response/Policy` | `Interaction` 请求/响应 + interaction handler 后端 | `agent/approval.rs` |
| `CancellationToken` 单独机制 | never-resume handler(cancel 是 handler 行为) | `agent/context/cancel.rs` |
| pivot queue(`interject`) | 两次 step 之间多喂一个 `AgentInput::Pivot` | `agent/state/queue.rs` |
| `LoopCursor`(恢复 hint) | 整台机器的可序列化状态(`&mut state` 即推进句柄) | `agent/state/cursor.rs` |
| `AgentFeedGuard`(stream backpressure) | `&mut self` 天然背压,guard 弱化/删除 | `agent/loop_driver.rs` |
| (无) | `AgentPath` / `RequirementId` / requirement 登记表 | 新增 |
| (无) | hierarchy(嵌套机器)+ subagent handler | 新增,阶段 4 |
| `RunContext`(cancel/budget/trace) | 保留;由 drain scope 隐式派生,interaction 走 pop↑ | `agent/context.rs` |
| `Conversation::cancel_pending` / `fork_at` | 不动,成为 never-resume 与多路径的地基 | `conversation/` |

---

## 2. sans-io step 的核心签名

### 2.1 三个数据类型 + 一个纯函数

```rust
// agent/machine.rs (新文件;或并入 agent/loop_driver.rs 重命名后的模块)

/// 纯状态机:不做 IO,只推进状态并请求 IO。无 async。
pub trait AgentMachine {
    /// 推进一步。纯函数语义:不 await,不触碰 client/tool/进程。
    ///
    /// - `input` 要么是一次外部输入(user / pivot),要么是某个 requirement 的兑现结果。
    /// - 返回本步产生的通知、以及本步新卡住的 requirement(可能为空)。
    fn step(&mut self, input: StepInput) -> StepOutcome;

    /// 当前机器状态的只读视图(等价于现在的 inspect_state)。
    fn cursor(&self) -> &LoopCursor;
}

/// step 的输入:外部输入 或 requirement 回灌。
pub enum StepInput {
    /// 新的外部输入(开新 turn 或软转向)。
    External(AgentInput),
    /// 某个已发出 requirement 的兑现结果(回程)。
    Resume(RequirementResolution),
    /// 决定丢弃某个已发出的 requirement(never-resume;见 §7)。
    Abandon(RequirementId),
}

/// step 的产物:通知流片段 + 本步新增的 requirement + 是否静止。
pub struct StepOutcome {
    /// 本步产生的通知(可安全跳过,drain 只透传)。
    pub notifications: Vec<Notification>,
    /// 本步新卡住、等待外部兑现的 requirement(可为空)。
    pub requirements: Vec<Requirement>,
    /// 本步之后机器是否静止(所有分支要么产出、要么卡在 requirement 上)。
    pub quiescent: bool,
}
```

> **要点**:`step` 一次只做"从当前状态到下一个卡点/静止"的**同步**推进。它绝不 await。
> 所有 await 都在 driver 里对 requirement 的兑现上。`&mut self` 就是 §2.2 里说的天然背压:
> 没有把上一批 requirement 的结果 `Resume` 回来,机器无法前进。

### 2.2 现有 `AgentInput` 的调整

现有:
```rust
pub enum AgentInput { UserMessage(..), QueuedPivotTurn(..), Resume(ResumeInput) }
```
新模型下 `AgentInput` 只保留**外部**输入语义(`Resume` 语义搬到 `StepInput::Resume`,不再是
"从 cursor 恢复的黑盒",而是"某个 requirement 的结果"):
```rust
pub enum AgentInput {
    /// 新的 user-authored turn。
    UserMessage(AgentUserInput),
    /// 软转向:在两次 step 之间插入的 user-role pivot(取代 pivot queue)。
    Pivot(PivotMessage),
}
```
`QueuedPivotTurn` 消失:pivot 不再"排队等边界",而是 driver 在合适的 step 间隙直接
`step(StepInput::External(AgentInput::Pivot(..)))`。排队策略(何时插)归 driver / Session。

---

## 3. Notification 与 Requirement:把 AgentEvent 一分为二

### 3.1 Notification(通知,drain 可跳过)

从现有 `AgentEvent` 拆出**纯通知**部分,payload 全部复用现有类型:

```rust
// agent/event.rs
pub enum Notification {
    Llm(StreamEvent),                 // 现 AgentEvent::Llm
    StepBoundary(StepBoundary),       // 现 AgentEvent::StepBoundary
    ToolCallStarted(ToolCallStarted), // 现 AgentEvent::ToolCallStarted
    ToolCallFinished(ToolCallFinished),// 现 AgentEvent::ToolCallFinished
    // 未来:SubagentSpawned / SubagentOutput 等 hierarchy 通知
}
```

`AgentEvent::Done(AgentOutcome)` **不再是流里的一个事件**:turn 结束由 `StepOutcome.quiescent
== true 且 requirements 为空 且 cursor 到达 Done/Error` 表达。最终 output message 通过
`cursor()` / Conversation 读取(见 §5)。

### 3.2 Requirement(请求,drain 不能跳过)

```rust
// agent/requirement.rs (新文件)
pub struct Requirement {
    pub id: RequirementId,       // 本次请求唯一标识(回程路由用)
    pub origin: AgentPath,       // 发出者在 hierarchy 中的路径
    pub kind: RequirementKind,
}

pub enum RequirementKind {
    /// 要一次 LLM 调用。payload 复用 client::ChatRequest。
    NeedLlm { request: ChatRequest, mode: LlmStepMode },
    /// 要执行一个 tool。call/call_id 复用现有类型。
    NeedTool { call_id: ToolCallId, call: ToolCall },
    /// 要跟"用户"交互(泛化 AwaitingApproval)。
    NeedInteraction { request: Interaction },
    /// 要派生并驱动一个子 agent(唯一加深作用域链的 requirement)。
    NeedSubagent { spec_ref: AgentSpecRef, brief: Interaction, result_schema: Option<Value> },
}
```

- `NeedLlm` / `NeedTool` 的 payload 就是现在 `DefaultAgentLoop` 内部构造的 `ChatRequest` /
  `(ToolCallId, ToolCall)`,只是从"内部直接 await"变成"吐出来让 driver await"。
- `NeedInteraction` 承载现有 `ApprovalRequest`(退化成 yes/no interaction)+ 未来的开放问题/
  选项/澄清。见 §4。

### 3.3 回程:RequirementResolution

```rust
pub struct RequirementResolution {
    pub id: RequirementId,
    pub result: RequirementResult,
}

pub enum RequirementResult {
    Llm(Result<Response, ClientError>),
    Tool(Result<ToolResponse, ToolRuntimeError>),
    Interaction(InteractionResponse),      // 泛化 ApprovalResponse
    Subagent(Result<SubagentOutput, AgentError>),
}
```

driver 维护一张 `BTreeMap<RequirementId, PendingRequirement>` 登记表(§3 of effect-model:
"复杂度搬到这个可测试的纯数据点")。兑现后 `step(StepInput::Resume(resolution))` 回灌。
**类型对齐由库检查**:`RequirementKind::NeedLlm` 只接受 `RequirementResult::Llm`,否则分类报错。

### 3.4 RequirementId / AgentPath

```rust
pub struct RequirementId(Uuid);   // 或复用现有 id 生成边界(ToolExecutionIds 风格,不自己生成)
pub struct AgentPath(Vec<AgentSlot>);  // 根到当前节点的路径;根为空
pub struct AgentSlot(u32);        // 父机器里子机器的槽位
```
> **决策点(§12-A)**:`RequirementId` 是库自己生成(需引入 uuid 依赖 / 计数器)还是沿用
> `ToolExecutionIds` 那套"host 供给 id"的哲学。建议:**引入一个 `RequirementIds` 供给 trait**,
> 与现有 `ToolExecutionIds` 一致,保持"库不自己造 id"的既定风格。

---

## 4. NeedInteraction:泛化 approval

现有 `agent/approval.rs`:`ApprovalRequirement`(yes/no)+ `ApprovalResponse` +
`ToolApprovalPolicy`(NoApprovalPolicy 等)。新模型把它泛化:

```rust
// agent/interaction.rs (由 approval.rs 演进)
pub struct Interaction {
    pub step_id: StepId,
    pub kind: InteractionKind,
}
pub enum InteractionKind {
    /// 退化的 yes/no 审批(承接现有 ApprovalRequirement / ApprovalRequest)。
    Approval { call_id: ToolCallId, requirement: ApprovalRequirement },
    /// 开放问题 / 澄清。
    Question { prompt: String },
    /// 选项选择。
    Choice { prompt: String, options: Vec<String> },
}
pub enum InteractionResponse {
    Approval(ApprovalResponse),  // 复用现有类型
    Answer(String),
    Choice(usize),
}
```

- `ToolApprovalPolicy` 从"loop 内部调用的 policy"变成 **interaction handler 的一个后端**:
  attended 后端弹 UI 等人;unattended 后端就是把现有 policy(auto-approve/deny/default)
  包一层。**"运行模式"= 顶层 interaction handler 挂哪个后端**(effect-model §4.1)。
- 现有 `respond_approval(response)` 这个 loop 方法**删除**:审批响应就是
  `RequirementResult::Interaction(InteractionResponse::Approval(..))` 走通用回程。

---

## 5. LoopCursor 升格为整台机器的可序列化状态

现有 `LoopCursor`(state/cursor.rs)已经是 data-only、serde、无 live handle,变体几乎与
requirement 一一对应:

| 现有 LoopCursor 变体 | 对应 requirement / 状态 |
|---|---|
| `Idle` | 静止,无未决 requirement |
| `StreamingStep(StepCursor)` | 卡在 `NeedLlm`(id 记入 cursor) |
| `AwaitingTool(ToolWaitCursor)` | 卡在一批 `NeedTool`(id 集合记入 cursor) |
| `AwaitingApproval(ApprovalCursor)` | 卡在 `NeedInteraction`(id 记入 cursor) |
| `CancelRecovery(CancelRecoveryCursor)` | never-resume 后待 `cancel_pending` 收尾 |
| `Done` / `Error` | turn 终态 |

**升格动作**:在这些 cursor 里补上 `RequirementId`(以及未来 `AgentPath`),使 cursor 不再是
"恢复 hint"而是"精确记住我卡在哪个 requirement 上"。跨进程恢复时,driver 用 cursor 里的
`RequirementId` 重建未决登记表(effect-model §11 第 3 条的落地点)。

`AgentState`(state.rs)整体 = 一台单机器的可序列化状态;hierarchy 落地后,父机器 state
*包含*子机器 state,整棵树可序列化(effect-model §7.1)。**`AgentRuntimeHandles`(live handles)
彻底移出机器,归 driver**。

---

## 6. Driver、Handler、drain、pop

库提供机制,**driver 归调用者**(effect-model §7.4)。库侧给出 handler trait 与 drain 骨架;
tokio 编排(join/select/串行)由调用者写。

```rust
// agent/drive.rs (新文件,库侧提供 trait + 一个参考 drain)

/// 一层 drain 的 handler 集合。缺省行为 = pop(向上抛)。
pub trait HandlerScope {
    /// 返回本层能兑现的 requirement handler;None = 本层不兜底,pop 给外层。
    fn llm(&self) -> Option<&dyn LlmHandler> { None }
    fn tool(&self) -> Option<&dyn ToolHandler> { None }
    fn interaction(&self) -> Option<&dyn InteractionHandler> { None }
    fn subagent(&self) -> Option<&dyn SubagentHandler> { None }
}

#[async_trait]
pub trait LlmHandler: Send + Sync {
    async fn fulfill(&self, req: &ChatRequest, ctx: &RunContext) -> RequirementResult;
}
// ToolHandler / InteractionHandler / SubagentHandler 同理

/// 参考 drain:用一层 scope 把一台机器推进到 turn 结束。
/// 本层兜不了的 requirement 通过 `parent` 逐级 pop(§4.2)。
pub async fn drain<M: AgentMachine>(
    machine: &mut M,
    input: AgentInput,
    scope: &dyn HandlerScope,
    parent: Option<&mut dyn Pop>,   // None = 顶层,必须 total(§4.3)
    ctx: &RunContext,
) -> Result<TurnDone, AgentError>;
```

**pop 路由规则(库强制,effect-model §4.2 / §4.3 / §7.3)**:
1. 本层 scope 有对应 handler → 兑现,`Resume` 回灌,该 requirement 对上层不可见。
2. 本层无 → pop 给 parent;被穿过的每层只透传不解释。
3. pop 查找**从发出者 scope 的外层开始**(跳过自身,防 §7.3 即时环)。
4. 顶层(parent = None)仍无 handler → **立即分类报错** `AgentError::UnhandledRequirement`,
   绝不静默跳过或挂起。

**"运行模式就是 scope 差异"**:attended = 顶层 scope 的 `interaction()` 挂真人 UI 后端;
unattended = 挂 policy 后端;headless subagent = 内层 scope 不挂 `interaction()`,自动 pop 到
挂了的那层(effect-model §4.4)。

---

## 7. cancel = never-resume,接 cancel_pending

cancel 不是单独机制,是 handler 行为(effect-model §6.3)。落地:

1. driver 决定 cancel 一个已发出的 requirement 时,调用 `machine.step(StepInput::Abandon(id))`。
2. 机器**不回灌结果**,把对应 cursor 迁到 `CancelRecovery`,并在 outcome 里指明需要对哪个
   Conversation 做收尾。
3. driver / 机器触发该 Conversation 的 [`Conversation::cancel_pending`](conversation-core.md):
   补合成 `Cancelled` tool result 或丢弃 pending,闭合裂缝。
4. 收尾后 cursor 回到可 `feed` 的一致态(承接现有"cancel 后仍可 feed"的硬性验收)。

> 现有 `CancellationToken`(context/cancel.rs)**保留**用于向下广播"该停了"的信号(driver 据此
> 决定 Abandon 哪些 requirement),但它**不再**是 cancel 的实现主体——实现主体是 never-resume +
> `cancel_pending`。现有 `CancelRecoveryCursor` / `CancelRecoveryReason` 正好承接这个状态。

**multishot 不做**;多路径一律 `Conversation::fork_at` → 新 Agent 承载(effect-model §6.2)。
本迁移不引入任何 continuation 复制设施。

---

## 8. RunContext 的线程方式

`RunContext`(cancel/budget/trace)**保留**,但语义收紧到 effect-model §7.2 / §7.4:

- **不再由机器持有并到处传**;它由 **subagent handler 从"当前正在 drain 的 scope"隐式派生**
  (`RunContext::derive_child`),机器只拿到 data(spec_ref / brief / result_schema)。
- cancel↓ / budget↕ / trace↓ 沿 hierarchy 派生;**interaction 不进 RunContext**,它走 pop↑。
- 深度上限、预算继承、cancel 传播全部在 **subagent handler** 里强制(§7.2),不散落别处。

现有 `RunContext::derive_child` / `charge_step` / `check_cancelled` 基本可直接复用,只是调用点
从"机器内部"挪到"subagent handler 内部"。

---

## 9. Hierarchy 与 subagent(阶段 4,接口先占位)

- `agent + subagents` = 嵌套机器:父机器 state *包含* 子机器 state。
- 对 root 一次 `feed`,`step` 递归推进整棵树到静止;树上任意位置的 outstanding requirement
  被聚合成一批交给 driver(effect-model §7.1)。
- **并行在兑现层**:driver 并发兑现"子 B 要 LLM、子 C 要 tool、父要 tool"这批 requirement,
  按完成顺序 `Resume` 回灌。父子天然并发,无"父等子还是并行"难题(§7.1)。
- `NeedSubagent` handler = 派生子机器 + **再开一层 drain** 递归驱动;深度检查挂这里(§7.2)。

阶段 1–3 只做单机器;`AgentPath` 在单机器里恒为根路径(空),但**类型先就位**,避免阶段 4
再改签名。

---

## 10. 分阶段迁移顺序

每阶段独立可编译、可测,尽量不一次性推翻 `DefaultAgentLoop`。

**阶段 0 — 类型骨架(不改行为)**
- 新增 `Requirement` / `RequirementKind` / `RequirementId` / `AgentPath` /
  `RequirementResult` / `RequirementResolution`(`agent/requirement.rs`)。
- 新增 `Notification` enum;`Interaction*`(由 `approval.rs` 演进,旧 approval 类型保留 re-export)。
- 验收:`cargo build` 通过,新类型有 serde round-trip 单测,旧路径不受影响。

**阶段 1 — 抽出 sans-io `step`**
- 把 `DefaultAgentLoop` 的推进逻辑(`NonStreamingSegment` / `StreamingSegment` 的
  `next_event`)重构成纯 `AgentMachine::step`:原来"await client/tool"的点改为"吐 requirement 并
  返回",原来 await 的结果改为"下一次 `Resume` 输入"。
- 验收:`step` 单测(喂 requirement 结果序列 → 断言 notifications/requirements/cursor),无 async。

**阶段 2 — 参考 driver + drain(单层)**
- 库侧 `drain` + `HandlerScope` + 四个 handler trait;顶层 total 检查 + `UnhandledRequirement`。
- 提供一个 driver 把现有 `LlmClient` / `ToolRegistry` / approval policy 包成 handler 后端。
- 验收:用 driver 复跑现有 `DefaultAgentLoop` 的集成测试(文本 turn、tool turn、审批 turn)全绿。

**阶段 3 — cancel / pivot 收编 + 删旧机制**
- cancel → `Abandon` + `cancel_pending`;pivot → `AgentInput::Pivot`;删 `respond_approval` /
  pivot queue / `AgentFeedGuard`(或降级)。
- 验收:"cancel 后仍可 feed"验收迁移到新路径并通过;pivot 软转向测试通过。

**阶段 4 — hierarchy / subagent**
- 嵌套机器 + `NeedSubagent` handler + 深度/预算/cancel 在 handler 强制 + trace resolved-by-scope。
- 验收:attended 父 + headless 子(子 `NeedInteraction` pop 到父真人)端到端测试。

**阶段 5 — 文档并轨**
- 更新 `agent-layer.md` §1.3/§3/§4、`PLAN.md`/`TODO.md` M2/M3(effect-model §10 列出的差异)。

---

## 11. Observability(随阶段 2/4 落地)

每个 requirement 在 trace 记录:**被哪层 scope 的 handler 兑现(resolved-at-scope)**、
**resume 还是 never-resume**(effect-model §8)。接现有 `TraceHandle` / `TraceNodeKind`,
新增 `TraceNodeKind::Requirement { resolved_at_scope, disposition }`。

---

## 12. 仍需拍板的决策(每条带默认建议)

- **A. RequirementId 生成边界**:库自造(引 uuid)还是沿 `ToolExecutionIds` 的"host 供给"哲学。
  *建议*:新增 `RequirementIds` 供给 trait,保持"库不造 id"的既定风格。
- **B. `step` 的批量语义**:一次 `step` 只推进到"下一个卡点"还是"推进到静止(可能一次吐一批
  requirement)"。*建议*:**推进到静止并一次吐一批**——直接支撑 §7.1 的 hierarchy 聚合与父子并发。
- **C. 一批 requirement 是否要稳定顺序 / 优先级**(interaction 优先于 llm?)。*建议*:阶段 1–3
  不排序(driver 自行编排),留到首批多 agent 用例再定(effect-model §11 第 2 条)。
- **D. token delta 的 tee**:`NeedLlm` 兑现里怎么把 token 流 tee 给 UI,与 drain 跳过通知的边界。
  *建议*:`LlmHandler::fulfill` 返回 `Response` 之外,另给一个可选 `sink` 参数供 driver 转发
  delta;阶段 2 先不做,drain 直接透传 `Notification::Llm`。
- **E. `DefaultAgentLoop` 去留**:重构成"薄 driver"还是彻底删除、只留 `AgentMachine` + 库 driver。
  *建议*:阶段 2 保留为"参考 driver 的默认实现",阶段 3 末评估是否合并进库 driver。
- **F. `AgentFeedGuard` 命运**:`&mut self` 已提供背压,guard 是否还需要防重入。*建议*:降级为
  debug_assert 或删除。
- **G. 文档主线**:`agent-effect-model.md` + 本文最终是**并入** `agent-layer.md` 主文档,还是长期
  作为独立设计卷。*建议*:阶段 5 并入,`agent-layer.md` §1.3 直接改写为 pull 契约。
