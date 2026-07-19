# Agent Effect Model —— sans-io 状态机与 effect-handler 编排

> **状态:已落地。** 本文记录的 sans-io + effect-handler 计算模型已实现并成为 agent 层的
> 当前形状,主文档 [`docs/agent-layer.md`](agent-layer.md) §1.3 / §3 / §4 已按本模型改写。
> 本文所述"若被采纳会翻转"的旧 loop 契约(push / 自驱 `feed → AgentEvent stream`)已被
> 移除。实现落位:
>
> - sans-io `step` 契约与 `StepInput` / `StepOutcome`:`src/agent/machine/mod.rs`;
>   默认 LLM/tool 机:`src/agent/machine/default/`;嵌套机器树:`src/agent/machine/nested.rs`。
> - `Notification` / `AgentInput`:`src/agent/event.rs`;`Requirement` / `RequirementKind`
>   ({`NeedLlm`,`NeedTool`,`NeedInteraction`,`NeedSubagent`,`NeedReconfigRegistry`})/
>   `RequirementId` / `AgentPath` 寻址:`src/agent/requirement.rs`;交互:`src/agent/interaction.rs`。
> - effect handler、`HandlerScope`、`drain` / `drive_turn` 参考 driver 与 `Pop` 路由:
>   `src/agent/drive.rs`;subagent 派生与作用域强制:`src/agent/drive/subagent.rs`。
> - never-resume cancel 接 `Conversation::cancel_pending`:`src/agent/context/cancel.rs` +
>   `src/agent/drive.rs`;`RunContext` 派生:`src/agent/context.rs`。
>
> 本模型复用而非重造 Conversation 层已落地的 committed log + pending + `Boundary` +
> `cancel_pending` 裂缝闭合 + `fork_at` 共享前缀能力。分阶段迁移与接口形状见
> [`docs/agent-effect-migration.md`](agent-effect-migration.md)。

## 0. 一句话

**把 agent loop 从"自己驱动的 async 引擎"改成"不做 IO、只请求 IO 的 sans-io 状态
机";把 cancel / pivot / 审批 / 子 agent / 预算 / 交互统一成"agent perform 一个
effect(requirement),由带作用域的 handler 兑现或逐级向上 pop"。** 这是
algebraic effects + delimited continuation 的精确落地,不是打比方(见 §5)。

## 1. 动机:现有模型是"自动化投影",目标应用是"重交互 + 兼顾自动化"

目标应用是一个通用 agent app(类 Claude Desktop / Codex app):多 agent、不同任务用
不同 model、**人长期在 loop 里**,同时要能无人值守跑自动化任务。

现有 `agent-layer.md` §1.3 的 `feed → AgentEvent stream` 模型里,loop **自己持有**
`LlmClient` 与 `ToolExecutor`、**自己驱动**多步推进(见 `TODO.md` M2-2/M2-3)。这是
"自动化优先"的形状:一个 run = 一个任务,cancel/budget/trace 生命周期重合,loop 闷头
跑到底。一旦"人一直在场"且"任意深度的子 agent 都可能要跟人交互",这个形状会在几个
地方被拉开:

- 子 agent 的交互请求没有路径冒泡到顶层唯一挂着人的地方(cancel 能向下传,交互无法
  向上路由)。
- "谁驱动一个 agent、是否并行"被写死在 loop 内部,每个前端 / 每个 headless 任务都要
  重写事件循环,且容易在 cancel/pivot/交互的边角写错。
- attended(有人值守)与 unattended(无人值守)是两套代码,而不是同一张图的两种跑法。

本文提出的模型用**一个机制**同时解决这些:sans-io + effect handler 作用域。

## 2. 核心:agent 是 sans-io 状态机

### 2.1 turn = 一次输入到输出流

一个 agent 是一个状态机。**一次输入到本次输入处理完成,是一个 turn**。它与教科书
状态机的唯一区别:一次 turn 的 output 不是单个值,而是一个 **event stream**;但这个
stream 逻辑上仍有一个确定的"最终 output message"和确定的"turn 结束"。

```
feed(input) -> EventStream        // 一次 turn = 一个 event stream
```

- **重交互消费**:逐个 `next()` 处理事件(动态更新 UI、把请求交给人)。
- **要结果消费**:`drain` 到底,只取最终 output message(见 §4)。

两者不是两个模型,是**同一个 stream 的两种消费姿势**。token 级 delta 只是 stream 里
粒度最细的一类事件:想看就逐个消费,drain 就跳过。streaming 因此不再是"纯不纯"的
架构问题。

### 2.2 sans-io:状态机不做 IO,只请求 IO

关键取向:**状态机自己不调用 LLM、不执行 tool、不 spawn 子进程。** 它只在需要时
**perform 一个 requirement**(= 一个 effect),然后停下等外部把结果喂回来。

```
状态机核心(纯、可序列化、无 async):
    step(state, input) -> (state', outputs, requirements)

driver(外部、async、持有真实资源):
    兑现 requirements → 把结果作为下一个 input 喂回 step
```

这样做的直接收益是一批横切机制**塌缩成同一套词汇**:

| 原来是独立机制 | 在本模型里 |
|---|---|
| cancel token 向状态机内传播 | driver 停止喂食 / never-resume 一个 requirement(§6) |
| pivot 软转向 | 在两次 step 之间多喂一个 input |
| 审批 / 子 agent 问用户 | 一个本层 driver 兑现不了、需向上 pop 的 requirement(§4) |
| backpressure | 天然:没喂结果就无法 step,`&mut state` 即"一次一个推进" |
| 父等子 / 父子并行 | 不存在这个问题:并行发生在 requirement 兑现层,不在 step 层(§3) |

### 2.3 event stream 里有两类性质相反的事件

drain 一个 stream 时必须区分:

```
通知(notification)  —— "我告诉你发生了什么"
    LLM delta、tool 开始/结束、子 agent spawn、step 边界……
    → drain 可安全跳过,不看也不影响推进。

请求(requirement)   —— "我卡住了,必须有人回我才能继续"
    need_llm / need_tool / need_interaction / need_subagent……
    → drain 不能跳过。跳过 = stream 永不 advance = 死锁。
```

这是 drain 必须携带 handler 的根本原因(§4):忽略通知无害;忽略 requirement 会挂死。

### 2.4 step 的错误出口:软拒绝与硬失败(M4-4)

`step` 不能返回 `Result`,错误出口只有两种形状,按"错在谁"分开:

- **软拒绝(driver 协议违规)**:输入在当前位置不适用 —— stale / 未知 requirement id
  的 `Resume`/`Abandon`、不合法边界的 pivot、turn 进行中又喂一条 `UserMessage`、嵌套
  树里路由不到任何节点的 id。机器**状态逐位不变**(cursor、pending turn、outstanding
  requirement 全部保留),输入经 `StepOutcome.rejected: Option<StepRejectReason>` 回告
  (`UnknownRequirement` / `IllegalPivotBoundary` / `TurnInProgress`),driver 检查原因
  后可继续驱动 —— 一次手滑不再销毁整个在途 turn。
- **硬失败(运行时故障)**:payload kind 不匹配、内部不一致、conversation / state 操作
  失败。机器经 `cancel_pending(DiscardTurn)` 清理后停在 `LoopCursor::Error`。清理自身
  的失败不再被吞:折叠进错误消息;转移表含 `(Done|Error) → Error` 边,error 停靠对
  所有 cursor kind 全可达,诊断不会丢。

同契约下的两条终态修正:

- **步数上限是正常终态**:达到 `max_steps` 走 `LoopCursor::Done(StepLimitReached)`
  而非 Error;tool phase 已 drain(无 open call),pending turn 以 `ResumeTurn` 空闭包
  保全(已冻结的 tool 结果不丢)。facade 把该终态结构化映射为
  `FacadeError::LoopLimitExceeded`(不再字符串匹配)。
- **reconfig abandon 保全文本**:during-turn reconfig park 在 `ReadyToCommit`
  (`ResumeTurn` 在该相位非法),abandon 时改为 `commit_pending` 提交已冻结的文本响应;
  被放弃的 reconfiguration 留在队列,下个 turn 边界重发。

## 3. Requirement:被 reify 的 effect

一个 requirement 是状态机对外界的一次请求。它**必须可寻址**,因为兑现结果要精确送回
那个卡住的 step 点(这就是 delimited continuation 的句柄,见 §5)。

```
Requirement {
    id:          RequirementId,     // 本次请求的唯一标识
    origin_path: AgentPath,         // 发出它的 agent 在 hierarchy 中的路径(回程路由用)
    kind:        RequirementKind,   // 见下
}

RequirementKind =
  | NeedLlm { request }             // 要一次 LLM 调用
  | NeedTool { call }               // 要执行一个 tool
  | NeedInteraction { prompt }      // 要跟"用户"交互(审批/开放问题/选项/澄清)
  | NeedSubagent { spec_ref, brief, result_schema? }   // 要派生并驱动一个子 agent
```

要点:

- `NeedInteraction` **泛化了**现有 `AwaitingApproval`(后者只建模 yes/no 审批)。它承载
  审批、开放问题、选项选择、澄清请求等带类型的交互;请求与响应都是结构化的。
- `NeedSubagent` 是**唯一会加深作用域链**的 requirement(§7)。深度护栏挂在它上。
- 兑现结果作为 `input = (RequirementId, Result)` 喂回;driver 维护一张"未决
  requirement"登记表按 `id` 路由(复杂度没消失,它搬到了这个可测试的纯数据点)。

**这里的 requirement 是抽象效果,不是具体 API 形状**;它对应 §5 的 effect operation。

## 4. Handler 与 drain:effect handler 的作用域

### 4.1 每层 drain 是一个 handler scope

驱动一个 agent = 用一组 **requirement handler** 去 drain 它的 event stream。每类
requirement 一个 handler:

- `NeedLlm`         → 一个 client(自动调)
- `NeedTool`        → 一个 tool executor(自动执行)
- `NeedInteraction` → 一个 interaction backend(弹 UI 等人 / 按 policy 立即应答)
- `NeedSubagent`    → 一个 spawner(派生子 agent 并**再开一层 drain** 递归驱动它,§7)

**"运行模式"= 顶层这组 handler 挂了什么后端**:
- `NeedInteraction` 挂真人 UI → attended(交互式 app)。
- `NeedInteraction` 挂 policy(auto-approve / auto-deny / 默认值 / fail) → unattended(headless)。
- 混合:默认 policy 自动过,遇高危 tool 才升级到人(若人在线)。

没有单独的"运行模式开关",没有单独的 InteractionChannel 对象;**运行模式就是 handler
集合的差异**。

### 4.2 pop:本层无 handler 则逐级向上

```
本层 drain 有对应 handler → 就地兑现,resume 回 step 点。
                            该 requirement 对上层不可见(被本 scope 消化)。
本层 drain 无对应 handler → pop:把 requirement(带 id + origin_path)交给外层 drain。
                            被 pop 穿过的每层只透传,不解释。
```

`drain_tree` 的**缺省 handler = pop**。于是 sub-tree 的 drain 不会有奇怪行为:它兜不了
的 requirement 自然向上冒泡,直到某层接住,或到顶报错。

### 4.3 顶层必须 total

作用域链必须有尽头:

```
中间层 drain 无 handler → pop(安全)。
顶层 drain(session root,上面再无 drain)无 handler → 分类报错(unhandled requirement)。
```

因此:**顶层 drain 必须是 total 的(覆盖所有 requirement 变体);中间层可以 partial。**
一个 headless 批处理程序的 root **必须**给 `NeedInteraction` 挂一个 policy,否则任何
深层 agent 一问用户,整个 turn 就 error —— 这是对的:它把"我以为全自动、结果某个
subagent 卡在等人"从神秘死锁变成**启动即可测出的显式契约**。

护栏建议:drain 遇到没有注册 handler、且已到顶的 requirement,**立即返回分类 error**,
绝不静默跳过或挂起。这与本库"能力缺失是结构性的、不变量违反要分类报错"的哲学一致。

### 4.4 scoping 是免费的:handler 是动态作用域

handler 查找沿**动态** drain 栈(谁 drain 谁),不是**词法**定义位置。直接后果:

> 同一个 subagent spec,它的 requirement 被谁兜底,取决于**运行时谁在 drain 它**,
> 而不是它被定义 / spawn 在哪。

典型场景(一个 UI 交互 agent 需要跑一个纯 headless 子任务):

```
UI 交互 agent(顶层 drain)
  handlers = { llm, tool, interaction→真人UI, subagent }
       │
       └── spawn 一个 headless subagent,用内层 drain 驱动
             handlers = { llm, tool }          ← 故意不挂 interaction
                  │
                  └── subagent 内部冒出 need_interaction
                        本层无 handler → pop
                        └── 冒到 UI agent 层 → 有 interaction handler → 交给真人
```

headless subagent **不知道也不需要知道**头顶有没有真人;它照常 perform
`need_interaction`,由驱动它的那层 drain 的 handler 集合决定命运。**同一个 spec、同一段
代码,挂了 interaction handler 的 drain 下是 attended,没挂的 drain 下自动 headless,
无需给 subagent 任何配置。** 这就是 §1 说的"attended/unattended 是同一张图的两种跑
法"的机制落点。

代价(动态作用域的固有负担):你没法只看一个 subagent 的定义就断定它的 requirement
会怎么被处理,必须知道运行时的 drain 栈。对策见 §8(trace 记 resolved-by-scope)。

## 5. 与 algebraic effects + delimited continuation 的精确对应

这不是类比,是精确对应;凡该理论成立的结论在此都成立,该理论的陷阱在此都会遇到。

```
algebraic effects 理论              本模型
──────────────────────────────────────────────────────
effect operation                    requirement(NeedLlm / NeedTool / …)
perform / raise                     agent step 吐出一个 requirement
handler                             某类 requirement 的兑现器
handler scope(with-handler)         一层 drain 及其 handler 集合
handler 沿动态作用域向上查找          requirement 本层无则 pop 给上层
resume / continuation               兑现结果沿回程路径送回原 step 点,机器继续
delimited(resume 回到 perform 点)   子 agent 从卡住处精确恢复,父不受影响
top-level default handler set       session root 的 total handler
one-shot continuation               每个 requirement 恰好被兑现一次或永不(§6)
```

"精确"的关键在 **delimited** 这一条:pop 上去的 requirement 被兑现后,continuation
不是"从头重来",而是**回到那个 perform 点、带着结果继续**。`requirement.id +
origin_path` 就是被 reify 出来的 delimited continuation 句柄。

## 6. Continuation 是线性的:one-shot resume 或 never-resume

### 6.1 不做 multishot

effect 理论允许 continuation 被 resume 零次 / 一次 / **多次**(多次 = 从同一卡点分叉出
多条未来,用于回溯 / 搜索)。**本模型明确禁止 multishot**,理由是硬约束而非取舍:

- multishot 要求 continuation 可复制,即卡点之下**整棵子树状态可克隆**,包括**已经发出
  的 side effect**(已调过的 LLM、已写过的 blackboard、已扣的预算)。这些 side effect
  不可回滚,multishot 会重放它们 —— 语义灾难。
- 本库的数据模型**不是纯 immutable**(blackboard、plan、budget 消耗都是可变副作用),
  multishot 在这里根本不 sound,不是"现在没空做"而是"永远不 sound"。
- 实现 sound 的 continuation 捕获 / 复制是语言运行时级工程。**我们做 agent lib,不做
  Koka**;投入产出比荒谬。

### 6.2 多路径走 fork,而且 fork 语义更对

多路径探索**一律走 `Conversation::fork_at` → 新 Agent 承载**(呼应 `agent-layer.md`
§1.2 "多路径靠 fork,不靠一个 agent 多会话")。fork 不是 multishot 的"穷人版模拟",
它在语义上就是 agent 想要的那个东西:

> multishot = 从卡点**重新执行**同一段未来(会重放 side effect)。
> fork      = **共享到分叉点为止的已确定历史,然后各走各的**(side effect 不重演,各自
>             新 identity,O(1) 共享 immutable 前缀)。

agent 探索多路径要的从来是后者。**所以 multishot 在这个领域反而是错的工具,fork 是对
的原语。** 不留 multishot 的口子,连"以后也许"都不留。

### 6.3 cancel = never-resume handler

cancel 不需要单独机制,它是一种 handler 行为。但要**精确**:不是 "zero-shot"。

> zero-shot 暗示 continuation 从未被 reify、effect 被拦在 perform 之前。而真实发生的
> 是:requirement **已经 perform**(卡住的子树状态真实存在、且可序列化),handler
> **收到了**它,只是**选择不 resume** —— 那个回程路径永远不被回灌。

所以精确表述:

> **cancel = 一个 never-resume handler:continuation 已 reify,handler 选择不回灌
> 结果,并负责让被丢弃 continuation 底下的 Conversation 走 cancel disposition 闭合到
> 一致态。**

这个用词直接指向正确实现:never-resume **不是"什么都没发生"**,而是"有一个已知的、
挂起的子树,我们决定丢弃它"。被丢弃的 continuation 底下可能挂着 committed 的 pending
turn、半开的 tool 配对(悬空 tool_use)。never-resume 必须触发那个 conversation 的
[`Conversation::cancel_pending`](conversation-core.md) —— 补合成 `Cancelled` tool
result 或丢弃 pending。**"zero-shot 什么都没发生"会漏掉这步清理;"never-resume 有个
已知挂起态要收尾"会记得它。** 这也正是"cancel 后 conversation 仍可 feed"这条硬性验收
的实现基础:never-resume 是**受控丢弃 + 闭合**,不是撒手不管。

## 7. Hierarchy:嵌套的 handler scope

### 7.1 agent + subagents = 嵌套状态机

`agent + 它的 subagents` 是一个 agent hierarchy;hierarchy 树中**每一个 subtree 都满足
§2 的约束**(有确定的 turn 边界与最终 output)。

落地取向:hierarchy 是**一台嵌套机器**(父机器 state 里*包含*子机器),而非"driver
持有的一组兄弟机器"。于是:

- 对 root 做一次 `feed`,`step` 递归推进整棵树到**静止(quiescent)**:每个节点要么产出
  中间 output、要么卡在 requirement 上。这一批来自树上任意位置的 outstanding
  requirements 被聚合交给 driver。
- **"hierarchy 聚合 interface"和"整 session 快照"直接白送**:整棵树是一台可序列化
  机器,`LoopCursor` 升格为"整台状态机的可序列化状态"。
- **并行发生在兑现层,不在 step 层**:driver 可并发兑现"子 B 要 LLM、子 C 要 tool、父
  要 tool"这一批 requirement,按完成顺序喂回。**父子天然并发,不需要"父等子还是父子
  并行"这个概念** —— 上一版设计里的那个难题在本模型里不存在。

### 7.2 subagent handler 是唯一会加深作用域链的 handler

`NeedSubagent` 的 handler = 派生一个子 agent 并**再开一层 drain** 驱动它;这个子 agent
会 perform `NeedLlm` / `NeedTool` / 甚至再 `NeedSubagent`。于是作用域链是真实递归嵌套,
深度 = hierarchy 深度。

- 其它 handler(`NeedLlm` 就是调个 client)不加深链;**只有 subagent handler 递归**。
- 因此 `agent-layer.md` §6.3 的"深度上限"护栏 = "作用域链最大深度",**理论直接告诉你
  它该挂在哪:在 subagent handler 里、每加深一层检查深度**,而不是在别处。
- **RunContext 从 ambient 派生**:coordinator 通过 tool 化的 `NeedSubagent` 派生子
  agent 时,模型只提供 data(spec ref、brief、result schema);子 agent 的 cancel /
  budget / trace / interaction 由 **subagent handler 从"当前正在 drain 的 scope"隐式
  派生**,模型不可见、不可绕过。深度上限、预算继承、cancel 传播全部由 handler 强制。

### 7.3 pop 从外层起,防即时环

一个 handler 在兑现 requirement 时可以自己 perform 别的 requirement(常态:subagent
handler 内部就在 perform 子 agent 的一堆 requirement)。为避免"handler 处理自己
perform 的同类 effect"造成即时环:

> **pop 查找从"发出 requirement 的 scope 的外层"开始,跳过自身 scope。** 一个 handler
> 在兑现时 perform 的同类 requirement 不会回到它自己。

### 7.4 结构在库、驱动在调用者

明确的库 / app 分界(呼应几轮讨论的结论):

```
库(结构 + 机制,保证不变量):
  - sans-io 状态机(step / requirement / 线性 continuation)
  - agent hierarchy(受检嵌套结构:派生、cancel 子树、budget 聚合、trace parentage)
  - requirement 类型 + pop 路由规则 + 回程路由记账
  - RunContext(贯穿:cancel↓ / budget↕ / trace↓ / interaction 经 pop↑)

调用者(驱动 + I/O 端点,编码策略):
  - 谁 drain 一个 agent、是否并行(driver 用普通 tokio 编排:join / select / 串行)
  - 每层挂什么 handler(= 能力集 + 运行模式)
  - interaction backend 顶端接什么(真人 UI / policy)
  - Session:用"库的 hierarchy + handler scope"组装出的编排容器(见 §9 与 agent-layer.md §7)
```

**Session 归 app**(它是编排策略、形状因产品而异),但它持有的**贯穿机制归库**
(分层 RunContext、pop 路由、cancel 传播);Session 是"用库原语组装出的普通 Rust
结构"的最典型例子。

## 8. Observability:动态作用域必须记 resolved-by-scope

动态作用域的负担(§4.4)要求 trace 补两项,否则出问题无从追:

- 每个 requirement 在 trace 中记录**它最终被哪一层 scope 的 handler 兑现**
  (resolved-at-scope)。
- 并记录**兑现结果是 resume 还是 never-resume** —— never-resume 是一个真实发生、
  会影响下层 Conversation 状态的事件(§6.3),不是 non-event,必须留痕。

这与 `agent-layer.md` §1.4 / `TODO.md` M5-2 的 trace tree 对齐:run → step →
llm/tool/sub-agent 一棵可重建的树,此处只是补上"requirement 的作用域归属与恢复处置"。

两条边界规则(H-STATE-4 收口):

- **pivot 同 id 重发**:pivot 会以**同一个 requirement id** 重新 perform 未决的
  requirement,因此同一个 id 会被 settle(并记录)多次。除首次外,后续 settle 记录在
  派生 node id `<id>#attempt-N`(N 从 2 递增)下,trace 上的每次 settle 仍然完整可查。
- **trace 记录是 best-effort**:trace 是纯观测侧设施,记录失败(含重复 id、缺失父
  节点)绝不中止 drain;无法在 trace 上留痕的失败直接丢弃该节点,驱动照常推进。

## 9. 收敛规则(单条)

> 每层 drain 是一个 **handler scope**,带一组 requirement handler。agent step
> **perform** requirement(带 `id + origin_path`,是被 reify 的 delimited
> continuation);查找沿**动态 drain 栈**从**外层**起(跳过自身,防即时环);本层有
> handler 则就地兑现并 **resume**(**线性 continuation**);本层无则 **pop** 给外层
> (`drain_tree` 缺省 handler = pop);**cancel = never-resume handler**(continuation
> 已 reify,选择不回灌,并负责触发被弃子树的 `Conversation::cancel_pending` 闭合);
> **多路径 = `fork_at`,不提供 multishot**;**session root scope 必须 total**,pop 到顶
> 仍无 handler 即分类报错;每个 requirement 在 trace 记录**被哪层 scope 兑现、是
> resume 还是 never-resume**。

这一条把前面所有横切关注点 —— backpressure、cancel、pivot、审批、interaction 向上
路由、attended/unattended、headless scoping、hierarchy 聚合、父子并行 —— **全部收编成
同一个机制**,无需额外设施。

## 10. 对现有计划的影响(仅陈述,不在本文改动)

本模型若被采纳,与现有文档 / 计划的差异:

- **`agent-layer.md` §1.3**:`feed → AgentEvent stream(loop 自持 client/executor、内部
  驱动)` → `step → (outputs, requirements)(外部驱动、pull 模型)`。这是 **push → pull
  的翻转**,不是微调。
- **`agent-layer.md` §3 / §4**:审批 / pivot / cancel 从"三种并列的边界机制"→"同一套
  requirement + handler 的三种表现"(pivot = 多喂 input;审批 = `NeedInteraction`;
  cancel = never-resume)。
- **`TODO.md` M2-2 / M2-3**:"loop driver 接收 `LlmClient`""定义 `ToolExecutor` 并行
  执行"是 push 契约,需重述为"状态机吐 `NeedLlm`/`NeedTool` requirement,由外部 handler
  兑现"。
- **`TODO.md` M3**:pivot queue / reconfig / approval responder / cancel glue 需按
  requirement + handler scope 重新表述。
- **基本不受影响**:M1 数据层(`AgentSpec`/`AgentState`),`RunContext` 的 handle 分类,
  serde/runtime 分界,Conversation 层全部不变量;`LoopCursor` 反而**更本质** —— 它几乎
  *就是*整台机器的可序列化状态。
- **DESIGN.md §3 "一律 async_trait"**:核心状态机不是 async(它是纯 `step`);async
  只活在 driver 与 handler(client/tool 兑现)侧。这不矛盾,但改写了那条已定技术方向的
  适用范围。

## 11. 尚未定死的问题

- **requirement 的具体类型形状**:本文只定抽象效果(§3)。`NeedInteraction` 的结构化
  请求 / 响应 schema、`NeedSubagent` 的 result schema handoff,留到具体实现时定。
- **一次 step 到静止的批量 requirement 顺序**:聚合交给 driver 时是否需要稳定顺序 /
  优先级(如 interaction 优先于 llm),留待首批多 agent 用例验证。
- **read cursor / interaction 请求的持久化粒度**:整台机器可序列化(§7.1)已覆盖恢复,
  但未决 requirement 登记表在跨进程恢复时的重建细节待定。
- **token delta 的 tee 与 drain 的交互**:driver 从 `NeedLlm` 兑现里 tee 出 token 流给
  UI 的具体接口,与"drain 跳过通知"的边界待实现时明确。
