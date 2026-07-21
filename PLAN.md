# 实施计划：openai_chat 适配器（chat/completions + DeepSeek/vLLM 方言）

> **唯一设计输入**：[`docs/openai-chat-api.md`](docs/openai-chat-api.md)（适配器设计、方言规则、
> 库内触点、测试计划、规模估算；下称「设计文档」，按 §号引用）。
>
> 旧版计划和任务单已归档（最近一轮）：
>
> - [docs/archive/2026-07-20-mag-gaps/PLAN.md](docs/archive/2026-07-20-mag-gaps/PLAN.md)
> - [docs/archive/2026-07-20-mag-gaps/TODO.md](docs/archive/2026-07-20-mag-gaps/TODO.md)
>
> 逐任务清单见 [`TODO.md`](TODO.md)。

## 目标

1. 新增 `src/adapter/openai_chat/` 适配器 `OpenAiChatAdapter`，实现 `LlmClient`
   （`src/client/mod.rs:29`），覆盖非流式 `chat()` 与 SSE 流式 `chat_stream()`
   （设计文档 §2.1、§4.1）。
2. 一个适配器覆盖三家：OpenAI 兼容基线 + DeepSeek 方言（思考模式 `reasoning_content`）
   + vLLM 自建服务；差异只体现在 `EndpointConfig` 与 `provider_extras`，不建 quirk
   配置类型（设计文档 §5.3）。
3. 测试三层齐备：模块内单测（request/response/stream + fixtures）、transport 测试、
   `#[ignore]` 真实端点测试（DeepSeek + vLLM），并纳入跨 provider 归一化矩阵
   （设计文档 §7）。
4. 完成库内触点（`ProviderId`、capability、facade、文档注释）与文档同步；`DESIGN.md`
   §1.1 的「不支持 chat/completions」决策反转必须修订（设计文档 §1、§8）。

## 非目标

1. 设计文档 §2.2 列出的第一期非目标全部不做：`logprobs`、`n > 1` 多 choice、audio
   content、采样参数扩充（一律走 `provider_extras`）、quirk 配置体系、OpenAI 官方账号侧
   新字段适配。
2. 不为方言建新适配器类型；§5.2 的 vLLM `reasoning_content` 回放兼容性是待验证项，
   只有实测确认有端点拒绝时才考虑引入 quirk 开关（本计划不建）。
3. 不改动 `src/adapter/common/` 与归一化模型（`Usage`、`ContentBlock::Thinking`、
   extras 逃生舱均已消化 chat/completions 差异，设计文档 §3 确认零改动复用）。
4. 不用 agent-testkit mock provider 传输层（它明确不做这个，设计文档 §7）。
5. 默认测试保持离线可跑；真实端点测试一律 `#[ignore]`，缺环境干净跳过（exit 0）。
6. 1.0 前的 API 稳定性不作为约束，但优先向后兼容形状（新增类型/变体/静态，不改既有
   签名）；breaking change 必须在任务完成记录中显式注明。

## 排序原则

1. **先骨架后细节（M1）**：库内前置触点（`ProviderId`、capability 静态、模块注册）是
   适配器编译的前提，与请求侧映射一起构成可编译骨架，最先落地。
2. **先非流式后流式（M2 → M3）**：非流式 `parse_response` 是流式 Accumulator 折叠结果的
   对照基准（设计文档 §7.1 要求两者对照），必须在流式之前完成并钉住。
3. **先适配器后接线（M4）**：facade 分支、归一化矩阵、真实端点测试都依赖一个完整的
   适配器；真实端点测试放最后是因为它需要人工持 key 跑 `#[ignore]`，不阻塞离线开发。
4. **先行为后文档（M5）**：`DESIGN.md` §1.1 修订与 capability-matrix 的实测一节依赖
   前面里程碑的最终行为与实测结论；每个里程碑的 review 任务核对正确性与完整性后才许
   勾销。

## 里程碑

### M1：适配器骨架与请求侧

落地库内前置触点与可编译的适配器骨架，完成 `ChatRequest` → chat/completions wire 的
完整请求映射（设计文档 §4.2）。

- `ProviderId::OpenAiChat` 变体（`src/model/extras.rs:14`）、
  `OPENAI_CHAT_DEFAULT_CAPABILITY` 静态（`src/client/capability.rs`，比照 :77 的
  `OPENAI_RESP_DEFAULT_CAPABILITY`）、`src/adapter/mod.rs` 注册。
- `OpenAiChatAdapter { http_client, endpoint }` 结构体 + `new()` / `with_http_client()`
  + `Clone + Debug`，`capability()` 返回新静态；`chat()`/`chat_stream()` 的 stream 标志
  互斥校验（先例 `openai_resp/response.rs:53-57`）。
- 请求映射：system 首条消息、`reasoning_content` 原样回放（§5.1 统一安全默认）、
  `tool_calls` 嵌套形状（arguments 为 JSON 字符串）、`tool` 角色消息扁平化、
  `stream_options: {"include_usage": true}` 注入、`provider_extras` 最后合并。
- 请求单测：`json!` 精确比对完整 body，含带工具调用多轮历史中 assistant 消息完整携带
  `reasoning_content` + `tool_calls` 的关键用例（§5.1 规则）。

重点文件：`src/model/extras.rs`、`src/client/capability.rs`、`src/adapter/mod.rs`、
`src/adapter/openai_chat/{mod.rs,request.rs,request/input.rs,request/tests.rs}`，
模板 `src/adapter/openai_resp/` 对应文件。

### M2：非流式响应侧

完成 chat/completions 响应 → 归一化 `Response` 的解析与非流式 `chat()`（设计文档 §4.3）。

- wire 类型 + `parse_response`：`object == "chat.completion"` 校验、取 `choices[0]`、
  `content`/`reasoning_content`/`tool_calls` → `ContentBlock`、arguments 字符串解析
  （失败保留原文进 extra）、`finish_reason` 全表映射为 `Normalized<StopReason>`、
  未建模字段落 `Response.extra`。
- 非流式 `chat()`：`execute_json_response` + `map_transport_error` 复用。
- fixtures（文本 / 工具调用 / 含 `reasoning_content`）+ 解析单测 + 一次性
  `TcpListener` transport 测试（状态码/内容类型/错误映射）。

重点文件：`src/adapter/openai_chat/{response.rs,response/convert.rs,response/tests/}`，
模板 `src/adapter/openai_resp/{response.rs,response/convert.rs,response/tests/}`。

### M3：SSE 流式

完成 chunk 流 → `StreamEvent` 的状态机与 `chat_stream()`（设计文档 §4.4）。

- `stream/wire.rs` chunk 视图 + `data: [DONE]` 哨兵特判（JSON 解析前终止）。
- `stream/decoder.rs`：`SseNormalizer` 绑定（约 30 行，照 `openai_resp/stream/decoder.rs`）。
- `stream/normalizer.rs`：`delta.content` → 文本增量；`delta.reasoning_content` →
  `BlockKind::Reasoning` + `Delta::Reasoning`；`tool_calls[]` 按 `index` 键控增量 →
  `BlockStart(ToolInput)` + `Delta::Json` + `BlockStop`（绝不中途解析 JSON，`BlockId`
  用位置派生稳定 id）；末 chunk `finish_reason` → `MessageStop`；空 `choices` 的 usage
  chunk → 单段加性 `Usage`；EOF 无 `[DONE]` → `incomplete_error`。
- 流式 fixtures（脱敏录屏）：纯文本、多 `index` 并行工具调用、`reasoning_content`、
  `include_usage` 终态 chunk；不规则字节分块 `[1,2,7,3,19,5,11]` 喂 normalizer；
  `Accumulator` 折叠结果与 M2 非流式解析对照；`[DONE]`/EOF 错误路径；transport 测试。

重点文件：`src/adapter/openai_chat/stream/{mod.rs,decoder.rs,wire.rs,normalizer.rs,tests/}`，
模板 `src/adapter/openai_resp/stream/` 与 `src/adapter/anthropic/stream/normalizer.rs:423-424`
（位置派生 block id 先例）。

### M4：facade 接线与集成

把适配器接进 facade 与测试矩阵，补真实端点回归（设计文档 §6、§7）。

- `src/facade/chat.rs:387` `client_for_provider` 加分支；`src/facade/config.rs` 新增
  Bearer 风格构造器（如 `openai_chat_from_env`，读 `OPENAI_CHAT_BASE_URL` /
  `OPENAI_CHAT_API_KEY`；现有 `openai_from_env` :109-117 是 Azure 风格，不可复用）；
  `src/lib.rs:16-17` 协议列表文档。
- `tests/normalization/config.rs:20` 注册新 `Provider` 分支，纳入跨 provider 归一化
  矩阵。
- `tests/integration_openai_chat.rs`（`#[ignore]`，Option 模式缺 env 跳过）：DeepSeek
  （`DEEPSEEK_API_KEY`，可选 `DEEPSEEK_BASE_URL`/`DEEPSEEK_MODEL`）与 vLLM
  （`VLLM_BASE_URL`，可选 `VLLM_API_KEY`/`VLLM_MODEL`）两套配置；DeepSeek 用例含思考
  模式多轮 + 工具调用（验证 §5.1 的 400 规则与回放策略），顺带实测 §5.2 的 vLLM
  `reasoning_content` 回放兼容性待验证项。

重点文件：`src/facade/chat.rs`、`src/facade/config.rs`、`src/lib.rs`、
`tests/normalization/config.rs`、`tests/integration_openai_chat.rs`，模板
`tests/integration_openai_resp.rs:24-54`。

### M5：文档同步与收尾

决策反转落档，重复实现收口（设计文档 §1、§8、§7.5）。

- `DESIGN.md` §1.1：修订协议清单与「不支持 chat/completions」决策（必须做）；DeepSeek、
  vLLM 从 Anthropic 协议归类移出（`DESIGN.md:15`）。
- `docs/capability-matrix.md`：协议级默认值表加 chat/completions 列 + DeepSeek/vLLM
  实测一节（记录 M4-3 的实测结论，含 vLLM 回放待验证项的结果；环境缺失未实测则如实
  标注）。
- `README.md`（provider 选择、示例、ignored 测试命令）、`AGENTS.md`（`src/` 布局、新增
  env 变量）、`docs/client-layer-references.md`（参考分工总表加一行，可参考
  `async-openai` 的 chat 模块）。
- 可选收尾：把 `tests/agent_external_managed_real_e2e.rs:245-442` 与
  `tests/agent_external_real_e2e.rs:144-184` 的手搓 DeepSeek 客户端换成本适配器，
  删除重复代码。

## 完成定义

每个里程碑的 review 任务必须确认：

1. 该里程碑覆盖的设计文档条目（§号）逐条核实已落地或明确降级（降级 = 文档与实现一致
   地承认现状），无半截实现。
2. 全部门禁通过：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

3. 默认测试离线可跑，不依赖网络/真实 key；真实端点测试 `#[ignore]` 且缺环境干净跳过。
4. 行为变更同步更新拥有该行为的文档（M5 集中收口，但 M1–M4 中触及既有文档口径的改动
   随任务同步，不留到 M5 补）。
5. 跨里程碑验收线索：M2 完成后非流式解析可作为 M3 流式折叠的对照基准；M4 完成后
   facade 用户可用 `ProviderConfig::openai_chat_from_env()` 一行接入 DeepSeek；M5 完成
   后 `DESIGN.md` 不再有与本适配器矛盾的「不支持」表述。
