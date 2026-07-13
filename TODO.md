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

### [DONE] M1-R Milestone 1 Review

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

**完成记录**:

- **Review 结论:通过,无返工项。** M1 类型骨架(M1-1..M1-3)完整、可 serde、与迁移文档
  §3/§4/§12 形状一致,且未改动任何现有行为。以下为逐项核对结果。
- **§12 决策核对**:
  - A(RequirementId host 供给):`RequirementIds` 供给 trait(`next_requirement_id(kind_tag)`)
    + `NoRequirementIds` 默认实现,风格对齐 `ToolExecutionIds`,库不造 id。已在类型层体现。
  - B(step 推进到静止并一次吐一批):属 M2 `StepOutcome` 语义,类型层已留位——`Requirement`
    携带 `id + origin: AgentPath` 使其可寻址,支撑一次一批 requirement 的 hierarchy 聚合;
    本阶段不需在类型层再加结构。
  - C(一批 requirement 暂不排序/优先级):类型层未强加 priority/顺序字段,符合"阶段 1–3
    不排序,由 driver 编排"。`RequirementKindTag` 的 `Ord` 仅用于 id 分配/匹配,非 requirement 排序。
- **RequirementKind 四变体 / RequirementResult 四变体 / accepts 矩阵**:`NeedLlm`/`NeedTool`/
  `NeedInteraction`/`NeedSubagent` 与 `Llm`/`Tool`/`Interaction`/`Subagent` 一一对应;
  `RequirementKind::accepts` 先按 `RequirementKindTag` 做 4×4 家族对齐(`accepts_matrix_pairs_...`
  覆盖 16 组合),再对 `NeedInteraction` 追加 `Interaction::accepts_response` 深校验;
  `Requirement::accepts_resolution` 先校验 id(`IdMismatch`)再校验类型。测试全绿。
- **Notification 只含通知**:四个纯通知变体(`Llm`/`StepBoundary`/`ToolCallStarted`/
  `ToolCallFinished`),与对应 `AgentEvent` 变体 wire 兼容;刻意排除 `AwaitingApproval`(→ 未来
  `NeedInteraction`)与 `Done`(→ 未来 `StepOutcome.quiescent + cursor`),无反向
  `From<AgentEvent> for Notification`;`event.rs` 测试显式钉住通知变体集合。
- **Interaction 泛化 approval 且旧类型仍可用**:`InteractionKind` 三变体(Approval/Question/
  Choice,payload 复用 `ApprovalRequirement`)、`InteractionResponse` 三变体;受检构造器
  (`choice_for` 越界拒绝、`approval_for` step/call 匹配);`From<ApprovalResponse>` +
  `TryFrom<InteractionResponse> for ApprovalResponse` 无损互转;旧 approval 类型全部保留并从
  `agent/mod.rs` re-export,`DefaultAgentLoop` 现有 approval 用法未改。
- **serde 边界 rustdoc**:`requirement.rs` 模块级文档明确划分"可持久化 requirement 描述"
  (`Requirement`/`RequirementKind`/`RequirementId`/`AgentPath`/`AgentSlot`,derive serde)与
  "运行时结果"(`RequirementResult`/`RequirementResolution`,含 `ClientError`/`ToolRuntimeError`/
  `AgentError`,刻意不 derive serde,跨进程由 cursor 里的 `RequirementId` 重建)。边界清晰。
- **DefaultAgentLoop 与现有测试未受影响**:M1 仅新增类型文件与纯派生 serde
  (`ApprovalRequirement`/`LlmStepMode`),无行为改动;`agent::loop_driver` 24 个 loop 测试
  (含 `default::tests` 21 个)全绿。
- **验证**(本任务仅评审,复跑确认):`cargo fmt --all`(clean);
  `cargo clippy --all-targets -- -D warnings`(clean,无告警);
  `cargo test --all --all-targets`(lib **375 passed**;集成/tests target 全绿;真实端点用例
  ignored;**0 failed**);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(通过);
  `git diff --check`(干净)。聚焦回归:requirement 10 / interaction 6 / event 9 / approval 4 全绿。

---

## Milestone 2 — sans-io step（迁移文档阶段 1）

### [DONE] M2-1 `AgentMachine`、`StepInput`、`StepOutcome` 与 `AgentInput` 调整

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

**完成记录**:

- 新增 `src/agent/machine.rs`,从 `agent/mod.rs` 以 `pub mod machine;` 声明并 re-export
  `AgentMachine`、`StepInput`、`StepOutcome`。
  - `trait AgentMachine { fn step(&mut self, input: StepInput) -> StepOutcome;
    fn cursor(&self) -> &LoopCursor; }`,**非** `async_trait`,对象安全(测试以
    `Box<dyn AgentMachine>` 驱动验证)。
  - `enum StepInput { External(AgentInput), Resume(RequirementResolution), Abandon(RequirementId) }`
    + `external/resume/abandon` 构造器,仅 `Clone + Debug`。整体**不** serde:`Resume` 携带运行期
    `RequirementResolution`(非 persistable);可 serde 的部分是 `External` 内的 `AgentInput` 与
    `Abandon` 内的 `RequirementId`。
  - `struct StepOutcome { notifications, requirements, quiescent }`(决策 B:一次 step 推进到静止并
    可一次吐一批 requirement),`pub` 字段 + `new/is_quiescent/has_requirements`;全字段可 serde,
    派生 `Serialize/Deserialize + Clone/Debug/Default/PartialEq`,`#[serde(deny_unknown_fields)]`。
  - 本任务只定义类型与 trait,**未**实现具体 `step` 逻辑(留 M2-3/M2-4);`#[cfg(test)]` `FakeMachine`
    仅验证对象安全性、`step` 返回结构可断言、cursor 迁移可读,且 machine 自身不涉 serde。
- 调整 `AgentInput`(`event.rs`):新增 `Pivot(PivotMessage)` 变体 + `pivot(..)` 构造器。
  - **并存策略选择**:`DefaultAgentLoop` 仍消费 `QueuedPivotTurn`/`Resume`,直接删除会破坏它,故按任务
    "并存策略"**保留旧变体并加 `#[deprecated]`**(变体与 `queued_pivot_turn`/`resume` 构造器均标注,
    note 指向 `AgentInput::Pivot` / `StepInput::Resume`,M4 清理)。内部消费点加 `#[allow(deprecated)]`
    以过 `-D warnings`:`default.rs::prepare_user_turn` 的 match、event.rs 构造器体与两处测试、
    `loop_driver.rs` 测试 `input()`、`default/tests.rs` 的 `queued_pivot_turn_input` 与
    `parent_cancel_..._resume_feed_continues_turn`。serde 派生对已弃用变体不触发弃用告警
    (`#[automatically_derived]`),故 wire shape 不变。
  - `default.rs` 的 `AgentInput` match 补 `Pivot` 臂:legacy loop 不支持直插 pivot(走队列),返回
    `AgentError::Other` 明确报错;现有测试不构造 `Pivot`,行为不变。
- 验证:`cargo fmt --all` clean;`cargo clippy --all-targets -- -D warnings` clean;
  聚焦 `agent::machine` 5 tests passed;`cargo test --all --all-targets` = lib 380 passed / 0 failed
  (较上一轮 375 +5 为本任务新增,网络用例 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  通过;`git diff --check` 干净。

### [DONE] M2-2 `LoopCursor` 升格为整台机器的可序列化状态

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

**完成记录**:

- **设计决策(requirement 寻址在 stage-0 为可选)**:legacy `DefaultAgentLoop` 直接 await IO、
  不 reify requirement,也没有 `RequirementIds` 供给器(默认 `NoRequirementIds` 会报错,强注入
  会破坏所有 legacy 测试)。迁移原则要求 M1–M3 legacy 仍可编译可测,故 cursor 的 requirement
  绑定为 `Option`:legacy/测试传 `None`,未来 sans-io 机器(M2-3/M2-4)传 `Some(..)` 并在其测试
  断言可读回 id。这是对“新旧路径并存”的忠实建模,**非**弱化不变量(新机器强不变量在 M2-3/M2-4
  强制)。`AgentPath` 阶段 0 恒为根,但类型随绑定一起就位,避免阶段 4 改签名。
- **新增类型(`src/agent/state/cursor.rs`)**:
  - `CursorRequirement { id: RequirementId, origin: AgentPath }`(单 requirement 绑定,Step/Approval
    用);构造器 `new(id, origin)`/`root(id)`,访问器 `id()`(Copy 返回值)/`origin()`;origin
    `#[serde(default, skip_serializing_if = "AgentPath::is_root")]`,根路径不落 wire。
  - `ToolWaitRequirements { origin: AgentPath, ids: BTreeMap<ToolCallId, RequirementId> }`(一批 tool
    requirement 绑定,共享 origin);`new`/`root`/`origin()`/`ids()`/`get(call_id)`。
- **cursor 结构升格(字段私有 + 受检构造器 + serde)**:
  - `StepCursor { step_id, requirement: Option<CursorRequirement> }` + `requirement()`/`requirement_id()`。
  - `ToolWaitCursor { step_id, tool_call_ids, requirements: Option<ToolWaitRequirements> }` + `requirements()`。
  - `ApprovalCursor { step_id, tool_call_id, requirement: Option<CursorRequirement> }` +
    `requirement()`/`requirement_id()`。
  - 三处 Option 绑定均 `skip_serializing_if = "Option::is_none"`,故既有 `AgentState` snapshot wire
    shape 不变(legacy cursor 不新增字段,旧快照仍可反序列化)。
- **构造器更新(任务点名)**:`LoopCursor::streaming_step(step_id, Option<CursorRequirement>)`、
  `awaiting_tool(step_id, tool_call_ids, Option<ToolWaitRequirements>)`、
  `awaiting_approval(step_id, tool_call_id, Option<CursorRequirement>)`。保留既有校验(空 tool set、
  重复 call id);新增:`requirements` 为 `Some` 时其 map 键集必须与 `tool_call_ids` 集合**完全一致**
  (缺失或多余都拒),新增 `AgentStateError::ToolRequirementMismatch { call_id }`。
- **读回未决登记**:`LoopCursor::pending_requirement_ids(&self) -> Vec<RequirementId>`
  (StreamingStep→step 绑定 id;AwaitingTool→map values;AwaitingApproval→approval 绑定 id;其余空),
  供 driver 跨进程恢复重建未决登记表。
- **调用点更新**:legacy `default.rs` 四处(streaming_step/awaiting_tool×2/awaiting_approval)传 `None`;
  `machine.rs` `#[cfg(test)] FakeMachine` 传 `None`;`state/tests.rs` 既有 cursor 单测更新签名。live
  handle(`AgentRuntimeHandles`)仍在机器 serde 之外(既有 `runtime_handles_are_kept_outside_agent_state_serde`
  断言不变);模块 rustdoc 写明 cursor 现为“整台机器可序列化状态”核心与 live handle 边界。
- **导出**:`state.rs` 与 `agent/mod.rs` 追加 `CursorRequirement`、`ToolWaitRequirements`。
- **聚焦测试(state,+8 全绿)**:streaming/approval/awaiting_tool 带 `RequirementId`/`AgentPath` 的 serde
  round-trip、root origin 省略 wire 且默认回根、legacy 无绑定省略、`pending_requirement_ids` 读回集合、
  tool 绑定不覆盖 call 集(缺失/多余)双向拒 `ToolRequirementMismatch`、requirement-free cursor 报空、
  `AgentState` 携升格 cursor 端到端 round-trip。
- **验证**:`cargo fmt --all` 通过;`cargo clippy --all-targets -- -D warnings` 通过;
  `cargo test --lib agent::state`(21 passed,含 8 新);`cargo test --all --all-targets`
  (lib 388 passed / 0 failed,较 M2-1 的 380 +8;网络用例 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过;`git diff --check` 干净。

### [DONE] M2-3 抽出 LLM step:`NeedLlm` 与 text-only turn 折叠

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

**完成记录**:

- **结构性修复(前置)**:M2-3 的标题行 `### [TODO] M2-3 …` 在 `TODO.md`(含 HEAD)中丢失,
  任务正文完好夹在 M2-2 完成记录与 M2-4 之间,使 M2-3 不可见为"第一个未完成任务"。按"任务条目
  本身错误须结构性修复"的例外先补回标题(commit `[M2-3] Restore dropped M2-3 heading …`),再实现。
- **可复用请求构造(class-wide 重构)**:新增 `src/agent/request.rs`,把 `build_chat_request` +
  `combine_system_prompt` 从 `loop_driver/default.rs` 抽出为 `pub(crate)`;签名由 `&dyn ToolRegistry`
  改为数据 `Vec<Tool>`(sans-io 机器传 `state.current_tool_set().tools()`,legacy loop 传
  `tool_registry.declarations()`),使纯机器无需持 live registry 即可构造 `ChatRequest`。
  `LlmStepMode::request_stream_flag` 提升为 `pub(crate)`;`agent/mod.rs` 加私有 `mod request;`
  (后代模块可见)。default.rs 删除两个本地函数并改调共享版本,legacy 行为不变。
- **模块化**:`git mv src/agent/machine.rs → src/agent/machine/mod.rs`(trait `AgentMachine`/
  `StepInput`/`StepOutcome` 与既有 FakeMachine 契约测试留 mod.rs);新增 `src/agent/machine/default.rs`
  承载具体机器与其纯测试。`machine/mod.rs` 追加 `mod default; pub use default::DefaultAgentMachine;`,
  `agent/mod.rs` 追加导出 `DefaultAgentMachine`。
- **`DefaultAgentMachine`(sans-io,text-only turn)**:字段 `state: AgentState`、`mode: LlmStepMode`、
  `requirement_ids: Arc<dyn RequirementIds>`(唯一非序列化字段,与 `ToolExecutionIds` 同一"库不造 id"
  边界)、`pending_assistant_message_id: Option<MessageId>`(mirror 现有 `PreparedAssistantCall` 的
  assistant id:cursor 只记 `RequirementId`,折叠所需的 caller 供给 assistant id 在单个在飞 step 内暂存)。
  `step` 分派:
  - `External(UserMessage)`:`requirement_ids.next(Llm)` 取 id → `begin_turn` →
    `build_chat_request(state, current_tool_set().tools(), mode.stream_flag)` → cursor
    `StreamingStep(step_id, Some(CursorRequirement::root(id)))` → 记 assistant id → 吐单个
    `NeedLlm { request, mode }`,`quiescent=true`,无通知。
  - `Resume(Llm(Ok(response)))`:校验 cursor=`StreamingStep` 且 `resolution.id` 与 cursor 记的
    `RequirementId` 一致 → `start_assistant_response` + `finish_assistant(assistant_id)`;
    `ReadyToCommit` → `commit_pending(TurnMeta::default())` → boundary=`conversation().head()` →
    cursor `Done(Completed)` → 吐 `Notification::StepBoundary`,quiescent 无 requirement。
  - `Resume(Llm(Err(e)))`:分类错误 → cursor `Error`(discard pending),quiescent。
  - `fail(msg)` 助手:`cancel_pending(DiscardTurn)` 清 pending → cursor→`Error`(best-effort)→
    清 assistant id → 返回 quiescent 空 outcome(`step` 无 `Result`,运行期失败以 Error cursor 表达)。
- **tool 分支未实现(留 M2-4,占位处理已写明)**:`finish_assistant` 返回 `RequiresToolCallMappings`
  (即 response 含 tool call)时,machine **不** 静默跳过,而是 `fail("tool orchestration is not
  implemented until M2-4")`——明确的分类错误 + discard pending。M2-4 将在此接 `NeedTool`/`NeedInteraction`。
  同理 `External(Pivot)`/legacy `QueuedPivotTurn`/`Resume(ResumeInput)`/`Abandon` 均分类为
  "M4 实现"错误,不默默无视(遵守"无 workaround、不静默跳过")。
- **决策 D(streaming delta tee)本任务不做**:文本折叠统一走 `Resume` 的完整 `Response`;streaming
  模式仅体现在 `NeedLlm.mode = Streaming` 且 `request.stream = true`,delta 的 `Notification::Llm`
  由未来 driver 从兑现里透传(migration §3.1 决策 D),不在纯机器内合成。
- **聚焦测试(纯,无 tokio,+8 全绿)**:`External(UserMessage)` 吐 `NeedLlm`(id/root origin、
  request.model/max_tokens/messages、mode、stream 标志)、cursor `StreamingStep`、
  `pending_requirement_ids` 读回该 id;`Resume(Llm(Ok(text)))` → cursor `Done`、committed history
  追加 assistant message(user/assistant 两条)、吐 `StepBoundary(step_id)`、quiescent 无 requirement;
  `Resume(Llm(Err))` → cursor `Error` 且 pending 被 discard;`Resume` id 不匹配 → Error;
  `Resume` 结果类型不符(Interaction)→ Error;tool-use response → Error(未实现,pending discard);
  streaming 模式 → `request.stream=true`;Idle 直接 `Resume` → Error。
- **验证**:`cargo fmt --all`(clean);`cargo clippy --all-targets -- -D warnings`(clean,含折叠
  `collapsible_if`);`cargo test --lib agent::machine`(13 passed:8 新 default + 5 既有 mod);
  `cargo test --all --all-targets`(lib 396 passed / 0 failed,较 M2-2 的 388 +8;网络用例 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean);`git diff --check`(clean)。

### [DONE] M2-4 抽出 tool step:`NeedTool` 与 `NeedInteraction`

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

**完成记录**:

- **共享助手(前置,已单独提交)**:把 `approval_response_for_decision`(approve/deny/timeout/cancel →
  合成 `ToolResponse`)从 `loop_driver/default.rs` 抽到 `src/agent/approval.rs` 为 `pub(crate)`,
  legacy 与 sans-io 机器共用同一 approval→ToolResponse 转换,避免重复实现(class-wide)。
- **模块化**:`git mv src/agent/machine/default.rs → src/agent/machine/default/mod.rs`;新增
  `src/agent/machine/default/tools.rs` 承载 tool phase;M2-3 的内联测试外移到
  `src/agent/machine/default/tests/mod.rs`,新增 `src/agent/machine/default/tests/tools.rs`。
- **`DefaultAgentMachine` 结构调整**:去掉 M2-3 的 `pending_assistant_message_id`,改持非序列化 scratch
  `in_flight: Option<InFlight>`(`InFlight { assistant_message_id, steps_started, tools: Option<ToolPhase> }`,
  mirror legacy segment 的栈局部);新增 host 供给的 `tool_ids: Arc<dyn ToolExecutionIds>`(默认
  `NoToolExecutionIds`)与 `approval_policy: Arc<dyn ToolApprovalPolicy>`(默认 `NoApprovalPolicy`),
  经 builder `with_tool_execution_ids`/`with_approval_policy` 注入。cursor 仍是唯一可序列化状态,
  只记 `RequirementId`。
- **tool phase 模型(决策 B:一次吐一批;规避非法 cursor 迁移)**:`fold_llm_response` 遇
  `RequiresToolCallMappings` 调 `begin_tool_phase`——从 pending 末条 assistant 抽 tool-use(复用
  legacy `extract_last_tool_calls` 的过滤),`tool_ids.tool_call_id` 映射 + `register_tool_calls`,
  `tool_ids.tool_result_message_id` 预分配结果 message id,`approval_policy.approval_requirement`
  按需分流为 `auto_pending: Vec<ToolSlot>`(AutoApprove)与 `approval_pending: VecDeque<ToolSlot>`
  (RequireApproval)。`advance_tool_phase`:①auto 非空 → 一次性 drain 全部 auto 为**一批**
  `NeedTool` → `AwaitingTool`;②否则弹一个待审批 → `NeedInteraction` → `AwaitingApproval`;
  ③都空 → `finish_tool_phase`。因所有 auto 一次吐尽,后续 advance 只会从 `AwaitingApproval`
  (审批通过)或 finish 进入,**永不出现被禁止的 `AwaitingTool→AwaitingTool`**。
- **回灌路径**:`resume` 按 cursor 路由。`resume_tool`(`AwaitingTool`)用
  `running: BTreeMap<RequirementId, ToolSlot>` 按 `resolution.id` 定位 → 顺序无关的乱序批回灌;
  `Tool(Ok)` 追加 `append_tool_response` + `ToolCallFinished`;`Tool(Err)` 按 `ToolFailurePolicy`
  self-heal(`ReturnErrorToModel` 走 `to_tool_response`;`StopRun` → `fail`);一批全回灌后
  `advance_tool_phase`。`resume_approval`(`AwaitingApproval`)校验 requirement id 与
  `Interaction::accepts_response`(step/call 匹配),approve → 直接吐单个 `NeedTool`
  (`AwaitingApproval→AwaitingTool`);deny/timeout/cancel → 合成 `ToolResponse` 追加 + 经
  "restore bounce"`AwaitingApproval→AwaitingTool([call], None)` 再 advance(全走合法迁移)。
- **步数上限与续 step**:`finish_tool_phase` 先吐 tool step 的 `StepBoundary(head)`(mirror legacy
  pending step-boundary pivot 点),再判 `steps_started >= max_steps` → `fail_with_notifications`
  (保留已吐的 StepBoundary);否则 `tool_ids.next_step_id()` + `next_assistant_message_id()` 续下
  一 `NeedLlm`,`steps_started += 1`。通知统一挂 tool 所属 step 的 `step_id`。
- **无 workaround / 未静默跳过**:tool id/approval/conversation 任一失败均分类 `fail`(discard pending →
  `Error` cursor),不 papering。`External(Pivot)`/legacy 输入/`Abandon` 仍分类为 "M4 实现" 错误。
- **已知边界(写明,非绕开)**:(1)批内先跑 auto 再逐个审批,与 legacy 严格 call-order 交错略有不同,
  但结果按 tool-call id 归位、turn 组装一致,任务显式允许批顺序无关;(2)`in_flight`/`ToolPhase`
  为非序列化 scratch(同 M2-3 的 assistant-id 边界),中途序列化会丢"哪些已回灌",持久化中途续跑属
  M3+ driver/persistence 职责。
- **聚焦测试(纯,同步,无 tokio;+13)**:`tests/tools.rs` 覆盖 single auto tool、parallel batch 乱序回灌、
  tool error(ReturnErrorToModel self-heal / StopRun 停机)、approval approve/deny/cancel、
  auto+approval 混合批(走 `StreamingStep→AwaitingTool→AwaitingApproval→AwaitingTool→StreamingStep`
  全合法迁移)、多轮 `tool→llm→tool→text`、step-limit(保留 StepBoundary 后 Error)、未知 requirement/
  结果类型不符/审批错配 call 的分类失败;断言每步 requirements/notifications/cursor 与 Conversation
  追加(message 计数/末条文本)。测试用脚本化 `ScriptedRequirementIds`/`ScriptedToolIds`(host 供 id)与
  `ApproveByName`/`AlwaysApprove` 审批策略。M2-3 的 `tool_use_response_is_rejected_until_m2_4` 相应改为
  `tool_use_response_without_tool_id_source_fails`(默认 `NoToolExecutionIds` 下分类 "tool id unavailable")。
- **验证**:`cargo fmt --all`(clean);`cargo clippy --all-targets -- -D warnings`(clean,修正
  `needless_lifetimes`);`cargo test --lib agent::machine`(26 passed:13 新 tool + 13 既有);
  `cargo test --all --all-targets`(417 passed / 0 failed,较 M2-3 的 396 之基线增量,网络用例 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean);`git diff --check`(clean)。

### [DONE] M2-R Milestone 2 Review

**前置依赖**:M2-1..M2-4。

**上下文**:确认 sans-io step 完整、纯、覆盖 text/tool/approval,且 Conversation 不变量未被绕开。

**做什么**:

- 审计 `machine.rs`:`step` 及其调用链**无 `await`、无 client/tool/进程调用**(可用 grep 断言)。
- 核对 requirement/notification 二分正确;turn 结束由 `quiescent + cursor` 表达,无 `Done` 事件。
- 核对乱序回灌一批 tool result 的确定性;approval 三态(approve/deny/timeout)语义与旧路径等价。
- 核对所有 tool result / assistant message 仍走 Conversation 受检 append,未新造 bypass。

**验证**:

- 运行全套命令(见通用约束)。Review 结论写入完成记录。

**完成记录**:

纯审阅任务,未发现 spec 违背/缺陷,零代码改动。四项审计结论:

- **① sans-io 纯度(通过)**:对整个 `src/agent/machine/` grep `await`/`async fn`/`async move`/
  `tokio::`/`spawn`/`block_on`/`.send(`/`.recv(` 无任何真命中(仅有 doc 文字、方法名 `block_on_llm`、
  字段 `awaiting_approval` 的形近误报);grep `LlmClient`/`ToolRegistry`/`.execute(`/`.generate(`/
  `.stream(`/`.chat(`/`reqwest`/`std::process`/`Command::`/`.invoke(`/`.run(` **零命中**。`step` 及其
  调用链(`begin_user_turn`/`block_on_llm`/`resume*`/`fold_llm_response`/`begin_tool_phase`/
  `advance_tool_phase`/`emit_tool_batch`/`emit_approval`/`resume_tool`/`resume_approval`/
  `finish_tool_phase`)全部同步纯函数,只吐 `Requirement`、park 到 `LoopCursor`,IO 由 driver 兑现。
- **② requirement/notification 二分 + 无 `Done` 事件(通过)**:机器只吐 `Notification`
  (`Llm`/`StepBoundary`/`ToolCallStarted`/`ToolCallFinished`,event.rs 第 348 行 enum,**无 `Done` 变体**)
  与 `Requirement`(`NeedLlm`/`NeedTool`/`NeedInteraction`);`RequirementResult`
  (`Llm`/`Tool`/`Interaction`/`Subagent`)与 kind 一一对齐,resume 路径显式校验错配结果类型(`other.tag()`
  分类 fail)并有 `tool_resume_with_wrong_result_kind_fails` 覆盖。turn 结束由 `commit_text_turn`/
  `finish_tool_phase` 走 `LoopCursor::done(Completed)`(成功)或 `LoopCursor::error`(失败)+
  `StepOutcome.quiescent==true`+空 requirement 表达,机器从不构造 `AgentEvent::Done`/`AgentOutcome`
  (grep 确认 machine 模块无 `AgentEvent`/`AgentOutcome` 命中,`Done` 仅出现在 legacy `AgentEvent` 与
  event.rs 的迁移说明 doc 中)。
- **③ 乱序回灌确定性 + approval 三态等价(通过)**:一批 `NeedTool` 各自 `RequirementId`,
  `ToolPhase.running: BTreeMap<RequirementId, ToolSlot>` 按 `resolution.id` 定位(`resume_tool` 的
  `running.remove(&resolution.id)`);每个 slot 持**预分配** `result_message_id`,`append_tool_response`
  按该固定 message id 落位,故最终 Conversation 与回灌顺序无关;仅当 `tool_batch_idle()`
  (running 空且无 awaiting_approval)才 `advance_tool_phase`。`parallel_tool_batch_resumes_out_of_order`
  测试断言乱序回灌结果一致。approval 三态:`approve` 转吐单个 `NeedTool`;`deny/timeout/cancel` 复用
  **与 legacy loop 同一** `approval::approval_response_for_decision`(grep 确认 legacy
  `loop_driver/default.rs:1136` 与 machine `tools.rs:455` 共用),合成 `ToolResponse` 追加走同一 append 路径,
  语义等价、非重复实现。
- **④ 受检 append,无 bypass(通过)**:机器对 Conversation 的**全部**变更仅经公共受检 API——
  `begin_turn`/`start_assistant_response`/`finish_assistant`/`register_tool_calls`/
  `append_tool_response`(真实结果 + 合成拒绝各一处)/`commit_pending`/`cancel_pending(DiscardTurn)`
  (失败清 pending);只读用 `head()`/`pending()`。grep 确认**无** `history_mut`/`committed_mut`/
  `push_message`/直接 `.history` 访问。`append_tool_response`(conversation/mod.rs:307)拒重复 message id、
  按 provider call id 关联开放调用、每调用只关一次——受检边界与 legacy 完全一致。

**验证命令(全绿)**:`cargo fmt --all -- --check`(clean);`cargo clippy --all-targets -- -D warnings`
(clean);`cargo test --lib agent::machine`(26 passed:13 tool + 13 既有);`cargo test --all --all-targets`
(409 lib + 8 integration = 417 passed / 0 failed,网络用例 ignored,较 M2-4 基线一致);
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean);`git diff --check`(clean)。M2 sans-io step
里程碑(text/tool/approval)审阅通过,可进入 M3 driver + drain。

---

## Milestone 3 — driver + drain 单层（迁移文档阶段 2）

### [DONE] M3-1 `HandlerScope` 与四个 handler trait

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

**完成记录**:

新建 `src/agent/drive.rs`(§6 机制层),从 `agent/mod.rs` 以 `pub mod drive` 导出并公开
re-export `HandlerScope`/`LlmHandler`/`ToolHandler`/`InteractionHandler`/`SubagentHandler`。

- **`HandlerScope`(`Send + Sync`)**:四个访问器 `llm()`/`tool()`/`interaction()`/`subagent()`
  各**默认返回 `None`**(该家族 pop 到外层),分别返回
  `Option<&dyn LlmHandler/ToolHandler/InteractionHandler/SubagentHandler>`。对象安全,供 M3-2
  `drain` 以 `&dyn HandlerScope` 组合。
- **四个 handler trait(`#[async_trait]`,`Send + Sync`)**:签名与 TODO 一致——
  `LlmHandler::fulfill(&self, request: &ChatRequest, mode: LlmStepMode, ctx: &RunContext)`、
  `ToolHandler::fulfill(&self, call_id: ToolCallId, call: &ToolCall, ctx: &RunContext)`、
  `InteractionHandler::fulfill(&self, request: &Interaction, ctx: &RunContext)`,均返回
  `RequirementResult`;`SubagentHandler::fulfill(&self, spec_ref: &AgentSpecRef, brief:
  &Interaction, result_schema: Option<&Value>, ctx: &RunContext)` **仅定义签名**(唯一
  scope-deepening 家族;派生 + 再开一层 drain 的实现留 M5,doc 标注)。所有 `await` 落在 handler
  内(真正做 IO),`step` 仍纯同步。
- **返回路径类型对齐**:模块 doc 写明"handler 返回的 `RequirementResult` 家族必须与其兑现的
  requirement kind 一致,失败编码进结果内(如 `Llm(Err(..))`)而非返回错家族",由 driver(M3-2)
  用 M1-1 `RequirementKind::accepts` 校验后再 `Resume`。
- **范围边界**:本任务只交付 trait 定义;`drain`/`Pop`/`UnhandledRequirement` 归 M3-2,真正包装
  client/registry/policy 的公共参考 driver + 复跑 50 集成测试归 M3-3。
- **聚焦测试(`#[cfg(test)]`,5 个)**:`EmptyScope`(无覆盖)断言四访问器全 `None`;`WrappedScope`
  挂 `LlmClient`/`ToolRegistry`/`ToolApprovalPolicy` 三个最小 fixture handler(不挂 subagent)断言
  前三 `Some`、subagent `None`;三个 `#[tokio::test]` 分别调 llm/tool/interaction handler 的
  `fulfill`,断言结果家族正确且通过对应 `RequirementKind::{NeedLlm/NeedTool/NeedInteraction}
  ::accepts`。interaction fixture 真实消费 `ToolApprovalPolicy`(`AutoApprove→approve`、
  `RequireApproval→deny`)。

**验证命令(全绿)**:`cargo fmt --all`(clean);`cargo clippy --all-targets -- -D warnings`
(clean);`cargo test --lib agent::drive`(5 passed);`cargo test --all --all-targets`
(414 lib + 8 integration = 422 passed / 0 failed,较 M2-R 基线 +5 新测试,网络用例 ignored);
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean);`git diff --check`(clean)。

### [DONE] M3-2 `drain` 参考实现与 pop 路由

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

**完成记录**:

`src/agent/drive.rs` 在 M3-1 的 trait 之上新增 §6 机制层的 drain/pop 参考实现,从 `agent/mod.rs`
以 `drain` / `Pop` / `ScopePop` / `TurnDone` 公开 re-export。

- **`AgentError::UnhandledRequirement { kind: RequirementKindTag, origin: AgentPath }`**
  (+ `AgentErrorKind::UnhandledRequirement`,`event.rs`):requirement 冒泡到顶层仍无 handler
  时的分类错误(顶层 scope 必须 total,§4.3),`kind()` 已接线。
- **`trait Pop`(`#[async_trait]`,`Send`)**:`pop(&mut self, requirement, ctx) ->
  Result<RequirementResult, AgentError>`,向外层转交一个 requirement 并取回其结果。
- **`struct ScopePop<'a> { scope, parent }` impl `Pop`**:把"外层 drain"表示成 pop 目标——
  先用本外层 scope 兑现,兜不了再向自身 parent 继续 pop,故 popped requirement **绝不回到
  它 pop 出来的那层**(§7.3 即时环防护)。
- **`pub async fn drain<M: AgentMachine + ?Sized>(machine, input, scope, parent, ctx) ->
  Result<TurnDone, AgentError>`**:喂 `External(input)` 后循环——每步收集 `Notification`,对
  本步 requirement 批:本层能兜的用 `FuturesUnordered` **并发兑现、按完成顺序** `Resume`(决策
  B),兜不了的顺序 pop 给 parent;`RequirementKind::accepts` 校验返回家族对齐(错则
  `AgentError::Other`),直至 `pending` 空且 cursor ∈ {Done, Error} 返回 `TurnDone`。
- **`struct TurnDone { notifications, cursor }`**:一趟 drain 的通知汇总 + 终态 cursor
  (阶段 2 直接透传通知,§12 决策 C)。
- **范围边界**:真正包装 `LlmClient`/`ToolRegistry`/`ToolApprovalPolicy` 的公共参考 driver +
  复跑 50 集成测试归 M3-3;`SubagentHandler` 实现(唯一 scope-deepening,§7.2)归 M5。
- **聚焦测试(`#[cfg(test)]`,新增 5 个,合计 10)**:`BatchMachine` fake 机器一次吐一批、按 id
  路由乱序 resume;`drain_fulfills_locally_without_popping`(本层兑现不冒泡)、
  `drain_pops_to_parent_when_local_scope_lacks_handler`(本层无→pop 到 parent 兑现)、
  `drain_top_scope_without_handler_is_unhandled_requirement`(顶层无→`UnhandledRequirement`,
  断言 kind/origin)、`pop_starts_from_outer_scope_skipping_the_emitter`(§4.4/§7.3:headless
  内层无 interaction→pop 到 attended 外层兑现,内层不回灌)、
  `drain_resolves_a_concurrent_batch_out_of_order`(三 `NeedTool` 按 delay 反序完成,按完成
  顺序 resume,结果按 id 一致、机器 Done)。

**验证命令(全绿)**:`cargo fmt --all`(clean);`cargo clippy --all-targets -- -D warnings`
(clean);`cargo test --lib agent::drive`(10 passed);`cargo test --all --all-targets`
(419 lib + 8 integration = 427 passed / 0 failed,较 M3-1 基线 +5 新测试,网络用例 ignored);
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean);`git diff --check`(clean)。

### [DONE] M3-3 参考 driver：复跑现有 loop 集成测试

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

**完成记录**:

新增 `src/agent/drive/reference.rs` 生产模块——把 M3-1 的四个 handler trait 落到真实运行时后端上,
构成迁移文档 §10 阶段 2 的“单层参考 driver”。从 `agent/mod.rs` re-export
`LlmClientHandler` / `ToolRegistryHandler` / `ApprovalInteractionHandler` / `ReferenceScope` /
`drive_turn`。

- **`LlmClientHandler`(impl `LlmHandler`)**:包 `Arc<dyn LlmClient>`;按 requirement 携带的
  `LlmStepMode` 分流——`NonStreaming` 走 `chat`,`Streaming` 走 `chat_stream` 再用
  `stream::accumulator::collect` 折回完整 `Response`;传输错误装在 `RequirementResult::Llm` 的
  `Err` 里(不改变结果家族)。
- **`ToolRegistryHandler`(impl `ToolHandler`)**:包 `Arc<dyn ToolRegistry>`,`execute(call_id,
  call)` 兑现 `NeedTool`;执行失败装在 `RequirementResult::Tool` 的 `Err`,由 machine 在回程按
  `ToolFailurePolicy` 处理。
- **`ApprovalInteractionHandler`(impl `InteractionHandler`)**:以固定 `ApprovalDecision` 应答
  approval interaction(`approve()` / `deny(msg)` / `new(decision,msg)`)。它是返回路径的决策源
  (attended UI 或 unattended 默认处置);“哪些调用需要审批”仍由 machine 自身的
  `ToolApprovalPolicy`(auto vs require 分流)在上游决定,与 legacy loop 完全一致。非审批
  interaction 以同族平凡应答对齐类型(machine 从不发)。
- **`ReferenceScope`(impl `HandlerScope`)**:`new(client, registry)` 建无 interaction 的 headless
  层;`with_interaction(handler)` 挂上后成为 attended 层(§4.4 / §6 “运行模式 = scope 差异”);
  顶层 total,未兜的 requirement 即 `UnhandledRequirement`。
- **`drive_turn(machine, input, scope, ctx)`**:`drain(.., None, ..)` 的薄封装,把一个
  `AgentInput` 跑完一个 turn,返回 `TurnDone`(通知汇总 + 终态 cursor)。
- **范围边界**:嵌套 scope 与 `SubagentHandler`(唯一 scope-deepening,§7.2)仍归 M5;本任务只建
  顶层单层。

**等价性测试(`src/agent/drive/reference/tests.rs`,新增 6 个)**:复用从
`loop_driver/default/tests.rs` 迁移来的 fake(`FakeClient` / `FakeToolRegistry` /
`RequireApprovalPolicy` / `FakeToolIds` / `ScriptedRequirementIds`),逐一对照
`DefaultAgentLoop` 对应用例的 Conversation committed 终态与通知序列:

- `reference_text_only_matches_default_loop`:纯文本 turn,cursor=Done(text),committed 两条消息
  (user + assistant),`usage` 一致;通知 `[StepBoundary(turn_count=1)]`。
- `reference_single_tool_matches_default_loop`:单工具调用→结果→收束文本,pairing 的
  `call_id`/`result_msg` 一致;通知含 `ToolCallStarted`/`ToolCallFinished` 与两个 `StepBoundary`。
- `reference_parallel_tools_matches_default_loop`:一批两工具并发兑现(`FakeToolRegistry` 无内部
  await,按 push=call 序完成),两 pairing 按 `[a,b]` 对齐,机器 Done。
- `reference_tool_failure_self_heal_matches_default_loop`:工具执行失败→错误结果回灌→模型自愈续跑,
  committed 序列与通知与 loop 用例一致。
- `reference_approval_approve_matches_default_loop`:`ReferenceScope::with_interaction(approve())`,
  require-approval 的调用经 interaction 批准后执行。
- `reference_approval_deny_matches_default_loop`:用 `ScriptedApprovalInteraction`(按 `call_id`
  逐调用给 deny/timeout/cancel 决策)+ 复用 `ReferenceScope` llm/tool 的 `ComposedScope`,断言
  三调用分别 Denied/Denied/Cancelled、无实际执行、模型收束文本一致。

`DefaultAgentLoop` 及其原 50 个集成测试保持不动(并存),新参考 driver 测试作为等价性证据。

**验证命令(全绿)**:`cargo fmt --all`(clean);`cargo clippy --all-targets -- -D warnings`
(clean);`cargo test --lib agent::drive::reference`(6 passed);`cargo test --all --all-targets`
(425 lib + 8 integration = 433 passed / 0 failed,较 M3-2 基线 +6 新测试,网络用例 ignored);
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean);`git diff --check`(clean)。

### [DONE] M3-R Milestone 3 Review

**前置依赖**:M3-1..M3-3。

**做什么**:

- 核对 pop 路由四条规则(本层兑现不冒泡 / 无则 pop / 顶层报错 / 从外层起防即时环)有测试覆盖。
- 核对"运行模式 = scope 差异":同一 machine 在挂/不挂 interaction handler 下行为差异有测试。
- 核对参考 driver 与 `DefaultAgentLoop` 在 text/tool/approval 的等价性证据充分。
- 确认 `UnhandledRequirement` 是分类错误、不静默跳过或挂起。

**验证**:运行全套命令。Review 结论写入完成记录。

**完成记录(Review 结论)**:

里程碑 3(`HandlerScope` + 四 handler trait / `drain` + pop 路由 / 单层参考 driver)通过评审。逐条核对:

- **pop 路由四条规则均有专测**(`src/agent/drive.rs` 单测):
  1. *本层兑现不冒泡* → `drain_fulfills_locally_without_popping`:`parent=None`、两个 tool
     requirement 在本层并发兑现,`tool.calls==2`、cursor `Done`,无 pop。
  2. *本层无 handler → pop* → `drain_pops_to_parent_when_local_scope_lacks_handler`:inner 仅
     handle tool,interaction 弹到 outer(`outer_interaction.calls==1`、`inner_tool.calls==0`)。
  3. *顶层无 handler → 分类错误* → `drain_top_scope_without_handler_is_unhandled_requirement`:
     空顶层 scope 上的 interaction 返回 `AgentError::UnhandledRequirement { kind:Interaction,
     origin:root }`,`error.kind()==UnhandledRequirement`。
  4. *从外层起防即时环* → `pop_starts_from_outer_scope_skipping_the_emitter`:弹出的 interaction
     只在 outer 兑现一次,inner/outer 的 tool handler 均未被回绕触达。
     机制层面 `ScopePop::pop` → `resolve_requirement(.., self.scope, self.parent, ..)` 只对*外层*
     scope 兑现、失败再向外弹,从不回到发出者自身 scope;并发批次乱序回灌另有
     `drain_resolves_a_concurrent_batch_out_of_order` 佐证。
- **"运行模式 = scope 差异"有测试**:同一 `DefaultAgentMachine` + `RequireApprovalPolicy` 配置下,
  挂 interaction 后端(`ReferenceScope::with_interaction(approve())`)时审批在本层兑现、工具执行、
  turn 收束(`reference_approval_approve_matches_default_loop`);本次新增
  `reference_headless_scope_surfaces_unhandled_approval`——**同一 machine**改用 headless 顶层
  scope(无 interaction 后端)时,审批 requirement 冒泡到顶层无兜底,得到分类
  `UnhandledRequirement { kind:Interaction }`,被守卫的工具从不执行(`registry.calls().is_empty()`),
  直接对照 attended 路径证明"行为差异仅由 scope 接线决定"。
- **参考 driver 与 `DefaultAgentLoop` 等价性证据充分**:6 个 `reference_*_matches_default_loop`
  覆盖 text-only / single tool / parallel tools / tool-failure self-heal / approval-approve /
  approval-deny,逐一断言 committed `Conversation` 终态(消息、pairing、`ToolStatus`、usage)与
  `Notification` 序列(`StepBoundary` / `ToolCallStarted` / `ToolCallFinished`)与 legacy 用例一致;
  `DefaultAgentLoop` 及其原集成测试保持不动、并存。
- **`UnhandledRequirement` 为分类错误**:`drain` / `fulfill_batch` / `resolve_requirement` 在
  `parent=None` 且无 handler 时一律返回 `AgentError::UnhandledRequirement { kind, origin }`(带
  family tag + origin 可寻址),绝不静默跳过或挂起;`AgentErrorKind::UnhandledRequirement` 分类由
  上述三处顶层测试断言。

评审中发现 check #2 的字面要求("同一 machine 挂/不挂 interaction 的行为差异")此前仅由
`BatchMachine` 机制测 + attended 参考测分别覆盖,缺少"同一 `DefaultAgentMachine` headless 变体"
的直接对照,遂在参考等价性测试中补齐 `reference_headless_scope_surfaces_unhandled_approval`(非
workaround,属评审范围内的覆盖补全)。

**验证命令(全绿)**:`cargo fmt --all`(clean);`cargo clippy --all-targets -- -D warnings`
(clean);`cargo test --lib agent::drive`(17 passed,含新增 1);`cargo test --all --all-targets`
(426 lib + 3+2+3 integration = 434 passed / 0 failed,较 M3-3 基线 +1 新测试,网络用例 ignored);
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean);`git diff --check`(clean)。里程碑 3 验收
通过,下一未完成任务 = M4-1。

---

## Milestone 4 — cancel / pivot 收编与删旧机制（迁移文档阶段 3）

### [DONE] M4-1 cancel = never-resume,接 `Conversation::cancel_pending`

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

**完成记录**:

cancel = never-resume 落地。`step(StepInput::Abandon(id))` 不再回灌任何结果,而是按 parked cursor
选择 disposition,由 machine 自己拥有的唯一 `Conversation` 触发 `cancel_pending` 闭合裂缝,再经
瞬态 `CancelRecovery` 收束到可 feed 的 `Idle`(迁移文档 §7)。

- **machine 实现**(`src/agent/machine/default/mod.rs`):
  - 新增 `abandon(id)`:先取 `cursor.pending_requirement_ids()` 校验 `id` 属于当前未决集合(不属
    → `fail`),再按 cursor 分派。借用先收成 `Option<(AbandonKind, StepId)>` 局部再放开,规避对
    `self` 的可变借用冲突。
  - `StreamingStep`(仅 LLM step 未决)→ `abandon_llm_step`:`cancel_pending(DiscardTurn)` 整体丢弃
    pending,reason `LlmInterrupted`。
  - `AwaitingTool`/`AwaitingApproval`(有 open tool call)→ `abandon_tool_phase`(tools.rs)。
  - `finish_cancel(step_id, reason)`:清空 `in_flight` scratch,`current → CancelRecovery → Idle`
    两跳(均为 `can_transition_to` 合法边),返回 quiescent outcome。
  - `begin_user_turn`:cursor==`Idle` 且残留 pending 时先 `cancel_pending(DiscardTurn)` 再
    `begin_turn`——tool-abandon 留下的是 *coherent* `Resumed` pending,新 user turn 取代它(否则
    `begin_turn` 因 `AlreadyPending` 失败)。仅在 `Idle` 触发,turn 中途行为不变。
- **tool-phase 闭合**(`src/agent/machine/default/tools.rs`):
  - `abandon_tool_phase`:`open_cancelled_results()` 枚举全部仍未闭合的 slot(`auto_pending` +
    `running` + `approval_pending` + `awaiting_approval`)生成 `CancelledToolResult`,恰好等于 pending
    仍缺 result 的 frozen call 集合,满足 `cancel_pending(ResumeTurn{cancelled_results})` 的一一闭合
    约束;已完成的 call 保留真实 result → 支撑"一批 tool 部分 abandon"。reason `ToolInterrupted`。
- **参考 driver**(`src/agent/drive.rs` `drain`):批兑现前检查 `ctx.is_cancelled()`;命中则对
  `pending[0]` 喂一次 `Abandon`(单次 abandon 即整 turn 闭合),`break` 返回 cursor=`Idle` 的
  `TurnDone`。`CancellationToken` 仍是向下"该停了"信号,闭合由 never-resume + `cancel_pending` 完成;
  未取消路径行为不变(现有 6 个 `reference_*_matches_default_loop` + drain 单测全绿)。

**新增测试**(9 个,全绿):

- machine(`tests/mod.rs`):`abandon_streaming_step_discards_turn_and_settles_idle`、
  `abandon_streaming_step_then_user_message_opens_new_turn`、
  `abandon_with_unmatched_requirement_id_fails`、`abandon_without_outstanding_requirement_fails`。
- machine tool-phase(`tests/tools.rs`):
  `abandon_tool_batch_synthesizes_cancelled_results_and_settles_idle`(2-tool 批,A 真结果 + B 合成
  取消,断言 `open_calls()==0`、`tool_calls()==2`、4 条 pending 消息)、
  `abandon_awaiting_approval_synthesizes_cancelled_result`、
  `abandon_tool_batch_then_user_message_opens_new_turn`(残留 pending 被新 turn 丢弃)。
- 参考 driver(`drive/reference/tests.rs`):`reference_cancel_during_tool_wait_abandons_turn`(LLM
  handler 返回 tool_use 时取消 token → drain 下一轮 `is_cancelled` → abandon 批,tool handler 若被调
  用即 panic;断言 cursor `Idle`、pending coherent、无 `ToolCallFinished`/`StepBoundary`)、
  `reference_new_turn_after_cancel_starts_fresh`(取消后再喂新 uncancelled turn → 丢弃残留、正常
  Done)。

**范围说明**:pivot 注入(`Pivot(_)` 仍 `fail`)属 M4-2,`respond_approval`/`AgentFeedGuard` 删除属
M4-3,本任务未触碰。`commit_text_turn` 收于 `Done`(终态)导致的 "Done 后多 turn" 是既有边界、非
M4-1 要求(abandon 收于 `Idle`,`Idle→StreamingStep` 可开新 turn),未改动。未新增 `Notification`
变体:machine 自持 Conversation,"machine 层触发 cancel_pending" 即满足 §7,合成取消 result 文本属
`cancel_pending` 内部,不在 outcome 中镜像(避免 scope creep)。

**验证命令(全绿)**:`cargo fmt --all`(clean);`cargo clippy --all-targets -- -D warnings`
(clean,修正 2 处 `collapsible_if` → let-chain,与既有 `&& let` 风格一致);
`cargo test --all --all-targets`(435 lib + 3+2+3 integration = 443 passed / 0 failed,较 M3-R 基线
426→435 即 +9 新测试,网络用例 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean);
`git diff --check`(clean)。

### [DONE] M4-2 pivot = 多喂 input,删除 pivot queue

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

**完成记录**:

pivot 改为"多喂一个 `AgentInput::Pivot` 输入",由 sans-io 机器在合法 step boundary 直接注入,库内
不再有 pivot queue(迁移文档 §2.2)。

- **machine 实现**(`src/agent/machine/default/mod.rs`):
  - 新增 `inject_pivot(pivot)`:要求 cursor 为 `StreamingStep`(否则 `fail`),取出其 `requirement_id`
    (`Copy`,借用先释放再 `conversation_mut()`),先做一次 `pivot.validate()` role 兜底(防 serde 绕过
    `new()` 造出非 user payload),再复用 `Conversation::inject_user_message(head, id, msg, meta)` 在闭合
    tool-result 边界追加 `Role::User` 消息;随后用**同一** `requirement_id` 重渲染并重发 `NeedLlm`,
    **不迁 cursor**(`StreamingStep→StreamingStep` 非合法 cursor 边;同 id 让 driver 天然把重发当替换),
    使 pivot 进入下一次生成、本 turn 从注入消息推进。
  - `step` 分派:`External(AgentInput::Pivot(pivot)) => inject_pivot`,删除旧 `Pivot(_) => fail` 与
    catch-all `External(_) => fail`。
  - 合法边界由 `inject_user_message` 自身把守:仅当 pending 处于闭合 tool-result 之后(post-tool
    `StreamingStep`)才接受;初始 `StreamingStep`(仅 user 消息)、`AwaitingTool`/`AwaitingApproval`
    (有 open tool call)、`Idle` 全部被拒 → `fail` → cursor `Error`,不破坏 open tool calls。
- **pivot 排队语义删除**:
  - `state/queue.rs`:`QueuedPivot::validate()` 提升为 `pub(crate)` 供 machine 兜底;新增
    `QueuedPivot::message_meta()`(写入 `pivot_source` extra + `PivotSource::label()` source 标签)与
    `PivotSource::label()`(`pivot:human`/`pivot:coordinator:{id}`/`pivot:skill:{id}`/`pivot:host:{label}`)。
    `PivotSource`/`QueuedPivot`(= `PivotMessage`)数据类型保留。
  - `state.rs`:删除 `queued_pivots` 字段、`queue_pivot`/`dequeue_pivot`/`queued_pivots()`、record 的
    serde 字段与 `from_record` 校验循环。
  - `event.rs`:`AgentInput` 收敛为 `{ UserMessage, Pivot }`,删除 `#[deprecated]` 的 `QueuedPivotTurn`
    与 `Resume` 两个变体及其 `QueuedPivotTurnInput`/`ResumeInput` 构造类型与 ctor;删除
    `AgentError::QueuedPivotPending`/`NoQueuedPivot` 及其 `kind()` 分支(`InvalidPivotRole` 保留)。
  - `mod.rs`:移除 `QueuedPivotTurnInput`/`ResumeInput` re-export。
- **legacy 参考 loop**(`src/agent/loop_driver/default.rs` + `loop_driver.rs`):
  - 删除 `AgentLoop::interject` trait 方法及 `DefaultAgentLoop`/`FakeLoop` 实现;`prepare_user_turn`
    收敛为 `{ UserMessage, Pivot }` 穷尽匹配(`Pivot` 走明确错误——直接注入归 machine,不在 legacy loop),
    删掉 `queued_pivots` 预检、`QueuedPivotTurn`/`Resume` 分支与 pivot 出队块。
  - `apply_pivots_at_pending_step_boundary` → `tool_result_step_boundary`:tool 批收尾只发普通
    `StepBoundary`,不再注入/记录 pivot;删除 `deferred_pivot_metadata`/`pivot_metadata`/`pivot_record`/
    `pivot_message_meta`/`pivot_source_label` 与 `InitialUserTurn.queued_pivot` 字段(`reconfig` 元数据链
    保留)。删除因 `Resume` 移除而失效的 `feed(Resume)` 续跑路径。

**测试变更**:

- 新增 machine pivot 测试(`tests/tools.rs`,5 个全绿):
  `pivot_at_post_tool_boundary_injects_user_message_and_reemits_same_requirement`(注入后同一
  `requirement_id` 重发 `NeedLlm`、请求末条为 pivot、pending 追加 user、resume 后本 turn 提交且消息 meta
  source==`pivot:human`)、`non_user_pivot_payload_is_rejected_at_boundary`(serde 造 assistant payload →
  cursor `Error`)、`pivot_before_any_tool_result_is_rejected`(初始 streaming step)、
  `pivot_with_open_tool_calls_is_rejected`(`AwaitingTool`)、`pivot_while_idle_is_rejected`。
- 删除随特性移除而失效的 legacy 测试:`state/tests.rs` 的 `agent_state_deserialize_rejects_invalid_queued_data`
  与 round-trip 中的 queue_pivot 片段;`loop_driver/default/tests.rs` 的
  `streaming_interject_does_not_interrupt_text_and_starts_next_pivot_turn`、
  `interject_rejects_invalid_pivot_role_without_queueing`、
  `pivot_and_reconfig_queues_share_final_boundary_without_interfering`、
  `parent_cancel_interrupts_open_tool_call_and_resume_feed_continues_turn`(`Resume` 续跑特性已删,等价覆盖
  见 M4-1 `reference_new_turn_after_cancel_starts_fresh`)、
  `streaming_tool_result_boundary_injects_queued_pivots_fifo_in_same_turn`、
  `rejected_pivot_is_reported_and_dropped_without_blocking_recovery`;连带清理只服务于这些测试的
  `queued_pivot_turn_input`/`pivot`/`pivot_records`/`BlockingToolRegistry`/`run_id_seed`/`streaming_tool_loop`
  helper 与 `loop_driver.rs` 的 mock（`input()` 改用 `AgentInput::pivot`)。
- 文档同步:`src/lib.rs`(pivot 注入描述、去掉 `interject`)、`README.md`(pivot=多喂 input、去掉 pivot
  queue)。迁移文档 §2.2/§10 为迁移叙述,已描述目标语义,无需改写。

**范围说明**:`respond_approval`/pivot queue 残留清理与 `AgentFeedGuard` 删除属 M4-3,本任务未触碰。移除
`AgentInput::Resume` 使 legacy loop 的 cancel-then-resume 续跑特性消失——这与 M4-1"cancel = never-resume"
一致,非回避,故删除对应测试而非 workaround。

**验证命令(全绿)**:`cargo fmt --all`(clean);`cargo clippy --all-targets -- -D warnings`(clean);
`cargo test --all --all-targets`(433 lib + 3+2+3 integration = 441 passed / 0 failed;较 M4-1 基线 443:
删除 ~8 个 legacy pivot/interject/resume 测试 + 1 个 state 测试、新增 5 个 machine pivot 测试;网络用例
ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean);`git diff --check`(clean)。

### [DONE] M4-2a 迁移 turn-boundary reconfig 应用进 sans-io 机器（含 registry 解析 effect）

**前置依赖**:M4-2。

**上下文**:reconfig-at-turn-boundary 应用目前**只有** legacy `DefaultAgentLoop` 实现
(`LoopRuntime::apply_queued_reconfigs_before_turn` 在 turn 起始前应用;
`prepare_queued_reconfig_application` 在 `finish_assistant`→`commit_pending` 处应用并写
`reconfigs` step-boundary metadata)。sans-io 机器(`src/agent/machine/`)与参考 driver
(`src/agent/drive/`)**零 reconfig 代码、零 reconfig 测试**(已用 `git grep` 在 HEAD 核实)。
`TODO.md` 与 `docs/agent-effect-migration.md` **完全没有 reconfig 迁移任务**——属计划遗漏,
本任务补齐。这是 M4-3 决策 E(删除 legacy loop)的**前置依赖**:直接删除 loop 会
(1) 回归 reconfig(`AgentState::queue_reconfig` 仍在,但无人在 turn 边界应用 queued reconfig,
队列累积却永不生效);(2) 使 loop 的 3 个 reconfig 测试失去等价覆盖(违反 M4-3 bullet 3);
(3) 让 `plan_reconfig_with`/`queue_prevalidated_reconfig`/`apply_reconfig_application`/
`ReconfigApplication::current_tool_set`/`ReconfigQueue::clear` 无生产调用者 →
`cargo clippy --all-targets -- -D warnings` 失败。

**关键难点**:reconfig 应用依赖 `LoopRuntime::resolve_reconfig_registry`,它调用
`tool_registry_resolver.resolve_tool_set(ToolSetRef)` —— 这是 **host I/O effect**,sans-io
机器不能内联执行,必须像 `NeedTool` 一样把 registry 解析 **reify 成一个 requirement/effect**,
由 driver 解析后回喂。

**做什么**:

- **reify registry 解析为 sans-io effect**:新增 `RequirementKind::NeedReconfigRegistry
  { tool_set }`(或等价)。机器在 turn 边界发现 queued reconfig 会改变 tool set 时发出该
  requirement;driver 用 `ToolRegistryResolver` 解析,校验 `registry.declarations()` 匹配
  `ToolSetRef.tools()`(不匹配 → fail),通过 `RequirementResult` 回喂解析后的 registry。
  当前 tool set 未变则短路、不发 effect(复用 `resolve_reconfig_registry` 的短路逻辑)。
- **机器在 turn 边界应用 queued reconfig**(`DefaultAgentMachine::begin_user_turn`):
  `queued_reconfig_application()` →(必要时)registry effect → `apply_reconfig_application()`
  → 切换 current tool set/registry → 在 step boundary metadata 写 `reconfigs` 记录(复用
  `reconfig_records`/`reconfig_metadata` 语义)。
- **提供机器 reconfigure/队列入口**(对应 M4-3 将移除的 `AgentLoop::reconfigure`):
  `plan_reconfig_with` 校验 → 解析 registry 校验 declarations → `queue_prevalidated_reconfig`。
  入口形态(专用方法 / `AgentInput` 变体 / 直用 `AgentState::queue_reconfig`)实现时定,须保证
  host 能向运行中的机器排队 reconfig。
- **参考 driver 串接**(`src/agent/drive/`):`ReferenceScope` 增加 tool-registry resolver,
  `drive_turn` 处理 `NeedReconfigRegistry`。
- 保持 `apply_reconfig_application`/`ReconfigApplication::current_tool_set`/
  `ReconfigQueue::clear`/`plan_reconfig_with`/`queue_prevalidated_reconfig` 全部有真实生产
  调用者(消除 dead code)。

**验证**:

- 聚焦测试:迁移 loop 的 3 个 reconfig 测试到机器/参考 driver 路径,等价覆盖——reconfig 排队于
  text turn → 下一 turn 边界应用且下次 `NeedLlm` 请求 tool set 变化;tool turn 期间保持本 turn
  registry 快照、reconfig 在其后应用;冲突 reconfig 请求原子拒绝。每个测试 <1min。
- 运行全套命令。

**范围说明**:本任务**不删除** legacy loop(loop 的 reconfig 实现保留作对照,直至 M4-3 删除);
机器与 loop 各操作自己的 `AgentState` 实例,reconfig 应用互不干扰,故可在 loop 存活时并行落地。

**完成记录**:

- **reify registry 解析为 effect**:`src/agent/requirement.rs` 新增 `RequirementKindTag::Reconfig`、
  `RequirementKind::NeedReconfigRegistry { tool_set: ToolSetRef }`、
  `RequirementResult::Reconfig(Result<(), ToolRuntimeError>)`,并补齐 `kind_of`/`result_of` 测试矩阵
  (`ALL_TAGS` 现为 5 元)。
- **cursor**:`src/agent/state/cursor.rs` 新增 `LoopCursor::AwaitingReconfig(ReconfigCursor)` 与
  `LoopCursorKind::AwaitingReconfig`,并在 `can_transition_to` 加入 `Idle→AwaitingReconfig`、
  `StreamingStep→AwaitingReconfig`、`AwaitingReconfig→{StreamingStep,CancelRecovery,Done,Error}`;
  经 `state.rs`/`agent/mod.rs` 重导出。
- **共享 metadata helper**:`reconfig_boundary_records`/`reconfig_boundary_metadata` 从 legacy loop 提取到
  `src/agent/state/queue.rs`,loop 与机器共用,保证 boundary metadata 逐字节一致(既有 loop reconfig 测试
  仍全绿,验证提取安全)。
- **机器 reconfig 逻辑**(`src/agent/machine/default/mod.rs`):新增
  `tool_registry_resolver`(默认 `DeclaredOnlyToolRegistryResolver`,`with_tool_registry_resolver` 可换)
  与 `reconfigure()` host 入口(`plan_reconfig_with` → `validate_reconfig_registry` → `queue_prevalidated_reconfig`);
  `begin_user_turn`/`commit_text_turn` 在 tool set 改变时发 `NeedReconfigRegistry` effect 并停在
  `AwaitingReconfig`,`resume_reconfig` 收到 `Reconfig(Ok)` 后应用并开/提交 turn。start-of-turn 应用不写
  metadata,during-turn commit 应用写 `reconfigs` metadata(resolve-before-commit)。
- **参考 driver**(`src/agent/drive.rs`、`src/agent/drive/reference.rs`):新增 `ReconfigHandler` trait、
  `HandlerScope::reconfig()`、`ReconfigRegistryHandler`;`ReferenceScope` 用共享可换 registry slot
  连接 tool + reconfig handler,`with_tool_registry_resolver` 注入解析器。
- **等价测试**:legacy 的 3 个 reconfig 测试迁移为 5 个新测试并全绿——
  `src/agent/machine/default/tests/reconfig.rs` 三个(during-text-turn 延后到 commit 且写 2 条 metadata、
  during-tool-turn 保持本 turn tool 快照、冲突请求原子拒绝),
  `src/agent/drive/reference/tests.rs` 两个(idle 排队于 turn 起始应用且请求变化、registry swap 端到端执行)。
  机器为单 turn(终态 `Done`),"下一次请求变化" 的观察落在参考 driver 测试。
- **验证**:`cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`(438 lib 测试全过,新增 5 个)、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check` 均通过。legacy loop 与其 reconfig 实现/测试保留不动(留待 M4-3 删除)。

### [DONE] M4-3 删除 `respond_approval`、pivot queue 残留与 `AgentFeedGuard`

**前置依赖**:M4-2、M4-2a。

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

**完成记录**:

- **决策 E = 彻底删除 `DefaultAgentLoop` 及整个 `loop_driver` 模块**(不重构为"薄 driver")。
  理由:sans-io `AgentMachine`(machine/default)+ 参考 driver(drive.rs / drive/reference.rs)
  已完整取代 loop 的自驱运行时,自 M3-3 起参考 driver 已按 `reference_*_matches_default_loop`
  等价复跑 loop 集成测试;删除 `respond_approval` + `ApprovalWaiters` 后 loop 审批路径已无法闭合,
  把 loop 重构成"薄 driver"只会重复 `drive_turn`/`ReferenceScope`;且 `DefaultAgentLoop`/`AgentLoop`
  未被任何集成测试(tests/)或 example 依赖。故删除是无 workaround 的收尾。
- **决策 F = 删除 `AgentFeedGuard`/`AgentFeedPermit`**(随 loop 一并删除):机器 `&mut self` +
  单活 turn 已提供背压,不再需要 feed guard 防重入。
- **删除**:整个 `src/agent/loop_driver.rs`(trait `AgentLoop`、`BoxAgentLoop`、
  `BoxAgentEventStream`、`AgentEventStream`、`AgentFeedGuard`、`AgentFeedPermit`、
  `respond_approval` 默认方法)、`src/agent/loop_driver/default.rs`(`DefaultAgentLoop`、
  `LoopRuntime`、`ApprovalWaiters`、`NonStreamingSegment`、`StreamingSegment`)、
  `src/agent/loop_driver/default/tests.rs`(约 18 个 loop 单测);并移除随之无意义的
  `AgentError::FeedInProgress` / `AgentErrorKind::FeedInProgress`。
- **迁移**:`LlmStepMode`(+`request_stream_flag`)迁到 `src/agent/requirement.rs`(它是
  `NeedLlm` 的载荷类型),`agent/mod.rs` 改从 requirement 重导出。
- **等价覆盖**:loop 集成测试的绝大多数场景已被 machine/reference 测试覆盖(text、streaming
  transport、single/parallel tool、tool 错误自愈、approval approve/deny/cancel、cancel 丢弃在途、
  client 错误丢弃 pending、reconfig text/tool/conflict/idle)。缺口补 3 个 machine 单测:
  `llm_invalid_assistant_response_moves_cursor_to_error_and_discards_pending`
  (`tests/mod.rs`)、`duplicate_framework_tool_call_id_moves_cursor_to_error_and_discards_pending`
  与 `unknown_provider_call_result_moves_cursor_to_error_and_discards_pending`(`tests/tools.rs`,
  含 `ScriptedToolIds::with_tool_call_ids` 构造 duplicate id)。streaming delta 转发按迁移文档
  §12-D 仍延后,机器 `streaming_mode_requests_stream_transport` 覆盖传输选择。
- **文档**:更新 `lib.rs`/`agent/mod.rs` 模块级 doc(改述为 machine + `Requirement` + 参考
  driver)、`README.md` Agent 层段落,以及 event.rs / approval.rs / interaction.rs / request.rs /
  tool.rs / state/cursor.rs / machine/default/mod.rs / drive/reference*.rs 里指向已删符号的
  rustdoc 链接与措辞。`PLAN.md`/`DESIGN.md` 为阶段/历史设计文档,未改。
- **验证**:`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`(lib 423 测试全过:removed ~18 loop 测试、新增 3 machine 测试)、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check` 均通过。每测试 <1min。

### [DONE] M4-R Milestone 4 Review

**前置依赖**:M4-1..M4-3(含 M4-2a)。

**做什么**:

- 核对 cancel=never-resume 的"受控丢弃 + 闭合"语义:被弃子树都触发了 `cancel_pending`,
  且"cancel 后仍可 feed"有测试。
- 核对 pivot/approval/cancel 三者已收编为"requirement + handler + 多喂 input"的统一表现,
  旧三套并列机制(pivot queue / approval responder / cancel token 主体)已删除或降级。
- 确认无 multishot / continuation 复制被引入;多路径路径仍指向 `fork_at`。

**验证**:运行全套命令。Review 结论写入完成记录。

**完成记录**:

Milestone 4 三条验收全部通过,未发现遗漏或 workaround。纯 review 任务,仅改本文件文档,
无源码改动;复跑全套验证确认 M4 收编后的机器/参考 driver 处于全绿态。

- **核对点 1 — cancel = never-resume 的"受控丢弃 + 闭合"**:通过。
  - 被弃子树都经**唯一** `Conversation::cancel_pending` 闭合裂缝(共 5 处生产调用):
    LLM step abandon → `abandon_llm_step` 用 `DiscardTurn`(`machine/default/mod.rs:750`);
    reconfig abandon → `abandon_reconfig` 用 `DiscardTurn`(`mod.rs:769`);tool/approval phase
    abandon → `abandon_tool_phase` 用 `ResumeTurn { cancelled_results }`,由
    `open_cancelled_results()` 对全部仍未闭合 slot(`auto_pending`+`running`+`approval_pending`+
    `awaiting_approval`)合成 `CancelledToolResult`,已完成 call 保留真实 result(部分 abandon 也闭合;
    `tools.rs:634`);`begin_user_turn` 残留 pending 先 `DiscardTurn` 再开新 turn(`mod.rs:300`);
    `fail` 路径 `DiscardTurn` 收尾(`mod.rs:818`)。`abandon(id)` 先 `pending_requirement_ids()`
    校验归属(不属即 `fail`)。
  - 参考 driver `drain` 检查 `ctx.is_cancelled()`,命中即对 `pending[0]` 喂一次 `Abandon`,
    `break` 返回 cursor=`Idle` 的 `TurnDone`;`CancellationToken` 仅作向下"该停了"信号,闭合由
    never-resume + `cancel_pending` 完成(`drive.rs:357-366`)。
  - "cancel 后仍可 feed"有测试:`abandon_streaming_step_then_user_message_opens_new_turn`
    (`machine/default/tests/mod.rs:429`)、`abandon_tool_batch_then_user_message_opens_new_turn`
    (`tests/tools.rs:953`)、参考 driver `reference_new_turn_after_cancel_starts_fresh`
    (`drive/reference/tests.rs:1163`);另有 `reference_cancel_during_tool_wait_abandons_turn`
    (`tests.rs:1115`)与 4 个 abandon 语义单测。集成层 `parallel_tool_cancel_resume_keeps_state_machine_usable`
    (`tests/conversation_state_machine.rs`)亦通过。
- **核对点 2 — pivot/approval/cancel 收编为 requirement + handler + 多喂 input**:通过。
  - **旧三套并列机制已删除/降级**:
    - pivot queue → 删除。`git grep` 确认无 `queued_pivots` 字段、`queue_pivot`/`dequeue_pivot`
      方法、`AgentInput::QueuedPivotTurn`/`AgentInput::Resume` 变体。`AgentInput` 收敛为
      `{ UserMessage, Pivot }`;`PivotSource`/`QueuedPivot`(=`PivotMessage`)保留为**数据类型**。
    - approval responder → 删除。`git grep` 确认无 `respond_approval`/`ApprovalWaiters`。审批统一走
      `RequirementKind::NeedInteraction { request: Interaction::approval(..) }`(`tools.rs:305,320`)
      发出、`RequirementResult::Interaction(InteractionResponse::Approval(ApprovalResponse))` 回喂
      (`tools.rs:400-457`);旧审批类型降级为 `InteractionKind::Approval` 内嵌的**数据后端**。
      `ApprovalError` 仅作数据错误类型保留(`event.rs:726` `#[from]`),`AlreadyPending`/`ResponderGone`
      已无生产构造点。
    - cancel token 主体 → 降级为纯向下信号(见核对点 1),闭合逻辑不再依赖 token 内部状态。
  - **整个 `loop_driver` 模块已删除**:`git grep` 确认 src 内无 `loop_driver`/`DefaultAgentLoop`/
    `AgentLoop`/`AgentFeedGuard`/`AgentFeedPermit`/`FeedInProgress`。背压由机器 `&mut self` + 单活 turn
    提供(决策 F)。三者现均为"发 requirement → handler 兑现 → `StepInput::Resume`/`Abandon` 回喂"的统一
    表现:approval=Resume(Interaction)、pivot=多喂 `External(Pivot)`、cancel=Abandon(never-resume)。
- **核对点 3 — 无 multishot / continuation 复制;多路径仍走 `fork_at`**:通过。
  - `git grep` 确认无 `multishot`/`multi_shot`/`Multishot` 机制。"continuation" 仅为普通助手续写
    消息/步骤的描述词(post-tool 同 turn 续写),非分支复制。机器不克隆自身 state 去并行探索多路径。
  - 会话级多路径分支的唯一原语是 `Conversation::fork_at`(`conversation/boundary/fork.rs:71`),
    cancel/pivot/approval 均为 feed 驱动的单机器推进,不引入 continuation 复制。

**验证命令(全绿)**:`cargo fmt --all --check`(clean);`cargo clippy --all-targets -- -D warnings`
(clean);`cargo test --all --all-targets`(423 lib + 2+3 integration state-machine/adapter 全过,网络用例
ignored,与 M4-3 基线 423 一致);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean)。每测试 <1min。
本任务仅改 `TODO.md`;因源码自 M4-3 全绿以来未变,全套结果为对该态的复核确认。

---

## Milestone 5 — hierarchy / subagent（迁移文档阶段 4）

### [DONE] M5-1 嵌套机器状态与 `AgentPath` 落位

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

**完成记录**:

- **新增 `src/agent/machine/nested.rs`**(+ `nested/tests.rs`):live `NestedMachine`(实现
  `AgentMachine`)与可序列化快照 `MachineTreeState`。
  `NestedMachine { own: DefaultAgentMachine, children: BTreeMap<AgentSlot, ChildNode>,
  path: AgentPath }`,`ChildNode { machine: NestedMachine, pending_start: Option<AgentInput> }`
  递归树;`path` 为节点在树中的**绝对路径**,不入 serde(由 `from_state` 按结构重建)。
- **`step` 递归推进整树**:`step(External)` 经 `step_own` 喂 own,再 `start_pending_children`
  用各子存的 opening `AgentInput` 开启所有未开子机器(一次 feed 推进整树);
  `step(Resume)`/`step(Abandon)` 经 `route_by_id` 扫描各 cursor 的 `pending_requirement_ids`
  与 `subtree_contains` 按 **id** 精确定位命中节点后投递,无节点等待该 id 时投给 own 让其分类报错。
  `cursor()` 返回 root 的 `own.cursor()`。
- **真实 `AgentPath` 落位(bullet 2 & 4,统一机制)**:单机 `DefaultAgentMachine` 恒把 emitted
  `Requirement.origin` 与 cursor 绑定打在 root;每节点在 own 机步进后由 `step_own` 把 own 刚产出的
  requirement(`stamp_requirements`)与 own cursor 绑定(`rebase_cursor_origin`)重打成本节点的
  `path`,子节点递归自打故冒泡=纯 append。据此 `StepOutcome.requirements` 与持久化 cursor 绑定
  **同源一致**地携带真实绝对路径;`outstanding_requirements()` 由结构重建同一 `(id, path)` 视图。
- **cursor 打戳链(新增)**:`LoopCursor::rebase_origin(&AgentPath)`(同模块直改
  `CursorRequirement`/`ToolWaitRequirements` 私有 origin,仅改寻址元数据不过 transition 校验)
  ← `AgentState::rebase_cursor_origin`(`pub(crate)`)← `DefaultAgentMachine::rebase_cursor_origin`
  (`pub(crate)`)。requirement-free cursor(Idle/CancelRecovery/Done/Error)不变。
- **serde**:`impl Serialize for NestedMachine`(借用 `own.state()`,递归子树 `ChildStateRef`,含
  `pending_start`;无子时跳过 `children`)+ `MachineTreeState`/`ChildState`(`Deserialize`,
  `deny_unknown_fields`)+ `NestedMachine::from_state(state, make)` 递归 `from_state_at` 按结构
  重建各节点 `path` 并重注入 handle。
- **序列化边界(遵循既有不变量,非 workaround)**:单机 parked(卡在 NeedLlm)时 Conversation 有
  pending turn,Conversation 核心明确拒绝快照(`serializing_state_with_pending_conversation_is_rejected`
  等既有测试确立);故整树仅在 committed 边界(Idle/Done)可序列化,与既有
  `agent_state_serde_round_trips_through_conversation_snapshot` 的 serde 模型一致。round-trip
  聚焦测试遂以 parent=Done + child=Idle(保留 `pending_start`)两个不同 cursor 独立恢复,并验证
  恢复后子机器凭 `pending_start` 在下一次 feed 以真实路径 `[slot]` 重新开启。
- **导出**:`machine/mod.rs` `mod nested;` + re-export `MachineTreeState`/`NestedMachine`/
  `NestedMachineError`;`agent/mod.rs` 同步 re-export。
- **聚焦测试(nested,4 个全绿)**:`step_aggregates_parent_and_child_requirements_with_real_paths`
  (父 origin=root、子 origin=`[slot]`,且各自 cursor 绑定 origin 落真实路径)、
  `resume_routes_by_id_to_the_child_only`(按 id 只命中子,父保持 parked)、
  `whole_tree_round_trips_and_each_cursor_restores_independently`、
  `attach_child_rejects_an_occupied_slot`。移除随实现不再使用的 `AgentPath::prepend` 及其单测
  (绝对打戳不需前缀一跳)。
- **验证**:`cargo fmt --all`(clean)、`cargo clippy --all-targets -- -D warnings`(0 warning)、
  `cargo test --all --all-targets`(lib 427 passed / 0 failed;新增 4 个 nested 聚焦测试、移除 1 个
  `AgentPath::prepend` 单测;网络用例 ignored)、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean)、
  `git diff --check`(clean)均通过。每测试 <1min。

### [DONE] M5-2 `SubagentHandler`:派生、再开一层 drain 与作用域强制

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

**完成记录**:

- **`RunContext` 加深度(`context.rs`)**:新增私有 `depth: u32`,`new_root=0`,`derive_child`
  `saturating_add(1)`,`pub const fn depth()` 访问器。深度护栏语义集中在派生链的唯一加深处
  (迁移文档 §7.2 / `agent-layer.md` §6.3),不散落别处。新增单测
  `depth_starts_at_zero_and_increments_per_derived_child`(root=0、child=1、grandchild=2,父不受影响)。
- **深度错误分类(`event.rs`)**:新增 `AgentErrorKind::Subagent` 与
  `AgentError::SubagentDepthExceeded { limit, depth }`,`AgentError::kind()` 映射到 `Subagent`。
- **drain 管道让 `NeedSubagent` 走「构造 outer 后串行兑现」路径(`drive.rs`)**:
  - `SubagentHandler::fulfill` 签名新增 `outer: &mut dyn Pop`(= 发出 `NeedSubagent` 那层 scope 及其
    父,作为子机器未兑现 requirement 的 pop 目标,§7.3)。
  - `fulfill_with_scope` 对 `NeedSubagent` 返回 `None`(它需要 outer,只能在 `resolve_requirement`
    构造),`resolve_requirement` 特判 `NeedSubagent`:有 handler 则 `ScopePop::new(scope, parent)`
    构造 outer 调 `fulfill`,否则落到既有 pop 路径。
  - `fulfill_batch` 把 `Subagent` 从并发集排除、与不可本层兑现者一起串行经 `resolve_requirement`
    (subagent handler 需要 `&mut parent` 作 pop 目标,无法进 `FuturesUnordered` 并发集)。
- **生命周期修复(恢复上一轮中断留下的编译错误)**:`ScopePop` 单一 `'a` 让 `scope` 与
  `parent: &mut dyn Pop` 的 pointee 生命周期被迫统一(`&mut` pointee 不变),导致在
  `resolve_requirement` 内为 subagent 构造 `ScopePop` 无法统一 scope/parent 两个独立生命周期。改为
  **双生命周期 `ScopePop<'a, 'p>`**(`parent: Option<&'a mut (dyn Pop + 'p)>`)解耦借用寿命与 pointee
  寿命;`resolve_requirement` 恢复独立省略生命周期。既有 `ScopePop::new(&outer, None)` 调用点不变。
- **参考实现 `src/agent/drive/subagent.rs`**(+ `subagent/tests.rs`):
  - `SpawnedChild { machine: Box<dyn AgentMachine + Send>, scope: Box<dyn HandlerScope>, opening:
    AgentInput }`——子机器、其**自有** drain 层、开启子 turn 的输入。
  - `trait SubagentSpawner`(host 策略,`Send+Sync`):`child_ids`(供 `derive_child` 的 run/trace id)、
    `spawn`(把 `AgentSpecRef` 解析为可驱动子)、`summarize`(把 `TurnDone` 收敛为 `SubagentOutput`)。
    库只拥有加深作用域的**机制**,把「spec→机器/scope」的**策略**留给 host。
  - `DrivingSubagentHandler { spawner, max_depth }` 实现 `SubagentHandler`:①深度护栏先行
    (`ctx.depth() >= max_depth` → `Subagent(Err(SubagentDepthExceeded))`,不 mint id 不 spawn);
    ②`derive_child`(共享父 budget ledger + 派生 cancel 链 + 记 sub-agent trace,预算继承/cancel 传播
    天然获得);③`spawn` 子;④`drain(child, opening, child_scope, Some(outer), child_ctx)` 再开一层;
    ⑤`Ok→summarize→Subagent(Ok)`,`Err→Subagent(Err)`。`max_depth==0` 即禁用 subagent。
- **导出**:`drive.rs` `mod subagent;` + `pub use subagent::{DrivingSubagentHandler, SpawnedChild,
  SubagentSpawner}`;`agent/mod.rs` 同步 re-export。
- **聚焦测试(subagent,4 个全绿,均用 mock 机器/scope 精确观测 handler 自身接线)**:
  `attended_parent_serves_headless_child_interaction_via_pop`(§7.3:headless 子 `NeedInteraction`
  pop 到 attended 父 scope 被兑现 count==1,父/子均完成,父被 `Subagent` 结果 resume)、
  `depth_guard_refuses_at_limit_without_spawning`(depth==max_depth==1 → `SubagentDepthExceeded
  {limit:1,depth:1}`,spawner 零调用)、`parent_cancel_propagates_and_abandons_child`(父 ctx cancel
  → 子 drain 见 cancel → `Abandon` 首个 requirement 的 never-resume 收尾,子 LLM handler 零调用、零
  resume)、`child_token_charge_counts_against_parent_budget`(子 LLM handler 在派生 ctx 上
  `charge_tokens(42)` → 父 ctx budget snapshot tokens==42,证明共享 ledger)。
- **验证**:`cargo fmt --all`(clean)、`cargo clippy --all-targets -- -D warnings`(0 warning)、
  `cargo test --all --all-targets`(lib 432 passed / 0 failed;含新增 1 个 depth 单测 + 4 个 subagent
  聚焦测试;网络用例 ignored)、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean)、
  `git diff --check`(clean)均通过。每测试 <1min。

### [DONE] M5-3 Observability:trace 记 resolved-by-scope 与 disposition

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

**完成记录**:

- **`RequirementDisposition` 与 `TraceNodeKind::Requirement`(`context/trace.rs`)**:新增
  `pub enum RequirementDisposition { Resumed, NeverResumed }`(Copy + serde `snake_case`),并给
  `TraceNodeKind` 补 `Requirement { kind_tag: RequirementKindTag, resolved_at_scope: u32,
  disposition: RequirementDisposition }` 结构变体。三字段全 `Copy`,故 `TraceNodeKind` 仍 `Copy`、
  `TraceRecord::kind()` 签名不变(零破坏)。`trace.rs` 引 `crate::agent::requirement::RequirementKindTag`。
  新增 `TraceHandle::record_requirement(id, kind_tag, resolved_at_scope, disposition)` 走既有
  `record_node`(沿用 dup-id / unknown-parent 校验),节点挂在**执行该 requirement 的那层** trace
  parent(root 或 sub-agent 节点)下。`context.rs` / `agent/mod.rs` 同步 re-export
  `RequirementDisposition`。
- **`resolved_at_scope` 语义 = pop 跳数**:从「perform 该 requirement 的那层 scope」向外 pop 到
  真正兑现处的跳数(0 = 本层就地兑现,每向外一层 +1)。这是动态作用域下最直接可测的「被哪层兑现」
  相对表示,天然经 pop 链累加,且与 trace 节点归属(挂在 emitting 层)组合后完整定位兑现位置。
- **hop 计数经 pop 链回传(`drive.rs`)**:
  - `Pop::pop` 返回类型 `Result<RequirementResult, AgentError>` → `Result<(RequirementResult,
    u32), AgentError>`,u32 = 从「本 pop 目标 scope」起到兑现处的跳数;唯一实现 `ScopePop` 原样透传。
  - `resolve_requirement` 同步改返回 `(RequirementResult, u32)`:本层兑现(subagent handler /
    `fulfill_with_scope`)返回 `(result, 0)`;pop 时 `let (r,h)=parent.pop(..)?; (r, h+1)`(+1 =
    跨到 parent 的那一跳);顶层无 handler 仍 `UnhandledRequirement`(不记录)。
  - 新增内部结构 `Resolved { resolution, resolved_at_scope }`;`fulfill_batch` 返回
    `Vec<Resolved>`:并发本层集 hop=0,串行集经 `resolve_requirement` 得 hop。
- **记录集中在 `drain`(单处、且只记「真会被 Resume/Abandon」的)**:`fulfill_batch` 只返回 Ok
  兑现(错误经 `?` 上抛、不记录半途失败),故 `drain` 对每个 `Resolved` 先
  `record_requirement_resolution(ctx, &resolution, resolved_at_scope, Resumed)` 再 `Resume`;
  cancel 分支对 `pending.first()` 先 `record_requirement(ctx, req, 0, NeverResumed)` 再 `Abandon`
  (cancel = 本层 never-resume handler,故 scope=0)。trace 节点 id **复用 host-minted requirement
  id**(库不造 id 哲学);记录失败经 `RunContextError::Trace` → `AgentError`(kind=`Trace`)。
  新增 `record_requirement` / `record_requirement_resolution` / `record_requirement_node` 三个私有
  helper。
- **聚焦测试**:
  - `drive.rs` `drain_records_resolved_at_scope_for_local_and_popped_requirements`:一批
    `[tool(本层), interaction(pop 到外层)]` → tool 节点 `resolved_at_scope==0`+`Resumed`、
    interaction 节点 `resolved_at_scope==1`+`Resumed`,kind_tag 分别为 Tool/Interaction。
  - `drive.rs` `drain_records_never_resumed_disposition_on_cancel`:cancel 的 ctx → 首个 requirement
    被 `Abandon`,tool handler 零调用,trace 记 `resolved_at_scope==0`+`NeverResumed`。
  - `context/tests.rs` `requirement_trace_node_round_trips_through_serde`:`Requirement` 变体
    serde round-trip,并核对 JSON 形状(`kind.requirement.{kind_tag,resolved_at_scope,disposition}`,
    disposition `never_resumed`)。
- **验证**:`cargo fmt --all`(clean)、`cargo clippy --all-targets -- -D warnings`(0 warning)、
  `cargo test --all --all-targets`(lib 435 passed / 0 failed;较上轮 +3 新测试;网络用例 ignored)、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean)、`git diff --check`(clean)均通过。
  每测试 <1min。既有 subagent / reference drain 测试(父子共享 trace 树、pop、cancel)全绿,证明新增
  记录不改行为、requirement id 在同一 trace 树内不冲突。

### [DONE] M5-R Milestone 5 Review

**前置依赖**:M5-1..M5-3。

**做什么**:

- 核对嵌套机器整树可序列化、requirement 按 `id + origin` 精确路由、父子并发兑现按完成顺序回灌。
- 核对深度上限、预算继承、cancel 传播全部在 subagent handler 强制(不散落别处)。
- 核对"同一 spec 在挂/不挂 interaction 的 scope 下 attended/headless 自动切换"有端到端测试。
- 核对 trace resolved-by-scope + disposition 完整。

**验证**:运行全套命令。Review 结论写入完成记录。

**完成记录**:

- **核对点 1 — 整树 serde + `id/origin` 路由 + 完成序回灌:通过**。`machine/nested.rs` 让
  `NestedMachine` 整树可序列化(`impl Serialize` 借 own `AgentState` + 递归 `ChildStateRef`;
  `MachineTreeState`/`ChildState` `Deserialize` + `deny_unknown_fields`;`from_state` 递归重建各节点
  `path` 并重注入 live handle)。requirement **按 id 精确路由**:`route_by_id` 先看 own cursor 的
  `pending_requirement_ids`,否则 `subtree_contains` 定位命中子树,无节点等待该 id 时投给 own 让其分类
  报错;**origin** 由 `stamp_requirements` + `rebase_cursor_origin` 打真实绝对 `AgentPath`,使
  `StepOutcome.requirements` 与持久化 cursor binding 同源一致,`outstanding_requirements()` 由结构
  重建同一 `(id, path)` 视图。**父子并发按完成顺序回灌**由 `drive.rs::fulfill_batch` 的
  `FuturesUnordered` 提供(本层集完成序收集;不可本层兑现者串行经 `resolve_requirement`)。测试:
  `step_aggregates_parent_and_child_requirements_with_real_paths`、`resume_routes_by_id_to_the_child_only`、
  `whole_tree_round_trips_and_each_cursor_restores_independently`、`attach_child_rejects_an_occupied_slot`、
  `drain_resolves_a_concurrent_batch_out_of_order` 全绿。
- **核对点 2 — 深度/预算/cancel 集中在 subagent handler:通过**。三护栏全部落在
  `DrivingSubagentHandler::fulfill`(`drive/subagent.rs`)这一处:①深度守卫先行
  (`ctx.depth() >= max_depth` → `AgentError::SubagentDepthExceeded`,不 mint id、不 spawn);
  ②`RunContext::derive_child`(`context.rs`)以 `budget.clone()` 共享同一 ledger(预算继承)、
  `cancellation.derive_child()` 派生子 token(cancel 传播)、`depth.saturating_add(1)`;
  ③child drain 的 pop 目标为 handler 收到的 `outer`,子未兑现 requirement pop 到外层而非回到自身
  (§7.3)。`CancellationToken`(`context/cancel.rs`)子观察父链,父 cancel 对所有后代可见。未在
  machine/其它 handler 处重复实现。测试:`depth_guard_refuses_at_limit_without_spawning`、
  `parent_cancel_propagates_and_abandons_child`、`child_token_charge_counts_against_parent_budget` 全绿。
- **核对点 3 — attended/headless 自动切换有测试:通过(机制已测;完整同 spec 双跑验收下沉 M6-2)**。
  `attended_parent_serves_headless_child_interaction_via_pop`(`drive/subagent/tests.rs`)证明**自动
  切换的核心机制**:同一子机器,其 `NeedInteraction` 因子 scope **不挂** interaction(headless)而 pop
  到**挂** interaction 的父 scope(attended)被兑现(count==1),子/父均 `Done`,父被 `Subagent` 结果
  resume——即"由挂/不挂 interaction 决定 headless/attended,子无需任何配置"。attended-本层直服方向由
  `drive.rs` 的 interaction handler 测试覆盖。**同一 subagent spec 用真实 `DefaultAgentMachine` + 离线
  fake client 两种 scope 各跑一次**的完整端到端验收示例是下游 **M6-2** 的专属任务(依赖链已正确:
  M6-2 ← M6-1 ← M5-R),本 Review 不重复该验收,亦无需新增 prerequisite。
- **核对点 4 — trace resolved-by-scope + disposition 完整:通过**。`context/trace.rs` 有
  `RequirementDisposition { Resumed, NeverResumed }`(Copy + serde snake_case)与
  `TraceNodeKind::Requirement { kind_tag, resolved_at_scope, disposition }`,`TraceNodeKind` 仍 `Copy`、
  `TraceRecord::kind()` 签名不变。记录集中在 `drive.rs::drain` 单处:Resumed 批经
  `record_requirement_resolution(ctx, &resolution, resolved_at_scope, Resumed)`,cancel 分支经
  `record_requirement(ctx, req, 0, NeverResumed)`(never-resume 是真实影响下层 Conversation 的事件,
  必留痕);`resolved_at_scope` = pop 跳数,经 `Pop::pop` 返回 `(result, hops)` 沿 pop 链 `+1` 累加,
  节点 id 复用 host-minted requirement id。测试:
  `drain_records_resolved_at_scope_for_local_and_popped_requirements`(本层 0 / pop 一层 1,均 Resumed)、
  `drain_records_never_resumed_disposition_on_cancel`(NeverResumed)、
  `requirement_trace_node_round_trips_through_serde`(serde 形状)全绿。
- **验证(本轮实跑,HEAD=f1ce9fb、工作树在核对前 clean、无源码改动)**:`cargo fmt --all -- --check`
  (clean)、`cargo clippy --all-targets -- -D warnings`(0 warning)、`cargo test --all --all-targets`
  (lib 435 passed / 0 failed;doctest 3 passed;集成/示例全绿;网络用例 ignored,需凭据;每测试 <1min)、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(clean)、`git diff --check`(clean)均通过。
- **结论**:Milestone 5(嵌套机器 + subagent handler + observability)四项核对全部通过,无 spec 偏差、
  无 workaround、无未排期失败测试,层次不变量(整树 serde、`id/origin` 路由、深度/预算/cancel 集中强制、
  trace 归属与处置)与迁移文档 §7–§9 一致。未引入新 prerequisite。PLAN.md 阶段级计划无变化,不改。

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
