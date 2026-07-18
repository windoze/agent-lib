# 执行计划：M1-2 默认 HTTP 超时 + 错误路径 body 读取上限（H-SEC-2）

## 任务来源
- `TODO.md` 第一个未完成任务：**M1-2**（M1-1 已 DONE）。
- 审查条目：`docs/review-2026-07.md` 的 H-SEC-2，修复后标注 `✅ 已修复（M1-2）`。

## 任务要求（TODO.md M1-2）
1. `AnthropicAdapter::new()` / `OpenAiRespAdapter::new()` 默认 client 带 `connect_timeout`（建议 10s）。
   注意：**不**用 `Client::timeout()`（会覆盖整个 body 读取，误杀长 SSE 流）。
2. 非流式 `chat()`：请求 future 整体包默认总超时（建议 10 min）。
3. 流式 `chat_stream()`：只对"建立连接 + 收到响应头"阶段设超时（建议 10 min 同口径，可复用同一常量；body 流不设总超时）。
4. 错误 body 读取（非 2xx，共 4 处）：大小上限 1 MiB（截断后标注 `[truncated]`）+ 独立 30s 超时。
   建议实现为共享 helper：分块读到上限即停 + `tokio::time::timeout` 包裹。
   - `src/adapter/anthropic/stream/mod.rs:48`
   - `src/adapter/openai_resp/stream/mod.rs:47`
   - `src/adapter/anthropic/response.rs:67`
   - `src/adapter/openai_resp/response.rs:67`
   - M8 才做代码收敛，本任务各处自修（但同一个 helper 函数可以先放一处 pub(crate) 共用？——TODO 说"4 处错误路径行为一致（M8 才做代码收敛，本任务先各自修）"。折中：helper 是新增代码，不算收敛既有重复；为行为一致性与可测性，放一个共享 helper（如 `src/adapter/http_util.rs` 或 client 层），4 处调用。这是新代码而非搬迁旧代码，符合任务意图。实施时再定位置。）
5. 文档：在 adapter 文档（`AnthropicAdapter::new` / `OpenAiRespAdapter::new` rustdoc）写明默认超时值与 `with_http_client` 覆盖方式。

## 验证条件
- 单元测试：错误 body helper 输入超长流 → 截断 + `[truncated]` 标注（离线内存 stream）。
- `cargo test --all --all-targets` 全过，无挂起。

## 步骤
1. 阅读 4 个错误路径与两个 adapter 的 `chat()`/`chat_stream()` 实现。
2. 设计 helper：`read_error_body(response) -> Result<String>`，内部：
   - `tokio::time::timeout(30s, ...)` 包裹分块读取循环；
   - `bytes_stream()` 逐 chunk 累积到 1 MiB 上限，超出即停并标注 `[truncated]`；
   - 返回 `String::from_utf8_lossy`。
   常量：`DEFAULT_CONNECT_TIMEOUT = 10s`、`DEFAULT_REQUEST_TIMEOUT = 10min`（chat 整体 + stream 建连阶段）、`ERROR_BODY_READ_TIMEOUT = 30s`、`ERROR_BODY_MAX_BYTES = 1 MiB`。
3. 修改 `new()`：用 `reqwest::Client::builder().connect_timeout(10s).build()`（build 失败的回退策略需定——builder 错误仅在 TLS 配置等异常时发生；可用 `unwrap_or_else(|_| reqwest::Client::new())` 或 panic 带上下文。倾向 expect 带说明，记录选型）。
4. `chat()`：`tokio::time::timeout(DEFAULT_REQUEST_TIMEOUT, inner)`，超时映射为 `ClientError::Timeout`（需查 ClientError 现有变体）或 `Network`。
5. `chat_stream()`：对"send + 等响应头"阶段包 timeout；返回的 stream 不再包总超时。
6. 4 处错误路径改调 helper。
7. 新增 helper 单元测试（内存 stream：超长截断 + 标注；慢 stream 超时路径若可控则测，否则只测截断——30s 超时测试不可接受，测试需能注入超时时长或只测截断逻辑。设计 helper 时把 timeout 作为参数，生产调用传默认常量，测试传小值）。
8. rustdoc 更新默认超时说明。
9. 门禁：fmt → clippy（含 external features）→ 全量测试 → doc。
10. `docs/review-2026-07.md` H-SEC-2 标注 ✅；TODO.md M1-2 标记 [DONE] + 完成记录。
11. 提交 `[M1-2] ...`，停止。

## 变更记录
- 2026-07-19：M1-2 完成。新增 `src/adapter/http.rs` 共享限值/默认 client/有界错误 body 读取 helper；两个 adapter 的 `new()` 带 10s connect timeout；`chat()` 整体 10min 超时；`chat_stream()` 仅建连+响应头阶段 10min；4 处错误路径统一 1 MiB 截断 + `[truncated]` + 30s 超时。4 条新单元测试。fmt/clippy（含 external features）/全量测试/doc 全绿（doc 首报私有 intra-doc link，改明文后通过）。review 文档 H-SEC-2 标注 ✅，TODO 标记 [DONE]。提交后停止。
