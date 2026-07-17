# M1-3 `Chat` / `ChatBuilder` + `ask` / `ask_full`（one-shot，无 tool-use）

**当前任务 = TODO.md 首个未完成 = M1-3**（`### [TODO] M1-3`）。M1-1、M1-2 已 `[DONE]`。
唯一设计输入：`docs/facade-api.md` §5.1–§5.3。装配层，不新增 effect family，直接驱动 `Conversation`。

## 目标
新增 `src/facade/chat.rs`：
- `Chat`（可共享配置 + `Arc<dyn LlmClient>`，`Clone`）。
- `ChatBuilder`（`.provider(ProviderConfig).model(str).system(str).max_tokens(u32).temperature(f32).client(Arc<dyn LlmClient>).build()`）。
  - client 解析：显式 `.client` 优先；否则按 `ProviderId` 造 adapter（Anthropic→`AnthropicAdapter`，OpenAiResp→`OpenAiRespAdapter`）；都无→`FacadeError::Config`。
  - 缺 model → `FacadeError::Config`。
- `Chat::ask(input) -> Result<Reply, FacadeError>`、`ask_full(input) -> Result<RunOutput, FacadeError>`。
  - one-shot：每次新建临时 `Conversation`（`ConversationConfig::new(system)`）。
  - §5.3 驱动：`begin_turn` → effective_view+pending_context 构 `ChatRequest`（`ModelConfig::apply_to_request`，stream=false，无 tools）→ `client.chat` → `start_assistant_response` → `finish_assistant`。
    - `AssistantFinish::ReadyToCommit` → `commit_pending(TurnMeta::default())`（response usage 由 pending 自动 merge）。
    - `RequiresToolCallMappings` → `FacadeError::UnexpectedToolUse`。
  - 任意失败（含 tool-use / client error）→ 兜底 `cancel_pending(DiscardTurn)` 回到一致点。
- `session()` 入口延后到 M1-4（TODO 允许）。
- 全部公开项带 rustdoc + 一个 no_run doctest。
- mod.rs 重导 `Chat, ChatBuilder`；prelude.rs 补 `Chat`。

## 关键锚点（已核实）
- `Conversation::{new, begin_turn, effective_view, pending_context, start_assistant_response, finish_assistant, commit_pending, cancel_pending}`。
- `AssistantFinish::{ReadyToCommit, RequiresToolCallMappings}`；`CancelDisposition::DiscardTurn`。
- commit 时 response usage 已由 pending 累加并 `merge_pending` 进 meta → 传 `TurnMeta::default()`。
- request：`effective.into_parts()` + `pending_context().into_messages()`；`ModelConfig::apply_to_request` 覆盖 model/max_tokens/temperature/provider_extras。

## 测试（离线，fake `LlmClient` 返回固定 Response）
- `ask` 返回文本正确；`ask_full` 的 response/usage 正确。
- tool-use Response → `UnexpectedToolUse`。
- 连续两次 `ask` 互不保留历史（fake client 记录收到的 messages 长度均为 1）。

## 验证
1. `cargo fmt --all -- --check`
2. `cargo test -p agent-lib --lib facade::chat`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 完成状态：DONE
- 落地 `src/facade/chat.rs` + `chat/tests.rs`（6 离线测试全绿）。
- mod.rs 重导 `Chat, ChatBuilder`；prelude 补 `Chat`。
- 全序列绿：fmt ✅ / clippy ✅ / full suite 50 组 947 passed 0 failed ✅ / doc ✅ / diff --check ✅。
- `session()` 延后到 M1-4。TODO.md 已标 `[DONE] M1-3` 并补完成记录。
