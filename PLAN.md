# 实施计划：Effect 层清理(三刀重构)

> 本计划以 [`docs/effect-refine.md`](docs/effect-refine.md) 为唯一设计输入,并引用
> [`docs/agent-effect-model.md`](docs/agent-effect-model.md)(sans-io `step` / `HandlerScope` /
> `Pop` / `drain` / never-resume cancel 的计算模型)、
> [`docs/agent-layer.md`](docs/agent-layer.md)(machine / handler / driver 分层)。
>
> 上一轮「External Agent 接入」计划(Milestone 1–6)已完成并归档到
> [`docs/archive/2026-07-17-external-agent/`](docs/archive/2026-07-17-external-agent/)。更早的
> Client / Conversation / Agent Layer / Effect Migration / Testability / Complex-tests 记录在
> `docs/archive/2026-07-*` 下。逐任务要求见 [`TODO.md`](TODO.md)。

## 目标

在不改变 [`docs/agent-effect-model.md`](docs/agent-effect-model.md) 的计算模型、也不改任何对外运行时
语义的前提下,把当前实现里三处**结构性噪音**逐条止血,让
`src/agent/machine/default/mod.rs`(868 行)、`src/agent/machine/default/tools.rs`、
`src/agent/state/cursor.rs`、`src/agent/requirement.rs`、`src/agent/drive.rs`
读起来更接近它们背后那个干净的模型。

三刀(设计文档 §2–§4),按性价比与风险从低到高排序,一刀一个里程碑:

- **刀 (C)——给 `step` 内部一个 `Result` 层**:把 `DefaultAgentMachine` 里数十处
  `if let Err(..) return self.fail(..)` 塌缩成 `?`,只在 `step()` 最外层一处折叠成
  `LoopCursor::Error`。语义零变化、改动最局部。(Milestone 1)
- **刀 (B)——cursor 相位与 scratch 合一**:消灭游离在 cursor 之外、靠隐式约定与 cursor 相位对齐
  的两个非序列化 `Option`(`in_flight` / `pending_reconfig`),让「卡住状态」只有一个真相。
  采用设计文档 §3.3 的**落点 2**(不动序列化边界,风险最低)。(Milestone 2)
- **刀 (A)——用宏收敛 effect coproduct 的扇出**:用一份单一 effect 清单驱动的声明宏,生成
  `requirement.rs` 的三个 coproduct enum + `accepts` 与 `drive.rs` 的三处 handler 扇出,把
  「加一个 effect 要同步改 8 处」收敛为「清单里加一段」。(Milestone 3)

## 范围与非目标

**范围**:

- 刀 (C):在 `src/agent/machine/default/`(`mod.rs` + `tools.rs`)内引入 `StepError` 内部错误层,
  用 `?` 传播,`step()` 最外层折叠。
- 刀 (B):在 `src/agent/machine/default/mod.rs` 引入统一的非序列化 `TurnScratch` enum,替换
  `in_flight` + `pending_reconfig` 两个 `Option` 字段,并显式化一处
  `rebuild_scratch_from_state()`。
- 刀 (A):新增 `define_effects!` 声明宏(`src/agent/requirement.rs` 或独立 `macros.rs`),从单一
  清单生成 effect coproduct 与 handler 扇出,先与手写版并存做等价性断言,再删手写版。

**非目标**(与设计文档 §6 一致):

- 不改 `AgentMachine::step` 的 sans-io 契约、`HandlerScope` / `Pop` / `drain` 的 pop 路由、
  never-resume cancel 的语义。任一刀落地后,对外行为**逐字节不变**。
- 不引入 multi-shot 续延、编译期 effect row(row polymorphism)、nightly coroutine。刀 (A) 只
  「收敛」运行时 coproduct 的样板,不「消除」coproduct 本身。
- 不动 `NestedMachine` / external-agent 的机器实现;三刀都聚焦「单机器 + 扇出点」的可读性。
- 刀 (B) 只做设计文档 §3.3 的**落点 2**(cursor 保持纯地址、全序列化不变;scratch 收敛成一个
  非序列化 enum)。**落点 1**(把 scratch `#[serde(skip)]` 内联进 cursor 相位、需 snapshot 版本
  bump + 迁移)明确不在本计划内,作为将来可选后续。
- 刀 (A) 不由宏生成 handler trait 定义本身(签名差异大,损害 rustdoc),也不生成机器内的 resume
  分派(第 8 处,依赖具体机器 cursor 相位语义)。宏只覆盖设计文档 §4.1 表格的第 1–7 处。

## 里程碑总览

| 里程碑 | 对应刀 | 主题 | 主要文件 | 语义变化 | 序列化风险 |
|---|---|---|---|---|---|
| Milestone 1 | 刀 (C) | `step` 内部 `Result` 层 | `machine/default/mod.rs`、`machine/default/tools.rs` | 无 | 无 |
| Milestone 2 | 刀 (B) | cursor 相位与 scratch 合一(落点 2) | `machine/default/mod.rs` | 无 | 无 |
| Milestone 3 | 刀 (A) | effect coproduct 扇出宏 | `requirement.rs`、`drive.rs`(+ 可选 `macros.rs`) | 无(要求等价性测试) | 无 |

三刀之间无强依赖,可独立落地、独立验证;本计划按性价比顺序增量推进。每个里程碑结尾有独立 review
任务,确保本阶段的正确性与完整性。

## 现有地基(实现锚点)

以下是各任务的精确接入点(均已核实):

- **机器实现**:`src/agent/machine/default/mod.rs`(868 行)。
  - `DefaultAgentMachine` 结构体:`mod.rs:129`;`in_flight: Option<InFlight>` 字段 `mod.rs:156`,
    `pending_reconfig: Option<PendingReconfig>` 字段 `mod.rs:161`(顶注 `mod.rs:33-39` 与字段注
    释均写明它们是**非序列化 mid-turn scratch**)。
  - `PendingReconfig` enum:`mod.rs:86`(变体 `BeginTurn { user, application }` /
    `Commit { step_id, application, records }`)。
  - `AbandonKind` enum:`mod.rs:66`(`Llm` / `Tool` / `Reconfig`)。
  - fallible 私有方法:`begin_user_turn`@299、`open_user_turn`@353、`inject_pivot`@384、
    `block_on_llm`@435、`resume`@468、`resume_llm`@494、`fold_llm_response`@525、
    `commit_text_turn`@565、`finalize_text_commit`@591、`emit_reconfig_effect`@636、
    `resume_reconfig`@669、`abandon`@733、`abandon_llm_step`@768、`abandon_reconfig`@787、
    `finish_cancel`@803。
  - 收尾模板:`fail()`@825、`fail_with_notifications()`@832(discard pending →
    `cancel_pending(DiscardTurn)` → 清 `in_flight` → 迁 `LoopCursor::Error` → quiescent 空 outcome)。
  - `impl AgentMachine for DefaultAgentMachine`:`step()`@852(match 4 个 `StepInput` 分支)、
    `cursor()`@861。
  - `mod.rs` 现有 `self.fail*` 调用 **33 处**、`if let Err` **10 处**。
- **工具相位**:`src/agent/machine/default/tools.rs`(672 行)。
  - `InFlight` 结构体:`tools.rs:68`(`assistant_message_id` / `steps_started` / `tools: Option<ToolPhase>`)。
  - `ToolPhase`@92、`ToolSlot`@109。
  - fallible 方法:`begin_tool_phase`@129、`advance_tool_phase`@193、`emit_tool_batch`@220、
    `emit_approval`@286、`resume_tool`@335、`resume_approval`@400、`finish_tool_phase`@501、
    `abandon_tool_phase`@626。
  - `tools.rs` 现有 `self.fail*` 调用 **32 处**;其中 `emit_tool_batch` / `emit_approval` /
    `finish_tool_phase` 走 `fail_with_notifications`(**带副产品 notification 的失败路径**)。
- **cursor**:`src/agent/state/cursor.rs`(700 行)。`LoopCursor` enum(8 变体)`cursor.rs:129`,
  `#[serde(tag = "state", content = "data", rename_all = "snake_case")]`,全部 derive
  `Serialize`/`Deserialize`;`LoopCursorKind`@350;`ErrorCursor`@660(仅 `message: String`,
  **无独立 `CursorError` 类型**)。`transition_cursor` 返回 `Result<(), AgentStateError>`
  (`state.rs:242`)。
- **effect coproduct**:`src/agent/requirement.rs`(1047 行,含测试)。
  - `RequirementKindTag`@132(6 变体:`Llm`/`Tool`/`Interaction`/`Subagent`/`Reconfig`/`ExternalSession`),
    `Display`@147。
  - `RequirementKind`@366(6 变体,`#[serde(rename_all = "snake_case")]`,可持久化,derive serde),
    `tag()`@423,`accepts()`@448(含 `NeedInteraction` 的响应校验特例)。
  - `RequirementResult`@471(6 变体,**运行时半、不 derive serde**),`tag()`@497。
  - 现有测试:`accepts_matrix_pairs_each_kind_with_its_result_only`@825、
    `every_requirement_kind_round_trips`@801、`external_requirement_accepts_only_external_result`@987
    等;`ALL_TAGS: [RequirementKindTag; 6]`@757。
- **handler 扇出**:`src/agent/drive.rs`(1713 行,含测试)。
  - `HandlerScope` trait@114,各访问器默认 `None`:`llm`@116、`tool`@121、`interaction`@127、
    `subagent`@132、`reconfig`@138、`external`@144。
  - 各 handler trait:`LlmHandler`@154、`ToolHandler`@169、`InteractionHandler`@186、
    `SubagentHandler`@203、`ReconfigHandler`@232、`ExternalSessionHandler`@256。
  - `scope_handles`@533(tag→访问器 `is_some`)、`fulfill_with_scope`@548(kind→handler 调用,
    `NeedSubagent` 返回 `None`)、`resolve_requirement`@600(Subagent 特例:构造
    `ScopePop`@350 走串行路径,§4.3 的 `needs_outer`)、`fulfill_batch`@668(`expect("scope_handles
    confirmed a handler for this family")`@683)。
- **错误类型**:`AgentStateError`(`src/agent/state.rs:483`,`Conversation` 变体 `#[from]
  ConversationError`)、`ConversationError`(`src/conversation/error.rs:1255`)、`ToolRuntimeError`
  (`src/agent/tool.rs`)、`RequirementError`(`src/agent/requirement.rs:535`)。
- **宏先例**:`src/agent/id.rs:22` 已有 `macro_rules! define_id!`,是刀 (A) 声明宏的现成风格参照。
- **测试布局**:`src/agent/machine/default/tests/`(`mod.rs` / `reconfig.rs` / `tools.rs`)、
  `src/agent/state/tests.rs`(cursor 序列化往返:`streaming_step_cursor_round_trips_requirement_binding`
  @458、`awaiting_tool_cursor_round_trips_requirement_ids`@510、
  `agent_state_serde_round_trips_through_conversation_snapshot`@155 等)。

## 验证策略

每个任务结尾定义完整验证条件。默认完整验证序列(与归档计划一致):

1. `cargo fmt --all -- --check`
2. 聚焦测试:`cargo test`(仅本任务新增/相关用例,给出精确过滤名)
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`(credential-gated 集成测试保持 ignored)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

三刀的核心验收标准都是**对外行为逐字节不变**:失败仍落在 `LoopCursor::Error` 且 quiescent;现有
`machine/default/tests/`、`state/tests.rs`、`requirement.rs` / `drive.rs` 的测试**无需修改断言即全绿**
(除非任务明确选择顺带丰富诊断,那会**新增**而非**修改**断言)。

## 风险与开放问题

- **刀 (C) 的 notifications-on-fail**:`tools.rs` 有若干「在同一 step 内已产出 notification 后再撞到
  失败」的路径(`emit_tool_batch` / `emit_approval` / `finish_tool_phase` 的 step limit 分支)。设计
  文档 §2.2 给出两种处理,本计划取推荐的后者:**只把纯失败路径改成 `?`,带副产品的失败仍走显式
  `fail_with_notifications`**,不强求统一(改动更小、罕见路径不冒险)。
- **刀 (B) 落点选择**:严格走**落点 2**,不动 `LoopCursor` 的 serde 形状,规避已持久化暂停态失配的
  风险。`rebuild_scratch_from_state()` 的正确性靠新增 restore 往返测试兜底。
- **刀 (A) 的两处特例**:①`NeedSubagent` 是唯一走串行 `resolve_requirement` + `ScopePop` 的
  effect,宏必须支持 `needs_outer` 标记而非假设所有 effect 同构;②`accepts` 里 `NeedInteraction`
  有响应校验特例,`RequirementResult` 不 derive serde 而 `RequirementKind` derive serde——宏必须允许
  逐变体差异化(自定义 `accepts` 分支、按半区区分 derive)。若声明宏表达力不足,退化为独立 proc-macro
  crate(设计文档 §4.4)。
- **刀 (A) 的时机**:effect 变体集合已随 external-agent 里程碑落定为 6 个且稳定(设计文档 §4.4 的前置
  条件已满足),故本计划将其纳入。宏落地必须「先并存 + 等价性断言,再删手写版」。
