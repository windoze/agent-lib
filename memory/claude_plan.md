## Execution Plan — M5-1：文档同步（DESIGN.md 决策反转 + capability-matrix + README + AGENTS.md + client-layer-references）

TODO.md 第一个未完成任务：**M5-1 [TODO]**（M1-1~M4-R 全部 [DONE]）。
这是**纯文档同步任务**，定义来源是设计文档 `docs/openai-chat-api.md` §8 的同步清单。
不新增功能、不改生产代码，只改 `*.md` 文档（+ 顺手核对 `src/lib.rs`/`src/adapter/mod.rs` 注释一致性，纯注释核对）。

### 任务范围（逐条对 TODO M5-1 实现要求）
1. **`DESIGN.md` §1.1 决策反转（必须做）**：协议清单加 chat/completions；删除/修订「不支持」段 →「经 `openai_chat` 适配器支持，方言策略见 `docs/openai-chat-api.md`」；DeepSeek、vLLM 协议归类从 Anthropic 移到 chat/completions。
2. **`docs/capability-matrix.md`**：协议级默认值表加 chat/completions 列（与 `OPENAI_CHAT_DEFAULT_CAPABILITY` 一致）；新增 DeepSeek/vLLM 实测一节（思考模式、400 规则、vLLM 回放兼容性——引用 M4-3 实测结论，vLLM 未实测如实标注）。
3. **`README.md`**：provider 选择段落加 chat/completions；ignored 测试命令加 `cargo test --test integration_openai_chat -- --ignored --nocapture`。
4. **`AGENTS.md`**：`src/` 布局 `adapter/` 描述加 openai_chat；「Required environment」表加 `OPENAI_CHAT_BASE_URL`/`OPENAI_CHAT_API_KEY`/`VLLM_*`（注明可选/跳过语义）。
5. **`docs/client-layer-references.md`**：参考分工总表加一行（可参考 `async-openai` 的 chat 模块）。
6. **顺手核对** `src/lib.rs` 与 `src/adapter/mod.rs` 的协议清单注释与实际一致（M4-1 已改 lib.rs，需确认 mod.rs）。

### 验证条件
- 文档中的命令、env 变量名、文件路径与代码实际**逐条对照，不凭记忆**。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过；`cargo fmt --all` 无 diff。
- 因纯 `.md` 改动（+注释核对），**无需重跑全量测试套件**（沿用 M4-R 绿基线，注明 skip）。

### 关键数据源（实测结论引用，来自 M4-3 完成记录 TODO.md:1252-1301）
- DeepSeek 实测：4 用例全过（非流式 text+usage / 流式 text delta+usage / thinking 模式 Thinking block / **thinking 多轮+工具调用 round-2 回放 reasoning_content+tool_calls 不 400**）。
- 真实 spec 细节 2 条：① chat/completions `tool_choice` 必须嵌套 `{"type":"function","function":{"name":...}}`（非 Responses 扁平形）；② DeepSeek 思考模式拒 `tool_choice` 字段 → 用强指令 system prompt 自然触发工具调用。
- §5.1 400 规则验证成立；thinking_extras passthrough 确认。
- vLLM 未实测（无 `VLLM_BASE_URL`/凭据，2 测试干净 skip）→ 如实标注「待实测」。
- env 约定：DeepSeek `DEEPSEEK_API_KEY`(必需)/`DEEPSEEK_BASE_URL`(默认 https://api.deepseek.com)/`DEEPSEEK_MODEL`；vLLM `VLLM_BASE_URL`(必需)/`VLLM_API_KEY`(缺省 None)/`VLLM_MODEL`；facade `OPENAI_CHAT_BASE_URL`(必需)/`OPENAI_CHAT_API_KEY`(可选)。

### OPENAI_CHAT_DEFAULT_CAPABILITY 字段（capability-matrix 列必须与此一致，来自 M1-1）
`max_context_tokens: None`；`input_modalities: {Text, Image}`；`output_modalities: {Text}`；
`streaming/tool_calling/parallel_tool_calls/reasoning = true`；`prompt_caching/structured_output = false`；
`stop_reasons: {ToolUse, EndTurn, MaxTokens, StopSequence, Refusal}`。

### 执行步骤
1. 读取所有目标文档 + 设计文档 §8 同步清单 + capability 静态实际值（逐条对照，不凭记忆）。
2. 逐文件编辑（小而精准的 patch，每个文件一组改动）。
3. 跑 `cargo fmt --all --check` + `cargo doc --no-deps --workspace`（注释核对可能触及 src/lib.rs/adapter/mod.rs 但 M4-1 已确认 lib.rs，若 mod.rs 需改属注释）。
4. 标记 TODO M5-1 [TODO]→[DONE] + 完成记录。
5. commit。
6. stop。

### 进度日志
- [x] 读取文档 + 设计 §8 + 代码实际值（capability.rs:106-123 确认 OPENAI_CHAT_DEFAULT_CAPABILITY；config.rs/normalization/integration 测试确认 env 名；lib.rs/adapter/mod.rs 注释核对一致无需改）。
- [x] 逐文件编辑（DESIGN.md §1.1 决策反转 / capability-matrix 加列+实测节 / README 三处口径+provider段+ignored命令 / AGENTS adapter+env表 / client-layer-references 加行）。
- [x] 门禁：`cargo fmt --all --check` exit0；`cargo doc --no-deps --workspace -D warnings` Finished+Generated exit0；逐条对照 env/路径/命令 vs 代码全一致。全量 test 套件未重跑（纯 .md，沿用 M4-R 绿基线）。
- [x] TODO M5-1 [TODO]→[DONE] + 完成记录（逐文件+逐条对照+门禁摘要）。commit + stop。
