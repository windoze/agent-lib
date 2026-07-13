# 执行计划 — M1-2

## 当前任务

第一个未完成任务：**M1-2 `Notification`：从 `AgentEvent` 拆出通知部分**（迁移文档 §3.1）。
阶段 0 不删 `AgentEvent`，只新增并存的 `Notification`。

## 约束

- 只完成 M1-2，完成后提交并停止。
- 不改现有行为；`DefaultAgentLoop` 仍用 `AgentEvent`。
- payload 复用现有 struct（`StreamEvent`/`StepBoundary`/`ToolCallStarted`/`ToolCallFinished`），不重定义。

## 步骤

1. [done] 阅读 TODO.md M1-2、迁移文档 §3.1、现有 `event.rs` 的 `AgentEvent` 与四个 payload。
2. 在 `src/agent/event.rs` 定义 `Notification` enum，四个通知变体，serde 与 `AgentEvent` 同形
   （`tag="type", content="data", snake_case`）。
3. `impl From<Notification> for AgentEvent`（变体一一映射，payload 保留）。
   rustdoc 说明 `AwaitingApproval → Requirement::NeedInteraction`、`Done → StepOutcome.quiescent + cursor`。
4. `agent/mod.rs` 导出 `Notification`。
5. 聚焦测试：`Notification` serde round-trip；`From<Notification> for AgentEvent` 四变体映射；
   `Notification` 不含 approval/done 变体（显式测试，且与 AgentEvent 通知变体 wire 兼容）。
6. 验证：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → 聚焦 event 测试
   → `cargo test --all --all-targets`（≤30 分钟）→ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
   → `git diff --check`。
7. TODO.md 中 M1-2 标题 `[TODO]`→`[DONE]` 并补完成记录；提交并停止。

## 进度

- 已确认第一个未完成任务为 M1-2（M1-1 已 [DONE]）。
- 已实现 `Notification` enum + `From<Notification> for AgentEvent`，`agent/mod.rs` 导出，模块 rustdoc 更新。
- 聚焦测试 2 个全绿；fmt/clippy/全量测试(lib 369)/rustdoc/diff check 全部通过。
- TODO.md 中 M1-2 标题改为 [DONE] 并补完成记录；未改 PLAN.md（无阶段级计划变化）。
- 准备提交并停止；下一轮从 M1-3 开始。
