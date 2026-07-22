## Execution Plan — M3-1：流式骨架（stream/wire.rs + decoder.rs + [DONE] 哨兵）

TODO.md 第一个未完成任务：**M3-1**（`[TODO]`，SSE 流式骨架）。
前置 M1（骨架+请求侧）、M2（非流式响应侧）全 `[DONE]`。本任务落地 chat/completions
流式的骨架层：chunk wire 视图 + SseNormalizer 绑定 + chat_stream 接线 + `[DONE]` 哨兵
特判（在 JSON 解析前终止）。normalizer 的状态机（文本/reasoning/tool 增量/终态）留给 M3-2，
本任务用最小桩让骨架可编译、可验证 `[DONE]` 终止。

### 设计要点（设计文档 §4.4）

1. chunk 形态：`choices[0].delta = {role?, content?, reasoning_content?, tool_calls?}`；
   `finish_reason` 在末个非空 chunk；usage 在 `include_usage` 后由**空 choices 的独立 chunk** 携带。
2. `[DONE]` 哨兵（§4.4.1）：`data: [DONE]` 非 JSON，**在 JSON 解析前特判**直接 terminal；
   `SseNormalizer` 的 `is_terminal`/`incomplete_error` 已支持适配器自控终止。
3. SSE `event:` 字段恒为 `message`，chat/completions 无 `type` 判别字段 → **不做 event/type
   一致性检查**（设计文档「自然通过」），故 `event: message` 不触发任何错误。

### 文件清单（全部新增/改 openai_chat/stream/ 内，零越界）

| 文件 | 动作 | 内容 |
|---|---|---|
| `stream/wire.rs` | 新建 | crate-private chunk serde 视图：`DecodedChunk{choices, usage}` / `Choice{delta, finish_reason}` / `Delta{role,content,reasoning_content,tool_calls}` / `ToolCallDelta{index,id?,function?}` / `FunctionDelta{name?,arguments?}` + `decode(&str)->Result<DecodedChunk,String>`。多余字段（id/object/created/model/system_fingerprint/type/choices[].index）不建模，serde 默认忽略。 |
| `stream/decoder.rs` | 新建 | 照 openai_resp/stream/decoder.rs：`normalize_sse` 包装 `common::normalize_sse::<StreamNormalizer,…>` + `impl SseNormalizer for StreamNormalizer`（委托 + `invalid_sse`→`invalid_stream`）。 |
| `stream/normalizer.rs` | 新建 | `StreamNormalizer{terminal: bool}` 桩（Default）：`translate` 先判 terminal→报错；再特判 `event.data.trim()=="[DONE]"`→terminal+空事件；否则 `decode(&event.data)` 验证可解析（M3-2 替换为状态机产出）。`is_terminal`→`self.terminal`；`incomplete_error`→"SSE body ended before the [DONE] sentinel"。 |
| `stream/mod.rs` | 改 | 替换 M1-2 桩为接线（照 openai_resp/stream/mod.rs）：`mod decoder; mod normalizer; mod wire;` + `use decoder::normalize_sse;` + `chat_stream`（stream 守卫→`build_request`→`execute_sse_response`→`normalize_sse`）+ `invalid_stream` + `#[cfg(test)] mod tests;`。rustdoc 写 10min connect+headers 限定、body 无总超时。 |
| `stream/tests/mod.rs` | 新建 | 最小测试（M3-3 扩 parsing/transport/errors）：`decode_fixture`+`irregular_chunks` helper + 2 个 #[tokio::test]：① 含合法 chunk + `[DONE]` → 正常 terminal 收尾、events 空、无 JSON 解析错误；② `event: message` 行不触发一致性错误。 |

### 执行步骤

1. [x] 上下文读取（TODO/PLAN/设计文档 §4.4 + openai_resp stream 模板 + common sse/http + stream/mod.rs 桩 + StreamEvent 定义 + anthropic 位置派生 block id 先例 + openai_resp stream tests 惯例）。
2. [ ] 新建 `stream/wire.rs`（chunk serde 视图 + decode）。
3. [ ] 新建 `stream/normalizer.rs`（StreamNormalizer 桩 + [DONE] 特判）。
4. [ ] 新建 `stream/decoder.rs`（normalize_sse 包装 + SseNormalizer impl）。
5. [ ] 改 `stream/mod.rs`（chat_stream 接线 + mod 声明 + tests）。
6. [ ] 新建 `stream/tests/mod.rs`（[DONE] terminal + event:message 最小测试）。
7. [ ] 门禁：`cargo fmt --all` / `cargo clippy --all-targets -- -D warnings` / `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings` / `cargo test -p agent-lib --lib adapter::openai_chat` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
8. [ ] TODO.md M3-1 `[TODO]`→`[DONE]` + 追加完成记录。
9. [ ] git commit + stop。

### 关键决策

- **[DONE] 特判位置**：在 `StreamNormalizer::translate` 入口（frame→chunk 的入口），先于 `wire::decode`。
  这正是设计文档 §4.4.1「JSON 解析前特判」，且 `is_terminal()` 让 `common::normalize_sse` 的
  unfold 循环自然终止流（无需额外机制）。
- **M3-1 桩对合法 chunk 仍 decode**：验证 wire.rs 能解析真实 chunk（若字段建模错，测试 fixture
  会报错），但不产出 StreamEvent（M3-2 填充）。这同时满足「无 JSON 解析错误」的反向断言。
- **event 一致性检查不做**：chat/completions 无 `type` 判别字段，`event: message` 是常态，
  normalizer 直接忽略 `event.event` 字段，自然不报一致性错误（与 openai_resp 的 type 校验不同）。
- **wire.rs 类型零 `extra`**：设计文档明确「进不了 extra 的流式 chunk 不需要」，故未建模字段
  全部 serde 默认忽略（非「保留」），与 openai_resp wire（保留 raw）不同——chat chunk 无需保留。
- **过渡性 allow**：wire.rs 的 `DecodedChunk/Choice` 等类型在 M3-1 仅被 `decode` 内部用、未跨模块
  引用，但作为 `pub(super)` 类型 Rust 不报 dead_code（pub item 豁免），无需 `#[allow(dead_code)]`。

### 进度日志

- [x] 上下文读取 + 设计要点梳理
- [x] wire.rs / normalizer.rs / decoder.rs / mod.rs / tests/mod.rs 实现
- [x] 门禁全绿（fmt 无 diff / 默认+external clippy exit 0 / test 全绿 lib 1090→1094 / doc exit 0）
- [x] TODO M3-1 记录 + [DONE]
- [x] git commit (d701f2c) + stop
