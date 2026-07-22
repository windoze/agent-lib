## Execution Plan — M2-1：响应解析（wire 类型 + parse_response + finish_reason 映射 + chat()）

TODO.md 第一个未完成任务：**M2-1**（标题 `[TODO]`）。这是 openai_chat 适配器的非流式响应侧。
（注：docs/review-2026-07-23.md 的 C-SEC-1 等是全库历史安全 review，与 M2-1 无关，按执行规则不抢占。）

### 任务目标（设计文档 §4.3）

- `response.rs`：chat/completions 响应 wire 类型 + `parse_response`，校验 `object == "chat.completion"`，
  取 `choices[0]`（`n>1` 只取第一条）。
- `response/convert.rs`：`choices[0].message` → `Vec<ContentBlock>` + `finish_reason` 映射表函数。
  归一落点：`message.content` → `ContentBlock::Text`；`message.reasoning_content` →
  `ContentBlock::Thinking{text, signature:None}`；`message.tool_calls[]` → `ContentBlock::ToolUse`
  （`function.arguments` 字符串解析为 `Value`，解析失败保留原文进 extra）。
  `finish_reason` 映射：stop→EndTurn、length→MaxTokens、tool_calls→ToolUse、
  content_filter→Refusal、其它/缺失→Normalized::Other。
  block 顺序：reasoning 在 text 前（与 anthropic/openai_resp 惯例一致）。
  未建模字段（created/system_fingerprint/logprobs 等）进 `Response.extra`。
- `mod.rs` 的 `chat()`：stream 互斥校验（M1-2 已钉）→ `build_request` →
  `execute_json_response` → `parse_response`；错误经 `map_transport_error` /
  `ClientError::from_http_response` 分类。

### 复用（设计文档 §3，零改动 common + 现成模型）

- `common::execute_json_response`、`common::map_transport_error`（common/http.rs）已存在。
- `ClientError::from_http_response`（error.rs:61-105）已覆盖 OpenAI 拼写（429/Retry-After、408/504、
  401/403、context-length、content-filter），零改动。
- `Usage` 自定义 Deserialize 已认识 prompt_tokens/completion_tokens/total_tokens +
  details.cached_tokens / details.reasoning_tokens，直接反序列化。
- `common::insert_preserving_collision`（响应侧 extra 冲突保护）。

### 实现文件计划

1. `src/adapter/openai_chat/response.rs`（替换 M1-2/M1-3 的 chat() 桩 + invalid_response）：
   - `parse_response(body: &[u8]) -> Result<Response, ClientError>`：serde_json 反序列化 →
     `parse_response_value`。
   - `parse_response_value(value: Value)`：校验 `object == "chat.completion"`（比照
     openai_resp/response.rs:69 三态 match：Some(String) 匹配、Some(String) 不匹配报错、
     Some(_) 非字符串报错、None 缺失报错）；取 `choices`（可选数组）；`choices[0]`（缺失/空
     允许吗？——设计文档只说取 choices[0]，对无 choices 的响应，messages 全空时 usage-only
     不常见；非流式必有 choices，缺失报错更安全，比照 openai_resp 取 output required）；
     `usage` 经 take_usage；委托 convert_message + normalize_finish_reason；extra = wire。
   - `chat()`：`if request.stream` 守卫 → `build_request` →
     `common::execute_json_response(&self.http_client, request, Self::parse_response)`。
   - 保留 `pub(super) fn invalid_response`（convert.rs 复用）。
   - 定义 `RESPONSE_EXTRA_KEY`（crate-private 常量，比照 openai_resp 的 RESPONSE_EXTRA_KEY）。

2. `src/adapter/openai_chat/response/convert.rs`（新建）：
   - `convert_choice`/`convert_message`：把 choices[0].message 转成 Vec<ContentBlock>。
     顺序：reasoning_content(Thinking) 在前，content(Text) 在后（anthropic 惯例）；
     tool_calls → ToolUse（放 text 之后，与 wire message 形状一致）。
     实际：message.content 可为 string 或 null（chat/completions assistant content 可空）；
     message.reasoning_content 可选 string；message.tool_calls 可选数组。
   - `convert_tool_call`：取 id（可选，provider 可能不返回，缺失用空串？——比照 review 的
     M-STATE-1 指出 accumulator 不检查空 id/name 会泄漏；但本任务实现应取 id/name 为 string，
     缺失则报错 protocol 还是留空？设计文档 §4.3 只说 arguments 字符串解析失败保留原文进 extra，
     对 id/name 未说明。openai_resp 的 function_call 取 call_id/name/arguments 为 required string。
     chat/completions 的 tool_calls 的 id/function.name 实际是必备字段；保守取 required string，
     缺失报 protocol error，符合「不可信 wire 数据要么完整要么报错」原则，不引入空值风险）。
   - arguments 解析：`serde_json::from_str(arguments)` 失败 → 保留原文进 extra（key 如
     "raw_arguments"），input 设为 Value::Null 或 json!({})。设计文档说「解析失败保留原文进
     extra」；input 字段不能缺失（ContentBlock::ToolUse 需要 input: Value）。用 Value::Null
     表示解析失败更诚实，原文进 extra。但 ToolUse 的 input 为 Null 语义上奇怪——比照
     openai_resp 不容忍（直接报错）。这里按设计文档明确要求保留原文进 extra 而非报错，
     所以 input=Value::Null + extra["raw_arguments"]=原文。空 arguments("") → json!({})（与
     openai_resp empty_function_arguments 一致）。
   - `normalize_finish_reason(finish_reason: Option<&str>, has_tool_call) -> Normalized<StopReason>`：
     finish_reason 映射表。缺失 → Normalized::unknown(missing)？设计文档说「缺失→Other」。
     但若 has_tool_call 且 finish_reason 缺失，是否归 ToolUse？设计文档映射表里 content_filter
     → Refusal；tool_calls → ToolUse；未提 has_tool_call 兜底。严格按表：finish_reason 缺失
     → Other。保守实现：只看 finish_reason 字符串，不引入 has_tool_call 兜底（避免与 openai_resp
     那种 status+evidence 复杂逻辑漂移；chat/completions 的 finish_reason 是权威终止信号）。
     实现：match finish_reason { "stop"=>EndTurn, "length"=>MaxTokens,
     "tool_calls"=>ToolUse, "content_filter"=>Refusal, 其它(含缺失)=>unknown }。缺失时 raw
     无值 → 用 Normalized::without_raw(Other)? 但 unknown 需要 raw。缺失时给一个合成 raw
     如 "missing"？或直接 None → Other。设计文档「缺失→Normalized::Other」。用
     Normalized::without_raw(StopReason::Other)（without_raw 是 pub(crate)，crate 内可用）。
   - wire 类型：放 response.rs 或 convert.rs 内，crate-private（用 serde 反序列化 Value 再
     手动取，与 openai_resp 一致，避免定义 wire struct 泄漏）。openai_resp 用的是
     Value + take_required_string/take_required_array 手动取字段，无 serde struct。照此惯例，
     chat/completions 也用手动 Value 提取，不定义 wire struct（更符合「wire 类型 crate-private
     且不泄漏」+「比 Responses 扁」）。

3. `src/adapter/openai_chat/response/tests/{mod.rs, parsing.rs, transport.rs}` +
   `response/tests/fixtures/{text_response.json, tool_response.json, reasoning_response.json}`：
   - fixtures：三种响应（纯文本、工具调用、含 reasoning_content）。脱敏（无真实 key）。
   - parsing.rs：object 校验、choices[0]、三种 content 落点、finish_reason 全表、未知字段落 extra、
     usage（含 cached/reasoning details）、arguments 非法 JSON 保留原文进 extra、object 不符报错。
   - 注意：M2-1 验证条件要求 response/tests/ 含 fixtures + parsing + transport。但 transport
     完整状态码/内容类型/错误映射是 **M2-2** 的任务。M2-1 验证条件只列了「object 不符报错」等
     解析层断言，未列 transport 用例。transport.rs 整文件属 M2-2。本任务建 parsing.rs + fixtures，
     transport.rs 留 M2-2（mod.rs 里先不声明 transport mod，避免 dead；或在 mod.rs 留 transport
     的最小 happy-path）。重新读 M2-1 验证条件：列了 5 类，全是 parse_response 层（fixture 解析、
     finish_reason 全表、unknown→extra、usage、arguments、object 报错）。无 transport 用例。
     → 本任务只建 parsing.rs + fixtures + mod.rs。transport.rs 整个留 M2-2。
   - mod.rs 放 REAL_*_RESPONSE include_str! + minimal_request + local_endpoint（比照 openai_resp
     tests/mod.rs），声明 parsing 子模块。

### 验证条件（M2-1）

- `cargo test -p agent-lib --lib adapter::openai_chat` 通过。

### 执行步骤

1. [x] 读全部上下文（TODO/设计文档/openai_resp 模板/common/模型定义/既有 openai_chat 实现）。
2. [进行中] 写 convert.rs（message→ContentBlock + finish_reason 映射 + 工具函数）。
3. 改 response.rs（parse_response + chat() 接线 execute_json_response + RESPONSE_EXTRA_KEY）。
4. 建 response/tests/{mod.rs, parsing.rs} + 3 个 fixtures。
5. `cargo fmt --all`。
6. `cargo clippy --all-targets -- -D warnings`（默认 + external features）。
7. `cargo test -p agent-lib --lib adapter::openai_chat`。
8. `cargo test --all --all-targets`（全量，确保无回归 + accumulator 等无影响）。
9. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
10. TODO.md M2-1 标 [DONE] + 完成记录。
11. git commit。
12. stop。

### 进度日志

- [x] 上下文读取
- [x] convert.rs（convert_message 直接返回 Vec + normalize_finish_reason + convert_tool_call + parse_arguments + helpers）
- [x] response.rs（parse_response + parse_response_value + chat() 接线 execute_json_response + read_choice/take_usage + RESPONSE_EXTRA_KEY 声明移到 mod.rs）
- [x] tests + fixtures（3 fixtures + parsing.rs 10 用例 + mod.rs 含 minimal_request/local_endpoint allow(dead_code) 留 M2-2）
- [x] 门禁全绿（fmt 无 diff / 默认+external clippy / test --all 0 failed / doc）
- [ ] TODO 标 [DONE]
- [ ] commit

### 关键设计决策（写入完成记录）

- **choices 不从 extra 移除**：只 remove usage（消费进 Usage 字段，unmodeled usage 字段落 Usage.extra）；
  choices（含 choices[0].logprobs/message/finish_reason）与 object/id/created/model/system_fingerprint 保留在
  Response.extra——满足设计文档 §4.3「未建模字段落 extra」+ §2.2「logprobs 只能进 extra」。
  （与 openai_resp「remove output + 按 block 重建 evidence」不同：chat 因 §2.2 必须保 logprobs，保 choices 最简洁。）
- finish_reason 为权威终止信号：normalize_finish_reason 只看 finish_reason 字符串，缺失/null→Other(without_raw)，
  不引入 has_tool_call 兜底（避免与 openai_resp 复杂逻辑漂移）。
- arguments 解析失败 → input=Value::Null + extra[RESPONSE_EXTRA_KEY]["raw_arguments"]=原文（§4.3 保留原文进 extra）；
  空 arguments ""→json!({})（与 openai_resp empty_function_arguments 一致）。
- block 顺序：reasoning(Thinking) → text(Text) → tool_calls(ToolUse)（anthropic reasoning-before-text + wire 字段序）。
- role 校验 == "assistant"（防御，与 openai_resp 一致）。
- convert_message 借用 message（&Value），不消费，使 choices 能完整留在 extra。
- minimal_request/local_endpoint 加 #[allow(dead_code)] + 注释标注 M2-2 transport 接线（沿用 M1-2 过渡 allow 惯例）。
