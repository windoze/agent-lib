## Execution Plan — M1-1 库内前置触点

This file records the actionable plan and progress updates for the current invocation.

## 任务（TODO.md 第一个未完成任务）
**M1-1 [TODO] 库内前置触点：ProviderId 变体 + capability 静态 + 模块注册**

三个核心交付物 + 保证全 crate 编译通过（`cargo clippy --all-targets -- -D warnings`）。

## 三处触点现状（已读源码确认）

1. **`src/model/extras.rs:14`** `ProviderId`（`#[non_exhaustive]` + `#[serde(rename_all="snake_case")]`）。
   - 现有：`Anthropic`→`anthropic`、`OpenAiResp`→`open_ai_resp`。新增 `OpenAiChat`→`open_ai_chat`。
   - extras.rs 内 exhaustive 测试：`provider_extras_round_trip_for_every_provider_id`（:163-180）遍历 `(provider, wire_name)` 表，需追加 `(OpenAiChat, "open_ai_chat")`。其余用具体值，无 exhaustive match，无需改。

2. **`src/client/capability.rs:77`** 新增 `OPENAI_CHAT_DEFAULT_CAPABILITY`（full struct literal，比照既有静态）：
   - `max_context_tokens: None`；`input_modalities: {Text, Image}`；`output_modalities: {Text}`
   - `streaming/tool_calling/parallel_tool_calls/reasoning = true`
   - `prompt_caching/structured_output = false`（关键差异，显式写出）
   - `stop_reasons: {ToolUse, EndTurn, MaxTokens, StopSequence, Refusal}`
   - rustdoc 同既有口径；测试追加断言 + use 导入。

3. **`src/adapter/mod.rs`** 加 `pub mod openai_chat;`，建占位 `src/adapter/openai_chat/mod.rs`（M1-2 填充）。模块文档注释无协议清单，无需同步。

## 关键风险：facade 内两处 exhaustive match（blocker）
- `src/facade/config.rs:237-252` `ProviderConfigBuilder::build()`：无 wildcard。
- `src/facade/chat.rs:391-394` `client_for_provider()`：无 wildcard。
- `ProviderId` 虽 `#[non_exhaustive]`，**定义 crate 内仍要求 exhaustive**，新增变体让这两处编译失败。
- 验证条件要求 clippy 全绿。策略：先做三处核心改动 → `cargo check` 实证 → 最小正确修复（非 workaround）。
  - `config.rs` build()：加 `OpenAiChat` arm（Bearer endpoint，设计文档 §5.3/§6 形态明确）。
  - `client_for_provider`：依赖 `OpenAiChatAdapter`（M1-2 才有），实证后处置——优先在 mod.rs 放满足编译的最小桩，绝不 `unimplemented!`/wildcard 绕过。

## 执行步骤
1. 本计划文件 ✓
2. extras.rs：`OpenAiChat` 变体 + round-trip 测试表 ✓
3. capability.rs：静态 + 测试 + use 导入 ✓
4. adapter/mod.rs + 占位 mod.rs ✓
5. `cargo check` 实证 facade break ✓（config.rs build() + chat.rs client_for_provider() 两处 exhaustive match 断裂）
6. 最小正确修复 facade ✓
7. `cargo fmt` → clippy（base + external-features）✓
8. 相关单测 + 全量 ✓
9. TODO.md 标 [DONE] + 完成记录
10. git commit + stop

## Progress（已完成）
- 实现完成且全门禁通过：
  - `cargo fmt --all`（无 diff）
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
  - `cargo test -p agent-lib --lib client::capability`（6 通过，含新增 openai_chat 用例）
  - `cargo test -p agent-lib --lib model::extras`（4 通过，round-trip 已含 OpenAiChat）
  - `cargo test --all --all-targets`（全绿）
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
- 关键实现决策：新增 `ProviderId::OpenAiChat` 使 crate 内两处 exhaustive match（facade/config.rs `build()`、facade/chat.rs `client_for_provider()`）编译断裂。M1-1 验证要求 clippy 全绿，故这两处必须在本任务内最小正确处理（非 workaround）：
  - config.rs：加 `OpenAiChat` arm + `openai_chat_endpoint()` Bearer helper（设计 §5.3/§6 正确传输形态）；env 读取构造器 `openai_chat_from_env` 留给 M4-1。
  - chat.rs：加 `OpenAiChat` arm 引用 `OpenAiChatAdapter`；为此在 openai_chat/mod.rs 放**最小编译桩**（结构体形状 + Clone/Debug + new() + LlmClient，capability() 已为最终值；chat/chat_stream 返回 `ClientError::Other` 占位）。构造函数 with_http_client、stream 标志互斥校验、真实 body、子模块空壳、rustdoc、stream=true/false 单测全部留给 M1-2。
- 显式留给后续任务：lib.rs 协议清单文档 → M4-1；真实适配器骨架 → M1-2；env 构造器 → M4-1。
- 无 breaking change（non_exhaustive enum 新增变体 + 新静态 + 新模块，均为向后兼容的形状新增）。
