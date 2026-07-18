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
| runtime adapter 抽象(`ExternalRuntimeAdapter` / `ExternalRuntimeSession`) | 已实现(离线 scripted/cassette 替身;真实 CLI adapter 待 M6-M8) |
| 真实 Claude Code / Codex / OpenCode runtime adapter | 未实现(待 M6-M8) |
| external runtime 发起 host tool call | machine + scripted runtime handler 已实现(真实 adapter 待实现) |
| external runtime 发起 host subagent | machine + scripted runtime handler 已实现(真实 adapter 待实现) |
| 长生命周期 session registry / process handles | `ExternalSessionRegistry`(start/reattach/resume/cleanup)已实现;真实 process handle 待 M6-M8 |
| structured streaming live sink + replay sequence | 已实现(sink 已 sequenced + buffered replay dedup;`ExternalStreamPolicy` 选择/真实 runtime 接线待实现) |
| cassette replay 真实 external session | runtime-parser cassette 回放层已实现(synthetic fixtures);真实 Claude/Codex/OpenCode cassette 待 M6-M8 |

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

> **落地状态（截至 M9-5）**:下表「备注」列记录 *as-built* 状态而非目标。managed 路径已整链
> 打通——`ExternalAgentMachine`(sans-io)+ registry-backed `ExternalSessionHandler`(M5)+ 三个
> feature-gated runtime adapter(Claude Code M6 / Codex M7 / OpenCode M8)——离线由
> scripted/cassette handler 验证,真实 CLI 由 `#[ignore]` e2e 覆盖。可运行的 scoped-effect wiring
> 见 `examples/managed_claude_code.rs`、`examples/managed_codex.rs`、`examples/managed_opencode.rs`、
> `examples/managed_mixed.rs`(共享装配 `examples/support/managed.rs`)。**真实能力仍以 ignored e2e
> 跑绿为准**;能力矩阵的保守默认与门控见 [`capability-matrix.md`](./capability-matrix.md)。

| 内部 agent 能力 | `DefaultAgentMachine` | Managed external agent | 备注 |
|---|---|---|---|
| 文本 turn | `NeedLlm` -> assistant `Response` | `NeedExternalSession` -> `Completed.output` | 已落地:machine 折 `RuntimeDecisionPoint::Completed.output` 进 Conversation(M2/M5),三 adapter 解结构化流 |
| 多轮会话 | Conversation + model history | `ExternalSessionRef` + runtime resume/continue | 已落地:registry `get_or_start` 首轮 start、后续 reattach;跨进程 `resume` 按 adapter capability 门控(runtime-dependent) |
| 流式文本 | `StreamEvent` / `Notification::Llm` | `ExternalAgentEvent::TextDelta` / live sink | 已落地:`ExternalEventSink` sequenced(M4-1),三 adapter 把解码帧镜像到 live sink;handler 经 `get_or_start(.., Some(sink))` 接线(M6–M8) |
| tool call | `ContentBlock::ToolUse` -> `NeedTool` | runtime tool call -> `NeedTool` -> `RespondToolResults` | 已落地:machine external tool phase(M2)+ registry-backed handler(M5)。**host custom-tool 注入**仍 capability-gated 关(`host_tools=false`,§12.3),对声明工具的请求以 `UnsupportedCapability{HostTools}` 拒绝 |
| tool approval | `NeedInteraction(Approval)` | runtime permission 或 host tool approval -> `NeedInteraction` | 已落地:runtime permission pause(`PausedForInteraction`)→ `NeedInteraction`,由 scope interaction handler 就地批准(M3;real e2e M9-4;examples 用 `approve_all`) |
| user question | `NeedInteraction(Question)` | runtime question -> `NeedInteraction(Question)` | 已落地:runtime question/choice → `NeedInteraction`(M3-2) |
| subagent | `NeedSubagent` | runtime spawn request -> `NeedSubagent` | 已落地:`PausedForSubagent`→`NeedSubagent`(M3-1)+ `spawn_agent` tool-bridge 特判(M3-3);real DeepSeek 协调器派生 Claude/Codex child(M9-4) |
| tool failure policy | `ToolFailurePolicy` | external tool result error 回灌或 fail turn | 已落地(M4-3):`ExternalToolFailurePolicy`(`ReturnErrorToRuntime` 默认 / `StopRun`) |
| cancel | `StepInput::Abandon` closes pending | abandon marks cleanup + handler kills session | 已落地:registry `cleanup`/`cleanup_agent` 强制关 live session 并返回 `ExternalSessionShutdown` disposition(M5);force-close 为进程组级 SIGTERM→SIGKILL(M2-1,unix),adapter `kill_on_drop` 兜底 |
| budget | handler/driver charge tokens/cost | runtime usage/cost event charge | 已落地(M9-2):`ExternalUsageChargingHandler` 把 runtime usage/cost 计入 run budget,见 §17 |
| trace | requirement + tool + subagent nodes | external events + shutdown + artifacts | 部分已有:requirement/subagent/shutdown 节点已接;artifact 追踪见下 |
| artifact | tool/model output | patch/diff/test/file artifact refs | 部分已有:见 §18 |
| worktree isolation | `WorktreeRef` | shared / per-agent / ephemeral worktree manager | 已落地(M9-1):`WorktreeManager`/`GitWorktreeManager`(prepare/cleanup + residual 标记),见 §16 |
| reconfig | queued tool set swap | boundary-level tool bridge reconfigure | 已落地(M9-3):`ExternalAgentMachine::reconfigure` + `ExternalReconfigTiming`/`ExternalReconfigOutcome`(boundary 应用/排队;in-flight `Hot`→`UnsupportedCapability{Reconfigure}`),见 §19 |
| snapshot/restore | `AgentState` + Conversation snapshot | `ExternalAgentState` + `ExternalSessionRef` resume | 已落地:`ExternalAgentState` 持久化 spec/session/cursor/conversation,registry 依 `ExternalSessionRef` reattach/`resume`(capability-gated),未知不可 resume 的 session 以 `ResumeUnavailable` 显式失败 |

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

> **实现注记（M2-2 / review M-EXT-1）**：seq 线必须**跨进程**连续。四个 adapter（claude_code /
> codex / opencode / acp）的 `resume` 会用持久化 `ExternalSessionRef.last_event_seq` 播种新 session：
> decoder 经 `with_next_seq(high_water + 1)` 从旧水位之后继续编号，session 自身的 `last_event_seq`
> 也恢复为持久化值（`session_ref()` 永不回退水位）。若 seq 从 0 重启，machine 的
> `seq > consumed` dedup 会把恢复后的全部观测误判为重复而静默丢弃。

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

超限映射到 `ExternalAgentError::LimitExceeded` 或 machine error cursor。已落地(M4-3):
`ExternalAgentMachineConfig::max_decision_loops` 提供一个统一的 session round-trip 上限(`max_session_steps`
的粗粒度实现),计数持久化在 `ExternalAgentState::decision_loops`,超限时在铸下一个 `NeedExternalSession`
之前以 `LimitExceeded` 失败;更细的 per-phase 上限(tool rounds / parallel tools / interaction rounds /
wall time)仍待后续里程碑补齐。

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

external 也需要对应能力。已落地(M4-3)的做法把配置分成两半:

- **runtime-facing hints** = `ExternalSessionPolicy`(permission/isolation/max_turns/stream_events),
  随每个 `ExternalSessionRequest` 传给 handler / runtime。
- **machine-local policy** = `ExternalAgentMachineConfig`(纯数据 serde DTO,不进 `ExternalAgentState`,
  也不持有 live handler/sink/id source):

```rust
pub struct ExternalAgentMachineConfig {
    tool_failure: ExternalToolFailurePolicy,        // ReturnErrorToRuntime(默认) / StopRun
    required_capabilities: BTreeSet<ExternalCapability>, // 覆盖 require host tools / require subagents 等
    max_decision_loops: Option<u32>,               // 运行期 session round-trip 上限,None=不限
}
```

live 的 `RequirementIds` / `ToolExecutionIds` 仍走各自的构造/builder 注入,不放进上面的 DTO(避免把
不可序列化的句柄混进 machine-local policy)。builder 接口保持向后兼容:

```rust
ExternalAgentMachine::new(state, requirement_ids)   // 默认 config = 与旧行为一致
    .with_tool_execution_ids(ids)
    .with_external_config(
        ExternalAgentMachineConfig::default()
            .with_tool_failure_policy(ExternalToolFailurePolicy::StopRun)
            .with_max_decision_loops(Some(32))
            .require_host_tools()
            .require_subagents(),
    )
    // 也可用聚焦 setter:
    .with_tool_failure_policy(ExternalToolFailurePolicy::StopRun)
    .with_max_decision_loops(Some(32))
```

行为约定:

- **loop limit**:每次把控制权交回 runtime(初始 `Start`/`Continue`,以及每个 `RespondToolResults` /
  `RespondInteraction` / `RespondSubagent`)都记一次 decision loop,计数持久化在 `ExternalAgentState`
  (跨 restore 存活),超过 `max_decision_loops` 时以 `ExternalAgentError::LimitExceeded` 失败,在再铸一个
  `NeedExternalSession` 之前挡住无界 pause loop。
- **tool failure policy**:bridged host tool 返回 `Err` 时,`ReturnErrorToRuntime`(默认)把失败作为
  `ExternalToolResult{status: Error}` 回灌 runtime;`StopRun` 直接 fail turn(Error cursor,丢弃 pending turn)。
- **required capabilities**:声明本次 run 依赖的 managed feature(capability set)。当到达需要该能力的决策点
  但宿主无法服务时(例如声明 `require_host_tools` / `require_subagents` 却没有注入 `ToolExecutionIds`,
  无法为 runtime 发起的 tool / spawn_agent 调用铸造 tool-call id),以 classified
  `ExternalAgentError::UnsupportedCapability` 失败而非静默降级或吐通用错误。

若 `ExternalAgentMachine` 没有 `ToolExecutionIds`,但 runtime 发起 tool call,始终进入 classified error,不能
静默丢弃;声明了对应 capability requirement 时错误会升级为 `UnsupportedCapability`。

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

### 11.4 实现状态（M5，已落地）

Milestone 5（TODO `M5-1`..`M5-3`）冻结了 runtime abstraction 边界。真实
Claude/Codex/OpenCode adapter（M6-M8）只需在 adapter 层填 parser + process 管理，
**不需要改 `ExternalAgentMachine` / driver**。已落地的形状与上面草案略有出入，以实际代码为准：

**已落地的 trait / 类型**（`src/agent/external/adapter.rs`、`registry.rs`）：

- `ExternalRuntimeAdapter`（per-runtime 工厂，`Send + Sync`）：
  - `fn kind(&self) -> ExternalRuntimeKind`
  - `fn capabilities(&self) -> ExternalRuntimeCapabilities`（保守基线 `none()`，probe 确认后逐项开启）
  - `async fn start(&self, request, ctx, sink) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError>`
  - `async fn resume(&self, session, request, ctx, sink) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError>`
    （默认返回 `ResumeUnavailable`；仅 `capabilities().resume` 的 adapter override）
- `ExternalRuntimeSession`（单个 live session，`Send`）：
  - `fn session_ref(&self) -> ExternalSessionRef`（start/resume 后必须带 `session_id` 供 registry keying）
  - `async fn advance(&mut self, input: &ExternalSessionInput, ctx) -> Result<RuntimeDecisionPoint, ExternalAgentError>`
    （驱动到**下一个** decision point，禁止一次阻塞跑到底）
  - `async fn shutdown(&mut self) -> ExternalSessionShutdown`
- `ExternalSessionRegistry`（**具体 struct**，非 trait；不进 `ExternalAgentState`）：
  - `get_or_start(request, ctx, sink)`：`session=None` → `start`；已注册 → reattach live handle；未注册且 `resume`
    支持 → `resume`；否则 `ResumeUnavailable`。
  - `cleanup(agent_id, session)` / `cleanup_agent(agent_id)`：cancel/drop 清扫，返回 `ExternalSessionShutdown`。
  - `get` / `live_len` / `kind` / `capabilities`。
- `RuntimeDecisionPoint`（`advance` 的成功返回）五路：`Completed` / `PausedForInteraction` /
  `PausedForToolCalls` / `PausedForSubagent`，外加 `Err` → machine 折成 `ExternalSessionResult::Failed`。每路都带
  buffered `observations`（machine 按 `seq` dedup 后转 `Notification::ExternalAgent`）。

**真实 adapter 必须实现的错误映射**（`ExternalAgentError` 分类，禁止 ad-hoc error / panic）：

- `Launch { runtime, detail }`：CLI/SDK 启动失败。
- `Protocol { .. }`：wire schema 漂移 / 缺 `session_id` 等协议违例。
- `SessionLost { session, .. }`：live session 中途丢失。
- `ResumeUnavailable { session, detail }`：无 live handle 且不支持 resume。
- `ShutdownFailed { session, .. }`：关闭失败。
- `LimitExceeded { limit }`：max_turns / budget 触顶。
- `UnsupportedCapability { .. }`：请求了 capability 未开启的功能。
- `Runtime { code, message }`：其他运行期错误兜底。

**生产 handler 组合**：production `ExternalSessionHandler` 只组合 `registry + adapter`，
**不持有 machine 状态**（每次 `fulfill` 走 `registry.get_or_start` → `session.advance` → 折成
`RequirementResult::ExternalSession`）。离线替身 `ScriptedRuntimeExternalSessionHandler`
（`crates/agent-testkit/src/external/runtime.rs`）与 cassette `CassetteRuntimeExternalSessionHandler`
（`.../external/cassette.rs`）都是这个形状，驱动整条 managed loop（start / tool batch / interaction /
subagent / mixed tool+subagent）离线跑通，证明边界无需改 machine。

**runtime-parser cassette 层**（`.../external/cassette.rs`，schema version 1）：冻结「原始帧 → sequenced
observations + decision point」，未知字段 `#[serde(flatten)]` 保守保留可 round-trip；`scan_secrets` /
`assert_no_secrets` + `external_cassette_fixtures_are_redacted` 保证 fixture 脱敏（`API_KEY` / `AUTH_TOKEN`
/ `sk-` / `-----BEGIN` 等）。synthetic fixtures 在 `tests/fixtures/external/synthetic/`；
`tests/fixtures/external/{claude_code,codex,opencode}/` 目录已预留，真实录制待 M6-M8。

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

#### 实现状态（M6-1，已落地）

`external-claude-code` feature 下已落地**启动配置 + capability probe**（尚无 stream decoder /
live session，见 M6-2/M6-3）：

- `ClaudeCodeConfig`（`src/agent/external/claude_code/config.rs`）：binary path / env override /
  working dir(worktree) / permission mode / optional model / profile / 三个独立超时的纯数据配置
  （`timeout` 只管 probe/launch；`read_idle_timeout` 默认 10 min，是每行 stdout 空闲上限；
  `shutdown_grace` 默认 30s，是优雅关闭的等待上限——见下文「三类超时」）。serde
  round-trip（新增超时字段带 serde default，旧配置可反序列化）；手写 `Debug` 脱敏 env 值
  （只印 key + `<redacted>`）。`permission_mode_arg()` 映射
  `ExternalPermissionMode` → Claude CLI `--permission-mode` 值（`Prompt→default`、
  `AcceptEdits→acceptEdits`、`Plan→plan`、`BypassPermissions→bypassPermissions`）；`base_session_args()`
  产出 `--print --output-format stream-json --input-format stream-json --permission-mode <m> [--model <m>]`。
- `probe`/`probe_with_exec`（`.../claude_code/probe.rs`）：跑 `--version` + `--help`。缺失/损坏 binary
  或非零退出 → `ExternalAgentError::Launch`；不广告 `stream-json` 结构化流 → `UnsupportedCapability{Streaming}`；
  其余能力从 `--help` 开关**保守探测**（未广告即 `false`），永不 panic。探测走可注入的
  `ClaudeCodeProbeExec`（生产实现 `SystemClaudeCodeExec` 用 `tokio::process`），单测用 fake exec 离线覆盖
  全部错误分类，无需真实 Claude Code，也不泄露 env secret。

**三类超时（M1-5 拆分，三个 CLI adapter 口径一致）**：

- `timeout`（默认 30s）：只管一次性控制操作——capability probe 与 launch 握手。
- `read_idle_timeout`（默认 10 min）：live session 的**每行 stdout 空闲上限**。CLI 跑长静默命令
  （构建/测试套件）数分钟无帧属正常，绝不复用 30s 的 launch 超时，否则长静默 turn 会被误判
  `SessionLost` 杀掉。
- `shutdown_grace`（默认 30s）：close 时丢弃 stdin 发 EOF 后等待 CLI 自行退出的上限，超时后
  进入进程组级强杀（unix 下先向**整个进程组**发 SIGTERM、2s 升级窗口后 SIGKILL，见 §16
  「进程组级 kill」）→ `ForcedKill`。

Claude Code 是单条长驻进程，该空闲上限跨整个 session 逐行生效；Codex/OpenCode 是每 turn 一个
一次性进程（见 §13/§14），同一上限在单个 turn 进程内逐行生效——语义相同，只是作用域是一个
turn 而非整条 session。

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

#### 实现状态（M6-2,已落地）

`external-claude-code` feature 下已落地**私有 `stream-json` decoder**（尚无 live session,见 M6-3）:

- `ClaudeStreamDecoder`（`src/agent/external/claude_code/decoder.rs`）:有状态、跨 turn 单调 `seq` 的
  逐帧解码器。全程走 `serde_json::Value` 防御式导航,**不导出任何 raw frame 类型**,Claude 私有 wire
  schema 不进 `agent-lib` 稳定 API。`push_line` 产出中立的 `ExternalObservedEvent` 观测流,turn 落定时
  返回一个中立 `ClaudeDecision`(`Completed` / `PausedForToolCalls` / `PausedForInteraction` / `Failed`)。
- frame 映射:`system/init`→`SessionStarted`;assistant `text`→`TextDelta`;`tool_use` `Bash`→
  `CommandStarted`,edit/write→`FilePatch`,其它内建→`ToolStarted`,`mcp__*` 宿主桥接工具→折成
  `PausedForToolCalls` 批次;user `tool_result`→`CommandFinished`/`ToolFinished`;`control_request`
  `can_use_tool`→`PermissionRequested` 观测 + `PausedForInteraction`(权限 `Interaction` 绑定宿主
  `step_id`/`actor`,绝不取自模型输出);`result` `success`→`SessionCompleted` + `Completed`(带 usage/cost),
  error 子类型→`Failed`(`error_max_turns`→`LimitExceeded`,其余→`Runtime`)。
- 容忍策略(稳定):空行 / `stream_event` 部分帧 / 未知 `type` / 未知 content block / 未关联的 `tool_result`
  → 容忍(无观测、无错);非法 JSON / 非对象 / 缺字符串 `type` / 已知帧缺必需内层对象 → `ExternalAgentError::Protocol`。
  所有诊断均为固定字符串,永不夹带 prompt/tool input/凭据。
- 回归:committed cassette `tests/fixtures/external/claude_code/full_session.json`(三 turn,覆盖 text /
  command / patch / 宿主 tool / permission / completion)经 `tests/agent_claude_code_cassette.rs` 用同一
  decoder 回放全程,断言观测流与每 turn 决策;另有内联 raw-frame 单测覆盖容忍 / `Protocol` / error-result 分类。
  `assert_no_secrets` 保证 fixture 无凭据。离线,无需真实 Claude Code。

#### 实现状态（M6-3,已落地）

`external-claude-code` feature 下已落地**live session + runtime adapter**,把 M6-1 的启动配方与 M6-2 的
私有 decoder 接进 milestone-5 的 `ExternalRuntimeAdapter` / `ExternalRuntimeSession` 抽象(§11):

- `ClaudeCodeAdapter`(`src/agent/external/claude_code/adapter.rs`,**唯一 pub 类型**):per-runtime 工厂。
  `new(config)` 报告本 adapter 实现的全部能力;`with_probed_capabilities(config, &probed)` 把实现能力与
  probe 实测能力**逐位取交**(缺 `--resume` 即关 resume)。`start` 启动全新 CLI session,`resume` 用
  `--resume <session_id>` 复活既有 session,失败回落到 `ResumeUnavailable`。
- `ClaudeCodeSession`(私有):单条 live session,持有 CLI 子进程与一个跨全程单调 `seq` 的
  `ClaudeStreamDecoder`。**关键时序**:Claude Code 在收到第一条 stdin turn 之前不产出任何 frame(连
  `system/init` 都不发)。因此 `start` 先把首个输入(prompt)写进 stdin,再读 stdout 直到 `system/init`
  帧给出真实 session id 作为 registry key;该 turn 其余帧(text/result)留给第一次 `advance` 续读,故第一次
  `advance`(携带同一 input)**不再重复写入**。`resume` 走 `--resume <id>`,session id 已从持久化 ref 得到,
  `begin` 不做预读,直接由第一次 `advance` 写续跑 turn 并读新的 `init`。`advance(input)` 先把输入写成 stdin 的
  `stream-json` 帧(`Start`/`Continue`→`user` 文本帧;`RespondInteraction`(权限)→`control_response`
  `allow`/`deny` 帧),再逐行读 stdout 喂 decoder、把观测镜像到 live sink,直到 decoder 落定一个
  `RuntimeDecisionPoint`;stdout 提前 EOF→`SessionLost`,非法帧→`Protocol`。`shutdown` 丢弃 stdin 让 CLI
  见 EOF,在 shutdown grace 内等待退出并按退出码分类(0 → `Graceful`,非 0 → `Failed`;超时则
  进程组级强杀 → `ForcedKill`,见 §16「进程组级 kill」)。
- IO 经私有 `ClaudeSessionIo` trait 注入:生产用 `ClaudeProcessIo`(`tokio::process`,stderr 丢弃、
  `kill_on_drop`、每读空闲超时 `read_idle_timeout`),单测注入 fake transport 回放固定帧并捕获 stdin,**离线**跑通
  start/advance/resume/shutdown 全状态机,无需真实 binary、无网络。
- 宿主工具(spec 允许,§12.3):本 adapter **不跑 MCP server**,故 `implemented_capabilities()` 诚实报告
  `host_tools=false` / `host_subagents=false`,并对声明了 `tools` 的 `start`/`resume` 请求以
  `UnsupportedCapability{HostTools}` 明确拒绝,而非静默忽略;`RespondToolResults`/`RespondSubagent`
  同样拒绝。其余能力(streaming/resume/permission_bridge/artifacts/usage/graceful_shutdown)为 true。
  权限暂停对应的 `Interaction` `step_id`/`actor` 绑定宿主 `RunContext.run_id`/请求 `agent_id`,绝不取自
  runtime 输出。
- 真机 e2e:`tests/external_claude_code.rs` 有一个 `#[ignore]` 用例,通过 `CLAUDE_CODE_BIN` 或 PATH 发现
  `claude`,缺失 binary/登录即带清晰信息**跳过**(退出为绿),否则在临时 git worktree 里用
  `ClaudeCodeAdapter` + `ExternalSessionRegistry` 驱动 start→advance(自动 approve 权限暂停)→completion,
  断言观测流确为多步(SessionStarted + 文本 + SessionCompleted)。运行:
  `cargo test --features external-claude-code --test external_claude_code -- --ignored --nocapture`。

### 12.3 tool 注入

Claude Code 可优先通过 MCP/custom tools 注入宿主工具:

- 为每个 `Tool` 生成 MCP tool declaration。
- MCP tool handler 不直接执行,而是把 call 转给 `ExternalSessionHandler` decision point。
- `spawn_agent` 使用 host bridge。

如果某 CLI 模式无法 tool bridge:

- capability `tool_bridge = false`。
- `ExternalSessionRequest.tools` 只能作为 prompt hint,不得声明 full parity。

M6-3 的 `ClaudeCodeAdapter` 走的正是**不 bridge** 这条路:它不启动 MCP server,因此
`host_tools`/`host_subagents` 恒为 `false`;为避免"声明了工具却被静默忽略"的 spec 偏离,它对任何声明了
`tools` 的 `start`/`resume` 请求直接以 `UnsupportedCapability{HostTools}` 拒绝。MCP tool bridge 本身是后续
milestone 的独立任务,不在 M6-3 范围内。

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

#### 实现状态（M7-1，已落地）

- 新增非默认 feature gate `external-codex`（`Cargo.toml`）；开启才编译 adapter，探测复用 tokio 的
  process 支持，不引入新重依赖。
- 新增 feature-gated 模块 `src/agent/external/codex/{mod.rs,config.rs,probe.rs}`，在
  `src/agent/external/mod.rs` 以 `#[cfg(feature = "external-codex")]` 挂载并 re-export `CodexConfig`、
  `CodexProbeExec`、`CodexProbeOutput`、`SystemCodexExec`，以及别名 `codex_probe`、
  `codex_probe_with_exec`（避免与 Claude adapter 的裸 `probe`/`probe_with_exec` 名冲突）。为 M7-2/M7-3
  预留目录,本任务只填 config + probe。
- 本任务以**当前本机 Codex CLI（v0.144.1）实测 `--help` / `exec --help` 为准**，而非旧版参数假设。
  实测要点:结构化事件流 `--json` 位于 `codex exec` 子命令;审批策略 `-a/--ask-for-approval`
  （`untrusted`/`on-request`/`never`）与 `mcp` 子命令位于**顶层**;`-s/--sandbox`
  （`read-only`/`workspace-write`/`danger-full-access`）顶层与 `exec` 均有;`exec resume <id>` 支持续跑。
- `CodexConfig`:binary path / env override（BTreeMap，手写 `Debug` 脱敏,只印 key + `<redacted>`）/
  working dir(worktree) / permission mode / optional model / profile / 三个独立超时（口径同 §12 的
  「三类超时」:`timeout` 只管 probe/launch,`read_idle_timeout` 默认 10 min 是每行 stdout 空闲上限,
  `shutdown_grace` 默认 30s;新字段带 serde default）;serde round-trip 可持久化。
  `approval_policy_arg()` 与 `sandbox_mode_arg()` 把 `ExternalPermissionMode` 映射到当前 CLI 词汇:
  `Prompt→untrusted+read-only`（仅受信命令免批,其余升级宿主）、`AcceptEdits→on-request+workspace-write`、
  `Plan→never+read-only`、`BypassPermissions→never+danger-full-access`。`base_exec_args()` 产出
  `-a <approval> exec --json -s <sandbox> --skip-git-repo-check [--model M] [--profile P]`,顶层全局 flag
  严格排在 `exec` 子命令之前(规避“全局参数放 exec 后”的 CLI 踩坑),working dir 走进程 `current_dir`。
- probe:跑 `--version`（缺失/损坏/非零退出 → `ExternalAgentError::Launch`）+ `--help`（顶层）+
  `exec --help`;两份 help 均空 → `Launch`;`exec` help 无 `--json` 结构化流 →
  `UnsupportedCapability{Streaming}`。能力探测保守（未广告即 `false`）:streaming←`exec --json`,
  permission_bridge←`--ask-for-approval`/`--sandbox`,resume←`resume` 子命令,host_tools←顶层 `mcp`,
  usage/artifacts←结构化流,graceful_shutdown=true,host_subagents=false（留待后续）。探测走可注入
  `CodexProbeExec`（生产实现 `SystemCodexExec` 用 `tokio::process`,kill_on_drop + timeout）,永不 panic。
- 测试（模块内联 13 个,离线、无需真实 Codex、无网络）:config 默认/approval+sandbox 映射/`base_exec_args`
  顺序与 model/profile 省略/serde round-trip/Debug 脱敏;probe full-capability 探测、缺 binary→Launch、
  非零 version→Launch、空 help→Launch、无 `--json`→Unsupported{Streaming}、env secret 不泄露
  （Display+Debug 均断言）、真实 `SystemCodexExec` 对不存在 binary→Launch、`detect_capabilities` 未广告即
  false、探测子命令顺序（version→--help→exec --help）。
- Codex 真实 e2e 未在本任务范围（属 M7-3）;本机未运行真实 CLI。

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

#### 实现状态（M7-2,已落地）

- 新增 feature-gated 私有 decoder `src/agent/external/codex/decoder.rs`:有状态、跨 turn 单调 `seq` 的
  逐帧 `codex exec --json` 解码器。全程走 `serde_json::Value` 防御式导航,**不导出任何 raw frame 类型**,
  Codex 私有 wire schema 不进 `agent-lib` 稳定 API。经 `src/agent/external/mod.rs` 的
  `#[cfg(feature = "external-codex")]` re-export `CodexDecision`、`CodexDecodeContext`、`CodexStreamDecoder`。
- **以当前本机 Codex CLI(v0.144.1)实测 `codex exec --json` 输出为准**:该流是 `ThreadEvent` JSONL——
  `thread.started` / `turn.started` / `turn.completed` / `turn.failed`、`item.started` / `item.updated` /
  `item.completed`(包 `{id,type,...}` typed item)、以及顶层瞬时 `error` 通知。上表原先假设的
  `approval request → PausedForInteraction` **在 exec `--json` 流里不存在**:codex exec 自主运行,自己执行工具
  (含 MCP tool call 并回报 result),审批按启动时预设的 sandbox/approval 策略内部解决。故本 decoder 每 turn
  只落定 `CodexDecision::Completed`(`turn.completed`)或 `Failed`(`turn.failed`),**没有** host-pausable 的
  tool-call / interaction 决策。
- frame 映射:`thread.started`→`SessionStarted`(捕获 thread_id);`item.completed` `agent_message`→`TextDelta`
  (并作为本 turn summary);`item.started` `command_execution`→`CommandStarted`(cwd 取自 host `CodexDecodeContext`,
  流里不含 cwd);`item.completed` `command_execution` completed/failed→`CommandFinished`,`declined`(被审批策略拒绝)
  →信息性 `PermissionRequested`(无可应答项,runtime 已裁决);`item.completed` `file_change`→逐 change `FilePatch`
  (`summary="{kind} {path}"`);`item.started`/`item.completed` `mcp_tool_call`→`ToolStarted`/`ToolFinished`
  (`name="{server}/{tool}"`);`turn.completed`→`SessionCompleted` + `Completed`(usage 映射
  input/output/cached/cache_write/reasoning,cost=None);`turn.failed`→`Failed{Runtime}`。
- 容忍策略(稳定):空行 / `turn.started` / 顶层 `error` / `item.updated` / 未知顶层 type / 未知或缺失
  item `type`(`reasoning`/`web_search`/`todo_list`/`collab_tool_call`/error item…)→容忍(`Ok(None)`,无观测);
  非法 JSON / 非对象帧 / 缺字符串 `type` / `thread.started` 缺 `thread_id` / `item.*` 缺 `item` 对象或 item 非对象
  →`ExternalAgentError::Protocol`。所有诊断均为固定字符串,永不夹带 prompt/命令/输出/凭据。
- committed cassette `tests/fixtures/external/codex/full_session.json`:两 turn(turn1 = text/command/patch/
  MCP tool/declined 命令 → `Completed`,含 usage;turn2 = text/顶层 error/`turn.failed` → `Failed`)。由 in-code
  builder 经 `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` 再生成;`assert_no_secrets` 保证无凭据。测试放
  feature-gated 集成测试 `tests/agent_codex_cassette.rs`(7 个,离线、无需真实 Codex)。
- Codex 真实 e2e 与 live session adapter 未在本任务范围(属 M7-3);本机仅回放合成 cassette。

### 13.3 tool bridge

Codex 的 full tool injection 取决于当前 CLI/exec-server/MCP 能力:

- 如果 MCP/custom tool 可用,走与 Claude 相同的 bridge。
- 如果不可用,只支持半托管:
  - Codex 自己执行 read/search/shell。
  - host 只能观察 event 和 permission。
  - `ExternalSessionRequest.tools` 不暴露或仅作为 prompt contract。
- 对 unsupported tool bridge 的任务,dispatcher 应避免派给 Codex 或升级到支持 runtime。

#### 实现状态（M7-3,已落地）

`external-codex` feature 下已落地 **live session + runtime adapter**,把 M7-1 的启动配方与 M7-2 的私有
decoder 接进 milestone-5 的 `ExternalRuntimeAdapter` / `ExternalRuntimeSession` 抽象(§11):

- `CodexAdapter`(`src/agent/external/codex/adapter.rs`,**唯一 pub 类型**):per-runtime 工厂。
  `new(config)` 报告本 adapter 实现的全部能力;`with_probed_capabilities(config, &probed)` 把实现能力与
  probe 实测能力**逐位取交**。`start` 用 `codex … exec … <prompt>` 启动全新 session,`resume` 用
  `codex … exec resume … <thread_id> <message>` 复活既有 thread,启动失败分别归类 `Launch` /
  `ResumeUnavailable`。
- **关键差异——每 turn 一个一次性进程**:与 Claude Code 的单条长驻 `stream-json` 进程不同,`codex exec`
  的 prompt 是 CLI **位置参数**(不是 stdin 帧),进程在一个 turn 落定后即退出;续跑是全新的
  `codex exec resume <thread_id> <message>` 进程。故 `CodexSession`(私有)在 `begin` 里为首个 turn spawn
  一个进程并读到 `thread.started` 帧拿到真实 thread id(作为 registry key、resume token),该 turn 其余帧
  (`item.*`/`turn.completed`)留给第一次 `advance` 续读;此后每个 `Continue` follow-up 在 `advance` 里 spawn
  一个新的 `exec resume` 进程。整段 session 共用一个跨全程单调 `seq` 的 `CodexStreamDecoder`。
- **参数顺序**:`resume` 子命令**不接受** `-s/--sandbox`、`-p/--profile`(实测当前 CLI),故新增
  `CodexConfig::base_resume_args(session_id)` 把 sandbox/model/profile 上提到顶层
  (`codex -a <approval> -s <sandbox> [--model M][--profile P] exec resume --json --skip-git-repo-check
  <id>`),再由 adapter 追加 `<message>`;frozen 的 `base_exec_args()`(M7-1,有断言精确顺序的测试)不改动。
  生产进程 **stdin=null**(否则 codex 阻塞在 "Reading additional input from stdin…")、**stderr 丢弃**(防原始
  文本泄漏)、stdout piped 逐行喂 decoder,`kill_on_drop`、每读空闲超时 `read_idle_timeout`。
- 能力(诚实按 M7-2 结论):`codex exec --json` **自主运行**——审批按命令行预置的 sandbox/approval 策略解决、
  自己执行工具,流里**没有**任何 host 可暂停的 tool-call/approval 帧,一个 turn 只会 `Completed` 或
  `Failed`。故 `implemented_capabilities()` 报告 `host_tools=false` / `host_subagents=false` /
  **`permission_bridge=false`**;`streaming`/`resume`/`artifacts`/`usage`/`graceful_shutdown` 为 true。声明了
  `tools` 的 `start`/`resume` 请求以 `UnsupportedCapability{HostTools}` 明确拒绝;follow-up 的
  `RespondToolResults`→`UnsupportedCapability{HostTools}`、`RespondSubagent`→`{HostSubagents}`、
  `RespondInteraction`→`{PermissionBridge}`,均**明确拒绝而非静默忽略**。
- IO 经私有 `CodexLauncher` / `CodexTurnStream` trait 注入:生产用 `SystemCodexLauncher`(`tokio::process`),
  单测注入 `FakeLauncher` 回放固定 JSONL 帧并**逐 turn 捕获 `CodexTurnSpec`**,**离线**跑通
  begin/advance(fresh + resume)/shutdown 全状态机,无需真实 binary、无网络。
- 真机 e2e:`tests/external_codex.rs` 有一个 `#[ignore]` 用例,通过 `CODEX_BIN` 或 PATH 发现 `codex`,缺失
  binary/登录即带清晰信息**跳过**(退出为绿),否则在临时 git worktree 里以 `AcceptEdits`(`workspace-write`,
  让自主 CLI 能落盘且无需 host 审批)驱动 probe→start→advance→completion→graceful shutdown,断言观测流确为
  多步(SessionStarted + ≥1 文本 + SessionCompleted)。本机 codex-cli 0.144.1 实跑通过(5 个观测事件、生成
  `READY.txt`、优雅关闭,约 51s)。运行:
  `cargo test --features external-codex --test external_codex -- --ignored --nocapture`。

## 14. OpenCode adapter

OpenCode 需要先做 capability probe,因为部署形态可能更多。

> **实现状态(M8-1,已落地)**:feature `external-opencode` 下新增了受管 OpenCode adapter 的
> **启动配置**([`OpenCodeConfig`](../src/agent/external/opencode/config.rs))与 **capability probe**
> ([`agent::external::opencode_probe`](../src/agent/external/opencode/probe.rs))。probe 以**当前本机
> `opencode` CLI 实测** `--version` / `--help` / `run --help` 为准,不硬编码假设:缺失/损坏的 binary →
> `Launch`;`opencode run` 无 `--format json` 结构化事件流 → `UnsupportedCapability{Streaming}`;其余能力
> 位从两份 help **保守探测**(默认 `false`,仅当 help 明确广告才开):streaming←`run --format` 且 `json`、
> permission_bridge←`run --auto`、resume←`run --continue`/`--session` 或顶层 `session`、host_tools←顶层
> `mcp`、usage/artifacts←结构化流、graceful_shutdown←恒 `true`、host_subagents←恒 `false`(spawn bridge 待
> M8-3 验证;选预设 `--agent` ≠ host 铸造 subagent)。`OpenCodeConfig` 把 `ExternalPermissionMode` 保守映射
> 到 `run` 的唯一权限旁路开关 `--auto`:**仅 `BypassPermissions` 发 `--auto`**,其余模式不加(交给
> permission bridge 或默认拒绝),避免用全量自动批准越权放宽宿主权限边界;更细的 read-only/accept-edits 由
> `--agent` 预设 agent 表达。stream decoder 待 M8-2,live session adapter 与真机 e2e 待 M8-3。
>
> **实现状态(M8-2,已落地)**:`external-opencode` 下新增了 adapter 私有的 `opencode run --format json`
> **stream decoder**([`OpenCodeStreamDecoder`](../src/agent/external/opencode/decoder.rs))。它防御式地
> 解析 CLI 逐行输出的事件信封 `{ type, timestamp, sessionID, ... }`(`run.ts` 的 `emit()` 只镜像
> `text` / `tool_use` / `step_start` / `step_finish` / `reasoning` / `error` 六种),归一化成 sequenced
> [`ExternalObservedEvent`](../src/agent/external/mod.rs) 与 per-turn `OpenCodeDecision`,不把私有
> wire schema 变成稳定 public API。与 `codex exec --json` 一样,`run --format json` **自主运行**:权限提示
> 按 `--auto` 启动开关裁决而非回灌 host(JSON `run` loop 在 `--auto` 下自动批准、否则自动拒绝,从不把
> `permission.asked` 镜像到 stdout),故 decoder **无 host-pausable 决策臂**——turn 只会 `Completed`
> (终结 `step_finish`,`reason != "tool-calls"`;usage 跨步累加)或 `Failed`(顶层 `error`)。因为流里只镜像
> **已结算**的 tool part(`state.status` 已是 `completed`/`error`),decoder 从这一帧重建 started/finished
> 事件对:`bash` → `CommandStarted`+`CommandFinished`,`edit`/`write`/`patch` → `FilePatch`,`task` 子代理
> 与其余工具 → `ToolStarted`+`ToolFinished`;被权限拒绝的工具(错误串是 OpenCode 稳定的
> `PermissionRejectedError`/`PermissionDeniedError` 文案)→ **信息型** `PermissionRequested`。回归由离线
> cassette([`tests/agent_opencode_cassette.rs`](../tests/agent_opencode_cassette.rs) +
> `tests/fixtures/external/opencode/full_session.json`)冻结,覆盖 text/command/patch/permission/tool/
> subtask/completion/error 及容忍/畸形帧分类;live session adapter 与真机 e2e 仍待 M8-3。
>
> **实现状态(M8-3,已落地)**:`external-opencode` 下把 M8-1 启动配方与 M8-2 私有 decoder 接进
> milestone-5 的 `ExternalRuntimeAdapter` / `ExternalRuntimeSession` 抽象(§11),新增 **live session +
> runtime adapter** [`OpenCodeAdapter`](../src/agent/external/opencode/adapter.rs)(该模块**唯一 pub
> 类型**):`new(config)` 报告本 adapter 实现的全部能力,`with_probed_capabilities(config, &probed)` 把实现
> 能力与 probe 实测能力**逐位取交**;`start` 用 `opencode run … <prompt>` 启动全新 session,`resume` 用
> `opencode run … --session <id> <message>` 续跑既有 session,启动失败分别归类 `Launch` / `ResumeUnavailable`。
> - **每 turn 一个一次性进程**(同 Codex):`opencode run` 的 prompt 是 CLI **位置参数**,进程在一个 turn 落定
>   后退出;续跑是全新的 `opencode run --session <id> <message>` 进程。故为 `base_run_args()` 新增配套的
>   `OpenCodeConfig::base_resume_args(session_id)`(= 复用 `run --format json [--auto][--model][--agent]` 再追加
>   `--session <id>`,对齐官方 CLI:`run` 接受 `-s/--session` / `-c/--continue`),由 adapter 追加 `<message>`。
> - **无 init 帧**:与 Codex 的 `thread.started` 前导帧不同,OpenCode 的 session id 随**每帧** `sessionID` 到达,
>   故 `OpenCodeSession`(私有)在 `begin` 里读到 decoder 惰性捕获首个 `sessionID`(并发出唯一
>   `SessionStarted` 观测)为止,把这些前导观测缓存给第一次 `advance` 续读;此后每个 `Continue` follow-up 在
>   `advance` spawn 一个新的 `run --session` 进程,整段 session 共用一个跨全程单调 `seq` 的 decoder。生产进程
>   **stdin=null**(否则 `run` 阻塞在从 stdin 读消息)、**stderr 丢弃**(防原始文本泄漏)、stdout piped 逐行喂
>   decoder,`kill_on_drop`、每读空闲超时 `read_idle_timeout`。
> - 能力(诚实按 M8-2 结论):`run --format json` **自主运行**,流里没有 host 可暂停的 tool-call/approval 帧,
>   一个 turn 只会 `Completed`/`Failed`。故 `implemented_capabilities()` 报 `host_tools=false` /
>   `host_subagents=false` / **`permission_bridge=false`**,`streaming`/`resume`/`artifacts`/`usage`/
>   `graceful_shutdown` 为 true;声明 `tools` 的 `start`/`resume` 以 `UnsupportedCapability{HostTools}` 拒绝,
>   follow-up 的 `RespondToolResults`→`{HostTools}`、`RespondSubagent`→`{HostSubagents}`、
>   `RespondInteraction`→`{PermissionBridge}` 均**明确拒绝而非静默忽略**。
> - IO 经私有 `OpenCodeLauncher` / `OpenCodeTurnStream` trait 注入:生产用 `SystemOpenCodeLauncher`
>   (`tokio::process`),单测注入 `FakeLauncher` 回放固定 JSON 帧并**逐 turn 捕获 `OpenCodeTurnSpec`**,**离线**
>   跑通 begin/advance(fresh + resume)/shutdown 全状态机,无需真实 binary、无网络。
> - **worktree 隔离**:配置了 `working_dir` 时,`base_run_args()`/`base_resume_args()` 以显式
>   `--dir <path>` 传入。OpenCode 从 `--dir`/继承的 `$PWD` 解析其项目与文件落盘位置,**而非仅**子进程的
>   OS 级 cwd——`tokio::process` 的 `current_dir()` 只 `chdir` 却不更新继承来的 `PWD`(仍指向启动进程的
>   目录),故若只设 cwd,OpenCode 会把文件写进**启动它的那个 checkout**(实测复现)。因此 working dir 必须
>   走 `--dir`(authoritative,压过 cwd 与 `$PWD`);launcher 另把它设为进程 `current_dir` 作 belt-and-suspenders。
> - 真机 e2e:[`tests/external_opencode.rs`](../tests/external_opencode.rs) 有一个 `#[ignore]` 用例,通过
>   `OPENCODE_BIN` 或 PATH 发现 `opencode`(可选 `OPENCODE_MODEL`/`OPENCODE_AGENT`),缺失 binary/登录即带清晰
>   信息**跳过**(退出为绿),否则在临时 git worktree 里以 `BypassPermissions`(映射 `--auto`,让自主 CLI 能落盘
>   且无需 host 审批)驱动 probe→start→advance→completion→graceful shutdown,断言观测流确为多步,并**断言
>   worktree 隔离**:`READY.txt` 落在 worktree 内、且**绝不**泄漏进启动它的 checkout(cwd)。**本机
>   opencode 1.17.15 实跑通过**(6 个观测事件、1 条文本、`READY.txt` 生成于 worktree 内、无泄漏、优雅关闭,
>   约 20s)。运行:`cargo test --features external-opencode --test external_opencode -- --ignored --nocapture`。

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

**已落地**(执行 M4-2,`src/agent/external/capability.rs`)。落地的能力集采用与 M4-4 review 清单
一致的粗粒度 8 项(而非本节初稿的细粒度字段),避免在 runtime 尚未接线时就固化过细的探测维度:

```rust
pub struct ExternalRuntimeCapabilities {
    pub runtime: ExternalRuntimeKind,
    pub streaming: bool,
    pub resume: bool,
    pub permission_bridge: bool,
    pub host_tools: bool,
    pub host_subagents: bool,
    pub artifacts: bool,
    pub usage: bool,
    pub graceful_shutdown: bool,
}
```

- `ExternalRuntimeCapabilities::none(runtime)` / `ExternalRuntimeKind::conservative_capabilities()`：
  保守基线,所有能力为 `false`,未探测即不假装支持(§2 非目标)。
- `supports(ExternalCapability)`：按 enum 查单项能力。
- `unsupported(ExternalCapability, detail)`：构造 classified error 值(external error 在本 crate 里作为值
  载入 `ExternalSessionResult::Failed` / cursor,不作为 `Result` err 返回)。

使用场景:

- adapter 启动前探测。
- dispatcher 做 worker selection。
- machine 遇到 unsupported decision point 时给出 classified error。
- docs/capability-matrix.md 记录实测值。

**已落地**错误(执行 M4-2):`capability` 为强类型 `ExternalCapability` enum(非初稿的 `String`),
`Display` 只含 runtime + capability 标签 + 稳定 detail,不含 raw prompt/tool input:

```rust
ExternalAgentError::UnsupportedCapability {
    runtime: ExternalRuntimeKind,
    capability: ExternalCapability,
    detail: String,
}
```

## 16. worktree isolation

`WorktreeIsolation` 曾经只是 data。M9-1 起 `WorktreeManager` 真正执行隔离,默认实现
`GitWorktreeManager`(handler/scheduler 侧 hook,object-safe,可作 `Arc<dyn WorktreeManager>`):

| isolation | prepare 行为 | cleanup 行为 |
|---|---|---|
| `Shared` | 直接返回传入 worktree,不做任何 IO | 从不删除;dirty close 仍标 residual |
| `PerAgentWorktree` | `<root>/agent-<agent_id>` 固定 linked git worktree,已存在则幂等复用 | 持久保留(跨 session 复用),从不删除;dirty close 标 residual |
| `EphemeralGitWorktree` | `<root>/ephemeral/<agent_id>-<n>` 每 session 新建 linked git worktree | graceful → `git worktree remove --force` 删除;forced/failed → **保留** 并标 residual |

`root` 默认 `std::env::temp_dir()/agent-lib-worktrees`,置于 base checkout 之外避免 git worktree
嵌套;`with_root` 可覆盖。ephemeral 路径用 per-manager 单调计数器(非随机/时钟),遵循本 crate
“nondeterminism 由 caller 掌控” 约束,并对已存在(retained)目录做 existence 跳过。

`WorktreeManager`:

```rust
#[async_trait]
pub trait WorktreeManager: Send + Sync {
    async fn prepare(
        &self,
        agent_id: AgentId,
        base: &WorktreeRef,
        isolation: WorktreeIsolation,
    ) -> Result<PreparedWorktree, WorktreeError>;

    async fn cleanup(
        &self,
        prepared: PreparedWorktree,
        disposition: ExternalSessionShutdown,
    ) -> Result<WorktreeCleanupOutcome, WorktreeError>;
}
```

git 操作走 `WorktreeGitExec` hook(生产实现 `SystemGit` shell out `git worktree add --detach` /
`git worktree remove --force`),因此 placement/teardown 策略在无真实仓库下即可单测。

### residual side-effect 策略(design §6.4/§16)

`cleanup` 消费 session registry 报告的 `ExternalSessionShutdown` disposition:

- `Graceful` → 无 residual;ephemeral 被删除,worktree 视为 clean 可复用。仅当子进程**退出码为 0**
  时才归此类(M1-6 / H-EXT-3):非零退出意味着 CLI 中途失败,按 `Failed` 处理。
- `ForcedKill` / `Failed` → `leaves_residual_side_effects()` 为真;**任何** isolation 都标
  `WorktreeCleanupOutcome::residual_side_effects()=true`,ephemeral 也**保留**供排查,
  `safe_to_reuse()` 返回 false。forced kill / failed 绝不误标 clean。

registry 的 `cleanup` / `cleanup_agent` 返回 `ExternalSessionShutdown`;scheduler 把该 disposition
既喂给 `TraceHandle::record_external_shutdown`(审计),又喂给 `WorktreeManager::cleanup`
(决定删除/保留/标记),二者用同一 disposition 保持一致。

### 进程组级 kill（M2-1 / H-EXT-2）

CLI 经其 shell 工具拉起的孙进程（构建、测试、dev server）若只杀直接子进程会变成孤儿，可能继续写
已被删除/复用的 worktree。因此三个 CLI adapter 与 ACP connection 统一收口在
`agent::external::process_group`（crate 私有，M8-2 将并入共享 process 模块）：

- **spawn**：unix 上 `Command::process_group(0)` 使子进程自成进程组（pgid == pid）。
- **force-close**（shutdown grace 超时后）：先向**整个进程组**发 SIGTERM，2s 升级窗口内未退出
  再发 SIGKILL；组信号投递失败（如 EPERM）回退 `start_kill` 至少杀掉 leader。`ForcedKill`/
  `Failed` 分类不变。
- **平台差异**：Windows 无 POSIX 进程组语义，保持 `start_kill` 只杀直接子进程（本文上述
  force-close 保证仅限 unix）。
- 该保证覆盖协作式 `close` 路径；transport 未经 `close` 直接 drop 时仍只有 `kill_on_drop`
  兑底（只杀直接子进程）。capability probe 是 `wait_with_output` 有界的一次性进程，不在此列。

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

1. **boundary toolset reconfig(已落地 M9-3)**
   - host-facing 入口 [`ExternalAgentMachine::reconfigure(active_tools, timing)`](../src/agent/external/machine.rs),
     不属于 sans-io `step`,对应内部 `DefaultAgentMachine::reconfigure`。
   - 边界判据 = 无 in-flight turn(cursor 停在 `Idle`/`Done`/`Error`)。边界上任意 `timing` 都**立即**替换
     `ExternalAgentState.active_tools`(并丢弃任何已排队的旧 reconfig),返回 `ExternalReconfigOutcome::Applied`。
   - turn 进行中且 `timing = NextBoundary`:把新 tool set **排队**进可序列化的
     `ExternalAgentState.pending_reconfig`(随 snapshot/restore 持久),live session 不受影响,返回 `Queued`;
     下一次 `begin_user_turn` 打开新 turn 时折入 `active_tools`。
   - 效果:下一次 `NeedExternalSession(Start/Continue)` 的 `request.tools` 使用新集(`build_request` 直接读
     `active_tools`)。如果 runtime session 已启动但不能动态改 tools,由 handler 在下一 boundary 重启/新建 session。

2. **live tool bridge reconfig(hot swap)**
   - runtime 支持 MCP/tool refresh 时,handler 发 runtime-specific reconfigure。
   - 首版 machine 只做 boundary reconfig:turn 进行中调用 `timing = Hot` 会以
     `UnsupportedCapability{ capability: Reconfigure }` 拒绝,并**不改动任何状态**(active_tools / 排队 / cursor 全不变),
     从而绝不悄悄改变 live session。`ExternalCapability::Reconfigure` /
     `ExternalRuntimeCapabilities.reconfigure` 显式建模该能力,当前所有 runtime adapter 均声明 `false`。

首版只做 boundary reconfig,并要求 runtime capability(`Reconfigure`)明确。

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
> parity(执行 M3,含 `spawn_agent` tool-bridge 特判)、sequenced live sink(执行 M4-1)、capability model
> (执行 M4-2:`ExternalRuntimeCapabilities` / `ExternalCapability` / `UnsupportedCapability`,设计见
> §15)。下列条目保留设计意图,并就实现差异就地标注。

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

- 更新 [`capability-matrix.md`](./capability-matrix.md)。**已落地(M9-5)**:补齐 Codex/OpenCode adapter 的
  probe/decoder/adapter 落地状态与 examples 指针。
- examples(**已落地 M9-5**,均为 scoped-effect wiring,经 `ExternalAgentMachine` + 作用域
  `ExternalSessionHandler` 驱动,不直接调 adapter):
  - Claude Code managed — [`examples/managed_claude_code.rs`](../examples/managed_claude_code.rs)。
  - Codex managed — [`examples/managed_codex.rs`](../examples/managed_codex.rs)。
  - OpenCode managed — [`examples/managed_opencode.rs`](../examples/managed_opencode.rs)。
  - mixed external agents — [`examples/managed_mixed.rs`](../examples/managed_mixed.rs)。
  - 共享装配 [`examples/support/managed.rs`](../examples/support/managed.rs);每个 example 用
    `required-features` 门控,CLI 缺失/probe 失败即打印非密 skip 并 exit 0。
- **facade 构造(快速上手)**:examples 展示的是全手工 scoped-effect wiring(probe → registry →
  `ExternalSessionHandler`),推荐给需要完全掌控装配的宿主。若只想快速拿到一个带 handler、可直接
  委托的受管 agent,用 facade 的一步式构造
  `ManagedExternalAgent::codex()...build_with_default_session_handler().await?`
  (见 [`README.md`](../README.md) quick start 与 [`facade-api.md`](./facade-api.md) §11):它在 build 时
  探测本机已登录 CLI 并接上官方 registry-backed handler。默认 crate build 不含任何 CLI adapter,
  未开启对应 `external-*` feature(或 CLI 未登录)时该装配 **fail-fast**(非密错误,点名要开的 feature),
  绝不产出「build 后即可 run 但缺 session handler」的 agent。手工自定义 handler 仍走
  `.session_handler(..).build()`(短路 probe)。
- 安全说明:权限、worktree、secret redaction、ignored tests。运行说明另见根目录
  [`AGENTS.md`](../AGENTS.md)。

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
