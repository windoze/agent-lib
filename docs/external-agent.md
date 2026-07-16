# External Agent 设计

> 状态:设计草案。本文讨论如何在现有 sans-io + effect-handler 体系上接入 Claude Code、Codex、
> OpenCode 等外部 coding-agent runtime,并让它们和 `DefaultAgentMachine`、内部 LLM agent、
> MCP/tool、plan/blackboard 等能力在同一个程序里混合编排。

> 相关文档:
> [`agent-effect-model.md`](./agent-effect-model.md)(effect / requirement / pop 模型)、
> [`agent-layer.md`](./agent-layer.md)(machine / handler / driver 分层)、
> [`capability-matrix.md`](./capability-matrix.md)(provider 能力矩阵与逃生舱)。
>
> 标注约定:本文中标记为 **(拟新增)** 的类型或变体尚未在代码中存在,属于本设计提议新增的
> API;未标注者为已实现设施,名称与代码一致。

## 0. 一句话

**External agent 是一类 custom Agent,不是 `LlmClient` provider。** 它把外部 coding-agent
session(进程、SDK、transcript、权限请求、文件改动、子 agent 能力)建模为一个可暂停、可恢复、可取消、
可观测的 agent 节点,通过现有 `Requirement` / `HandlerScope` / `InteractionHandler` /
`SubagentHandler` / `RunContext` 接入同一套 effect 模型。

## 1. 背景与目标

Claude Code、Codex、OpenCode 这类系统不是一次 LLM completion。它们通常包含:

- 一个长期或半长期 session,带 transcript / resume token。
- 自己的 coding loop,会读写文件、运行 shell、调用 MCP 或内置工具。
- 流式输出,包括文本、命令、patch、权限请求、子任务状态。
- 独立的模型选择、权限模式、工作目录和隔离策略。
- 可能的子 agent / teammate / task-list / mailbox 机制。

现有 `LlmClient` 的语义是 provider-neutral 的“一次模型生成”:

```text
ChatRequest -> Response 或 StreamEvent stream -> folded Response
```

它适合 Anthropic Messages、OpenAI Responses、DeepSeek 等 API endpoint,不适合承载完整 coding-agent
runtime。强行塞进 `LlmClient` 会丢掉 session、权限、文件改动、子 agent 和外部事件的语义。

本文目标:

- 给外部 coding agent 一个一等接入形状。
- 复用现有 effect-handler 机制,不另造一套交互/取消/预算/trace。
- 支持在同一个 Session 中混合 Claude Code、Codex、OpenCode、内部 cheap model agent。
- 支持 cost-aware / capability-aware 调度:便宜模型做简单任务,强 agent 做复杂任务。
- 为后续 testkit cassette / scripted handler 留出可测试边界。

非目标:

- 不把 Claude Code / Codex 的私有实现协议作为本库稳定协议。
- 不假设所有外部 agent 都能暴露同等粒度的事件。
- 不要求首版接管外部 agent 的所有 tool 执行。黑盒、半托管、全托管应分阶段支持。

## 2. 现有地基

当前 agent 层已经有以下可复用设施:

| 设施 | 现有语义 | External agent 用法 |
|---|---|---|
| `AgentMachine::step` | sans-io 状态机,只吐 `Notification` 与 `Requirement` | 为 CC/CX/OpenCode 实现 custom machine |
| `Requirement` | 被 reify 的 effect,带 id 与 origin path | 增加 external session effect 或用现有 effect 组合 |
| `HandlerScope` | 每层 drain 的 effect handler 集合 | 外部 agent scope 可局部处理 session/tool,交互向外 pop |
| `Pop` | requirement 沿动态作用域向上冒泡 | default outer handler 由父 scope 提供 |
| `InteractionHandler` | UI / policy 后端,处理 approval/question/choice | 承接外部 agent 的权限请求、选择、澄清 |
| `SubagentHandler` | 唯一加深 scope 的 handler,派生 child context | 把 external agent 作为 child agent 启动 |
| `NestedMachine` | 把 child `AgentMachine` 挂到父 machine 的 slot 上,组成 machine 树 | external agent machine 作为 child machine 挂载 |
| `RunContext` | cancel、budget、trace、depth 贯穿 | 外部 agent 共享预算,接受取消,记录 trace |
| `AgentSpec::WorktreeRef` | agent 的工作目录边界 | 外部 agent session 的 repo/worktree |
| `AgentRuntimeHandles` | live handle 不进 serde state | 放 CLI 进程、SDK session、watcher、task handle |
| `agent-testkit` cassette | effect 边界录制/重放 | 后续录制 external session req/resp 与 interaction |

这些设施说明:外部 agent 不需要绕过现有模型。缺的是“外部 session effect”和对应 DTO,不是新的
interaction 或新的 orchestration runtime。

## 3. 核心决策

### 3.1 External agent 是 Agent,不是 LLM provider

允许一个低保真 adapter 把 Claude Code/Codex 流式输出 fold 成 `RequirementResult::Llm(Response)`,
用于快速试验。但这只是兼容层,不是目标设计。

> (spike 修正,见附录 A) Phase 0 实测证实 fold 可跑通,但**有损**:折叠后的 `Response`
> 无法承载 per-event usage/成本、也无法表达 permission 请求这类决策点,进一步印证它只能作为
> 一次性兼容层,不能沿用为目标 DTO。

目标设计中:

```text
CcAgent / CxAgent / OpenCodeAgent
  = ExternalAgentSpec(data)
  + ExternalAgentState(data)
  + ExternalAgentMachine(sans-io)
  + ExternalSessionHandler(runtime effect handler)
```

父 agent 通过 `NeedSubagent` 派生它。它内部通过 external session effect 推进 CLI/SDK session,通过
`NeedInteraction` 问人或问 policy,通过 `NeedTool` 或注入工具使用宿主能力。

`ExternalAgentMachine` 实现现有 `AgentMachine` trait,因此可以像 `DefaultAgentMachine` 一样被
`NestedMachine` 挂到父 machine 的 child slot 上,并由 `SubagentHandler` 在兑现 `NeedSubagent` 时
派生 child `RunContext`/scope 后驱动。它与内部 agent 走完全相同的挂载与派生路径,不需要独立 driver。

### 3.2 Session 是 effect,不是一次 LLM call

外部 agent 的基本 IO 单位是“推进一个外部 session”,而不是“请求一次模型生成”。建议新增一类 requirement:

```rust
pub enum RequirementKind {
    NeedLlm { request: ChatRequest, mode: LlmStepMode },
    NeedTool { call_id: ToolCallId, call: ToolCall },
    NeedInteraction { request: Interaction },
    NeedSubagent { spec_ref: AgentSpecRef, brief: Interaction, result_schema: Option<Value> },
    NeedReconfigRegistry { tool_set: ToolSetRef },
    NeedExternalSession { request: ExternalSessionRequest }, // (拟新增)
}
```

`NeedExternalSession` 由 `ExternalSessionHandler` 兑现。handler 持有真实 runtime:CLI process、SDK client、
stdout/stderr decoder、stdin responder、session registry、watcher 等。

### 3.3 交互复用 `InteractionHandler`

外部 agent 的问题、选择、权限请求应该进入同一套 interaction 机制:

- 问用户补充信息 -> `InteractionKind::Question`。
- 让用户选择策略 -> `InteractionKind::Choice`。
- shell/edit/network/agent spawn 权限 -> 建议新增通用 permission variant。

现有 `InteractionKind::Approval` 绑定 `ToolCallId`,适合本框架 tool approval。外部 agent 的权限请求不一定是
provider-neutral `ToolCall`,因此不应长期复用这个 shape。建议演进为:

```rust
pub enum InteractionKind {
    Approval { call_id: ToolCallId, requirement: ApprovalRequirement },
    Question { prompt: String },
    Choice { prompt: String, options: Vec<String> },
    Permission { request: PermissionRequest },
}

pub struct PermissionRequest {
    pub action_id: String,
    pub actor: AgentId,
    pub category: PermissionCategory,
    pub summary: String,
    pub subject: serde_json::Value,
    pub risk: PermissionRisk,
    pub reason: Option<String>,
}

pub enum PermissionCategory {
    Shell,
    FileRead,
    FileWrite,
    Network,
    SpawnAgent,
    Mcp,
    Other,
}
```

顶层可以挂真人 UI,也可以挂 headless policy。中间 external agent scope 不挂 interaction handler 时,
权限请求自然 pop 到外层。

### 3.4 工具是 capability adapter

External agent 可以被注入工具,但这些工具应是宿主能力的薄 adapter,不应绕过 `RunContext` 护栏。

常见工具:

| 工具 | 宿主语义 |
|---|---|
| `spawn_agent` | 转成 `NeedSubagent`,由 `SubagentHandler` 派生 child context |
| `send_message` | 写入本库 mailbox / blackboard,不是直接写 CC 私有 mailbox |
| `plan_claim` / `plan_claim_first_available` / `plan_update` | 操作本库 plan API;claim 必须检查依赖已完成 |
| `blackboard_post` / `blackboard_read` | 操作本库 blackboard API |
| `report_artifact` | 把 diff、patch、测试结果、文件路径记录为 artifact/notification |
| `run_host_tool` | 受控调用宿主注册的 tool |

这样 CC/CX/OpenCode 即使有自己的内置工具,也可以通过注入这些桥接工具参与同一个 mixed-agent
Session。

### 3.5 外部 agent 通信走本库协议

Claude Code 的普通 subagent 只把结果回报给调用方。Claude Code Agent Teams 有共享 task list 和 mailbox,
底层使用本地目录与 JSON inbox。这个机制可以作为参考,但不应成为本库的稳定通信协议。

本库应提供自己的 plan / blackboard / mailbox 原语:

- `plan`:有状态任务板,支持 task status、`depends_on` 依赖数组、claim 前置完成检查、claim-first 可用任务入口、CAS 更新。
- `blackboard`:append-only 消息流,用于发现、讨论、广播。
- `mailbox`:可选的定向消息层,用于 agent-to-agent direct message。

外部 agent 通过注入工具访问这些原语。内部 agent、CC/CX/OpenCode agent 使用同一个协议,才能跨 runtime
协作、测试和回放。

## 4. 概念数据模型

### 4.1 Static spec

```rust
pub struct ExternalAgentSpec {
    pub id: AgentId,
    pub runtime: ExternalRuntimeKind,
    pub worktree: WorktreeRef,
    pub profile: WorkerProfileRef,       // (拟新增) worker 能力/成本画像引用,见 §9
    pub initial_tools: ToolSetRef,
    pub session_policy: ExternalSessionPolicy,
}

pub enum ExternalRuntimeKind {
    ClaudeCode,
    Codex,
    OpenCode,
    Custom(String),
}

pub struct ExternalSessionPolicy {
    pub permission_mode: ExternalPermissionMode,
    pub isolation: WorktreeIsolation,    // (拟新增) worktree 隔离策略,见 §10
    pub max_turns: Option<u32>,
    pub stream_events: ExternalStreamPolicy,
}
```

> 说明:`AgentId`、`WorktreeRef`、`ToolSetRef` 为已有类型;`ExternalAgentSpec`、
> `ExternalRuntimeKind`、`ExternalSessionPolicy`、`ExternalPermissionMode`、
> `ExternalStreamPolicy`、以及上面标注的 `WorkerProfileRef` / `WorktreeIsolation`
> 均为 **(拟新增)**。`WorkerProfileRef` 承载 worker 的能力标签、成本档位与升级规则(§9);
> `WorktreeIsolation` 描述 shared / per-agent-worktree / ephemeral-git-worktree 等隔离级别(§10)。

它可以并入现有 `AgentSpec` 的扩展字段,也可以作为 external-agent registry 内部的 spec。

### 4.2 Serializable state

```rust
pub struct ExternalAgentState {
    pub spec: ExternalAgentSpec,
    pub conversation: Conversation,
    pub session: Option<ExternalSessionRef>,
    pub cursor: ExternalAgentCursor,
    pub active_tools: ToolSetRef,
}

pub struct ExternalSessionRef {
    pub runtime: ExternalRuntimeKind,
    pub session_id: Option<String>,
    pub transcript_ref: Option<String>,
    pub resume_token: Option<String>,
    pub last_event_seq: Option<u64>,
}

pub enum ExternalAgentCursor {
    Idle,
    AwaitingSession { requirement: CursorRequirement },
    AwaitingInteraction { requirement: CursorRequirement, pending_action: String },
    Done,
    Error { message: String },
}
```

状态只存可恢复事实。CLI process、SDK client、stdout reader、watcher、tmux pane、in-process teammate handle
属于 runtime handle,不进 serde。

### 4.3 Runtime handle

```rust
pub struct ExternalRuntimeHandles<R> {
    pub runtime: R,
    pub interaction: Option<Arc<dyn InteractionHandler>>,
    pub tool_registry: Option<Arc<dyn ToolRegistry>>,
    pub session_tasks: TaskSet,
}
```

这可以沿用 `AgentRuntimeHandles` 的泛型 holder 思路,也可以由外部应用自己的 Session 容器持有。

## 5. External session effect

### 5.1 Request

```rust
pub struct ExternalSessionRequest {
    pub agent_id: AgentId,
    pub runtime: ExternalRuntimeKind,
    pub worktree: WorktreeRef,
    pub session: Option<ExternalSessionRef>,
    pub input: ExternalSessionInput,
    pub tools: Vec<Tool>,
    pub policy: ExternalSessionPolicy,
}

pub enum ExternalSessionInput {
    Start { prompt: String },
    Continue { message: String },
    RespondInteraction { action_id: String, response: InteractionResponse },
    Shutdown,
}
```

### 5.2 Result

```rust
pub enum ExternalSessionResult {
    Completed {
        session: ExternalSessionRef,
        output: ExternalAgentOutput,
        observations: Vec<ExternalAgentEvent>,
    },
    PausedForInteraction {
        session: ExternalSessionRef,
        action_id: String,
        request: Interaction,
        observations: Vec<ExternalAgentEvent>,
    },
    Failed {
        session: Option<ExternalSessionRef>,
        error: ExternalAgentError,
        observations: Vec<ExternalAgentEvent>,
    },
}

pub struct ExternalAgentOutput {
    pub summary: String,
    pub artifacts: Vec<ExternalArtifactRef>,
    pub usage: Option<Usage>,
    pub cost_micros: Option<u64>,
}
```

`PausedForInteraction` 是关键:session handler 不需要自己调用 `InteractionHandler`。它把外部 runtime 的权限请求
转换为一个普通 `Interaction`,返回给 external agent machine。machine 下一步 emit `NeedInteraction`,该 requirement
按现有 pop 规则由 local 或 outer interaction handler 兑现。拿到 `InteractionResponse` 后,machine 再 emit
`NeedExternalSession { input: RespondInteraction { .. } }` 把结果喂回外部 runtime。`action_id` 是 runtime 对本次
暂停动作的句柄:machine 存为 cursor 的 `pending_action`,并在 `RespondInteraction { action_id, response }` 里原样回喂,
让 runtime 能把回答对回它暂停的那个动作。在 `InteractionKind::Permission`(§4)落地前,`action_id` 由这里显式携带;
落地后它仍是 machine 回喂时使用的规范句柄。

这个两段式设计比“给任意 handler 一个 `Pop`”更克制:

- 每个 interaction 都有标准 `RequirementId`、cursor、trace。
- 不需要让所有 handler 都能 re-enter driver。
- default outer handler 仍由动态 scope 决定。
- 跨进程恢复时能从 cursor 重建未决 interaction。

### 5.3 Event

```rust
pub enum ExternalAgentEvent {
    SessionStarted { session_id: Option<String> },
    TextDelta { text: String },
    CommandStarted { command: String, cwd: String },
    CommandFinished { exit_code: Option<i32>, stdout_tail: String, stderr_tail: String },
    FilePatch { path: String, summary: String, diff_ref: Option<String> },
    PermissionRequested { action_id: String, summary: String },
    ToolStarted { name: String },
    ToolFinished { name: String, status: ToolStatus },
    MessageSent { to: AgentId, summary: String },
    TaskUpdated { task_id: String, status: String },
    SessionCompleted,
}
```

建议新增 `Notification::ExternalAgent(ExternalAgentEvent)`。首版也可以把 observations 放在
`ExternalSessionResult` 中,由 machine 在 resume 后一次性吐出 notifications。若 UI 需要实时 token/command/patch
展示,需要给 handler 增加 event sink 或在 `RunContext` 挂 trace/event emitter。

> 当前 `Notification` 只有 `Llm` / `StepBoundary` / `ToolCallStarted` / `ToolCallFinished` 四个变体
> (见 `src/agent/event.rs`),`Notification::ExternalAgent` 为 **(拟新增)**。

### 5.4 Error

`ExternalSessionResult::Failed` 与 §7 都引用 `ExternalAgentError`,这里给出建议形状。它应把
"可诊断的失败原因"与"是否可重试 / 是否残留副作用"分开表达:

```rust
pub enum ExternalAgentError {          // (拟新增)
    /// 无法启动 runtime(二进制缺失、SDK 初始化失败、鉴权失败)。
    Launch { runtime: ExternalRuntimeKind, detail: String },
    /// session 进程/连接在推进中断开或崩溃。
    SessionLost { session: Option<ExternalSessionRef>, detail: String },
    /// 流事件或 transcript 解析失败(协议/版本漂移)。
    Protocol { detail: String },
    /// 超过 policy 限额(max_turns、wall-clock、budget)。
    LimitExceeded { limit: String },
    /// resume 失败:session/transcript/resume_token 不再有效。
    ResumeUnavailable { session: ExternalSessionRef, detail: String },
    /// 关闭 session 时失败,可能残留未纳管进程或未回滚副作用。
    ShutdownFailed { session: ExternalSessionRef, detail: String },
    /// 外部 runtime 主动上报的错误。
    Runtime { code: Option<String>, message: String },
}
```

`ShutdownFailed` 与 `SessionLost` 应被视为"可能残留副作用"信号:上层 trace 必须记录,
调度器不应默认把该 worktree 当作干净状态复用(见 §6.4、§10)。

### 5.5 阻塞式 effect 与持续流的调和

现有 effect 模型是"一次 `Requirement` → 一次 `RequirementResult`"的阻塞语义,而 CC/Codex/OpenCode
session 是**持续流式**的:一个 turn 内会吐大量 text/command/patch event,并可能多次请求权限。二者
需要显式调和,否则 handler 要么阻塞住整条流,要么被迫在 effect 边界外偷偷调用 `InteractionHandler`。

约定的模型是:**handler 在后台 task 上跑 session,把 event 缓冲,只在"下一个决策点"返回一次 result。**

> (spike 修正,见附录 A) Phase 0 用「独立 reader task + `mpsc` + `tokio::select!`」原型验证了这条
> 后台缓冲模型:`mpsc::Receiver::recv` 的 cancel-safe 特性让"投递增量"与"侦测取消"可以安全竞速。
> 但 spike 的**逐行文本**解码不足以表达结构化 event(text/command/patch/permission),目标
> `ExternalAgentEvent` 必须是从帧解码出的结构化枚举,且 reader task 应归 runtime handle 长期持有,
> 而非每次兑现 `NeedExternalSession` 重开子进程。

- `ExternalSessionHandler` 兑现 `NeedExternalSession` 时,不是同步跑到 session 结束,而是把 session
  推进到**下一个决策点**:`Completed`(本轮无更多输入需求)、`PausedForInteraction`(需要审批/澄清)、
  或 `Failed`。
- 决策点之前累积的所有 event 放进 `observations: Vec<ExternalAgentEvent>` 一并返回,machine 在 resume
  后把它们转成 `Notification::ExternalAgent` 吐出。这样"阻塞的 result"只标记**控制流转移点**,
  "非阻塞的流"通过 observations / event sink 表达,两者不互相污染。
- session 进程、stdout/stderr decoder、watcher 由 runtime handle 长期持有(见 §4.3),跨多个
  `NeedExternalSession` 兑现存活;`ExternalSessionRef` 里的 `last_event_seq` 用于 resume 时对齐游标、
  避免重复回放已消费的 event。
- 若 UI 需要**真正实时**(在决策点之前就看到 token/command),则在 handler 上挂 event sink 或
  `RunContext` trace emitter 旁路输出;这条旁路不阻塞 continuation,可丢弃、可跳过。真正阻塞
  continuation 的只有 `Requirement`。

这条约定同时约束了取消语义:后台 session task 的关闭责任落在 handler / runtime handle 上,而不是靠
machine 再走一步 effect,详见 §6.4。

## 6. 推进流程

### 6.1 启动外部 agent

```text
parent agent
  perform NeedSubagent { spec_ref: cc-agent, brief }
    ↓
SubagentHandler derives child RunContext and child scope
    ↓
CcAgentMachine.step(UserMessage(brief))
  -> NeedExternalSession(Start { prompt })
    ↓
ExternalSessionHandler starts claude/codex/opencode session
  -> Completed / PausedForInteraction / Failed
```

### 6.2 权限请求

```text
ExternalSessionHandler observes: "run shell command?"
  -> ExternalSessionResult::PausedForInteraction { request: Permission(...) }
    ↓
CcAgentMachine resumes session result
  -> NeedInteraction(Permission(...))
    ↓
local scope lacks interaction -> pop to parent/root
    ↓
root InteractionHandler asks human or applies policy
    ↓
CcAgentMachine receives InteractionResponse
  -> NeedExternalSession(RespondInteraction { action_id, response })
```

### 6.3 子 agent 创建

```text
CcAgent runtime calls injected tool: spawn_agent(kind="internal-cheap", brief="run tests")
  -> tool adapter returns a structured request to CcAgentMachine
  -> CcAgentMachine emits NeedSubagent
  -> SubagentHandler enforces depth, budget inheritance, cancel propagation
```

不要让外部 agent 直接 spawn 未纳管进程来创建“子 agent”。子 agent 创建应通过宿主工具回到 `NeedSubagent`,否则会绕过
depth、budget、cancel、trace 和 permission。

### 6.4 取消与 session 清理

取消在本库是 **never-resume**:`RunContext` 触发 cancel 后,driver 放弃当前 continuation,machine
**不会再被 step**。这带来一个外部 agent 特有的问题——被放弃的 continuation 不可能再 emit
`NeedExternalSession { input: Shutdown }`,那么已经启动的 CLI 进程 / SDK session 谁来关?

约定:**外部 session 的进程生命周期归 runtime handle 所有,清理不依赖 machine 再走一步 effect。**

- session 进程、task、watcher 由 `ExternalRuntimeHandles`(§4.3)与一个 session registry 持有。
  cancel/drop 时,由 handler 侧的 registry 清扫对应 session:kill 进程、关闭 stdin/stdout、终止后台
  task。`Shutdown` input 只是**正常路径**下的优雅关闭,不是 cancel 路径的唯一出口。
- `AgentRuntimeHandles` / `ExternalRuntimeHandles` 应实现 `Drop`(或由 owning Session 容器在 teardown
  时统一清扫),保证 continuation 被放弃后不残留孤儿进程。
- cancel 属于"可能残留不可回滚副作用"的情形:即便进程被 kill,外部 runtime 此前已经执行的
  shell/edit/network 副作用无法回滚。trace 必须记录 cancellation 与 shutdown disposition
  (成功优雅关闭 / 强制 kill / 关闭失败),调度器据此决定该 worktree 是否需要重建再复用(§10)。
- `session_id` / `transcript_ref` / `resume_token` 仍留在 serializable state 中:进程虽被关闭,
  若外部 runtime 支持 resume,后续可用这些游标重新起一个 session,而不是丢失整段历史。

M3-4 落地的具体类型:

- `ExternalSessionShutdown`(`Graceful` / `ForcedKill` / `Failed`,`Copy` 分类)是 shutdown disposition
  的载体,`leaves_residual_side_effects()` 对 `ForcedKill` / `Failed` 返回 `true`,提示调度器该
  worktree 可能不干净(§10)。详细失败文本仍留在 `ExternalAgentError::ShutdownFailed`,不塞进这个分类。
- `TraceHandle::record_external_shutdown(id, disposition)` 把该 disposition 记进 trace,对应
  `TraceNodeKind::ExternalShutdown { disposition }` 节点,由 handle 侧在强制关闭 session 后调用。
- machine 是 sans-io 的,cancel 时 `step(Abandon)` 只能把孤儿 session 标记到 state 上:
  `ExternalAgentState::mark_cleanup_required()` 置位、`cleanup_required()` 查询、
  `clear_cleanup_required()` 在 handle 侧清扫后复位。machine **不** emit `Shutdown`,真正的 kill 与
  disposition 记录都发生在 handle 层(`ExternalRuntimeHandles` 的 Drop 或 session 容器统一清扫)。
- 外部 agent 作为子 agent 挂载走标准 `NeedSubagent` 路径:`ExternalAgentMachine` 是普通
  `AgentMachine`,由 `DrivingSubagentHandler` 在派生的子 `RunContext` 下开一层嵌套 drain 驱动,天然继承
  depth / budget / cancel / trace。

## 7. 三种集成深度

选择哪种集成深度,取决于目标 runtime 暴露了什么能力。下表是三个参考 runtime 的能力预估
(需以实际 CLI/SDK 版本为准,建议随实现在 [`capability-matrix.md`](./capability-matrix.md) 补录实测值):

| 能力 | Claude Code | Codex | OpenCode |
|---|---|---|---|
| session resume / transcript token | 有(resume) | 视 CLI 版本 | 视配置 |
| 结构化流事件(text/command/patch 可解析) | 较完整 | 部分 | 视配置 |
| 权限 hook(可外接审批) | 有(permission mode) | 有限 | 视配置 |
| 注入自定义工具 / tool bridge | 有(MCP / 内置工具) | 有限 | 可配置 |
| 子 agent / teammate 机制 | 有(Agent Teams) | 无/弱 | 视配置 |
| 适合的默认集成深度 | 半托管→全托管 | 黑盒→半托管 | 半托管 |

> 上表为设计期预估,不是运行时保证;能力探测与归一化的开放问题见 §14。

### 7.1 黑盒模式

外部 runtime 自己执行工具和审批,本库只看到最终 summary。

优点:

- 最快实现。
- 对 CLI/API 要求低。
- 适合作为 premium worker 跑复杂任务。

缺点:

- trace 粗。
- 权限和文件改动不完全受本库管控。
- 难做细粒度预算、回放、质量门。

### 7.2 半托管模式

本库解析流式事件,接管 interaction、cancel、session resume、artifact 记录,但 shell/edit 仍由外部 runtime 执行。

优点:

- 能复用 `InteractionHandler`。
- UI 能看到 command/patch/text 事件。
- 实现成本适中。

缺点:

- 文件写入仍发生在外部 runtime 内。
- 工具执行结果不一定能转成 provider-neutral `ToolResponse`。

### 7.3 全托管模式

外部 runtime 的 shell/edit/network/subagent 等能力都通过本库注入工具完成。

优点:

- 权限、trace、budget、cassette、policy 最统一。
- 容易混合 internal/cc/cx/opencode worker。
- 更适合企业审计和 headless 自动化。

缺点:

- adapter 工作量最大。
- 需要外部 runtime 支持自定义工具或结构化 tool bridge。

推荐路径:先做半托管,保留黑盒 fallback,再逐步把高风险能力迁到全托管工具。

## 8. Mixed-agent Session

最终产品形态可以是一个混合 agent 集:

```text
Session root
  scope:
    interaction -> UI / headless policy
    subagent    -> spawn internal / cc / cx / opencode agents
    tool        -> host tools / MCP / orchestration tools
    reconfig    -> registry resolver

  coordinator / planner
    -> task evaluator
    -> dispatcher
    -> workers:
         internal-cheap-agent
         deepseek-shell-agent
         deepseek-code-agent
         cc-agent
         cx-agent
         opencode-agent
         review-agent
```

不同 worker 不需要同构:

| Worker | 实现 | 用途 |
|---|---|---|
| internal cheap agent | `DefaultAgentMachine + LlmHandler` | 搜索、简单 shell、小修改 |
| DeepSeek agent | `DefaultAgentMachine + cheap LlmHandler` | 低成本代码阅读、机械改动 |
| Claude Code agent | `ExternalAgentMachine + ExternalSessionHandler` | 复杂实现、多文件修改、debug |
| Codex agent | `ExternalAgentMachine + ExternalSessionHandler` | 代码生成、review、替代强 worker |
| OpenCode agent | `ExternalAgentMachine + ExternalSessionHandler` | 本地/可配置 coding runtime |
| review agent | 可强可弱 | 验证、升级判断、质量门 |

## 9. Task evaluator 与调度

混合 agent 集需要一个 cost-aware / capability-aware dispatcher。建议把它拆为两层:

- 规则路由:确定性、低成本,先处理明显任务。
- LLM evaluator:只在模糊任务或高风险任务上调用。

评估维度:

| 维度 | 示例 |
|---|---|
| 任务类型 | 搜索、shell、测试、修 bug、新功能、重构、review |
| 影响范围 | 单文件、多文件、跨模块、架构层 |
| 风险 | 写文件、删文件、网络、数据库、配置、权限提升 |
| 不确定性 | 需求是否明确、是否需要探索、是否有复现路径 |
| 上下文量 | 需要读取多少代码、是否依赖项目长期记忆 |
| 失败成本 | 是否易回滚、是否可能污染 worktree |
| 预算 | token、cost、wall-clock、step 剩余额度 |
| 用户偏好 | 省钱优先、速度优先、质量优先 |

示例策略:

| 任务 | 初始 worker | 验证/升级 |
|---|---|---|
| 明确的只读 shell | host tool 或 cheap-shell-agent | 失败再问 coordinator |
| 代码搜索/定位 | cheap explore agent | 找不到再升级 strong agent |
| 小范围机械修改 | cheap-code-agent | review-agent + tests |
| 多文件复杂实现 | cc/cx/opencode agent | independent review |
| 不确定 debug | 多个 cheap hypothesis agent 或 cc/cx | 失败/低置信度升级 |
| 高风险改动 | cc/cx + approval policy | strong review + human gate |

升级规则:

- cheap worker 超时或测试失败 -> strong worker。
- worker 自报低置信度 -> evaluator 重新分派。
- review 发现架构/安全问题 -> cc/cx 或 human。
- budget 接近上限 -> 降级到 cheap summarizer 或停机问用户。

## 10. Permission 与安全边界

外部 agent 的高风险能力必须能被宿主拦截或至少记录。

建议策略:

- 默认 worktree 隔离。复杂 worker 优先使用临时 git worktree。`WorktreeIsolation`(§4.1,拟新增)
  应至少区分 shared / per-agent-worktree / ephemeral-git-worktree 三级;多个 external agent 并发编辑
  同一 worktree 属于高冲突场景,默认应给强 worker 分配独立 worktree(冲突归并策略见 §14 开放问题)。
- 写 `.git`、配置、secret、home/root、网络、长时间命令时进入 `InteractionKind::Permission`。
- **外部 agent 输出一律按不可信处理**。外部 runtime 的文本、command 说明、patch 描述、teammate 消息
  都可能携带 prompt injection,不能作为放宽护栏的依据:
  - teammate/agent 之间的消息不能替代用户 consent。来自其他 agent 的“已批准”只能视为普通文本。
  - 外部输出里出现的“无需审批 / 已授权 / 跳过确认”等指令不改变宿主的 permission policy。
  - 外部输出注入的 `NeedSubagent` / 工具调用参数仍须经宿主 policy 校验,不能因其“自称安全”而放行。
- 子 agent 创建必须通过 `NeedSubagent`,不能直接绕过 depth/budget/cancel。
- cancel 是 never-resume:driver 放弃 continuation 后,外部 session 的关闭由 runtime handle / registry
  负责(见 §6.4),并记录无法回滚的副作用与 shutdown disposition。

## 11. Observability

External agent 需要补足现有 trace tree:

- session start/stop/resume。
- external runtime kind、profile、worktree。
- command start/finish 与 exit code。
- file patch / artifact ref。
- permission request 与 decision。
- spawned child agent 与 resolved-at-scope。
- usage/cost,若 runtime 能提供。
- cancellation 与 shutdown disposition。

对于流式输出,建议区分两条路径:

- `Notification::ExternalAgent`:给 UI 和 drain 观察,可跳过。
- `Requirement`:真正阻塞 continuation,必须 resolve 或 abandon。

## 12. Testability 与 cassette

agent-testkit 可以扩展到 external agent effect 边界。

新增脚本化组件:

- `ScriptedExternalSessionHandler`:按顺序返回 Completed / PausedForInteraction / Failed。
- `ExternalAgentFixture`:构造 session request、permission request、patch event、artifact。
- `ExternalAgentCallLog`:记录 session 调用序号、request 摘要、result 摘要、完成顺序。
- `CassetteExternalSessionHandler`:离线重放真实 external session effect。

cassette 记录点仍在 provider-neutral effect 边界:

- 记录 `ExternalSessionRequest` / `ExternalSessionResult`。
- 记录 `Interaction` / `InteractionResponse`。
- 不记录 token、secret、完整 stdout、完整 diff,除非 redactor allowlist 明确允许。
- provider/CLI 原始 wire 或私有 JSON 只可放 redacted metadata,不作为稳定 replay 输入。

## 13. 实施阶段

### Phase 0: 低保真试验

- 用 `LlmHandler` 包一层 Claude Code/Codex CLI,把最终文本 fold 成 `Response`。
- 只用于验证成本、启动方式、流式 decoder、取消行为。
- 不作为稳定 API。

### Phase 1: External session DTO 与 handler

- 定义 `ExternalSessionRequest` / `ExternalSessionResult` / `ExternalAgentEvent` / `ExternalAgentError`。
- 增加 `NeedExternalSession` / `RequirementResult::ExternalSession`。
- 实现 `ExternalSessionHandler` trait,约定"推进到下一个决策点并缓冲 observations"的语义(§5.5)。
- 实现 scripted handler 与基础 tests。

### Phase 2: Custom external agent machine

- 实现 `ExternalAgentMachine`(实现 `AgentMachine` trait,可由 `NestedMachine` 挂载)。
- 支持 Start -> AwaitingSession -> Completed/Paused/Failed。
- 支持 PausedForInteraction -> NeedInteraction -> RespondInteraction。
- 支持 cancel/abandon 后由 runtime handle / registry 关闭 session 或记录 shutdown failure(§6.4)。

### Phase 3: Interaction permission 泛化

- 增加 `InteractionKind::Permission`。
- 增加 `PermissionResponse` 或扩展 `InteractionResponse`。
- root UI/headless policy 支持 approve/deny/timeout/cancel。

### Phase 4: Event sink 与 artifact

- 新增 `Notification::ExternalAgent`。
- 定义实时 event sink 或 trace emitter。
- 将 patch/diff/test result 记录为 artifact ref。

### Phase 5: Mixed-agent scheduler

- 定义 worker profile registry。
- 实现 task evaluator 与 dispatcher。
- 提供 `spawn_agent`、plan、blackboard、mailbox 工具 adapter。
- 增加 cheap -> strong escalation 与 verifier。

## 14. 未定问题

- `NeedExternalSession` 是否进入核心 `agent-lib`,还是先在上层 crate 作为 custom machine + custom driver 扩展。
- `Notification` 是否应承载 external event,还是通过独立 app event sink 输出。
- `InteractionKind::Permission` 的最小字段集与审批结果 shape。
- 外部 runtime 的 session resume 能力差异如何归一化。
- black-box 模式下如何定义“完成”和“文件改动归属”。
- 多 external agent 同时编辑同一 worktree 时的冲突策略。
- task evaluator 是规则优先、模型优先,还是 policy engine + LLM fallback。
- 是否需要稳定 `Mailbox` 一等 API,还是先用 `blackboard` + direct-message tool 组合。

## 15. 设计收敛

External agent 的正确抽象层次是:

```text
不是: ClaudeCode/Codex -> LlmClient
可以: ClaudeCode/Codex -> low-fidelity LlmHandler adapter
目标: ClaudeCode/Codex/OpenCode -> custom AgentMachine + ExternalSessionHandler
```

它们作为 mixed-agent Session 中的 worker,通过 `NeedSubagent` 被派生,通过 `NeedInteraction` 问用户,
通过注入工具访问 plan/blackboard/mailbox/subagent 能力,通过 `RunContext` 继承 cancel/budget/trace。

这样 cheap model、premium coding agent、内部 agent、外部 CLI agent 可以在同一个程序里协作,同时保留
effect-model 的核心价值:可暂停、可恢复、可测试、可审计、可按动态作用域组合 handler。

## 附录 A:Phase 0 spike 结论

> 来源:Milestone 1 / 任务 M1-1 的低保真 spike(`examples/external_cli_spike.rs`,可用
> `cargo run --example external_cli_spike` 复现)。spike 用 `sh -c` stub 脚本占位外部 CLI,
> 全程离线、无真实 Claude Code/Codex/OpenCode、无网络与 credentials,只用现有 `LlmHandler`
> 边界观察一个进程外 runtime 如何插入。本附录把实测结论回灌到设计假设,指导 Milestone 2+ 的取舍;
> 附录只追加、不修改上文既有结论。

### A.1 启动方式

- 实测:以 `tokio::process::Command` 拉起子进程,`stdin=null`、`stdout=piped`、`stderr=null`、
  `kill_on_drop(true)`;请求 prompt 经 `SPIKE_PROMPT` env 透传给 stub。进程 spawn 是廉价、同步的,
  失败以 `ClientError::Other` 表达。
- 观察:env 透传 prompt 只是 spike 便宜行事——真实 CLI 各家投递入口不同(argv / stdin / 配置文件 /
  `--prompt-file`),不该把"单一投递通道"焊进 DTO。prompt 应作为**数据**留在 request 里,由 handler
  决定落到哪个通道。
- 对设计的影响:`ExternalSessionRequest::Start` 需承载 prompt(及可选投递提示)作为纯数据;进程持有权、
  `kill_on_drop`、spawn 失败到 `ExternalAgentError` 的映射归 `ExternalSessionHandler` / runtime handle。

### A.2 流 decoder 形态

- 实测:独立 reader task 用 `BufReader::lines()` 逐行读 stdout,经**有界 `mpsc`**(cap 16)把每行投递给
  主循环;主循环 `tokio::select!` 让"收增量"与"轮询取消"竞速。`recv()` cancel-safe,不会丢半行。
  Streaming 模式下每行打印 `[stream +N]`,EOF 后把累计文本 fold 成一个 `Response`。
- 观察:"后台 task 缓冲 + 决策点返回一次 result"的形态(§5.5)成立且顺手。但**逐行纯文本**解码把
  text/command/patch/permission 混为一谈,无法表达权限请求这类决策点;真实 CLI 多为 JSON 事件流
  (可能多行成帧),不是裸文本行。此外 spike 每个场景**重开一个子进程**,没有跨兑现存活的 session。
- 对设计的影响:`ExternalAgentEvent` 必须是从帧解码的**结构化枚举**;reader/decoder 与进程应由
  runtime handle(§4.3)长期持有、跨多次 `NeedExternalSession` 兑现存活,`last_event_seq` 游标才有意义。

### A.3 取消行为

- 实测:主循环每 10ms 轮询 `RunContext::is_cancelled`,一旦置位即 `child.start_kill()` + `wait()`,
  并返回 `ClientError::Other("run cancelled: killed external CLI after N streamed chunk(s)")`。
  场景 3 用 1000 chunk × 50ms 的长跑 stub,在 ~150ms 处 `cancellation().cancel()`,子进程被稳定 kill。
- 观察:§6.4 的 never-resume 语义被证实——取消后 machine 不会再被 step,进程只能由 handler 侧 kill,
  没有机会再走一步 `Shutdown` effect。spike 用 `sleep` 轮询是权宜;真实 handler 应 `await` cancellation
  future / token 而非忙轮询。折叠出的 `Response` 无法区分"正常 EOF"与"被 kill",disposition 丢失。
- 对设计的影响:进程生命周期与清理必须落在 handler / runtime handle 的 `Drop` 路径(§6.4);结果 DTO
  需显式携带 shutdown disposition(优雅关闭 / 强制 kill / 关闭失败),不能靠折叠文本推断。

### A.4 成本量级

- 实测:fold 时 usage 用**词数粗估**(`split_whitespace().count()` 作 output token,input=0),
  仅为兼容 handler 契约的 shape,并非真实计量。spike 进程本身极廉价(stub 只做 `printf`+`sleep`)。
- 观察:黑盒 stdout 文本拿不到真实 token / 成本;fold-to-`Response` 会丢掉 per-event usage。真实成本
  只能来自 runtime 自己的账单/用量输出(若有),否则应显式标记为未知,而不是用文本反推。
- 对设计的影响:`ExternalSessionResult` 需要独立的 usage/cost 字段,来源是 runtime 自报或标记未知;
  调度器(§9)的 cheap→strong 决策不能建立在折叠文本的词数估算上。

### A.5 对 Milestone 2 的具体影响(可操作项)

1. **M2-1 DTO**:`ExternalSessionRequest::Start` 把 prompt 当纯数据承载,不焊死投递通道(A.1);
   `ExternalAgentEvent` 设计为结构化枚举(text-delta / command / patch / permission-request / usage),
   而非裸文本行(A.2);`ExternalSessionResult` 增加显式 shutdown disposition 与 usage/cost 字段(A.3、A.4)。
2. **M2-2 `NeedExternalSession` / `RequirementResult::ExternalSession`**:result 的三态
   (Completed / PausedForInteraction / Failed)必须能携带决策点前缓冲的 `observations: Vec<ExternalAgentEvent>`,
   permission 请求走 `PausedForInteraction` 而不是混进文本流(A.2),对齐 §5.5。
3. **M2-3 `ExternalSessionHandler` trait**:约定 handler 持有**长期存活**的 runtime handle(进程 + decoder task),
   跨多次兑现存活;清理走 `Drop` / registry 而非再走一步 effect(A.2、A.3),满足 §6.4 的 never-resume 清理。
4. **M2-4 `ScriptedExternalSessionHandler`**:用脚本化决策点转移 + 预置 observations 复现三态,
   **默认路径禁用真实子进程 / `sleep` / 网络**(spike 已证明 stub 替身可行,且真实进程带来的不确定性
   不该进默认测试条件),保证每个用例 <1 分钟。
5. **§14 未定问题回填**:"black-box 完成定义"与"取消后副作用"两条获得实测支撑——完成须由结构化
   `Completed` 事件而非文本 EOF 判定;取消后副作用不可回滚,disposition 必须入 trace(§6.4、§10),
   供 M3-4 的 shutdown disposition 与 worktree 复用决策使用。

### A.6 Milestone 2 go/no-go 结论

- **Go**:effect 边界(`LlmHandler` → `RequirementResult`)足以承载一个进程外 runtime;后台缓冲 + 决策点
  返回、reader-task/`mpsc` cancel-safe 竞速、`is_cancelled` 驱动 kill 三点均实测可行,可进入正式 DTO/handler。
- **不需要**为核心库引入真实进程依赖:scripted 替身足以覆盖 Milestone 2 语义,真实 CLI 留到更后期的
  cassette / 集成层。
- **必须先决**:Milestone 2 的 DTO 一开始就要为"结构化 event、显式 disposition、显式 usage、
  长期存活 runtime handle"留位(A.1–A.4),否则 Milestone 3 的 machine 会被迫重演 spike 的有损折叠。
