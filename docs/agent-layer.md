# Agent 层设计

> 本文档细化 `DESIGN.md` §1.3 Agent Management / Orchestration 层。它承接 Conversation
> 层已落地的地基(committed log + pending + projection + `Boundary` + cancel 一致性,
> 见 `docs/conversation-core.md`),只讨论 agent 层自己的形状与它对下层的复用/约束。
>
> **迁移状态:已落地。** 本稿早期的 push / 自驱 `feed → AgentEvent stream` 契约已被
> **sans-io + effect-handler** 模型取代并落地实现(设计见
> [`docs/agent-effect-model.md`](agent-effect-model.md),接口与迁移见
> [`docs/agent-effect-migration.md`](agent-effect-migration.md))。核心翻转:agent loop
> 不再自己驱动、自持 client/tool,而是一台 **sans-io 状态机**(`AgentMachine::step`,
> 见 `src/agent/machine/`),只**请求** IO(reify 成可寻址的 `Requirement`,见
> `src/agent/requirement.rs`),由库外**带作用域的 handler**(`src/agent/drive.rs`)兑现
> 或逐级向上 pop。审批 / pivot / cancel 不再是三种并列机制,而是**同一 requirement +
> handler 机制的三种表现**(见 §3)。下文 §1.3 / §3 / §4 已按该模型改写。
>
> 本稿的两个核心取向仍然成立:
> 1. **垂直功能一律 API-first,tool 只是其中一个 adapter**(见 §5)。
> 2. **pivot / reconfig 等运行时 context 变更归一到两级 `Boundary` 机制**:
>    pivot 在 step boundary 向 pending 注入 `user` 消息;skill 激活、tool set 与
>    system prompt 变更作为 reconfig 在 turn boundary 走配置路径生效(见 §4)。

## 0. 范围与非目标

**本层负责**:把"一个 LLM client + 一个 conversation + 一组 tool + 一个 loop 策略"
组织成一个可推进、可暂停/恢复、可取消、可观测的执行单元(Agent Runtime),并为多
agent 编排提供**最小原语**。

**非目标**:
- 不发明"大而全的编排引擎"。多 agent 拓扑(委派 / pipeline / swarm)用普通 Rust
  (tokio task / channel / join)写,本层只给可 spawn / 可传消息 / 可收结果的原语。
- 不重新实现 conversation 的不变量。tool 配对、边界合法性、cancel 闭合都在下层完成,
  本层**调用**而非重造。
- 不把 plan / blackboard / 调度做成隐藏的执行引擎(见 §6 的克制原则)。

## 1. 分层:Agent 不是一个 struct,是三样东西的组合

反模式:把 metadata、runtime state、loop 引擎糊成一个大 `Agent` struct。本稿显式拆三层,
每层的**序列化边界**和**可变性**不同(呼应 `DESIGN.md` §3 的序列化分界)。

```
Agent = AgentSpec  (静态 identity/config)   ← serde,不变
      + AgentState (运行时状态)              ← 部分 serde(数据),部分不 serde(句柄)
      + AgentMachine (sans-io 状态机/行为)   ← 推进状态,不做 IO
        由库外 Driver + HandlerScope 驱动    ← 不 serde,async,持资源
        贯穿 RunContext (贯穿三层的 run 级上下文)
```

### 1.1 AgentSpec —— 静态 identity / config(serde)
一个 agent 的"出厂设置",可持久化、可作为模板复制:

```rust
struct AgentSpec {
    id: AgentId,
    worktree: WorktreeRef,          // 工作目录/仓库根,agent 的文件系统边界
    system_prompt: SystemPrompt,    // 初始 system prompt(可被 skill 叠加,见 §6.1)
    initial_tools: ToolSetRef,      // 初始工具集(name → 声明)
    model: ModelRef,                // 默认 client / model 选择
    // loop 策略参数(max_steps、并行度、错误处理策略等)
}
```

> `AgentSpec` **不含** conversation、不含运行时句柄。它是"配方",不是"活体"。

### 1.2 AgentState —— 运行时状态
一个正在跑的 agent 的活体状态。**注意区分数据(可 serde,用于中断续跑)与句柄(不
serde,恢复时重建)**:

```rust
struct AgentState {
    conversation: Conversation,     // 唯一活动会话(data,serde)
    active_skills: Vec<SkillId>,    // 当前激活的 skill(data)—— 影响 prompt/tool
    tool_registry: ToolRegistry,    // 当前有效工具集(句柄,不 serde,恢复时按 skill 重建)
    loop_cursor: LoopCursor,        // loop 停在哪(用于暂停/恢复,data)
    // budget 余量在 RunContext,不在这里(见 1.4)
}
```

**"一个活动 conversation"是刻意的简单,不是权宜**:一个 Agent 同时只持有**一个**
活动 conversation,**不给 agent 加"会话池 / 挂起会话"这类复杂度**。设计原则:

> **保持 agent 结构简单,复杂度上移到 multi-agent orchestration。**
> 简单的 agent 才能被灵活编排,而编排(拓扑发散:委派 / pipeline / swarm)才是**最经常
> 变**的地方。把复杂度堆在 agent 里,会让每次编排改动都被 agent 内部状态绊住;把 agent
> 做薄,编排层就能自由组合。这与 `DESIGN.md` §1.3"先把 Layer A 做扎实、过早抽象编排必
> 选错"同向。

因此多路径探索 / A-B / 分叉不靠"一个 agent 多个会话",而靠 **fork 出新 Conversation →
新 Agent 承载**(fork 在下层是 O(1) 共享前缀,见 conversation-core §7)。"agent ↔ 会话"
严格一一对应,状态推理简单;发散拓扑一律交给编排原语(§7)。

### 1.3 AgentMachine —— sans-io 状态机(推进状态,不做 IO)
`DESIGN.md` §1.3 Layer A 的步进模型已翻转为 **pull / sans-io**:agent 不再自己驱动、
自持 client/tool,而是一台纯状态机,由库外的 driver 反复调用 `step` 推进。`step`
**不 `await`、不碰 client/tool/进程**;它只推进自己的状态,并把需要的 IO **reify 成
`Requirement`** 交回给 driver。实现见 `src/agent/machine/mod.rs`(契约)与
`src/agent/machine/default/`(默认 LLM/tool 机)。

```rust
// src/agent/machine/mod.rs
pub trait AgentMachine {
    /// 一次同步推进:从当前状态走到下一个阻塞点或静止,绝不 await。
    /// input 要么是一个新的外部输入,要么是某个 requirement 的兑现结果(回程)。
    fn step(&mut self, input: StepInput) -> StepOutcome;
    /// 只读查看当前 cursor(effect 模型下等价于查看 loop cursor)。
    fn cursor(&self) -> &LoopCursor;
}

pub enum StepInput {
    External(AgentInput),               // 新的外部输入:开新 turn 或软转向(pivot)
    Resume(RequirementResolution),      // 某 requirement 的兑现结果(回程)
    Abandon(RequirementId),             // 丢弃某 requirement(never-resume,cancel 用)
}

pub struct StepOutcome {
    pub notifications: Vec<Notification>,  // 本步产出的通知,driver 只转发
    pub requirements: Vec<Requirement>,    // 本步新阻塞的 requirement(可为空,可为一批)
    pub quiescent: bool,                   // 机器是否已静止(每个分支都产出或已阻塞)
}
```

**背压 = `&mut self`。** 机器一旦阻塞在一批 requirement 上,不喂回结果(`Resume`)就无法
再推进;没有跑在 driver 前面的隐藏内部队列,不再需要单独的 feed guard。

`step` 产出两类东西,严格二分(见 `src/agent/event.rs` 与 `src/agent/requirement.rs`):

```
Notification(通知,driver 只转发,不必回应)=
  | Llm(StreamEvent)                       // 透传 text/thinking delta
  | StepBoundary(StepBoundary)             // 边界:trace/预算/compaction/pivot 生效点
  | ToolCallStarted / ToolCallFinished

Requirement(请求,必须由某层 handler 兑现或逐级 pop)= { id, origin: AgentPath, kind }
  kind =
  | NeedLlm { .. }                         // 要一次 LLM 生成(非流式/流式由 LlmStepMode 定)
  | NeedTool { .. }                        // 要执行一次 provider-neutral tool
  | NeedInteraction { request }            // 要一次交互(审批 / 人类输入)
  | NeedSubagent { .. }                    // 要派生并驱动一个子 agent
  | NeedReconfigRegistry { .. }            // 要把新的 tool set 解析为 live registry
```

每个 `Requirement` 带 `id`(`RequirementId`)与 `origin`(`AgentPath`),可在嵌套 agent
树里精确寻址回程结果(见 §6.3、`src/agent/requirement.rs`)。旧 push 契约里的
`AwaitingApproval`(就地挂起)、`respond_approval`(回灌审批)、`interject`(pivot 队列)、
`Done(Outcome)`(结束事件)与 `AgentEvent` 单一混装流都已删除:审批变成
`NeedInteraction`、pivot 变成 `StepInput::External(AgentInput::Pivot)`、cancel 变成
`StepInput::Abandon`、结束由 `quiescent` + 终态 cursor 表达(见 §3)。

> `Agent` 顶层 struct 只是把 spec / state / machine 按引用组合起来的壳,不承载额外语义。
> **谁来驱动一台机器(哪个前端、是否并行)由库外的 driver 决定,不再写死在 loop 内部。**

### 1.4 RunContext —— 贯穿三层的 run 级上下文(必须一等)
`DESIGN.md` 下向约束第 6 条要求 budget / cancellation 有能贯三层下传的载体。它由**驱动机器
的 scope 派生并持有**(不进机器的 serde 状态),否则子 agent 的取消/预算传不下去
(实现见 `src/agent/context.rs`):

```rust
struct RunContext {
    cancel: CancellationToken,      // 贯穿 driver / streaming / tool / 子 agent
    budget: BudgetHandle,           // token / 成本 / 步数 / wall-clock 上限,每步检查
    tracer: TraceHandle,            // run→step→llm/tool/sub-agent 一棵树,可重建
    // 子 agent 从父的 RunContext 派生(cancel 传播、budget 继承,见 §6.3)
}
```

## 2. Agent 推进模型(Layer A 复用点)

一个 turn 通常跨多个 LLM 轮次(call→tool→call…):driver 反复 `step`,机器每次同步走到
下一个阻塞点(交回一批 requirement)或静止;driver 兑现 requirement 后经 `Resume` 喂回,
如此往复直到本 turn 到达终态 cursor。每个 step 边界(= conversation 的 step `Boundary`)
是**同一批横切逻辑的统一求值点**:

| 在 step 边界发生 | 归属 |
|---|---|
| 预算/配额检查(超限中止) | RunContext.budget |
| compaction trigger 求值(turn 边界才执行) | conversation projection |
| pivot 并入 pending(见 §4) | conversation 注入入口 |
| trace step 节点收尾 | RunContext.tracer |
| loop 可暂停点(序列化 `LoopCursor` + conversation) | AgentState |

**别各挖各的洞**:上面全部挂在"conversation 暴露的边界事件"这一个点上,呼应
`DESIGN.md` 下向约束第 3 条(Boundary 被 agent 层复用)。

## 3. 审批 / pivot / cancel:同一 requirement + handler 机制的三种表现

旧稿把审批 / pivot / cancel 当**三种并列机制**(各有各的挂起点、回灌入口、取消通道)。
落地模型把它们统一到**一个机制**上:机器只 `step`、只 reify `Requirement`,由带作用域的
handler(`src/agent/drive.rs`)兑现或逐级向上 pop;取消是"永不兑现某 requirement"。
三者只是这套机制的三种表现:

| 干预 | 在新模型里的形状 | 兑现 / 路由 |
|---|---|---|
| **审批**(human-in-loop) | 机器 `step` 交回 `Requirement{ kind: NeedInteraction }`;`&mut self` 天然挂起,不结束任何流 | `InteractionHandler` 兑现:attended scope 接人类 UI,unattended scope 接 `ToolApprovalPolicy`;本层不挂 handler 则 **pop** 到外层。结果经 `StepInput::Resume(RequirementResult::Interaction(..))` 回灌 |
| **pivot**(用户改向) | 不是特殊通道,而是两次 step 之间**多喂一个** `StepInput::External(AgentInput::Pivot(..))`;在合法 step 边界注入 `user` 消息(见 §4) | 由 driver / session 决定何时喂;库内不再排队(`interject` / pivot queue 已删) |
| **cancel** | 不是单独的取消流,而是对在途 `Requirement` 喂 `StepInput::Abandon(id)`,即 **never-resume** | driver 观察到 `RunContext.cancel` 后 abandon 未兑现 requirement,并调 `Conversation::cancel_pending` 闭合 pending;闭合后仍可再喂(硬性验收标准) |

**统一带来的三个好处**(正是 push 契约做不到的,见 `agent-effect-model.md` §1):

- **交互能向上路由。** 子 agent 的 `NeedInteraction` 若本层 scope 不挂 interaction handler,
  会**逐级 pop** 到最外层挂着人的 scope 被兑现(`src/agent/drive.rs` 的 `Pop` 路由),
  cancel 向下传、交互向上路由这两条路径都成立。
- **attended 与 unattended 是同一张图的两种跑法。** 同一台机器、同一批 requirement;
  差别只在**外层 scope 挂的是哪种 handler**(人类 UI vs policy),机器本身不需要任何配置。
- **谁驱动、是否并行由 driver 决定。** 前端 / headless 任务复用同一台机器,不再各写一套
  事件循环,cancel / pivot / 交互的边角也不必各自重写。

三者的正确性仍压在下层已实现的不变量上:pivot 不违反 tool 配对,cancel 后 conversation
仍可 feed。

## 4. 两级边界分工:pivot 走 step 边界,reconfig 只在 turn 边界

**核心取向:选能简化设计的方案 —— 把两类变更钉在两级不同边界上**(呼应 conversation-core
§2.2 的 step 边界 vs turn 边界):

| 变更 | 内容 | 生效边界 | 落地形式 |
|---|---|---|---|
| **pivot** | 追加一条 `user` 消息 | **step 边界**(turn 内也可,tool_result 之后) | 向 pending 注入 message |
| **reconfig** | 改 system prompt 叠加 / tool set / skill 启停 | **仅 turn 边界**(turn 结束后) | config/projection 级变更,不进 role 序列 |

**为什么 reconfig 限死在 turn 边界 —— 这是最大的简化**:一个 turn 内**工具集恒定**,于是
"turn 中途换了工具集、pending 里还挂着引用旧工具集的调用"这类问题**根本不存在**。skill
激活、tool 增删、system prompt 变更一律推迟到当前 turn 完全结束(机器静止 `quiescent`、
tool 配对全闭合)后才应用。pivot 只管注入消息、不碰配置,两者互不干扰。

> 换句话说:**pivot 限定为"消息",不包含"reconfig";reconfig 只能在 turn 之间做。**

### 4.1 pivot(消息注入)的精确语义
原始表述"pivot 插入点钉死在 tool_result 后面"方向对,但要精确成四条:

1. **落点是"最近一个未来的合法 `Boundary`",不保证在当前 turn 内。**
   若当前 step 是无 tool 的纯文本收尾,当前 turn 内没有"tool_result 之后"的切点,
   pivot 只能落到 turn 末尾 = 下一个 turn 开头。准确说法是**"下一个 step 边界生效"**,
   不承诺"一定插进当前 turn"。

2. **注入进 pending 区,不碰 committed log。**
   committed log 的不变量(I1 tool 配对 / I2 role 合法 / I3 无 partial)恒真。pivot 是
   往 pending turn 追加一条消息,随这一段推进到边界一起 commit。

3. **注入的消息一律是 `user` role。**
   其他 role 没有明显用处,且引入方言坑:`system` 在下层被归一化成 config
   (`ConversationConfig`),本就不进 role 序列,想改 system 应走 config/skill 那条路,
   而非往消息流塞 system;会话中段注入 system 的语义各 provider 不一致。选 `user` 则:
   - **兼容**:`tool_result` 后接 user 两家 provider 无条件合法。
   - **语义自洽**:即便是 coordinator 向 subagent 插入的消息,站在 subagent 视角,它的
     "用户"就是那个 coordinator —— 外部输入天然是 user turn。
   - **原语更简单**:注入只有一个 role,边界合法性判定只剩一种情况。

   注入的**来源**(human / coordinator / skill 触发等)靠消息 `meta` 记录,**不新造 role**。

4. **需要 conversation 层一个"边界注入入口"。**
   `begin_turn(user_payload)` 一个 turn 绑**一条** user 消息;pivot 注入第二条 user
   消息意味着 pending turn 要支持"在 step 边界追加 user 消息"。当前实现由
   `Conversation::inject_user_message` 提供该受检入口,注入后仍须满足 role 序列合法
   (tool_result 后接 user 合法)与 tool 配对。**在 agent 层,pivot 不是特殊通道**:
   driver / session 在合法 step 边界把 `AgentInput::Pivot(..)` 作为
   `StepInput::External` 喂给机器,机器再调该入口注入,并在同一 step 重渲染 LLM 请求推进
   本 turn(见 §3、`src/agent/event.rs` 的 `AgentInput::Pivot`)。

### 4.2 reconfig(配置变更)只在 turn 边界
skill 启停、tool 增删、system prompt 变更都改的是 config/projection,**不进 role 序列**,
一律推迟到 turn 边界应用:
- turn 内工具集恒定 ⇒ 无"引用旧工具集的悬空调用"问题(见 §4 表下说明)。
- 变更改的是 `AgentState.active_skills` / `tool_registry` 与下一 turn 渲染进 context 的
  system 叠加;它是 config 写入路径,**不复用 pivot 的 message 注入原语**。
- 若 reconfig 请求在 turn 进行中到达,**排队到当前 turn 结束后生效**(类比 pivot 的
  "软转向",只是生效边界更粗)。

当前默认机器由 `ReconfigRequest`/`ReconfigQueue` 表达 skill、tool set、system overlay、
model 与 loop policy 变更,并在 turn 边界原子应用。其中 tool set 变化会 reify 成
`Requirement{ kind: NeedReconfigRegistry }`,由 driver 端的 `ReconfigHandler` 用
`ToolRegistryResolver` 把新的 `ToolSetRef` 解析为 live registry 再换入;解析或版本校验
失败会分类返回且不部分应用(当前 turn 的 registry snapshot 保持恒定)。

## 5. 垂直功能:API-first,tool 只是 adapter

原始想法是"垂直功能只提供 api,通过 tool 暴露给 agent"。本稿把它**明确成 API-first**,
纠正"只通过 tool 暴露"的风险:

- **驱动者不止 LLM。** agent 调度、plan 的驱动者至少一半是**宿主程序(Rust)**。
  `DESIGN.md` Layer B 明确编排要能用普通 Rust 写。若只有 tool 入口,等于把编排权全
  交给模型,失去程序化编排,也失去可测试性(测一个功能要 mock 整个 LLM 往返)。
- **正确形状**:每个垂直功能先是**一等 Rust API**;`ToolRegistry` 里注册的 tool 是
  这些 API 的**薄封装**。两条路(程序 / 模型)共用同一套语义与校验。

```
VerticalFeature (Rust API, 一等)
     │
     ├── 宿主程序直接调用(编排、测试)
     └── ToolAdapter(薄封装)→ 注册进 ToolRegistry → 模型可调
```

## 6. 各垂直功能的形状与边界

### 6.1 skill / mcp
- **mcp tool** 本质是 `LocalTool` / `ProviderTool`,注册进 `ToolRegistry` 即可,不需要
  agent 层特殊对待。
- **skill** 是更高层的 bundle:`prompt 片段 + 一组 tool + 资源`。激活/停用 skill 是
  **reconfig**,改 `AgentState.active_skills` 与 `tool_registry`,**只在 turn 边界生效**
  (走 §4.2,不是 pivot 的 message 注入)。turn 内工具集恒定,规避了中途换工具集的一整
  类问题。

### 6.2 plan API —— 一等数据结构,"办公室里的计划板"
plan 概念上是一种**特化的 blackboard**,但它**极常用**,且需要"可变 task 状态 + 认领
原子性"这套 blackboard(§6.4,纯 append-only 聊天流)给不了的语义,因此**值得单列为
一等数据结构**,而不是做成 blackboard 之上的视图。定位是一块**"办公室里的计划板"**:
一张公共、可检查 / 可认领 / 可更新的任务单。

- **只存内容 + 状态**:task 列表、每个 task 的状态(待认领 / 已认领 / 进行中 / 完成 /
  阻塞)、认领者、依赖关系。每个 task 可声明 `depends_on: Vec<TaskId>`,表示它依赖
  一个或多个前置 task;依赖图必须引用已知 task,不得自依赖或形成环。可 serde。
- **更新与执行都由外部负责**:plan **自己不推进任何东西**。谁来做、跑多快由 agent
  loop / 宿主决定;plan 只如实记录"有哪些事、各是什么状态、谁认领了"。这直接排除了
  `DESIGN.md` 警告的"大而全编排引擎"——plan 里没有 executor。
- **认领(claim)需要原子性与依赖检查**:多个 agent 从板上认领 task,认领要用 CAS / 版本
  避免两个 agent 同时抢到同一 task;同时必须检查该 task 的所有 `depends_on` 前置均已
  `completed`。前置未完成时返回分类错误,且不得部分修改 owner/status/version。
- **提供 claim-first 简化入口**:`claim_first_available(owner, ...)` 按 plan 的创建/显示稳定顺序
  找到第一个未完成、未被认领且依赖全部完成的 task,并以同一套 CAS/原子规则认领。这个
  入口减少模型先 read 再选择 task 的往返和出错面;没有可认领 task 时返回 `NoAvailableItem`
  类错误,而不是认领被依赖阻塞的 task。
- **API 形状**:`create_plan` / `read` / `add_task(depends_on)` / `claim(task)` /
  `claim_first_available` / `update_status` 等,全是对"板上数据"的读写;**没有 `execute`**。

### 6.3 agent 调度 API —— 编排最小原语 + 安全护栏
- 原语:`spawn_agent` / `stop_agent` / `send` / `await result`(呼应 `DESIGN.md`
  Layer B "只给原语")。
- **安全护栏(tool 化时尤其重要)**:模型能 spawn/stop agent 时必须有
  - **深度上限**:防止无限递归 spawn;
  - **budget 继承**:子 agent 从父 `RunContext.budget` 派生,不能绕过总预算;
  - **cancellation 传播**:父的 `CancellationToken` cancel 时,所有子 agent 一并 cancel。
- 子 agent 必须挂在父的 `RunContext` 下,不能是游离进程。

### 6.4 blackboard —— agent 聊天群(append-only,无强制机制)
给多 agent 一个交互渠道。它的模型就是一个**"agent 聊天群"**:每个 agent 可以
**post message** / **check message**,**没有任何强制机制**——没有锁、没有认领、没有
CAS,谁读没读、读了做不做,blackboard 一概不管。需要状态与认领语义的场景走 plan
(§6.2),不要往 blackboard 上加约束。

- **append-only 消息流**:blackboard 是只追加的有序消息日志,不是可覆写的 KV。历史
  message 不可变(与 conversation 的 immutability 取向一致)。
- **一致性照搬 IM 群聊**:有序、单调、每个 agent 自己维护读游标(读到哪了),like 群聊
  里"未读消息"。不追求跨 agent 的强一致事务;群聊语义足够。
- **投递 best-effort**:blackboard 只是**辅助 / 参考**渠道,不在关键路径上,关键协调走
  plan 的认领(§6.2)。加之本机运行、丢消息概率几乎为零,投递做成 best-effort 即可,
  不引入确认 / 重传 / 恰好一次这类重机制。
- **命名空间**:多个群(topic / channel)隔离,避免不相干消息互相淹没。
- **与 conversation 分离**:blackboard 是 agent 间的交互媒介,**不是**任何 agent 的
  conversation 的一部分;别把会话内容和群聊消息混在一起。
- **post 的消息带发送者身份 + 时间戳**,便于 check 方过滤/归因。

## 7. 多 agent 编排(Layer B):只给原语

不发明编排引擎。把 agent 当**可 await 的任务**:`spawn` 一个 agent(带派生的
`RunContext`)、用 channel 传消息、`join` 收结果。委派 / pipeline / group 用普通 Rust
组合。理由:多 agent 拓扑发散,过早抽象必选错;先把 Layer A(单 agent loop)做扎实。

## 8. 对下层的新增/复用约束(本稿最重要的产出)

**复用**(已在 conversation 层落地,agent 层直接用):
- Turn / step `Boundary` 事件作为统一求值点(§2)。
- cancel 闭合裂缝、committed log 恒满足不变量(§3)。
- fork 出新 Conversation 承载多路径(§1.2)。

**新增需求**(agent 层向下层新提的,现均已落地):
1. **conversation 需要一个"边界注入入口"**:在 step 边界向 pending turn 追加
   `user` 消息(pivot 专用),注入后仍满足 role 合法 + tool 配对(§4.1)。skill 激活、
   tool set 变更和 system prompt 变更属于 reconfig,只在 turn 边界走配置路径生效
   (§4.2、§6.1),不复用 pivot 的 message 注入入口。当前 Conversation pending 层已通过
   `Conversation::inject_user_message` 提供该入口。
2. **机器可暂停/可恢复**:整台 sans-io 机器的状态可序列化(`LoopCursor` 已升格为
   `AgentMachine` 的可持久化状态,`StepOutcome` 亦全字段 serde,见 §1.3),配合
   conversation snapshot 一起落盘;审批、外部等待、中断续跑本质都是"停在某个
   requirement 上、序列化、恢复"。live 句柄(client/tool/interaction 后端)不进 serde,
   恢复时由 driver 重建。
3. **RunContext 载体**:budget / cancellation / trace 要能从 agent 贯穿到 tool 与子
   agent(§1.4、§6.3),由驱动机器的 scope 派生。

## 9. 待定问题(Open Questions)

- pivot 注入的消息 role 已定为 **`user`**(见 §4.1);来源用 `meta` 记录。剩余待验证:
  中段以 user 注入在带 thinking / 带 cache_control 的 provider 方言下是否有额外约束。
- skill / tool set / system prompt 变更(reconfig)已定为**只在 turn 边界生效**
  (见 §4.2、§6.1):turn 内工具集恒定,"引用旧工具集的调用"问题被设计消除,不再是待定项。
- blackboard 的一致性模型已定:**append-only 群聊 + IM 风格 + best-effort 投递**
  (见 §6.4)。剩余待定:读游标是否需持久化,留到首批多 agent 用例再定。
