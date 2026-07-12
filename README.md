# agent-lib

`agent-lib` 是一个面向 LLM API 的 Rust Client 与 Conversation Core 基础库。它用
provider-neutral 的数据模型承载消息、内容块、工具调用和 token usage，同时保留
provider 原始值与尚未建模的字段，避免上层 Conversation / Agent 逻辑依赖特定厂商的
wire 格式。

当前实现包含完整态数据模型、可折叠的归一化流式事件、dyn-safe Client 抽象、
Anthropic Messages 与 OpenAI Responses 的非流式/流式适配器、真实 endpoint 的跨
provider 归一化验收，以及 Conversation Core 的强类型 identity、独立 system 配置和
immutable message envelope。已完成的 Client 层实施记录见
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
- `conversation`：外部注入的强类型 id、独立 system 配置与不可原地修改的消息 envelope。

Conversation Core 正按任务顺序继续实现 closed Turn、pending、boundary、projection 与
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
