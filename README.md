# agent-lib

`agent-lib` 是一个面向 LLM API 的 provider-neutral Rust 库。它用统一的数据模型承载消息、
内容块、工具调用与 token usage，并在此之上分四层逐级构建能力——从底层的厂商无关 client，
到可校验/可分支/可快照的会话，再到 sans-io 的 agent 状态机，最后是 batteries-included 的
facade 装配层：

- **Client 层** —— 把 Anthropic Messages 与 OpenAI Responses 的 wire 格式统一成同一套请求 /
  响应 / 流式事件，上层只依赖 dyn-safe 的 `LlmClient`，不感知具体厂商。
- **Conversation 层** —— 以强类型 identity、不可变消息 envelope 和唯一的 pending 事务，把一次
  会话建模成可校验、可分支、可投影 / 压缩、可快照恢复的历史。
- **Agent 层** —— 在 Conversation 之上提供 sans-io 状态机（`AgentMachine`），把每个副作用
  reify 成可寻址的 `Requirement`，由 driver 兑现后折回同一个 Conversation；`agent::external`
  再把外部 coding-agent CLI、混合调度器（`Dispatcher`）与 cheap→strong 升级 / verifier
  （`Escalator`）纳入同一 pull/pop 模型。
- **Facade 层** —— 在上述三层之上的装配层（`agent_lib::facade` + `agent_lib::prelude`），让常见的
  聊天、工具 agent、subagent、managed external agent、dispatcher 场景不必手写 pending 事务、
  `AgentMachine`、`HandlerScope` 与 driver wiring。它内部仍复用 `Conversation` +
  `DefaultAgentMachine` + `Requirement`，**不绕过底层不变量**。

保留 provider 原始值与尚未建模的字段，是贯穿各层的设计原则：上层逻辑永远不需要绑定特定厂商的
wire 细节。

## 安装

需要支持 Rust 2024 edition 的稳定版工具链。在同一工作区中作为 path dependency 使用：

```toml
[dependencies]
agent-lib = { path = "../agent-lib" }
```

可选 feature（默认全部关闭）：

| Feature | 作用 |
| --- | --- |
| `facade-schema` | 为 `Tool::function` 派生 JSON schema（依赖 `schemars`）；不开则用 `Tool::function_with_schema` 显式给 schema。 |
| `external-claude-code` / `external-codex` / `external-opencode` | 受管 CLI adapter（Claude Code / Codex / OpenCode）。 |
| `external-acp` | 受管 ACP（Agent Client Protocol）adapter，唯一带 permission bridge 的 runtime。 |

## Quick Start（Facade 层）

大多数应用应从 `agent_lib::facade` 入手。它按「渐进式使用」逐层加概念：从 one-shot `Chat`，到
有状态多轮 `ChatSession`，再到会调用工具的 `Agent`，最后到委托外部 coding-agent 的 external
agent。所有入口内部仍复用 `Conversation` + `DefaultAgentMachine`，不绕过底层不变量。

### 1. Chat —— 一次性问答（无状态）

```rust
use agent_lib::facade::{Chat, ProviderConfig};
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let chat = Chat::builder()
        .provider(ProviderConfig::openai_from_env()?)
        .model("gpt-5.5")
        .system("Answer concisely.")
        .build()?;

    let reply = chat.ask("What is a provider-neutral client?").await?;
    println!("{}", reply.text());
    Ok(())
}
```

### 2. ChatSession —— 有状态多轮对话

`Chat` 无状态；`chat.session()` 派生一个持有 `Conversation` 的多轮 `ChatSession`，`send` 会把历史
接续下去。`snapshot` / `restore` 只保存 data-only 事实（**不**含凭据、闭包、client 或 live handle）。

```rust
use agent_lib::facade::{Chat, ProviderConfig};
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let chat = Chat::builder()
        .provider(ProviderConfig::openai_from_env()?)
        .model("gpt-5.5")
        .system("回答简洁。")
        .build()?;

    let mut session = chat.session().build()?;
    session.send("解释 agent-lib 的 Client 层。").await?;
    let second = session.send("再解释 Conversation 层。").await?; // 记得上一轮

    println!("{}", second.text());

    let snapshot = session.snapshot()?; // data-only，可持久化后 restore
    let _ = snapshot;
    Ok(())
}
```

### 3. Agent —— typed function tool + 审批档位

`Agent` 在会话之上运行工具循环。用 typed 函数注册工具（`Tool::function_with_schema` 始终可用；
开 `facade-schema` 后可用自动派生 schema 的 `Tool::function`），用三档 `Approval`
（`auto_allow` / `auto_deny` / `ask`）控制权限。`run` 返回 `Reply`，`run_full` 返回携带
`response` / `usage` / `tool_calls` / `delegations` / `artifacts` / `events` 的 `RunOutput`。

```rust
use agent_lib::facade::tool::{Tool, ToolContext};
use agent_lib::facade::{Agent, Approval, ProviderConfig};
use serde_json::json;
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut agent = Agent::builder()
        .provider(ProviderConfig::openai_from_env()?)
        .model("gpt-5.5")
        .system("You are a concise weather assistant.")
        .tool(Tool::function_with_schema(
            "get_weather",
            "Look up the current weather for a city.",
            json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
            |_ctx: ToolContext, args: serde_json::Value| async move {
                let city = args.get("city").and_then(|v| v.as_str()).unwrap_or("?");
                Ok::<_, std::convert::Infallible>(format!("{city}: sunny, 26C"))
            },
        ))
        .approval(Approval::auto_allow())
        .build()?;

    let reply = agent.run("What is the weather in Shanghai?").await?;
    println!("{}", reply.text());
    Ok(())
}
```

### 4. External agent —— 委托外部 coding-agent CLI

`ManagedExternalAgent` 把一个外部 coding-agent runtime（Codex / Claude Code / OpenCode / ACP）
建模成受管 external delegate，挂到主 agent 上由模型按需委托。启动 / 写工作区 / resume 默认更保守
（需审批或显式 opt-in），每个子 agent 在隔离的临时 worktree 中运行。**需要开启对应 `external-*`
feature。**

```rust
use agent_lib::facade::{
    Agent, ApprovalPolicy, Delegation, ExternalRunMode, ManagedExternalAgent, ProviderConfig,
};
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let codex = ManagedExternalAgent::codex()
        .worktree("/home/me/repos/my-app")
        .mode(ExternalRunMode::Managed)
        .build()?;

    let mut agent = Agent::builder()
        .provider(ProviderConfig::openai_from_env()?)
        .model("gpt-5.5")
        .system("你是主 coding agent，需要改代码时委托 coder。")
        .external_agent("coder", codex)
        .delegation(Delegation::model_routed().expose_external_agents_as_tools())
        .approval(ApprovalPolicy::default().ask_external_agents())
        .build()?;

    let output = agent.run_full("修复当前 failing tests。").await?;
    println!("{}", output.reply.text());

    for delegation in output.delegations {
        println!("{}: {:?}, usage={:?}", delegation.delegate, delegation.status, delegation.usage);
    }
    Ok(())
}
```

要为宿主注入生产级 live-adapter handler，可用 `default_external_session_handler(&agent)`（feature-gated）
接 `.session_handler(..)`；跑通真机 CLI 的完整示例见下方[可运行示例](#可运行示例)。

`use agent_lib::prelude::*;` 可一次性重导最常用的 facade 入口（`Chat` / `ChatSession` / `Agent` /
`Tool` / `Approval` / `Delegation` / `ManagedExternalAgent` / `ProviderConfig` / `Reply` /
`RunOutput`，以及宿主嵌入用的 `ApprovalRequest` / `WireRunEvent` / `WireRunOutput`）。

## Facade 与下层高级接口

Facade 是**装配层**，把 identity、pending 事务、`AgentMachine`、`HandlerScope` 与 driver 封装成
渐进式 API。当需要更细粒度控制时，可以逐层退回到下面的原始接口——facade 用的正是它们。

### Facade 层能力一览（`agent_lib::facade`）

| 能力 | 入口 |
| --- | --- |
| 聊天（有/无状态） | `Chat` / `ChatSession`、`Reply`、`RunOutput`、streaming |
| 工具 agent | `Agent`、typed `Tool` + `ToolContext`、三档 `Approval` / `ApprovalPolicy`、loop policy |
| Local subagent | `Agent::worker()` + `.subagent(..)`、model-routed `Delegation` |
| Managed external agent | `ManagedExternalAgent`（Codex / Claude Code / OpenCode / ACP）、`ExternalRunMode` 能力分级 |
| 路由 / 升级 | `Delegation::dispatcher()`、`Dispatcher` / `Escalator`、verifier 闭环 |
| 协作底座 | 按 delegate 拓扑自动启用的 `Collaboration`（plan / blackboard / mailbox / artifacts） |
| snapshot / restore | data-only 快照，不含凭据 / 闭包 / client / live handle |
| 宿主嵌入注入口（M7） | `Agent::interaction_handler`、`RunEvent::to_wire()`→`WireRunEvent`、富化 `ApprovalRequest`、`default_external_session_handler`、`Delegation::dispatcher_evaluator/verifier`、`ApprovalPolicy::on_permission` |

设计详见 [`docs/facade-api.md`](docs/facade-api.md)。

### 下层高级接口

想脱离 facade 的装配约定、自己控制副作用兑现时，直接用下面各层：

- **Client 层（`agent_lib::client` / `adapter`）** —— 通过 dyn-safe 的 `LlmClient` 直接发请求。
  `EndpointConfig` 只描述传输端点，adapter（`AnthropicAdapter` / OpenAI Responses）负责协议路径与
  wire 转换；`chat` 用 `stream=false`，`chat_stream` 用 `stream=true`，流式事件交给统一的
  `Accumulator` 折叠成同一个 `Response`。

  ```rust
  use agent_lib::{
      adapter::anthropic::AnthropicAdapter,
      client::{AuthScheme, ChatRequest, EndpointConfig, LlmClient},
      model::{content::ContentBlock, message::{Message, Role}},
  };
  use serde_json::Map;

  let endpoint = EndpointConfig {
      base_url: std::env::var("ANTHROPIC_BASE_URL")?,
      auth: AuthScheme::Bearer(std::env::var("ANTHROPIC_AUTH_TOKEN")?),
      query_params: Vec::new(),
      extra_headers: vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())],
  };
  let client: Box<dyn LlmClient> = Box::new(AnthropicAdapter::new(endpoint));
  let response = client.chat(ChatRequest {
      model: "databricks-claude-haiku-4-5".to_owned(),
      messages: vec![Message {
          role: Role::User,
          content: vec![ContentBlock::Text { text: "用一句话解释归一化 Client 层。".to_owned(), extra: Map::new() }],
      }],
      tools: Vec::new(),
      system: Some("回答简洁。".to_owned()),
      max_tokens: 128,
      temperature: None,
      stream: false,
      provider_extras: None,
  }).await?;
  println!("stop={:?}, usage={:?}", response.stop_reason, response.usage);
  ```

- **Conversation 层（`agent_lib::conversation`）** —— 历史只能通过唯一的 pending 事务推进，公开 API
  不暴露裸 message/turn push。一次典型往返是 `begin_turn` → `start_assistant_response` →
  `finish_assistant` → `commit_pending`；带工具的响应需先用 `register_tool_calls` /
  `append_tool_response` 闭合本轮 call。核心能力：`Boundary` 安全切割、`revert_to` / redo、
  `fork_at` 分支、projection / compaction、data-only snapshot / restore。设计见
  [`docs/conversation-core.md`](docs/conversation-core.md)。

- **Agent 层（`agent_lib::agent`）** —— sans-io 的 `AgentMachine` 本身**不做任何 IO**：`step` 每次
  只把「现在需要什么」表达成可寻址 `Requirement`（`NeedLlm` / `NeedTool` / `NeedInteraction` …）
  然后停下等外部兑现。一组 handler 打包成一个 `HandlerScope`（每个副作用家族一个），构成一层
  drain layer；处理不了的 requirement 会 pop 到外层 scope。由此：**run mode = scope 的接线方式**
  （挂 interaction handler 即 attended，不挂即 headless），**父子 agent = scope 的嵌套**
  （`SubagentHandler` / `NestedMachine`）。官方参考实现 `ReferenceScope` 把 `LlmClient` +
  `ToolRegistry` + 审批后端接成一层 total scope，用 `drive_turn` 跑完一整轮。设计见
  [`docs/agent-layer.md`](docs/agent-layer.md) 与
  [`docs/agent-effect-model.md`](docs/agent-effect-model.md)。

- **数据模型与逃生舱（`agent_lib::model`）** —— 内容块的 `extra` 保留尚未建模的 provider 字段，
  `ProviderExtras` 让 provider 专属请求参数绑定目标 provider、只在匹配时合并，上层永不绑死 wire 细节。

## 模块概览

| 模块 | 作用 |
| --- | --- |
| `model` | 完整态消息、多模态内容块、工具 schema、token usage、归一化枚举，以及保留未建模字段的逃生舱。 |
| `stream` | 稳定 block id、归一化 delta，以及把增量事件折叠回完整 `Response` 的统一 `Accumulator`。 |
| `client` | `EndpointConfig`、认证、结构化 capability、分类错误，以及 dyn-safe 的 `LlmClient` trait。 |
| `adapter` | Anthropic Messages 与 OpenAI Responses 的 HTTP / SSE 适配器。 |
| `conversation` | 强类型 identity、`Conversation`、`PendingTurn` 事务、`Boundary`、fork、projection / compaction、snapshot / restore。 |
| `agent` | data-only 的 Agent 配置与状态、sans-io `AgentMachine`、`Requirement` 副作用模型与参考 driver；`agent::collab` 协作原语；`agent::external` 外部会话、`Dispatcher` 与 `Escalator`。 |
| `facade` | batteries-included 装配层：`Chat` / `ChatSession`、工具 `Agent`、subagent 与 external agent delegation、`Dispatcher` / `Escalator` 路由、自动 `Collaboration`，以及统一 `Reply` / `RunOutput` / `RunEvent` / snapshot-restore。 |
| `prelude` | `use agent_lib::prelude::*;` 重导最常用的 facade 入口类型。 |

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

Agent 端到端示例（交互式对话 + 需审批的 mock 工具）用同一组环境变量：

```bash
cargo run --example agent_chat
```

受管外部 agent 示例（经 `ExternalAgentMachine` + 作用域 `ExternalSessionHandler` 驱动真实
coding-agent CLI）按各自的 feature flag 门控，缺 CLI / probe 失败即打印非密提示并 skip：

```bash
cargo run --example managed_claude_code --features external-claude-code
cargo run --example managed_codex        --features external-codex
cargo run --example managed_opencode     --features external-opencode
cargo run --example managed_mixed        --features "external-claude-code external-codex"
```

运行说明、env、worktree 隔离与 secret 处理见 [`AGENTS.md`](AGENTS.md) 与
[`docs/capability-matrix.md`](docs/capability-matrix.md)。

### Endpoint 环境变量

| 变量 | Anthropic | OpenAI Responses |
| --- | --- | --- |
| provider 选择 | `AGENT_LIB_PROVIDER=anthropic` | `AGENT_LIB_PROVIDER=openai` |
| base URL | `ANTHROPIC_BASE_URL`（必填） | `OPENAI_BASE_URL`（必填） |
| 凭据 | `ANTHROPIC_AUTH_TOKEN`（Bearer） | `OPENAI_API_KEY`（`api-key` header） |
| model | `ANTHROPIC_MODEL`（默认 `databricks-claude-haiku-4-5`） | `OPENAI_MODEL`（默认 `gpt-5.5`） |
| API version | `ANTHROPIC_VERSION`（默认 `2023-06-01` header） | `OPENAI_API_VERSION`（默认 `2025-04-01-preview` query） |

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

触碰受管 external adapter 时，额外跑一遍带 feature 的 clippy：

```bash
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
```

真实 endpoint / 真机 CLI 测试默认标记为 `#[ignore]`，配置好凭据后可显式运行：

```bash
cargo test --test integration_anthropic -- --ignored --nocapture
cargo test --test integration_openai_resp -- --ignored --nocapture
cargo test --features external-claude-code --test external_claude_code -- --ignored --nocapture
```

## 参考文档

- [`AGENTS.md`](AGENTS.md) —— 构建 / 测试 / 运行（含受管外部 agent）的操作指南。
- [`DESIGN.md`](DESIGN.md) —— 完整设计。
- [`docs/conversation-core.md`](docs/conversation-core.md) —— Conversation 层设计。
- [`docs/agent-layer.md`](docs/agent-layer.md)、[`docs/agent-effect-model.md`](docs/agent-effect-model.md)
  —— Agent 层 sans-io + effect-handler 模型。
- [`docs/facade-api.md`](docs/facade-api.md) —— Facade（batteries-included 装配层）设计。
- [`docs/managed-external-agent.md`](docs/managed-external-agent.md) —— 受管外部 agent 设计与能力 parity。
- [`docs/capability-matrix.md`](docs/capability-matrix.md) —— provider 能力差异与实测范围。
