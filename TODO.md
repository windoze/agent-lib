# TODO：2026-07 审查收口任务单

本任务单对应 [PLAN.md](PLAN.md) 和 [docs/review-2026-07.md](docs/review-2026-07.md)。旧任务单已归档到 [docs/archive/2026-07-19-refine/TODO.md](docs/archive/2026-07-19-refine/TODO.md)。

执行规则：

- 严格按编号顺序实现，除非当前任务明确要求先补充前置信息。
- 每个标题中的 `[TODO]` 表示尚未完成。完成后把 `[TODO]` 改成 `[DONE]`，并在任务下方追加"完成记录"，写明关键实现决策、验证结果和（如有）breaking change。
- 不要跳过每个 milestone 末尾的 review 任务。
- 审查条目编号（H-SEC-1 等）定义见 [docs/review-2026-07.md](docs/review-2026-07.md)；修复后在该文档对应条目上标注 `✅ 已修复（M*-*)` 或 `📄 已降级（文档承认现状，M*-*）`。
- 修改行为时同步修改拥有该行为的文档，至少检查 `README.md`、`AGENTS.md`、`docs/facade-api.md`、`docs/managed-external-agent.md`、`docs/capability-matrix.md`、`docs/conversation-core.md`、`docs/agent-effect-model.md`、`docs/agent-layer.md`。
- 默认测试必须离线可跑，不依赖真实 provider、真实 CLI login、网络或用户本机配置。
- 行号引用自审查时点（2026-07-19），随后续修复可能漂移，以符号名为准。

全量门禁命令（每个 milestone review 必跑）：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

---

## M1：安全与崩溃级修复

### M1-1 [DONE] `EndpointConfig`/`AuthScheme`/两个 LLM adapter 手写脱敏 Debug（H-SEC-1）

上下文：

- `src/client/config.rs:9` `EndpointConfig` 与 `src/client/config.rs:36` `AuthScheme` 均 `#[derive(Debug)]`；`AuthScheme::Bearer(String)` / `AuthScheme::Header { value }` 含明文密钥。
- `src/adapter/anthropic/mod.rs:22-26` 与 `src/adapter/openai_resp/mod.rs:25-29` 均 `#[derive(Clone, Debug)]` 且内嵌 `endpoint: EndpointConfig`——`format!("{adapter:?}")` 会原样打印密钥。
- 参照：`src/facade/` 的 `ProviderConfig` 已做脱敏 Debug（可 grep `REDACTED` 找先例），HTTP 侧认证头已 `set_sensitive(true)`（`src/adapter/anthropic/request.rs:292`、`src/adapter/openai_resp/request.rs:161`）。

实现要求：

- 为 `AuthScheme` 手写 `impl Debug`：scheme 名可见，密钥一律显示 `[REDACTED]`。
- 为 `EndpointConfig` 手写 `impl Debug`：`base_url`/`query_params`/`extra_headers` 可见，`auth` 走脱敏后的 `AuthScheme` Debug。`extra_headers` 若含认证类头（如 `api-key`）也应脱敏，至少对值为 secret 的头做处理或整体标注。
- 两个 adapter 保持 derive 即可（自动继承脱敏后的 `EndpointConfig` Debug）。
- 保留 serde 行为不变（serde 明文 round-trip 是有意设计，`config.rs:33-35` 已有文档警告）。

验证条件：

- 新增单元测试：构造含 `"sk-ant-secret"` 的 `EndpointConfig`，断言 `format!("{:?}")` 不含该子串、含 `[REDACTED]`；两个 adapter 各一条同样的断言。
- `cargo test -p agent-lib --lib client::` 通过；`cargo clippy --all-targets -- -D warnings` 通过。

完成记录：

- `src/client/config.rs`：`AuthScheme` 与 `EndpointConfig` 去掉 derive Debug，改为手写。`AuthScheme` 只显示 scheme 名（`Bearer([REDACTED])` / `Header { name, value: [REDACTED] }` / `None`）；`EndpointConfig` 的 `base_url`/`query_params` 原样可见，`auth` 走脱敏 Debug，`extra_headers` 头名可见、认证类头（`key`/`token`/`secret`/`auth`/`password`/`credential` 大小写不敏感子串）的值显示 `[REDACTED]`，其余值可见。占位符按任务规格用 `[REDACTED]`（facade 既有的 `<redacted>` 风格不动，`ProviderConfig` 保持其自有 Debug）。serde 行为不变（明文 round-trip 仍为有意设计，rustdoc 警告补充了 Debug 脱敏说明）。
- 两个 adapter（`anthropic/mod.rs`、`openai_resp/mod.rs`）保持 derive Debug，自动继承脱敏；各新增 `adapter_debug_redacts_endpoint_credentials` 测试。
- 测试：`auth_scheme_debug_redacts_every_credential_value`、`endpoint_config_debug_redacts_auth_and_sensitive_extra_headers`、`endpoint_config_debug_preserves_serde_and_equality_behavior` + 两个 adapter 断言，全部通过。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`（883 lib 测试在内全部通过）、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 全部通过。
- `docs/review-2026-07.md` H-SEC-1 已标注 `✅ 已修复（M1-1）`。无 breaking change（Debug 输出格式变化不计入 API 稳定性承诺）。

### M1-2 [DONE] 默认 HTTP 超时 + 错误路径 body 读取上限（H-SEC-2）

上下文：

- `src/adapter/anthropic/mod.rs:31`、`src/adapter/openai_resp/mod.rs:34`：`reqwest::Client::new()` 无任何超时，文档把超时推给 `with_http_client` 调用方。
- 错误路径 4 处无条件 `response.bytes().await`：`src/adapter/anthropic/stream/mod.rs:48`、`src/adapter/openai_resp/stream/mod.rs:47`、`src/adapter/anthropic/response.rs:67`、`src/adapter/openai_resp/response.rs:67`。对端保持连接不关闭时永久挂起。
- 注意：reqwest 的 `Client::timeout()` 覆盖整个响应 body 读取，直接设总超时会误杀正常的长 SSE 流，设计时要区分。

实现要求：

- `new()` 构造的默认 client 至少带 `connect_timeout`（建议 10s）；整体读超时通过以下方式实现而非 `Client::timeout()`：
  - 非流式 `chat()`：请求 future 整体包一个默认总超时（建议 10 min，可经 `with_http_client` 覆盖后由调用方自定）。
  - 流式 `chat_stream()`：只对"建立连接 + 收到响应头"阶段设超时，body 流不设总超时。
- 错误 body 读取（非 2xx）加大小上限（建议 1 MiB 截断）和独立超时（建议 30s）：用 `bytes_stream()` 分块读到上限即停，或先 `tokio::time::timeout` 包 `bytes()` 再截断。截断后在 body 末尾标注 `[truncated]`。
- 4 处错误路径行为一致（M8 才做代码收敛，本任务先各自修）。
- 在 `EndpointConfig` 或 adapter 文档中写明默认超时值与覆盖方式。

验证条件：

- 单元测试：错误 body 读取 helper 输入超长流时返回截断结果且带标注（离线，用内存 stream 即可）。
- 现有全部测试通过：`cargo test --all --all-targets`。
- 无挂起的测试（AGENTS.md：任何测试必须远小于一分钟完成）。

完成记录：

- 新增 `src/adapter/http.rs`（crate 私有模块）：集中定义默认限值常量 `DEFAULT_CONNECT_TIMEOUT = 10s`、`DEFAULT_REQUEST_TIMEOUT = 10min`、`ERROR_BODY_READ_TIMEOUT = 30s`、`ERROR_BODY_MAX_BYTES = 1 MiB`、`TRUNCATED_SUFFIX = "[truncated]"`，以及 `default_http_client()`（builder 只设 connect_timeout，`build()` 失败仅可能为 TLS 初始化异常，选型为 `expect` 带说明，已在代码注释记录）和 `read_error_body()`。helper 是**新增共享代码**而非搬迁既有重复实现，四处调用同一实现保证行为一致；M8 收敛时可直接并入公共传输模块。
- 错误 body 读取核心 `read_error_body_bounded(stream, timeout, cap)` 对 stream 泛型化（chunk `AsRef<[u8]>`），生产调 `response.bytes_stream()`，测试用内存 stream + 小超时，无需网络。分块读到上限即停（不 drain 剩余 body），截断后追加 `[truncated]`；超时映射 `ClientError::Timeout`，传输错误映射 `Network`（与既有 `map_transport_error` 口径一致）。
- `AnthropicAdapter::new()` / `OpenAiRespAdapter::new()` 改用 `default_http_client()`（10s connect timeout）；**不**用 `Client::timeout()`（会误杀长 SSE 流）。
- 非流式 `chat()`：拆出 `chat_inner`，外层 `tokio::time::timeout(10min)` 包裹整个请求，超时映射 `ClientError::Timeout`。
- 流式 `chat_stream()`：只对 `execute()`（建连 + 响应头）包 10min 超时；返回的 SSE body 流不设总超时。
- 4 处错误路径（两个 `stream/mod.rs` + 两个 `response.rs`）统一改调 `http::read_error_body()`；错误分类仍走 `ClientError::from_http_response`，截断标注随 body 进入错误消息。
- 文档：两个 adapter 的 `new()` rustdoc 写明四项默认限值与 `with_http_client` 覆盖方式（调用方 client 上更严的超时先生效；10min 相位上限为 adapter 层固定策略，文档如实说明）。
- 测试：`adapter::http::tests` 4 条——超限截断 + `[truncated]` 标注、恰好在 cap 不标注、stalled stream 以 `Timeout` 返回（注入 10ms 超时，瞬完）、默认 client 可构建。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`、`cargo test --all --all-targets` 全过（含 73 条 adapter 测试，无挂起）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 初报私有 intra-doc link 错误，已将 4 处指向 crate 私有常量的链接改为明文后通过。
- `docs/review-2026-07.md` H-SEC-2 已标注 `✅ 已修复（M1-2）`。无 breaking change（`new()` 行为仅增加超时防护）。

### M1-3 [DONE] `Usage` 算术溢出改饱和/错误化（H-SEC-3、facade 报告 M2）

上下文：

- `src/model/usage.rs:138-141`：`checked_add(...).unwrap_or_else(|| panic!(...))`；调用点为 `Usage::merge`（usage.rs:34-46）与 `total_computed`（usage.rs:49-59）。
- 触发链：wire 数据 → `Accumulator::push` 对每条 `StreamEvent::Usage` 调 `merge`（`src/stream/accumulator/mod.rs:146`）；facade `UsageSummary::add_*`（`src/facade/run.rs:249-269`）层层聚合。伪造大计数即可 panic 宿主进程。

实现要求：

- 把 u32 字段加法改为 `saturating_add`（token 计数语义上饱和优于失败），并在文档注明饱和行为。若选择返回 `Result`，需同步改 `Accumulator`/`UsageSummary` 全部调用链——优先选 saturating 以控制爆炸半径。
- `extra` 数值合并（如有同样 panic 路径）一并处理。

验证条件：

- 单元测试：`merge` 两个 `u32::MAX` 级 usage 不 panic，结果为 `u32::MAX`。
- 单元测试：`Accumulator` 连续 push 伪造大计数 Usage 事件，`collect` 正常返回。
- `cargo test -p agent-lib --lib model::usage stream::accumulator` 通过。

完成记录：

- 选型：按任务推荐采用 `saturating_add`（不返回 `Result`，爆炸半径最小，调用链零改动）。理由写入 `merge`/`total_computed` rustdoc：usage 计数来自不可信 wire 数据，伪造计数不得 panic 宿主；饱和方向是多报而非少报，对预算记账是安全失败方向。
- `src/model/usage.rs`：`merge` 六个 u32 计数（input/output/cache_read/cache_write/reasoning/total）与 `total_computed` 的 fold 全部改 `saturating_add`；`checked_add` panic helper 删除。`extra` 合并经查为 `Map::extend` 覆盖语义，无数值加法路径，不存在同类 panic（已核实，无需处理）。
- 测试：`model::usage::tests::merge_saturates_instead_of_panicking_on_overflow`（两个 u32::MAX 级 usage merge 全部饱和为 u32::MAX）、`total_computed_saturates_instead_of_panicking_on_overflow`；`stream::accumulator::tests::folding::forged_oversized_usage_counters_saturate_instead_of_panicking`（连续 push 3 条全 MAX usage 事件后 finish 正常返回饱和值）；`stream::accumulator::tests::collect::collect_saturates_forged_oversized_usage_counters`（异步 `collect` 路径同样正常返回）。
- 验证：`cargo test -p agent-lib --lib -- model::usage stream::accumulator` 25 条全过；全量门禁 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 全部通过。
- `docs/review-2026-07.md` H-SEC-3 已标注 `✅ 已修复（M1-3）`。无 breaking change（仅消除 panic，正常值域行为不变）。

### M1-4 [DONE] `ClientError::Network` 中 URL query 脱敏（H-SEC-4）

上下文：

- 4 份 `map_transport_error` 副本：`src/adapter/anthropic/stream/mod.rs:89`、`src/adapter/anthropic/response.rs:173`、`src/adapter/openai_resp/stream/mod.rs:88`、`src/adapter/openai_resp/response.rs:163`，均为 `ClientError::Network(error.to_string())`。
- reqwest 错误 Display 含完整 URL；`EndpointConfig.query_params`（`src/client/config.rs:44`）可能被部署方放 `?key=` 类凭据。

实现要求：

- 构造 Network 错误时从 `reqwest::Error::url()` 取 URL，把 query 整体替换为 `[REDACTED]`（或仅对 query 值脱敏）后拼进消息；无法取 URL 时回退原文。
- 同时在 `EndpointConfig.query_params` 文档中明确"禁止放置 secret，错误消息中的 query 会被脱敏但不作为凭据保护手段"。
- 4 处行为一致。

验证条件：

- 单元测试：模拟带 `?api-key=secret` 的 transport 错误，断言错误消息不含 `secret`。
- `cargo test -p agent-lib --lib adapter::` 通过。

完成记录：

- 选型：4 份 `map_transport_error` 副本收敛为 `src/adapter/http.rs` 的单一 `pub(crate) fn map_transport_error`（M1-2 已建共享模块，M8 收敛时自然并入公共传输模块）。`is_timeout()` → `ClientError::Timeout` 不变；其余取 `reqwest::Error::url()`，有 query 时把 URL 中 `?` 之后整体替换为 `[REDACTED]`（保留 `#fragment`，不用 `Url::set_query` 以免百分号编码占位符），再在 Display 消息中替换原 URL 子串；query 只能经 URL 文本泄露，故消息不含 URL 原文时 no-op 也安全；无 URL 时回退原文。
- 同类修复：`read_error_body_bounded`（M1-2 新增）内部 bytes_stream chunk 错误的 `Network(error.to_string())` 同样改走 `map_transport_error`，错误 body 读取路径不再可能泄露 URL query。
- 4 处调用点（两个 `stream/mod.rs` 的 `map_err` + 传给 `normalize_sse` 的回调、两个 `response.rs` 的 `map_err`）统一改 `http::map_transport_error`，本地副本删除——行为一致由单一实现保证。
- `EndpointConfig.query_params` rustdoc 补充：禁止放置 secret；错误消息中的 query 会被脱敏，但那是错误输出的缓解措施而非凭据保护手段。
- 测试（`adapter::http::tests`）：`redact_url_query_replaces_entire_query`（含 fragment 保留）、`redact_url_query_leaves_queryless_urls_untouched`、`transport_error_message_redacts_url_query`（真实 connect 失败 `http://127.0.0.1:1/?api-key=secret`，离线瞬时，断言消息不含 `secret`/`api-key`、含 `[REDACTED]` 与 host 上下文）、`transport_error_without_query_keeps_message`（无 query 时消息原文保留、无标记）。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`、`cargo test --all --all-targets`（含 77 条 adapter 测试、8 条 http 测试全过）、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 全部通过。
- `docs/review-2026-07.md` H-SEC-4 已标注 `✅ 已修复（M1-4）`。无 breaking change（错误消息措辞变化不计入 API 稳定性承诺）。

### M1-5 [DONE] external 流读取超时与 launch 超时拆分（H-EXT-1）

上下文：

- 三个 adapter 把 `config.timeout()`（默认 30s，本为 probe/launch 设计）同时用作每行 stdout 读取超时：`src/agent/external/claude_code/adapter.rs:168-169`、`src/agent/external/codex/adapter.rs:222-223`、`src/agent/external/opencode/adapter.rs:236-237`（`read_timeout: config.timeout(), shutdown_grace: config.timeout()`）。
- 读超时实现如 `claude_code/adapter.rs:185-193`：`timeout(self.read_timeout, self.stdout.next_line())` 超时即 `TimedOut` → `SessionLost`。CLI 跑长静默命令（构建/测试）30s 无输出即被误杀。
- 默认常量如 `claude_code/config.rs:34`：`const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30)`。

实现要求：

- 三个 config 各新增独立字段（建议 `read_idle_timeout: Duration`，默认 10 min；`shutdown_grace` 也可独立，默认保持 30s），`timeout()` 保留为 probe/launch 语义。serde 兼容：新字段 `#[serde(default = ...)]`，旧配置可反序列化。
- 三个 adapter 的 session 构造改用新字段。
- 同步 `docs/managed-external-agent.md` 与对应 config 文档，明确三个超时的语义口径。
- codex `exec` one-shot 与 claude/opencode 长会话的静默上限语义如有差异，在文档中说明。

验证条件：

- 单元测试：config 默认值与 serde round-trip（含缺字段旧 JSON 反序列化）。
- 现有 external 测试全过：`cargo test --features "external-claude-code external-codex external-opencode" --all-targets`。
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode" -- -D warnings`。

完成记录：

- 三个 config（`claude_code/config.rs`、`codex/config.rs`、`opencode/config.rs`）各新增两个独立字段：`read_idle_timeout: Duration`（默认 10 min，`DEFAULT_READ_IDLE_TIMEOUT`）与 `shutdown_grace: Duration`（默认 30s，`DEFAULT_SHUTDOWN_GRACE`），均带 `#[serde(default = ...)]` 私有默认函数，旧 JSON 缺字段反序列化落到新默认值而非 30s launch 超时。`timeout()` 保留为 probe/launch 语义，rustdoc 与 struct 文档新增「The three timeouts」节写清三者口径；配套 `with_read_idle_timeout`/`with_shutdown_grace` setter 与 getter，手写 `Debug` 同步补两个字段。
- 三个 adapter session 构造改接线：`claude_code/adapter.rs`（`ClaudeProcessIo::spawn`）与 `codex/adapter.rs`、`opencode/adapter.rs` 的 `read_timeout`/`shutdown_grace` 分别改取 `config.read_idle_timeout()`/`config.shutdown_grace()`；`ClaudeProcessIo`/`CodexProcessTurn`/`OpenCodeProcessTurn` 的 doc comment 同步改述。probe 路径（`*/probe.rs`）仍用 `config.timeout()`，语义不变。
- codex/opencode one-shot 与 claude 长会话的静默上限语义差异已写入文档：同一 per-line 空闲上限，claude 跨整条 session 逐行生效，codex/opencode 在单个 turn 进程内逐行生效（config rustdoc 与 `docs/managed-external-agent.md` §12 新增「三类超时」段落均有说明）。
- 测试：三个 config 各 3 条新断言——默认值含两个新字段、serde round-trip 携带自定义值、旧 JSON（删掉 `read_idle_timeout`/`shutdown_grace` 键）反序列化得到新默认值。
- 文档：`docs/managed-external-agent.md` §12 新增「三类超时（M1-5 拆分）」段落（含 codex/opencode one-shot 语义差异说明），§12/§13/§14 中「每读超时」「在 timeout 内等待优雅退出」等旧措辞改述为新字段口径。
- 范围说明：ACP（`acp/connection.rs` 的 `read_timeout`、`acp/adapter.rs` 的 `shutdown_grace` 也取 `config.timeout()`）不在 H-EXT-1 条目与 M1-5 范围内（审查条目只列三个 CLI adapter），未改动。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`、`cargo clippy --all-targets -- -D warnings`、`cargo test --features "external-claude-code external-codex external-opencode" --all-targets`、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 全部通过。
- `docs/review-2026-07.md` H-EXT-1 已标注 `✅ 已修复（M1-5）`。无 breaking change（新增字段均有 serde default；新增 API 纯增量）。

### M1-6 [DONE] `close()` 按退出码分类 Graceful/Failed（H-EXT-3）

上下文：

- `src/agent/external/claude_code/adapter.rs:198-199`、`codex/adapter.rs:257-258`、`opencode/adapter.rs:271-272`、`acp/connection.rs:188-189`：`Ok(Ok(_status)) => ExternalSessionShutdown::Graceful`，忽略退出码。
- 下游 `src/agent/external/worktree.rs:470` 用 `disposition.leaves_residual_side_effects()` 决定 ephemeral worktree 是否删除/复用——崩溃 session 会把写了一半的 worktree 判为干净。

实现要求：

- 四处统一改为：`status.success()` → `Graceful`；否则 → 表示失败的变体（查看 `ExternalSessionShutdown` 现有变体选合适的，必要时新增带 exit code 的变体，注意其 serde 兼容与 `leaves_residual_side_effects()` 语义）。
- 检查 `ExternalSessionShutdown` 的全部 match 点，确认新分类不破坏现有穷尽匹配。
- 同步 `docs/managed-external-agent.md` §6.4 的关闭分类描述。

验证条件：

- 单元测试（可用 testkit 的 scripted/cassette handler 或 fake 进程）：子进程 exit 0 → Graceful；exit 1 → Failed 类；grace 超时 → ForcedKill 不变。
- external feature 测试与 clippy 全过（命令同 M1-5）。

完成记录：

- 四处 close 站点（`claude_code/adapter.rs`、`codex/adapter.rs`、`opencode/adapter.rs`、`acp/connection.rs`）统一改为 guard 分类：`Ok(Ok(status)) if status.success()` → `Graceful`；`Ok(Ok(_))`（非零退出）→ `Failed`；wait 错误 / start_kill 失败仍为 `Failed`；grace 超时 + 成功 kill 仍为 `ForcedKill`。
- 变体选型：复用现有 `ExternalSessionShutdown::Failed`，**未新增变体**——该 enum 是 `Copy` 分类载体，详细失败文本按设计留在 `ExternalAgentError::ShutdownFailed`；`Failed` 语义（“关闭未干净完成，可能有残留副作用”）正好覆盖非零退出，serde wire 不变，`leaves_residual_side_effects()` 已对其返回 `true`，无需改动。`Failed` 变体 rustdoc 补充说明非零退出也归此类。
- match 点核查：`ExternalSessionShutdown` 的全部使用点（shutdown.rs `label()`、worktree.rs `cleanup` 经 `leaves_residual_side_effects()`、registry/handler/budget/machine 透传）均为非穷尽构造或对现有三变体的完整覆盖，未新增变体故无穷尽匹配破坏。
- 测试：四个模块各新增 `close_classification` 子模块（真实 `sh -c` 短生命周期子进程，按生产 transport 接线）：`exit 0` → `Graceful`、`exit 1` → `Failed`、`sleep 30` + 250ms grace → `ForcedKill`，共 12 条，全部通过（约 0.3s）。
- 文档：`docs/managed-external-agent.md` §12（claude shutdown 段）与 §16（residual side-effect 策略）改为按退出码分类口径；`docs/external-agent.md` §6.4 补记非零退出 → `Failed`；`docs/review-2026-07.md` H-EXT-3 已标注 `✅ 已修复（M1-6）`。三处 struct/trait doc comment（`ClaudeProcessIo`、`CodexProcessTurn`、`OpenCodeProcessTurn`、acp `close`）同步。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`、`cargo clippy --all-targets -- -D warnings`、`cargo test --features "external-claude-code external-codex external-opencode" --all-targets`、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 全部通过。无 breaking change（enum 形状与 serde wire 未变）。

### M1-7 [DONE] M1 review：安全与崩溃级修复收口

检查项：

- 逐条核对 H-SEC-1/2/3/4、H-EXT-1、H-EXT-3 已修复，`docs/review-2026-07.md` 对应条目已标注。
- 无新增 unwrap/panic；无 secret 进入 Debug/错误消息/日志的回归（grep `REDACTED` 相关测试全过）。
- 全量门禁命令通过（见任务单头部）。
- `README.md`、`docs/managed-external-agent.md`、`docs/capability-matrix.md` 如需更新已更新。

完成记录：

- 条目核对：`docs/review-2026-07.md` 六条均已标注 `✅ 已修复（M1-1..M1-6）`；代码点位抽查确认修复在场——`AuthScheme`/`EndpointConfig` 手写脱敏 Debug（`src/client/config.rs`，`[REDACTED]` 共 23 处分布 4 文件）、`src/adapter/http.rs` 共享超时/错误 body 上限/query 脱敏、`src/model/usage.rs` 7 处 `saturating_add`、三 config 的 `read_idle_timeout`/`shutdown_grace` 字段、四处 close 站点 `status.success()` guard 分类。
- 回归扫描：M1 触及文件无新增生产路径 `unwrap()`/`panic!`（`src/adapter/http.rs` 仅测试断言内 2 处 `panic!`）；usage 溢出 panic helper 已删除；secret 脱敏测试随全量套件通过。
- 文档核对：`docs/managed-external-agent.md`（M1-5 超时拆分、M1-6 关闭分类）与 `docs/external-agent.md` §6.4 已随对应任务同步；`README.md` 与 `docs/capability-matrix.md` 不覆盖 M1 修复的内部行为面（超时默认值、脱敏、关闭分类），无需更新。
- 全量门禁：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`、`cargo test --all --all-targets`（exit 0，约 35s，无挂起）、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 全部通过。
- 本任务纯审查，无代码改动，无 breaking change。

---

## M2：external 子进程生命周期正确性

### M2-1 [DONE] kill 升级为进程组级，消除孙进程泄漏（H-EXT-2）

上下文：

- 全部进程管理只有 `start_kill()` / `kill_on_drop(true)`；`grep -rn "process_group\|setsid\|killpg" src/` 零命中。
- 典型位置：`src/agent/external/claude_code/adapter.rs:201-204`（`start_kill` + `wait`）。CLI 经 Bash 工具拉起的孙进程（构建/测试/dev server）kill 后成孤儿，可能继续写已被删除的 worktree。

实现要求：

- spawn 时在 unix 上 `process_group(0)` 使子进程自成进程组（tokio `Command::process_group`，或 `pre_exec` + `setsid`）；kill 路径先向进程组发 SIGTERM、grace 后 SIGKILL（可用 `nix`？——注意非目标"不引入新的默认依赖"：优先考虑 `std::process::Command` 无法做，则只在使用 tokio 的 unix 路径用 `unsafe` libc kill 或现有依赖传递；方案选型写入完成记录）。
- Windows 无进程组语义：保持 `start_kill` 现状并在文档注明平台差异。
- 三 CLI adapter + ACP connection 行为一致。
- 同步 `docs/managed-external-agent.md` §16/§6.4 的清理保证描述。

验证条件：

- 集成测试（可 `#[ignore]` 之外的离线形态）：spawn 一个会再 fork 子进程的 shell 脚本（如 `sh -c 'sleep 300 & sleep 300'`），走 force-close 路径后断言进程组内无存活进程（用 `kill(-pgid, 0)` 或 `/proc`/`ps` 检查，注意 macOS/Linux 兼容）。
- external feature 测试与 clippy 全过。

完成记录：

- 方案选型：spawn 侧用 `tokio::process::Command::process_group(0)`（tokio 1.52 自带，无新依赖）；信号侧用 `unsafe libc::kill(-pgid, sig)`。`libc` 新增为 **unix-only optional 依赖**（`[target.'cfg(unix)'.dependencies]`，`default-features = false`），仅被四个 `external-*` feature 经 `dep:libc` 启用——默认构建不编译它（已在 Cargo.lock 中经 tokio 传递存在，非重依赖）；`nix` 未引入（其 signal 模块同样只是 `kill(2)` 封装，收益不抵新依赖）。AGENTS.md「Build, lint, and test」段的依赖表述同步更新。
- 新增 crate 私有模块 `src/agent/external/process_group.rs`（M8-2 收敛时并入共享 process 模块）：`configure_managed_command()`（unix 下 `process_group(0)`，非 unix no-op）与 `force_kill()`——unix 下先向整个进程组发 SIGTERM，2s 固定升级窗口（`SIGTERM_ESCALATION_GRACE`，独立于已耗尽的 shutdown_grace，使 force-close 有界）内未退出再发 SIGKILL；`ESRCH`（leader 已在超时瞬间退出）直接落回收割；信号投递失败（如 EPERM）回退 `start_kill` 保证 leader 必死。非 unix（Windows 无进程组语义）保持 `start_kill` 只杀直接子进程，平台差异写入模块 rustdoc 与 `docs/managed-external-agent.md` §16。
- 四个点行为一致：三 CLI adapter 的 spawn（`ClaudeProcessIo::spawn`、`SystemCodexLauncher::launch`、`SystemOpenCodeLauncher::launch`）与 ACP `TokioProcessLauncher::launch` 统一调 `configure_managed_command`；四个 close 的 `start_kill` 阶梯统一换成 `process_group::force_kill(...).await`，`ForcedKill`/`Failed` 分类（M1-6）不变。
- 测试：`process_group::tests` 3 条 unix 单测（configured child 是进程组 leader；`sleep 300 & sleep 300` force_kill 后组内无存活；`trap '' TERM` 的 leader 经 SIGKILL 升级收割）；四个 `close_classification` 模块各新增 `force_close_kills_the_whole_process_group`——`sh -c 'sleep 300 & sleep 300'` 走 250ms grace 超时 close，断言 `ForcedKill` 且 `kill(-pgid, 0)` 返回 `ESRCH`（共享断言 `assert_process_group_reaped`，20ms×100 重试吸收 init 异步收割延迟，macOS/Linux 均用 `kill(2)` 无 `/proc` 依赖）；四个 spawn_sh 测试 helper 同步接上 `configure_managed_command` 与生产一致。全部离线、秒级完成。
- 范围说明：三个 probe（`wait_with_output` 有界一次性进程）与 `kill_on_drop` 兑底路径不在本任务范围——后者只杀直接子进程的限制已写入模块 rustdoc 与 §16 文档。
- 文档：`docs/managed-external-agent.md` §16 新增「进程组级 kill（M2-1 / H-EXT-2）」节（含 Windows 平台差异、close 路径覆盖范围），「三类超时」段与 §12 shutdown 段的 `start_kill` 措辞改为进程组级强杀，§能力矩阵 cancel 行同步；`docs/review-2026-07.md` H-EXT-2 已标注 `✅ 已修复（M2-1）`；AGENTS.md Safety properties 新增 Process-group kill 条目。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`、`cargo test --all --all-targets`、`cargo test --features "external-claude-code external-codex external-opencode external-acp" --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 全部通过。
- 无 breaking change（仅进程管理内部行为收紧；feature 新增 unix-only `libc` optional 依赖）。

### M2-2 [TODO] resume 时用持久化高水位播种 decoder seq（M-EXT-1）

上下文：

- resume 路径构造全新 decoder（`src/agent/external/claude_code/adapter.rs:645-651`，`ClaudeCodeSession::new` → `next_seq = 0`）；codex/opencode 同构。
- machine 去重用持久化旧高水位：`src/agent/external/machine.rs:732-741` `observe()`：`observed.seq > consumed` 才保留。恢复后 seq 从 0 重启 → 全部 observation 被静默丢弃直到爬过旧水位。
- 设计要求见 `docs/managed-external-agent.md` §5.5（"seq spans the whole session"）。

实现要求：

- decoder（或 session）提供以指定 `next_seq` 起始的构造方式；adapter resume 时用 `ExternalSessionRef.last_event_seq`（或等价持久化字段）播种。
- 三个 adapter 行为一致；补注释说明 seq 单调性依赖。
- 若 ACP 路径有同类问题一并修。

验证条件：

- 单元测试：模拟"已消费到 seq=50 → resume → 新事件"场景，断言 resume 后第一个 observation 不被 `observe()` 丢弃（machine 层测试，参考 `src/agent/external/machine/tests.rs` 现有模式）。
- external feature 测试与 clippy 全过。

### M2-3 [TODO] decoder 错误消息与"不折叠原文"承诺对齐（M-EXT-3）

上下文：

- 三个 decoder 文档承诺（`src/agent/external/claude_code/decoder.rs:48-49`）："Every diagnostic is a fixed string; no prompt text, tool input, or credential is ever folded into an error message."
- 实现违背：`claude_code/decoder.rs:517-521` 把 `frame.get("result").and_then(Value::as_str)` 原文塞进 `ExternalAgentError::Runtime.message`；`codex/decoder.rs:446-453`（`turn.failed` 的 `error.message`）、`opencode/decoder.rs:496-509`（`error.data.message`）同构。该文本受模型输出影响，可含模型读到的任意文件内容。

实现要求：

- 选定方案（写入完成记录）：(a) `message` 固定字符串 + 把原文放入一个单独的、标注"may contain runtime output, do not log blindly"的字段；(b) 截断 + 脱敏后保留。推荐 (a)，与文档承诺一致。
- 检查 `error.to_string()` 的 Display 实现与 `machine.rs:713` 的使用点，确认敏感原文不会经 Display 进入 cursor/日志。
- 同步 decoder 模块文档。

验证条件：

- 单元测试：构造含敏感字样（如 `API_KEY=...`）的 runtime error frame，断言 `error.to_string()` 不含该字样。
- external feature 测试与 clippy 全过。

### M2-4 [TODO] Codex/OpenCode prompt 传参加固（M-EXT-4）

上下文：

- `src/agent/external/codex/adapter.rs:121-124`：`args.push(prompt.clone())` 作为最后位置参数，前无 `--`；`opencode/adapter.rs:127-143` 同。`-` 开头的用户消息会被 clap 当 flag；prompt 对本机 `ps` 可见。Claude Code 走 stdin frame 无此问题。

实现要求：

- 在 prompt 前插入 `"--"` 分隔符（先用 `codex exec --help` / `opencode --help` 或文档确认两 CLI 支持 `--`；记录确认结果）。若某 CLI 不支持，改为其支持的等价机制（如 stdin）并在代码注释说明。
- `ps` 可见性问题：评估 stdin 传 prompt 的可行性；若维持 argv，在 `docs/managed-external-agent.md` 安全节明确记载该暴露面与理由。

验证条件：

- 单元测试：构造以 `--model` 开头的 prompt，断言生成的 argv 含 `--` 分隔且 prompt 原样位于其后。
- external feature 测试与 clippy 全过；`#[ignore]` real e2e 手工抽查一次（如环境允许）。

### M2-5 [TODO] 决策点后 reap 子进程 + prelude 总时限与取消（M-EXT-5、M-EXT-6）

上下文：

- `codex/adapter.rs:559-561`：decoder 在 `turn.completed` 行即返回 decision，不读到 EOF；`codex/adapter.rs:462-464`（opencode 同）：`let _ = old.close().await;` 丢弃 disposition——ForcedKill 被吞，不进 trace 也不影响 worktree 判定。
- `claude_code/adapter.rs:298-311`（codex/opencode 同）：`begin()` 的 prelude 循环 `while self.decoder.session_id().is_none()`，per-line timeout 每行重置、无取消检查、无总 deadline；`advance()` 循环反而有 `ctx.is_cancelled()`（adapter.rs:482）。

实现要求：

- close disposition 不再 `let _ =` 丢弃：记录进 trace/日志，并在判断 worktree 残余副作用时纳入（与 M1-6 的分类联动）。
- prelude 循环加总 deadline（用 M1-5 的 launch timeout）与 `ctx.is_cancelled()` 检查；超时/取消走正常错误路径。
- 三个 adapter 行为一致。

验证条件：

- 单元测试：fake CLI 持续吐非 init 帧时 `begin()` 在 deadline 内返回错误而非挂起。
- 单元测试：close 超时被强杀的场景 disposition 被观测到（断言 trace 或返回值包含 ForcedKill）。
- external feature 测试与 clippy 全过。

### M2-6 [TODO] worktree cleanup 使用记录的 base repo（M-EXT-7）

上下文：

- `src/agent/external/worktree.rs:477-485`：`self.git.remove_worktree(prepared.worktree.path(), prepared.worktree.path())`——`PreparedWorktree` 不保存 base repo 路径，cleanup 把 worktree 自己当 `-C` 目录（靠 git 向上发现 gitdir），目录部分损坏/被移动时失败。

实现要求：

- `PreparedWorktree` 增加 base repo 路径字段（创建时已知），cleanup 用它作为 `-C`。
- serde 兼容：`PreparedWorktree` 若参与持久化，新字段 `#[serde(default)]` 并在缺省时回退旧行为。

验证条件：

- 单元测试：创建 worktree 后将其 `.git` 文件/目录模拟损坏（或移动），cleanup 仍能经 base repo 完成 `git worktree remove`（离线临时目录即可）。
- external feature 测试与 clippy 全过。

### M2-7 [TODO] `ExternalSessionPolicy` 与 `WorktreeManager` 接入（M-PROM-5）

上下文：

- `src/agent/external/machine.rs:584-585` 每请求携带 `policy: *spec.session_policy()`，但 `grep request.policy src/agent/external` 只命中测试——请求级 `permission_mode`/`max_turns` 静默失效。
- `GitWorktreeManager`（worktree.rs）在 `src/` 生产路径无人调用；AGENTS.md 宣称的 worktree 隔离只存在于 `examples/support/managed.rs:367-394`。
- `opencode/config.rs:254-257` 注释承认漏传 `--dir` 会写进启动 checkout。
- 适配器当前从构造期 `config.working_dir()`（`claude_code/adapter.rs:149-150`）取工作目录，而非 `request.worktree`；隔离要生效必须把 `WorktreeManager::prepare` 产出的路径喂给会话工作目录。

决策（已定）：`isolation` 采用**库内接线**方案——把 `GitWorktreeManager` 接进 `src/` 生产路径，让 policy 的 `isolation` 字段真正生效，而非退回"隔离是宿主责任"。`permission_mode`/`max_turns` 两字段仍按原口径处理（请求级覆盖 / CLI flag 或 machine 强制），未接入部分显式拒绝或文档标注未实现，不允许继续静默忽略。

实现要求：

- **接入点选定 registry 层**（`ExternalSessionRegistry`，`src/agent/external/registry.rs`）：它是持有 adapter、统一驱动 `start`/`resume`/`cleanup` 的唯一 choke point。
  - `ExternalSessionRegistry` 持有一个 `Arc<dyn WorktreeManager>`（构造时注入，默认 `GitWorktreeManager::new()`）。
  - `get_or_start`（registry.rs:184）在 `adapter.start` 之前先按 `request.policy().isolation` 调 `WorktreeManager::prepare(agent_id, &request.worktree, isolation)`，把产出的 `PreparedWorktree` 路径作为该会话的工作目录传给 adapter（贯通到 `config.working_dir()` / opencode 的 `--dir`，一并修掉 `opencode/config.rs:254-257` 的漏传）。
  - `cleanup`/`cleanup_agent`（registry.rs:255、279）在会话关闭后按 `ExternalSessionShutdown` 调 `WorktreeManager::cleanup(prepared, disposition)`；registry 需记住每个 live session 对应的 `PreparedWorktree`。
- `permission_mode`：请求级覆盖 adapter 构造期 config；`max_turns`：传 CLI flag 或 machine 强制。未接入的字段必须使 machine/adapter 显式拒绝或在文档标注未实现。
- 更新 AGENTS.md 与 `docs/managed-external-agent.md`：worktree 隔离改为库级保证的准确描述（示例 `examples/support/managed.rs` 不再是唯一来源）。
- 同步 `docs/capability-matrix.md`。

验证条件：

- 决策与理由写入完成记录；文档措辞与实现一致。
- 单元测试覆盖：请求级 permission_mode 覆盖生效；registry 在 start 前调 `prepare`、在 cleanup 时按 disposition 调 `cleanup`（可用现有 `ScriptedGit`/`MockAdapter` 测试替身，见 worktree.rs:532、registry.rs:479）。
- external feature 测试与 clippy 全过。

### M2-8 [TODO] M2 review：external 生命周期收口

检查项：

- 逐条核对 H-EXT-2、M-EXT-1~7、M-PROM-5 状态，`docs/review-2026-07.md` 已标注。
- 重点复验：force-close 后无存活孙进程；resume 后事件流无缺口；崩溃 session 不再判 Graceful。
- 全量门禁命令通过（含 external-acp feature 的 clippy）。
- `docs/managed-external-agent.md`、`docs/capability-matrix.md`、`AGENTS.md` 与实现一致。

---

## M3：Conversation 正确性

### M3-1 [TODO] 禁止在 reverted head 上 compaction（H-STATE-1）

上下文：

- `src/conversation/projection/compaction.rs:251-258`：`apply_compaction` 只校验 `plan.head_turn_count == active_len`，不校验 head 是否等于 lineage 上限。`source_spans()`（compaction.rs:296-312）与 `build_replacement_spans()`（compaction.rs:474-480）只取到 `active_len`。
- 破坏路径：revert_to(3) → compact(head=3) → 新投影只覆盖 0..3 → redo revert_to(5) 成功（`boundary/head.rs:89-91` 不触碰投影）→ `effective_view()`（`projection/mod.rs:485-507`）静默丢 turn 3..5；且无法自愈（再 compact 报 `IncompleteProjection`，snapshot restore 报 `SpanGap`）。

实现要求：

- 在 `validate_compaction_plan_header`（或 `apply_compaction` 入口）增加校验：`active_len == lineage_len`（即 head 在 lineage 末尾）才允许 compaction；否则返回明确错误（新增或复用合适的 `CompactionError` 变体）。
- 错误消息说明"reverted head 上不可 compaction，先 redo 到 lineage 末尾"。

验证条件：

- 回归测试（精确复现报告路径）：5 turn + compact 0..5 → revert_to(3) → apply_compaction 返回新错误而非成功；redo revert_to(5) 后 `effective_view()` 仍含全部 5 turn。
- 现有 projection/compaction 测试全过：`cargo test -p agent-lib --lib conversation::projection`。
- `docs/conversation-core.md` compaction 节补充该约束。

### M3-2 [TODO] `MessageRecord` 增加 `meta` 字段（H-STATE-2）

上下文：

- `src/conversation/persistence/rows.rs:123-133`：`MessageRecord` 只有 `payload: Message`；`rows.rs:351-356` 分解时 `payload: message.payload().clone()` 丢弃 envelope meta。
- meta 来源：`src/conversation/message.rs:70-75` `new_with_meta`，由 `inject_user_message`（`pending/turn.rs:328-332`）写入。
- 现有 e2e 断言（`persistence/tests/e2e.rs:44-47` `assert_eq!(rebuilt_snapshot, snapshot)`）因夹具未用 `inject_user_message` 从未覆盖。

实现要求：

- `MessageRecord` 增加 `meta: Option<MessageMeta>`，serde `#[serde(default)]` 保持旧行数据可反序列化。
- `to_rows`/`into_snapshot` 双向携带 meta；`ConversationMessage` 构造走 `new_with_meta`。
- 检查 `ConversationRows` 文档（rows.rs:3-5）恢复"与 snapshot 描述同一一致点"的承诺。

验证条件：

- e2e 夹具增加一条经 `inject_user_message` 注入的消息（带 source meta），断言 `to_rows → into_snapshot` round-trip 相等。
- `cargo test -p agent-lib --lib conversation::persistence` 与 `cargo test --test conversation_persistence*`（按现有测试目标名）全过。

### M3-3 [TODO] 修复 restore 派生索引的空校验（M-CONV-1）

上下文：

- `src/conversation/persistence/snapshot.rs:566-575`：`ToolCallIndex::rebuild(turns, None)` 同一纯函数调两次再比较，`RestoreError::DerivedIndexMismatch` 不可达。

实现要求：

- 二选一（记录选型）：(a) 删除该校验与不可达错误变体，restore 直接 `rebuild`；(b) 改为真实校验：用 `push_committed_turn` 逐 turn 增量推进一个 index，与全量 `rebuild` 比较。推荐 (a)——纯函数重建本身无校验价值；若选 (b) 需说明增量路径是生产实际使用的路径。
- 若删除变体注意 `RestoreError` 的 `#[non_exhaustive]` 状态与文档。

验证条件：

- 现有 restore 测试全过；若保留 (b)，新增一条"增量与全量不一致"的构造性测试（可通过测试专用 hook 注入）。
- `cargo test -p agent-lib --lib conversation::persistence` 通过。

### M3-4 [TODO] 消除长链递归（restore 校验 + History drop）（M-CONV-2）

上下文：

- `src/conversation/persistence/snapshot.rs:415-440` `visit_parent`：restore 时对不可信快照做环检测，递归深度 = 父链长。
- `src/conversation/history.rs:359-378` `build_restored_node`：restore 重建节点，同样递归。
- `src/conversation/history.rs:352-356`：`RawEntry` cons 链表与 `HistoryNode.parent` 链递归 drop——长会话（数万 turn）析构 `History` 时逐层递归 drop `Arc`，经典栈溢出。

实现要求：

- 两处 restore 递归改迭代（显式栈）。
- 为 `History`（或 cons 链表节点类型）实现手工 `Drop`：循环 `Arc::try_unwrap` 摘链，遇共享引用即停。
- 不改变任何可观察行为。

验证条件：

- 单元测试：构造 100_000+ turn 的链（可用最小 payload），restore 校验与 drop 均正常完成不溢出（注意测试时长 < 1 min，payload 尽量小）。
- `cargo test -p agent-lib --lib conversation::` 全过。

### M3-5 [TODO] rows 引入代次（generation）键支持会话演进（M-CONV-3，方案 b）

上下文：

- `src/conversation/persistence/rows.rs:1077-1092` `diff_single_conversation()`：同 conversation 第二次导出必然 `InsertConflict`（commit/revert 改 `head_turn_count`/`structural_version`；lineage/span 行同序号内容变化）。
- 冲突面精确清单（`insert_set_against`，rows.rs:527-594 的 diff key）：
  - `ConversationRecord`（key = `conversation_id`）：任何 commit/revert 都改 `structural_version`/`head_turn_count` → 冲突。
  - `ConversationLineageTurnRecord`（key = `conversation_id#lineage_sequence`，rows.rs:549）：revert 后新分支在同一序号引用不同 turn → 冲突。
  - `ProjectionSpanRecord`（key = `conversation_id#span_sequence`，rows.rs:584）：compaction 重写 span → 冲突。
  - 不冲突的表：`ConversationTurnRecord`（raw 成员，append-only）、`TurnRecord`/`MessageRecord`/`ToolPairingRecord`/`ArtifactRecord`（不可变事实）、`ProjectionRecord`（内容仅 schema_version）。

决策（已定，方案 b）：为**会演进的三类行**（conversation、lineage 关联、projection span）引入代次键 `generation: u64`，主键变为 `(原 key, generation)`；演进 = 插入新一代行而非更新，保持 insert-only 前提成立。代次直接复用 `ConversationRecord.structural_version`（每次结构性变更递增，天然是代次计数器）。事实表保持原样（不可变、insert-only）。

拆解为 M3-5-1 ~ M3-5-4，按序实施。

#### M3-5-1 [TODO] schema 变更：三类行增加 `generation` 字段 + `to_rows` 写入

实现要求：

- `ConversationRecord`、`ConversationLineageTurnRecord`、`ProjectionSpanRecord` 各增加 `generation: u64`；`ConversationRecord.generation` 恒等于 `structural_version`（在 `to_rows` 构造点断言或直接用其填充）。
- `ConversationLineageTurnRecord`/`ProjectionSpanRecord` 的 `generation` = 该关联/span 生效时的 conversation structural_version（即导出快照时刻的 version——导出走 `Conversation::snapshot` 的一致点语义，直接用当前 version 即可）。
- `CONVERSATION_ROW_SCHEMA_VERSION` 递增；`validate_schema_versions`（rows.rs:597-613）只接受新版本。旧版本行数据的策略：显式报错"schema 过旧，需迁移"（pre-1.0 不提供迁移路径，写入完成记录）；新字段**不要**加 `#[serde(default)]` 静默吞旧数据。
- `to_rows`（`ConversationRowInsertSet::from_snapshot` 路径，rows.rs:320 附近）填充 generation。

验证条件：

- 现有 persistence 测试按新 schema 更新后全过（允许本任务阶段 `into_snapshot`/diff 暂时沿用旧行为，由 M3-5-2/3 完成语义切换——若中间态难以编译，可与 M3-5-2 合并提交）。
- 单元测试：旧 schema_version 的行集反序列化/`into_snapshot` 报明确错误。

#### M3-5-2 [TODO] `into_snapshot` 重组：按最大代次选取演进行

实现要求：

- `ConversationRowInsertSet::into_snapshot`（rows.rs:438 附近）重组规则改为：
  - conversation 行：取 `generation` 最大者；行集必须恰好含该 conversation 至少一行，多行时代次必须可确定唯一最大值。
  - lineage 关联/projection span：只取 `generation == conversation 行最大代次` 的行，按 `lineage_sequence`/`span_sequence` 排序重组；低代次行忽略（它们是历史版本）。
  - 校验：选中代次的 lineage/span 序列必须稠密从 0 开始（复用或扩展现有 `validate_row_owners` 类校验，rows.rs:616-651），owner 校验不变。
- 顺带评估放宽 `insert_set_against` 对 existing 的限制（审查 L-3：existing 必须恰好是单一 conversation 完整行集）：重组改为按 owner 过滤后，existing 可以是多 conversation 行集的子集查询结果。若改动超范围，记录为后续项。

验证条件：

- 单元测试：同一 conversation 两个代次的行混合的行集，`into_snapshot` 选取最大代次重组出正确 snapshot。
- 单元测试：代次稀疏/缺行（如只有 gen 1,3 无 2 的当前代次行）报明确 `InvalidRow`。

#### M3-5-3 [TODO] `insert_set_against` 代次键 diff + 演进场景测试

实现要求：

- `diff_single_conversation`（rows.rs:1077-1092）：key 改为 `(conversation_id, generation)`——同 conversation 不同代次不再冲突，作为新行插入；同代次内容不同仍 `InsertConflict`。
- `diff_rows` 的 lineage/span key 闭包（rows.rs:549、584）加入 generation：`conversation_id#generation#lineage_sequence` / `conversation_id#generation#span_sequence`。
- 事实表 diff key 不变。

验证条件（每条一个测试）：

- commit 演进：导出 gen N → 再 commit → 导出 gen N+1 → `insert_set_against` 成功，insert set 只含新 conversation 行 + 新 lineage 行 + 新 turn/message 事实行。
- revert 演进：导出 → revert → 导出 → 不冲突，新 lineage 行以新代次共存。
- compaction 演进：导出 → apply_compaction → 导出 → 不冲突，新 span 行以新代次共存。
- 同代次篡改：手工改同 generation 行的内容 → 仍 `InsertConflict`。
- round-trip：两次导出的行集合并后 `into_snapshot` 得到最新状态（与 M3-5-2 联动）。
- `cargo test -p agent-lib --lib conversation::persistence` 全过。

#### M3-5-4 [TODO] rows 代次模型文档同步

实现要求：

- `rows.rs` 模块文档（rows.rs:1-40 附近）与 `ConversationRowInsertSet` 文档（rows.rs:233-237）：改写为代次模型描述——事实表 insert-only；演进表按代次版本化；"当前状态 = 最大代次"；`structural_version` 即代次。
- `docs/conversation-core.md` 持久化节同步；DESIGN.md §10 如有相关描述一并核对。
- 文档中给出演进时序示例：commit → gen 1 行集；revert → gen 2 行集；查询当前状态取 gen 2。

验证条件：

- 文档与 M3-5-1~3 实现一致；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。

### M3-6 [TODO] `finish_assistant` 前置块级校验（M-CONV-5）

上下文：

- `src/conversation/pending/turn.rs:195-239` `finish_assistant` 只抽 tool_use id，不检查：同一 assistant 消息内重复 tool_use id（→ `register_tool_calls` 恒 `DuplicateProviderCallId`，`pending/turn/tool.rs:126-138`，永久卡死只能 cancel）；assistant 消息含 Image/ToolResult 块（→ commit 时 `validate_role_sequence` 的 `InvalidRoleBlock`，`validation/sequence.rs:179-197`），而 `ReadyToCommit` 态禁止 `ResumeTurn`/`CommitTurn` cancel（`pending/cancel/prepare.rs:189-193`），只剩 DiscardTurn，整轮作废。

实现要求：

- `finish_assistant` 增加与 commit 同级的块级预检：拒绝 assistant 消息中的非法块类型、重复 tool_use id，尽早返回明确错误（复用或新增合适错误变体）。
- 预检规则与 `validation/sequence.rs` 保持单一来源（抽公共函数或调用同一 validator 的部分），避免两处规则漂移。

验证条件：

- 单元测试：含重复 tool_use id 的 assistant 响应在 `finish_assistant` 即报错；含 Image 块的 assistant 响应同样即时报错；报错后 pending turn 可正常 DiscardTurn 并继续 feed。
- `cargo test -p agent-lib --lib conversation::pending` 全过。

### M3-7 [TODO] `resolved_provider_call_id` 按 claimed 排除语义重推导（M-CONV-6）

上下文：

- `src/conversation/history/index.rs:413-433`：按内容顺序取候选，validation（`validation/pairing.rs:135-163`）保证的是"未被 claimed 的 provider id 中唯一"。构造场景：同一 call_msg 含 ToolUse A、B + 同一 result_msg 含 A、B result + 一个 pairing 显式声明 B、另一个为 None → index 重推导候选 {A,B}，release 下 `expect` 通过但可能取错，debug 下 `debug_assert!` panic。
- 本 crate pending 路径总写 `Some`（`pending/turn/tool.rs:272-282`），但 restore 接受外部快照的 `None` pairing。

实现要求：

- index 重推导实现与 validation 相同的 claimed 排除逻辑：先处理显式 `Some` 的 pairing 并标记 claimed，再推导 `None` 的。
- 或在 restore 时把 `None` 规范化为解析后的 `Some`（单点修复，推荐评估此方案）。
- 消除该路径上的 `expect`/`debug_assert!` 差异行为。

验证条件：

- 单元测试：用上述 A/B 构造场景（手工快照数据）restore，断言解析结果与 validation 语义一致且 debug 构建不 panic。
- `cargo test -p agent-lib --lib conversation::history` 全过。

### M3-8 [TODO] fork 不继承 compaction projection 的文档化（M-CONV-7，方案 a）

上下文：

- `src/conversation/boundary/fork.rs:92-95`：child 无条件 `Projection::raw_for_active_turns`，父已压缩区间回退 raw——compaction 成果（已付费 summary artifact）不随 fork 继承。

决策（已定，方案 a）：fork **不继承** compaction projection，child 前缀一律回退 raw 渲染。本任务为纯文档任务，不改行为。

实现要求：

- `src/conversation/boundary/fork.rs` 文档（fork.rs:40-47 及 `fork_at` 的 doc comment）：明确写出取舍——fork 不继承父的 compacted span 与 artifact，已压缩区间在 child 中回退为 raw 渲染；说明理由（child 有独立 owner/version 身份，compacted span 的 `CheckedTurnRange` anchor 跨 conversation 重验证成本高，raw 回退语义永远正确）；并指出影响（child 首 token 成本可能高于父的投影视图，如需压缩应对 child 重新 compact）。
- `docs/conversation-core.md` §7（compaction/projection 相关节）补同一说明。
- 若 DESIGN.md 的 fork 段落（§Revert / Fork）需要一句交叉引用，一并补上。

验证条件：

- 文档措辞与 `fork.rs:92-95` 实现一致（人工核对）。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- `cargo test -p agent-lib --lib conversation::boundary` 全过（无行为变化，原样通过）。

### M3-9 [TODO] M3 review：Conversation 正确性收口

检查项：

- 逐条核对 H-STATE-1/2、M-CONV-1/2/3/5/6/7 状态，`docs/review-2026-07.md` 已标注（M-CONV-3 以 M3-5-1~4 全部落地为准，重点复验演进场景二次导出不冲突）。
- 重点复验：M3-1 回归测试（revert→compact→redo→effective_view 完整）；rows round-trip 含 meta；10 万级链不栈溢出。
- `docs/conversation-core.md` 与实现一致。
- 全量门禁命令通过。

---

## M4：Agent 状态机与 drive 语义

### M4-1 [TODO] 修复 `blackboard_read` 丢弃正文 + 补 mailbox 读工具（H-STATE-6）

上下文：

- `src/agent/collab/tools.rs:587-598`：`blackboard_read` 读出 `messages` 后只 `messages.len()` 计数即丢弃，黑板对模型只写不读。
- mailbox 工具面只有 `send_message`，无读收件箱工具（collab/tools.rs 全文无 mailbox_read）。
- 对照：`plan_read`（tools.rs:511-525）返回了状态摘要（但也缺 owner/depends_on，可一并补齐）。

实现要求：

- `blackboard_read` 返回消息正文（含 author/seq/内容），注意输出长度控制（截断或分页参数），与现有工具的输出风格一致。
- 新增 mailbox 读工具（收件箱读取，语义与 `Mailbox` API 对齐），注册进同一工具组。
- 评估 `plan_read` 是否补 owner/depends_on 字段（低成本则一并做）。

验证条件：

- 单元测试：post 两条消息后 `blackboard_read` 返回内容包含两条正文；mailbox send 后读工具能取到。
- `cargo test -p agent-lib --lib agent::collab` 全过。
- `docs/agent-layer.md` §6.4 如需更新则同步。

### M4-2 [TODO] `AwaitingReconfig` 期间的新 reconfigure 不再静默丢失（H-STATE-5）

上下文：

- `src/agent/machine/default/mod.rs:314-319` `reconfigure` 无 cursor 守卫，随时可入队。
- resume 路径 `mod.rs:890-899`：`PendingReconfig::Commit { application, .. } => finalize_text_commit(step_id, Some((application, records)))` 应用 park 时预 plan 的 application；`apply_reconfig_application`（`src/agent/state.rs:200-208`）无条件 `queued_reconfigs.clear()`。
- 场景：队列 [R1] → park → `reconfigure(R2)` 校验通过入队 [R1,R2] → resume 只应用 A1 并清空队列 → R2 静默丢失。

实现要求：

- 二选一（记录选型）：(a) `reconfigure` 在 `AwaitingReconfig` cursor 下拒绝（返回明确错误，调用方可重试）；(b) resume 时重新 plan 整个队列而非用预存的 application。推荐 (a)，改动小且语义清晰。
- 选 (a) 时注意与 M4-4 软拒绝出口的衔接（若 M4-4 先落地，复用其非破坏性错误通道）。

验证条件：

- 单元测试：复现上述场景，R2 不再静默丢失（被拒绝并可在 resume 后重新提交，或被一并应用）。
- `cargo test -p agent-lib --lib agent::machine` 全过。

### M4-3 [TODO] pivot 重发的 requirement id 与 trace 去重解耦（H-STATE-4）

上下文：

- `src/agent/machine/default/mod.rs:614-623`：pivot 路径用同一 requirement id 重发 LLM 请求（注释明确 "re-emitted under the *same* requirement id"）。
- `src/agent/drive.rs:445-494`：drain 把 requirement id 直接当 trace node id；`TraceHandle::record_node` 对重复 id 返回 `TraceError::DuplicateNodeId`（`src/agent/context/trace.rs:379-382`），经 `?` 中止整个 drain（`drive.rs:425-430`）。
- 广义问题：观测侧（trace）失败杀死实际驱动。

实现要求：

- trace 记录改 best-effort：`record_node` 失败（含 DuplicateNodeId）记录 warning 类痕迹但不中止 drain；或 drain 对 pivot 重发生成派生 node id（如 `<id>#attempt-2`）。选型写入完成记录——注意 effect-model 文档对 trace 完整性的承诺需同步修订。
- 检查 `NeverResumed` 等其他 trace 失败点同样不致命。

验证条件：

- 单元测试：手写 driver 触发 pivot 重发同 id requirement + 开启 trace，drain 不再因 DuplicateNodeId 失败，turn 正常完成。
- `cargo test -p agent-lib --lib agent::` 全过。

### M4-4 [TODO] 引入非破坏性 step 错误出口（软拒绝）（M-ERR-1 及连带项）

上下文：

- `src/agent/machine/default/mod.rs:1048-1058`：任何 `StepError` → `fail_from` → `fail` → `cancel_pending(DiscardTurn)` + Error cursor。stale resume id（mod.rs:706-713）、不合法边界 pivot（mod.rs:576-588）、turn 中途第二条 UserMessage 都会销毁整个 pending turn。
- `fail()` 自身吞错（mod.rs:1021-1030）：转移表（`src/agent/state/cursor.rs:308-344`）无 `(Done|Error) → Error` 边，机器停在 Done/Error 时 fail 的转移静默失败，错误消息丢失。
- `NestedMachine::route_by_id` fallback（`src/agent/machine/nested.rs:266-272`）：未知 id 的 Resume/Abandon 转发给根机 → 走破坏性 fail。
- 步数上限误用终态：`machine/default/tools.rs:593-604` 达 `max_steps` 走 `LoopCursor::Error`，而 `LoopDoneReason::StepLimitReached`（cursor.rs:650）是死变体。
- during-turn reconfig abandon 用 DiscardTurn（mod.rs:978-986），而 tool abandon 用 ResumeTurn 保全工作（tools.rs:701-716）。

实现要求：

- `StepOutcome` 增加软拒绝表达（如 `rejected: Option<StepRejectReason>` 或专用 outcome 变体）：协议违规类输入（stale id、非法边界 pivot、turn 中重复 UserMessage、未知路由 id）被拒绝但机器状态不变。
- `fail()` 的 cancel_pending 与 cursor 转移失败必须显式处理（至少日志 + outcome 标注），不再 `let _ =`。
- 步数上限改用 `LoopDoneReason::StepLimitReached` 正常终态（保留已完成 tool 结果的提交路径，与文本提交一致）。
- during-turn reconfig abandon 与 tool abandon 对齐（优先 ResumeTurn 保全文本响应）。
- 该任务触及 `AgentMachine` trait 的公共契约，属 breaking change，完成记录注明。

验证条件：

- 单元测试：上述四类协议违规输入各自被软拒绝、cursor 不变、pending turn 完好、后续正常输入可继续。
- 单元测试：max_steps 到达后 cursor 为 Done(StepLimitReached) 且已冻结的 tool 结果不丢。
- `cargo test -p agent-lib --lib agent::machine` 与 nested 测试全过。
- `docs/agent-effect-model.md` 的 step 契约节同步。

### M4-5 [TODO] 取消语义：延迟有界 + `TurnDone` 契约修正（M-ERR-2）

上下文：

- `src/agent/drive.rs:405`：只在每批之间检查 `ctx.is_cancelled()`；批次 fulfill 完成后 resume 循环前不复查（drive.rs:414-434）。参考 handler 全部忽略 ctx（`src/agent/drive/reference.rs:91,153` 的 `_ctx`）。
- `drive.rs:405-412`：取消时只对 `pending.first()` 记录 `NeverResumed` 并 abandon，批次其余 requirement 无 trace 记录，违反 effect-model §8 "每个 requirement 恰好以一种方式 settle 并记录"。
- `drive.rs:236-240`：`TurnDone` 文档声称 cursor 是 terminal `Done|Error`，实际取消落点是 `Idle`（`finish_cancel`，`machine/default/mod.rs:991-1004`）；调用方无法用 `is_terminal`（drive.rs:497-499）区分取消与自然结束。

实现要求：

- drain 在 fulfill_batch 返回后、resume 前增加取消复查；参考实现的 handler 把 ctx 传入 LLM 调用（至少支持在等待响应期间被取消，可配合 M1-2 的超时设施）。
- 取消时对批次内全部 outstanding requirement 逐一记录 `NeverResumed` 并 abandon。
- 修正 `TurnDone` 契约：增加 `cancelled: bool`（或独立 outcome 变体），文档与 `is_terminal` 行为一致。
- 评估 `CancelRecovery` cursor 的 restore 坑（`mod.rs:501-508` 的重置只覆盖 Done|Error）：要么纳入重置，要么从 serde 形状排除。

验证条件：

- 单元测试：飞行中的 LLM requirement 期间触发取消，drain 在当前批次 settle 后立即停，不再推进下一批；全部 outstanding requirement 有 trace 记录。
- 单元测试：取消路径返回的 outcome 可与自然结束区分。
- `cargo test -p agent-lib --lib agent::drive` 全过；`docs/agent-effect-model.md` §8 同步。

### M4-6 [TODO] 统一 reconfig 的 resolver 来源，修复默认 resolver footgun（M-ERR-3）

上下文：

- queue 时机器用 `self.tool_registry_resolver` 校验（`machine/default/mod.rs:327-346`），apply 时 driver 用 scope 的 resolver（`drive/reference.rs:183-195`）——两个不同对象，queue 通过 apply 失败 → 已完成 turn 被销毁 + reconfig 留队列下轮再失败。
- 默认 `DeclaredOnlyToolRegistryResolver`（`src/agent/tool.rs:143-152`）对任何 tool set "解析成功"但 `execute` 恒 `UnknownTool`（tool.rs:207-220），且 declarations 由请求集构造使匹配检查恒真——全链路"成功"后每个工具调用开始失败。

实现要求：

- queue 与 apply 使用同一 resolver 实例（或 apply 时由机器持有的 resolver 重新解析），消除"两处决议"窗口。
- 默认 resolver 改为保守失败（未显式配置 resolver 时 reconfig tool set 报错），或在文档显著位置标注其 declared-only 语义；推荐前者。

验证条件：

- 单元测试：配置"queue 通过 apply 失败"的 resolver 组合不再可能（单一来源）；默认接线下 tool-set reconfig 给出显式错误而非假成功。
- `cargo test -p agent-lib --lib agent::` 全过。

### M4-7 [TODO] M4 review：Agent 语义收口

检查项：

- 逐条核对 H-STATE-4/5/6、M-ERR-1/2/3 状态，`docs/review-2026-07.md` 已标注。
- 重点复验：协议违规软拒绝后 turn 完好；取消延迟有界且 trace 完整；协作工具双向可用。
- `docs/agent-effect-model.md`、`docs/agent-layer.md` 同步。
- 全量门禁命令通过。

---

## M5：facade 承诺对齐

### M5-1 [TODO] `run_full` 增加 drop/timeout 安全防护（H-STATE-3）

上下文：

- `src/facade/agent.rs:354`：非流式路径直接 `drain(&mut self.machine, agent_input, &scope, None, &ctx).await?`，无 Drop 守卫。`tokio::time::timeout`/`select!` 包裹 `agent.run(..)` 超时后 machine cursor 停在携带 outstanding requirement 的中间态，下一次 run 以 protocol error 失败——Agent 被一次超时永久毒化。
- 流式路径已有参照：`AgentRunStream::abandon`（`src/facade/agent/stream.rs:496-521`）通过 `StepInput::Abandon` 回收滞留 turn。
- facade 文档声称 "A failed turn discards its uncommitted work inside the machine, so the `Agent` stays usable"（agent.rs:268-269）——只对 `Err` 返回成立。

实现要求：

- 为 `run_full`/`run` 增加 guard：future 被 drop（含 timeout）时通过 RAII guard 或显式 abandon 步骤把 machine 恢复到一致状态（参考 `AgentRunStream::abandon` 的 Abandon 驱动方式；注意 async cleanup 的限制——可能需要同步 best-effort abandon 或下次 run 前的惰性恢复，选型写入完成记录）。
- 文档更新：明确 timeout/drop 后 Agent 仍可用。

验证条件：

- 单元测试：`timeout(short, agent.run(..))` 超时后，下一次 `run` 正常完成；`snapshot` 一致。
- `cargo test -p agent-lib --lib facade::agent` 全过。

### M5-2 [TODO] 审批行为与文档对齐 + 流式 `Done.events` 审批事件补齐（M-PROM-4、M-ADP-3）

上下文：

- 文档承诺 deny 时 run 抛 `FacadeError::ApprovalDenied`（`src/facade/approval.rs:190-193`、`src/facade/error.rs:71-80`）；实际 typed tool 被拒绝时 machine 合成 `ToolStatus::Denied` 回灌模型、run 正常 `Ok`（`src/agent/machine/default/tools.rs:535-542`；测试 `auto_deny_skips_tool_execution` 断言成功）。`ApprovalDenied` 只在 external delegate 路径抛出（agent.rs:369-371）。
- 非流式用 `weave_approval_events(collected.events, recorded_approvals)`（agent.rs:387）织入审批事件；流式终止输出直接 `events: collected.events`（`src/facade/agent/stream.rs:291`），缺审批事件。`RunOutput.events` 文档承诺跨路径一致（`src/facade/run.rs:144-148`）；现有 parity 测试（`facade/agent/tests.rs:1919-2047`）比较的是流 yield 序列而非 `Done.events`。

实现要求：

- 审批：二选一（记录选型）——(a) 改文档，明确 typed tool deny = 合成 Denied result 回灌、`ApprovalDenied` 仅 external delegate 路径；(b) 增加配置让 deny 可中断 run。推荐 (a)（行为本身合理且被测试钉住），同时给 `FacadeError::ApprovalDenied` 文档标注触发范围。
- 事件：流式 `Done.events` 用与非流式相同的 `weave_approval_events` 织入审批事件。
- parity 测试扩展为同时比较 `Done.events`。

验证条件：

- 单元测试：带审批的 run，流式 `Done.events` 含 `ApprovalRequested`，与非流式逐条相等。
- `cargo test -p agent-lib --lib facade::` 全过；`docs/facade-api.md` 审批节同步。

### M5-3 [TODO] 结构化错误 kind 替代字符串匹配分类（M-ERR-5）

上下文：

- `src/facade/agent.rs:1546-1552` `classify_error`：`message.contains("loop step limit")` → `LoopLimitExceeded`，依赖 `src/agent/machine/default/tools.rs:601` 的字面量措辞；同时服务 run_full 与 stream 两条路径（`agent/stream.rs:294`）。

实现要求：

- `LoopCursor::Error`（或机器错误出口）携带结构化 kind（枚举），facade 按 kind 分类。M4-4 落地后基于其错误形状实现。
- 保留 message 作为人类可读补充，但不再参与分类。

验证条件：

- 单元测试：触发步数上限，facade 错误为 `LoopLimitExceeded`；修改内部措辞不影响分类（通过构造直接验证 kind 路径）。
- `cargo test -p agent-lib --lib facade::agent` 全过。

### M5-4 [TODO] facade 暴露 cancel 与 pivot 入口（M-PROM-2 cancel/pivot 部分）

上下文：

- 每次 run 新建私有 `CancellationToken`：`src/facade/agent.rs:298-303, 599-603, 681-685` 的 `RunContext::new_root(...)`；facade 无 `cancel()` 方法、不保留 token、无 pivot 入口。`ToolContext.cancel`（`src/facade/tool.rs:70`）因此永不被取消。
- `docs/facade-api.md` §13 草拟了 cancel API 形态但未实现。
- 注意与 M4-3/M4-4 的衔接：pivot 注入依赖下层 `inject_pivot`（M4-4 落地软拒绝后，不合法边界注入不再杀 turn）。

实现要求：

- facade 提供 run 级取消句柄：`Agent::run`/`stream` 返回或接受一个 `CancelHandle`（或 `Agent::cancel()` 取消当前活动 run），内部接到 `RunContext` 的 token；`ToolContext.cancel` 接同一 token。
- 提供 pivot 注入入口（如 `Agent::interject(PivotMessage)`），经 drain/机器路径在 step 边界生效；若当前 drain 架构不支持中途喂输入（`drive.rs:369-438` 单输入一路跑到 terminal），先实现 stream 路径的 pivot，非流式标注限制。
- 同步 `docs/facade-api.md` §13。

验证条件：

- 单元测试：run 进行中调 cancel，run 以取消语义结束，Agent 后续可用。
- 单元测试：stream 路径 pivot 注入在下个 step 边界生效（事件序列可见注入消息）。
- `cargo test -p agent-lib --lib facade::` 全过。

### M5-5 [TODO] builder 暴露 `provider_extras`（M-PROM-6）

上下文：

- `ModelConfig::provider_extras(...)` 存在且 `apply_to_request` 会传递（`src/facade/config.rs:400-405`），但 `ChatBuilder`/`AgentBuilder`/`AgentWorkerBuilder`/`AgentRestoreBuilder` 无一暴露它，`build_request` 恒为 `provider_extras: None`（`src/facade/chat.rs:235`）。

实现要求：

- 各 builder 增加 `provider_extras`（或接受整个 `ModelConfig` 的入口）并贯通到 `build_request`。
- 校验 `ProviderExtras` 的 `ProviderId` 与 builder 的 provider 一致（不一致报错或丢弃，按 `model/extras.rs` 既有语义）。

验证条件：

- 单元测试：builder 设置 extras 后，fake client 收到的 `ChatRequest.provider_extras` 与设置一致。
- `cargo test -p agent-lib --lib facade::` 全过；`docs/facade-api.md` §7.1 附近同步。

### M5-6 [TODO] restore 路径补齐 build 同级校验（M-ADP-5）

上下文：

- `src/facade/agent.rs:1280-1291`：`AgentBuilder::build` 对 typed tools + extra + custom registry + delegation 合成工具做 `ensure_unique_declaration_names` 全量校验，并对 rules/dispatcher 引用未注册 delegate 报错。
- `src/facade/agent/snapshot.rs:758-853`：`AgentRestoreBuilder::build` 只调 `ensure_unique_tool_names`（snapshot.rs:772-776），不把 restored `snapshot.delegation` 合成的 `ask_<name>` 声明与重新注入的 typed tool 名对撞检查。

实现要求：

- restore 路径复用 build 路径的同一段校验逻辑（抽公共函数），覆盖 delegation 合成声明对撞与 delegate 引用校验。

验证条件：

- 单元测试：restore 一个带 delegation 的 agent 再 `.tool(..)` 注入同名 `ask_<name>` 工具，`build` 报错而非带病上线。
- `cargo test -p agent-lib --lib facade::agent` 全过。

### M5-7 [TODO] M5 review：facade 承诺收口

检查项：

- 逐条核对 H-STATE-3、M-PROM-2（cancel/pivot）、M-PROM-4、M-PROM-6、M-ERR-5、M-ADP-3、M-ADP-5 状态，`docs/review-2026-07.md` 已标注。
- 重点复验：timeout 后 Agent 可用；流式/非流式事件 parity（含 `Done.events`）；cancel/pivot/provider_extras 实际可达。
- `docs/facade-api.md` 同步；`README.md` 如需更新已更新。
- 全量门禁命令通过。

---

## M6：预算端到端接线

### M6-1 [TODO] drain/drive_turn 接入预算记账（M-PROM-1 核心）

上下文：

- `charge_step`/`charge_usage`/`charge_tokens`/`charge_cost_micros`（`src/agent/context/budget.rs`）在默认路径零调用，只有 external/ 和测试在用。
- LLM `Response` 的 usage 被 fold 后未计费；`CancelRecoveryReason::BudgetExceeded`、`LoopDoneReason::BudgetExhausted`（`src/agent/state/cursor.rs:615,654`）是死变体。
- 设计要求：`docs/agent-layer.md` §1.4 "每步检查，超限中止"；`docs/agent-effect-model.md` §0 "预算统一成 effect"。

实现要求：

- drain 在 StepBoundary（每批 requirement settle 后 / LLM response fold 后）调用 `charge_step` 与 `charge_usage`；超限时按既有 cursor 语义走 `BudgetExhausted`/`BudgetExceeded` 路径（M4-4 已激活这些变体的产生路径后接线）。
- 预算预检与 charge 的非原子窗口（审查 L-8）：保持现状但文档化，或评估在 `BudgetHandle` 内加原子预扣（选型记录）。
- 同步 effect-model 文档的预算节。

验证条件：

- 单元测试：配置小额 token 预算的 run 在超限后以 BudgetExhausted 终止，conversation 状态一致（已 committed 部分完好）。
- 单元测试：预算充足时记账值与各步 usage 之和一致。
- `cargo test -p agent-lib --lib agent::` 全过。

### M6-2 [TODO] facade budget 旋钮 + dispatch 预算硬出口（M-PROM-2 budget 部分、L-9）

上下文：

- facade 恒 `BudgetLimits::unbounded()`（`src/facade/agent/stream.rs:203-207`），无任何 builder 旋钮。
- `src/agent/external/dispatch.rs:585-587, 654-659`：预算完全耗尽时 dispatch 仍降级派 cheapest worker（`saturating_sub` 为 0 → low → downgrade），无"预算尽 → 停止"硬出口。

实现要求：

- builder 增加 `budget(BudgetLimits)` 入口，贯通到每次 run 的 `RunContext`；预算耗尽的终态以结构化错误/事件暴露给 facade 用户（与 M5-3 的 kind 设施对齐）。
- dispatch 增加预算耗尽硬出口：可用预算为 0 时不再派工，返回显式 BudgetExhausted 类结果。

验证条件：

- 单元测试：facade 设置小预算 → run 超限终止且错误可识别；dispatch 在零预算下不派工。
- `cargo test -p agent-lib --lib facade:: agent::external::dispatch` 全过（dispatch 部分带 external features）。
- `docs/facade-api.md`、`docs/agent-layer.md` §1.4 同步。

### M6-3 [TODO] M6 review：预算接线收口

检查项：

- 核对 M-PROM-1、L-8、L-9 状态，`docs/review-2026-07.md` 已标注。
- 确认 `BudgetExhausted`/`BudgetExceeded` 变体不再是死代码（grep 有生产路径构造点）。
- 全量门禁命令通过。

---

## M7：adapter 健壮性与协议契约

### M7-1 [TODO] HTTP 错误分类顺序修正（M-ERR-4）

上下文：

- `src/client/error.rs:90-99`：任意状态码先做 body 子串匹配再判 401/403。403 body 含 "content policy" 误报 `ContentFiltered`；500 body 回声含 "too many tokens" 误报 `ContextLengthExceeded`。

实现要求：

- marker 匹配限定在 4xx 且排除 401/403（401/403 优先判 `Auth`）；5xx 不做内容猜测分类。
- 检查 `client/error/tests.rs` 现有分类测试，补充误分类场景。

验证条件：

- 单元测试：403 + "content policy" body → `Auth`；500 + "too many tokens" body → 非 `ContextLengthExceeded`；413/真实 context 超限 body 仍正确分类。
- `cargo test -p agent-lib --lib client::error` 全过。

### M7-2 [TODO] `StreamEvent::Usage` 语义契约文档化并断言（M-ADP-1）

上下文：

- `src/stream/mod.rs:144-148` 只说 "intermediate or final token-usage update"。实际：Anthropic 发增量段（`src/adapter/anthropic/stream/usage.rs:17-35` `UsageTracker::incremental`），OpenAI 终态一次性发完整累计（`src/adapter/openai_resp/stream/normalizer/terminal.rs:35`）。`Accumulator` 靠 `merge` 加法碰巧都正确（`src/stream/accumulator/mod.rs:146`），但直接消费事件流的上层无法知道该求和还是取最新。

实现要求：

- 统一为"所有 Usage 事件均为可累加增量"语义：OpenAI 侧若历史上会发多条（如中途 usage），改为相对上一条的增量；或在文档明确"每条事件都是对之前所有事件的替换/增量"二选一并让两 adapter 一致。选型记录。
- `StreamEvent::Usage` 文档写明语义与推荐消费方式。

验证条件：

- 单元测试：两个 adapter 的 cassette/fixture 流，按文档语义消费得到与 `collect` 相同的最终 usage。
- `cargo test -p agent-lib --lib adapter:: stream::` 全过。

### M7-3 [TODO] openai_resp `sequence_number` 校验对兼容端点降级（M-ADP-2）

上下文：

- `src/adapter/openai_resp/stream/normalizer/mod.rs:333-345`：要求 sequence_number 从 0 严格连续；wire 结构体 `sequence_number: u64` 必填（`openai_resp/stream/wire.rs:98,109,124,143,162,177,190,197`）。第三方 OpenAI 兼容端点常省略或不保证连续，整个流以 protocol error 终止。

实现要求：

- wire 字段改 `Option<u64>`（`#[serde(default)]`）；缺失时跳过连续性校验，存在时保持严格校验。乱序仍报错。
- 文档注明对兼容端点的降级行为。

验证条件：

- 单元测试：无 sequence_number 的事件流正常归一化；有号但跳号仍报错。
- `cargo test -p agent-lib --lib adapter::openai_resp` 全过。

### M7-4 [TODO] 线缆容错批量修复（adapter L2/L3、external L-1、facade L1）

上下文：

- 空 `arguments`：`src/adapter/openai_resp/stream/normalizer/item.rs:350` `serde_json::from_str(&complete)` 对空串报错；非流式 `convert.rs:172` 同。
- Anthropic 必填字段脆：`src/adapter/anthropic/stream/wire.rs:40` `MessageDelta.usage` 无 `#[serde(default)]`；`normalizer.rs:126-128` 要求 `message_stop` 前必有 `stop_reason`。
- CLI decoder：`src/agent/external/claude_code/decoder.rs:206-207` 非 JSON 行直接 Protocol 错误杀 session（codex/opencode 同构）。
- `src/model/usage.rs:158-162`：非对象 details 字段（如 `"prompt_tokens_details": null`）使整个 usage 解析失败。

实现要求：

- 空 `arguments` 按 `{}` 处理。
- Anthropic wire 可选字段补 `#[serde(default)]`；缺 `stop_reason` 的 message_stop 给 `Normalized` 的 Unknown/缺省值而非报错（保持 raw 证据）。
- CLI decoder 容忍连续 N 个（建议 ≤8）非 JSON 行并计数，超过才 Protocol 错误；容忍的行记录到诊断/日志（不含行内容或截断）。
- usage details 非对象时跳过而非报错。

验证条件：

- 每处一条单元测试（空 arguments 流/响应正常、缺 usage 的 message_delta 正常、N 行 noise 后 session 存活、null details 解析成功）。
- `cargo test -p agent-lib --lib` 与 external feature 测试全过。

### M7-5 [TODO] `ContentBlock` 增加反序列化兜底 variant（facade 报告 M8，单向兼容）

上下文：

- `src/model/content.rs:16-77` + `content/serialization.rs:10-49`：`#[serde(tag = "type")]` 封闭枚举，provider 新增 block 类型（如 `redacted_thinking`、新 server tool block）导致整个 Response 反序列化失败。与 lib.rs:246-251 的前向兼容承诺冲突；flatten `extra` 只保已知 variant 内的未知字段。

决策（已定）：增加 `Unknown` 兜底 variant，但**只做单向兼容**：新版代码必须能反序列化包含未知 block 类型的旧数据/新 provider 响应；反向不要求——`Unknown` 的序列化不要求 round-trip 保真（收到什么发回什么不做保证）。

实现要求：

- `ContentBlock` 增加 `Unknown` variant：反序列化时捕获未识别的 `type` 标签，保留 `raw: Value`（整个 block JSON）与（如可取）原始 type 字符串。serde 实现：内部 tag 枚举的兜底可用 `#[serde(untagged)]` 外层或手写 `Deserialize`（参照 `content/serialization.rs` 现有结构选型，写入完成记录）。
- `Serialize`：允许直接把 `raw` 原样写出（实现成本低、多数情况保真），但**文档明确不保证** round-trip；不为保真做额外机制。
- 排查所有 `match ContentBlock` 穷尽点（`grep -rn "ContentBlock::" src/`），`Unknown` 的处理原则：conversation validator 对 assistant 消息中的 Unknown 放行（作为 provider 输出证据保留）；request 构造侧（`adapter/*/request/`）序列化直接透传 `raw`；不新增报错路径。
- 更新 lib.rs:246-251 前向兼容承诺措辞：未知 block 类型由 `Unknown` 保留，序列化保真为 best-effort。

验证条件：

- 单元测试：含伪造未知 block 类型（如 `{"type": "future_block", "data": ...}`）的 fixture 响应，两个 adapter 的非流式与流式路径均解析成功，block 进入 `ContentBlock::Unknown` 且 `raw` 可读。
- 单元测试：`Unknown` 序列化输出 JSON 可再次被解析为 `Unknown`。
- `cargo test -p agent-lib --lib model:: adapter:: conversation::` 全过（validator 相关既有测试不受影响）。

### M7-6 [TODO] M7 review：adapter 收口

检查项：

- 逐条核对 M-ERR-4、M-ADP-1、M-ADP-2、M7-4/M7-5 覆盖项状态，`docs/review-2026-07.md` 已标注。
- 全量门禁命令通过。

---

## M8：复制代码收敛

### M8-1 [TODO] 两个 LLM adapter 收敛公共传输/解码模块（adapter 报告 M4）

上下文：

- 逐字重复清单：整个 SSE decoder（`src/adapter/anthropic/stream/decoder.rs` 与 `src/adapter/openai_resp/stream/decoder.rs`，87/88 行仅注释不同，已对 `S/B/E/F` 泛型）；`validate_event_stream_content_type` + `map_transport_error`（两个 `stream/mod.rs:61-95` 附近）；`chat()`/`chat_stream()` HTTP 传输样板（两个 `response.rs:48-78`、两个 `stream/mod.rs:23-58`）；`endpoint_headers`/`append_header`/URL 拼接（`anthropic/request.rs:229-296` vs `openai_resp/request.rs:98-165`）；`insert_preserving_collision`（`openai_resp/stream/normalizer/terminal.rs:217-223` vs `openai_resp/response.rs:154-160`，同 crate 内已重复）。

实现要求：

- 新增 `src/adapter/common/`（或并入 `src/client/`）承载：泛型 SSE decoder、HTTP 传输样板（execute→status→retry_after→body 读取→错误映射，含 M1-2 的超时/上限设施单一实现）、`map_transport_error`（含 M1-4 脱敏单一实现）、header/URL 工具。
- 两 adapter 改为调用公共模块；行为零变化（M1–M7 修复后的行为为准）。
- 纯移动重构，不改公共 API。

验证条件：

- 全部现有 adapter 测试原样通过（不重写断言）：`cargo test -p agent-lib --lib adapter::` 与 `cargo test --all --all-targets`。
- `cargo clippy --all-targets -- -D warnings`；无重复实现残留（人工 diff 两 adapter 目录）。

### M8-2 [TODO] 三个 CLI adapter 收敛共享 child-process 模块（external 报告 L-12）

上下文：

- 每个 adapter 复制约 200 行同构代码：`ProcessTurn`/`ProcessIo`（spawn/close/kill/wait）、`drain_and_emit`、`maybe_session_ref`、`intersect_capabilities`（`claude_code/adapter.rs:677-693` / `codex/adapter.rs:806-822` / opencode 同）、`reject_unsupported_tools`、`turn_message`。M1-5/M1-6/M2-1/M2-5 的修复已在三份中各做一遍。

实现要求：

- 新增 `src/agent/external/process/`：共享的 line-oriented 子进程管理（spawn + 进程组、read line + idle 超时、grace close + 退出码分类 + kill 兜底、prelude 循环 + deadline + 取消）、`intersect_capabilities` 等纯函数。
- 三 adapter 改为薄封装（只保留各自 argv 构造与 decoder 接线）；ACP 能复用的部分一并复用。
- 行为零变化（以 M1/M2 修复后为准）。

验证条件：

- 全部 external feature 测试原样通过。
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`。
- 三 adapter 目录行数显著下降（完成记录给出 before/after）。

### M8-3 [TODO] M8 review：收敛收口

检查项：

- 确认两份收敛无行为回归（全量测试 + 人工抽查关键路径 diff）。
- `docs/managed-external-agent.md`、AGENTS.md 的模块描述同步（新增公共模块的位置）。
- 全量门禁命令通过。

---

## M9：低严重度清扫与文档收尾

### M9-1 [TODO] panic/poison 策略统一

上下文：

- 同 crate 两种中毒策略并存：collab 做恢复（`src/agent/collab/mailbox.rs:110-114`、`plan.rs:343-347`、`blackboard.rs:110-113`），trace/budget/facade 十余处 `.expect("… poisoned")`（如 `src/agent/context/trace.rs:213,234,379`、`budget.rs:235,247,322`、`src/facade/approval.rs:646,662,679,751`、`src/facade/agent/stream.rs:78,84`、`src/agent/drive/reference.rs:143,193`）。
- 其他生产 expect：`src/agent/drive.rs:603`（manifest 不变量）、`src/conversation/persistence/rows.rs:1` 等少量点。

实现要求：

- 统一为中毒恢复（`unwrap_or_else(|e| e.into_inner())`）或文档化的显式 panic 策略——库代码推荐恢复；写入贡献约定（AGENTS.md Conventions）。
- `drive.rs:603` 类不变量 expect 保留但改为带上下文的 panic 消息或 debug_assert + 防御分支。

验证条件：

- `grep -rn 'expect("' src/ | grep -i poison` 清零（或仅剩文档化例外）。
- 全量测试通过。

### M9-2 [TODO] API 打磨批

上下文（逐项小改，逐一勾选）：

- `src/facade/run.rs:494-498,513-521`：`ApprovalRequest::call_id` 空串哨兵 → `Option<String>`（字段已 `#[non_exhaustive]`，成本低）。
- `src/model/normalized.rs:8-14`：`Normalized` 字段全 pub 可构造 value/raw 矛盾值 → 构造器私有化 + 只读访问器（评估 breaking 面）。
- `src/prelude.rs:23-27`：补 `FacadeError` 与高频 model 类型导出（或 `model/mod.rs` 根级 re-export）。
- 配置校验缺口：`ChatBuilder::model("")`/`AgentBuilder::model("")` 空白通过（`src/facade/chat.rs:327-356`、`agent.rs:1206-1218`）；`ModelConfig::temperature` 接受 NaN/无穷（`facade/config.rs:348-352`）；空 delegate 工具名/空 keywords（`facade/delegate.rs:799-813`）。
- `src/facade/approval.rs:482,644-647`：`FacadeApproval.pending` 跨 run 泄漏，run 收尾清理。
- `src/facade/chat.rs:639-643`：`ChatSessionBuilder` 无法把继承 system prompt 清为 None。
- `src/facade/agent.rs:1800,1977-1981`：`Some(Usage::default())` 与 `None` 语义含混，统一。
- `src/facade/ids.rs:26-34`：`FacadeIds` "globally unique" 文档措辞过强，修正。
- `src/agent/state/queue.rs:232`：`pub type QueuedReconfig = ReconfigRequest` 兼容别名删除（pre-1.0）。
- 命名/动词一致性（`ask`/`send`/`run`、`Agent` vs `AgentSession`、lib.rs 未提 facade、`facade/mod.rs` milestone 叙事、不存在的 `AgentSession`）：统一或文档说明现状，0.1.x 窗口内决策。
- `src/model/tool.rs:19-27`：`ToolCall` 补 `extra` 逃生舱，与 `ContentBlock::ToolUse` 对齐（评估）。
- `RunEvent` 从不产生的 variant 与 "shape stable" 承诺矛盾（`src/facade/run.rs:282-285,298,417`）：加 `#[non_exhaustive]` 或收回承诺。

实现要求：

- 逐项实现或显式记录"不做"及理由；公共 API breaking 项在完成记录汇总。

验证条件：

- 每项至少一条测试或编译期保证；`cargo test --all --all-targets` 通过；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。

### M9-3 [TODO] 性能小项批

上下文：

- `src/agent/context/trace.rs:380`：`record_node` 每次 O(n) 全表扫重 → O(n²)。
- `src/agent/collab/plan.rs:399`：`add_task` 为环检测整表 clone。
- `src/facade/agent.rs:314-319`：每 run 深拷贝 `self.tools` 与 `extra_declarations`（整棵 JSON schema 树）→ `Arc` 化。
- `src/conversation/history.rs:211-217`：`contains_message_id` 每次 O(全量历史)，被各 pending 操作调用（M-CONV-4）——增量 id 索引。
- `src/conversation/persistence/rows.rs:531-532,1130`：`insert_set_against` 双份深拷贝校验。
- `src/adapter/openai_resp/stream/normalizer/`：`raw.clone()`、`done_item` 保留、`unmodeled_events` 堆积（内存 2–3 倍放大，M-ADP 报告 M7）。

实现要求：

- 逐项优化或记录"暂不优化"理由；优化项附简单 bench 或计数断言（不要求正式 benchmark 设施）。
- conversation id 索引改动较大，若插入此处影响 M9 进度可单列子任务。

验证条件：

- 全量测试通过；优化点无行为变化（现有断言原样通过）。

### M9-4 [TODO] 文档同步与审查报告勾销

实现要求：

- `src/lib.rs` crate 文档补 facade 层描述（README.md:41-45 已把 facade 定位为推荐入口，docs.rs 首页应一致）。
- `docs/review-2026-07.md` 全部条目标注最终状态（已修复/已降级/不做+理由）；全文移入 `docs/archive/2026-07-review/`（或保留在 docs/ 并标注已收口，二选一记录）。
- `AGENTS.md`、`README.md`、`docs/facade-api.md`、`docs/managed-external-agent.md`、`docs/capability-matrix.md`、`docs/conversation-core.md`、`docs/agent-effect-model.md`、`docs/agent-layer.md` 全面过一遍与现状的一致性。

验证条件：

- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- 抽查 10 条文档声明与代码行为一致（人工）。

### M9-5 [TODO] 终审 review：全计划收口

检查项：

- `docs/review-2026-07.md` 无未标注条目。
- PLAN.md 五个目标逐条核对达成情况，写入收尾结论。
- 全量门禁命令通过（含全部 external features）。
- `cargo test --all --all-targets` 无挂起、无 ignore 泄漏（默认离线）。
- 收尾：PLAN.md/TODO.md 归档到 `docs/archive/2026-07-19-review-fixes/`（本计划完成后）。
