# 执行计划：M1-1 `EndpointConfig`/`AuthScheme` 手写脱敏 Debug（H-SEC-1）

## 任务来源
- `TODO.md` 第一个未完成任务：**M1-1**（安全与崩溃级修复，H-SEC-1）。
- 审查条目：`docs/review-2026-07.md` 的 H-SEC-1，修复后需在该条目标注 `✅ 已修复（M1-1）`。

## 现状
- `src/client/config.rs`：`AuthScheme`（第 9 行）与 `EndpointConfig`（第 36 行）均 `#[derive(Debug)]`，`Bearer(String)`/`Header{value}` 含明文密钥。
- `src/adapter/anthropic/mod.rs:22`、`src/adapter/openai_resp/mod.rs:25`：derive Debug 内嵌 `EndpointConfig`。
- 先例：`src/facade/config.rs` `ProviderConfig` 手写 Debug（`RedactedAuth`/`RedactedPairs`，用 `<redacted>`）；facade `approval.rs` 有 `is_sensitive_key`（子串匹配敏感 key）。
- TODO 明确要求占位符为 `[REDACTED]`（与 facade 的 `<redacted>` 不同，按 TODO 规格实现，facade 不动）。

## 步骤
1. 提交当前未提交的任务单重生成文件（PLAN.md/TODO.md rename + 新 PLAN.md/TODO.md/docs/review-2026-07.md），保持历史清晰。
2. `src/client/config.rs`：
   - `AuthScheme` 去掉 derive Debug，手写：scheme 名可见，密钥一律 `[REDACTED]`。
   - `EndpointConfig` 去掉 derive Debug，手写：`base_url`/`query_params` 可见；`auth` 用脱敏 Debug；`extra_headers` 头名可见，认证类头名（子串匹配 key/token/secret/auth/password/credential，大小写不敏感）值显示 `[REDACTED]`，其余值可见。
   - serde 行为不变（保留 derive Serialize/Deserialize + 既有 round-trip 测试）。
3. 新增单元测试（config.rs tests）：
   - 含 `"sk-ant-secret"` 的 `EndpointConfig`：`format!("{:?}")` 不含该子串、含 `[REDACTED]`。
   - Bearer/Header/None 各 variant 形态；认证类 extra header 脱敏、普通 header 值可见。
4. 两个 adapter 各加一条断言（各自 mod.rs 新增 `#[cfg(test)]` 小测试模块）：`format!("{adapter:?}")` 不含密钥、含 `[REDACTED]`。
5. 验证：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → `cargo test -p agent-lib --lib client::` → 全量 `cargo test --all --all-targets`（代码有改动，需要跑）。
6. 文档：
   - `docs/review-2026-07.md` H-SEC-1 标注 `✅ 已修复（M1-1）`。
   - rustdoc 中补充 Debug 脱敏说明（config.rs 既有警告段落附近）。
   - 检查 README/AGENTS 等是否需要同步（预计不需要行为级文档更新）。
7. `TODO.md`：M1-1 标题加 `[DONE]`，追加完成记录。
8. 提交：`[M1-1] Redact secrets in EndpointConfig/AuthScheme Debug output`，然后停止。

## 变更记录
- 2026-07-19：完成任务单重生成文件的初始提交（da851c0）。
- 2026-07-19：M1-1 完成。手写 `AuthScheme`/`EndpointConfig` 脱敏 Debug（占位符 `[REDACTED]`，认证类 extra header 值脱敏），两个 adapter 继承 derive；新增 5 条测试；`docs/review-2026-07.md` H-SEC-1 标注 ✅；TODO.md 标记 [DONE]。fmt/clippy/全量测试/doc 全绿。准备提交并停止。
