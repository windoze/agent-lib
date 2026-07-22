## Execution Plan — M2-2：非流式 transport 测试（状态码/内容类型/错误映射）

TODO.md 第一个未完成任务：**M2-2**（标题 `[TODO]`）。M2-1 已 `[DONE]`（commit d01f398），
parse_response + chat() 接线已落地，本任务钉死 `chat()` 的传输层行为。

### 任务目标（TODO M2-2）

`response/tests/transport.rs`：本地 `TcpListener` 起一次性服务器，逐个覆盖 6 个用例：

1. 200 + 合法 body → 正常 `Response`；
2. 429 带 `Retry-After` → `ClientError::RateLimited { retry_after: Some(..) }`；
3. 401 → `ClientError::Auth`；
4. 400 + OpenAI context-length 错误 body → `ClientError::ContextLengthExceeded`；
5. 400 + content-filter body → `ClientError::ContentFiltered`；
6. 非 2xx 其它（500）→ `ClientError::Api { status, body }`。

全部离线回环，端口用 `bind("127.0.0.1:0")`；测试秒级完成。

### 复用（零改动）

- `ClientError::from_http_response`（error.rs:61-105）已覆盖 429/Retry-After、408/504、401/403、
  context-length、content-filter 的 OpenAI 拼写——transport.rs 只需断言分类变体，零改动 common/error。
- `common::execute_json_response`（chat() 已在 M2-1 接线）：200→parse_response，非 2xx→
  `ClientError::from_http_response`。
- 模板 `openai_resp/response/tests/transport.rs`（serve_once 一次性 TcpListener + chat_with_timeout）。
- `response/tests/mod.rs` 已有的 `minimal_request()` / `local_endpoint()` / `REAL_TEXT_RESPONSE`
  （M2-1 加了 `#[allow(dead_code)]` 标注「M2-2 transport 接线」，本任务消费后移除 allow，沿用 M1-2→M1-3
  过渡 allow 的移除惯例）。

### 错误分类对照（error.rs:94-104，4xx 分支）

`from_http_response_at`：429→RateLimited；408/504→Timeout；401/403→Auth；
4xx 内：`status==413 || body 含 CONTEXT_LENGTH_MARKERS`→ContextLengthExceeded；
`body 含 CONTENT_FILTER_MARKERS`→ContentFiltered；否则→Api。

- 用例 4（context-length）body 必须含 marker（如 `context_length_exceeded`/`maximum context length`），
  且**不含** content-filter marker（避免误命中 ContentFiltered 先返回——实际顺序是 context-length 先判）。
- 用例 5（content-filter）body 含 `content_filter`，且**不含**任何 context-length marker（否则被先判成 ContextLengthExceeded）。
- 用例 6 用 500（非 4xx），绕开 marker 分支 → Api。

### 实现文件计划

1. 新建 `src/adapter/openai_chat/response/tests/transport.rs`：
   - `serve_once(status, headers, body)`：一次性 TcpListener，断言请求行 `POST /chat/completions HTTP/1.1`。
   - `chat_with_timeout(adapter, request)`：5s 外层超时包裹 `adapter.chat(request)`，防 transport 回归卡死。
   - 6 个 `#[tokio::test]`：精确对应 TODO 6 用例，断言 ClientError 分类变体。
2. `response/tests/mod.rs`：
   - 末尾 `mod parsing;` 后加 `mod transport;`。
   - 移除 `minimal_request` / `local_endpoint` 的 `#[allow(dead_code)]`（transport.rs 已消费，allow 失效）。
   - 收紧 doc 注释（去掉「M2-2 接线」占位说明）。

### 注意

- 请求 body 含 `"stream":false`（OpenAiChatRequestBody.stream 非可选）——mock server 只断言请求行，忽略 body。
- 200 用例用既有 `REAL_TEXT_RESPONSE`（input=13/output=26），复用 parsing 已验证的 fixture。
- rate-limit 用 seconds 形式 `Retry-After: 3`，确定性 `Duration::from_secs(3)`。
- 不复制 openai_resp transport 的 invalid-success-body / stream-guard 用例：前者 parsing.rs 已充分覆盖，
  后者 mod.rs tests 已钉（M1-2）；本任务严格按 TODO 6 用例，避免冗余。

### 执行步骤

1. [x] 读上下文（TODO M2-2 + openai_resp transport 模板 + error.rs + common/http + 既有 openai_chat response/tests）。
2. [x] 建 transport.rs（serve_once + chat_with_timeout + 6 用例）。
3. [x] 改 response/tests/mod.rs（加 mod transport + 移除两个 allow(dead_code)）。
4. [x] `cargo fmt --all`（无 diff）。
5. [x] `cargo clippy --all-targets -- -D warnings`（默认 + external features，全绿）。
6. [x] `cargo test -p agent-lib --lib adapter::openai_chat`（28 通过，0.02s）。
7. [x] `cargo test --all --all-targets`（全量 0 failed，lib 1078 +6 无回归）。
8. [x] `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
9. [x] TODO.md M2-2 标 [DONE] + 完成记录。
10. [进行中] git commit + stop。

### 进度日志

- [x] 上下文读取
- [x] transport.rs（serve_once 一次性 TcpListener 断言 `POST /chat/completions` + chat_with_timeout 5s 包裹 + 6 用例：200/429+retry/401/400-context-length/400-content-filter/500）
- [x] mod.rs（mod transport + 移除 minimal_request/local_endpoint 的过渡 allow(dead_code)）
- [x] 门禁全绿（fmt 无 diff / 默认+external clippy / openai_chat 28 用例 0.02s / test --all 0 failed / doc）
- [x] TODO 标 [DONE] + 完成记录

