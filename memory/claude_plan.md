# 执行计划

## 范围

- 以 `TODO.md` 为唯一任务顺序与完成状态来源。
- 本轮只完成第一个标题未标 `[DONE]` 的任务。
- 完成实现、验证、`TODO.md` 完成记录与 Git 提交后停止。

## 步骤

1. 阅读 `TODO.md`，确认第一个未完成任务及其验证要求。
2. 只检查最近提交中与当前任务直接相关的未完成事项。
3. 阅读当前任务相关实现与文档，不做开放式历史问题扫描。
4. 以最小正确改动完成任务。
5. 按顺序运行验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、任务定向测试、必要的全量测试与 rustdoc。
6. 若验证暴露未被任务单安排的失败测试，先修复或在 `TODO.md` 中加入最小前置任务。
7. 将当前任务标题改为 `[DONE]`，写入完成记录。
8. 在关键决策、实现完成、验证完成和任务记录完成时更新本文件。
9. 提交本轮全部预期变更后停止。

## 当前状态

- 已选择首个未完成任务：`M5-5 [TODO] builder 暴露 provider_extras（M-PROM-6）`。
- 实现决策：当 builder 设置了 `ProviderConfig` 时，用 `ProviderConfig::provider()` 校验 `ProviderExtras.provider`；纯 `.client(...)` 注入路径无法从 `Capability` 推断 wire provider，因此保留逃生舱语义，原样透传给注入 client。
- worker 语义：`AgentWorkerBuilder` 只在显式 pin model 时应用 worker-local extras；继承模型的 worker 继承 supervisor 的完整模型配置（含 extras），不接受单独 extras。
- 实现已完成：新增四个 builder 的 `.provider_extras(...)`、共享 provider 校验 helper、restore 当前模型 extras 覆盖、Chat/Agent/restore 请求级测试、worker spec/拒绝测试，以及 facade API 文档。
- 验证已完成：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test -p agent-lib --lib facade::`（222 条）、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 全部通过。
- 记录已完成：`TODO.md` 已将 M5-5 标为 `[DONE]` 并写入完成记录；`docs/review-2026-07.md` 已将 M-PROM-6 标为已修复。验证后只修改了 Markdown/任务记录，未再修改编译输出相关代码。
