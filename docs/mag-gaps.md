# mag 对 agent-lib 的缺口需求（mag-gaps）

> 来源：mag 仓库 [`docs/CLI.md`](../../mag/docs/CLI.md) §5A（2026-07-20 评估，评估时点 agent-lib
> `@0add094`）。本文档是这些缺口在 agent-lib 侧的**需求定义与实现锚点**，是本轮 `PLAN.md` /
> `TODO.md` 的唯一设计输入。行号引用自评估时点，随后续修改可能漂移，以符号名为准。

## 背景

mag 是基于 agent-lib 的编码 agent 应用（facade `Agent` 嵌入方）。它的 CLI 里程碑需要四项
agent-lib 目前不具备的能力：

- **多 agent 委派**：supervisor 经 model-routed `ask_<name>` 委派给 local LLM subagent 与
  **external ACP agent**；
- **子 agent 交互穿透**：委派链中子 agent 的审批/权限请求统一路由到 root 会话注入的异步
  `InteractionHandler`（mag 的 `IpcApproval`），并带 delegate 来源标注；
- **运行时 reconfig**：把新 model/tools/system prompt 应用到活会话，turn 边界生效
  （mag 配置系统的 `apply_config`）；
- **cancel 强化**：取消沿委派链传导、external 子进程不泄漏、被阻塞的 tool/interaction 批
  可被抢占。

评估同时确认了**无需改动**的部分（pivot 盲重试、`ask_<name>` 自动生成、delegation 事件、
external ACP presets、`MarkInterrupted` 恢复、cancel 后会话复用等），见
[`docs/CLI.md`](../../mag/docs/CLI.md) §5A-D，本文件不重复。

缺口按阻塞程度分三档：**A 组 = 硬阻塞**（不做则 mag 里程碑核心目标无法达成），
**B 组 = 强烈建议**（mag 侧有兜底但体验/正确性受损），**C 组 = 可选后置**（本计划不做，
仅登记）。

---

## A1（硬阻塞）委派子 agent 的交互路由到父级异步 `InteractionHandler`，带来源标注

状态：`Interaction` 归因模型已落地（✅ 已修复，M1-1）；local subagent 的已暂停交互路由到父级
注入 handler 已落地（✅ 已修复，M1-2）；external 委派路径的 `NeedInteraction` 路由到父级
注入 handler 已落地（✅ 已修复，M1-3）；external-start ask 决策在父级注入 handler 存在时也会路由到
该异步 handler，并带 delegate/depth 归因（✅ 已修复，M1-4）。

### 现状

- **local subagent 路径**（✅ 已修复，M1-2）：子 agent 仍用 worker 自带 `ApprovalPolicy` 作为
  gate；父级注入 `InteractionHandler` 时,已暂停交互由 `ChildInteractionRouter` 标注 origin 后
  转发父级 handler。父级未注入时仍使用子 `FacadeApproval` fallback，`Approval::ask` 同步闭包与
  headless deny 行为保持不变。
- **external ACP/CLI 路径**（✅ 已修复，M1-3）：ACP adapter 真实地把
  `session/request_permission` 桥接为 `RuntimeDecisionPoint::PausedForInteraction`（携带
  `Interaction::permission`，`agent/external/acp/adapter.rs:446-465, 500-511`），Claude Code adapter
  也可具现 `PausedForInteraction`；machine 统一转为 `NeedInteraction`（`agent/external/machine.rs:765-806`）。
  facade 委派驱动现在用 external interaction route 替代旧 `EmptyExternalScope`：父级注入 handler 存在时,
  interaction 带 `origin { delegate, depth }` 转发父级 handler,应答经 `RespondInteraction` 回灌 runtime；未注入时
  保持失败语义,但错误明确说明 external agent 请求了权限且无 handler 可应答。Codex/OpenCode 当前无
  host-answerable permission pause,同一路由层覆盖未来具现 `NeedInteraction` 的 runtime。
- **父级注入 handler 对 local 与 external 子 agent 可见**（✅ 已修复，M1-2/M1-3）：
  `AgentBuilder::interaction_handler(..)` 的注入 handler 会传入 local 与 external 委派驱动。
- **归因模型**（✅ 已修复，M1-1）：`Interaction` 已有可选 `origin { delegate, depth }` 渲染归因；
  `PermissionRequest.actor: AgentId`（`agent/permission.rs:98`）仍保留权限主体语义，不与归因合并。

### 需求

1. 委派驱动路径上，子 agent（local + external）暂停的交互（Approval / Permission / Question /
   Choice）路由到**父级注入的异步 `InteractionHandler`**；父级未注入时保持现状行为
   （local：worker 同步 policy；external：失败），向后兼容。
2. **职责划分与 supervisor 一致**：子 agent 自己的 `ApprovalPolicy` 仍决定**哪些调用暂停**
   （gate），被暂停的交互由父级 handler **应答**（answer）——与 `agent.rs:1265-1276` 文档化的
   supervisor 语义对齐。
3. **来源标注**：路由到父级 handler 的交互携带 delegate 归因（至少 delegate 名 + 委派深度），
   形式可以是 `Interaction` 增加可选 `origin` 字段（serde 向后兼容）或包装类型；mag 据此渲染
   `[approval · codex(sub, depth 1)]`。external 的 `PermissionRequest.actor` 语义保持，不归并。
4. **异步语义不丢**：路由全程保持 `InteractionHandler::fulfill` 的 async 暂停点语义
   （machine 停在 `.await`，cancel 经 `RunContext` 传播），不得退化为同步闭包。
5. external 的 `ExternalPermissionMode::Prompt` 已在 facade 委派路径端到端可用（✅ 已修复，M1-3）：ACP 子 agent
   `session/request_permission` → 父级 handler 应答 → outcome 回灌 ACP transport → 委派继续。

### 关联：external-start approval（可降级）

`FacadeApproval::resolve_external_start`（`approval.rs:573`，调用点 `delegate.rs:1892`）仍是无父级 handler
时的**同步 fallback**；但 external-start 的 effective tier 为 ask 且父级注入了异步
`InteractionHandler` 时，启动门会在 drive layer 构造成 `InteractionKind::Approval`，标注
`origin { delegate, depth }` 后交给父级 handler（✅ 已修复，M1-4）。`Approve` 才启动 external
delegate；`Deny` / `Timeout` / `Cancel` 均表面为 `FacadeError::ApprovalDenied`，不会驱动 runtime。

触发 ask 的方式包括 `ApprovalPolicy::ask_external_agents()`、`ApprovalPolicy::ask_tool("ask_<name>")`
或显式给 `ask_<name>` 设置 ask tier。父级未注入 handler 时保持原同步行为：同步 `Approval::ask`
handler 可应答；无同步 handler 的 headless ask 会 deny，而不是挂起等待前端。external runtime 启动后的
permission/question/choice 仍按 M1-3 路由到父级 handler。

---

## A2（硬阻塞）facade 级 reconfigure API（turn 边界生效）

状态：facade reconfig 入口与准入校验已落地（M2-1）：`Agent::reconfigure(ReconfigRequest)` 支持
model / system overlay / tool-set declaration / loop policy 请求,skill 请求显式拒绝；facade 选择
between-run/rest cursor 准入,active turn 返回 `InvalidState`。流式/非流式 reconfig handler 与
`ReplaceToolSet` / `PatchToolSet` 的 live registry 一致性已修复（M2-2）；snapshot/restore 交互仍待
M2-3/M2-R 收口。

### 现状

- agent 层 reconfig 机制**齐备**：`AgentState::queue_reconfig`（`src/agent/state.rs:182-188`），
  `ReconfigRequest::{ActivateSkill, DeactivateSkill, ReplaceActiveSkills, SetSystemPromptOverlay,
  ReplaceToolSet, PatchToolSet, SetModel, SetLoopPolicy}`（`agent/state/queue.rs:186-229`），
  turn 边界经 `NeedReconfigRegistry` requirement + `ReconfigRegistryHandler` /
  `ToolRegistryResolver` 应用（`agent/drive/reference.rs:191-244`）。
- **facade 入口已接线（M2-1）**：`Agent::reconfigure` 接受 facade 重导的 `ReconfigRequest`,并在
  between-run/rest cursor 准入；active/parked turn 显式 `InvalidState`。`SetModel` 与
  `SetSystemPromptOverlay` 可在下一 turn 起点进入 LLM request。
- **live registry 一致性已接线（✅ 已修复，M2-2）**：facade 的非流式与流式 drive scope 都提供
  `ReconfigRegistryHandler`;queue-time validation 与 apply-time swap 使用同一个 facade
  `ToolRegistryResolver`。`ReplaceToolSet` / `PatchToolSet` 的目标声明名字必须来自已注册的 facade
  tool surface,否则准入时显式失败；每个 run 的初始 registry 也按 `state.current_tool_set()` 过滤,
  避免上一轮移除的工具在下一轮重新可执行。
- **snapshot/restore 不能替代**：恢复以快照 `AgentState` 为权威，`current_model` / system
  prompt / `current_tool_set` 声明全部保留（`facade/agent/snapshot.rs:864-869`）；重注入不同
  工具集只按名字替换执行闭包（`snapshot.rs:656-664`），模型看到的声明仍是旧快照——静默不一致。

### 需求

1. facade 暴露 reconfig 入口（如 `Agent::reconfigure(..)` 或 builder 形式），接受
   `ReconfigRequest`（至少覆盖 mag 需要的 `SetModel` / `ReplaceToolSet` /
   `SetSystemPromptOverlay`；skill 类可一并透出或显式不支持）。
2. **时机语义与 `docs/agent-layer.md` §4.2 一致**：reconfig 只在 turn 边界应用。底层
   `DefaultAgentMachine` 支持 turn 中请求排队；facade M2-1 入口选择更保守的 between-run/rest
   cursor 准入，turn 进行中调用返回 `InvalidState`。run 空闲时调用 → 下一 turn 起点生效。
3. 接线 `ReconfigRegistryHandler` / `ToolRegistryResolver` 到 facade 的同步与流式两条驱动
   路径；`ReplaceToolSet` 时**声明与执行闭包的一致性**要有明确答案（一并替换注册表 / 校验
   名字集合并报错，不允许静默 mismatch）。
4. reconfig 后的 `Agent::snapshot()` 反映新 spec（snapshot 本就捕获 `AgentState`，需确认
   reconfig 落进 state 的时机先于快照点）。
5. 文档同步：`docs/facade-api.md`、`docs/agent-layer.md` §4.2（从「机制齐备」更新为「facade
   可达」）。

---

## A3（强烈建议）cancel 对 external agent 的传导与清理

### 现状

- 进程内传导**已可用**：`RunContext::derive_child` 派生子 token（`agent/context.rs:233-245`），
  子 agent 继承父 cancel；external drive abandon 置 `cleanup_required`
  （`agent/external/machine.rs:1571-1589`），facade 折成 `DelegationStatus::Failed`
  （`facade/delegate.rs:1854`）。
- **子进程不被杀**：facade 从不调 `ExternalSessionRegistry::cleanup_agent/cleanup`
  （`agent/external/registry.rs:406/448`）——grep 确认只有测试与文档调用。cancel 后 ACP
  子进程泄漏，除非宿主自己持有 registry 句柄 sweep。
- **cancel 响应性**：ACP read loop 只在**行间隔**查 `ctx.is_cancelled()`
  （`agent/external/acp/adapter.rs:431`）；子进程静默时阻塞到一行到达或 120s 读超时
  （`acp/connection.rs:157-160`，`facade/external.rs:1006` 的 `DEFAULT_EXTERNAL_IO_TIMEOUT`）。

### 需求

1. **read loop 取消响应**：ACP adapter 的读循环对 cancellation 做 `select!`（或等价机制），
   子进程静默时 cancel 也能在**有界短时间**内生效（目标：秒级，不再等满 120s IO 超时）。
   其余三个 CLI adapter 的读循环同样核查（它们的行读带独立超时，语义不同，按现状评估
   是否需要同样处理）。
2. **abandoned session 清理**：cancel/流 drop 导致 external drive abandon（`cleanup_required`）
   时，facade 驱动路径负责触发对应 session 的清理（`session/cancel` + transport close +
   进程组终止），或在 facade 暴露**一等清理入口**让宿主在 cancel 后调用。二选一，以
   「宿主不做任何额外动作也不泄漏子进程」为验收标准。
3. 语义文档同步：`docs/managed-external-agent.md`（cancel 传导、清理责任归属）。

---

## A4（强烈建议）cancel 抢占被阻塞的 tool/interaction 批

### 现状

- drive 只在 tool 批**启动前与完成后**检查 `ctx.is_cancelled()`
  （`facade/agent/stream.rs:244-253, 291-305`）；`fulfill_batch` 等批内全部 requirement 完成
  （`agent/drive.rs:794-842`）；`ToolRegistryHandler::fulfill` 忽略 ctx、无 cancel select
  （`agent/drive/reference.rs:179-189`）。只有 LLM handler 是 cancel-selecting
  （`reference.rs:132-138`）。
- 后果：一个**阻塞中的 tool handler**（如 mag 的 `ask_user` 等人类回答）使整个 run 冻结——
  无流式事件、无 pivot 窗口、cancel 不生效，直到 handler 自行返回。tool handler 拿到
  `ToolContext.cancel`（`facade/tool.rs:59-73`）可以自行 select，但那是把责任推给每个工具。

### 需求

1. cancel 触发时，**批级等待被抢占**：不再等未完成的 tool/interaction fulfill future，
   在途 requirement 按既有 never-resume 语义 settle（`StepInput::Abandon`，
   `stream.rs:270-279` 已有先例），turn 以 cancelled 收尾。
2. **被丢弃的 tool future 的语义要定义并文档化**：drop（detached，副作用工具应自己响应
   `ToolContext::cancel`）还是 join 等待——推荐 drop + 文档强制要求长工具 select cancel；
   `InteractionHandler::fulfill` 侧同理（mag 的 handler 已自行 select `ctx.cancellation()`）。
3. 非流式路径（`run`/`run_full`）与流式路径行为一致。
4. 这是行为收紧：现有的「批完成后才响应 cancel」测试需要更新；文档
   （`docs/agent-layer.md` / `docs/agent-effect-model.md`）同步取消延迟口径。

---

## C 组（可选后置，本计划不做，仅登记）

- **C1**（mag A5）专用 `FacadeError::Cancelled` 变体：现在 cancel 以
  `FacadeError::Agent(AgentError::Other("agent run cancelled (cursor: …)"))` 字符串呈现
  （`facade/agent/stream.rs:476-485`，注释自述推迟到 M5-4）。mag 已用 `cancel.is_cancelled()`
  判别兜底。
- **C2**（mag A6）`DelegationProgress`/`DelegationMessage` 真正发射：变体与 wire 投影齐备
  （`facade/run.rs:560/573`），生产代码从不 emit。
- **C3**（mag A7）local subagent 可执行工具：worker 只携带声明（`delegate.rs:311-315`），
  子 agent 调任何工具得 `UnknownTool`（`facade/tool.rs:674`）。mag 第一版把 LLM subagent
  当纯文本「审查/咨询」角色绕开。
- **C4**（mag A8）pivot 边界窗口对 host 可见（事件/标志）；queued pivot 跨窗口存活。
- **C5**（mag A9）external-start approval 走异步 handler：✅ 已修复（M1-4）。当父级注入
  `InteractionHandler` 且启动策略需要 ask 时，启动审批走同一异步 interaction 通道；无父级 handler
  时保留同步 fallback/headless deny。
- **C6**（mag A10）`DelegationTrace` 增加 `is_external`；`RunEvent` 携带 run id。
- **C7**（2026-07-20 review 登记，agent-lib 内部一致性项，非 mag 阻塞）委派的审批绕过
  tap/recorder：两条 run 路径把裸 handler 传给 `DelegationToolHandler`，子 agent 审批不产生
  `RunEvent::ApprovalRequested`、不进 `RunOutput.events`，与 supervisor 层口径不一致。对 mag
  无害（`IpcApproval` 自行发事件）；修复需注意 `enriched_approval_request` 只看 supervisor
  pending 表，需 origin-aware  enrichment。M3-R 评估收口与否。
- **C8**（2026-07-20 review 登记）SingleTool 委派模式下 external start 被双重 gate：机器 tool
  gate 对统一工具名暂停一次（无归因），驱动层 start-ask 再问一次（有归因）。PerSubagentTool
  模式的豁免是真实的（`facade/approval.rs:778-780`），SingleTool 模式不在豁免内
  （`facade/delegate.rs:1213`）。mag 用 PerSubagentTool 模式，不受影响。
- **C9**（2026-07-20 review 登记）语义文档与测试缺口：(a) child 的 auto-deny 层会暂停且父
  handler 可改判 Approve——与 supervisor 层设计一致（mag 依赖此兜底模式），但子 policy 的
  「deny 可应答」语义未文档化；(b) M1-3 external 路径缺 cancel-while-parked 测试、M1-4 缺
  family-mismatch 测试、Claude Code 路径只有结构性覆盖。M3-R 评估收口与否。

## 使用约束（mag 侧已知晓的陷阱，非缺口）

- restore 不重新 `.subagent(..)` 注册，子 agent 审批策略静默回落 `ApprovalPolicy::default()`
  = auto_allow（`facade/agent/snapshot.rs:838-842`）；external delegate 不重注册则
  `session_handler: None`，drive 即失败。mag 恢复路径必须重注册全部 delegate。
- `AgentRunStream` 是 `!Send` 且整个 run 借用 `&mut Agent`（`facade/agent/stream.rs:676-688`）——
  宿主需 per-session actor 模型。
- 注入 `interaction_handler` 后它是被暂停交互的**唯一应答方**，但 gate 仍由 `ApprovalPolicy`
  控制；要全量过审批需配 `auto_deny` 兜底（`agent.rs:1265-1276`）。
