# agent-lib

`agent-lib` 是一个面向 LLM API 的 Rust Client 层基础库。它用 provider-neutral
的数据模型承载消息、内容块、工具调用和 token usage，同时保留 provider 原始值与
尚未建模的字段，避免上层 Conversation / Agent 逻辑依赖特定厂商的 wire 格式。

当前仓库已完成 Milestone 1 的完整态数据模型。流式事件、Client trait 以及
Anthropic Messages / OpenAI Responses 适配器会按 [`TODO.md`](TODO.md) 的顺序继续实现。

## 设计边界

- `model::message`：不带 Conversation `MessageId` 的 `Message` 与 `Role`。
- `model::content`：text、image、tool use/result、thinking 等完整态内容块。
- `model::tool`：工具 JSON Schema、工具调用与包含非正常状态的工具响应。
- `model::usage`：input/output/cache read/cache write/reasoning 分列的 token 统计。
- `model::normalized`：归一化枚举值，同时保留 provider 原始字符串。
- `model::extras`：绑定 `ProviderId`、仅在最终请求序列化阶段合并的方言字段。

本 crate 不负责 Conversation 日志、Agent loop、Tool registry 或多 agent 编排。完整设计
和阶段计划分别见 [`DESIGN.md`](DESIGN.md) 与 [`PLAN.md`](PLAN.md)。

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

## 基础用法

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

真实 endpoint 的集成测试配置与认证方式记录在 [`PLAN.md`](PLAN.md)；相应测试在后续
适配器里程碑实现前不会发起网络请求。
