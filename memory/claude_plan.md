## Execution Plan — M1-R：M1 review（骨架与请求侧正确性核对）

本文件记录本轮（2026-07-23）可执行计划与进度。TODO.md 第一个未完成任务：**M1-R**（标题 `[TODO]`）。

### 任务性质

Review 任务（不新增功能）。对 M1（骨架 + 请求侧）做独立正确性/完整性核对，跑全量门禁，
在本任务下方追加 review 记录（核对结论 + 门禁摘要 + 发现的问题及处置）。

### 核对清单（TODO M1-R）

1. 设计文档 §4.2 映射表逐行对照实现与测试，确认无遗漏行（尤其 `reasoning_content`
   无条件回放与 `stream_options` 注入）。
2. `ProviderId::OpenAiChat`、`OPENAI_CHAT_DEFAULT_CAPABILITY`、模块注册三处触点的
   形状与既有先例一致；capability 各字段与设计文档 §6 描述一致。
3. wire 类型无泄漏（`pub(crate)`/私有）；`Debug` 不泄露密钥。
4. 请求单测覆盖 §7.1 列出的全部关键用例，断言是 `json!` 精确比对而非字段抽查。
5. 跑全量门禁命令（见文件头），全部通过。

### 代码核查结论（已完成，M1 实现与设计文档一致）

- §4.2 表逐行核对，全部命中：
  - system 首条消息（request.rs:67-69）
  - user/assistant 文本（input.rs user/assistant_message_to_wire）
  - assistant Thinking → reasoning_content **无条件回放**（input.rs:95-97,117-119，不依
    赖是否有 tool_call，符合 §5.1 推论）
  - assistant ToolUse → tool_calls[{id,type:function,function:{name,arguments:<JSON 字符串>}}]
    （input.rs:202-216）
  - ToolResult → 独立 {role:tool,tool_call_id,content}；扁平化文本；非 Ok 拼入前缀
    （input.rs:132-164）
  - tools → function 嵌套（input.rs:220-229）
  - stream=true → stream_options.include_usage 注入（request.rs:83-85）
  - provider_extras 最后合并 + mismatch 报错（request.rs:90-103）
  - max_tokens 直接对应（request.rs:49,79）
- 三处触点：ProviderId::OpenAiChat（extras.rs:21，serde open_ai_chat，round-trip
  extras.rs:170）；OPENAI_CHAT_DEFAULT_CAPABILITY（capability.rs:106-123，字段与 §6 全
  一致：text+image 输入、text 输出、prompt_caching/structured_output=false、stop_reasons
  含 StopSequence+Refusal，逐字段测试 capability.rs:230-261）；模块注册 adapter/mod.rs:5；
  client/mod.rs:14-16 已 re-export。
- wire 类型无泄漏：input.rs 全部产出 serde_json::Value（无 wire struct）；request.rs 的
  OpenAiChatRequestBody/StreamOptions 私有；映射 fn 为 pub(super)/私有。
- Debug 脱敏：mod.rs:122-140 测试钉住（不含 secret、含 [REDACTED]，依赖 EndpointConfig Debug）。
- §7.1 关键用例：6 个全用 json! 精确 assert_eq! 比对完整 body；transport/auth/optional
  用例为字段抽查（合理，非 body-shape 用例）。
- facade 耦合（M1-1 因 exhaustive match 而触及）：config.rs:256/289、chat.rs:395 分支在位。

### 待发现的问题

代码核查未发现 spec 偏差或 workaround。最终以门禁输出为准。

### 执行步骤

1. [进行中] 跑全量门禁：
   - `cargo fmt --all`（无 diff）
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
   - `cargo test --all --all-targets`
   - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
2. 把核对结论 + 门禁摘要 + 问题处置追加到 TODO.md M1-R 下方。
3. M1-R 标题 [TODO] → [DONE]。
4. git commit。
5. stop。

### 进度日志

- [x] 读 TODO/设计文档 §4.2/§6/§7.1 + 全部 openai_chat 实现文件
- [x] 代码逐行核查（§4.2 表 / 三触点 / wire 泄漏 / Debug / 测试覆盖）
- [x] 跑全量门禁（fmt + 两套 clippy + test --all + doc 全绿）
- [x] 写 review 记录 + 标 [DONE]
- [ ] commit

### 验证结果摘要（全绿）

- `cargo fmt --all` 无 diff
- `cargo clippy --all-targets -- -D warnings`（默认 + external features 两套）
- `cargo test --all --all-targets` 全部 0 failed（lib 1074，无回归）
- `cargo test -p agent-lib --lib adapter::openai_chat` 12 通过
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`

### 结论

无 spec 偏差/workaround/未调度失败。M1 实现与设计文档 §4.2/§5.1/§6/§7.1 完全一致。
