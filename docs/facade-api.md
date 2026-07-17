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
   `ChatSession`、`AgentSession`、delegating agent session 都应支持 snapshot/restore。
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
    Agent, AgentSession, Approval, ApprovalPolicy, Chat, ChatSession, Delegation,
    ManagedExternalAgent, ModelConfig, ProviderConfig, Reply, RunEvent, RunOutput, RunStream,
    Tool, ToolContext,
};
```

不建议把 `AgentMachine`、`Requirement`、`Boundary`、adapter 细节放进 prelude。需要这些能力的用户,
应显式从原模块导入。

Facade 分三层能力:

| 层级 | 类型 | 适用场景 | 用户需要理解 |
|---|---|---|---|
| Level 1 | `Chat` / `ChatSession` | one-shot 或多轮聊天 | provider、model、system、stream |
| Level 2 | `Agent` / `AgentSession` | 工具调用、审批、loop policy | tool、approval、run output |
| Level 3 | `Agent` + delegation | subagent / managed external agent / dispatcher | delegate、policy、trace、snapshot |

第一版可以只公开一个 `Agent::builder()`,通过是否配置 tool / delegate 进入 Level 2/3。文档上仍应按三层讲。

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
    .temperature(0.2);
```

Builder 上保留短写:

```rust
Agent::builder()
    .model("gpt-5.5")
    .max_tokens(1024)
    .temperature(0.2);
```

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

### 6.3 RunEvent

Facade 事件应比底层 `Notification` 更贴近 UI/CLI:

```rust
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
    Done(RunOutput),

    RawStream(StreamEvent),
    RawNotification(Notification),
}
```

Raw variants 是逃生舱,不应成为简单示例的主路径。

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
    pub async fn run_full(&mut self, input: impl IntoUserMessage) -> Result<RunOutput, FacadeError>;
    pub async fn stream(&mut self, input: impl IntoUserMessage) -> Result<RunStream, FacadeError>;

    pub fn conversation(&self) -> &Conversation;
    pub fn state(&self) -> &AgentState;
    pub fn snapshot(&self) -> Result<AgentSnapshot, FacadeError>;
    pub fn restore() -> AgentRestoreBuilder;
    pub fn into_parts(self) -> AgentParts;
}
```

`Agent::worker()` 用于构造 local subagent 模板。它可以要求更少 provider 配置,允许继承 supervisor
的 provider/model/client,也可以显式指定自己的 model。

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
    .tool_failure_policy(ToolFailurePolicy::ReturnErrorToModel);
```

默认策略建议:

| 参数 | 默认 |
|---|---|
| `max_steps` | 8 |
| `max_tool_rounds` | 4 |
| `tool_failure_policy` | `ReturnErrorToModel` |
| `llm_step_mode` | non-streaming,除非调用 `stream` |
| pending failure | cancel pending,回到上一个 committed 一致点 |

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

## 10. Subagent 集成

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
let codex = ManagedExternalAgent::codex()
    .worktree("/home/chenxu/repos/my-app")
    .mode(ExternalRunMode::Managed)
    .build()?;

let mut agent = Agent::builder()
    .provider(ProviderConfig::openai_from_env()?)
    .model("gpt-5.5")
    .system("你是主 coding agent,需要改代码时委托 coder。")
    .external_agent("coder", codex)
    .approval(ApprovalPolicy::default().ask_external_agents())
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

runtime adapter 已落地的有:三个私有 wire 的 CLI adapter(Claude Code / Codex / OpenCode,均自主运行、
无 permission bridge)与一个 **ACP adapter**(feature `external-acp`,基于官方 `agent-client-protocol`
crate,以一个标准 ACP client 对接任意 ACP agent——Gemini/OpenCode 原生、Claude/Codex 经 Zed adapter
进程)。ACP adapter 是首个支持 permission bridge 的 runtime(`session/request_permission` 映射到
`NeedInteraction`),facade 的 `ManagedExternalAgent` 构造器因此应能表达「ACP 后端 + permission bridge
可用」这一档(见 §11.3)。

如果 external agent 作为 child agent 挂载,推荐仍通过 `NeedSubagent` 进入 `ExternalAgentMachine`。
`ExternalAgentMachine` 内部再发 `NeedExternalSession` 推进真实 runtime。这样它与 local subagent
共享同一套 scope 派生、cancel、budget、trace 与 pop 语义。

### 11.3 能力分级

Managed external agent 的能力取决于 runtime:

| 能力 | facade 表达 |
|---|---|
| 黑盒执行,只返回 summary | `ExternalRunMode::BlackBox` |
| 受管 session,有流式事件和权限请求 | `ExternalRunMode::Managed` |
| 可注入宿主 tools/subagents | `ExternalRunMode::ManagedWithTools` |
| 可 attach/resume 长生命周期 session | `ExternalRunMode::Attachable` |

构建时应检查 `ExternalAgentCapabilities`,不支持的能力要 fail fast 或明确降级。

runtime 到能力档的现状(与 `ExternalRuntimeCapabilities` 8 项对齐):

- 三个 CLI adapter(Claude Code / Codex / OpenCode)自主运行,`permission_bridge` / `host_tools` 为
  `false`,对应 `ExternalRunMode::Managed` 但**无**权限桥;它们经 `session/update` 式观测提供流式事件与
  artifact。
- **ACP adapter** 是首个 `permission_bridge=true` 的 runtime(`session/request_permission`),属带权限桥的
  `Managed`;`resume` 取决于 ACP `loadSession` 协商能力;`host_tools`(经 client MCP)为后续能力。因为 ACP
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

### 15.2 AgentSession

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
    UnhandledRequirement(Requirement),
    Delegate(DelegateError),
    ExternalSession(ExternalSessionError),
    Restore(RestoreError),
    InvalidState(String),
}
```

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
    .build()?;

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
            .ask_external_agents()
            .ask_dangerous_tools(),
    )
    .build()?;

let output = agent.run_full("修复当前 failing tests,然后让 reviewer 检查。").await?;

println!("{}", output.reply.text());

for delegation in output.delegations {
    println!(
        "{}: {:?}, usage={:?}",
        delegation.worker,
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

## 19. 关键设计约束

- Facade 是装配层,不是第二套 runtime。
- `ChatSession` 与 `AgentSession` 必须内部使用 `Conversation`,不直接拼接 message Vec。
- `Agent` 必须内部使用 `AgentMachine` + effect handler,不绕过 `Requirement`。
- 简单 API 默认 cancel failed pending,高级 API 可保留 pending 供检查。
- Snapshot 不保存 secret、闭包、client、live process handle。
- Local subagent 默认作为 local delegate;managed external agent 默认作为 external delegate。
- Model-routed delegation 默认用每个 delegate 一个工具;统一 `delegate` 工具是高级选项。
- Managed external agent 默认比 local subagent 更保守,启动/resume/write 需要审批或显式 opt-in。
- `RunOutput` 必须能同时表达 LLM response、tool trace、delegation trace、artifact 与 raw events。
- 所有 provider-specific 行为继续通过 provider extras / capability model 显式表达。

## 20. 未定问题

1. `Chat::send` 是否保留为 stateful convenience,还是只提供 `Chat::ask` + `ChatSession::send`。
   为避免语义含混,本文倾向后者。

2. `Agent::builder().build()` 返回 `Agent` 还是 `AgentSession`。
   若 `Agent` 本身有状态,名字简单但语义容易和 spec 混合;若拆成 `Agent` / `AgentSession`,
   类型更准确但入口更长。

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

这些问题不阻塞第一版 Chat/Agent facade,但会影响 public API 一旦稳定后的演进空间。
