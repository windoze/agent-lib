## Execution Plan — M4-3：#[ignore] 真实端点测试 tests/integration_openai_chat.rs

TODO.md 第一个未完成任务：**M4-3 [TODO]**（M1/M2/M3 + M4-1/M4-2 全部 [DONE]）。
目标：新建 `tests/integration_openai_chat.rs`，两套真实端点配置（DeepSeek + vLLM），
全部 `#[ignore]`，缺 env 干净跳过（Option 模式）。设计文档 §7 item3 / §5.1 / §5.2 / §7.3。

### 现状盘点（已读源码核实）

- 模板 `tests/integration_openai_resp.rs`：`integration_adapter()->Option` + `text_request`/`tool_request`
  + `fold_events` + `collect_stream` + 3 个 `#[ignore]` 测试。照抄结构与风格。
- 适配器 API（`src/adapter/openai_chat/mod.rs`）：`OpenAiChatAdapter::new(endpoint)` /
  `with_http_client(endpoint, http)`；`chat(req)->Response` / `chat_stream(req)->BoxStream`。
  `chat()` 拒 `stream=true`，`chat_stream()` 拒 `stream=false`（请求侧 `stream` 标志决定走哪条）。
- 传输形态（§5.3/M4-1）：**Bearer 直连**，无 `api-key` 头/无 `api-version` query；vLLM 可 `AuthScheme::None`。
- 两套 env 约定（§7.3 / TODO M4-3 上下文）：
  - DeepSeek：`DEEPSEEK_API_KEY` 必需；`DEEPSEEK_BASE_URL` 默认 `https://api.deepseek.com`；
    `DEEPSEEK_MODEL` 默认 `deepseek-chat`；思考模型另需 `deepseek-reasoner`（DeepSeek 思考由模型名驱动）。
  - vLLM：`VLLM_BASE_URL` 必需；`VLLM_API_KEY` 缺省→`AuthScheme::None`；
    `VLLM_MODEL` 缺省→占位（部署相关，注释提示覆盖）。
- §5.1 关键规则（test 4 的验收点）：思考模式 + 有工具调用时，`reasoning_content` 必须在后续轮次完整回传，
  否则 API 400（`The reasoning_content in the thinking mode must be passed back to the API`）。
  适配器请求侧**自动**把 assistant 的 `Thinking` 块序列化为 `reasoning_content`、`ToolUse` 为 `tool_calls`，
  故 round-2 只需在历史里放回带 `Thinking`+`ToolUse` 的 assistant 消息即可（§5.1 推论：统一原样回放永远安全）。
- 强制工具调用：chat/completions 经 `provider_extras` 的 `tool_choice: {type:"function",name:...}`
  （openai_resp 集成测试同款，DeepSeek/OpenAI 兼容）。
- `StreamEvent`/`BlockKind`/`Delta`：`BlockKind::Reasoning` + `Delta::Reasoning(String)` 是 reasoning 落点；
  `BlockKind::Text` + `Delta::Text`；`MessageStart`/`BlockStart`/`BlockDelta`/`Usage`/`MessageStop`。
- `ContentBlock`：`Thinking{text,signature:Option,extra:Map}`、`ToolUse{id,name,input,extra}`、
  `ToolResult{tool_use_id,content:Vec<ContentBlock>,status:ToolStatus}`、`ToolStatus::{Ok,Error,Denied,Cancelled}`。
- 超时先例：e2e 用 75s；模板集成测试用 55s/call。本任务真实端点用 90s 外层包裹（ignored 测试默认不跑，
  手动 --ignored 才命中真实网络；每测外层超时兜底防卡死）。
- 安全审查 C1（ACP fs 沙箱）属不同子系统，不阻塞本任务（不抢占 TODO 顺序）。

### 实施（单文件新增 `tests/integration_openai_chat.rs`）

#### helpers
- `deepseek() -> Option<DeepSeek>`：`DEEPSEEK_API_KEY` 缺失→`None`+skip 文案（不打印 key 值）；
  否则构造 `OpenAiChatAdapter::with_http_client`（Bearer，`query_params:[]`/`extra_headers:[]`，
  90s timeout client）+ 读 base_url/chat_model/reasoner_model 默认值。
- `vllm() -> Option<Vllm>`：`VLLM_BASE_URL` 缺失→`None`；否则 adapter（Bearer 或 None 取决于 `VLLM_API_KEY`）+ model。
- `text_request(model, prompt, stream)`：纯文本 user 请求（system=None, tools=[], max_tokens=128）。
- `tool_choice_extras(name)`：`ProviderExtras{provider:OpenAiChat, fields:{tool_choice}}`。
- `thinking_extras()`：`{"thinking":{"type":"enabled"}}`（§5.1 provider_extras 开思考；DeepSeek 思考实由
  `deepseek-reasoner` 模型驱动，extras 走透传、被忽略无害——注释说明）。
- `weather_tool()` / `fold_events()` / `collect_stream()`：照模板。

#### 测试（全部 `#[tokio::test]` + `#[ignore = "..."]`）
1. `deepseek_non_streaming_text_returns_content_and_usage` — chat model 非流式；断言非空 text + role=Assistant + usage>0。
2. `deepseek_streaming_text_yields_text_delta_and_usage` — 流式；断言 MessageStart + Text block + Text delta +
   加性 Usage + MessageStop；fold 后 role/usage 一致。
3. `deepseek_thinking_mode_returns_reasoning_block` — reasoner model + thinking_extras；非流式断言响应含 `Thinking` block（reasoning_content 归一化）。
4. **`deepseek_thinking_multiturn_with_tool_call_avoids_400`**（§5.1 关键）：round-1（reasoner + weather_tool +
   tool_choice 强制）→ 断言响应含 `Thinking` + `ToolUse`；round-2 把完整历史（user1, assistant1 带 Thinking+ToolUse,
   tool result, user2）回放——断言**不 400**且正常收尾（验证适配器请求侧自动回放 reasoning_content 的 400 防线）。
5. `vllm_non_streaming_text_smoke` — base_url 非流式；断言非空 text + role + usage>0。
6. `vllm_streaming_text_smoke` — 流式；断言 text delta + Usage；若流中出现 reasoning block 则顺带记录（§5.2 待验证）。

#### 约束
- 全部离线可编译、可 `cargo test --test integration_openai_chat`（无 env 全跳过 exit 0）。
- 不打印 key 值；skip 文案只点 env 变量名。
- 真实端点行为未实测则完成记录如实标注「未实测」（无 DEEPSEEK_API_KEY/VLLM 凭据），留给 M4-R / M5-1 引用。

### 验证
- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`（编译 integration_openai_chat binary + lint）
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
- `cargo test --test integration_openai_chat`（无 env：全部 ignored/跳过，exit 0，秒级）
- 本任务只新增 ignored 测试文件，不改生产代码。

### 执行顺序

1. [x] 上下文读取（模板/适配器/设计文档/模型类型/e2e 参考）。
2. [ ] 写 `tests/integration_openai_chat.rs`。
3. [ ] 跑门禁（fmt → clippy ×2 → test --test integration_openai_chat）。
4. [ ] TODO.md M4-3 [TODO]→[DONE] + 完成记录；commit + stop。

### 进度日志

- [x] 上下文读取完成；计划定稿。
- [x] 写 `tests/integration_openai_chat.rs`（6 测试 + helpers）。
- [x] 修 `ToolResult` 缺 `extra` 字段（编译错误）。
- [x] **真实端点实测**（环境有 `DEEPSEEK_API_KEY`，VLLM_* 全无）：
  - 发现并修 2 个真实 spec 细节：
    1. chat/completions `tool_choice` 必须嵌套 `{"type":"function","function":{"name":...}}`
       （DeepSeek 报 `field function: invalid type: null`）——非 Responses 的扁平形式。
    2. DeepSeek 思考模式**拒绝** `tool_choice` 字段（`Thinking mode does not support this tool_choice`）→
       test 4 改用强指令 system prompt 自然触发工具调用（§5.1 本就不需要 tool_choice）。
  - 4 个 DeepSeek 测试全过真实端点（含 §5.1 reasoning_content 回放防 400）；2 个 vLLM 干净跳过。
  - **测试不打印 key 值**（仅 skip 文案点变量名）；grep 确认无凭据泄漏。
- [x] 门禁全绿：fmt 无 diff / clippy 默认 exit0 / clippy external exit0 /
      `test --test integration_openai_chat`（默认 6 ignored exit0；--ignored 4 live + 2 skip 全 ok）。
- [x] TODO M4-3 [TODO]→[DONE] + 完成记录（含真实实测结论）。
- [x] commit（a647a19）+ stop。


