# Effect 层清理 —— 把 sans-io 机器的三笔"税"收敛掉

> **状态:提案。** 本文不改变 [`docs/agent-effect-model.md`](agent-effect-model.md) 的
> 计算模型(sans-io `step` + `HandlerScope`/`Pop`/`drain` + never-resume cancel),也不
> 改任何对外运行时语义。它只针对当前实现里三处**结构性噪音**给出局部重构方案,目标是
> 让 `src/agent/machine/default/mod.rs`(867 行)、`src/agent/drive.rs` 与
> `src/agent/state/cursor.rs` 读起来更接近它们背后那个干净的模型。
>
> 三处噪音都不是设计错误,而是"用 Rust future/state-machine 模拟 one-shot delimited
> continuation + 运行时 coproduct effect row"这条路本身自带的税(见 §1)。本文把这些税
> 逐条止血。
>
> 实现锚点:
> - effect coproduct:`src/agent/requirement.rs`(`RequirementKind` / `RequirementResult` /
>   `RequirementKindTag` / `accepts`)。
> - handler 扇出:`src/agent/drive.rs`(`HandlerScope` 访问器、`scope_handles`、
>   `fulfill_with_scope`)。
> - 机器状态:`src/agent/machine/default/mod.rs`(`fail()` 模式、`in_flight` /
>   `pending_reconfig` scratch)、`src/agent/state/cursor.rs`(`LoopCursor`)。

## 0. 一句话

现有实现忠实落地了 sans-io + scoped-handler 模型;剩下的"乱"集中在三笔可点名的税:
**(A) effect row 是运行时 coproduct,加一个 effect 要同步改 8 处**;**(B) `LoopCursor`
一身二职,真正的 mid-turn scratch 游离在它管不到的非序列化 `Option` 字段里,靠隐式约定
维系**;**(C) `step` 不能返回 `Result`,于是几十处 `if let Err(..) return self.fail(..)`
把纯函数手动反脱糖回了 CPS。** 本文按性价比给出三刀:先 (C)(最局部)、再 (B)(消灭
隐式约定)、最后 (A)(需要宏,收益最大)。三刀都不动架构,可独立落地、独立验证。

## 1. 为什么会有这三笔税(定位,不是背景)

我们选的路是:用 `async`/state-machine 帮我们省掉 CPS 变换,把 agent loop 写成"不做 IO、
只 reify IO"的纯状态机,再用动态作用域的 handler 栈兑现 effect。这条路的收益是真实的
(可暂停、可恢复、可测试、pop 到外层),但它有两个先天约束会直接变成代码噪音:

1. **Rust 没有 row polymorphism。** "一个计算需要哪些 effect"无法在类型层表达为可增减
   的 row,只能退化成一个**运行时 coproduct**(`RequirementKind` 枚举)。coproduct 的每
   个消费点(handler 访问器、分派 match、对齐校验)都必须穷举所有变体 —— 这是税 (A)。

2. **`step` 是纯同步、多返回值(notifications + requirements + quiescent),不能是
   `async fn` 也不宜返回 `Result`。** 于是 async/`?` 本来替我们脱糖掉的错误传播,在
   `step` 内部又得手搓回来 —— 这是税 (C)。而 one-shot 续延的"卡在哪 + 卡住时的局部
   变量"这两件事,当前被拆到了 `LoopCursor`(序列化)和 `Option<InFlight>`(非序列化)
   两个地方 —— 这是税 (B)。

结论:这三刀是在**已知天花板内**做减法,不是推翻重来。特别是 (A) 只能"收敛",不能
"消除"(消除需要编译期 row type,那要回到 nightly coroutine + 类型级 coproduct,代价是
类型推断脆、报错噪音大,与本库"运行时可寻址、可序列化"的取向冲突)。

---

## 2. 刀 (C):给 `step` 内部一个 `Result` 层,把 `fail()` 换成 `?`

**性价比最高、改动最局部、语义零变化。建议第一刀。**

### 2.1 现状

`AgentMachine::step` 签名不能返回 `Result`(它要吐 `StepOutcome`),所以
`DefaultAgentMachine` 里每个 fallible 调用都手写成:

```rust
// src/agent/machine/default/mod.rs 中出现数十次
if let Err(error) = self.state.conversation_mut().begin_turn(
    user.turn_id(), user.message_id(), user.message().clone(),
) {
    return self.fail(format!("conversation operation failed: {error}"));
}
```

`fail()` / `fail_with_notifications()`(`default/mod.rs:825`、`:832`)做的事是固定的:
若有 pending 就 `cancel_pending(DiscardTurn)`、清 `in_flight`、把 cursor 迁到
`LoopCursor::Error`、返回 quiescent 空 outcome。这套模板在 `begin_user_turn` /
`open_user_turn` / `inject_pivot` / `block_on_llm` / `fold_llm_response` /
`commit_text_turn` / `finalize_text_commit` / `emit_reconfig_effect` /
`resume_*` 里被反复展开。

### 2.2 目标形状

内部逻辑改为返回 `Result<StepOutcome, StepError>`,错误传播用 `?`;**只在
`AgentMachine::step` 最外层一处**把 `Err` 折成 `self.fail(..)` cursor:

```rust
/// 机器内部一步计算的失败:携带分类信息与人读消息。
/// 只在 step() 最外层被折叠成 LoopCursor::Error,不对外暴露。
enum StepError {
    Conversation(ConversationError),
    State(AgentStateError),
    Cursor(CursorError),
    RequirementId(String),
    /// 语义违例(如 resume 落在错误的 cursor 上)。
    Protocol(String),
}

impl DefaultAgentMachine {
    fn begin_user_turn(&mut self, user: AgentUserInput) -> Result<StepOutcome, StepError> {
        // ...
        self.state.conversation_mut().begin_turn(
            user.turn_id(), user.message_id(), user.message().clone(),
        )?;                              // ← 数十处 if-let-Err 塌缩成一个 ?
        self.in_flight = Some(InFlight::new(user.assistant_message_id()));
        self.block_on_llm(user.step_id(), Vec::new())
    }
}

impl AgentMachine for DefaultAgentMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        let result = match input {
            StepInput::External(AgentInput::UserMessage(user)) => self.begin_user_turn(user),
            StepInput::External(AgentInput::Pivot(pivot))       => self.inject_pivot(pivot),
            StepInput::Resume(resolution)                       => self.resume(resolution),
            StepInput::Abandon(id)                              => self.abandon(id),
        };
        // 唯一的错误折叠点:把 StepError 变成 Error cursor + quiescent 空 outcome。
        match result {
            Ok(outcome) => outcome,
            Err(error) => self.fail_from(error),      // = 旧 fail() 的收尾语义
        }
    }
}
```

要点:
- `From<ConversationError>` / `From<AgentStateError>` / … for `StepError`,让 `?` 直接用。
- `fail_from(StepError)` 复用旧 `fail()` 的收尾(discard pending、清 scratch、迁
  `Error` cursor)。**分类信息**(是 Conversation 错还是协议违例)可以顺带记进
  `ErrorCursor`,比现在只有一条格式化字符串更可诊断。
- **保留 notifications-on-fail 能力**:有些失败点在同一 step 内已产出 notification(如
  tool step boundary 之后撞到 step limit,见 `fail_with_notifications`)。做法是让这些
  路径返回 `StepError` 的同时,把已产出的 notifications 挂在错误上(`StepError` 带一个
  `Vec<Notification>` 字段),`fail_from` 折叠时一并带出。或者更简单:这类"带副产品的
  失败"仍走显式 `return self.fail_with_notifications(..)`,只把**纯失败**路径改成 `?`。
  二者取一,建议后者(改动更小,罕见路径不强求统一)。

### 2.3 影响面与验证

- 只动 `src/agent/machine/default/`(`mod.rs` + `tools.rs`),不动 trait、不动 `drive`、
  不动 cursor 数据形状。
- 对外行为**逐字节不变**:失败仍落在 `LoopCursor::Error`,仍 quiescent。现有
  `machine/default/tests/` 应全绿,无需改断言(除非你选择顺带丰富 `ErrorCursor` 的分类,
  那会新增而非修改断言)。
- 预期:`mod.rs` 减少数十行防御性 `if let Err`,`fold_llm_response` /
  `finalize_text_commit` 这类多步方法从"每步一个 if-let"变成线性 `?` 链。

---

## 3. 刀 (B):cursor 相位与 scratch 合一,让"卡住状态"只有一个真相

**消灭最累人的隐式约定。建议第二刀。**

### 3.1 现状:一个续延地址被拆成两半,靠约定粘合

`LoopCursor`(`state/cursor.rs`,8 个变体)同时承担两个职责:

- **(a) 续延地址**:machine 卡在哪个 `RequirementId`(`CursorRequirement` /
  `ToolWaitRequirements`),用于序列化/恢复。
- **(b) turn 内相位机**:`Idle → StreamingStep → AwaitingTool/AwaitingApproval →
  AwaitingReconfig → Done/Error/CancelRecovery`。

而真正的 mid-turn scratch 在 cursor **之外**、且**非序列化**:

```rust
// src/agent/machine/default/mod.rs:156,161
in_flight: Option<InFlight>,               // 当前 turn 的 assistant msg id / llm step 计数 / tool phase
pending_reconfig: Option<PendingReconfig>, // 仅当 cursor == AwaitingReconfig 时 Some
```

两者的一致性靠**隐式约定**维系:"cursor 是 `StreamingStep` ⇒ `in_flight` 必 `Some`"、
"cursor 是 `AwaitingReconfig` ⇒ `pending_reconfig` 必 `Some`"。代码里到处是校验这条约定
的防御分支:

```rust
// inject_pivot:先确认 cursor 相位,再摸 scratch,两者可能不一致
let LoopCursor::StreamingStep(cursor) = self.state.loop_cursor() else {
    return self.fail(format!("pivot injection requires a streaming step boundary, but cursor is `{kind:?}`"));
};
// resume_reconfig:cursor 已是 AwaitingReconfig,但 scratch 还得单独 take + 校验
let Some(pending) = self.pending_reconfig.take() else {
    return self.fail("reconfig resume with no deferred reconfiguration in flight");
};
```

这是 `default/mod.rs` 里读起来最累的部分:每条 resume 路径都要先 re-match cursor 决定
分支,再去另一处摸 scratch,还得处理"万一它俩对不上"的不可能分支。

### 3.2 目标形状:scratch 内联进对应的 cursor 相位

让每个"卡住"的相位**直接携带**它那一相的 scratch,消灭游离的 `Option`:

```rust
// 概念形状(字段名示意)。序列化边界见 3.3。
enum Phase {
    Idle,
    Streaming { step: StepId, req: RequirementId, in_flight: InFlight },
    AwaitingTool { step: StepId, reqs: ToolWaitRequirements, in_flight: InFlight },
    AwaitingApproval { step: StepId, req: RequirementId, in_flight: InFlight },
    AwaitingReconfig { req: RequirementId, pending: PendingReconfig },
    Done(LoopDoneReason),
    Error(ErrorCursor),
}
```

于是 3.1 的两段防御坍塌成"match 到相位即拿到 scratch,不可能不一致":

```rust
fn inject_pivot(&mut self, pivot: PivotMessage) -> Result<StepOutcome, StepError> {
    let Phase::Streaming { req, in_flight, .. } = &self.phase else {
        return Err(StepError::Protocol(format!(
            "pivot injection requires a streaming step boundary, but phase is `{:?}`",
            self.phase.kind()
        )));
    };
    // req 与 in_flight 天然在手,无需再 take / 再校验一致性
    ...
}
```

### 3.3 关键约束:序列化边界必须重画

这是本刀唯一有难度的地方,不能糊弄:

- 当前 `LoopCursor` **整体可序列化**(`state/cursor.rs` 全 `Serialize`/`Deserialize`),
  用于"暂停机器 → 换进程恢复 → 从 cursor 重建 pending-requirement registry"。
- 而 `InFlight` / `PendingReconfig` 是**有意不序列化的 mid-turn scratch**
  (`default/mod.rs` 顶注明确写了这点):跨进程恢复时,一个卡在 turn 中途的机器是从
  **持久的 Conversation pending + 队列**重新驱动出这些 scratch 的,而不是反序列化它们。

所以合一后不能简单地"整个 `Phase` 都 `Serialize`"。两个可选落点:

- **落点 1(推荐):`Phase` 拆成"可序列化续延地址" + "非序列化 scratch"两个投影,但物理
  上放在同一个 enum 里,用 serde 属性区分。** 即续延地址字段(`StepId` / `RequirementId`
  / `ToolWaitRequirements`)照常序列化;scratch 字段(`InFlight` / `PendingReconfig`)标
  `#[serde(skip)]` 且要求 `Default` 或恢复时重建。反序列化得到一个"地址在、scratch 空"
  的相位,由恢复路径(见 §3.4)把 scratch 重新灌进去。**cursor 仍是唯一真相,只是它的
  scratch 半区在恢复时才填充。**
- **落点 2:保持 `LoopCursor`(纯地址、全序列化)不变,另立一个非序列化的
  `TurnScratch` enum 收拢 `in_flight` + `pending_reconfig`,并用类型保证"scratch 相位与
  cursor 相位同构"。** 改动更小(不碰序列化),但没有完全消灭"两个东西要对齐",只是把
  对齐从"两个 `Option`"收敛成"一个 enum vs 一个 enum",且可以给一个
  `debug_assert!(scratch.matches(cursor))` 兜底。折中方案。

建议先按**落点 2** 落地(风险低、可先享受"scratch 变成一个 enum、match 即得"的收益),
若之后确认恢复路径能干净重建 scratch,再推进到落点 1 彻底合一。

### 3.4 恢复路径(restore)必须显式处理 scratch 重建

无论落点 1/2,都要有一处明确的"从持久态重建 mid-turn scratch"的代码:

- 从 `Conversation.pending()` 读出 in-flight 的 assistant message id / 已闭合的 tool
  batch,重建 `InFlight`。
- 从 `queued_reconfigs` 重新 `plan_reconfig`,若 cursor 停在 `AwaitingReconfig` 则重建
  `PendingReconfig`(注意:`PendingReconfig::Commit` 里的 `records` 是可从 application 再
  渲染的,不必持久化)。

**这段逻辑现在其实是隐式散布的**(靠 `begin_user_turn` 里"若 pending 存在则 discard"这类
补丁式判断)。本刀应把它显式化为一个 `rebuild_scratch_from_state()`,既服务 restore,也让
"cursor + scratch 一致"成为一个可测试的不变量。

### 3.5 影响面与验证

- 主要动 `default/mod.rs`(scratch 字段与所有 resume/abandon 分支)与 `state/cursor.rs`
  (若走落点 1)。
- 风险点是**序列化兼容**:若 `LoopCursor` 的 serde 形状变了,已持久化的暂停态会失配。
  落点 2 完全规避此风险;落点 1 需要一次 snapshot 版本号 bump + 迁移测试。
- 验证:`state/tests.rs`(cursor 序列化往返)、`machine/default/tests/`(resume/abandon/
  reconfig/pivot 全路径)、以及新增一条 `rebuild_scratch_from_state` 的 restore 往返测试。

---

## 4. 刀 (A):用宏收敛 effect coproduct 的 8 处扇出

**收益最大(直接决定"加一个 effect 有多痛"),但需要写宏,风险与工作量最高。建议第三刀,
且可等 external-agent 那批新 effect 落地、变体集合稳定后再做。**

### 4.1 现状:一个 effect = 分散在 8 处的样板

新增一个 effect(如正在做的 `NeedExternalSession`,见 [`PLAN.md`](../PLAN.md))要同步改:

| # | 位置 | 改什么 |
|---|---|---|
| 1 | `requirement.rs` `RequirementKind` | 新增请求变体 |
| 2 | `requirement.rs` `RequirementResult` | 新增结果变体 |
| 3 | `requirement.rs` `RequirementKindTag` | 新增 tag 变体 |
| 4 | `requirement.rs` `RequirementKind::accepts` | 新增 kind↔result 对齐分支 |
| 5 | `drive.rs` `HandlerScope::<family>()` | 新增访问器(默认 `None`) |
| 6 | `drive.rs` `scope_handles` | 新增 tag→访问器 match 臂 |
| 7 | `drive.rs` `fulfill_with_scope` | 新增 kind→handler 调用 match 臂 |
| 8 | `machine` resume 分派 | 新增 cursor→result 分派(按机器而定) |

8 处彼此靠约定对齐("result family 必须匹配 requirement kind"),编译器只在
`accepts`/`validate` 运行时兜底,而不是类型级保证。漏改任一处要么 panic(`expect`
"scope_handles confirmed a handler")要么运行时 `AgentError::Other`。这就是运行时
coproduct 的核心税,也是"感觉乱"的最大来源。

### 4.2 目标形状:单一 effect 清单驱动的声明宏

用一份**单一 effect 清单**生成第 1–7 处的全部样板。清单只有一处,但第 1–4 处(coproduct)
要落在 `requirement.rs`、第 5–7 处(handler 扇出)要落在 `drive.rs`,而一个 `macro_rules!`
只在**一个模块**里展开。为此清单以**回调宏**的形式存在:`with_effect_manifest!` 持有清单本
身,把整份清单转发给调用方点名的生成器宏;两个生成器各生成一半:

```rust
// 概念示意:唯一的事实来源(src/agent/effect_manifest.rs)
macro_rules! with_effect_manifest {
    ($generator:ident) => { $generator! {
        Llm {
            tag_name: "llm",
            kind: NeedLlm { request: ChatRequest, mode: LlmStepMode },
            result: Result<Response, ClientError>,
            handler: LlmHandler,
            accessor: llm,
            fulfill: (request, *mode),   // 交给 handler 的实参形状(值/引用)
        }
        Tool { .. }
        Interaction { .. accepts_check: request.accepts_response, }  // 唯一带后置校验
        Subagent    { .. needs_outer: true, }  // 唯一走 resolve_requirement 串行路径
        Reconfig    { .. }
        ExternalSession { .. }
    } };
}

// requirement.rs:唯一一行,生成第 1–4 处
with_effect_manifest!(define_effect_coproduct);
// drive.rs:唯一一行,生成第 5–7 处
with_effect_manifest!(define_effect_fan_out);
```

`define_effect_coproduct` 产出 `RequirementKind` / `RequirementResult` /
`RequirementKindTag` 三个 enum、`RequirementKindTag::Display`、各 `tag()`、
`RequirementKind::accepts`;`define_effect_fan_out` 产出 `HandlerScope` 的访问器默认实现、
`scope_handles`、`fulfill_with_scope`。清单里的裸类型名(`ChatRequest`、`LlmHandler` …)在
**各生成器的展开点**解析,所以 coproduct 的字段/结果类型在 `requirement.rs` 解析、扇出的
handler trait 在 `drive.rs` 解析,两个模块互不需要对方的 import。**加一个 effect 从"改 8 处"
变成"清单里加一段"**(完整 diff 见 [§7 附录](#7-附录加一个-effect-的完整-diff))。

### 4.3 边界与不做什么

- **`Subagent` 是特例,不能被宏一刀切平。** 它是唯一"加深 scope 链"的 effect:
  `fulfill_with_scope` 对它返回 `None`,改由 `resolve_requirement` 串行处理并构造
  `ScopePop`(`drive.rs:600` 一带)。宏必须支持给某个 effect 打
  `needs_outer` 标记,让它生成"走串行路径"而非"走并发本地路径"的代码,而不是假设所有
  effect 同构。
- **handler trait 本身(`LlmHandler::fulfill` 的具体签名)不建议由宏生成。** 各 handler
  的参数形状差异大(有的要 `&mut dyn Pop`,有的要 `mode`),宏化 trait 定义会让签名藏进
  宏里、损害可读性与 rustdoc。宏只负责"把已定义的 handler 接进 coproduct 的扇出点",
  trait 定义仍手写。
- **不追求编译期 row polymorphism。** 那需要类型级 coproduct(`frunk` 式)+ 可能的
  nightly coroutine,与本库"运行时可寻址、可序列化、报错友好"的取向冲突,明确不做(见
  §1)。本刀是"把运行时 coproduct 的样板集中化",不是"换成编译期 row"。
- **机器内 resume 分派(第 8 处)大概率仍手写。** 它依赖具体机器的 cursor 相位语义
  (哪个 cursor 接哪种 result),不同 `AgentMachine` 实现各异,不宜由通用宏生成。宏覆盖
  第 1–7 处即可把痛点砍掉七成。

### 4.4 影响面与验证

- 动 `requirement.rs` 与 `drive.rs`,以及新增一个 `src/agent/effect_manifest.rs`(私有模块,
  内含 `with_effect_manifest!` 清单 + 两个生成器宏;声明宏表达力足够,未引入独立 proc-macro
  crate)。
- **先验证等价性再删旧码**(已按 M3-1 → M3-2 → M3-3 三步落地):M3-1 让宏产物取 `*Gen` 临时名
  与手写版**并存**;M3-2 用 `#[test]` 断言两者的 serde 输出与 `accepts`/扇出行为逐字段一致;
  M3-3 删除手写版、让宏产物接管正式名,并把等价性测试改为对宏产物本身的行为测试。
- 验证:`requirement.rs` 现有测试、`drive.rs` 的 dispatch 测试全绿;`RUSTDOCFLAGS="-D
  warnings" cargo doc` 通过(宏生成项的 rustdoc 需可编译)。
- **时机建议**:等 external-agent 里程碑把 `NeedExternalSession` 等新变体落定、effect
  集合稳定后再做。在变体集合仍在增长时引入宏,会边写宏边改宏。

---

## 5. 落地顺序与验证矩阵

| 刀 | 主要文件 | 语义变化 | 序列化风险 | 建议时机 |
|---|---|---|---|---|
| (C) `?` 层 | `machine/default/` | 无 | 无 | **立即**,独立 PR |
| (B) cursor/scratch 合一 | `machine/default/`、`state/cursor.rs` | 无(落点 2)/ 需迁移(落点 1) | 落点 2 无 / 落点 1 有 | (C) 之后 |
| (A) effect 宏 | `requirement.rs`、`drive.rs` | 无(要求等价性测试) | 无 | external-agent 变体稳定后 |

每刀单独一个 PR,单独走 [`PLAN.md`](../PLAN.md) 的默认验证序列(fmt / 聚焦测试 / clippy /
全量测试 / doc / `git diff --check`)。三刀之间无强依赖,可按上表顺序增量推进;若只做一刀,
做 (C)——它性价比最高、风险最低,做完即可直观看到 `default/mod.rs` 的噪音下降多少。

## 6. 明确的非目标

- 不改 `AgentMachine::step` 的 sans-io 契约、`HandlerScope`/`Pop`/`drain` 的 pop 路由、
  never-resume cancel 的语义。
- 不引入 multi-shot 续延、编译期 effect row、nightly coroutine —— 这些是这条路的已知天花板
  之外的东西,与本库取向冲突(见 [`docs/agent-effect-model.md`](agent-effect-model.md) §5)。
- 不在本轮动 `NestedMachine` / external-agent 的机器实现;三刀都聚焦"单机器 + 扇出点"的
  可读性,nested 层自然受益于同样的改动,但不作为本文验收目标。

## 7. 附录:加一个 effect 的完整 diff

刀 (A) 落地后,新增一个 effect 只需在 `src/agent/effect_manifest.rs` 的
`with_effect_manifest!` 清单里**加一段 stanza**,其余第 1–7 处由两个生成器宏自动展开。下面以
一个虚构的 `Timer` effect(机器请求"睡到某个时刻",由驱动侧计时器兑现)为例,展示完整 diff。

**唯一需要手写的改动**——在清单里加一段(第 8 处「机器内 resume 分派」按各机器语义单独接线,
不在本清单覆盖范围):

```diff
 // src/agent/effect_manifest.rs, macro_rules! with_effect_manifest
         ExternalSession {
             tag_name: "external_session",
             kind: NeedExternalSession { request: ExternalSessionRequest },
             result: Box<ExternalSessionResult>,
             handler: ExternalSessionHandler,
             accessor: external,
             fulfill: (request),
         }
+        Timer {
+            tag_name: "timer",
+            kind: NeedTimer { deadline: Instant },
+            result: Result<(), TimerError>,
+            handler: TimerHandler,
+            accessor: timer,
+            fulfill: (*deadline),
+        }
```

加上这段后,两个生成器宏自动补齐:

- **coproduct(`requirement.rs`,第 1–4 处)**:`RequirementKindTag::Timer` 及其 `Display`
  (`"timer"`)、`RequirementKind::NeedTimer { deadline }`、`RequirementResult::Timer(Result<(),
  TimerError>)`、两个 `tag()` 分支、以及 `accepts` 里的 `Timer ⟷ Timer` 对齐分支。
- **handler 扇出(`drive.rs`,第 5–7 处)**:`HandlerScope::timer()`(默认 `None`)、
  `scope_handles` 的 `Timer => scope.timer().is_some()`、`fulfill_with_scope` 的
  `NeedTimer { deadline } => Some(scope.timer()?.fulfill(*deadline, ctx).await)`。

**清单之外仅需的配套**(不是"改扇出点",而是提供被扇出点引用的类型/trait):

1. 让 `NeedTimer` 的字段/结果类型在 `requirement.rs` 可见(`Instant`、`TimerError`)——与手写
   时代要为任何新变体引类型一样。
2. 在 `drive.rs` 定义 `TimerHandler` trait(它的 `fulfill` 具体签名有意保持手写,见 §4.3)。
3. 若某台 `AgentMachine` 会**发起**或**消费** `NeedTimer`,在它的 cursor 相位里接线 resume
   分派(第 8 处,按机器语义各自实现)。

对比刀 (A) 之前:同样一个 effect 要在 `requirement.rs` 的三个 enum + `accepts`、`drive.rs` 的
`HandlerScope` + `scope_handles` + `fulfill_with_scope` 共 7 处手工对齐,漏改任意一处只在运行期
或 review 时才暴露。清单化后,7 处对齐塌缩成一段 stanza,"漏改一处"在结构上不再可能。
