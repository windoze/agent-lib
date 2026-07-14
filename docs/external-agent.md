# External Agent 设计

> 状态:设计草案。本文讨论如何在现有 sans-io + effect-handler 体系上接入 Claude Code、Codex、
> OpenCode 等外部 coding-agent runtime,并让它们和 `DefaultAgentMachine`、内部 LLM agent、
> MCP/tool、plan/blackboard 等能力在同一个程序里混合编排。

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

### 3.2 Session 是 effect,不是一次 LLM call

外部 agent 的基本 IO 单位是“推进一个外部 session”,而不是“请求一次模型生成”。建议新增一类 requirement:

```rust
pub enum RequirementKind {
    NeedLlm { request: ChatRequest, mode: LlmStepMode },
    NeedTool { call_id: ToolCallId, call: ToolCall },
    NeedInteraction { request: Interaction },
    NeedSubagent { spec_ref: AgentSpecRef, brief: Interaction, result_schema: Option<Value> },
    NeedReconfigRegistry { tool_set: ToolSetRef },
    NeedExternalSession { request: ExternalSessionRequest },
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
    pub profile: WorkerProfileRef,
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
    pub isolation: WorktreeIsolation,
    pub max_turns: Option<u32>,
    pub stream_events: ExternalStreamPolicy,
}
```

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
`NeedExternalSession { input: RespondInteraction { .. } }` 把结果喂回外部 runtime。

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

## 7. 三种集成深度

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

- 默认 worktree 隔离。复杂 worker 优先使用临时 git worktree。
- 写 `.git`、配置、secret、home/root、网络、长时间命令时进入 `InteractionKind::Permission`。
- teammate/agent 之间的消息不能替代用户 consent。来自其他 agent 的“已批准”只能视为普通文本。
- 子 agent 创建必须通过 `NeedSubagent`,不能直接绕过 depth/budget/cancel。
- cancel 是 never-resume:driver 放弃 continuation 后还要关闭外部 session 或记录无法回滚的副作用。

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

- 定义 `ExternalSessionRequest` / `ExternalSessionResult` / `ExternalAgentEvent`。
- 增加 `NeedExternalSession` / `RequirementResult::ExternalSession`。
- 实现 `ExternalSessionHandler` trait。
- 实现 scripted handler 与基础 tests。

### Phase 2: Custom external agent machine

- 实现 `ExternalAgentMachine`。
- 支持 Start -> AwaitingSession -> Completed/Paused/Failed。
- 支持 PausedForInteraction -> NeedInteraction -> RespondInteraction。
- 支持 cancel/abandon 后关闭 session 或记录 shutdown failure。

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
