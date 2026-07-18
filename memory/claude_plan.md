# 当前执行计划：M1-5 external 流读取超时与 launch 超时拆分（H-EXT-1）

## 任务识别
- `TODO.md` 中第一个未完成任务是 **M1-5 [TODO]**（M1-1~M1-4 均已 `[DONE]`）。
- 问题：三个 CLI adapter 把 `config.timeout()`（默认 30s，本为 probe/launch 设计）同时用作每行 stdout 读取超时与 shutdown grace。CLI 跑长静默命令（构建/测试）30s 无输出即被误杀。

## 执行步骤
1. 阅读三个 config：`src/agent/external/claude_code/config.rs`、`codex/config.rs`、`opencode/config.rs`，弄清现有 `timeout()` 语义与 serde 形状。
2. 阅读三个 adapter 的 session 构造点（`claude_code/adapter.rs:168-169`、`codex/adapter.rs:222-223`、`opencode/adapter.rs:236-237` 附近），确认 `read_timeout` / `shutdown_grace` 如何被消费；同时检查 ACP 是否使用同名字段（本任务范围是三个 CLI adapter，ACP 如涉及一并核对）。
3. 三个 config 各新增字段：
   - `read_idle_timeout: Duration`，默认 10 min，`#[serde(default = ...)]`，旧 JSON 可反序列化。
   - `shutdown_grace` 独立字段，默认保持 30s，`#[serde(default = ...)]`。
   - 保留 `timeout()` 为 probe/launch 语义，rustdoc 写清三者口径。
4. 三个 adapter 的 session 构造改用新字段。
5. 文档同步：`docs/managed-external-agent.md` + config rustdoc；说明 codex `exec` one-shot 与 claude/opencode 长会话的静默上限语义差异（如有）。
6. 单元测试：每个 config 的默认值断言 + serde round-trip（含缺新字段的旧 JSON 反序列化）。
7. 验证（按 TODO 要求）：
   - `cargo test --features "external-claude-code external-codex external-opencode" --all-targets`
   - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode" -- -D warnings`
   - `cargo fmt --all`
8. 在 `TODO.md` 标记 M1-5 `[DONE]` 并写完成记录；`docs/review-2026-07.md` H-EXT-1 标注 `✅ 已修复（M1-5）`。
9. 提交 git commit，停止。

## 进度
- [x] 识别任务 M1-5，写入本计划
- [x] 代码阅读（三个 config + 三个 adapter 构造点；确认 ACP 不在本任务范围）
- [x] 实现：三个 config 新增 `read_idle_timeout`（默认 10 min）与 `shutdown_grace`（默认 30s），
  均带 serde default；三个 adapter session 构造改接线；rustdoc 同步
- [x] 测试：三个 config 各补默认值/serde round-trip/旧 JSON 兼容断言；
  `cargo test --features "external-claude-code external-codex external-opencode" --all-targets` 通过；
  `cargo clippy --all-targets --features "external-... external-acp" -- -D warnings` 通过
- [x] 文档：`docs/managed-external-agent.md` §12 新增「三类超时」段落并改述旧措辞；
  `docs/review-2026-07.md` H-EXT-1 标注 ✅；TODO.md 标记 M1-5 [DONE] + 完成记录
- [x] 全量门禁（clippy 默认 feature + 全量测试 + rustdoc）全部通过
- [x] git commit `4fe86f6` 并停止。M1-5 完成。
