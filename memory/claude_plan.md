# 执行计划

## 当前任务

- 第一个未完成任务：`M5-4 [TODO] facade 暴露 cancel 与 pivot 入口（M-PROM-2 cancel/pivot 部分）`。
- 任务来源：`TODO.md`，行 1316 起。
- 本次只完成 M5-4，完成后更新 `TODO.md`、提交 Git，然后停止。

## 约束摘要

- `TODO.md` 是任务顺序、需求、验证和完成状态的唯一来源。
- 任务标题未带 `[DONE]` 即视为未完成；完成记录不能替代 `[DONE]` 标记。
- 不做开放式历史问题扫描；只处理阻塞 M5-4 或由 M5-4 引入的缺陷。
- 不通过缩小功能、私有特例或规避路径来绕开规格不匹配；若发现必须先修的前置问题，更新 `TODO.md` 并提交后停止。
- 验证按 cheap-to-expensive 顺序执行：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、相关测试、必要时全量测试与 rustdoc。
- 代码改动保持最小正确范围；同步拥有该行为的文档，至少检查 `docs/facade-api.md` 与审查报告标注。

## 执行步骤

1. 检查最近提交与当前工作树，确认是否有与 M5-4 直接相关的未完成事项或并行改动。
2. 阅读 M5-4 相关实现：`src/facade/agent.rs`、`src/facade/agent/stream.rs`、`src/facade/tool.rs`、`src/agent/drive.rs`、默认机器 pivot/cancel 入口、现有 facade 测试。
3. 设计 facade cancel 入口：优先采用显式 run 级句柄或 `Agent::cancel()` 中最小可行方案；确保 `RunContext` 与 `ToolContext.cancel` 使用同一 token，取消后 Agent 可继续使用。
4. 设计 facade pivot 入口：优先实现 stream 路径的中途 pivot 注入；若非流式 `drive` 架构仍不支持中途喂输入，则在 API/文档中明确限制并保证不违背任务要求。
5. 实现 cancel/pivot API、内部状态接线与测试夹具；避免破坏 M5-1 drop guard 与 M4-4 软拒绝语义。
6. 补单元测试：运行中 cancel 后 run 以取消语义结束且后续可用；stream pivot 在下个 step 边界生效并可见注入消息；补 `ToolContext.cancel` 传递验证（如有现有测试入口）。
7. 同步 `docs/facade-api.md` §13、相关 rustdoc、`docs/review-2026-07.md` M-PROM-2 状态；完成后将 `TODO.md` 的 M5-4 标为 `[DONE]` 并写完成记录。
8. 运行验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test -p agent-lib --lib facade::`；若代码触及更广泛 agent/drive 契约，再运行相应定向测试与全量测试。
9. 复查 `git diff`/`git status`，提交本任务全部变更，停止。

## 进度记录

- 已读取 `TODO.md`，确认第一个未完成任务是 M5-4。
- 已写入本轮 M5-4 执行计划；下一步检查最近提交与工作树，再阅读相关 facade/drive/machine 代码。
- 最近提交为 M5-3，未包含与 M5-4 直接相关的未完成事项；当前工作树除本计划文件外无其他改动。
- 下一步阅读 facade run/stream、ToolContext、drive 与默认机器 pivot/cancel 接口，确定最小接线方案。
- 已确认取消 token 由 `RunContext::new_root` 私有创建，`ToolContext.cancel` 已能接收 run token；需要新增可由 facade 调用方持有/传入的 cancel handle，并让 `RunContext` 支持用外部 token 创建 root。
- 已确认 pivot 在机器层通过 `AgentInput::Pivot(PivotMessage)` 软拒绝/注入，合法点是 post-tool 的 `StreamingStep`。facade streaming loop 当前会立即 fulfill 下一条 LLM，所以 stream 路径需要共享控制队列，由 drive 在边界消费 pivot 请求；非流式中途 pivot 仍受单 future drive 架构限制，需文档化限制。
- API 选型：新增 facade `CancelHandle`，`run_with_cancel`/`run_full_with_cancel` 接受该句柄；现有 `run`/`run_full` 内部创建默认句柄。`AgentRunStream` 直接暴露 `cancel()` 与 `interject(...)`，因为 stream 持有 `&mut Agent` 的机器借用，调用方无法同时再通过 `Agent` 方法注入 pivot。
- 已实现初版接线：`RunContext::new_root_with_cancellation`、facade `CancelHandle`、`run_with_cancel`/`run_full_with_cancel`、`stream_with_cancel`、`AgentRunStream::cancel`/`interject`，以及 stream drive 的 pivot boundary window。现有 `cargo test -p agent-lib --lib facade::agent` 52 条通过。
- 下一步补 M5-4 指定测试：运行中 cancel 后可继续使用 Agent；stream 在 tool 后边界 interject 后，下一条 LLM 请求包含 pivot 消息。
- 已补测试并通过 `cargo test -p agent-lib --lib facade::agent`（55 条）：非流式 cancel handle、stream cancel、stream interject pivot 均覆盖。下一步同步 `docs/facade-api.md`、`docs/review-2026-07.md` 与 `TODO.md` 完成记录。
- 文档与任务单已更新：`docs/facade-api.md` 记录 cancel/pivot API，`docs/review-2026-07.md` 对 M-PROM-2 做 cancel/pivot 部分标注，`TODO.md` 已将 M5-4 标为 `[DONE]` 并写入完成记录。
- 最终验证已通过：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test -p agent-lib --lib facade::agent`、`cargo test -p agent-lib --lib facade::`、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- 下一步复查 git diff/status 并提交本任务变更。
