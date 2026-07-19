# Facade API 设计

> 状态:设计草案。本文讨论如何在 `agent-lib` 已有 Client / Conversation / Agent 三层之上,
> 增加一组 batteries-included 的简单 API,让常见聊天、工具 agent、subagent、managed external
> agent 场景不必手写 `Conversation` pending 事务、`AgentMachine`、`RequirementIds`、
> `HandlerScope` 与 driver wiring。
>
> 相关文档:
> [`conversation-core.md`](./conversation-core.md)(Conversation 不变量与 snapshot/restore)、
> [`agent-layer.md`](./agent-layer.md)(Agent sans-io 分层)、
> [`agent-effect-model.md`](./agent-effect-model.md)(Requirement / HandlerScope / Pop)、
> [`external-agent.md`](./external-agent.md)(外部 coding-agent 接入)、
> [`managed-external-agent.md`](./managed-external-agent.md)(受管 external agent)。
>
> 标注约定:本文中的类型与方法名是建议的 facade API 形状,未必已在代码中存在。

## 0. 一句话

**Facade API 是一层 batteries-included 的装配层:默认帮调用方接好 client、id source、
Conversation session、AgentMachine、ReferenceScope、tool registry、approval policy、
subagent/external-agent delegation 与 driver;但所有核心事实仍然落在现有 provider-neutral
类型里。**

它不是第四套 runtime,也不是 `Conversation` / `AgentMachine` 的替代品。它只降低入口复杂度,
并提供清晰的逃生舱回到底层 API。

## 1. 背景

当前库的能力完整,但简单任务需要理解太多底层概念:

- Client 层用户要手写 `ChatRequest`、adapter、stream mode、provider extras。
- Conversation 层用户要手写 `begin_turn`、`start_assistant_response`、`finish_assistant`、
  `commit_pending`、tool call mapping 与 message id。
- Agent 层用户要手写 `AgentSpec`、`AgentState`、`DefaultAgentMachine`、`RequirementIds`、
  `ToolExecutionIds`、`ReferenceScope`、`RunContext`、`drive_turn`。
- subagent / external agent 用户还要理解 `NeedSubagent`、scope pop、`Dispatcher`、
  `Escalator`、mailbox、blackboard、plan、external session attach/cancel。

这些 API 对库内核是必要的:它们让状态可校验、可恢复、可测试、可组合。但对大多数调用方,
第一段代码应该只表达:

```rust
let chat = Chat::builder()
    .provider(ProviderConfig::anthropic_from_env()?)
    .model("databricks-claude-haiku-4-5")
    .system("回答简洁。")
    .build()?;

let reply = chat.ask("用一句话解释这个库。").await?;
println!("{}", reply.text());
```

Facade 的目标就是让这段代码成立,同时不牺牲底层设计原则。

## 2. 目标与非目标

### 2.1 目标

1. **渐进式使用**
   从 one-shot chat 到 stateful chat,再到 tool agent、subagent、managed external agent,
   API 逐层增加概念,不要求用户一开始理解完整 effect 模型。

2. **保留强不变量**
   Facade 内部仍然使用 `Conversation` 推进 turn,仍然通过 `AgentMachine` reify effect,
   仍然由 `HandlerScope` / driver 兑现 requirement。不能绕过底层不变量重新实现一套轻量状态机。

3. **默认可用**
   默认生成稳定 identity,默认创建 session,默认处理 pending 失败,默认接好 tool registry,
   默认给 headless/attended 审批策略一个明确行为。

4. **可恢复**
   `ChatSession`、stateful `Agent`、delegating agent 都应支持 snapshot/restore。
   Snapshot 只保存 data-only facts,不保存凭据、闭包、进程句柄、client handle。

5. **可观测**
   简单路径可只拿文本;需要调试或产品集成时,可拿完整 `RunOutput`、tool trace、
   delegation trace、artifact、usage、raw response/event。

6. **逃生舱清楚**
   用户能拿到 normalized `Response`、`StreamEvent`、`Conversation` snapshot、agent state、
   provider extras,并能在需要时退回底层模块。

### 2.2 非目标

- 不隐藏 provider 能力差异。Facade 可以提供方便默认值,但 provider-specific extras 仍显式绑定
  `ProviderId`。
- 不把所有能力塞进一个 `EasyClient`。聊天、stateful conversation、tool agent、delegating agent
  的状态语义不同,需要在 API 上区分。
- 不让 `Reply` 只等于 `String`。可以提供 `ask_text`,但主路径返回结构化 `Reply` / `RunOutput`。
- 不把 API key、base URL token、运行时闭包、live process handle 写进 snapshot。
- 不让 managed external agent 伪装成普通函数工具。它有 session、artifact、worktree、权限、
  cancel、attach 等语义,需要一等建模。

## 3. 模块与公开层级

建议新增:

```rust
agent_lib::facade
agent_lib::prelude
```

`prelude` 只重导简单路径类型:

```rust
pub use agent_lib::facade::{
    Agent, Approval, ApprovalPolicy, ApprovalRequest, BudgetLimits, Chat, ChatSession, Delegation,
    FacadeError, ManagedExternalAgent, ModelConfig, ProviderConfig, Reply, RunEvent, RunOutput,
    RunStream, Tool, ToolContext, WireRunEvent, WireRunOutput,
};
pub use agent_lib::model::{
    ContentBlock, ImageSource, Message, Normalized, ProviderExtras, ProviderId, Role, StopReason,
    ToolCall, ToolResponse, ToolStatus, Usage,
};
```

不建议把 `AgentMachine`、`Requirement`、`Boundary`、adapter 细节放进 prelude。需要这些能力的用户,
应显式从原模块导入。

> **当前口径**:`prelude` 重导常用 facade 入口、`FacadeError`、run/event wire 投影与高频 model
> 类型。更底层的 `AgentMachine`、`Requirement`、adapter 细节和 feature-gated external handler
> 类型按设计不入 prelude。

Facade 分三层能力:

| 层级 | 类型 | 适用场景 | 用户需要理解 |
|---|---|---|---|
| Level 1 | `Chat` / `ChatSession` | one-shot 或多轮聊天 | provider、model、system、stream |
| Level 2 | `Agent` | 工具调用、审批、loop policy | tool、approval、run output |
| Level 3 | `Agent` + delegation | subagent / managed external agent / dispatcher | delegate、policy、trace、snapshot |

当前实现只公开一个 stateful `Agent::builder()`,通过是否配置 tool / delegate 进入 Level 2/3。

## 4. 配置类型

### 4.1 ProviderConfig

`ProviderConfig` 是 `EndpointConfig` 的易用包装,负责从环境变量或 builder 构造 provider endpoint:

```rust
let anthropic = ProviderConfig::anthropic_from_env()?;

let openai = ProviderConfig::openai()
    .base_url("https://example.openai.azure.com/openai/v1")
    .api_key(env::var("OPENAI_API_KEY")?)
    .api_version("2025-04-01-preview")
    .build()?;

let custom = ProviderConfig::custom(endpoint_config, ProviderId::Anthropic);
```

设计约束:

- `ProviderConfig` 可以持有凭据,但必须标记为不应 debug/log/persist。
- snapshot 不保存 `ProviderConfig`。
- provider extras 继续使用现有 `ProviderExtras`,并显式绑定目标 provider。

### 4.2 ModelConfig

`ModelConfig` 包装常用模型参数:

```rust
let model = ModelConfig::new("gpt-5.5")
    .max_tokens(1024)
    .temperature(0.2)?
    .provider_extras(provider_extras);
```

Builder 上保留短写:

```rust
Agent::builder()
    .model("gpt-5.5")
    .max_tokens(1024)
    .temperature(0.2)
    .provider_extras(provider_extras);
```

`provider_extras` 使用现有 `ProviderExtras`，必须显式绑定目标 `ProviderId`。`ChatBuilder`、
`AgentBuilder`、`AgentWorkerBuilder`（仅显式 pin model 的 worker）与 `AgentRestoreBuilder` 都暴露
`.provider_extras(...)` 短写，并通过 `ModelConfig` / `ModelRef` 贯通到最终 `ChatRequest.provider_extras`。
当 builder 同时设置了 `ProviderConfig` 时，extras 的 `provider` 必须与该 provider 一致，否则 build
返回 `FacadeError::Config`；纯 `.client(...)` 注入路径无法从 `Capability` 可靠推断 wire provider，
因此保留逃生舱语义，把 extras 原样交给注入的 client。继承模型的 worker 继承 supervisor 的完整
model config（含 extras），不能额外设置 worker-local extras。

配置校验采用 fail-closed 口径：`ChatBuilder` / `AgentBuilder` / 显式模型的 `AgentWorkerBuilder`
拒绝空白 model 名；`ModelConfig::temperature` 与各 builder 的 `.temperature(...)` 在 build 时拒绝
`NaN` / 正负无穷，避免向 provider 发送非 JSON 数值。

`ModelConfig` 应能转成 agent 层已有 `ModelRef`,也应能构造 Client 层 `ChatRequest` 的公共字段。

## 5. Chat Facade

### 5.1 角色划分

建议区分:

```text
Chat        = 可共享的配置 + LlmClient 装配入口
ChatSession = 有状态 Conversation session
```

`Chat::ask` 是 one-shot,不保留历史。`ChatSession::send` 是多轮,保留历史。
如果保留 `Chat::send` 作为 convenience,文档必须明确它是否 stateful。更清晰的形状是:

```rust
let chat = Chat::builder()
    .provider(ProviderConfig::openai_from_env()?)
    .model("gpt-5.5")
    .system("回答简洁。")
    .build()?;

let reply = chat.ask("什么是 provider-neutral client?").await?;

let mut session = chat.session().build()?;
let a = session.send("解释 ownership。").await?;
let b = session.send("给一个例子。").await?;
```

### 5.2 基础 API

```rust
impl Chat {
    pub fn builder() -> ChatBuilder;
    pub async fn ask(&self, input: impl IntoUserMessage) -> Result<Reply, FacadeError>;
    pub async fn ask_full(&self, input: impl IntoUserMessage) -> Result<RunOutput, FacadeError>;
    pub fn session(&self) -> ChatSessionBuilder;
}

impl ChatSession {
    pub async fn send(&mut self, input: impl IntoUserMessage) -> Result<Reply, FacadeError>;
    pub async fn send_full(&mut self, input: impl IntoUserMessage) -> Result<RunOutput, FacadeError>;
    pub async fn stream(&mut self, input: impl IntoUserMessage) -> Result<RunStream, FacadeError>;

    pub fn conversation(&self) -> &Conversation;
    pub fn snapshot(&self) -> Result<ConversationSnapshot, FacadeError>;
    pub fn restore(snapshot: ConversationSnapshot, chat: Chat) -> Result<Self, FacadeError>;
}
```

`IntoUserMessage` 可以先支持:

- `&str`
- `String`
- `Message`
- `Vec<ContentBlock>`

后续可扩展图片、文件、tool result 等多模态输入。

### 5.3 内部映射

`ChatSession::send_full` 内部负责:

1. 生成 `TurnId`、user `MessageId`、assistant `MessageId`。
2. `Conversation::begin_turn`。
3. 从 `effective_view` 构造 `ChatRequest`。
4. 调用 `LlmClient::chat`。
5. `start_assistant_response`。
6. 如果无 tool-use,`finish_assistant` + `commit_pending`。
7. 返回 `RunOutput`。

Chat facade 不执行工具。如果模型返回 tool-use,第一版可以报 `FacadeError::UnexpectedToolUse`。
需要工具时,用户应使用 Agent facade。

### 5.4 Streaming

流式 API 不暴露 `Accumulator` 为必需概念,但保留 raw event:

```rust
let mut stream = session.stream("写一段短诗。").await?;

while let Some(event) = stream.next().await.transpose()? {
    match event {
        RunEvent::TextDelta(text) => print!("{text}"),
        RunEvent::Done(output) => eprintln!("usage={:?}", output.usage),
        RunEvent::RawStream(raw) => trace!("{raw:?}"),
        _ => {}
    }
}
```

内部仍使用 `stream::Accumulator` 折叠完整 `Response`,并在最终 `Done` 中提交 Conversation。

## 6. Reply、RunOutput 与事件

### 6.1 Reply

`Reply` 是最小成功结果:

```rust
pub struct Reply {
    text: String,
    usage: Option<TokenUsage>,
    stop_reason: Option<StopReason>,
}

impl Reply {
    pub fn text(&self) -> &str;
    pub fn usage(&self) -> Option<&TokenUsage>;
    pub fn stop_reason(&self) -> Option<&StopReason>;
}
```

`Reply::text()` 从 normalized `Response` 的 text blocks 聚合而来。若存在非文本 content,
`Reply` 不丢弃完整数据,而是在 `RunOutput.response` 中保留。

### 6.2 RunOutput

`RunOutput` 是产品集成和调试入口:

```rust
pub struct RunOutput {
    pub reply: Reply,
    pub response: Option<Response>,
    pub usage: UsageSummary,
    pub tool_calls: Vec<ToolTrace>,
    pub delegations: Vec<DelegationTrace>,
    pub artifacts: Vec<ArtifactRef>,
    pub events: Vec<RunEvent>,
}
```

说明:

- Chat one-shot / session 一般有 `response: Some(Response)`。
- Managed external agent 可能没有一对一 LLM `Response`,但仍有 `reply`、`delegations`、
  `artifacts` 与 events。
- `UsageSummary` 可以聚合 supervisor、local subagent、external runtime 报告的 usage。

#### 6.2.1 事件一致性边界(non-streaming 与 streaming)

`RunOutput.events`(由 `run_full` 返回,或流式路径终态 `Done` 内嵌)与 `stream` 逐个
yield 的事件遵循同一条**生命周期事件契约**:

- **生命周期事件一致**:approval、tool、delegation 三类归一化事件
  (`ApprovalRequested` / `ToolStarted` / `ToolFinished` /
  `DelegationStarted` / `DelegationArtifact` / `DelegationFinished` /
  `DelegationFailed`)在两条路径上产出**相同的归一化序列**——同样的顺序、同样的
  `tool_name` / `call_id` / `reason` / 脱敏 `input` / `delegate` / `status`。
  - 一次被批准的工具调用:`ApprovalRequested`(若被 gate)紧接其
    `ToolStarted` → `ToolFinished`。
  - 一次**被拒**的工具调用从未执行,因此**两条路径都不产** `ToolStarted` 或
    `ToolFinished`,只保留 `ApprovalRequested`。non-streaming 路径过去会为被拒调用
    投出一个 name 为空的幽灵 `ToolFinished`,M2-2 已将其对齐删除。
  - 一次委派:`DelegationStarted` →(每个 artifact 一条 `DelegationArtifact`)→
    `DelegationFinished`(或失败时 `DelegationFailed`)。
- **token delta 只属于 streaming 路径**:`RunEvent::TextDelta` 是流式路径逐 token
  产出的增量文本;non-streaming `run_full` **绝不伪造** token delta,其
  `RunOutput.events` 不含任何 `TextDelta`(最终文本从 `reply.text()` 读取)。
- 终态 `RunEvent::Done` 仅由 `stream` 作为最后一个事件 yield;`run_full` 直接返回
  `RunOutput`,其 `events` 不含 `Done`。
- 两条路径共享同一套事件采集机制:non-streaming 由 facade 内部的
  `collect_traces` + `weave_approval_events`(把审批记录编织回工具/委派事件流)产出;
  streaming 由 `TapToolHandler` / `TapInteractionHandler` 实时 emit,并在终态
  `Done.events` 中用同一审批记录 + `weave_approval_events` 重建完整生命周期序列。二者被上述契约
  与 `facade::agent` 的 parity 回归测试锁定一致。

### 6.3 RunEvent

Facade 事件应比底层 `Notification` 更贴近 UI/CLI:

```rust
#[non_exhaustive]
pub enum RunEvent {
    TextDelta(String),
    ToolStarted(ToolTrace),
    ToolFinished(ToolTrace),
    ApprovalRequested(ApprovalRequest),
    DelegationStarted(DelegationTrace),
    DelegationProgress(DelegationProgress),
    DelegationMessage(DelegationMessage),
    DelegationArtifact(ArtifactRef),
    DelegationFinished(DelegationTrace),
    DelegationFailed(DelegationTrace),
    Escalated(EscalationTrace),
    Done(Box<RunOutput>),

    RawStream(StreamEvent),
    RawNotification(Notification),
}
```

Raw variants 是逃生舱,不应成为简单示例的主路径。`RunEvent` 与 `WireRunEvent` 都是
`#[non_exhaustive]`; UI/CLI 和跨进程宿主在 match 时应保留 fallback arm。

**可序列化投影**:`RunEvent` 本身仍只 `derive(Clone, Debug, PartialEq, Eq)`,不派生
serde(逃生舱 `RawStream` / `RawNotification` 的序列化不作为稳定契约,见 §19)。为让跨进程宿主把事件投给
前端,facade 提供官方的**显式、单向、有损**投影:

```rust
impl RunEvent {
    pub fn to_wire(&self) -> WireRunEvent;
}
impl RunOutput {
    pub fn to_wire(&self) -> WireRunOutput;
}
```

- `WireRunEvent` / `WireRunOutput` 均 `Serialize + Deserialize`(adjacently-tagged
  `{"type":.., "data":..}` 的 snake_case 线格式,与 `agent::Notification` 一致)。
- 归一化变体(`TextDelta` / `ToolStarted` / `ToolFinished` / `ApprovalRequested` /
  `Delegation*` / `Escalated` / `Done`)**如实转发**各自本已 serde 友好的 payload,无损。
- `Done` 内嵌的 `RunOutput` 投影为 `WireRunOutput`,其中 `events` 递归投影为 `Vec<WireRunEvent>`。
- 两个 Raw 逃生舱**降级**为 opaque 标记 `WireRunEvent::Raw(RawEventKind::{Stream, Notification})`——
  只记录哪种逃生舱触发,不承载底层不可序列化载荷,故不把逃生舱序列化提升为稳定契约。

`ApprovalRequest` 也在 M7 富化(见 §9.3),`ApprovalRequested` 投影因此自动携带富化字段。

## 7. Tool Facade

### 7.1 Typed function tool

Facade 应让用户用 typed Rust 函数注册工具,而不是先实现 `ToolRegistry`:

```rust
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct WeatherArgs {
    city: String,
}

async fn get_weather(_ctx: ToolContext, args: WeatherArgs) -> anyhow::Result<String> {
    Ok(format!("{} 晴,26C", args.city))
}

let tool = Tool::function("get_weather", "查询城市天气", get_weather);
```

`Tool::function` 负责:

```text
WeatherArgs -> JSON schema -> ToolDeclaration
serde_json::Value -> WeatherArgs
Result<T> -> provider-neutral ToolResult
```

返回值第一版可支持:

- `String`
- `serde_json::Value`
- 实现 `Serialize` 的结构体
- 显式 `ToolResult`

### 7.2 ToolContext

`ToolContext` 传递运行期上下文:

```rust
pub struct ToolContext {
    pub run_id: RunId,
    pub agent_id: AgentId,
    pub tool_call_id: ToolCallId,
    pub worktree: WorktreeRef,
    pub cancel: CancellationToken,
    pub trace: TraceHandle,
}
```

它不应该暴露可破坏 Conversation 不变量的可变引用。需要写入 blackboard / artifact / mailbox 时,
应通过受控 handle。

### 7.3 逃生到高级 ToolRegistry

高级用户可以直接注入已有 registry:

```rust
Agent::builder()
    .tool_registry(my_registry)
    .tool_declarations(my_declarations);
```

当同时使用 typed tool 与 custom registry 时,构建期必须检查 name 冲突。

## 8. Agent Facade

### 8.1 基础工具 agent

```rust
let mut agent = Agent::builder()
    .provider(ProviderConfig::anthropic_from_env()?)
    .model("databricks-claude-haiku-4-5")
    .system("你可以使用工具,但回答要简洁。")
    .tool(Tool::function("get_weather", "查询城市天气", get_weather))
    .approval(Approval::auto_allow())
    .build()?;

let reply = agent.run("查一下上海天气。").await?;
println!("{}", reply.text());
```

### 8.2 API 形状

```rust
impl Agent {
    pub fn builder() -> AgentBuilder;
    pub fn worker() -> AgentWorkerBuilder;
    pub async fn run(&mut self, input: impl IntoUserMessage) -> Result<Reply, FacadeError>;
    pub async fn run_with_cancel(
        &mut self,
        input: impl IntoUserMessage,
        cancel: CancelHandle,
    ) -> Result<Reply, FacadeError>;
    pub async fn run_full(&mut self, input: impl IntoUserMessage) -> Result<RunOutput, FacadeError>;
    pub async fn run_full_with_cancel(
        &mut self,
        input: impl IntoUserMessage,
        cancel: CancelHandle,
    ) -> Result<RunOutput, FacadeError>;
    pub async fn stream(&mut self, input: impl IntoUserMessage) -> Result<AgentRunStream<'_>, FacadeError>;
    pub async fn stream_with_cancel(
        &mut self,
        input: impl IntoUserMessage,
        cancel: CancelHandle,
    ) -> Result<AgentRunStream<'_>, FacadeError>;
    pub fn reconfigure(&mut self, request: ReconfigRequest) -> Result<(), FacadeError>;

    pub fn conversation(&self) -> &Conversation;
    pub fn state(&self) -> &AgentState;
    pub fn snapshot(&self) -> Result<AgentSnapshot, FacadeError>;
    pub fn restore() -> AgentRestoreBuilder;
    pub fn into_parts(self) -> AgentParts;
}

impl CancelHandle {
    pub fn new() -> Self;
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
}

impl AgentRunStream<'_> {
    pub fn cancel_handle(&self) -> CancelHandle;
    pub fn cancel(&self);
    pub fn interject(&mut self, input: impl IntoUserMessage) -> Result<(), FacadeError>;
    pub fn interject_pivot(&mut self, pivot: PivotMessage) -> Result<(), FacadeError>;
}
```

`Agent::worker()` 用于构造 local subagent 模板。它可以要求更少 provider 配置,允许继承 supervisor
的 provider/model/client,也可以显式指定自己的 model。local subagent 的 `ApprovalPolicy` 仍决定
哪些子工具调用会暂停；managed external delegate 的 runtime permission bridge 仍由其 adapter / session
handler 产生 `NeedInteraction`。当 supervisor 通过 `AgentBuilder::interaction_handler` 注入了异步
`InteractionHandler` 时,local 与 managed external 子交互都会转发给该父级 handler,并在
`Interaction.origin` 中携带 delegate 名与委派深度。未注入父级 handler 时保持旧行为:local 子 agent
使用自己的同步 `FacadeApproval` fallback 应答,headless ask 仍会 deny；external 子 agent 的 permission
prompt 则以明确的 `ExternalAgent` 错误失败,避免静默等待。

managed external delegate 的**启动门**也走同一 attended 通道:当 external-start 的 effective approval
tier 为 ask（例如 `.ask_external_agents()`、`.ask_tool("ask_coder")` 或显式给 `ask_coder` 设置 ask tier）且
supervisor 注入了 `interaction_handler` 时,facade 在 drive layer 构造一个带 `Interaction.origin` 的
`InteractionKind::Approval` 交给该 handler。handler 返回 `Approve` 才启动 delegate；`Deny` / `Timeout` /
`Cancel` 都表面为 `FacadeError::ApprovalDenied`。未注入 handler 时保留同步 fallback:同步
`Approval::ask` handler 可应答,headless ask deny 而不挂起。

`Agent::reconfigure(request)` 是 facade 级运行时配置入口(M2-1)。它接受 facade 重导的
`ReconfigRequest`,并在准入时校验请求:支持 `SetModel`、`SetSystemPromptOverlay`、
`ReplaceToolSet`、`PatchToolSet` 与 `SetLoopPolicy`;skill 三个变体(`ActivateSkill` /
`DeactivateSkill` / `ReplaceActiveSkills`)会以 `FacadeError::InvalidState` 显式拒绝,因为
facade 尚无 skill registry 与 skill-to-prompt/tool 展开层。facade 选择的时机语义比底层 machine
更保守:只在两次 run 之间的 rest cursor(`Idle` / `Done` / `Error` / `CancelRecovery`)接受
reconfig;active 或 parked turn 上调用返回 `InvalidState`,不会 turn 中直接生效。`AgentRunStream`
存活时持有 `&mut Agent`,因此类型系统也阻止同一 agent 被并发 reconfigure。

已准入的 reconfig 仍只在下一 turn 边界应用。`SetModel` 与 `SetSystemPromptOverlay` 会在下一次
LLM request 渲染前落入 `AgentState`;`ReplaceToolSet` / `PatchToolSet` 目前完成声明层准入,live
registry handler 与声明/执行闭包一致性在 M2-2 接线,因此不要把 snapshot/restore 当作运行时改
tools/model/system 的替代路径。

`run` / `run_full` 与 `stream` 都会在调用方提前放弃一次运行时保持 agent 可继续使用:非流式
future 被 drop(例如外层 `tokio::time::timeout` 或 `select!` 分支取消)时,facade 会同步向
底层 machine 发送 never-resume/abandon 输入,丢弃未提交的 pending turn;流式 `AgentRunStream`
被提前 drop 时走同一 abandon 语义。两种路径都回到上一 committed 一致点,因此 timeout/drop
之后可以立即再次 `run` 或 `snapshot`。

`AgentRunStream` 持有发起它的 `Agent` 的运行中 `&mut` 借用,并用本地 drop/cancel guard
保护该借用内的 machine,因此它**不是 `Send`**。调用方应在创建它的同一 task 内 poll/消费该
stream;需要从其他 task 触发取消时,传递 `CancelHandle` 或调用 `cancel_handle()` 克隆出的句柄,
不要尝试把 `AgentRunStream` 本身移入 `tokio::spawn`。

显式取消使用 `CancelHandle`:`run_with_cancel` / `run_full_with_cancel` / `stream_with_cancel`
接收调用方持有的句柄,普通 `run` / `run_full` / `stream` 内部创建未暴露的句柄。调用
`CancelHandle::cancel()` 或 `AgentRunStream::cancel()` 后,底层 driver 在既有有界取消观测点
never-resume 当前 outstanding requirement,工具看到同一个 token (`ToolContext.cancel`),完成后
agent 回到可继续使用的一致点。当前取消在 facade 层仍以 `FacadeError::Agent` 中的取消诊断返回;
专用 `FacadeError::Cancelled` 可作为后续 API 打磨项。

流式 pivot 使用 `AgentRunStream::interject(...)`。由于 `AgentRunStream` 持有 `&mut Agent` 的
运行中 machine 借用,中途注入不能通过 `Agent::interject` 形式同时调用;控制入口放在 stream
对象上。facade 只在 tool phase 关闭、下一次 LLM request 尚未发出时开放一个短边界窗口;
`interject` 在该窗口内排队一个 `Role::User` pivot,下一次 poll 通过下层 `AgentInput::Pivot`
重渲染同一个 LLM requirement。非流式 `run` / `run_full` 仍是一条 future drive 到终态,没有中途喂
输入通道;需要 human-in-the-loop pivot 的调用方应使用 `stream`。

`AgentBuilder` 另有若干**依赖注入口**(Milestone 7,详见 §21):

```rust
impl AgentBuilder {
    // 注入自定义 async InteractionHandler,可在 fulfill 内「发前端 -> await -> 折回」。
    pub fn interaction_handler(self, handler: Arc<dyn InteractionHandler>) -> Self;
}
```

该 handler 是被暂停交互的应答方,但不是 gate:普通 supervisor 工具与 local subagent 工具是否暂停
仍由各自的 `ApprovalPolicy` 决定,managed external permission prompt 是否出现由 runtime adapter / session
policy 决定。local subagent 与 managed external delegate 转发到父级 handler 的交互都会带
`Interaction.origin`,用于 UI 渲染「哪个 delegate 在问」;权限主体仍由 `PermissionRequest.actor` 等具体
请求字段表示。

`snapshot` / `restore` / `into_parts` / `builder` 用途各异,文档不暗示它们可互相替代:

- **需要持久化恢复**用 `snapshot` + `restore`:`snapshot()` 产出 **data-only** 的
  `AgentSnapshot`(会话 + 可序列化 `AgentState` + 协作快照 + delegate recipe),**不**含 client、
  凭据、tool 闭包、approval / interaction handler 等运行期句柄,可安全持久化;`restore()` 经
  `AgentRestoreBuilder` 重注入这些句柄后续跑同一会话(§15.2)。
- **需要接管 live handles**用 `into_parts`:它消费 agent 并把已装配的运行部件按原样交给高级调用方,
  用于直接驱动下层或接管仍存活的句柄。`AgentParts` 交出:`AgentState`(持有 live `Conversation`)、
  `LlmClient`、typed tools 与逃生舱声明、共享 approval bridge 与被注入的 `InteractionHandler`、
  identity source、已注册的 local subagent 与 managed external delegates、`Delegation` 路由模式、
  run budget limits、每个 external delegate 的最近一次 data-only 会话事实
  (`retained_external_sessions`,不含进程句柄 / 凭据),以及 live 协作底座(`Collaboration` config +
  共享 `Mailbox` / `Blackboard` / `Plan` 句柄)。
  它**不会**静默 drop 任何仍有语义价值的字段。它是**拆解逃生舱、不是 restore API**——按原样交出
  live/owned 部件,**不**提供 `AgentParts -> Agent` 的重建 helper;持久化恢复请用 snapshot / restore。
- **需要常规构造**用 `builder`:`Agent::builder()`(或 `Agent::worker()` 构 subagent 模板)从零装配
  一个新 agent,这是默认入口。

### 8.3 内部映射

Agent facade 内部负责装配:

```text
AgentBuilder
  -> AgentSpec
  -> AgentState(Conversation::new)
  -> DefaultAgentMachine
  -> generated RequirementIds + ToolExecutionIds
  -> ReferenceScope(client, registry, interaction handler)
  -> RunContext
  -> drive_turn / drain
```

用户不需要直接看到 `Requirement`。但 `RunOutput.events` 与 raw variants 可以保留底层事实。

### 8.4 Loop policy

保留简单默认值,同时允许覆盖:

```rust
Agent::builder()
    .max_steps(8)
    .max_tool_rounds(4)
    .budget(BudgetLimits::new(None, Some(100_000), None, None))
    .tool_failure_policy(ToolFailurePolicy::ReturnErrorToModel);
```

默认策略建议:

| 参数 | 默认 |
|---|---|
| `max_steps` | 8 |
| `max_tool_rounds` | 4 |
| `budget` | `BudgetLimits::unbounded()` |
| `tool_failure_policy` | `ReturnErrorToModel` |
| `llm_step_mode` | non-streaming,除非调用 `stream` |
| pending failure | cancel pending,回到上一个 committed 一致点 |

`budget(...)` 是 run 级共享 ledger 配置:每次 `run` / `run_full` / `stream` 都用该
`BudgetLimits` 创建新的根 `RunContext`,所以顶层 run 之间计数重置;同一次 run 内的 supervisor、
subagent、managed external delegate 共享同一个 budget handle。LLM response 在回灌 machine 前扣
step 与 usage;预算预检或 charge 超限时未回灌 requirement 以 `NeverResumed` 留痕,当前未提交 turn
被丢弃,facade 返回 `FacadeError::BudgetExhausted`。`AgentRestoreBuilder::budget(...)` 用于恢复后
重新注入同类运行配置;`AgentSnapshot` 是 data-only 状态快照,不携带该运行配置,未设置时恢复 agent
默认 `BudgetLimits::unbounded()`。

## 9. Approval 与权限边界

### 9.1 三档 Approval

Facade 先提供三个简单档位:

```rust
Approval::auto_allow()
Approval::auto_deny()
Approval::ask(handler)
```

工具级别可覆盖:

```rust
Tool::function("shell", "运行命令", run_shell)
    .approval(Approval::ask(cli_approval))
```

Agent 级别可以使用 policy:

```rust
ApprovalPolicy::default()
    .allow_tool("get_weather")
    .ask_tool("shell")
    .ask_external_agents()
    .ask_worktree_write()
```

### 9.2 默认权限语义

建议默认:

| 行为 | 默认 |
|---|---|
| 普通 typed tool | 继承 agent approval policy |
| local subagent 启动 | 不额外审批 |
| local subagent 调用需审批工具 | 仍触发该工具审批 |
| managed external agent 启动 | 需要审批 |
| managed external agent 写工作区 | 需要审批或显式 opt-in |
| managed external agent resume/attach 既有 session | 需要审批 |
| headless 且无 matching policy | deny 或 error,不可静默等待 |

理由:local subagent 仍在同一 effect 模型内,权限会沿工具/interaction 继续受控;managed external agent
可能启动外部 runtime、写文件、运行命令或消耗大量资源,默认必须更保守。

普通 typed tool 的 deny 不会让 `Agent::run` / `run_full` 返回
`FacadeError::ApprovalDenied`:机器会合成一个模型可见的 denied tool result,跳过实际工具执行,
然后继续下一轮模型调用。`ApprovalRequested` 仍会出现在事件序列中,但被拒工具不会产生
`ToolStarted` / `ToolFinished`。`FacadeError::ApprovalDenied` 只用于 managed external delegate
启动前被审批策略拒绝的路径;这类 delegate 尚未进入普通工具执行相位,没有模型可见的 denied
tool result 可以回灌。

managed external delegate 启动审批发生在 drive layer,因为模型可见的 `ask_<name>` start tool 被机器
tool gate 豁免以避免同一次启动双重审批。有效 tier 为 ask 且存在 `AgentBuilder::interaction_handler(..)`
时,启动审批转成异步 `InteractionKind::Approval`,并用 `origin { delegate, depth }` 标注要启动的 delegate；
无异步 handler 时才回落到 `FacadeApproval::resolve_external_start` 的同步 `Approval::ask` / headless deny。
因此 attended 宿主可以用 `.ask_external_agents()` 覆盖全部 external start,也可以用
`.ask_tool("ask_coder")` 只覆盖单个 per-delegate start tool。

### 9.3 富化 ApprovalRequest(Milestone 7)

早期 `ApprovalRequest` 只有 `tool_name`,UI 无法渲染有意义的审批框。M7 借其 `#[non_exhaustive]` **纯加
字段**补齐渲染所需信息:

```rust
#[non_exhaustive]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub call_id: Option<String>,
    pub reason: Option<String>,
    pub input: Option<String>,
}
```

- `input` 用 `Option<String>`(脱敏、限长的紧凑摘要)而非 `serde_json::Value`,以保住整条投影链的 `Eq`
  并天然 redaction 友好:凭据样式 key(`token`/`secret`/`password`/`api_key`/`auth`/… 大小写不敏感子串)
  的值替换为 `<redacted>`,渲染后按 UTF-8 边界截断到上限;`None` 表示无参数。
- `call_id` 为 `Some` 时是 framework tool-call id;父级异步 handler 处理的 external-start 审批同样携带
  start tool 的 framework call id。只有无父级 handler 的同步 fallback external-start 路径用便捷构造
  `ApprovalRequest::for_tool(name)`,其 `call_id` 为 `None`,不再使用空串哨兵。
- 流式路径的 `TapInteractionHandler` 从底层 `InteractionKind::Approval` 的 `call_id` + `ApprovalRequirement`
  填充字段后再 emit `RunEvent::ApprovalRequested`,同步 external-start 路径用便捷构造 `ApprovalRequest::for_tool(name)`。

### 9.4 自定义 permission decider(Milestone 7)

`InteractionKind::Permission` 的裁决默认**一律 deny**。`ApprovalPolicy` 提供注入钩子,让调用方接 AI-based
permission 或自定义逻辑,未注入时保持默认 deny:

```rust
ApprovalPolicy::default()
    .on_permission(|req: &PermissionRequest| -> PermissionResponse {
        // 例如按 req.risk() 决定 allow / deny;返回时以 req.action_id() 盖章。
    })
```

优先级:当经 `Agent::interaction_handler`(§8.2 / §21)注入整体 `InteractionHandler` 时,后者是唯一权威,
decider 仅在无整体 handler 时生效。**facade 只开放注入口,不实现任何 AI 逻辑。**

### 10.1 Local delegate

Subagent 是同库内 child `AgentMachine`,应作为 facade 的 local delegate:

```rust
let reviewer = Agent::worker()
    .model("gpt-5.5")
    .system("你是严格的代码审查 agent,只输出问题和证据。")
    .build()?;

let mut agent = Agent::builder()
    .provider(ProviderConfig::openai_from_env()?)
    .model("gpt-5.5")
    .system("你是主 agent,可以把审查任务交给 reviewer。")
    .subagent("reviewer", reviewer)
    .build()?;
```

Facade 内部映射:

```text
Agent facade
  -> DefaultAgentMachine
  -> ReferenceScope
  -> SubagentHandler
  -> NestedMachine child drain
```

### 10.2 暴露给模型的形式

默认建议把每个 subagent 暴露成单独工具:

```text
ask_reviewer(task)
ask_researcher(task)
ask_fixer(task)
```

相较统一 `delegate(agent, task)`,单独工具更容易让模型正确调用,trace 也更清楚。

高级用户可以改成统一 delegation tool:

```rust
.delegation(Delegation::single_tool("delegate"))
```

统一工具适合 worker 数量动态变化或需要外层 policy 接管路由的场景。

### 10.3 Worker spec

`Agent::worker()` 产物应是 data-first 的 worker spec,而不是已绑定 live client 的完整 session:

```rust
pub struct LocalSubagent {
    pub name: String,
    pub description: String,
    pub spec: AgentSpec,
    pub tools: ToolSetRef,
    pub approval: ApprovalPolicy,
}
```

实际 child `AgentState`、`AgentMachine`、`RunContext` 在 `NeedSubagent` 被兑现时创建。这样 snapshot
和 restore 都更清晰。

## 11. Managed External Agent 集成

### 11.1 External delegate

Managed external agent 不是普通 tool。它是外部 coding-agent runtime 的受管 session,应该作为
external delegate:

```rust
// build_with_default_session_handler 在构造时一步装配 runtime session handler；默认 crate build
// 不含 CLI adapter，未开启对应 external-* feature 时 fail-fast(非密),开启后探测本机已登录 CLI。
let codex = ManagedExternalAgent::codex()
    .worktree("/home/chenxu/repos/my-app")
    .mode(ExternalRunMode::Managed)
    .build_with_default_session_handler()
    .await?;

let mut agent = Agent::builder()
    .provider(ProviderConfig::openai_from_env()?)
    .model("gpt-5.5")
    .system("你是主 coding agent,需要改代码时委托 coder。")
    .external_agent("coder", codex)
    // Headless quick start explicitly allows delegate starts. For attended
    // approval, combine `.ask_external_agents()` with `.interaction_handler(..)`.
    .approval(Approval::auto_allow())
    .build()?;
```

它内部可能需要:

```text
启动 / 恢复 external session
分配 worktree
传入 task brief
等待、轮询、取消
处理 external runtime 的权限请求和澄清问题
读取 artifact、patch、diff、test output
记录 transcript 与 usage
cheap -> strong 升级
verifier 检查
```

这些语义不能压扁成 `Tool::function`。

### 11.2 内部映射

Facade 应把 external delegate 接到现有 external agent 地基:

```text
Agent facade
  -> Delegation policy
  -> agent::external::Dispatcher / Escalator(可选)
  -> NeedSubagent 或 NeedExternalSession
  -> ExternalAgentMachine
  -> ExternalSessionHandler
  -> runtime adapter(Codex / Claude Code / OpenCode / ACP / Custom)
```

runtime adapter 已落地的有:三个私有 wire 的 CLI adapter(Claude Code / Codex / OpenCode,均自主运行；
其中 Claude Code 可产生 host-pausable permission bridge,Codex/OpenCode 当前无 host-answerable
permission bridge)与一个 **ACP adapter**(feature `external-acp`,基于官方 `agent-client-protocol`
crate,以一个标准 ACP client 对接任意 ACP agent——Gemini/OpenCode 原生、Claude/Codex 经 Zed adapter
进程)。ACP adapter 的 `session/request_permission` 映射到 `NeedInteraction`;作为 external delegate
挂在 facade 下时,该 interaction 会 pop 到父级注入的 `InteractionHandler`,带 delegate/depth 归因,应答再经
`RespondInteraction` 回灌 runtime。

如果 external agent 作为 child agent 挂载,推荐仍通过 `NeedSubagent` 进入 `ExternalAgentMachine`。
`ExternalAgentMachine` 内部再发 `NeedExternalSession` 推进真实 runtime。这样它与 local subagent
共享同一套 scope 派生、cancel、budget、trace 与 pop 语义。

**生产级 registry-backed handler(Milestone 7,feature-gated)**:跑真实本地 CLI agent 需要注入
`Arc<dyn ExternalSessionHandler>`,但早期全库只有 test double。M7 提供官方的最后一公里:

```rust
// runtime 无关(不带 feature gate):registry-backed handler,只持有 registry(+ 可选 sink),不持机器状态。
pub struct RegistryExternalSessionHandler { /* Arc<ExternalSessionRegistry> + Option<Arc<dyn ExternalEventSink>> */ }

// feature-gated 便捷构造:按 agent.runtime() 探测 live CLI 并 wire 匹配 adapter 到 registry。
pub async fn default_external_session_handler(
    agent: &ManagedExternalAgent,
) -> Result<Arc<RegistryExternalSessionHandler>, FacadeError>;

// 同上,但额外返回探到的 `Probed` capability view(CLI runtime = Some;ACP = None,能力经 initialize 每会话协商)。
pub async fn default_external_session_handler_with_capabilities(
    agent: &ManagedExternalAgent,
) -> Result<(Arc<RegistryExternalSessionHandler>, Option<ExternalAgentCapabilities>), FacadeError>;
```

- `fulfill` 每次 `registry.get_or_start(..)` 解析 live handle → `handle.advance(..)` 推进一个 decision
  point(`Completed` / `Paused*` / `Failed`),复用既有 capability-gated resume 与 worktree cleanup;launch
  失败与 advance 失败都折叠为 `ExternalSessionResult::Failed`,绝不串到别的 requirement family。
- 宿主 `.session_handler(default_external_session_handler(&agent).await?)` 直接注入;返回具体类型以保留
  `.registry()`(宿主用 `cleanup_agent` / `cleanup` 强制关闭)。更顺手的一步式装配是
  `ManagedExternalAgentBuilder::build_with_default_session_handler().await?`,它在 build 时探测本机
  runtime 并接上同一个 registry-backed handler(已手工 `.session_handler(..)` 时短路 probe、honor 自定义 handler)。
- **probe 结果折入真实能力视图(§11.3)**:一步式装配用
  `default_external_session_handler_with_capabilities` 把 CLI probe 探到的能力集折回 agent 的
  `ExternalAgentCapabilities`,来源标 `CapabilitySource::Probed`,取代构造时的 `Declared` 基线;因为 probed
  档可能比 declared 窄,会**再次**按 probed 视图校验 `ExternalRunMode`(缺能力则 `UnsupportedExternalMode`,
  source 标 `probed`)。ACP 无离线 probe(能力经 `initialize` 每会话协商),保留 declared/negotiated 视图。
- 缺二进制 / 未登录 / 能力不支持时走既有保守路径(probe fail-fast → `FacadeError::ExternalAgent` 或
  `UnsupportedCapability` / skip),**不静默降级**;未编入对应 feature 的 runtime 显式 fail-fast(消息点名要开的 feature)。

### 11.3 能力分级

Managed external agent 的能力取决于 runtime:

| 能力 | facade 表达 |
|---|---|
| 黑盒执行,只返回 summary | `ExternalRunMode::BlackBox` |
| 受管 session,有流式事件和权限请求 | `ExternalRunMode::Managed` |
| 可注入宿主 tools/subagents | `ExternalRunMode::ManagedWithTools` |
| 可 attach/resume 长生命周期 session | `ExternalRunMode::Attachable` |

构建时应检查 `ExternalAgentCapabilities`,不支持的能力要 fail fast 或明确降级。每个能力视图都带
`CapabilitySource`(`Declared` / `Supplied` / `Probed` / `Negotiated`),用 `.source()` 可读出判断依据:
preset 构造为 `Declared`(保守静态基线);`build_with_default_session_handler` 探测成功后折入 `Probed`;
`.capabilities(..)` 存调用方 `Supplied`;ACP `.acp_negotiated(..)` 为 `Negotiated`。宿主用
`ManagedExternalAgent::require_capability(cap)` 针对 agent **当前持有**的视图门禁某个能力,缺失时
返回 `FacadeError::UnsupportedExternalCapability { runtime, capability, capability_source }`(点名能力与来源、
不含 secret);因此 declared 基线声称支持、但 probe 未证实的能力会被正确拒绝(以 probed 为准)。

runtime 到能力档的现状(与 `ExternalRuntimeCapabilities` 8 项对齐):

- 三个 CLI adapter(Claude Code / Codex / OpenCode)自主运行,`host_tools` 为 `false`;Claude Code 可产生
  host-pausable `PausedForInteraction`,Codex/OpenCode 当前无 host-answerable permission bridge。它们经
  `session/update` 式观测提供流式事件与 artifact。
- **ACP adapter** 支持 `permission_bridge=true` 的 runtime(`session/request_permission`),属带权限桥的
  `Managed`;facade external delegate 路径会把该 permission bridge 路由到父级注入的 `InteractionHandler`
  并回灌 `RespondInteraction`。`resume` 取决于 ACP `loadSession` 协商能力;`host_tools`(经 client MCP)为后续能力。因为 ACP
  是标准协议,facade 侧「选哪个 external runtime」与「它能做什么」两件事解耦:runtime 选择决定启动命令,
  能力档由 `initialize` 协商结果填充。

facade 的 `ManagedExternalAgent` 构造器应提供对应入口(如 `ManagedExternalAgent::acp(binary, args)` 或
`::claude_agent_acp()` / `::gemini_acp()` 之类便捷预设),并把协商到的能力如实反映到 `ExternalRunMode` /
`ExternalAgentCapabilities`,不假装未验证的档位。

## 12. 统一 Delegate 抽象

Facade 层应把 local subagent 与 managed external agent 统一成 delegate:

```rust
pub enum Delegate {
    LocalSubagent(LocalSubagent),
    ManagedExternal(ManagedExternalAgent),
}

pub struct DelegateSpec {
    pub name: String,
    pub description: String,
    pub capabilities: Vec<Capability>,
    pub input_policy: DelegateInputPolicy,
    pub output_policy: DelegateOutputPolicy,
    pub approval: ApprovalPolicy,
    pub budget: BudgetLimits,
}
```

Builder 保持友好:

```rust
Agent::builder()
    .subagent("reviewer", reviewer)
    .external_agent("coder", codex);
```

内部可以统一成 `DelegateSpec`,再由 delegation policy 决定如何暴露和调度。

第一版不一定公开 `DelegateBackend` trait。若后续要支持第三方 delegate runtime,可考虑:

```rust
#[async_trait]
pub trait DelegateBackend {
    async fn start(&self, task: DelegateTask) -> Result<DelegateHandle, FacadeError>;
    async fn poll(&self, handle: DelegateHandle) -> Result<DelegateStatus, FacadeError>;
    async fn cancel(&self, handle: DelegateHandle) -> Result<(), FacadeError>;
    async fn resume(
        &self,
        handle: DelegateHandle,
        input: DelegateInput,
    ) -> Result<DelegateStatus, FacadeError>;
}
```

但公开这个 trait 前,应先验证 built-in local/external delegate 的需求是否稳定。

## 13. Delegation 策略

Facade 不应一开始暴露完整调度框架。建议三档:

### 13.1 Model-routed

默认模式。把 delegate 暴露成模型可调用工具:

```rust
.delegation(Delegation::model_routed().expose_as_tools())
```

默认单独工具:

```text
ask_reviewer(task)
ask_coder(task)
```

适合大多数 agent 应用,实现也最贴近现有 tool-use loop。

### 13.2 Rules-routed

由 facade/应用规则决定路由:

```rust
.delegation(
    Delegation::rules()
        .when_task_contains(["fix", "test", "compile"], "coder")
        .when_task_contains(["review", "audit"], "reviewer")
)
```

适合产品侧不希望模型任意启动昂贵 worker 的场景。模型可以不知道 delegate 的存在。

### 13.3 Dispatcher-routed

高级模式,对应 `agent::external::Dispatcher` / `Escalator`:

```rust
.delegation(
    Delegation::dispatcher()
        .primary("cheap-coder")
        .verify_with("verifier")
        .escalate_to("strong-coder")
        .max_attempts(2)
)
```

典型语义:

```text
cheap-coder 先尝试
verifier 检查产物
不通过则升级 strong-coder
最终结果和升级路径进入 DelegationTrace
```

Dispatcher-routed 不应成为第一版默认值。它适合 coding task、长任务、cost-aware 调度和 verifier
闭环。

**注入自定义 evaluator / verifier(Milestone 7)**:早期 dispatcher 把 AI-based routing / verification
接缝写死(`Escalator::new(ScriptedVerifier::passing())` + keyword/`ESCALATE` token)。M7 让 `Delegation`
接受调用方注入的 `TaskEvaluator` / `Verifier`,未注入时**逐字节还原 M5 行为**:

```rust
Delegation::dispatcher()
    .primary("cheap-coder")
    .escalate_to("strong-coder")
    .dispatcher_evaluator(Arc::new(my_task_evaluator)) // 选升级目标
    .dispatcher_verifier(Arc::new(my_verifier))        // 裁决产物是否升级
```

- 注入的 `Verifier` 既回填 `Escalator`(替换写死的 `ScriptedVerifier::passing()`),又作为额外裁决源合流:
  `worker_failed || run_verifier(ESCALATE token) || 注入 verifier 拒绝`,任一为真即升级。
- 注入的 `TaskEvaluator` 从 dispatcher roster(primary + escalate_to)选升级目标;`None` / 选中自身 /
  未注册 delegate 均视为「不升级」。
- 两个钩子存于 `Delegation` 的 `#[serde(skip)]` 运行时字段(配置身份忽略钩子),快照丢弃、回落内置默认,
  与 §19「snapshot 不存闭包/handler」一致。**facade 只开放注入口,不实现任何 AI 逻辑。**

## 14. Collaboration primitives

`agent::collab` 的 plan / blackboard / mailbox 是多 delegate 的协作底座,但 facade 不应要求用户
一开始手写这些原语。

建议默认:

| 场景 | 默认协作能力 |
|---|---|
| 无 delegate | 不启用 collab |
| 一个 delegate,model-routed | mailbox 可选,默认关闭 |
| 多个 delegate | 自动启用 mailbox |
| dispatcher/verifier | 自动启用 plan + blackboard + mailbox |
| managed external agent | 自动启用 artifact store |

可配置:

```rust
.collaboration(
    Collaboration::new()
        .plan()
        .blackboard()
        .mailbox()
        .artifacts()
)
```

外部 runtime 的 `spawn_agent`、`send_message`、`plan_update`、`blackboard_post` 等能力应桥接到
本库 collab primitives,不能直接依赖某个 runtime 私有协议。

## 15. Snapshot / Restore

### 15.1 ChatSession

`ChatSession` snapshot 可以直接使用 `ConversationSnapshot`:

```rust
let snapshot = session.snapshot()?;

let mut session = ChatSession::restore(
    snapshot,
    Chat::builder()
        .provider(ProviderConfig::openai_from_env()?)
        .model("gpt-5.5")
        .build()?,
)?;
```

恢复时重新注入 provider/client。

### 15.2 Agent

Agent facade 需要更大的 snapshot:

```rust
pub struct AgentSnapshot {
    pub supervisor: ConversationSnapshot,
    pub agent_state: AgentStateSnapshot,
    pub delegates: Vec<DelegateSnapshot>,
    pub delegation: Delegation,
    pub pending_delegations: Vec<DelegationSnapshot>,
    pub mailbox: Option<MailboxSnapshot>,
    pub blackboard: Option<BlackboardSnapshot>,
    pub plan: Option<PlanSnapshot>,
    pub artifacts: Vec<ArtifactRef>,
}
```

`delegates` 保存每个已注册 local subagent 的 data-only recipe（`name`/`description`/`spec`/`tools`/
`inherit_model`，不含 approval handler 这类运行期句柄），`delegation` 保存路由模式，restore 时据此
重新广告并路由到相同 subagent。`pending_delegations` 保存进行中 child 的 `ConversationSnapshot`；
同步 one-shot delegation 在单个 supervisor turn 内跑完 child，snapshot 仅在 committed 一致点可取，
故常规 capture 下为空（能力已就绪，供未来可中断 delegation）。task brief 默认不写入持久 snapshot
（R5）。

restore 重新注入 typed tools、escape-hatch declarations/custom registry、local/external delegate runtime
attachments 后，会复用 fresh build 的同一套声明与委托校验：重新注入的 runtime 工具不得与 restored
delegation 合成的 `ask_<name>` / unified delegation tool 重名，rules-routed 与 dispatcher-routed
delegation 也不得引用未注册 delegate。失败在 `AgentRestoreBuilder::build()` 阶段以
`DuplicateTool` 或 `Config` 返回，而不是把不一致的声明面带到下一次 provider request。

`mailbox` / `blackboard` / `plan` 保存已启用协作底座的 data-only snapshot（未启用为 `None`，
带 `#[serde(default)]` 兼容旧格式）。restore 采用 **snapshot 内容为准，topology 只作为兼容旧
snapshot 的 provision hint** 的冲突策略：

- snapshot 带某个协作 slice 时，无论 restore 时 topology 派生的 `Collaboration` 是否启用该底座，
  都优先从 snapshot 恢复该底座及其内容（mailbox 续接 inbox 与 seq 游标，blackboard / plan 保留
  board / plan 身份与消息 / 任务历史）；
- snapshot 缺该 slice 时才回落到 topology：topology 启用但无快照内容（如早于协作 capture 的旧
  snapshot）建空底座并从 ids 铸新身份，topology 也未启用则保持 `None`；
- 恢复出的 `CollabState.config` 会拓宽以覆盖任何由 snapshot 恢复的底座，使
  `Agent::collaboration()` 广告的 flag 与 `mailbox()` / `blackboard()` / `plan()` 访问器返回的
  live 原语始终一致。

顶层 `artifacts` 是**保留兼容字段**，不是行为来源：capture 恒写空、restore 不读取它。之所以不聚合，是
因为当前没有稳定的 facade-level artifact store（`CollabState` 的 artifact store 只是 config flag，delegate
artifact refs 已收进 `RunOutput.artifacts`）。调用方应从两处读取 artifacts：

- per-run：`RunOutput.artifacts`（每次 run 后 surface 的瞬时视图）；
- per-external-delegate：external delegate snapshot 的 `artifacts`（随每个 delegate 的会话事实持久化并在
  restore 时按 delegate 恢复）。

保留该字段（带 `#[serde(default)]`）只为持久化 shape 稳定与向前兼容，不会伪造聚合语义。

Local subagent:

```text
保存 child AgentState / Conversation snapshot
restore 时重建 child machine
```

Managed external agent:

```text
保存 external_session_id、runtime kind、worktree ref、last known status、task brief、
artifact refs、transcript refs
不保存进程句柄、API key、client handle、闭包
restore 时通过 ExternalAgentManager 重新 attach 或标记 interrupted
```

### 15.3 External restore policy

External session restore 必须有明确策略:

```rust
pub enum RestoreExternal {
    AttachOrFail,
    MarkInterrupted,
    RestartFromBrief,
}
```

默认建议 `MarkInterrupted`:外部 coding agent 可能已经改过工作区,盲目重启风险高。调用方可以检查
`RunOutput.delegations` 或 snapshot 状态后决定继续、取消、手动修复或重启。

Restore API:

```rust
let mut agent = Agent::restore()
    .snapshot(snapshot)
    .provider(ProviderConfig::openai_from_env()?)
    .external_agent("coder", codex_manager)
    .subagent("reviewer", reviewer_spec)
    .restore_external(RestoreExternal::MarkInterrupted)
    .build()?;
```

## 16. 错误模型

Facade 错误要少于底层错误,但保留 source:

```rust
pub enum FacadeError {
    Config(ConfigError),
    Client(ClientError),
    Conversation(ConversationError),
    Agent(AgentError),
    Tool(ToolError),
    ApprovalDenied,
    PermissionDenied,
    UnexpectedToolUse,
    LoopLimitExceeded,
    BudgetExhausted,
    UnhandledRequirement(Requirement),
    Delegate(DelegateError),
    ExternalSession(ExternalSessionError),
    Restore(RestoreError),
    InvalidState(String),
}
```

`LoopLimitExceeded` 的分类来自结构化状态,而不是错误消息文本:正常步数上限落在
`LoopCursor::Done(StepLimitReached)`,恢复/错误游标路径则读取 `ErrorCursorKind`。`message`
只作为人类可读诊断,修改措辞不影响 facade 错误类别。

`BudgetExhausted` 来自 run 级 `BudgetLimits` ledger,与 loop policy 的 `LoopLimitExceeded` 分开:
driver 观察到 `LoopCursor::Done(BudgetExhausted)` 后返回该 variant,调用方无需解析底层
`AgentError` 文本即可识别预算耗尽。

`send` / `run` 失败时默认行为建议:

```text
取消当前 pending
Conversation 回到上一个 committed 一致点
错误中携带 source 和可观测 trace
```

高级用户可配置:

```rust
.pending_failure_policy(PendingFailurePolicy::Cancel)
.pending_failure_policy(PendingFailurePolicy::KeepForInspection)
```

默认用 `Cancel`,因为简单 API 最怕失败后 session 半悬挂。`KeepForInspection` 适合调试和测试。

## 17. 完整示例

### 17.1 Stateful chat

```rust
let chat = Chat::builder()
    .provider(ProviderConfig::anthropic_from_env()?)
    .model("databricks-claude-haiku-4-5")
    .system("回答简洁。")
    .build()?;

let mut session = chat.session().build()?;

let first = session.send("解释 agent-lib 的 Client 层。").await?;
let second = session.send("再解释 Conversation 层。").await?;

println!("{}", second.text());
```

### 17.2 Tool agent

```rust
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct WeatherArgs {
    city: String,
}

async fn get_weather(_ctx: ToolContext, args: WeatherArgs) -> anyhow::Result<String> {
    Ok(format!("{} 晴,26C", args.city))
}

let mut agent = Agent::builder()
    .provider(ProviderConfig::openai_from_env()?)
    .model("gpt-5.5")
    .system("你可以使用工具,但回答要简洁。")
    .tool(Tool::function("get_weather", "查询城市天气", get_weather))
    .approval(Approval::auto_allow())
    .build()?;

let output = agent.run_full("查一下上海天气。").await?;

println!("{}", output.reply.text());
println!("{:?}", output.tool_calls);
```

### 17.3 Subagent + managed external agent

```rust
let reviewer = Agent::worker()
    .model("gpt-5.5")
    .system("你是代码审查 agent。只报告具体问题、文件位置和修复建议。")
    .build()?;

let codex = ManagedExternalAgent::codex()
    .worktree("/home/chenxu/repos/my-app")
    .mode(ExternalRunMode::Managed)
    .build_with_default_session_handler()
    .await?;

let mut agent = Agent::builder()
    .provider(ProviderConfig::openai_from_env()?)
    .model("gpt-5.5")
    .system("你是主 coding agent。需要改代码时委托 coder,需要检查时委托 reviewer。")
    .external_agent("coder", codex)
    .subagent("reviewer", reviewer)
    .delegation(
        Delegation::model_routed()
            .expose_subagents_as_tools()
            .expose_external_agents_as_tools(),
    )
    .approval(
        ApprovalPolicy::default()
            .auto_allow_subagents()
            .ask_dangerous_tools(),
    )
    .build()?;

let output = agent.run_full("修复当前 failing tests,然后让 reviewer 检查。").await?;

println!("{}", output.reply.text());

for delegation in output.delegations {
    println!(
        "{}: {:?}, usage={:?}",
        delegation.delegate,
        delegation.status,
        delegation.usage,
    );
}
```

### 17.4 Dispatcher + verifier

```rust
let mut agent = Agent::builder()
    .provider(ProviderConfig::openai_from_env()?)
    .model("gpt-5.5")
    .external_agent("cheap-coder", cheap_coder)
    .external_agent("strong-coder", strong_coder)
    .subagent("verifier", verifier)
    .delegation(
        Delegation::dispatcher()
            .primary("cheap-coder")
            .verify_with("verifier")
            .escalate_to("strong-coder")
            .max_attempts(2),
    )
    .build()?;
```

## 18. 建议落地顺序

1. **Chat facade**
   `ProviderConfig`、`ModelConfig`、`Chat`、`ChatSession`、`Reply`、`RunOutput`、stream、
   snapshot/restore。先覆盖无 tool-use 的路径。

2. **基础 Agent facade**
   typed tool、approval 三档、默认 id source、`DefaultAgentMachine` + `ReferenceScope` wiring、
   `run` / `stream` / `snapshot`。

3. **Local subagent**
   `Agent::worker()`、`.subagent(...)`、model-routed delegation、`DelegationTrace`。
   这一阶段完全复用 `NeedSubagent` / `SubagentHandler` / `NestedMachine`。

4. **Managed external agent**
   `.external_agent(...)`、external session manager 注入、approval defaults、artifact trace、
   restore policy。

5. **Dispatcher / Escalator**
   暴露 dispatcher-routed delegation,支持 cheap -> verifier -> strong 的升级闭环。

6. **Collaboration convenience**
   根据 delegate 拓扑自动启用 mailbox / blackboard / plan / artifact store,并提供显式配置。

7. **宿主嵌入接入面(Milestone 7)**
   在装配层补齐依赖注入口(interaction handler、`RunEvent` 可序列化投影、富化 `ApprovalRequest`、
   生产级 registry-backed `ExternalSessionHandler`、dispatcher evaluator/verifier 与 permission decider),
   让跨进程宿主无需下沉到 agent 层自组 scope。**只开注入口,不实现 AI 逻辑**(详见 §21)。

## 19. 关键设计约束

- Facade 是装配层,不是第二套 runtime。
- `ChatSession` 与 stateful `Agent` 必须内部使用 `Conversation`,不直接拼接 message Vec。
- `Agent` 必须内部使用 `AgentMachine` + effect handler,不绕过 `Requirement`。
- 简单 API 默认 cancel failed pending,高级 API 可保留 pending 供检查。
- Snapshot 不保存 secret、闭包、client、live process handle。
- Local subagent 默认作为 local delegate;managed external agent 默认作为 external delegate。
- Model-routed delegation 默认用每个 delegate 一个工具;统一 `delegate` 工具是高级选项。
- Managed external agent 默认比 local subagent 更保守,启动/resume/write 需要审批或显式 opt-in。
- `RunOutput` 必须能同时表达 LLM response、tool trace、delegation trace、artifact 与 raw events。
- 所有 provider-specific 行为继续通过 provider extras / capability model 显式表达。
- 宿主嵌入注入口(Milestone 7)只在装配层加**依赖注入**:未新增 effect family、未改底层状态机语义,
  注入的 handler/evaluator/verifier/decider 都喂给既有 `InteractionHandler` / `TaskEvaluator` / `Verifier` /
  `InteractionKind::Permission` 接缝;运行时钩子经 `#[serde(skip)]` 在快照中丢弃并回落内置默认。

## 20. 未定问题

1. `Chat::send` 是否保留为 stateful convenience,还是只提供 `Chat::ask` + `ChatSession::send`。
   为避免语义含混,本文倾向后者。

2. `Agent::builder().build()` 返回 `Agent` 还是 `AgentSession`。
   **已定**：当前 crate 采用 stateful `Agent`，不再引入单独 `AgentSession` 包装；`AgentSpec` /
   `AgentState` 保留在下层模块表达静态配置与持久化状态。

3. typed tool 是否在核心 crate 直接依赖 `schemars`。
   如果不想增加核心依赖,可放在 feature 或 companion crate,但 facade 易用性会下降。

4. local subagent 是否默认继承 supervisor provider/model。
   继承更方便,显式配置更可预测。可以允许 `Agent::worker().inherit_model()` 与 `.model(...)`
   两种模式。

5. `DelegationTrace` 中 task brief 是否可能包含敏感信息。
   如果会持久化 snapshot 或日志,需要 redact policy。

6. External restore 默认 `MarkInterrupted` 是否过于保守。
   对 coding agent 是安全默认;对只读 researcher external agent,`AttachOrFail` 可能更合适。

7. `RunEvent` 是否应该保证可序列化。
   UI 进程与测试 cassette 会受益,但 raw notification 中可能包含不易序列化的 source。

   **已定(Milestone 7)**:`RunEvent` 本体**不**派生 serde(保持逃生舱序列化不作为稳定契约,§19),
   改由显式、单向、有损的投影 `RunEvent::to_wire() -> WireRunEvent` / `RunOutput::to_wire() -> WireRunOutput`
   满足跨进程宿主;归一化变体无损,`RawStream` / `RawNotification` 降级为 opaque `RawEventKind` 标记(见 §6.3)。

这些问题不阻塞第一版 Chat/Agent facade,但会影响 public API 一旦稳定后的演进空间。

## 21. 宿主嵌入接入面(Milestone 7)

M7 在 facade 装配层补齐**宿主跨进程嵌入的依赖注入口**,让宿主(如把 agent 嵌入前端/服务的进程)无需
下沉到 agent 层自组 `HandlerScope` + `drain`。核心原则:**只开注入口,零 AI 逻辑**;每个注入口默认值保持
M1–M6 行为,未注入即向后兼容(公开类型均 `#[non_exhaustive]` 加字段)。

对照 PLAN §「Milestone 7」识别的 5 个 facade 写死缺口:

| # | 缺口(facade 写死项) | facade 注入接缝 | 未注入默认(保持 M1–M6) |
|---|---|---|---|
| 1 | 审批 handler 硬编码同步 `FacadeApproval`,宿主无法「发前端→await→折回」 | `Agent::interaction_handler(Arc<dyn InteractionHandler>)`(同步 + 流式两路,§8.2 / §9) | 回退到共享 `FacadeApproval`;与 `.approval(..)` 优先级已写清 |
| 2 | `RunEvent` 整体不可序列化,宿主无法把事件投给前端 | `RunEvent::to_wire()` → `WireRunEvent`、`RunOutput::to_wire()` → `WireRunOutput`(§6.3) | R7 不变:`RunEvent` 本体仍不 serde;`Raw*` 降级为 opaque `RawEventKind` |
| 3 | `ApprovalRequest` 只有 tool name,UI 无法渲染有意义审批框 | 富化 `ApprovalRequest{ tool_name, call_id: Option<String>, reason, input }`(§9.3) | `for_tool()` 保留同步 external-start 路径,其 `call_id` 为 `None` |
| 4 | 无生产级 live-adapter-backed `ExternalSessionHandler`(全库仅 test double) | `default_external_session_handler()` + `RegistryExternalSessionHandler`(§11.2,feature-gated) | feature 关闭时不透出;宿主 `.session_handler(default_..)` 直接用 |
| 5 | AI 路由/权限接缝写死(`ScriptedVerifier::passing()` / 权限默认 deny) | `Delegation::dispatcher_evaluator/verifier(..)`(§13.3)+ `ApprovalPolicy::on_permission(..)`(§9.4) | 未注入逐字节还原 M5 dispatcher 与「权限默认 deny」 |

一致性:M7 触碰到的每条 §19 约束均成立——仍是装配层、无新 effect family、不改底层语义;运行时钩子
(dispatcher hooks、permission decider、interaction handler)经 `#[serde(skip)]` 在快照中丢弃并回落内置
默认,与「snapshot 不存闭包/handler」一致。`prelude` 透出 `ApprovalRequest` / `WireRunEvent` / `WireRunOutput`
(见 §3)。

**restore 路径对齐(M7-F1)**:缺口 1 的注入口在 `Agent::restore()` 路径上同样开放——
`AgentRestoreBuilder::interaction_handler(Arc<dyn InteractionHandler>)` 与 `AgentBuilder::interaction_handler`
签名、优先级(相对 `.approval(..)`)、同步 + 流式两路生效完全对齐。snapshot 是 data-only、不携带该运行期
句柄(§15.2),故恢复出的会话须重注入才能保持宿主的跨进程审批往返;未重注入即回落到同步 `FacadeApproval`,
与恢复前行为一致(向后兼容)。
