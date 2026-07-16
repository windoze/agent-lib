# agent-lib

`agent-lib` 是一个面向 LLM API 的 Rust 基础库。它用 provider-neutral 的数据模型承载消息、
内容块、工具调用和 token usage，并在此之上提供三层可组合的能力：

- **Client 层** —— 把 Anthropic Messages 与 OpenAI Responses 的 wire 格式统一成同一套请求 /
  响应 / 流式事件，上层代码只依赖 dyn-safe 的 `LlmClient`，不感知具体厂商。
- **Conversation 层** —— 以强类型 identity、不可变消息 envelope 和唯一的 pending 事务，
  把一次会话建模成可校验、可分支、可投影 / 压缩、可快照恢复的历史。
- **Agent 层** —— 在 Conversation 之上提供 sans-io 的状态机（`AgentMachine`），把每个副作用
  reify 成可寻址的 `Requirement`，由 driver 兑现后折回同一个 Conversation。

保留 provider 原始值与尚未建模的字段，是贯穿三层的一个设计原则：上层逻辑永远不需要绑定
特定厂商的 wire 细节。

## 模块概览

| 模块 | 作用 |
| --- | --- |
| `model` | 完整态的消息、多模态内容块、工具 schema、token usage、归一化枚举，以及保留未建模字段的逃生舱。 |
| `stream` | 稳定 block id、归一化 delta，以及把增量事件折叠回完整 `Response` 的统一 `Accumulator`。 |
| `client` | `EndpointConfig`、认证、结构化 capability、分类错误，以及 dyn-safe 的 `LlmClient` trait。 |
| `adapter` | Anthropic Messages 与 OpenAI Responses 的 HTTP / SSE 适配器。 |
| `conversation` | 强类型 identity、`Conversation`、`PendingTurn` 事务、`Boundary`、fork、projection / compaction、snapshot / restore。 |
| `agent` | data-only 的 Agent 配置与状态、sans-io `AgentMachine`、`Requirement` 副作用模型和参考 driver;`agent::collab` 提供 plan / blackboard / mailbox 协作原语与桥接工具 adapter;`agent::external` 提供外部 coding-agent 会话、混合调度器(`Dispatcher`)与 cheap→strong 升级 / verifier(`Escalator`)。 |

## 安装

需要支持 Rust 2024 edition 的稳定版工具链。在同一工作区中作为 path dependency 使用：

```toml
[dependencies]
agent-lib = { path = "../agent-lib" }
```

## 快速开始：Client 层

通过 dyn-safe 的 `LlmClient` 发起一次完整（非流式）响应请求。调用 `chat` 时 `stream`
必须为 `false`；`chat_stream` 时必须为 `true`，流式事件可交给统一的 `Accumulator` 折叠成
同一个 `Response` 类型。

```rust
use agent_lib::{
    adapter::anthropic::AnthropicAdapter,
    client::{AuthScheme, ChatRequest, EndpointConfig, LlmClient},
    model::{
        content::ContentBlock,
        message::{Message, Role},
    },
};
use serde_json::Map;
use std::{env, error::Error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let endpoint = EndpointConfig {
        base_url: env::var("ANTHROPIC_BASE_URL")?,
        auth: AuthScheme::Bearer(env::var("ANTHROPIC_AUTH_TOKEN")?),
        query_params: Vec::new(),
        extra_headers: vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())],
    };
    let client: Box<dyn LlmClient> = Box::new(AnthropicAdapter::new(endpoint));
    let request = ChatRequest {
        model: env::var("ANTHROPIC_MODEL")
            .unwrap_or_else(|_| "databricks-claude-haiku-4-5".to_owned()),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "用一句话解释归一化 Client 层。".to_owned(),
                extra: Map::new(),
            }],
        }],
        tools: Vec::new(),
        system: Some("回答简洁。".to_owned()),
        max_tokens: 128,
        temperature: None,
        stream: false,
        provider_extras: None,
    };

    let response = client.chat(request).await?;
    println!("stop={:?}, usage={:?}", response.stop_reason, response.usage);
    Ok(())
}
```

`EndpointConfig` 只描述传输端点，adapter 负责附加协议路径（Anthropic 的 `/v1/messages`、
OpenAI Responses 的 `/responses`）并做 wire 转换。认证可用 Bearer、任意自定义 header 或
`None`。注意它包含凭据，不应写入日志或未经批准的持久化存储。

## 快速开始：Conversation 层

`Conversation` 用调用方提供的稳定 UUID 与完整 Client message 组成冻结 envelope，
system prompt 单独保存在配置里，不会被包装成 `Role::System` 历史消息。

```rust
use agent_lib::conversation::{Conversation, ConversationConfig, ConversationId};

let conversation_id: ConversationId = "018f0d9c-7b6a-7c12-8f31-1234567890ab"
    .parse()
    .expect("valid externally supplied id");
let conversation = Conversation::new(
    conversation_id,
    ConversationConfig::new(Some("回答简洁。".to_owned())),
);

assert_eq!(conversation.id(), conversation_id);
assert_eq!(conversation.config().system(), Some("回答简洁。"));
assert!(conversation.turns().is_empty());
assert!(conversation.pending().is_none());
```

历史只能通过唯一的 pending 事务推进，公开 API 不暴露裸 message/turn push。一次典型的
文本往返是：`begin_turn` 放入完整 user payload → `start_assistant_response` 冻结 assistant →
`finish_assistant` → `commit_pending`。

```rust
use agent_lib::{
    client::Response,
    conversation::{
        AssistantFinish, Conversation, ConversationError, MessageId, TurnId, TurnMeta,
    },
    model::message::{Message, Role},
};

fn commit_text_response(
    conversation: &mut Conversation,
    turn_id: TurnId,
    user_message_id: MessageId,
    assistant_message_id: MessageId,
    response: Response,
) -> Result<TurnId, ConversationError> {
    conversation.begin_turn(
        turn_id,
        user_message_id,
        Message { role: Role::User, content: Vec::new() },
    )?;
    conversation.start_assistant_response(response)?;
    let outcome = conversation.finish_assistant(assistant_message_id)?;
    assert_eq!(outcome, AssistantFinish::ReadyToCommit);
    conversation.commit_pending(TurnMeta::default())
}
```

带工具的响应会返回 `AssistantFinish::RequiresToolCallMappings`，需用 `register_tool_calls`
与 `append_tool_response` 闭合本轮 call 后再继续；只有不含 tool-use 的最终 assistant 才能
`commit_pending`。所有提交都会通过同一套 I1–I4 canonical 校验，失败时原 Conversation
结构不变。

### 主要能力

- **Boundary**：Turn 切割使用 Conversation 签发的 `Boundary` 而非裸 `usize`。可序列化传递，
  但消费前必须由当前 Conversation 校验 owner / version / 锚点 / 范围 / pending 一致点。
- **revert / redo**：`revert_to` 无损移动逻辑 head，raw Turn 与 immutable message 保持不变，
  旧 head 可作为 redo token。
- **fork**：`fork_at` 从合法 boundary 创建 child Conversation，O(1) 共享 fork 点之前的
  immutable prefix，父子随后独立推进。
- **projection / compaction**：`CompactionPlan` 只替换 projection overlay，raw history 不被改写；
  `effective_view` 渲染 Client-ready 的 system prompt 与投影后消息。
- **snapshot / restore**：`Conversation::snapshot` 只在无 pending 的 committed 一致点导出
  data-only facts，`Conversation::restore` 重新校验后恢复。

完整设计见 [`docs/conversation-core.md`](docs/conversation-core.md)。

## Agent 层

Agent 层在 Conversation 之上提供 data-only 的强类型 identity、静态 `AgentSpec` 配置和
可恢复的 `AgentState`，核心是 sans-io 的 `AgentMachine`：

- `step` 契约把每个副作用（LLM 请求、工具执行、审批、reconfigure 等）reify 成可寻址的
  `Requirement`；driver 兑现后经 `StepInput::Resume` 把 `RequirementResult` 折回同一个活动
  `Conversation`。
- `DefaultAgentMachine` 实现了文本与 tool turn 的完整状态机：请求非流式 / 流式 Client 生成、
  折叠进 pending、执行 provider-neutral 工具并回灌 result，在无 tool-use 的 final assistant
  后提交 Turn。
- `agent::drive` 是参考 driver，把这些 requirement 兑现到 live 的 `LlmClient` /
  `ToolRegistry` / interaction 后端；`NestedMachine` 与 `SubagentHandler` 把同一 pull/pop
  机制扩展到父子 agent 树。

设计详见 [`docs/agent-layer.md`](docs/agent-layer.md) 与
[`docs/agent-effect-model.md`](docs/agent-effect-model.md)。

### 用一组 scoped effect 构造一个 agent

这是本库最不直观的部分,值得单独说明。`AgentMachine` 本身**不做任何 IO**——它不会去调
LLM、不会执行工具、不会弹审批框。`step` 每次只把"我现在需要什么"表达成一个可寻址的
`Requirement`(如 `NeedLlm`、`NeedTool`、`NeedInteraction`、`NeedReconfigRegistry`),然后
**停下来**等外部兑现。真正干活的是 driver:它按 requirement 家族找到对应 handler,`await`
真实后端,把结果包成 `RequirementResult` 经 `StepInput::Resume` 折回机器,如此往复直到本轮
结束。

"scoped effect"指的就是:一组 handler 打包成一个 `HandlerScope`(每个副作用家族一个),
一层 scope 就是一个 drain layer。scope 只处理它声明能处理的家族,**处理不了的 requirement
会 pop 到外层 scope**;顶层还没人处理就报 `UnhandledRequirement`。这带来两个直接好处:

- **run mode 变成 scope 的接线方式**。`ReferenceScope` 的 interaction handler 是可选的:挂上
  就是 *attended*(审批在本层解决),不挂就是 *headless*(审批 pop 到外层)。
- **父子 agent 只是 scope 的嵌套**。`SubagentHandler` 兑现 `NeedSubagent` 时会开一层新的
  drain 驱动子机器;子机器自己 scope 处理不了的 requirement(比如 headless 子 agent 的审批)
  会 pop 回发起 `NeedSubagent` 的那层去解决。

下面用官方参考实现 `ReferenceScope`(把 `LlmClient` + `ToolRegistry` + 审批后端接成一层
total scope)把一整轮 turn 跑完。`client`、`registry` 是你实现的 `LlmClient` /
`ToolRegistry`;`ids` 是你实现的 `RequirementIds` + `ToolExecutionIds` 身份源:

```rust
use agent_lib::agent::{
    AgentInput, AgentSpec, AgentState, ApprovalInteractionHandler, BudgetLimits,
    DefaultAgentMachine, LlmStepMode, LoopPolicy, ModelRef, ReferenceScope, RunContext,
    ToolFailurePolicy, ToolSetRef, WorktreeRef, drive_turn,
};
use agent_lib::conversation::{Conversation, ConversationConfig};
use agent_lib::model::message::{Message, Role};
use std::num::NonZeroU32;
use std::sync::Arc;

async fn run_one_turn(
    client: Arc<dyn agent_lib::client::LlmClient>,
    registry: Arc<dyn agent_lib::agent::ToolRegistry>,
    ids: Arc<MyIds>, // 实现 RequirementIds + ToolExecutionIds
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. 静态配置:worktree、system prompt、初始 tool set、model、loop policy。
    let spec = AgentSpec::new(
        ids.agent_id(),
        WorktreeRef::new("/repo/my-agent"),
        Some("You are a helpful agent.".to_owned()),
        ToolSetRef::new(ids.tool_set_id(), registry.declarations()),
        ModelRef::new("gpt-5.5", NonZeroU32::new(512).unwrap(), Some(0.1), None),
        LoopPolicy::new(
            NonZeroU32::new(8).unwrap(),
            NonZeroU32::new(4).unwrap(),
            ToolFailurePolicy::ReturnErrorToModel,
        ),
    );

    // 2. 状态:持有唯一活动 Conversation 的 AgentState。
    let state = AgentState::new(
        spec,
        Conversation::new(ids.conversation_id(), ConversationConfig::new(None)),
    );

    // 3. sans-io 机器:只 reify effect,不做 IO。
    let mut machine = DefaultAgentMachine::new(state, LlmStepMode::NonStreaming, ids.clone())
        .with_tool_execution_ids(ids.clone());

    // 4. 一层 scope:把 LLM / 工具 / 审批后端接成 handler 集合。
    //    挂上 interaction handler => attended;去掉 .with_interaction 即 headless。
    let scope = ReferenceScope::new(client, registry)
        .with_interaction(ApprovalInteractionHandler::approve());

    // 5. 横切上下文:取消、预算、trace。
    let ctx = RunContext::new_root(ids.run_id(), BudgetLimits::unbounded(), ids.trace_root());

    // 6. driver 把机器 reify 的每个 requirement 兑现到 scope,直到本轮结束。
    let input = AgentInput::user_message(
        ids.turn_id(),
        ids.message_id(),
        Message { role: Role::User, content: Vec::new() },
        ids.message_id(),
        ids.step_id(),
    )?;
    let done = drive_turn(&mut machine, input, &scope, &ctx).await?;

    println!("turn ended at {:?}", done.cursor());
    Ok(())
}
```

要点回顾:机器负责*决定*要做什么(纯状态),scope 负责*怎么做*(真实 IO),两者通过
`Requirement` / `RequirementResult` 解耦。想换 provider、换审批策略、加子 agent,都只是换
scope 里的 handler 或多套一层 scope,机器代码一行不动。参考 driver 的完整语义(pop 路由、
嵌套、cancel)见 [`docs/agent-effect-model.md`](docs/agent-effect-model.md)。

一个接真实 provider、端到端跑通"对话 + 需审批的工具"的完整可运行示例见
[`examples/agent_chat.rs`](examples/agent_chat.rs)(下方[可运行示例](#可运行示例)有运行方式):
它自己实现了 `RequirementIds`/`ToolExecutionIds` 身份源、一个 mock 的 `get_weather` 工具、
一个要求审批的 `ToolApprovalPolicy`,以及一个从 stdin 读取放行/拒绝的 `InteractionHandler`,
最后把它们组合成一层自定义 `HandlerScope` 用 `drain` 驱动。

## 数据模型与逃生舱

内容块的 `extra` 会保留尚未建模的 provider 字段，`ProviderExtras` 则让 provider 专属请求
参数绑定目标 provider、只在匹配时合并。

```rust
use agent_lib::model::extras::{ProviderExtras, ProviderExtrasMergeOutcome, ProviderId};
use serde_json::{Map, json};

let extras = ProviderExtras {
    provider: ProviderId::Anthropic,
    fields: Map::from_iter([("top_k".to_owned(), json!(20))]),
};
let mut body = json!({ "model": "claude-example" });

let outcome = extras
    .merge_into(&mut body, ProviderId::Anthropic)
    .expect("merge matching provider extras");
assert_eq!(outcome, ProviderExtrasMergeOutcome::Merged);
assert_eq!(body["top_k"], json!(20));
```

## 可运行示例

`conversation_core` 完全离线，无需任何环境变量：

```bash
cargo run --example conversation_core
```

设置下表环境变量后，三个 Client endpoint 示例可原样切换 Anthropic 或 OpenAI Responses：

```bash
export AGENT_LIB_PROVIDER=anthropic # 或 openai
cargo run --example non_streaming
cargo run --example streaming_typewriter
cargo run --example tool_round_trip
```

Agent 端到端示例(交互式对话 + 需审批的 mock 工具)用同一组环境变量：

```bash
cargo run --example agent_chat
```

- `non_streaming`：通过 `Box<dyn LlmClient>` 获取完整的 normalized `Response`。
- `streaming_typewriter`：收到 `Delta::Text` 即刷新 stdout，同时把事件送入公共 `Accumulator`
  校验可折叠的完整响应。
- `tool_round_trip`：声明 `get_weather` schema，读取模型的统一 `ToolUse`，模拟本地执行后
  用同一 call id 回灌 `ToolResult`。
- `conversation_core`：用本地 fixture 演示 identity、pending/commit/cancel、Boundary/fork、
  projection/effective view 和 snapshot/restore。
- `agent_chat`：接真实 provider 的 `AgentMachine` + 自定义 scoped-effect driver 端到端演示——
  行输入的多轮对话、mock 的 `get_weather` 工具、逐次审批(stdin 放行/拒绝的 `InteractionHandler`),
  输入 `/quit` 退出并打印整段会话的 token 统计。

每个 endpoint 示例为单次 HTTP 操作配置 45 秒 timeout，缺少变量或 provider 非法时会给出
不含 secret 的明确错误。

### Endpoint 环境变量

| 变量 | Anthropic | OpenAI Responses |
| --- | --- | --- |
| provider 选择 | `AGENT_LIB_PROVIDER=anthropic` | `AGENT_LIB_PROVIDER=openai` |
| base URL | `ANTHROPIC_BASE_URL`（必填） | `OPENAI_BASE_URL`（必填） |
| 凭据 | `ANTHROPIC_AUTH_TOKEN`（Bearer） | `OPENAI_API_KEY`（`api-key` header） |
| model | `ANTHROPIC_MODEL`（默认 `databricks-claude-haiku-4-5`） | `OPENAI_MODEL`（默认 `gpt-5.5`） |
| API version | `ANTHROPIC_VERSION`（默认 `2023-06-01` header） | `OPENAI_API_VERSION`（默认 `2025-04-01-preview` query） |

其他部署若使用 `x-api-key` 或标准 OpenAI 认证，直接构造相应的 `AuthScheme` 与
`EndpointConfig` 即可，无需修改 adapter。

## 构建与测试

```bash
cargo build
cargo test --all --all-targets
cargo doc --no-deps --open
```

提交前建议依次运行：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

真实 endpoint 测试默认标记为 `#[ignore]`，配置好上述凭据后可显式运行：

```bash
cargo test --test integration_anthropic -- --ignored --nocapture
cargo test --test integration_openai_resp -- --ignored --nocapture
cargo test --test integration_normalization -- --ignored --nocapture
```

复杂 mock 测试套件（多轮、approval、subagent、plan/blackboard、cancel、pivot 的组合边界）默认离线、
可单独过滤运行，设计与落地状态见 [`docs/complex-tests.md`](docs/complex-tests.md)：

```bash
cargo test --test agent_complex_support    # mock plan/blackboard 支持层与断言 helper
cargo test --test agent_complex_flow       # 多轮 + approve/deny + plan 依赖 + pivot
cargo test --test agent_complex_subagent   # subagent pop、pivot 后重渲染 brief
cargo test --test agent_complex_cancel     # cancel never-resume、approval vs context cancel
```

## 参考文档

- [`DESIGN.md`](DESIGN.md) —— 完整设计。
- [`docs/conversation-core.md`](docs/conversation-core.md) —— Conversation 层设计。
- [`docs/agent-layer.md`](docs/agent-layer.md)、[`docs/agent-effect-model.md`](docs/agent-effect-model.md)
  —— Agent 层 sans-io + effect-handler 模型。
- [`docs/capability-matrix.md`](docs/capability-matrix.md) —— provider 能力差异与实测范围。
