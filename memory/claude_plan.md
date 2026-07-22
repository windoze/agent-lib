## Execution Plan — M1-3：请求侧映射 build_request + Message/Tool → messages/tools

本文件记录本轮（2026-07-23）可执行计划与进度。TODO.md 第一个未完成任务：**M1-3**（标题 `[TODO]`）。

### 现状判定

- M1-1/M1-2 已 `[DONE]`：`OpenAiChatAdapter` 骨架 + stream 互斥校验桩已落地。
  - `mod.rs`：结构体 + `LlmClient` impl（capability 返回 `OPENAI_CHAT_DEFAULT_CAPABILITY`）。
  - `request.rs`：纯 rustdoc 空壳（本任务填充）。
  - `response.rs` / `stream/mod.rs`：`chat()`/`chat_stream()` 桩，校验后返回 `ClientError::Other("…M1-3/M2-1")` 占位。
- M1-3 范围（**仅请求侧**）：`build_request` + input 映射 + 单测。**不**接线进 `chat()`/`chat_stream()`（那是 M2-1/M3-1）。
- 模板：`src/adapter/openai_resp/request.rs` + `request/input.rs` + `request/tests.rs`。

### 设计要点（设计文档 §4.2 映射表，逐条）

chat/completions 顶层 body：
```
{model, messages[], max_tokens, stream, temperature?, tools[]?, stream_options?}
+ provider_extras 最后 merge（可覆盖任意顶层字段，mismatch 报错）
```
- `max_tokens`（非 Responses 的 `max_output_tokens`）；`stream=true` → 注入 `stream_options.include_usage=true`。
- `messages[0]` = system（若 `ChatRequest.system` 存在）。
- **assistant 多 block 聚合成一条 chat 消息**：`content`(文本拼接/string|null) + `reasoning_content`(Thinking 拼接，无条件回放 §5.1) + `tool_calls`(ToolUse 列表)。
- ToolUse → `tool_calls:[{id,type:"function",function:{name,arguments:<JSON 字符串>}}]`。
- ToolResult（Tool 角色）→ 每块一条 `{role:"tool",tool_call_id,content}`；content 扁平化文本（image 有损丢弃）；非 Ok 状态拼入文本前缀 `[tool error/denied/cancelled]`。
- tools → `{type:"function",function:{name,description,parameters}}`（比 Responses 多一层 function 嵌套）。
- user 文本 → string content；user 含 image/unknown → array form（`{type:text,text}`/`{type:image_url,image_url:{url}}`/raw）。

### 文件改动

1. **新建 `src/adapter/openai_chat/request/input.rs`**：纯函数映射
   - `message_to_wire(idx,msg) -> Vec<Value>`（按 role 分派；System 报错）
   - `user_message_to_wire` / `assistant_message_to_wire` / `tool_message_to_wire`
   - `tool_call_to_wire` / `tool_to_wire`(pub(super)) / `flatten_tool_result_text` / `tool_result_status_marker`
   - `text_part` / `image_part` / `image_source_to_object` / `insert_string`
2. **重写 `src/adapter/openai_chat/request.rs`**：`build_request`(POST /chat/completions) + `serialize_body`(struct→Value→extras merge) + `invalid_request`/`invalid_endpoint` + `mod input;` + `#[cfg(test)] mod tests;`
3. **新建 `src/adapter/openai_chat/request/tests.rs`**：`json!` 精确比对完整 body + method/path/query/headers。覆盖 TODO 列出的 6 个关键用例 + image + invalid + endpoint 变体。
4. **`src/adapter/openai_chat/mod.rs`**：移除 `http_client` 字段的过渡 `#[allow(dead_code)]`（build_request 现已读取它，M1-2 完成记录要求届时移除）。
5. **不动** `response.rs` / `stream/mod.rs` 桩（M2-1/M3 再接线）。

### 验证

- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
- `cargo test -p agent-lib --lib adapter::openai_chat`
- M1-3 只改 openai_chat 内 + mod.rs 一行 allow 移除；改了源码故保守跑 `cargo test --all --all-targets` 确认无回归。

### 浮点注意

f32 temperature 经 serde 提升为 f64；测试用精确值（0.25/0.5）避免 f32↔f64 不等。extras-sourced 浮点天然 f64，与 `json!` 同源安全。

### 进度日志

- [x] 读 TODO/PLAN/设计文档/模板/模型/common helper
- [x] 写 input.rs
- [x] 重写 request.rs
- [x] 写 tests.rs
- [x] 移除 mod.rs allow(dead_code)
- [x] fmt + clippy + test（修了一处测试数据不一致：Ok 用例 text 应为 "lookup failed" 非 "sunny"）
- [x] TODO.md 标 [DONE] + 完成记录
- [ ] 提交

### 验证结果摘要（全绿）

- `cargo fmt --all` 无 diff
- `cargo clippy --all-targets -- -D warnings`（默认 + external features 两套）
- `cargo test -p agent-lib --lib adapter::openai_chat` 12 通过（3 既有 + 9 新增）
- `cargo test --all --all-targets` 全绿（lib 1065→1074，无回归）
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p agent-lib`
