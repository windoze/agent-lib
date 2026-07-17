# Managed External Agent 设计

> 状态:设计草案。本文是在 [`external-agent.md`](./external-agent.md) 已落地的
> `ExternalSessionRequest` / `ExternalSessionResult` / `NeedExternalSession` /
> `ExternalAgentMachine` 基础上,进一步设计“受管 external agent”:让 Claude Code、Codex、
> OpenCode 不只是黑盒子进程,而是尽可能具备内部 `DefaultAgentMachine` 已有的流式输出、tool
> 注入、user interaction、subagent、trace、budget、cancel、artifact 等能力。
>
> 标注约定:
>
> - **已实现**:当前代码已有对应类型或行为。
> - **拟新增**:本文建议新增或扩展的 API/行为。
> - **runtime-dependent**:取决于 Claude Code / Codex / OpenCode 当前 CLI/SDK 是否暴露能力。

## 0. 一句话

**Managed external agent = 外部 coding-agent runtime 的受管版 `AgentMachine`。**

它仍然把 Claude Code / Codex / OpenCode 当作外部 runtime,不把其私有 wire 协议变成
`agent-lib` 的稳定协议;但它不再只等一个最终 summary。它会把外部 runtime 的流式文本、命令、
patch、权限请求、tool call、子任务、artifact、usage 等事件归一化为 agent effect 模型中的
`Notification` 与 `Requirement`,由同一套 `HandlerScope` / `Pop` / `RunContext` 兑现。

目标效果:

```text
DefaultAgentMachine:
  NeedLlm -> NeedTool / NeedInteraction / NeedSubagent -> final assistant

Managed ExternalAgentMachine:
  NeedExternalSession
    -> External runtime stream events
    -> NeedTool / NeedInteraction / NeedSubagent
    -> RespondToolResults / RespondInteraction / RespondSubagent
    -> final external output
```

两者在 driver 看来都是 `AgentMachine`,都能作为子 agent 挂到 `DrivingSubagentHandler` 下,都能通过
scope wiring 决定 attended / headless 行为。

## 1. 背景

当前 `agent-lib` 已经具备 external agent 的基础层:

| 能力 | 当前状态 |
|---|---|
| `ExternalRuntimeKind::{ClaudeCode,Codex,OpenCode,Custom}` | 已实现 |
| `ExternalSessionRequest` / `ExternalSessionResult` DTO | 已实现 |
| `RequirementKind::NeedExternalSession` / `RequirementResult::ExternalSession` | 已实现 |
| `ExternalSessionHandler` effect handler | 已实现 |
| `ExternalAgentMachine` Start / Continue / Completed / Failed | 已实现 |
| external session 暂停到 `NeedInteraction` 再 `RespondInteraction` | 已实现 |
| observations -> `Notification::ExternalAgent` | 已实现 |
| artifact refs 记录到 `ExternalAgentState` | 已实现 |
| cancel / abandon 标记 `cleanup_required` | 已实现 |
| 作为 subagent child 被 `DrivingSubagentHandler` 驱动 | 已实现 |
| 真实 Claude Code / Codex / OpenCode runtime adapter | 未实现 |
| external runtime 发起 host tool call | machine 已实现(runtime handler 待实现) |
| external runtime 发起 host subagent | machine 已实现(runtime handler 待实现) |
| 长生命周期 session registry / process handles | 未实现 |
| structured streaming live sink + replay sequence | 部分实现(sink 已 sequenced,`ExternalStreamPolicy` 选择/runtime 接线待实现) |
| cassette replay 真实 external session | 未实现 |

也就是说,当前实现证明了“外部 agent 可作为一个 effect-driven machine 挂进系统”;但它还没有达到
内部 agent 的能力 parity。本文设计补齐这一层。

## 2. 目标与非目标

### 2.1 目标

1. **流式输出**
   外部 runtime 的 text delta、command、patch、permission prompt、tool event、task update 等应作为
   结构化 `ExternalAgentEvent` 暴露,可被 UI 实时消费,也可被 `drain` 折叠为 observations。

2. **tool 注入**
   `ExternalSessionRequest.tools` 不只是 prompt hint。runtime 如果支持 tool bridge/MCP/custom tool,
   它发起的 tool call 应转成 `NeedTool`,由宿主 `ToolHandler` 执行后回灌给 runtime。

3. **user interaction 集成**
   runtime 的权限请求、澄清问题、选项选择应转成 `NeedInteraction`,支持 attended parent scope 处理,
   也支持 headless policy 处理。

4. **subagent 集成**
   外部 runtime 请求派生子 agent 时,必须走现有 `NeedSubagent` / `SubagentHandler`,不能绕过宿主创建
   未受管进程树。

5. **与内部 machine 语义对齐**
   cancel、budget、trace、artifact、worktree isolation、tool failure policy、approval policy、
   loop limits、snapshot/restore 应尽量与 `DefaultAgentMachine` 一致。

6. **runtime 差异可观测**
   Claude Code、Codex、OpenCode 能力不同。系统应以 capability model 显示支持/降级/拒绝,不能假装全都
   支持。

7. **默认测试离线**
   真实 CLI/API 测试必须 ignored 或 cassette;默认 `cargo test --all --all-targets` 不依赖网络、凭据、
   本地登录态或未纳管进程。

### 2.2 非目标

- 不把 Claude Code / Codex / OpenCode 的私有 JSON/JSONL schema 作为 `agent-lib` 稳定 public API。
- 不要求三个 runtime 首版能力完全一致。
- 不要求 `ExternalAgentMachine` 自己做 IO。它仍是 sans-io machine;真实进程/SDK 在 handler/handles 层。
- 不把外部 runtime 的本地权限边界当作可信安全边界。宿主仍要通过 `InteractionHandler` / policy 控制。
- 不在核心 crate 默认启用真实 CLI adapter 所需的重依赖。真实 adapter 可用 feature gate 或上层 crate。

## 3. 能力 parity 表

| 内部 agent 能力 | `DefaultAgentMachine` | Managed external agent 目标 | 备注 |
|---|---|---|---|
| 文本 turn | `NeedLlm` -> assistant `Response` | `NeedExternalSession` -> `Completed.output` | 已有基础 |
| 多轮会话 | Conversation + model history | `ExternalSessionRef` + runtime resume/continue | runtime-dependent |
| 流式文本 | `StreamEvent` / `Notification::Llm` | `ExternalAgentEvent::TextDelta` / live sink | seq 已落地(M1),`ExternalEventSink` 已 sequenced(M4-1);runtime 接线待实现 |
| tool call | `ContentBlock::ToolUse` -> `NeedTool` | runtime tool call -> `NeedTool` -> `RespondToolResults` | machine 已实现,runtime handler 待实现 |
| tool approval | `NeedInteraction(Approval)` | runtime permission 或 host tool approval -> `NeedInteraction` | machine 已实现(interaction 校验),runtime handler 待实现 |
| user question | `NeedInteraction(Question)` | runtime question -> `NeedInteraction(Question)` | machine 已实现(`NeedInteraction`),runtime handler 待实现 |
| subagent | `NeedSubagent` | runtime spawn request -> `NeedSubagent` | machine 已实现,runtime handler 待实现 |
| tool failure policy | `ToolFailurePolicy` | external tool result error 回灌或 fail turn | 拟新增 |
| cancel | `StepInput::Abandon` closes pending | abandon marks cleanup + handler kills session | machine 已有,handler 待实现 |
| budget | handler/driver charge tokens/cost | runtime usage/cost event charge | 拟新增 |
| trace | requirement + tool + subagent nodes | external events + shutdown + artifacts | 部分已有 |
| artifact | tool/model output | patch/diff/test/file artifact refs | 部分已有 |
| worktree isolation | `WorktreeRef` | shared / per-agent / ephemeral worktree manager | 拟新增 |
| reconfig | queued tool set swap | boundary-level tool bridge reconfigure | 拟新增 |
| snapshot/restore | `AgentState` + Conversation snapshot | `ExternalAgentState` + `ExternalSessionRef` resume | state 已有,handler 待实现 |

## 4. 架构总览

```text
                          ┌────────────────────────────┐
                          │        host / driver        │
                          │ drain(machine, scope, ctx)  │
                          └──────────────┬─────────────┘
                                         │
                   requirements          │          resolutions
                                         │
┌────────────────────────────────────────▼─────────────────────────────────────┐
│                         ExternalAgentMachine                                  │
│ sans-io: opens Conversation turn, emits NeedExternalSession, NeedTool,         │
│ NeedInteraction, NeedSubagent, folds results, commits/cancels pending turn     │
└────────────────────────────────────────┬─────────────────────────────────────┘
                                         │ NeedExternalSession
┌────────────────────────────────────────▼─────────────────────────────────────┐
│                         ExternalSessionHandler                                │
│ owns live runtime registry, advances one runtime session to next decision      │
│ point, buffers structured observations, emits live stream events if configured │
└───────────────┬───────────────────────┬───────────────────────┬─────────────┘
                │                       │                       │
                ▼                       ▼                       ▼
       Claude Code adapter       Codex adapter           OpenCode adapter
       process/SDK/session       process/exec-server     process/API/session
       frame decoder             JSONL decoder           decoder
       tool bridge               MCP/tool bridge         MCP/tool bridge
```

关键分层:

| 层 | 职责 | 是否 sans-io |
|---|---|---|
| `ExternalAgentMachine` | 状态转移、effect reify、Conversation commit/cancel | 是 |
| `ExternalSessionHandler` | effect handler,查找/创建 live session,推进到决策点 | 否 |
| runtime adapter | CLI/SDK 启动、stdin/stdout/JSON 解码、tool bridge、permission bridge | 否 |
| session registry | live handle 生命周期、resume/reattach、cleanup | 否 |
| `ExternalAgentState` | 可持久化 spec/session/artifact/cursor/conversation | 是 |

## 5. external session 协议扩展

当前 `ExternalSessionResult` 三态不足以表达 tool 注入。建议扩展为五类决策点。

### 5.1 `ExternalSessionInput`

现有:

```rust
pub enum ExternalSessionInput {
    Start { prompt: String },
    Continue { message: String },
    RespondInteraction { action_id: String, response: InteractionResponse },
    Shutdown,
}
```

拟新增:

```rust
pub enum ExternalSessionInput {
    Start { prompt: String },
    Continue { message: String },
    RespondInteraction {
        action_id: String,
        response: InteractionResponse,
    },
    RespondToolResults {
        batch_id: ExternalToolBatchId,
        results: Vec<ExternalToolResult>,
    },
    RespondSubagent {
        request_id: ExternalSubagentRequestId,
        output: ExternalSubagentOutput,
    },
    Shutdown,
}
```

说明:

- `RespondToolResults` 把宿主工具执行结果回灌给 runtime。
- `RespondSubagent` 只在 runtime adapter 需要显式回写子 agent 结果时使用。若 runtime 的 spawn 请求本身以
  tool bridge 表达,也可以统一走 `RespondToolResults`。
- `output` 采用 serde-friendly 的 `ExternalSubagentOutput`(见 §5.3),而不是 runtime-only 的
  `SubagentOutput`:宿主 subagent 结果需要跨 external session 边界持久化,而 `SubagentOutput` 不带 serde
  derive。`From<SubagentOutput>` 提供从宿主结果到该 DTO 的转换。
- `batch_id` / `request_id` 是 runtime adapter 生成或保留的 correlation handle,不要求 provider-neutral
  全局唯一,但必须在同一 session 内可关联。

### 5.2 `ExternalSessionResult`

现有:

```rust
pub enum ExternalSessionResult {
    Completed { session, output, observations },
    PausedForInteraction { session, action_id, request, observations },
    Failed { session, error, observations },
}
```

拟新增:

```rust
pub enum ExternalSessionResult {
    Completed {
        session: ExternalSessionRef,
        output: ExternalAgentOutput,
        observations: Vec<ExternalObservedEvent>,
    },
    PausedForInteraction {
        session: ExternalSessionRef,
        action_id: String,
        request: Interaction,
        observations: Vec<ExternalObservedEvent>,
    },
    PausedForToolCalls {
        session: ExternalSessionRef,
        batch_id: ExternalToolBatchId,
        calls: Vec<ExternalToolCall>,
        observations: Vec<ExternalObservedEvent>,
    },
    PausedForSubagent {
        session: ExternalSessionRef,
        request: ExternalSubagentRequest,
        observations: Vec<ExternalObservedEvent>,
    },
    Failed {
        session: Option<ExternalSessionRef>,
        error: ExternalAgentError,
        observations: Vec<ExternalObservedEvent>,
    },
}
```

`PausedForSubagent` 携带的 subagent 请求已收敛为嵌套的 `ExternalSubagentRequest`(见 §5.3),
把 `request_id` / `spec_ref` / `brief` / `result_schema` / `raw` 聚成一个可独立 round-trip 的 DTO,
而不是把这些字段平铺进变体。

如果想少加一个 `PausedForSubagent`,也可以让 `spawn_agent` 暴露为普通 external tool call,由
`ExternalAgentMachine` 识别 `SpawnAgentRequest` 并转 `NeedSubagent`。两种设计的取舍:

| 方案 | 优点 | 缺点 |
|---|---|---|
| 专门 `PausedForSubagent` | 语义清晰,不混在 tool failure policy 里 | DTO 多一个变体 |
| `spawn_agent` 作为 tool call | 复用 tool bridge,更接近 collab adapter | machine 需要在 tool phase 特判 scope-deepening |

**已定方案**:M1 采用专门的 `PausedForSubagent` 变体作为原生 subagent event 的规范路径(语义清晰、
不与 tool failure policy 混在一起)。把 runtime 的 `spawn_agent` tool call 特判成子 agent 的 tool-bridge
路径作为补充能力,留到 M3(见 §8.3),二者并存而非互斥。

### 5.3 external tool DTO

拟新增:

```rust
pub struct ExternalToolCall {
    pub provider_call_id: String,
    pub name: String,
    pub input: Value,
    pub raw: Option<Value>,
}

pub struct ExternalToolResult {
    pub provider_call_id: String,
    pub status: ToolStatus,
    pub content: Vec<ContentBlock>,
    pub error: Option<String>,
    pub raw: Option<Value>,
}

pub struct ExternalToolBatchId(String);
pub struct ExternalSubagentRequestId(String);

pub struct ExternalSubagentRequest {
    pub request_id: ExternalSubagentRequestId,
    pub spec_ref: AgentSpecRef,
    pub brief: Interaction,
    pub result_schema: Option<Value>,
    pub raw: Option<Value>,
}

pub struct ExternalSubagentOutput {
    pub summary: String,
    pub raw: Option<Value>,
}
```

映射原则:

- `provider_call_id` 是 runtime 侧 id,用于回灌。
- machine 仍通过 `ToolExecutionIds` 分配 framework `ToolCallId`。
- `NeedTool` 使用现有 `ToolCall { id, name, input }`,其中 `id` 保留 provider call id。
- `ToolResponse.tool_call_id` 必须回答 provider call id。
- `ExternalToolResult.status` / `content` 直接镜像宿主 `ToolResponse`;`error` 只在工具**根本无法执行**
  (`ToolRuntimeError`)时携带稳定诊断,区别于工具**已运行**并返回 `ToolStatus::Error` 内容的情况。构造走
  `ExternalToolResult::from_tool_response` / `from_tool_runtime_error`。
- `ExternalSubagentRequest` 是 `PausedForSubagent` 的嵌套请求 DTO;`ExternalSubagentOutput` 是
  runtime-only `SubagentOutput` 的 serde-friendly 持久化对应物,`RespondSubagent.output` 使用它。
- `raw` 只保留未建模字段,不参与稳定逻辑。

### 5.4 observed event sequence

当前 `ExternalSessionRef.last_event_seq` 存在,但 `ExternalAgentEvent` 本身无 seq。为严格支持流式 replay
与 dedup,建议新增包装:

```rust
pub struct ExternalObservedEvent {
    pub seq: u64,
    pub event: ExternalAgentEvent,
}
```

替代所有 `observations: Vec<ExternalAgentEvent>`。这样:

- live sink 可以按 seq emit。
- machine resume 时只 replay `seq > previously_consumed_seq` 的事件。
- cassette 可以稳定断言 event 顺序。
- resume / duplicated decision point 不会重复通知 UI。

迁移策略:

1. 保留当前 `ExternalAgentEvent` 作为 event payload。
2. 新增 `ExternalObservedEvent`。
3. DTO observations 改为 `Vec<ExternalObservedEvent>`。
4. 提供 helper `ExternalObservedEvent::unsequenced_for_tests` 或 testkit fixture 减少样板。

## 6. `ExternalAgentMachine` 状态机扩展

当前 machine 的路径:

```text
UserMessage
  -> NeedExternalSession(Start/Continue)
  -> Completed | PausedForInteraction | Failed
PausedForInteraction
  -> NeedInteraction
  -> NeedExternalSession(RespondInteraction)
```

目标路径:

```text
UserMessage
  -> NeedExternalSession(Start/Continue)
  -> Completed
   | Failed
   | PausedForInteraction
   | PausedForToolCalls
   | PausedForSubagent

PausedForInteraction
  -> NeedInteraction
  -> NeedExternalSession(RespondInteraction)

PausedForToolCalls
  -> optional NeedInteraction for tool approval
  -> NeedTool batch
  -> NeedExternalSession(RespondToolResults)

PausedForSubagent
  -> NeedSubagent
  -> NeedExternalSession(RespondSubagent)
```

### 6.1 新增 cursor

`ExternalAgentCursor` 当前包含 `Idle` / `AwaitingSession` / `AwaitingInteraction` / `Done` / `Error` 等。
拟新增:

```rust
pub enum ExternalAgentCursor {
    Idle,
    AwaitingSession { requirement: CursorRequirement },
    AwaitingInteraction { requirement: CursorRequirement, pending_action: String },
    AwaitingTool {
        requirements: ToolWaitRequirements,
        batch_id: ExternalToolBatchId,
    },
    AwaitingToolApproval {
        requirement: CursorRequirement,
        batch_id: ExternalToolBatchId,
        provider_call_id: String,
    },
    AwaitingSubagent {
        requirement: CursorRequirement,
        request_id: ExternalSubagentRequestId,
    },
    Done,
    Error { message: String },
}
```

实现时可以选择复用现有 `LoopCursor::AwaitingTool` / `AwaitingApproval` 作为 driver-facing cursor view,
但 serializable external cursor 需要记录 external-specific correlation id。

### 6.2 新增 scratch

machine 需要类似 `DefaultAgentMachine::InFlight` 的 tool phase scratch:

```rust
struct ExternalInFlight {
    step_id: StepId,
    assistant_message_id: MessageId,
    session_step_count: u32,
    tool_round_count: u32,
    interaction_round_count: u32,
    pending_tool_batch: Option<PendingExternalToolBatch>,
    pending_subagent: Option<PendingExternalSubagent>,
}

struct PendingExternalToolBatch {
    batch_id: ExternalToolBatchId,
    calls: Vec<ExternalToolCall>,
    mappings: BTreeMap<ToolCallId, String>, // framework id -> provider_call_id
    results: BTreeMap<String, ExternalToolResult>,
}
```

这些 scratch 不进入 serde;cursor 和 pending Conversation 保留恢复所需的可持久化事实。跨进程 restore
由 handler 根据 `ExternalSessionRef` 重新注册 pending decision point。

### 6.3 loop limits

为避免外部 runtime 无限 tool loop,需要外部专用或复用内部 `LoopPolicy`:

| 限制 | 触发 |
|---|---|
| `max_session_steps` | `NeedExternalSession` 次数 |
| `max_tool_rounds` | `PausedForToolCalls` 次数 |
| `max_parallel_tools` | 单批 tool call 个数 |
| `max_interaction_rounds` | permission/question 循环次数 |
| `max_wall_time` | handler/session registry 层 |

超限映射到 `ExternalAgentError::LimitExceeded` 或 machine error cursor。

### 6.4 pivot

当前 `ExternalAgentMachine` 拒绝 pivot。要与内部 machine parity,需要设计:

```text
StepInput::External(AgentInput::Pivot)
  -> if awaiting session/tool/interaction:
       append user pivot into Conversation pending context
       NeedExternalSession(Continue { message: pivot_text }) 或 runtime interrupt
  -> if idle/done:
       reject or treat as new Continue
```

不同 runtime 能力差异较大。建议首版:

- capability `supports_mid_turn_interrupt` 为 false 时,继续拒绝 pivot。
- 支持时,将 pivot 转成 `ExternalSessionInput::Continue` 或专用 `Interrupt`。
- 文档明确不支持 runtime 的 fallback。

## 7. `ExternalAgentMachine` 配置与 policy 注入

内部 `DefaultAgentMachine` 持有:

- `RequirementIds`
- `ToolExecutionIds`
- `ToolApprovalPolicy`
- `ToolRegistryResolver`
- `LlmStepMode`
- `LoopPolicy`

external 也需要对应能力:

```rust
pub struct ExternalAgentMachineConfig {
    pub requirement_ids: Arc<dyn RequirementIds>,
    pub tool_execution_ids: Arc<dyn ToolExecutionIds>,
    pub approval_policy: Arc<dyn ToolApprovalPolicy>,
    pub tool_failure_policy: ToolFailurePolicy,
    pub tool_registry_resolver: Arc<dyn ToolRegistryResolver>,
    pub loop_policy: ExternalLoopPolicy,
}
```

可用 builder 接口避免破坏现有构造:

```rust
ExternalAgentMachine::new(state, requirement_ids)
    .with_tool_execution_ids(ids)
    .with_approval_policy(policy)
    .with_tool_failure_policy(ToolFailurePolicy::ReturnErrorToModel)
    .with_loop_policy(policy)
```

若 `ExternalAgentMachine` 没有 `ToolExecutionIds`,但 runtime 发起 tool call,应进入 classified error,不能
静默丢弃。

## 8. tool 注入设计

### 8.1 tool source

`ExternalSessionRequest.tools` 应来自 `ExternalAgentState.active_tools()`。这包括:

- host 注册工具。
- `agent::collab::bridge_tool_declarations()`:
  - `plan_*`
  - `blackboard_*`
  - `send_message`
  - `report_artifact`
  - `run_host_tool`
  - `spawn_agent`
- runtime-specific synthetic tools,例如 shell/edit bridge,如果选择全托管。

### 8.2 runtime tool bridge

每个 runtime adapter 负责把 provider/tool protocol 映射到 `ExternalToolCall`:

```text
runtime frame:
  "call tool X with args Y"

adapter:
  ExternalSessionResult::PausedForToolCalls {
      batch_id,
      calls: [ExternalToolCall { provider_call_id, name, input, raw }],
      observations,
  }
```

machine:

1. 为每个 call 分配 framework `ToolCallId`。
2. 若 `ToolApprovalPolicy` 要求审批,先 emit `NeedInteraction(Approval)`。
3. emit `NeedTool` batch。
4. 收齐 `RequirementResult::Tool` 后构造 `ExternalToolResult`。
5. emit `NeedExternalSession(RespondToolResults { batch_id, results })`。

### 8.3 `spawn_agent` 特判

`spawn_agent` 是 scope-deepening operation,不能当普通 inline tool 执行。处理方式:

```text
ExternalToolCall(name = "spawn_agent")
  -> SpawnAgentRequest::parse
  -> NeedSubagent { spec_ref, brief, result_schema }
  -> SubagentOutput
  -> ExternalToolResult(status=Ok, content=summary)
  -> RespondToolResults
```

这样 external runtime 看到的是“工具调用返回了子 agent summary”,宿主看到的是标准 `NeedSubagent`
路径,depth/cancel/budget/trace 都不绕过。

### 8.4 tool failure policy

`ToolFailurePolicy::ReturnErrorToModel`:

- tool handler 返回 `Err(ToolRuntimeError)` 时,构造 `ExternalToolResult { status: Error, content: error_text }`
  回灌给 runtime。

更严格策略可直接 fail turn:

- `ToolFailurePolicy::FailTurn` 或类似策略:machine 进入 Error cursor,取消 pending Conversation。

需要保证:

- unknown tool。
- malformed args。
- id unavailable。
- approval deny。
- tool cancelled。

都能转成稳定行为。

## 9. user interaction 设计

### 9.1 permission request

外部 runtime 权限事件映射到:

```rust
pub struct PermissionRequest {
    action_id: String,
    agent_id: AgentId,
    category: PermissionCategory,
    summary: String,
    payload: Value,
    risk: PermissionRisk,
    reason: Option<String>,
}
```

分类建议:

| runtime 行为 | `PermissionCategory` |
|---|---|
| shell command | `Shell` |
| file edit / patch apply | `Edit` |
| network | `Network` |
| MCP / external tool | `Mcp` 或 `Tool` |
| spawn subagent / background agent | `Subagent` |
| unknown privileged action | `Other` |

`ExternalPermissionMode` 映射:

| mode | handler 行为 |
|---|---|
| `Prompt` | 所有 gated action 转 `PausedForInteraction` |
| `AcceptEdits` | worktree 内 edit 可自动 approve,其它 prompt |
| `Plan` | 使用 runtime read-only/plan mode;mutating action deny |
| `BypassPermissions` | 只允许显式配置;trace 标记高风险 |

### 9.2 非权限交互

runtime 可能问澄清问题或选择题:

- `InteractionKind::Question { prompt }`
- `InteractionKind::Choice { prompt, options }`

adapter 需要把 runtime 的答案回写协议映射为 `RespondInteraction`。若 runtime 不支持程序化回答,capability
必须标记 unsupported。

### 9.3 interaction pop

external child 通常是 headless scope:

```text
child scope:
  external -> runtime handler
  tool     -> host tools
  interaction absent

parent scope:
  subagent -> DrivingSubagentHandler
  interaction -> UI / approval policy
```

child 发出的 `NeedInteraction` 会 pop 到 parent,与内部 child 完全一致。

## 10. 流式输出设计

### 10.1 两条通道

需要同时支持:

1. **live stream**:handler decoder 读到事件时立刻发给 `ExternalEventSink`。
2. **effect result observations**:handler 到达 decision point 后把同一批事件放进
   `ExternalSessionResult.observations`,machine resume 后转成 `Notification::ExternalAgent`。

```text
runtime stdout/json frames
  -> adapter decode ExternalObservedEvent(seq,event)
  -> if Streaming: sink.emit(&observed_event)   // 同一 seq,与 buffer 对齐
  -> buffer.push(observed_event)
  -> decision point result includes buffer
  -> machine resume emits Notification::ExternalAgent(event)
```

### 10.2 `ExternalStreamPolicy`

| policy | live sink | observations | 用途 |
|---|---:|---:|---|
| `Buffered` | 否 | 是 | 默认测试/driver |
| `Streaming` | 是 | 是 | UI 实时显示 + 可回放 |
| `Disabled` | 否 | 否或仅 terminal | headless 低开销 |

### 10.3 event vocabulary

现有 `ExternalAgentEvent` 已覆盖:

- `SessionStarted`
- `TextDelta`
- `CommandStarted`
- `CommandFinished`
- `FilePatch`
- `PermissionRequested`
- `ToolStarted`
- `ToolFinished`
- `MessageSent`
- `TaskUpdated`
- `SessionCompleted`

拟补充:

- `UsageReported { usage, cost_micros }`
- `ToolCallRequested { name, provider_call_id }`
- `ToolResultSubmitted { provider_call_id, status }`
- `SubagentRequested { spec_ref, summary }`
- `DebugLog { level, message }` 或保持 raw 不公开,只给 handler log。

不建议把 provider raw frame 全量放进 public event。raw frame 可进 cassette/artifact/debug store,默认 redacted。

## 11. runtime adapter 设计

### 11.1 公共 trait

拟新增:

```rust
#[async_trait]
pub trait ExternalRuntimeAdapter: Send + Sync {
    fn kind(&self) -> ExternalRuntimeKind;
    fn capabilities(&self) -> ExternalRuntimeCapabilities;

    async fn start(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError>;

    async fn continue_session(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError>;

    async fn respond_interaction(...);
    async fn respond_tool_results(...);
    async fn shutdown(...);
}
```

实际实现可拆成:

- `ExternalRuntimeFactory`:创建 session。
- `ExternalRuntimeSession`:单个 live session。
- `ExternalSessionRegistry`:按 `ExternalSessionRef` 查找 live session。

`ExternalSessionHandler` 组合这些 trait,兑现 `NeedExternalSession`。

### 11.2 session registry

职责:

- 持有 live process/SDK handles。
- 将 `ExternalSessionRef` 映射到 live session。
- 支持 restore 后 reattach/resume。
- cancel/drop 时清理。
- 记录 shutdown disposition。

建议接口:

```rust
pub trait ExternalSessionRegistry {
    async fn get_or_start(&self, request: &ExternalSessionRequest) -> Result<SessionHandle, ExternalAgentError>;
    async fn cleanup(&self, session: &ExternalSessionRef) -> ExternalSessionShutdown;
    async fn cleanup_agent(&self, agent_id: AgentId) -> ExternalSessionShutdown;
}
```

### 11.3 process handle

CLI adapter 通用需求:

- `tokio::process::Command`。
- `kill_on_drop(true)`。
- stdout/stderr reader task。
- stdin writer 或 runtime-specific control channel。
- bounded event channel。
- cancellation watcher。
- graceful shutdown timeout。
- forced kill fallback。

## 12. Claude Code adapter

### 12.1 启动策略

优先使用结构化流:

```text
claude --print --output-format stream-json --input-format stream-json ...
```

只读/权限映射:

| `ExternalPermissionMode` | Claude 参数 |
|---|---|
| `Prompt` | `--permission-mode manual` 或 runtime 支持的 prompt 模式 |
| `AcceptEdits` | `--permission-mode acceptEdits` |
| `Plan` | `--permission-mode plan` |
| `BypassPermissions` | `--permission-mode bypassPermissions` 且必须显式允许 |

其他建议:

- `--no-session-persistence` 用于 ephemeral 测试。
- production 可使用 session persistence + resume token。
- `--model` 从 runtime config 注入。
- `--max-budget-usd` 可映射 budget。

### 12.2 streaming decoder

Claude stream-json frame 映射:

| Claude frame | `ExternalAgentEvent` / decision |
|---|---|
| assistant text delta | `TextDelta` |
| tool_use bash start | `CommandStarted` 或 `ToolStarted` |
| command result | `CommandFinished` |
| file edit/patch | `FilePatch` |
| permission prompt | `PausedForInteraction` + `PermissionRequested` |
| final result | `Completed` |
| usage/cost | `UsageReported` / output usage |

decoder 必须:

- 容忍未知 frame。
- 保留必要 raw refs 到 debug/cassette。
- 不把 raw private schema 暴露为 stable event。

### 12.3 tool 注入

Claude Code 可优先通过 MCP/custom tools 注入宿主工具:

- 为每个 `Tool` 生成 MCP tool declaration。
- MCP tool handler 不直接执行,而是把 call 转给 `ExternalSessionHandler` decision point。
- `spawn_agent` 使用 host bridge。

如果某 CLI 模式无法 tool bridge:

- capability `tool_bridge = false`。
- `ExternalSessionRequest.tools` 只能作为 prompt hint,不得声明 full parity。

## 13. Codex adapter

### 13.1 启动策略

当前可用基础形态:

```text
codex -s read-only -a never exec --json -C <worktree> ...
```

注意 Codex 的部分参数是全局参数,必须放在 `exec` 前,例如 `-s read-only -a never`。

权限映射:

| `ExternalPermissionMode` | Codex 参数 |
|---|---|
| `Prompt` | `-a on-request` 或 runtime 支持的 approval mode |
| `AcceptEdits` | `-a on-request` + workspace-write,仍需宿主 policy |
| `Plan` | `-s read-only -a never` |
| `BypassPermissions` | `--dangerously-bypass-approvals-and-sandbox`,必须显式允许 |

### 13.2 streaming decoder

优先 `codex exec --json` JSONL:

| Codex JSONL event | `ExternalAgentEvent` / decision |
|---|---|
| assistant message delta | `TextDelta` |
| exec command begin/end | `CommandStarted` / `CommandFinished` |
| patch/apply event | `FilePatch` |
| approval request | `PausedForInteraction` |
| final message | `Completed` |

fallback:

- 无 JSONL 或不稳定时,plain stdout 只映射 `TextDelta` + final `Completed`。
- fallback 必须标记 `structured_stream = false`。

### 13.3 tool bridge

Codex 的 full tool injection 取决于当前 CLI/exec-server/MCP 能力:

- 如果 MCP/custom tool 可用,走与 Claude 相同的 bridge。
- 如果不可用,只支持半托管:
  - Codex 自己执行 read/search/shell。
  - host 只能观察 event 和 permission。
  - `ExternalSessionRequest.tools` 不暴露或仅作为 prompt contract。
- 对 unsupported tool bridge 的任务,dispatcher 应避免派给 Codex 或升级到支持 runtime。

## 14. OpenCode adapter

OpenCode 需要先做 capability probe,因为部署形态可能更多。

### 14.1 probe 项

- CLI 命令名和版本。
- 是否支持 JSON/JSONL stream。
- 是否支持 session resume。
- 是否支持 MCP/custom tools。
- 是否支持 permission hook。
- 是否支持 read-only / sandbox / plan mode。
- 是否输出 patch/command/test events。

### 14.2 adapter 策略

首版实现两层:

1. **黑盒/半托管基础模式**
   - 启动 OpenCode。
   - decode text stream。
   - final summary -> `Completed`。
   - command/patch 若可解析则结构化。

2. **managed mode**
   - tool bridge。
   - permission bridge。
   - session resume。
   - artifact store。

若缺少 managed mode 能力,capability 里标明,调度层避免选择它执行需要 host tools 的任务。

## 15. capability model

拟新增:

```rust
pub struct ExternalRuntimeCapabilities {
    pub runtime: ExternalRuntimeKind,
    pub structured_stream: bool,
    pub text_delta: bool,
    pub command_events: bool,
    pub patch_events: bool,
    pub permission_pause: bool,
    pub question_pause: bool,
    pub tool_bridge: bool,
    pub subagent_bridge: bool,
    pub session_resume: bool,
    pub usage_reporting: bool,
    pub cost_reporting: bool,
    pub worktree_sandbox: bool,
}
```

使用场景:

- adapter 启动前探测。
- dispatcher 做 worker selection。
- machine 遇到 unsupported decision point 时给出 classified error。
- docs/capability-matrix.md 记录实测值。

建议新增错误:

```rust
ExternalAgentError::UnsupportedCapability {
    runtime: ExternalRuntimeKind,
    capability: String,
    detail: String,
}
```

## 16. worktree isolation

`WorktreeIsolation` 当前只是 data。managed runtime 必须真正执行:

| isolation | 行为 |
|---|---|
| `Shared` | 直接在指定 worktree 运行 |
| `PerAgentWorktree` | 每个 agent 固定一个独立 worktree |
| `EphemeralGitWorktree` | 每次 session 创建临时 git worktree,结束后清理 |

拟新增 `WorktreeManager`:

```rust
pub trait WorktreeManager {
    fn prepare(&self, agent_id: AgentId, isolation: WorktreeIsolation) -> Result<PreparedWorktree, AgentError>;
    fn collect_artifacts(&self, worktree: &PreparedWorktree) -> Vec<ExternalArtifactRef>;
    fn cleanup(&self, worktree: PreparedWorktree, shutdown: ExternalSessionShutdown) -> Result<(), AgentError>;
}
```

forced kill / shutdown failed 后,worktree 应标记为 residual side effects,不能自动复用。

## 17. budget / usage / cost

外部 runtime usage 来源:

- CLI/SDK 明确报告 token/cost。
- transcript metadata。
- 无法报告时保持 `None`。

原则:

- 不用词数估算冒充真实 token。
- 有 usage 时由 handler/driver charge `RunContext`。
- child external agent 的 usage 必须进入 parent shared budget ledger。
- budget exhausted 应在启动前或 decision point 前检查。

拟新增 helper:

```rust
pub struct ExternalUsageChargingHandler<H> {
    inner: H,
}
```

或在 runtime handler 内显式:

```rust
if let Some(usage) = output.usage {
    ctx.charge_tokens(usage.total_computed().into())?;
}
if let Some(cost) = output.cost_micros {
    ctx.charge_cost_micros(cost)?;
}
```

## 18. artifacts

artifact 目标:

- patch/diff/test log/file refs 进入 `ExternalAgentOutput.artifacts`。
- full diff/log/blob 不内联进 state。
- artifact store 负责保存内容并返回 reference。

流程:

```text
runtime patch frame
  -> ExternalAgentEvent::FilePatch { path, summary, diff_ref }
  -> ArtifactStore stores full diff
  -> ExternalArtifactRef { kind: Patch, path, reference }
  -> ExternalAgentState.record_artifacts
```

需要补:

- `ArtifactStore` trait。
- cassette redaction。
- trace artifact node 或在 external session node 上挂 metadata。

## 19. reconfiguration

内部 machine 有 turn-boundary reconfig。external 支持分两级:

1. **boundary toolset reconfig**
   - 下一次 `NeedExternalSession(Start/Continue)` 使用新 tools。
   - 如果 runtime session 已启动但不能动态改 tools,handler 可重启/新建 session。

2. **live tool bridge reconfig**
   - runtime 支持 MCP/tool refresh 时,handler 发 runtime-specific reconfigure。
   - 不支持时返回 `UnsupportedCapability`。

首版建议只做 boundary reconfig,并要求 runtime capability 明确。

## 20. 测试策略

### 20.1 默认离线测试

- DTO serde round-trip。
- requirement result alignment。
- `ExternalAgentMachine` scripted state transitions:
  - Start -> Completed。
  - Start -> PausedForInteraction -> RespondInteraction -> Completed。
  - Start -> PausedForToolCalls -> NeedTool -> RespondToolResults -> Completed。
  - tool approval approve/deny/cancel。
  - tool batch parallel。
  - spawn_agent -> NeedSubagent -> RespondToolResults。
  - cancel before/while session/tool/interaction。
  - step limit / tool round limit。
- adapter parser fixtures:
  - Claude stream-json。
  - Codex JSONL。
  - OpenCode JSON/JSONL。
- cassette replay:
  - text stream。
  - permission pause。
  - tool bridge。
  - patch artifact。

### 20.2 ignored real e2e

真实测试必须显式运行:

```text
cargo test --test agent_external_real_e2e -- --ignored --nocapture
```

覆盖:

- Claude Code basic streaming。
- Claude Code permission。
- Claude Code tool bridge。
- Codex basic streaming。
- Codex tool bridge,如果 capability 支持。
- OpenCode basic streaming。
- mixed session: cheap/DeepSeek coordinator -> Claude Code + Codex + OpenCode subagents。

缺少 binary/auth/env 时应清晰 skip,不 panic。

## 21. 落地里程碑

> **编号说明**:本节是设计初稿的里程碑拆分,与实际执行的里程碑编号(见
> [`PLAN.md`](../PLAN.md) / [`TODO.md`](../TODO.md))并非一一对应。执行侧已落地:sequenced
> observations(执行 M1)、`ExternalAgentMachine` tool parity(执行 M2)、subagent / interaction
> parity(执行 M3,含 `spawn_agent` tool-bridge 特判)。下列条目保留设计意图,并就实现差异就地标注。

### M1: external tool/subagent 协议

- 新增 `ExternalToolCall` / `ExternalToolResult` / batch id。
- 扩展 `ExternalSessionInput::RespondToolResults`。
- 扩展 `ExternalSessionResult::PausedForToolCalls`。
- `spawn_agent`:已定采用专门 `PausedForSubagent` + 嵌套 `ExternalSubagentRequest` /
  `ExternalSubagentOutput`;tool-bridge 特判留给 M3(§8.3)。
- 更新 serde / requirement alignment / testkit fixtures。

### M2: `ExternalAgentMachine` tool parity

- 新增 `AwaitingTool` cursor。**实现差异**:未引入独立的 `AwaitingToolApproval` cursor —
  runtime permission / host tool approval 复用现有 interaction 相位(`NeedInteraction` /
  `AwaitingInteraction`),不单独建 cursor。
- 注入 `ToolExecutionIds` 分配 framework `ToolCallId`。**实现差异**:未引入 `ToolApprovalPolicy` /
  `ToolFailurePolicy` 类型 — tool-failure 采用固定的 *return-error-to-runtime* 策略(`Tool(Err)` 回灌为
  失败 `ExternalToolResult`,不停 turn),approval 由 interaction 相位承载。
- 实现 external tool phase(`PausedForToolCalls` -> `NeedTool` batch -> `RespondToolResults`,按
  runtime 原始 call 顺序回灌,支持乱序 resume)。
- 支持 `spawn_agent` -> `NeedSubagent`(tool-bridge 特判,§8.3;实际落在执行侧 M3-3,混合 batch 里
  普通 tool 与 spawn_agent 共存并折回同一 `RespondToolResults`)。
- 补 cancel / failure / limit 测试。

### M3: observed event sequence + streaming sink

- 新增 `ExternalObservedEvent { seq, event }`。**已落地**(执行 M1-1)。
- observations 改为 sequenced。**已落地**(执行 M1-1)。
- `ExternalEventSink` 升级为按 `seq` emit `ExternalObservedEvent` 的 sequenced live sink。**已落地**(执行 M4-1)。
- `ExternalStreamPolicy::Streaming` 策略选择与 runtime 接线。**待实现**(后续里程碑)。
- replay dedup。**已落地**:`observe` 按 `ExternalObservedEvent::seq` 对 `ExternalSessionRef::last_event_seq`
  逐事件去重。
- parser/testkit 更新。

### M4: runtime adapter abstraction + session registry

- 定义 `ExternalRuntimeAdapter` / `ExternalRuntimeSession` / `ExternalSessionRegistry`。
- 实现 process handle cleanup。
- 实现 resume/reattach 基础。
- 实现 shutdown trace。

### M5: Claude Code managed adapter

- stream-json decoder。
- permission bridge。
- tool bridge。
- artifact/usage extraction。
- cassette + ignored e2e。

### M6: Codex managed adapter

- JSONL decoder。
- read-only/approval mapping。
- capability-gated tool bridge。
- cassette + ignored e2e。

### M7: OpenCode managed adapter

- capability probe。
- streaming decoder。
- permission/tool bridge 能力接入。
- cassette + ignored e2e。

### M8: worktree/budget/reconfig hardening

- `WorktreeManager`。
- usage/cost charging。
- boundary reconfig。
- artifact store。
- residual side-effect policy。

### M9: docs/examples/capability matrix

- 更新 [`capability-matrix.md`](./capability-matrix.md)。
- examples:
  - Claude Code managed。
  - Codex managed。
  - OpenCode managed。
  - mixed external agents。
- 安全说明:权限、worktree、secret redaction、ignored tests。

## 22. 关键风险

1. **runtime private protocol drift**
   CLI JSON/JSONL schema 可能变。缓解:adapter 私有 parser + cassette + capability probe,不把 raw schema
   暴露为 public API。

2. **tool bridge 能力不对称**
   Claude Code、Codex、OpenCode 不一定都有等价 custom tool 机制。缓解:capability model + dispatcher
   避免把需要 host tool 的任务派给不支持 runtime。

3. **权限绕过**
   外部 runtime 可能自己执行 shell/edit。缓解:优先 read-only/plan/permission hook;mutating mode 必须受
   `ExternalPermissionMode` 与 worktree isolation 控制。

4. **cancel 后副作用残留**
   forced kill 不能回滚已执行命令。缓解:shutdown disposition + residual side-effect marker + ephemeral worktree。

5. **stream replay 重复**
   live sink 和 observations 双通道容易重复。缓解:`ExternalObservedEvent.seq` + `last_event_seq`。

6. **测试不稳定**
   真实 CLI/API 慢且依赖登录态。缓解:默认 scripted/cassette;真实 e2e ignored;skip 条件明确。

## 23. 建议的第一步

先不要直接写 Claude/Codex/OpenCode 生产 adapter。第一步应先补协议和 machine:

1. `ExternalSessionResult::PausedForToolCalls`。
2. `ExternalSessionInput::RespondToolResults`。
3. `ExternalAgentMachine` external tool phase。
4. `spawn_agent` tool call 转 `NeedSubagent`。
5. scripted tests 覆盖 tool / interaction / subagent 组合。

这一步完成后,真实 runtime adapter 只是“把 runtime frame decode 成这些 decision point”。否则直接写 CLI
封装只会得到更复杂的黑盒 handler,无法达到 managed parity。
