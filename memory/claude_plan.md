## Execution Plan — M3-3：流式 fixtures + 端到端折叠对照 + transport

TODO.md 第一个未完成任务：**M3-3 [TODO]**。M3-1/M3-2 已 DONE。本任务在其上加录屏级
`.sse` fixture + Accumulator 折叠一致性对照 + transport。设计文档 §4.4 / §7.1。

### 关键事实（已核对）

- M3-2 `stream/tests/mod.rs` 已有 12 个 inline 状态机测试（精确事件序列）+ helper
  `decode_fixture`/`irregular_chunks`/`sse`。**不重构、不移动**，本任务在其上追加。
- **折叠对照的 extra 差异**：流式 normalizer 不发 `ResponseMetadata` → folded `extra` 空；
  非流式 `parse_response` 把 `choices`/`object`/`id`/顶层字段全留 `extra`。故对照前**清空
  response-level extra**，只比 message(content blocks)/stop_reason/usage。per-block extra 两边都空
  （fixture 用合法 JSON arguments）→ content 可整体 `assert_eq!`。
- **Usage 两边同源**：流式与非流式都经同一份自定义 `Usage::deserialize`（prompt_tokens→input、
  cached_tokens→cache_read、reasoning_tokens→reasoning，details 包装器被消费，extra 空）。
- chat/completions **无 `type:"error"` 事件建模** → errors.rs 的「SSE 错误帧」= 畸形 data 帧
  （非法 JSON → `wire::decode` 失败 → Protocol）。
- `common::normalize_sse`：`is_terminal()`（[DONE] 后）→ unfold `None` 正常结束；[DONE] 后的 chunk
  **不会再喂进 translate**（terminal guard 仅直接调 normalizer 时可达）。

### 设计决策

1. **配对 fixture**：4 个 `.sse` 各配一个同语义非流式 `.json`（放 `stream/tests/fixtures/`）。
   折叠对照 = `comparable(fold(sse)) == comparable(parse(json))`，`comparable` 清空 response extra。
2. **fixture 区分度**：①text `stop`→EndTurn ②tool `tool_calls`→ToolUse（并行双 index）
   ③reasoning `stop`→EndTurn ④usage_terminal 用 `length`→MaxTokens（fixture 层补 finish_reason 覆盖，
   避免与 ①重复）。每 fixture 都带终态 usage chunk（真实 include_usage 形态）。
3. **parsing.rs**：每 fixture 一个测试 = 不规则分块喂管线断言**精确全序列** + 折叠对照 parse(json)。
4. **errors.rs**：①直接 normalizer：[DONE] 后再喂 chunk → Protocol「after the [DONE] sentinel」
   ②管线 EOF 无 [DONE] → Protocol「[DONE]」③`data: {broken` → decode 失败 Protocol
   ④非法 UTF-8(0xff) → Protocol「valid UTF-8」⑤直接 normalizer：tool 首片缺 id → Protocol「must carry `id`」
   ⑥空 delta + 未知字段 + [DONE] → 干净终止不 panic（M3-R 健壮性）。
5. **transport.rs**：照 `openai_resp/stream/tests/transport.rs`，断言改 `POST /chat/completions`、
   `[DONE]`。6 用例：200 成功折叠 + 429 retry + content-type + 截断体 EOF + 非 stream 守卫 + 500→Api。
6. **mod.rs 改动**：补 import（`Response`/`AuthScheme`/`EndpointConfig`/`LlmClient`/`Message`/
   `ContentBlock`/`Accumulator`/`AccumulatorError`/`Map`/`Value`）+ helper `fold_events`/`comparable`
   + 4 个 `.sse` `include_str!` 常量 + `mod errors/parsing/transport;` + 更新模块 doc。保留 12 inline 测试。

### 事件序列（trace normalizer，parsing.rs 断言依据）

- text: MsgStart, BlockStart(text), ΔText"Hello", ΔText" world", Usage(13/26/39,cr4), BlockStop(text), MsgStop(EndTurn,"stop")
- tool: MsgStart, BlockStart(t0,{first,call_demo_a}), BlockStart(t1,{second,call_demo_b}), ΔJson(t0,"{\"a\":"), ΔJson(t1,"{\"b\":"), ΔJson(t0,"1}"), ΔJson(t1,"2}"), Usage(53/18/71), BlockStop(t0), BlockStop(t1), MsgStop(ToolUse,"tool_calls")
- reasoning: MsgStart, BlockStart(reasoning), ΔReasoning"Let me think", ΔReasoning" step by step.", BlockStart(text), ΔText"The answer is 42.", Usage(30/50/80,cr6,r35), BlockStop(reasoning), BlockStop(text), MsgStop(EndTurn,"stop")
- usage_terminal: MsgStart, BlockStart(text), ΔText"Truncated", ΔText" answer", Usage(20/64/84,cr2), BlockStop(text), MsgStop(MaxTokens,"length")

### 执行步骤

1. [x] 上下文读取（TODO §M3-3 + openai_resp stream/tests 全套 + chat normalizer/wire/decoder +
   accumulator + common/sse + Response/Usage 形状 + response parsing fixtures）。
2. [ ] 建 8 个 fixture（4 .sse + 4 .json，脱敏 demo）。
3. [ ] 改 `stream/tests/mod.rs`（import + helper + 常量 + mod 声明 + doc）。
4. [ ] 新建 `parsing.rs`（4 fixture 精确序列 + 折叠对照）。
5. [ ] 新建 `errors.rs`（6 用例）。
6. [ ] 新建 `transport.rs`（6 用例）。
7. [ ] 门禁全绿：fmt / clippy(默认+external) / test -p openai_chat / test --all / doc。脱敏 grep。
8. [ ] TODO M3-3 [TODO]→[DONE] + 完成记录；commit + stop。

### 进度日志

- [x] 上下文读取 + 设计决策定稿
- [x] 8 fixture（4 .sse + 4 .json，脱敏 demo）
- [x] mod.rs（import + fold_events/comparable helper + 4 .sse 常量 + 3 mod 声明 + doc）；transport-specific import 下沉 transport.rs
- [x] parsing.rs（4 fixture 精确序列 + 折叠对照 + 加性 usage 单测）
- [x] errors.rs（6 用例：terminal guard / EOF / 畸形 JSON / 非法 UTF-8 / tool 首片缺 id / 空delta+未知字段）
- [x] transport.rs（6 用例：200 折叠 / 429 retry / 500→Api / content-type / 截断 EOF / 非 stream 守卫）
- [x] 修复：.sse 缺尾随空行导致 [DONE] 不 dispatch → 加 `\n\n: end of recorded fixture`
- [x] 门禁全绿：fmt 无 diff / clippy 默认+external exit 0 / test -p openai_chat 57 通过（40+17）/ test --all 全 `0 failed`（lib 1119）/ doc exit 0 / 脱敏 grep 无命中
- [ ] TODO M3-3 [TODO]→[DONE] + 完成记录；commit + stop
