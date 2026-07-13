# TODO：Agent Layer 实现任务列表

> 依据 [`PLAN.md`](PLAN.md)、规范性设计
> [`docs/agent-layer.md`](docs/agent-layer.md) 与 [`DESIGN.md`](DESIGN.md) §1.3。
> 任务按真实依赖顺序编号；coding agent 每次只执行首个标题未带 `[DONE]` 的任务，
> 完成后把该标题的 `[TODO]` 改为 `[DONE]` 并补充完成记录。已完成的 Conversation Core
> 计划和任务记录归档在
> [`docs/archive/2026-07-13-conversation/`](docs/archive/2026-07-13-conversation/)。

通用约束：不得重新实现或绕开 Conversation 的 committed log、pending、tool pairing、
`Boundary`、projection、cancel 和 restore 不变量；不得公开 unchecked Agent/Conversation
构造器、裸 mutable runtime state、可跳过 approval 的 tool 执行入口或 provider/task 私有
特判；id/时间由调用方注入；Agent 只持有一个活动 `Conversation`；pivot 只能注入
`Role::User` 消息，reconfig 只在 turn boundary 生效；每个测试用例必须在 1 分钟内完成。
每项的完整验证均按“format → 严格 clippy → 聚焦测试 → 全量测试 → rustdoc → diff check”
的顺序执行，全量测试最长 30 分钟。

---

## Milestone 1 — Agent 基础数据与 RunContext

### M1-1 [TODO] Agent identity、`AgentSpec` 与静态配置模型

**前置依赖**：Conversation Core 已完成并归档；直接复用 `client::ChatRequest` 可表达的
model/system/tool 声明边界，不改 Client wire 模型。

**上下文**：`docs/agent-layer.md` §1.1 要求 `AgentSpec` 是可 serde 的静态 identity/config，
不含 live conversation 或运行时句柄；`PLAN.md` 的关键决策要求 id 与时间外部注入。
后续 `AgentState`、spawn 和 skill activation 都会引用这些静态配置。

**做什么**：

- 新建 `src/agent/` 模块并从 `lib.rs` 导出，建立 `id.rs`、`spec.rs`、`mod.rs` 聚焦结构。
- 定义互不混淆的 `AgentId`、`RunId`、`StepId`、`ToolSetId`、`SkillId`、`PlanId`、
  `BlackboardId` newtype，只提供解析/serde/只读能力，不调用 RNG、时钟或自增。
- 定义字段私有的 `AgentSpec`，至少包含 `AgentId`、`WorktreeRef`、初始 system prompt、
  `ToolSetRef`、`ModelRef` 与 loop policy 参数；不持有 `Conversation`、`LlmClient`、
  `ToolRegistry` 或任何 runtime handle。
- 为公开类型补齐 rustdoc，说明 `AgentSpec` 是模板/配方，不是正在运行的 Agent。

**验证**：

- 聚焦测试覆盖全部 id serde/parse round-trip、不同 newtype 不能误用、`AgentSpec`
  serde 保留外部提供值、非法 UUID 拒绝、静态配置不含 conversation/runtime handle。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 agent spec 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M1-2 [TODO] `RunContext`、取消、预算与 trace handle 边界

**前置依赖**：M1-1。

**上下文**：`docs/agent-layer.md` §1.4 与 §6.3 要求 cancellation、budget、trace 贯穿
loop、tool 和子 agent；`DESIGN.md` §1.3 下向约束第 6 条要求下层 async 接口预留 run 级
上下文载体。该层应定义抽象边界，不把具体 executor 或全局单例写死。

**做什么**：

- 在 `agent::context` 中定义字段私有的 `RunContext`，包含取消、预算和 trace 三类 handle。
- 定义最小 trait/数据边界：取消可查询/派生，预算可按 step/token/cost/wall-clock 检查并返回
  分类错误，trace 可创建 run/step/llm/tool/sub-agent 节点记录。
- 明确 runtime handle 不 serde；只为可持久化 budget/trace record 定义 data DTO。
- 子 `RunContext` 必须从父 context 派生并继承 cancel、预算上限和 trace parent，不允许孤立构造
  绕过父级资源限制。

**验证**：

- 聚焦测试覆盖 cancel 传播、预算扣减/超限分类、trace parent 链、子 context 继承，以及
  `RunContext` live handle 不可 serde、data record 可 serde。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 context 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M1-3 [TODO] `AgentState`、唯一活动 Conversation 与 `LoopCursor`

**前置依赖**：M1-2。

**上下文**：`docs/agent-layer.md` §1.2 要求 Agent 严格持有一个活动 `Conversation`，
active skills 与 `LoopCursor` 是可恢复数据，tool registry 等句柄恢复时重建；Conversation
持久化必须继续走 `Conversation::snapshot`/`Conversation::restore`。

**做什么**：

- 在 `agent::state` 中定义 `AgentState`，持有 `AgentSpec`/spec id、唯一活动
  `Conversation`、active `SkillId` 列表、queued pivot/reconfig 数据和 `LoopCursor`。
- 定义 `LoopCursor` 的 data-only 状态，至少能表达 idle、streaming step、awaiting tool、
  awaiting approval、cancel recovery 与 done/error 等恢复位置。
- 将 tool registry、client、MCP session、approval responder、task handle 等 runtime 句柄放在
  单独 runtime holder/resolver 中，不进入 `AgentState` serde。
- 提供只读 getter 和受检状态转换；不提供可替换内部 `Conversation` 或 unchecked cursor 的公共入口。

**验证**：

- 聚焦测试覆盖 `AgentState` serde round-trip、唯一 conversation 保留、runtime handle 排除、
  illegal cursor transition 拒绝、恢复时必须调用 `Conversation::restore`。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 state/cursor 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M1-R [TODO] Milestone 1 Review

**前置依赖**：M1-1 至 M1-3 全部完成。

**上下文**：M1 建立后续所有 Agent 状态、serde 和 runtime 句柄分离的基础；若此处把 live handle
写进 data shape，后续 pause/restore、spawn 和 approval 都会继承错误边界。

**做什么**：

- 对照 `docs/agent-layer.md` §1、§8 与 `PLAN.md` 已定关键决策审查 `AgentSpec`/
  `AgentState`/`RunContext`/`LoopCursor`。
- 确认 Agent 只有一个活动 `Conversation`，id/时间外部注入，runtime handles 不 serde，子
  context 不能绕过父 budget/cancel/trace。
- 列出 M2 `AgentLoop` 允许使用的 crate-private/public API，确认未提前暴露 unchecked loop 推进路径。

**验证**：

- 运行全部 M1 聚焦测试并人工映射 serde/runtime 分离边界；公共 API rustdoc 无模糊承诺。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

---

## Milestone 2 — AgentLoop 步进模型

### M2-1 [TODO] `AgentInput`、`AgentEvent`、`AgentOutcome` 与 stream 契约

**前置依赖**：M1-R。

**上下文**：`docs/agent-layer.md` §1.3 与 §2 要求 `feed` 返回覆盖整段自主推进的
`AgentEvent` stream，事件包括 LLM delta、step boundary、tool start/finish、approval 等待和
done；stream 消费完之前不得再次 feed。

**做什么**：

- 定义 `AgentInput`、`PivotMessage`、`AgentEvent`、`AgentOutcome`、`StepBoundary` payload
  与分类化 `AgentError`。
- 定义 dyn-safe 或对象安全可用的 `AgentLoop` 抽象；若使用 associated stream，需要提供可装箱
  的公共类型别名或 wrapper。
- `AgentEvent::Llm` 透明承载 Client `StreamEvent`；`StepBoundary` 携带当前
  `conversation::Boundary` 与 `StepId`/trace metadata；`Done` 明确区分完成、预算耗尽、
  cancel、错误和等待外部恢复。
- 实现 feed reentrancy/backpressure guard，保证一个 Agent 同时只有一个活跃推进流。

**验证**：

- 聚焦测试覆盖事件 serde/data shape、`StreamEvent` 透传、done 分类、feed 重入拒绝和 stream drop
  后状态清理。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 event/loop trait 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M2-2 [TODO] 基础 LLM step 驱动与 Conversation pending 集成

**前置依赖**：M2-1。

**上下文**：Agent loop 的基础路径是 Client call → `Conversation::begin_turn`/
`start_assistant_response` 或 stream accumulator → `finish_assistant` → `commit_pending`。
无 tool-use 的 assistant 结束后应产生 `StepBoundary` 与 `Done`。

**做什么**：

- 实现默认 loop driver 的 text-only/non-stream 和 stream 路径，接收 `LlmClient`、`AgentState`
  与 `RunContext`。
- 从 `AgentInput` 构造 user `Message` 并调用 `Conversation::begin_turn`；将 Client response
  送入 `Conversation::start_assistant_response` 或 `start_assistant` + stream push。
- `AssistantFinish::ReadyToCommit` 时调用 `commit_pending`，随后重新取得合法 `Boundary`
  并发出 `AgentEvent::StepBoundary`。
- 保证任何 Client/Conversation 错误都不产生 partial committed state，并转换为分类 `AgentError`。

**验证**：

- 使用 fake `LlmClient` 做 text-only 非流式与流式聚焦测试，断言事件顺序、Conversation
  committed turns、usage、boundary version 和失败原子性。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 loop text 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M2-3 [TODO] Tool use 执行编排与 result 回灌

**前置依赖**：M2-2。

**上下文**：`docs/agent-layer.md` §3 与 `DESIGN.md` §1.3 要求 tool 发起由 Agent 决定，
配对记账由 Conversation 保证。现有 Conversation API 包括 `register_tool_calls`、
`append_tool_response`/`append_tool_result`、`finish_assistant` 和 `commit_pending`。

**做什么**：

- 定义 `ToolExecutor`/`ToolRegistry` 的最小 runtime trait，用 provider-neutral tool declaration、
  `ToolCallId` 和 `ToolResponse` 执行调用。
- 当 `finish_assistant` 返回 `AssistantFinish::RequiresToolCallMappings` 时，Agent 为每个
  provider call id 获取/注入稳定 `ToolCallId`，调用 `register_tool_calls`，并按策略并行或串行执行。
- 发出 `ToolCallStarted`/`ToolCallFinished` 事件；每个完整结果通过
  `Conversation::append_tool_response` 回灌，然后继续下一条 assistant，直到 final assistant commit。
- tool failure/denied/cancelled 必须以 `ToolStatus` 表达，不能塞进 `extra` 或 provider 私有字段。

**验证**：

- 聚焦测试覆盖单 tool、parallel tool、tool error/denied、重复/未知 call id 拒绝、tool 执行失败后
  可让模型继续自愈，以及所有 committed Turn 仍满足 Conversation I1--I4。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 loop tool 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M2-R [TODO] Milestone 2 Review

**前置依赖**：M2-1 至 M2-3 全部完成。

**上下文**：M2 首次把 Client、Conversation、Tool runtime 串成 Agent loop；若事件顺序、
backpressure 或 pending commit 边界不严，M3 的 pivot/approval/cancel 会放大缺陷。

**做什么**：

- 审查 `AgentLoop::feed` stream 契约、事件命名、错误分类、tool result 回灌和
  Conversation pending/commit 调用顺序。
- 确认 Agent 只决定 tool 执行策略，不复制 Conversation pairing 校验；确认 stream drop、
  Client error 和 tool error 不留下半提交状态。
- 明确 M3 可挂接的 step boundary、approval waiting 和 cancel hook 点。

**验证**：

- 运行全部 M2 聚焦测试，人工检查 text/tool/parallel 路径事件图与 Conversation history。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

---

## Milestone 3 — 边界干预、恢复与 Cancel

### M3-1 [TODO] Conversation step-boundary `user` 注入入口

**前置依赖**：M2-R。

**上下文**：`docs/agent-layer.md` §4.1 与 §8 要求 Agent pivot 在未来合法 step boundary 向
pending turn 追加 `Role::User` message。当前 Conversation 的 `begin_turn` 一个 turn 只绑定
一条 user message，`PendingTurn` 需要新增受检写入口，但 closed history 仍只能通过
`commit_pending`。

**做什么**：

- 在 Conversation pending 层新增 boundary-aligned user injection API，调用方提供 `MessageId`、
 完整 `Message { role: Role::User, .. }` 与来源 metadata。
- 只允许在合法 step boundary 注入：不能有 active partial assistant，不能打断 open tool call，
  不能注入 system/assistant/tool role，不能越过 stale/cross-conversation `Boundary`。
- 更新 validator/commit 路径，允许 canonical Turn 在 tool results 闭合后的 step boundary 出现
  额外 user message，并随后继续 assistant；不得破坏现有单 user turn 行为。
- 将注入消息来源保存在 metadata/extra data model 中，不新造 role。

**验证**：

- 聚焦测试覆盖 tool_result 后注入 user、纯文本 turn 只能落到下一 turn、非法 role、stale
  boundary、active partial、open call、重复 message id 和失败原子性。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 conversation injection 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M3-2 [TODO] Pivot queue 与 `interject` 软转向

**前置依赖**：M3-1。

**上下文**：`docs/agent-layer.md` §3/§4.1 要求 `interject` 不立即打断当前 LLM 调用，而是在下一个
合法 `StepBoundary` 并入 pending。pivot 是消息，不含 skill/tool/system reconfig。

**做什么**：

- 在 Agent runtime 中实现 thread-safe/async-safe pivot queue；`AgentLoop::interject` 只入队
  `PivotMessage` 并返回可观测 ack/error。
- 在每个 `StepBoundary` 求值点按 FIFO 或明确策略应用 pending pivots，调用 M3-1 的 Conversation
  user injection API。
- 对无当前 pending turn 的情况，将 pivot 转换为下一 turn 的初始 user input；对 turn 内合法点，
  注入同一 pending turn。
- 事件流发出 pivot accepted/applied/rejected 事件或在 `StepBoundary` metadata 中记录结果。

**验证**：

- 聚焦测试覆盖 LLM streaming 中 interject 延迟生效、多 pivot 顺序、无 tool text turn 落到下一 turn、
  tool_result 后同 turn 注入、queue cancel/drop 策略和非法消息分类错误。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 pivot 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M3-3 [TODO] Turn-boundary reconfig：skill/tool/system 变更排队

**前置依赖**：M3-2。

**上下文**：`docs/agent-layer.md` §4.2 与 §6.1 要求 skill 启停、tool set 增删和 system prompt
叠加只在 turn boundary 生效，保证 turn 内工具集恒定。reconfig 不走 pivot message 注入入口。

**做什么**：

- 定义 `ReconfigRequest`/`ReconfigQueue`，表达 skill activate/deactivate、tool set replace/patch、
  system prompt overlay 与 model/loop policy 可变项。
- 在 Agent loop 的 turn completion boundary 应用 reconfig，更新 `AgentState.active_skills`、
  runtime `ToolRegistry` 和下一次 Client request 的 system/tool 声明。
- 如果请求在 pending turn 中途到达，排队并在最终 assistant commit 后应用；同一 turn 内 tool calls
  继续使用启动该 turn 时的 registry snapshot。
- 对冲突 reconfig（重复 skill、未知 tool set、system overlay 版本冲突）返回分类错误且不部分应用。

**验证**：

- 聚焦测试覆盖 turn 内工具集恒定、reconfig 延迟到 turn boundary、生效后下一 turn request 改变、
  pivot 与 reconfig 同时排队互不干扰，以及失败原子性。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 reconfig 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M3-4 [TODO] Approval 挂起、responder 与 cancel 贯穿闭合

**前置依赖**：M3-3。

**上下文**：`docs/agent-layer.md` §3 要求审批以 `AwaitingApproval` 事件挂起 stream，不结束 feed；
cancel 通过 `RunContext` 的 cancellation token 立即打断 LLM/tool/sub-agent，并复用
`Conversation::cancel_pending`、`CancelDisposition` 闭合 pending 裂缝。

**做什么**：

- 为 tool execution 增加 approval policy；需要审批时发出 `AwaitingApproval { call, respond }`
  并暂停对应 tool 启动。
- responder 接受 approve/deny/timeout/cancel，转换为执行 tool、`ToolStatus::Denied` result 或
  cancelled result，并通过 Conversation append/cancel path 回灌。
- 将 `RunContext` cancellation token 接入 LLM stream、tool future 和子 agent future；cancel 后
  调用 `Conversation::cancel_pending` 的 discard/resume/commit 策略，保证后续可重新 feed。
- 保存 awaiting approval 所需 data-only cursor；live responder 不 serde，恢复时由 resolver 重建。

**验证**：

- 聚焦测试覆盖 approval approve/deny/timeout、stream 挂起不结束、恢复后 responder 重建、cancel
  active partial、cancel open tool call 后继续 feed、父 cancel 传播到子 agent/tool。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 approval/cancel 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M3-R [TODO] Milestone 3 Review

**前置依赖**：M3-1 至 M3-4 全部完成。

**上下文**：M3 覆盖所有外部干预路径；这些路径必须共享 Boundary、pending 和 RunContext 地基，
不能各自开洞。

**做什么**：

- 审查 pivot 只注入 user、reconfig 只在 turn boundary、approval 挂起可恢复、cancel 后可继续
  feed 的实现与测试。
- 确认新增 Conversation 注入入口仍受 validator/Boundary/pending phase 约束，未暴露 raw history
  或 unchecked pending mutation。
- 核对 M4 vertical APIs 可调用的公共边界，避免 vertical 功能直接操作 Agent 内部 runtime state。

**验证**：

- 运行全部 M3 聚焦测试，人工检查边界事件、pending phase、cancel disposition 和恢复 cursor。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

---

## Milestone 4 — 垂直功能 API-first

### M4-1 [TODO] ToolRegistry、ToolAdapter、SkillBundle 与 MCP 适配边界

**前置依赖**：M3-R。

**上下文**：`docs/agent-layer.md` §5/§6.1 要求垂直功能 API-first，tool 只是 adapter；
skill 是 prompt 片段、tool 和资源 bundle，activation 是 turn-boundary reconfig。

**做什么**：

- 完善 `ToolRegistry`，支持 local tool、provider tool、MCP-backed tool 的统一声明、查找、
  命名空间和 runtime resolver。
- 定义 `ToolAdapter`，把一等 Rust API 包装为模型可调用 tool；adapter 只能薄封装校验与参数转换，
  不复制业务状态。
- 定义 `SkillBundle`/`SkillManifest`，包含 prompt fragment、tool refs 和 resource refs；激活/
  停用只生成 M3 reconfig 请求。
- 处理工具名冲突、skill 资源缺失、MCP session 恢复和 registry rebuild 的分类错误。

**验证**：

- 聚焦测试覆盖 API direct call 与 tool adapter 调用语义一致、skill activation rebuild registry、
  namespace 冲突拒绝、MCP tool resolver mock、turn 内 registry snapshot 不变。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 tool/skill 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M4-2 [TODO] Plan API：可 serde 的任务板、依赖与 CAS claim

**前置依赖**：M4-1。

**上下文**：`docs/agent-layer.md` §6.2 把 plan 定义为一等数据结构：只记录 task 内容、状态、
认领者和依赖，不执行任务；claim 需要 CAS/版本避免多个 agent 同时认领。

**做什么**：

- 定义 `PlanBoard`、`PlanTask`、`PlanTaskStatus`、`ClaimToken`/version 等 data model。
- 提供 `create_plan`、`read`、`add_task`、`claim`、`release`、`update_status`、`block`、
  `complete` 等 API；不得提供 `execute`。
- claim/update 必须检查版本、依赖状态、当前认领者和状态转换合法性；失败不部分修改。
- 提供可选 ToolAdapter，将相同 API 暴露给模型使用，并保留宿主直接调用路径。

**验证**：

- 聚焦测试覆盖 serde、状态机、依赖阻塞、CAS 防双重认领、失败原子性、direct API 与 tool adapter
  语义一致、无 executor API。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 plan 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M4-3 [TODO] Blackboard API：append-only topic 消息流与 read cursor

**前置依赖**：M4-2。

**上下文**：`docs/agent-layer.md` §6.4 定义 blackboard 为 agent 聊天群：append-only、
topic/channel 隔离、sender identity、时间戳由调用方注入、best-effort、无锁/认领/CAS。

**做什么**：

- 定义 `Blackboard`、`BlackboardTopic`、`BlackboardMessage`、`ReadCursor` 与 sender metadata。
- 提供 `post`、`read_since`、`list_topics`、cursor advance 等 API；message 只追加不可修改。
- 不加入 claim、lock、ack/retry 或 exactly-once 机制；需要强协调的场景明确走 Plan API。
- 提供 ToolAdapter，使模型可以 post/check，但 adapter 仍调用同一 Rust API。

**验证**：

- 聚焦测试覆盖 append-only、topic 隔离、read cursor、sender/time 外部注入、best-effort 语义、
  direct API 与 tool adapter 等价、历史 message 不可覆盖。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 blackboard 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M4-4 [TODO] Multi-agent orchestration 最小原语

**前置依赖**：M4-3。

**上下文**：`docs/agent-layer.md` §7 与 §6.3 要求不发明编排引擎，只提供 spawn/stop/send/
await result 原语；子 agent 必须挂在父 `RunContext` 下，继承预算、cancel 和 trace。

**做什么**：

- 定义 `AgentSpawner`/`AgentHandle`、`spawn_agent`、`stop_agent`、`send`、`await_result` 等
  最小 API，并允许宿主用普通 Rust 组合 pipeline/group/swarm。
- spawn 输入必须包含 `AgentSpec`、一个 `Conversation` 或 fork point，以及父 `RunContext`；
  子 context 从父派生。
- 加入深度上限、budget 继承、cancel 传播和 trace parent 记录；模型通过 tool 化 API spawn/stop
  时也必须经过同一护栏。
- 多路径探索通过 `Conversation::fork_at` 创建 child conversation 后由新 Agent 承载，不在一个
  Agent 内保存会话池。

**验证**：

- 聚焦测试覆盖 spawn/send/await、父 cancel 传播、预算继承、深度上限、trace parent、
  fork conversation 承载新 Agent、tool adapter 路径不绕过护栏。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 orchestration 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M4-R [TODO] Milestone 4 Review

**前置依赖**：M4-1 至 M4-4 全部完成。

**上下文**：M4 引入可被宿主和模型共同使用的垂直 API；若 adapter 吞掉语义或偷偷执行调度，
会违背 API-first 和非编排引擎边界。

**做什么**：

- 审查 skill/tool、plan、blackboard、orchestration 是否均为 Rust API first，tool adapter 只是薄封装。
- 确认 plan 无 executor、blackboard 无 claim/lock、multi-agent 只有最小原语且子 agent 继承父
  `RunContext`。
- 检查 vertical API 未直接修改 Agent/Conversation 内部 unchecked state。

**验证**：

- 运行全部 M4 聚焦测试，人工抽查 direct API 与 tool adapter 行为一致性。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

---

## Milestone 5 — 横切运行时设施

### M5-1 [TODO] Budget accounting 与 step-boundary 超限处理

**前置依赖**：M4-R。

**上下文**：`docs/agent-layer.md` §2 与 `DESIGN.md` §1.3 要求预算在 step boundary 检查；
Client usage 每步可得，Conversation `TurnMeta` 和 `effective_view` 已保留 usage/token 信息。

**做什么**：

- 实现 token、cost、step count、wall-clock 等 budget accounting，并接入 LLM response usage、
  tool/sub-agent cost 和 loop step 计数。
- 在每个 `StepBoundary` 检查 soft/hard budget；超限产生分类 `AgentOutcome`/`AgentError`，
  并按策略停止、cancel pending 或让模型自愈。
- 子 agent budget 从父派生并回写消耗，不能绕过总预算。
- 预算 record 可 serde，live clock/timer handle 不 serde；时间由调用方 clock 注入以便测试。

**验证**：

- 聚焦测试覆盖 token/cost/step/wall-clock 超限、子 agent 消耗回写、streaming final usage、
  pending cancel 策略和确定性 clock。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 budget 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M5-2 [TODO] Trace tree 与 observability records

**前置依赖**：M5-1。

**上下文**：`docs/agent-layer.md` §1.4、§6.3 与 `DESIGN.md` §1.3 要求 trace 可重建
run → step → llm/tool/sub-agent 树，记录 request/response/usage/latency 等可观测数据。

**做什么**：

- 定义 trace record data model：run node、step node、llm call、tool call、approval wait、
  pivot/reconfig application、sub-agent node。
- 将 trace handle 接入 AgentLoop、ToolExecutor、approval、orchestration 和 budget path。
- trace record 必须可 serde；live subscriber/exporter 作为 runtime handle，不进入 AgentState。
- 提供只读查询和 test sink，避免用日志文本作为事实来源。

**验证**：

- 聚焦测试覆盖完整 text/tool/sub-agent trace parentage、usage/latency 记录、approval wait、
  cancel/error 节点、serde round-trip 和 live subscriber 排除。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 trace 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M5-3 [TODO] Runtime hooks 与 compaction trigger 集成

**前置依赖**：M5-2。

**上下文**：`DESIGN.md` §1.3 列出 before-llm、after-response、before-tool、after-tool 等 hook；
Conversation 已实现 `CompactionTrigger`、`CompactionStrategy`、`apply_compaction` 与
`effective_view`。Agent loop 是 compaction trigger 的天然求值点，但 compaction 只在完整 turn
boundary 应用。

**做什么**：

- 定义 hook/middleware registry，支持 before/after step、llm request/response、tool call/result、
  approval 和 sub-agent spawn。
- hook 只能观察或返回受检 action（pivot/reconfig/cancel/compaction request），不能直接修改
  AgentState 或 Conversation raw history。
- 在 step boundary 观察 compaction trigger；若 trigger 需要完整 turn boundary，则延迟到 turn
  commit 后调用 `Conversation::apply_compaction`。
- hook/runtime handle 不 serde；data-only hook effects 记录在 trace 或 cursor 中。

**验证**：

- 聚焦测试覆盖 hook 顺序、hook error 策略、禁止直接 mutable state、compaction trigger deferred/
  applied、pending 排除、effective_view 未泄漏未来 turn。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 hook/compaction 测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M5-R [TODO] Milestone 5 Review

**前置依赖**：M5-1 至 M5-3 全部完成。

**上下文**：M5 的横切逻辑最容易绕过主状态机；review 必须确认预算、trace、hook 和 compaction
都通过统一 step/turn boundary 接入。

**做什么**：

- 审查 budget、trace、hook、compaction trigger 的求值点和错误策略，确认没有全局单例或日志文本事实源。
- 确认 hook 不能直接改 raw state，compaction 仍只在 Conversation 完整 turn boundary 原子应用。
- 核对 M6 端到端验收所需 fake client、fake tool、trace sink 和 budget fixture 齐备。

**验证**：

- 运行全部 M5 聚焦测试，人工检查 trace tree 和 compaction/projection 交互。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

---

## Milestone 6 — 跨功能验收与文档

### M6-1 [TODO] 单 Agent 状态机组合验收

**前置依赖**：M5-R。

**上下文**：完整 Agent Runtime 必须证明 text/tool/pivot/reconfig/approval/cancel/budget/
compaction/pause-restore 可以组合，而不是只在孤立单测中成立。

**做什么**：

- 新增离线集成测试，用 fake `LlmClient` 和 fake tools 驱动多 step Agent：user → tool →
  approval → pivot → final assistant → commit → reconfig → next turn。
- 在同一场景中触发 budget check、trace recording、compaction trigger deferred/applied 与
  pause/restore。
- 每个阶段断言 `AgentEvent` 顺序、Conversation committed/pending 状态、`Boundary` 版本、
  tool pairing、trace tree 和 `effective_view`。
- 覆盖错误路径：tool failure self-heal、cancel open call 后继续 feed、restore 后继续 approval。

**验证**：

- 聚焦集成测试覆盖上述组合场景，单个测试 < 1 分钟，不访问真实 endpoint。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 agent state-machine 集成测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M6-2 [TODO] Multi-agent 与垂直功能端到端验收

**前置依赖**：M6-1。

**上下文**：`docs/agent-layer.md` §5--§7 要求 plan/blackboard/orchestration 可由宿主直接调用，
也可通过 tool adapter 给模型使用；多 agent 编排只提供最小原语，并继承父 `RunContext`。

**做什么**：

- 新增离线集成测试：父 Agent spawn 子 Agent，子 Agent claim plan task、读取/发送 blackboard
  message、执行 tool adapter，并将结果回传父 Agent。
- 断言 plan CAS 防双认领，blackboard append-only/read cursor，父 cancel 传播，预算继承与 trace
  sub-agent parentage。
- 验证 direct API 与 tool adapter 在同一 plan/blackboard/skill 操作上产生一致结果。
- 覆盖 fork Conversation → 新 Agent 承载多路径探索，不在单 Agent 内创建会话池。

**验证**：

- 聚焦 multi-agent/vertical 集成测试全部通过，单个测试 < 1 分钟，不访问真实 endpoint。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 multi-agent 集成测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M6-3 [TODO] Agent 示例、README、crate docs 与归档链接更新

**前置依赖**：M6-2。

**上下文**：完成 Agent 层后，根文档应把当前能力、示例和历史归档位置说明清楚；Conversation
与 Client 归档路径不能失效。示例必须离线可运行，真实 endpoint 仍按既有 ignored 策略。

**做什么**：

- 新增 `examples/agent_runtime.rs` 或等价离线示例，展示 text/tool/pivot/approval/cancel 或
  plan/blackboard 的核心路径，使用 deterministic id 和 fake client/tool。
- 更新 root `README.md`、crate root/module rustdoc、capability/architecture 文档中的当前阶段描述，
  指向 Agent `PLAN.md`/`TODO.md` 与 Conversation/Client 归档。
- 文档中明确 Agent 层仍不提供大而全编排引擎，不弱化 API-first、single conversation 和
  step/turn boundary 决策。
- 检查所有 `PLAN.md`/`TODO.md` 链接和示例命令，移除过期“Agent loop 不在范围内”的当前态措辞。

**验证**：

- `cargo run --example agent_runtime` 通过；README/crate docs 链接手工/命令审查无断链。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、示例与文档相关聚焦测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  `git diff --check`。

### M6-R [TODO] Milestone 6 / Agent Layer 总 Review

**前置依赖**：M6-1 至 M6-3 全部完成。

**上下文**：这是 Agent 层的最终验收。需要回溯 `docs/agent-layer.md`、`DESIGN.md` §1.3、
`PLAN.md` 与全部任务记录，确认实现、测试和文档闭合。

**做什么**：

- 对照 Agent 三层拆分、RunContext、feed stream、pivot/reconfig、approval/cancel、API-first
  verticals、plan/blackboard、多 agent 原语和 serde/runtime 分离逐项审查。
- 确认没有公开 unchecked Agent/Conversation mutation，没有 provider 特判或 workaround，
  没有把 plan/blackboard/orchestration 做成隐藏 executor。
- 运行完整验证并更新本 `TODO.md` 的完成记录；若所有任务已 `[DONE]`，按项目完成规则准备最终归档
  或 `endtag` 所需的下一步说明。

**验证**：

- 人工映射 `docs/agent-layer.md` 全文主要决策到实现、测试和文档；所有 review 发现必须修复或新增
  明确前置任务。
- 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`cargo test --doc`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`。
