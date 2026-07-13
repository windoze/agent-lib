# TODO：Agent Effect Model 迁移任务列表

> 依据 [`PLAN.md`](PLAN.md)、规范性设计
> [`docs/agent-effect-model.md`](docs/agent-effect-model.md) 与接口方案
> [`docs/agent-effect-migration.md`](docs/agent-effect-migration.md)。任务按真实依赖顺序编号;
> coding agent 每次只执行首个标题带 `[TODO]` 的任务,完成后把该标题的 `[TODO]` 改为
> `[DONE]` 并在任务末补"完成记录"。被本列表取代的旧 Agent Layer 任务(M1–M3 已落地)与
> Conversation Core 记录分别归档在
> [`docs/archive/2026-07-13-agent-layer/`](docs/archive/2026-07-13-agent-layer/) 与
> [`docs/archive/2026-07-13-conversation/`](docs/archive/2026-07-13-conversation/)。

通用约束:sans-io `step` **绝不 await**、不触碰 client/tool/进程;requirement 必须可寻址
(`id + origin`);drain 本层无 handler 则 pop、顶层无 handler 分类报错、绝不静默跳过;
cancel = never-resume 且必须触发被弃子树 `Conversation::cancel_pending`;多路径一律
`Conversation::fork_at`,不引入 multishot / continuation 复制;不得重新实现或绕开 Conversation
的 committed log、pending、tool pairing、`Boundary`、restore 不变量;id/时间由调用方注入;
Agent 只持有一个活动 `Conversation`;每个测试用例必须在 1 分钟内完成。每项完整验证按
“format → 严格 clippy → 聚焦测试 → 全量测试 → rustdoc → diff check”执行,全量测试最长 30
分钟。**迁移原则**:M1–M3 期间保留现有 `DefaultAgentLoop` 可编译可测,新旧路径并存;M4 起
才删旧机制。

---

## Milestone 1 — 类型骨架（迁移文档阶段 0，不改行为）

### [DONE] M1-1 `Requirement` 与回程寻址类型

**前置依赖**:无(在现有 `src/agent/` 上新增文件)。

**上下文**:迁移文档 §3.2/§3.3/§3.4。requirement 是被 reify 的 effect,必须可寻址,兑现
结果要精确送回卡住的 step 点。payload 全部复用现有类型:`client::ChatRequest`/`Response`/
`ClientError`、`conversation::ToolCallId`、`model::tool::{ToolCall, ToolResponse}`、
`agent::tool::ToolRuntimeError`、`agent::LlmStepMode`(现在在 `loop_driver/default.rs`)。
本任务只定义数据类型,不接线到任何驱动逻辑。

**做什么**:

- 新建 `src/agent/requirement.rs` 并从 `agent/mod.rs` 导出。
- 定义 `RequirementId`(不透明 id newtype,不自己生成)与供给 trait `RequirementIds`
  (`fn next_requirement_id(&self, kind_tag: RequirementKindTag) -> Result<RequirementId, ...>`),
  风格对齐现有 `agent::tool::ToolExecutionIds`(库不造 id、由 host 供给)。
- 定义 `AgentPath`(根到当前节点的路径,`Vec<AgentSlot>`,根为空)与 `AgentSlot` newtype;
  阶段 0 单机器下恒为空路径,但类型先就位,避免阶段 4 改签名。
- 定义 `Requirement { id: RequirementId, origin: AgentPath, kind: RequirementKind }` 与
  `RequirementKind`:`NeedLlm { request: ChatRequest, mode: LlmStepMode }`、
  `NeedTool { call_id: ToolCallId, call: ToolCall }`、`NeedInteraction { request: Interaction }`
  (`Interaction` 由 M1-3 提供,本任务先用 `todo!` 占位类型别名或在 M1-3 补齐该 variant)、
  `NeedSubagent { spec_ref, brief, result_schema: Option<serde_json::Value> }`(subagent 相关
  字段类型阶段 0 可用最小占位 newtype,标注"阶段 4 细化")。
- 定义 `RequirementResult`(`Llm(Result<Response, ClientError>)`、`Tool(Result<ToolResponse,
  ToolRuntimeError>)`、`Interaction(InteractionResponse)`、`Subagent(...)` 占位)与
  `RequirementResolution { id: RequirementId, result: RequirementResult }`。
- 提供一个受检的类型对齐校验函数
  `RequirementKind::accepts(&self, result: &RequirementResult) -> Result<(), RequirementError>`
  (NeedLlm 只接受 Llm result,以此类推),失败返回分类错误。
- 为所有可持久化数据类型 derive `serde`;`RequirementResult` 里含 `Result<_, ClientError>`
  等运行时错误的部分若不可 serde,则拆出"可持久化的 requirement 描述"与"运行时结果",
  只对前者要求 serde(在 rustdoc 里写清边界)。

**验证**:

- 聚焦测试:`Requirement`/`RequirementKind`/`RequirementId`/`AgentPath` serde round-trip;
  `accepts` 对每种 kind×result 组合的接受/拒绝矩阵;`RequirementIds` 供给失败返回分类错误。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 requirement 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

**完成记录**:

- 新建 `src/agent/requirement.rs` 并从 `agent/mod.rs` 导出;不接线任何驱动逻辑,不改现有行为。
- `RequirementId`:UUID 不透明 newtype,transparent serde,host 供给(库不生成)。
- `RequirementIds` 供给 trait(`next_requirement_id(kind_tag)`,对齐 `ToolExecutionIds` 风格)
  + `NoRequirementIds` 默认实现(始终返回分类错误 `IdUnavailable`)。
- `AgentPath`(`Vec<AgentSlot>`,transparent serde)+ `AgentSlot`(`u32` newtype);阶段 0 恒空
  根路径,类型先就位以避免阶段 4 改签名(`root`/`child`/`push`/`slots`/`iter` 等辅助函数齐全)。
- `Requirement { id, origin: AgentPath, kind }` + `RequirementKind` 四变体
  `NeedLlm{request: ChatRequest, mode: LlmStepMode}`、`NeedTool{call_id: ToolCallId, call: ToolCall}`、
  `NeedInteraction{request: Interaction}`、`NeedSubagent{spec_ref, brief, result_schema}`。payload
  复用现有类型。占位类型 `Interaction`/`InteractionResponse`(标注 M1-3 替换)、`AgentSpecRef`/
  `SubagentOutput`(标注 M5 细化)。
- `RequirementResult` 四变体 + `RequirementResolution`,作为运行时半(含 `ClientError`/
  `ToolRuntimeError`/`AgentError`),按规格**不要求 serde**;serde 边界在模块 rustdoc 写清
  (persistable 描述 vs runtime 结果)。
- `RequirementKindTag`(Display)统一驱动类型对齐;`RequirementKind::accepts` 按 tag 校验,
  失败返回分类 `RequirementError::ResultKindMismatch`;另补 `Requirement::accepts_resolution`
  先校验 id 再校验类型(`IdMismatch`/`ResultKindMismatch`)。
- 为使 `Requirement` 可序列化,给 `agent::LlmStepMode` 增加 `Serialize/Deserialize`
  (`snake_case`),纯派生、非行为变更。
- 聚焦测试(10 个,全绿):`Requirement`/`RequirementKind`/`RequirementId`/`AgentPath` serde
  round-trip、非根 origin 保序、`accepts` 4×4 接受/拒绝矩阵、`accepts_resolution` id/类型双检、
  `NoRequirementIds` 及 host 供给成功/耗尽的分类错误、占位类型 round-trip、tag Display 稳定。
- 验证:`cargo fmt --all` 通过;`cargo clippy --all-targets -- -D warnings` 通过;
  `cargo test --lib agent::requirement`(10 passed);`cargo test --all --all-targets`
  (lib 367 passed,其余 target 全绿,无 failed);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  通过;`git diff --check` 干净。

### [DONE] M1-2 `Notification`:从 `AgentEvent` 拆出通知部分

**前置依赖**:M1-1。

**上下文**:迁移文档 §3.1。现有 `src/agent/event.rs` 的 `AgentEvent` 混装通知与请求:
`Llm(StreamEvent)`、`StepBoundary(StepBoundary)`、`ToolCallStarted(ToolCallStarted)`、
`ToolCallFinished(ToolCallFinished)`、`AwaitingApproval(ApprovalRequest)`、`Done(AgentOutcome)`。
其中前四个是纯通知,`AwaitingApproval` 是请求(→ M1-3 NeedInteraction),`Done` 不再是流事件。
阶段 0 **不删** `AgentEvent`(`DefaultAgentLoop` 仍用它),只新增并存的 `Notification`。

**做什么**:

- 在 `src/agent/event.rs` 定义 `Notification` enum:`Llm(StreamEvent)`、
  `StepBoundary(StepBoundary)`、`ToolCallStarted(ToolCallStarted)`、
  `ToolCallFinished(ToolCallFinished)`;payload 复用现有 struct,不重定义。
- 提供 `impl From<Notification> for AgentEvent`(便于并存期把通知桥接到旧流),以及
  文档说明 `AgentEvent::AwaitingApproval` → 未来 `Requirement::NeedInteraction`、
  `AgentEvent::Done` → 未来 `StepOutcome.quiescent + cursor` 的对应关系。
- 从 `agent/mod.rs` 导出 `Notification`。

**验证**:

- 聚焦测试:`Notification` serde round-trip;`From<Notification> for AgentEvent` 对四个变体
  的映射正确;确认 `Notification` 不含 approval/done 变体(编译期结构断言或显式测试)。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 event 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

**完成记录**:

- 在 `src/agent/event.rs` 新增并存的 `Notification` enum（四个纯通知变体
  `Llm(StreamEvent)`/`StepBoundary(StepBoundary)`/`ToolCallStarted(ToolCallStarted)`/
  `ToolCallFinished(ToolCallFinished)`），payload 全部复用现有 struct,未重定义;serde 与
  `AgentEvent` 同形(`tag="type", content="data", snake_case`),故通知子集与对应 `AgentEvent`
  变体 wire 兼容。
- 未删 `AgentEvent`,`DefaultAgentLoop` 仍走旧流,不改任何现有行为。
- `impl From<Notification> for AgentEvent`:变体一一映射、payload 保留;因被排除的
  `AwaitingApproval`(请求→未来 `Requirement::NeedInteraction`)与 `Done`(终态→未来
  `StepOutcome.quiescent + cursor`)不是通知,刻意不提供反向 `From<AgentEvent> for Notification`。
  该对应关系写入 `Notification` 与模块级 rustdoc。
- 从 `agent/mod.rs` 导出 `Notification`;更新模块 rustdoc 说明 `AgentEvent`(旧合并流)与
  `Notification`(通知子集)并存关系。
- 聚焦测试(2 个,全绿):`notifications_round_trip_and_bridge_to_agent_events`(四变体 serde
  round-trip + `From` 映射正确 + 与桥接后 `AgentEvent` 的 JSON 编码相等即 wire 兼容);
  `notification_excludes_approval_and_done_variants`(approval/done 的 tagged 编码可解码为
  `AgentEvent` 但解码为 `Notification` 失败,显式钉住通知变体集合)。
- 验证:`cargo fmt --all` 通过;`cargo clippy --all-targets -- -D warnings` 通过;
  `cargo test --lib agent::event`(9 passed);`cargo test --all --all-targets`(lib 369 passed,
  其余 target 全绿,无 failed);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过;
  `git diff --check` 干净。

### [DONE] M1-3 `Interaction`:泛化 approval

**前置依赖**:M1-2。

**上下文**:迁移文档 §4。现有 `src/agent/approval.rs` 提供 `ApprovalRequirement`(yes/no)、
`ApprovalResponse`、`ApprovalDecision`、`ApprovalError`、`ToolApprovalPolicy`、`NoApprovalPolicy`。
新模型把 yes/no 审批泛化成 interaction 的一个子类型,承载审批 / 开放问题 / 选项选择。
阶段 0 **保留并 re-export** 所有旧 approval 类型,只新增 `Interaction*` 包装。

**做什么**:

- 新建 `src/agent/interaction.rs`(或在 `approval.rs` 内新增 section),从 `agent/mod.rs` 导出。
- 定义 `Interaction { step_id: StepId, kind: InteractionKind }` 与
  `InteractionKind`:`Approval { call_id: ToolCallId, requirement: ApprovalRequirement }`、
  `Question { prompt: String }`、`Choice { prompt: String, options: Vec<String> }`。
- 定义 `InteractionResponse`:`Approval(ApprovalResponse)`、`Answer(String)`、`Choice(usize)`。
- 提供受检构造器:`Choice` 响应的 index 必须落在 options 范围内,否则分类错误;`Approval`
  响应必须与请求的 `call_id/step_id` 匹配(复用现有 `ApprovalResponse` 的 id 访问器)。
- 把 M1-1 中 `RequirementKind::NeedInteraction { request: Interaction }` 与
  `RequirementResult::Interaction(InteractionResponse)` 的占位替换为真实类型,补齐
  `accepts` 校验。
- 在 rustdoc 说明:`ToolApprovalPolicy` 将来(M3)成为 interaction handler 的一个后端,
  而非 loop 内部直接调用的 policy;本任务不改 `DefaultAgentLoop` 的现有用法。

**验证**:

- 聚焦测试:`Interaction`/`InteractionResponse` serde round-trip;`Choice` 越界拒绝;
  `Approval` 响应 id 不匹配拒绝;`Approval` 变体与旧 `ApprovalRequirement/Response` 互转无损。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 interaction 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

**完成记录**:

- 新建 `src/agent/interaction.rs` 并从 `agent/mod.rs` 导出;不接线任何驱动逻辑,不改
  `DefaultAgentLoop` 现有 approval 用法。旧 approval 类型全部保留并 re-export。
- `Interaction { step_id: StepId, kind: InteractionKind }` + `InteractionKind` 三变体
  `Approval { call_id: ToolCallId, requirement: ApprovalRequirement }`、`Question { prompt }`、
  `Choice { prompt, options }`(payload 复用旧 `ApprovalRequirement`,不重定义)。构造器
  `Interaction::{new, approval, question, choice}`,访问器 `step_id`/`kind`。
- `InteractionResponse` 三变体 `Approval(ApprovalResponse)`、`Answer(String)`、`Choice(usize)`,
  复用旧 `ApprovalResponse`。受检构造器:`InteractionResponse::choice_for`(index 必须落在
  options 范围内,否则 `ChoiceOutOfRange`)、`approval_for`(step_id/call_id 必须匹配请求,复用
  `ApprovalResponse` 的 `step_id()`/`call_id()` 访问器,否则 `StepMismatch`/`CallMismatch`)、
  `answer`。统一入口 `Interaction::accepts_response`,family 不匹配返回 `ResponseKindMismatch`。
- `Approval` 变体与旧类型互转无损:`From<ApprovalResponse> for InteractionResponse` +
  `TryFrom<InteractionResponse> for ApprovalResponse`(非 Approval 变体返回分类错误)。
- `InteractionKindTag`(Display,`approval`/`question`/`choice`)统一 family 判定;
  `InteractionError`(Copy,四变体)为分类错误。
- 给旧 `ApprovalRequirement` 增加 `Serialize/Deserialize`(`snake_case`,纯派生、非行为变更),
  使 `InteractionKind::Approval` 可持久化。
- 替换 M1-1 占位:删除 `requirement.rs` 的占位 `Interaction`/`InteractionResponse`,改 import
  `agent::interaction` 真实类型;`RequirementKind::NeedInteraction { request: Interaction }` 与
  `RequirementResult::Interaction(InteractionResponse)` 现指向真实类型;`RequirementKind::accepts`
  对 NeedInteraction 补齐深校验(在 family 对齐后再调 `Interaction::accepts_response`),新增
  `RequirementError::Interaction(#[from] InteractionError)`。requirement re-export 去掉重复的
  `Interaction`/`InteractionResponse`。
- rustdoc 说明:`ToolApprovalPolicy` 将来(M3)成为 interaction handler 的一个后端、
  loop `respond_approval` 未来删除走通用 `RequirementResult::Interaction` 回程;本任务不改现有用法。
- 聚焦测试(interaction 6 个全绿):三种 `InteractionKind`/`InteractionResponse` serde round-trip、
  `Choice` 越界拒绝、`Approval` 响应 step/call 不匹配拒绝、family 不匹配拒绝、`Approval` 变体与旧
  `ApprovalResponse` 互转无损;requirement 10 个、approval 4 个仍全绿(matrix 深校验覆盖)。
- 验证:`cargo fmt --all`(clean);`cargo clippy --all-targets -- -D warnings`(clean);
  `cargo test --lib agent::interaction`(6 passed)/`agent::requirement`(10)/`agent::approval`(4);
  `cargo test --all --all-targets`(lib 375 passed,其余 target 全绿,网络用例 ignored,无 failed);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过;`git diff --check` 干净。

### [TODO] M1-R Milestone 1 Review

**前置依赖**:M1-1..M1-3。

**上下文**:确认类型骨架完整、可 serde、与迁移文档 §3/§4 形状一致,且未改动任何现有行为。

**做什么**:

- 逐项核对 §12 决策 A(RequirementIds 供给)、B/C(暂不排序)在类型层已体现或已留位。
- 核对 `RequirementKind` 四变体、`RequirementResult` 四变体、`accepts` 类型对齐矩阵齐全。
- 核对 `Notification` 只含通知、`Interaction` 正确泛化 approval 且旧类型仍可用。
- 核对新增类型的 serde 边界 rustdoc(哪些字段可持久化、哪些是运行时结果)清晰。
- 确认 `DefaultAgentLoop` 与现有 50 个 loop 测试未受影响(全绿)。

**验证**:

- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。Review 结论(通过/需返工项)写入本任务完成记录。

---

## Milestone 2 — sans-io step（迁移文档阶段 1）

### [TODO] M2-1 `AgentMachine`、`StepInput`、`StepOutcome` 与 `AgentInput` 调整

**前置依赖**:M1-R。

**上下文**:迁移文档 §2。核心是把"loop 自驱"改成"外部驱动 pull":`step(&mut self,
StepInput) -> StepOutcome`,纯、同步、无 async。现有 `AgentInput`(`event.rs`)为
`UserMessage/QueuedPivotTurn/Resume`;新模型下 `AgentInput` 只保留外部输入语义。

**做什么**:

- 新建 `src/agent/machine.rs`,从 `agent/mod.rs` 导出。
- 定义 `trait AgentMachine { fn step(&mut self, input: StepInput) -> StepOutcome; fn cursor(&self)
  -> &LoopCursor; }`;trait **不** 是 `async_trait`。
- 定义 `StepInput`:`External(AgentInput)`、`Resume(RequirementResolution)`、
  `Abandon(RequirementId)`。
- 定义 `StepOutcome { notifications: Vec<Notification>, requirements: Vec<Requirement>,
  quiescent: bool }`(决策 B:一次 step 推进到静止并可一次吐一批 requirement)。
- 调整 `AgentInput` 为 `UserMessage(AgentUserInput)` 与 `Pivot(PivotMessage)`;删除
  `QueuedPivotTurn`/`Resume` 变体(其语义搬到 `StepInput`)。**并存策略**:若删除会破坏
  `DefaultAgentLoop`,则暂时保留旧变体并加 `#[deprecated]`,在 M4 清理;在本任务完成记录里
  写明选择。
- 本任务只定义类型与 trait,不实现具体 `step` 逻辑(留 M2-3/M2-4)。可提供一个最小
  `#[cfg(test)]` fake machine 验证 trait 对象安全性与 serde 无关性(machine 状态 serde 在 M2-2)。

**验证**:

- 聚焦测试:`StepInput`/`StepOutcome`/调整后 `AgentInput` serde(对可 serde 部分)round-trip;
  `AgentMachine` 可作为 trait object;fake machine 的 `step` 返回结构可断言。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 machine 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### [TODO] M2-2 `LoopCursor` 升格为整台机器的可序列化状态

**前置依赖**:M2-1。

**上下文**:迁移文档 §5。现有 `src/agent/state/cursor.rs` 的 `LoopCursor` 变体
(`Idle`/`StreamingStep(StepCursor)`/`AwaitingTool(ToolWaitCursor)`/
`AwaitingApproval(ApprovalCursor)`/`CancelRecovery(CancelRecoveryCursor)`/`Done`/`Error`)
已与 requirement 一一对应,只是记的是"恢复 hint",需补 `RequirementId`(及 `AgentPath`)使其
精确记住"卡在哪个 requirement 上",支撑跨进程恢复时重建未决登记表。

**做什么**:

- 在 `StepCursor`、`ToolWaitCursor`、`ApprovalCursor` 中补 `RequirementId`
  (`AwaitingTool` 是一批 → `BTreeMap<ToolCallId, RequirementId>` 或等价)与 `AgentPath`
  (阶段 0 恒为根);保持字段私有 + 受检构造器 + serde。
- 更新 `LoopCursor` 的构造器(`streaming_step`/`awaiting_tool`/`awaiting_approval`)接受
  requirement id 参数;保留现有校验(空 tool set、重复 call id)。
- 在 rustdoc 明确:`LoopCursor` 现在是"整台机器可序列化状态"的核心;live handle
  (`AgentRuntimeHandles`)保持在 `state.rs` 之外,不进 serde。
- 更新 `src/agent/state/cursor.rs` 与 `src/agent/state.rs` 里受影响的转换/校验;更新
  现有 cursor 单测(`state/tests.rs`)。

**验证**:

- 聚焦测试:升格后各 cursor serde round-trip 含 `RequirementId`/`AgentPath`;非法转换仍被
  既有校验拒绝;从 cursor 能读回未决 requirement id 集合。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 state/cursor 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### [TODO] M2-3 抽出 LLM step:`NeedLlm` 与通知产出

**前置依赖**:M2-2。

**上下文**:迁移文档 §2/§3。现有 `src/agent/loop_driver/default.rs` 的推进逻辑分散在
`NonStreamingSegment::next_event`(~1189 行起)、`StreamingSegment::next_event`(~1311 行起)、
`chat_with_cancel`(~524)、`open_streaming_call`/`next_stream_event`(~534/545)。本任务把
"要一次 LLM 调用"这一步从"内部 await client"改成"`step` 吐 `NeedLlm`,等 `Resume` 回灌
`Response`"。text-only turn 的完整推进(begin_turn → NeedLlm → 折叠 Response → 静止)先打通。

**做什么**:

- 在 `machine.rs`(或 `loop_driver/` 下新 `machine` 子模块)实现一个 `AgentMachine`,内部持有
  `AgentState`,复用现有把 `Response` 折叠进 Conversation pending 的逻辑
  (`AssistantFinish`/`TurnMeta`/`MessageMeta` 等,见 default.rs 的 Conversation 集成)。
- `step(External(UserMessage))`:开 pending turn,产出 `NeedLlm { request, mode }`
  (request 由现有构造 `ChatRequest` 的代码路径生成),cursor → `StreamingStep`,`quiescent=true`。
- `step(Resume(Llm(Ok(response))))`:折叠 response;若无 tool call → 提交 turn,cursor → `Done`;
  产出 `Notification::StepBoundary` 等通知。streaming 模式下 delta 的 `Notification::Llm` 由
  driver 从兑现里 tee(决策 D 暂不做,本任务 drain 直接透传;文本折叠仍走 Resume 的完整
  `Response`)。
- `step(Resume(Llm(Err(e))))`:按现有错误分类迁 cursor → `Error`。
- 不实现 tool 分支(留 M2-4);遇到 tool call 时先返回一个明确的"未实现"分类错误或占位
  requirement,在本任务完成记录写明。

**验证**:

- 聚焦测试(纯,无网络):喂 `UserMessage` → 断言吐 `NeedLlm`;喂 `Resume(Llm(Ok(text_response)))`
  → 断言 cursor `Done`、Conversation committed 追加了 assistant message、产出 StepBoundary 通知;
  喂 `Resume(Llm(Err))` → cursor `Error`。全部同步,无 `tokio::test`。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 machine 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### [TODO] M2-4 抽出 tool step:`NeedTool` 与 `NeedInteraction`

**前置依赖**:M2-3。

**上下文**:迁移文档 §2/§3/§4。现有 tool 编排在 default.rs 的 `execute_prepared_tool`(~675)、
`process_next_ready_tools`(~958)、`resolve_pending_approval`(~1060)、`wait_for_approval`(~1457),
以及 tool-call id 映射(`ToolCallMapping`/`ToolExecutionIds`)。本任务把 tool 执行与审批从
"内部 await"改成"`step` 吐 `NeedTool`/`NeedInteraction`,等 `Resume` 回灌 `ToolResponse`/
`InteractionResponse`"。

**做什么**:

- `step(Resume(Llm(Ok(response_with_tool_calls))))`:映射 provider call id → `ToolCallId`
  (复用现有映射),对需审批的 tool 先吐 `NeedInteraction { Approval }`、其余(或审批通过后)
  吐一批 `NeedTool`(决策 B:一次吐一批);cursor → `AwaitingApproval`/`AwaitingTool`
  (记入各自 `RequirementId`)。
- `step(Resume(Interaction(Approval(resp))))`:approve → 转吐对应 `NeedTool`;deny/timeout →
  合成对应 `ToolResponse`(复用现有 approval→ToolResponse 转换),走同一 result 回灌路径。
- `step(Resume(Tool(Ok(resp))))` / `Tool(Err(e))`:按现有 `ToolFailurePolicy` self-heal 逻辑
  把 result 追加进 Conversation;一批 tool 全部回灌后,再吐下一个 `NeedLlm` 进入下一 step,
  或提交 turn → `Done`。
- 保持并行语义:一批 `NeedTool` 的 `RequirementId` 各自独立,driver 可并发兑现、按完成
  顺序 `Resume`(顺序无关性由 machine 保证)。

**验证**:

- 聚焦测试(纯):single tool、parallel tool、tool failure self-heal、approval approve/deny/timeout、
  多轮 tool→llm→tool;断言每步 requirements/notifications/cursor 与 Conversation 追加正确;
  乱序回灌一批 tool result 结果一致。全部同步。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 machine 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### [TODO] M2-R Milestone 2 Review

**前置依赖**:M2-1..M2-4。

**上下文**:确认 sans-io step 完整、纯、覆盖 text/tool/approval,且 Conversation 不变量未被绕开。

**做什么**:

- 审计 `machine.rs`:`step` 及其调用链**无 `await`、无 client/tool/进程调用**(可用 grep 断言)。
- 核对 requirement/notification 二分正确;turn 结束由 `quiescent + cursor` 表达,无 `Done` 事件。
- 核对乱序回灌一批 tool result 的确定性;approval 三态(approve/deny/timeout)语义与旧路径等价。
- 核对所有 tool result / assistant message 仍走 Conversation 受检 append,未新造 bypass。

**验证**:

- 运行全套命令(见通用约束)。Review 结论写入完成记录。

---

## Milestone 3 — driver + drain 单层（迁移文档阶段 2）

### [TODO] M3-1 `HandlerScope` 与四个 handler trait

**前置依赖**:M2-R。

**上下文**:迁移文档 §6。库提供机制:一层 drain = 一组 requirement handler,缺省行为 = pop。
handler 兑现时用现有资源:`client::LlmClient`、`agent::ToolRegistry`、`ToolApprovalPolicy`。

**做什么**:

- 新建 `src/agent/drive.rs`,从 `agent/mod.rs` 导出。
- 定义 `trait HandlerScope`,四个默认返回 `None` 的方法:`llm()`/`tool()`/`interaction()`/
  `subagent()`,分别返回 `Option<&dyn LlmHandler/ToolHandler/InteractionHandler/SubagentHandler>`。
- 定义四个 handler trait(`async_trait`):
  - `LlmHandler::fulfill(&self, req: &ChatRequest, mode: LlmStepMode, ctx: &RunContext) -> RequirementResult`
  - `ToolHandler::fulfill(&self, call_id: ToolCallId, call: &ToolCall, ctx: &RunContext) -> RequirementResult`
  - `InteractionHandler::fulfill(&self, req: &Interaction, ctx: &RunContext) -> RequirementResult`
  - `SubagentHandler`:阶段 0 只定义签名(派生 + 再开一层 drain),实现留 M5。
- 每个 handler 返回的 `RequirementResult` 变体必须与请求 kind 对齐(复用 M1-1 `accepts`)。

**验证**:

- 聚焦测试:一个把 `LlmClient`/`ToolRegistry`/`ToolApprovalPolicy` 包装成 handler 的最小
  fixture;断言 `HandlerScope` 缺省方法返回 `None`;handler 返回结果通过 `accepts` 校验。
- 运行全套命令。

### [TODO] M3-2 `drain` 参考实现与 pop 路由

**前置依赖**:M3-1。

**上下文**:迁移文档 §4.2/§4.3/§6/§7.3。drain 用一层 scope 把机器推进到 turn 结束;本层
兜不了的 requirement 通过 parent 逐级 pop;顶层无 handler 报错。

**做什么**:

- 定义 `trait Pop`(向外层转交一个 requirement 并取回其 `RequirementResult`)。
- 实现 `async fn drain<M: AgentMachine>(machine, input, scope, parent: Option<&mut dyn Pop>,
  ctx) -> Result<TurnDone, AgentError>`:循环 `step` → 对每个 requirement 查 scope handler,
  有则兑现并 `Resume`,无则 pop 给 parent(parent=None 且无 handler → 返回分类错误
  `AgentError::UnhandledRequirement { kind, origin }`);处理完一批后继续,直至 `quiescent &&
  requirements.is_empty() && cursor ∈ {Done, Error}`。
- pop 查找从"发出者 scope 的外层"开始(跳过自身,防即时环 §7.3)。
- 决策 B:一次 step 吐一批时,driver 可并发兑现本层能兜的 requirement(用现有 `join_all`
  风格),按完成顺序 `Resume`。
- 新增 `AgentError::UnhandledRequirement` 分类变体(`src/agent/event.rs` 的 `AgentErrorKind`)。

**验证**:

- 聚焦测试:本层有 handler → 兑现且不冒泡;本层无 → pop 到 parent 兑现;顶层无 → 返回
  `UnhandledRequirement`;handler 自身 perform 同类 requirement 时不回到自身(用两层 scope
  构造并断言);一批 requirement 并发兑现、乱序回灌结果一致。
- 运行全套命令。

### [TODO] M3-3 参考 driver：复跑现有 loop 集成测试

**前置依赖**:M3-2。

**上下文**:迁移文档 §10 阶段 2 验收、§12 决策 E。现有 `src/agent/loop_driver/default/tests.rs`
有 50 个覆盖 text/tool/parallel/failure/approval/cancel 的集成测试。本任务提供一个薄 driver
用 `machine + drain + 单层 scope` 复现这些语义,证明新路径等价。

**做什么**:

- 在 `drive.rs`(或新 `drive/reference.rs`)提供一个参考 driver:构造单层 `HandlerScope`
  (llm=`LlmClient` 包装、tool=`ToolRegistry` 包装、interaction=`ToolApprovalPolicy` 包装),
  对一个 `AgentInput` 调 `drain` 跑完一个 turn,收集 `Notification` 与终态。
- 将 default 测试里可迁移的 fake `LlmClient` / `ToolRegistry` / approval policy 复用到参考
  driver 的测试;逐一对照 text-only、single tool、parallel tool、tool failure self-heal、
  approval approve/deny turn 的 Conversation 终态与通知序列。
- 保留 `DefaultAgentLoop` 与其原测试不动(并存);新增参考 driver 测试作为等价性证据。

**验证**:

- 聚焦测试:参考 driver 复跑上述 turn 类型,Conversation committed 结果与 `DefaultAgentLoop`
  对应用例一致;通知序列包含预期 StepBoundary/ToolCall* 事件。
- 运行 `cargo test --all --all-targets` 确认新旧两套测试同时全绿;其余全套命令。

### [TODO] M3-R Milestone 3 Review

**前置依赖**:M3-1..M3-3。

**做什么**:

- 核对 pop 路由四条规则(本层兑现不冒泡 / 无则 pop / 顶层报错 / 从外层起防即时环)有测试覆盖。
- 核对"运行模式 = scope 差异":同一 machine 在挂/不挂 interaction handler 下行为差异有测试。
- 核对参考 driver 与 `DefaultAgentLoop` 在 text/tool/approval 的等价性证据充分。
- 确认 `UnhandledRequirement` 是分类错误、不静默跳过或挂起。

**验证**:运行全套命令。Review 结论写入完成记录。

---

## Milestone 4 — cancel / pivot 收编与删旧机制（迁移文档阶段 3）

### [TODO] M4-1 cancel = never-resume,接 `Conversation::cancel_pending`

**前置依赖**:M3-R。

**上下文**:迁移文档 §7。cancel 不是单独机制,是 handler 行为:`step(Abandon(id))` 不回灌
结果,迁 cursor → `CancelRecovery`,触发被弃子树 `Conversation::cancel_pending`(补合成
`Cancelled` tool result 或丢弃 pending),收尾后仍可 feed。现有 `CancelRecoveryCursor`/
`CancelRecoveryReason`/`CancelDisposition`/`CancelledToolResult` 已就位。

**做什么**:

- 实现 `step(StepInput::Abandon(id))`:定位该 requirement 对应 cursor,迁 `CancelRecovery`;
  产出一个明确指示"需对哪个 Conversation 做何种 disposition"的 outcome。
- 在 machine / driver 层触发 `Conversation::cancel_pending`(按 `CancelDisposition`),闭合
  裂缝;收尾后 cursor 回到可 `step(External(UserMessage))` 的一致态。
- `CancellationToken`(`context/cancel.rs`)保留为向下"该停了"信号:driver 据此决定 `Abandon`
  哪些未决 requirement;但 cancel 的**闭合**由 never-resume + `cancel_pending` 完成,不再靠
  token 内部状态。
- 在参考 driver 里接入 cancel 路径。

**验证**:

- 聚焦测试:发出 `NeedTool`/`NeedLlm` 后 `Abandon` → cursor `CancelRecovery` → 触发
  `cancel_pending` → Conversation 无悬空 tool_use、pending 一致 → 之后可成功 `step(UserMessage)`
  开新 turn。覆盖"streaming step 中途 abandon"与"一批 tool 部分 abandon"。
- 运行全套命令。

### [TODO] M4-2 pivot = 多喂 input,删除 pivot queue

**前置依赖**:M4-1。

**上下文**:迁移文档 §2.2/§10。现有 pivot 走 `interject` + pivot queue(`state/queue.rs` 的
`QueuedPivot`/`PivotSource`)+ `AgentInput::QueuedPivotTurn`,在 step boundary 生效。新模型
pivot 就是 `AgentInput::Pivot`,由 driver 在 step 间隙直接喂入。

**做什么**:

- 实现 `step(External(AgentInput::Pivot(msg)))`:在合法边界向 pending 追加 `Role::User`
  消息(复用现有 Conversation step-boundary user 注入入口),沿用现有 role sequence 校验。
- 删除 pivot 排队语义:移除 `interject` 的 queue 行为、`QueuedPivotTurn`;`PivotMessage`/
  `PivotSource` 数据类型保留。"何时插 pivot"归 driver / Session,不在库内排队。
- 更新 `state.rs`/`queue.rs` 与受影响测试;清理 M2-1 中若保留的 `#[deprecated]` `AgentInput` 变体。

**验证**:

- 聚焦测试:两次 step 间喂 `Pivot` → 下一 turn 从注入的 user 消息推进;非 `Role::User` pivot
  被拒;pivot 只在合法边界注入(不破坏 open tool calls)。
- 运行全套命令。

### [TODO] M4-3 删除 `respond_approval`、pivot queue 残留与 `AgentFeedGuard`

**前置依赖**:M4-2。

**上下文**:迁移文档 §4/§10、§12 决策 F。审批响应现在走
`RequirementResult::Interaction(Approval(..))` 通用回程,loop 的 `respond_approval` 冗余;
`&mut self` 已提供背压,`AgentFeedGuard` 冗余。决策 E:评估 `DefaultAgentLoop` 去留。

**做什么**:

- 删除 `AgentLoop::respond_approval`(`loop_driver.rs`)及其在 `DefaultAgentLoop` 的实现和
  `ApprovalWaiters` 相关运行时(default.rs);审批统一走 interaction handler + Resume。
- 将 `AgentFeedGuard`/`AgentFeedPermit` 降级为 `debug_assert` 或删除(`&mut self` 背压替代)。
- 决策 E:评估把 `DefaultAgentLoop` 重构为"薄 driver"或保留为参考 driver 的一个实现;在
  完成记录写明选择与理由,并保证现有 loop 测试要么迁移到新路径、要么随实现删除且被参考
  driver 测试等价覆盖。
- 更新 `agent/mod.rs`、`lib.rs` 导出与 crate 根文档,移除已删 API。

**验证**:

- 聚焦测试:审批完全走 interaction 回程、无 `respond_approval` 调用点;背压由 `&mut self`
  保证(重入在类型层不可能或 debug_assert 触发)。
- 运行 `cargo test --all --all-targets` 确认无悬挂引用;其余全套命令。

### [TODO] M4-R Milestone 4 Review

**前置依赖**:M4-1..M4-3。

**做什么**:

- 核对 cancel=never-resume 的"受控丢弃 + 闭合"语义:被弃子树都触发了 `cancel_pending`,
  且"cancel 后仍可 feed"有测试。
- 核对 pivot/approval/cancel 三者已收编为"requirement + handler + 多喂 input"的统一表现,
  旧三套并列机制(pivot queue / approval responder / cancel token 主体)已删除或降级。
- 确认无 multishot / continuation 复制被引入;多路径路径仍指向 `fork_at`。

**验证**:运行全套命令。Review 结论写入完成记录。

---

## Milestone 5 — hierarchy / subagent（迁移文档阶段 4）

### [TODO] M5-1 嵌套机器状态与 `AgentPath` 落位

**前置依赖**:M4-R。

**上下文**:迁移文档 §7.1/§9。`agent + subagents` = 嵌套机器:父机器 state *包含* 子机器
state;整棵树可序列化。此前 `AgentPath` 恒为空,本任务让它真实反映树中位置。

**做什么**:

- 扩展机器状态:一个节点可持有零或多个子机器(`BTreeMap<AgentSlot, ChildMachineState>`),
  整棵树 serde;live handle 仍全部在 driver 侧。
- `step` 递归推进整棵树到静止:每个节点要么产出中间通知、要么卡在 requirement 上;树上
  任意位置的 outstanding requirement 聚合进 `StepOutcome.requirements`,每个带真实
  `origin: AgentPath`。
- requirement 兑现结果 `Resume` 按 `id`(+ `origin`)精确路由回对应子机器的 step 点。
- `LoopCursor` 各 cursor 的 `AgentPath` 字段(M2-2 已留)填真实路径。

**验证**:

- 聚焦测试:构造父+子两层机器,`step` 后聚合出分别带父/子 `AgentPath` 的 requirement;
  按 id 回灌到正确子机器;整棵树 serde round-trip;父子各自 cursor 独立恢复。
- 运行全套命令。

### [TODO] M5-2 `SubagentHandler`:派生、再开一层 drain 与作用域强制

**前置依赖**:M5-1。

**上下文**:迁移文档 §7.2/§7.3/§8。`NeedSubagent` 是唯一加深作用域链的 requirement;其
handler 派生子 agent 并再开一层 drain 递归驱动,并从"当前 drain scope"隐式派生
`RunContext`(cancel↓/budget↕/trace↓),在此强制深度上限、预算继承、cancel 传播。

**做什么**:

- 实现 `SubagentHandler`:接收 `NeedSubagent { spec_ref, brief, result_schema }`(只有 data),
  用 `RunContext::derive_child`(`context.rs`)派生子上下文,构造子机器,`drain` 递归驱动;
  子机器冒出的、本内层 scope 兜不了的 requirement(如 `NeedInteraction`)pop 到外层。
- 深度上限:每加深一层在 handler 检查(承接旧 `agent-layer.md` §6.3 深度护栏),超限分类报错。
- 预算继承 / cancel 传播:子上下文共享父 budget ledger、继承 cancel 链(复用现有
  `RunContext` 语义);父 cancel → 子被 `Abandon` 并 `cancel_pending` 收尾。
- 子 turn 结束把 `SubagentOutput` 作为 `RequirementResult::Subagent(..)` `Resume` 回父机器。
- pop 从外层起(§7.3):subagent handler 内部 perform 的同类 requirement 不回到它自己。

**验证**:

- 聚焦测试:attended 父 scope(挂 interaction 真人后端)+ headless 子 scope(不挂 interaction)
  → 子 `NeedInteraction` pop 到父被兑现;深度超限报错;父 cancel 传播到子并 `cancel_pending`;
  子消耗计入父 budget ledger。
- 运行全套命令。

### [TODO] M5-3 Observability:trace 记 resolved-by-scope 与 disposition

**前置依赖**:M5-2。

**上下文**:迁移文档 §8/§11。动态作用域要求 trace 记录每个 requirement**被哪层 scope 兑现**
及**resume 还是 never-resume**。现有 `context/trace.rs` 有 `TraceHandle`/`TraceNodeKind`/
`TraceRecord`。

**做什么**:

- 新增 `TraceNodeKind::Requirement { kind_tag, resolved_at_scope, disposition }`,
  `disposition ∈ { Resumed, NeverResumed }`。
- 在 `drain` / handler 兑现点记录:requirement 最终被哪层 scope 的 handler 兑现;never-resume
  (cancel)也必须留痕(是真实发生、影响下层 Conversation 的事件,非 non-event)。
- 与旧 trace tree(run→step→llm/tool/sub-agent)对齐,只是补 requirement 归属与处置。

**验证**:

- 聚焦测试:一次含 pop 的兑现在 trace 记录了正确 `resolved_at_scope`;一次 cancel 在 trace
  记录 `NeverResumed`;trace record serde round-trip。
- 运行全套命令。

### [TODO] M5-R Milestone 5 Review

**前置依赖**:M5-1..M5-3。

**做什么**:

- 核对嵌套机器整树可序列化、requirement 按 `id + origin` 精确路由、父子并发兑现按完成顺序回灌。
- 核对深度上限、预算继承、cancel 传播全部在 subagent handler 强制(不散落别处)。
- 核对"同一 spec 在挂/不挂 interaction 的 scope 下 attended/headless 自动切换"有端到端测试。
- 核对 trace resolved-by-scope + disposition 完整。

**验证**:运行全套命令。Review 结论写入完成记录。

---

## Milestone 6 — 文档并轨与端到端验收（迁移文档阶段 5）

### [TODO] M6-1 更新主文档与 PLAN/TODO 交叉引用

**前置依赖**:M5-R。

**上下文**:迁移文档 §10。旧 `agent-layer.md` §1.3/§3/§4 描述 push 契约,需改写为 pull;
`agent-effect-model.md` 与 `agent-effect-migration.md` 应从"草稿"升为"已落地"。

**做什么**:

- 改写 `docs/agent-layer.md` §1.3(feed→stream → step→requirements pull 契约)、
  §3/§4(审批/pivot/cancel 从三种并列机制 → 同一 requirement+handler 的三种表现)。
- 更新 `docs/agent-effect-model.md` 与 `docs/agent-effect-migration.md` 顶部状态标注为已落地,
  并链接到实现位置(`agent/machine.rs`/`agent/drive.rs`/`agent/requirement.rs` 等)。
- 更新 crate 根文档(`src/lib.rs`)与 `README.md`,把 sans-io + effect-handler 模型纳入
  当前公开能力说明,移除已删 push API 的描述。

**验证**:

- 文档变更以 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 校验 rustdoc 链接;
  `git diff --check`;人工核对与代码一致。运行 `cargo fmt --all`、`cargo clippy`、
  `cargo test --all --all-targets`(确保 doctest/示例引用未失效)。

### [TODO] M6-2 端到端验收示例:attended 父 + headless 子

**前置依赖**:M6-1。

**上下文**:迁移文档 §1/§4.4/§7。目标是证明"attended/unattended 是同一张图的两种跑法":
同一 subagent spec,在挂 interaction 的 scope 下 attended、在不挂的 scope 下 headless,
无需给 subagent 任何配置。现有 `examples/` 有 `non_streaming.rs`/`tool_round_trip.rs` 可参考
离线 fake 风格。

**做什么**:

- 新增 `examples/agent_effect_hierarchy.rs`(或 `tests/agent_effect_e2e.rs`):用离线 fake
  `LlmClient`/`ToolRegistry` 与 policy interaction 后端,搭一个父 agent(顶层 scope 挂
  interaction=policy 或模拟真人)派生一个 headless 子 agent(内层 scope 不挂 interaction),
  子 `NeedInteraction` pop 到父被兑现;跑完一个含 tool 与 subagent 的 turn。
- 覆盖父子并发兑现、cancel 传播、budget 聚合的端到端断言。
- 若放 `tests/`,用 crate 公共 API;若放 `examples/`,补一个 `#[test]` 或 CI 可运行入口。

**验证**:

- 聚焦运行该示例/测试并断言终态;`cargo test --all --all-targets` 含新端到端用例全绿;
  其余全套命令。

### [TODO] M6-R Milestone 6 与迁移总 Review

**前置依赖**:M6-1..M6-2。

**上下文**:全库回溯,确认迁移完整且未弱化 Conversation Core 不变量。

**做什么**:

- 回溯 `PLAN.md` 与本 `TODO.md` 全文,逐条确认:sans-io `step` 不 await、
  requirement/notification 二分、`id + origin` 可寻址、pop 路由与顶层 total、
  cancel=never-resume 接 `cancel_pending`、多路径 `fork_at` 无 multishot、RunContext 由 scope
  派生、serde/runtime 分离。
- 确认 Conversation Core 的 committed log、pending、tool pairing、`Boundary`、restore 不变量
  在 Agent 层未被重新实现或绕开。
- 确认所有旧 push API(`respond_approval`/pivot queue/`AgentFeedGuard`/`AgentEvent::Done`)已
  删除或明确保留理由,文档与代码一致。
- 汇总遗留 / 后续项(如决策 C 排序、决策 D token tee 的最终形态)到一个"后续"小节。

**验证**:

- 运行全套命令:`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。总 Review 结论与遗留项写入完成记录。
