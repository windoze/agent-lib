# 执行计划

## 当前状态

- 已定位本轮首个未完成任务：`M7-1 [TODO] HTTP 错误分类顺序修正（M-ERR-4）`。

## 约束

- 以 `TODO.md` 为任务顺序、依赖、验证要求和完成状态的唯一权威来源。
- 本轮只完成第一个未完成任务；完成、验证、记录并提交后停止。
- 不做开放式历史问题扫描；只处理阻塞当前任务或最新提交明确指向当前任务的未完成问题。
- 如遇无法按规格完成的阻塞项，向 `TODO.md` 插入最小必要前置任务，保持当前任务未完成，提交后停止。
- 仅当阶段级计划、依赖或完成标准变化时更新 `PLAN.md`。

## 步骤

1. 阅读 `src/client/error.rs` 与现有 `client::error` 测试，确认当前状态码与 body marker 分类顺序。
2. 将 HTTP 错误分类改为：401/403 优先归 `Auth`；body marker 只在 4xx 且非 401/403 时启用；5xx 不再按 body marker 猜测 `ContextLengthExceeded`/`ContentFiltered`。
3. 补充单元测试：403 + `content policy` body → `Auth`；500 + `too many tokens` body 不为 `ContextLengthExceeded`；413 或真实 4xx context 超限仍正确分类。
4. 更新 `docs/review-2026-07.md` 中 M-ERR-4 标注，并在 `TODO.md` 将 M7-1 标为 `[DONE]` 且填写完成记录。
5. 按任务验证先运行 `cargo test -p agent-lib --lib client::error`，再按仓库要求运行格式、clippy、全量测试与文档（如代码变更需要）。
6. 检查 diff/status，提交本轮相关改动，然后停止。

## 进度日志

- 已重置本轮计划文件。
- 已读取 `TODO.md`，定位首个未完成任务为 `M7-1`。
- 已检查最近提交；`[M6-3] Review budget wiring` 未声明与 M7-1 直接相关的未完成问题。
- 已修改 `ClientError::from_http_response_at`：401/403 认证优先，body marker 只在非认证 4xx 内启用，5xx 保持 generic API 错误。
- 已更新 `client::error` 单元测试，覆盖认证优先、5xx 不按 marker 误分类，以及 4xx marker 正常分类。
- 已运行 `cargo test -p agent-lib --lib client::error`，12 条测试全部通过。
- 已完成门禁：`cargo fmt --all`、默认 clippy、external feature clippy、`cargo test --all --all-targets`、`cargo doc` 全部通过。
- 已更新 `docs/review-2026-07.md`：M-ERR-4 标注 `✅ 已修复（M7-1）`。
- 已更新 `TODO.md`：M7-1 标题改为 `[DONE]` 并写入完成记录。
- 下一步检查 diff/status 并提交本轮变更。
