# agent-lib

`agent-lib` 是一个面向 LLM API 的 Rust Client 与 Conversation Core 基础库。它用
provider-neutral 的数据模型承载消息、内容块、工具调用和 token usage，同时保留
provider 原始值与尚未建模的字段，避免上层 Conversation / Agent 逻辑依赖特定厂商的
wire 格式。

当前实现包含完整态数据模型、可折叠的归一化流式事件、dyn-safe Client 抽象、
Anthropic Messages 与 OpenAI Responses 的非流式/流式适配器、真实 endpoint 的跨
provider 归一化验收，以及 Conversation Core 的强类型 identity、独立 system 配置、
immutable message envelope、只读 closed Turn、I1--I4 validator、原子 commit 数据边界，
不暴露 partial 的 stream/non-stream `PendingMessage` 冻结边界，以及可容纳任意多轮工具往返、
显式记录 open call 并只在最终 assistant 后提交的 `PendingTurn` 事务；pending cancel 可选择
整体丢弃、合成 `Cancelled` tool results 后继续，或补入完整最终 assistant 后原子提交；
committed Turn 已进入保留全部 raw 分支节点的结构共享 history，当前 lineage 与隐藏旧 suffix
彼此分离，并由可从 closed turns + pending 重建的 `ToolCallIndex` 提供只读定位加速；
`Boundary` 以 owner、Turn 位置/锚点和 structural version 共同防止跨会话、越界、stale 与
同位置 ABA 误用，反序列化 token 仍须交回所属 Conversation 校验。
已完成的 Client 层实施记录见
[`docs/archive/2026-07-13-client-layer/TODO.md`](docs/archive/2026-07-13-client-layer/TODO.md)；
当前 Conversation Core 阶段计划和任务见 [`PLAN.md`](PLAN.md) 与 [`TODO.md`](TODO.md)。

## 设计边界

- `model::message`：不带 Conversation `MessageId` 的 `Message` 与 `Role`。
- `model::content`：text、image、tool use/result、thinking 等完整态内容块。
- `model::tool`：工具 JSON Schema、工具调用与包含非正常状态的工具响应。
- `model::usage`：input/output/cache read/cache write/reasoning 分列的 token 统计。
- `model::normalized`：归一化枚举值，同时保留 provider 原始字符串。
- `model::extras`：绑定 `ProviderId`、仅在最终请求序列化阶段合并的方言字段。
- `stream`：用稳定 block id 关联增量事件，并通过统一 accumulator 折叠为完整响应。
- `client`：dyn-safe `LlmClient`、分类错误、结构化 capability、endpoint 与请求配置。
- `conversation`：外部注入的强类型 id、独立 system 配置、不可原地修改的消息 envelope、
  共享只读 message 的 closed `Turn`、分类 commit 错误、唯一 I1--I4 校验门，以及复用 Client
  `Accumulator` 的单消息 pending/freeze 状态机、唯一 `PendingTurn` 事务状态机和原子 cancel
  disposition；raw history 采用 parent-pointer/`Arc` 节点结构共享，派生 `ToolCallIndex`
  可按框架或 provider call id 查询当前 lineage 与 pending，而不参与事实校验；字段私有的
  `Boundary` 只由 Conversation 签发，并统一校验 owner/version/anchor/range/pending。

Conversation Core 正按任务顺序继续实现 head/revert/fork、projection 与
持久化；Agent loop、Tool registry 与多 agent 编排仍不在范围内。完整设计和当前阶段
计划分别见 [`DESIGN.md`](DESIGN.md) 与 [`PLAN.md`](PLAN.md)。

下面把调用方提供的稳定 UUID 与完整 Client message 组合成冻结 envelope。system prompt
单独保存在配置中，不会被包装成 `Role::System` 历史消息：

```rust
use agent_lib::{
    conversation::{ConversationConfig, ConversationMessage, MessageId},
    model::message::{Message, Role},
};

let message_id: MessageId = "018f0d9c-7b6a-7c12-8f31-1234567890ad"
    .parse()
    .expect("valid externally supplied id");
let message = ConversationMessage::new(
    message_id,
    Message {
        role: Role::User,
        content: Vec::new(),
    },
);
let config = ConversationConfig::new(Some("回答简洁。".to_owned()));

assert_eq!(message.id(), message_id);
assert_eq!(message.payload().role, Role::User);
assert_eq!(config.system(), Some("回答简洁。"));
```

Closed `Turn` 只暴露有序 message、完整 tool pairing、parent 和 metadata 的共享只读视图；
克隆不会复制或重新分配 message identity。它没有 public raw constructor，也不能从 serde
输入 unchecked 反序列化：内存 draft 与 serde DTO 都必须通过同一个 I1--I4 validator，
成功后才能一次性推进 Conversation history/version。失败会返回分类化
`ConversationError`/`CommitError`，原 Conversation 全结构不变。

公开 API 可以创建空 Conversation、只读检查 closed history，并通过唯一 pending 事务推进
history；它不会暴露裸 message/turn push：

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
assert_eq!(conversation.version(), 0);
```

Turn 切割使用 Conversation 签发的 `Boundary`，而不是裸 `usize`。空会话只含 zero boundary；
每次结构变化后都应重新获取 token。`Boundary` 可以序列化传递，但 serde 只恢复字段声明，
消费前仍需由当前 Conversation 检查 owner、version、Turn 锚点、lineage 范围和 pending
一致点：

```rust
use agent_lib::conversation::{Boundary, Conversation};

fn round_trip_zero_boundary(
    conversation: &Conversation,
) -> Result<Boundary, Box<dyn std::error::Error>> {
    let zero = conversation
        .valid_boundaries()
        .into_iter()
        .next()
        .expect("every conversation has a zero boundary");
    assert_eq!(zero.turn_count(), 0);
    assert_eq!(zero.after_turn(), None);

    let encoded = serde_json::to_string(&zero)?;
    let restored: Boundary = serde_json::from_str(&encoded)?;
    conversation.validate_boundary(&restored)?;
    Ok(restored)
}
```

`begin_turn` 只把完整 user payload 放入 pending，不提前修改 raw history。assistant 可以从
完整 `Response` 开始，也可以逐个接收 `StreamEvent`；冻结后若扫描到 `ToolUse`，调用方必须
为每个 provider call id 提供唯一 `ToolCallId`，逐一追加完整 result，并在所有 open call
闭合后继续下一条 assistant。只有无 tool-use 的最终 assistant 才能进入 `commit_pending`：

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
        Message {
            role: Role::User,
            content: Vec::new(),
        },
    )?;
    conversation.start_assistant_response(response)?;
    let outcome = conversation.finish_assistant(assistant_message_id)?;
    assert_eq!(outcome, AssistantFinish::ReadyToCommit);
    conversation.commit_pending(TurnMeta::default())
}
```

带工具的响应会返回 `AssistantFinish::RequiresToolCallMappings`；随后使用
`register_tool_calls`、`append_tool_response`（或 `append_tool_result`）闭合本轮 calls。
pending 只公开 frozen messages、phase、usage、response metadata 与只读 tool-call 视图；
活跃 accumulator 和 partial JSON 始终不可见。

`cancel_pending` 只处理尚未提交的事务。`DiscardTurn` 整体丢弃 pending；`ResumeTurn` 丢弃
活跃 partial，并要求调用方为每个已冻结 open call 提供 provider id、稳定 `ToolCallId` 与
result `MessageId`。库会生成带明确中断文本和 `ToolStatus::Cancelled` 的完整结果，然后回到
`AwaitingAssistant`。尚未执行 `register_tool_calls` 的冻结 call 也能在同一个原子操作中建立
mapping；已 mapping 的 call 则必须使用原 id：

```rust
use agent_lib::conversation::{
    CancelDisposition, CancelOutcome, CancelledToolResult, Conversation, ConversationError,
    MessageId, ToolCallId,
};

fn cancel_open_call_and_resume(
    conversation: &mut Conversation,
    provider_call_id: &str,
    call_id: ToolCallId,
    result_message_id: MessageId,
) -> Result<CancelOutcome, ConversationError> {
    conversation.cancel_pending(CancelDisposition::ResumeTurn {
        cancelled_results: vec![CancelledToolResult::new(
            provider_call_id,
            call_id,
            result_message_id,
        )],
    })
}
```

若当前只有 active partial 而没有冻结 open call，`cancelled_results` 传空数组即可。
`CancelDisposition::commit_turn(...)` 会在同样闭合 calls 后追加调用方提供的完整、无 tool-use
最终 `Response`，再复用唯一 I1--I4 validator 原子提交。任一 identity、状态、freeze 或
validator 错误都保留原 pending 与 committed history，成功 discard/commit 后可立即开始新 Turn。

单条 assistant response 在进入 PendingTurn 前先通过 `PendingMessage` 冻结。流式调用逐个
`push(StreamEvent)`；非流式调用从完整 `Response` 创建同一状态机。两条路径都只在
`finish` 成功时绑定调用方提供的 id，并把 usage、stop reason 与 provider metadata 和
immutable message 一起返回；partial JSON、缺失 stop 或 error event 不会产生 message：

```rust
use agent_lib::{
    client::Response,
    conversation::{ConversationError, FrozenMessage, MessageId, PendingMessage},
};

fn freeze_response(
    response: Response,
    message_id: MessageId,
) -> Result<FrozenMessage, ConversationError> {
    let mut pending = PendingMessage::from_response(response);
    pending.finish(message_id)
}
```

validator 接受的 canonical Turn 必须从一条 user message 开始，只允许完整闭合的
assistant tool-use → 一条或多条 tool-result → assistant 往返，并以不含 tool-use 的
assistant message 结束；system message、partial marker、重复 identity、孤儿/悬空/重复
provider call 以及跨 Turn pairing 都会被拒绝。调用方可依赖如下 closed 只读边界：

```rust
use agent_lib::conversation::Turn;

fn inspect_turn(turn: &Turn) {
    for message in turn.messages() {
        println!("message={} role={:?}", message.id(), message.payload().role);
    }
    for pairing in turn.pairings() {
        // closed pairing 的 result message 在类型上始终存在，不是 Option。
        println!("call={} result={}", pairing.call_id(), pairing.result_msg());
    }
}
```

## 环境与构建

需要支持 Rust 2024 edition 的稳定版工具链。克隆仓库后执行：

```bash
cargo build
cargo test --all --all-targets
cargo doc --no-deps --open
```

若作为同一工作区中的 path dependency 使用：

```toml
[dependencies]
agent-lib = { path = "../agent-lib" }
```

## Endpoint 与认证配置

`EndpointConfig` 只描述传输端点；adapter 负责附加协议路径（Anthropic 的
`/v1/messages`、OpenAI Responses 的 `/responses`）并做 wire 转换。认证可用 Bearer、
任意自定义 header 或 `None`，因此 wire protocol 与部署环境的认证方式不会耦合。
`EndpointConfig` 可序列化但包含凭据，不应写入日志或未经批准的持久化存储。

仓库中的三个示例通过 `AGENT_LIB_PROVIDER` 选择 adapter，并采用已经过真实 Foundry
endpoint 验证的默认值：

| 变量 | Anthropic | OpenAI Responses |
| --- | --- | --- |
| provider 选择 | `AGENT_LIB_PROVIDER=anthropic` | `AGENT_LIB_PROVIDER=openai` |
| base URL | `ANTHROPIC_BASE_URL`（必填） | `OPENAI_BASE_URL`（必填） |
| 凭据 | `ANTHROPIC_AUTH_TOKEN`（Bearer，必填） | `OPENAI_API_KEY`（`api-key` header，必填） |
| model/deployment | `ANTHROPIC_MODEL`（默认 `databricks-claude-haiku-4-5`） | `OPENAI_MODEL`（默认 `gpt-5.5`） |
| API version | `ANTHROPIC_VERSION`（默认 `2023-06-01` header） | `OPENAI_API_VERSION`（默认 `2025-04-01-preview` query） |

其他部署若使用 `x-api-key`、标准 OpenAI Bearer 或不同 query/header，应直接构造相应的
`AuthScheme` 和 `EndpointConfig`；无需修改 adapter。环境变量的值不会被示例打印。

## Client 层用法

下面通过 dyn-safe `LlmClient` 发起一次完整响应请求。调用 `chat` 时 `stream` 必须为
`false`；调用 `chat_stream` 时必须为 `true`，流式事件可交给统一的 `Accumulator`
折叠为同一个 `Response` 类型。

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
        extra_headers: vec![(
            "anthropic-version".to_owned(),
            "2023-06-01".to_owned(),
        )],
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

## 可运行示例

设置上表对应的环境变量后，三个示例可原样切换 Anthropic 或 OpenAI Responses：

```bash
export AGENT_LIB_PROVIDER=anthropic # 或 openai
cargo run --example non_streaming
cargo run --example streaming_typewriter
cargo run --example tool_round_trip
```

- `non_streaming`：通过 `Box<dyn LlmClient>` 获取完整的 normalized `Response`。
- `streaming_typewriter`：收到 `Delta::Text` 即刷新 stdout，同时把同一事件逐个送入公共
  `Accumulator`，最后校验可折叠的完整响应。
- `tool_round_trip`：声明 `get_weather` JSON Schema，读取模型产生的统一 `ToolUse`，模拟
  本地执行，再用同一个 call id 回灌 `ToolResult` 并取得最终文本。

每个示例为单次 HTTP 操作配置 45 秒 timeout；缺少变量或 provider 值非法时会给出不含
secret 的明确错误。

## 数据模型与逃生舱

下面的例子构造一条包含文本和工具调用的 assistant 消息，并展示如何读取归一化
usage。内容块的 `extra` 会保留尚未建模的 provider 字段。

```rust
use agent_lib::model::{
    content::ContentBlock,
    message::{Message, Role},
    usage::Usage,
};
use serde_json::{Map, json};

let message = Message {
    role: Role::Assistant,
    content: vec![
        ContentBlock::Text {
            text: "我来查询天气。".to_owned(),
            extra: Map::new(),
        },
        ContentBlock::ToolUse {
            id: "call_weather_1".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
            extra: Map::new(),
        },
    ],
};

let encoded = serde_json::to_string(&message).expect("serialize message");
let decoded: Message = serde_json::from_str(&encoded).expect("deserialize message");
assert_eq!(decoded, message);

let usage: Usage = serde_json::from_value(json!({
    "input_tokens": 20,
    "output_tokens": 8,
    "cache_read_input_tokens": 5
}))
.expect("deserialize usage");
assert_eq!(usage.input, 20);
assert_eq!(usage.output, 8);
assert_eq!(usage.cache_read, 5);
```

Provider 专属请求参数必须绑定目标 provider。只有目标匹配时才会合并，并且调用方
需要检查可观测的合并结果：

```rust
use agent_lib::model::extras::{
    ProviderExtras, ProviderExtrasMergeOutcome, ProviderId,
};
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

## 开发验证

提交前依次运行格式化、严格 lint、完整测试和文档构建：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

真实 endpoint 测试默认标记为 `#[ignore]`，只在显式选择时发起网络请求。配置好上述
凭据后可分别运行：

```bash
cargo test --test integration_anthropic -- --ignored --nocapture
cargo test --test integration_openai_resp -- --ignored --nocapture
cargo test --test integration_normalization -- --ignored --nocapture
```

完整能力差异与已实测范围见 [`docs/capability-matrix.md`](docs/capability-matrix.md)，
Client endpoint 约定与测试策略见已归档的
[`Client 层 PLAN.md`](docs/archive/2026-07-13-client-layer/PLAN.md)。
