# M5-1 — 新增 `Notification::ExternalAgent` 变体

**状态:完成(已全绿,已提交)。**

## 目标(TODO.md M5-1)
给 `Notification`(`src/agent/event.rs`)新增 `ExternalAgent(ExternalAgentEvent)` 变体:
- `ExternalAgentEvent` 已在 M2-1 定义于 `src/agent/external/mod.rs`,并经 `crate::agent` 再导出。
- 保持既有变体 serde tag 不变(`#[serde(tag="type", content="data", rename_all="snake_case")]`),
  新变体 tag = `external_agent`,data = `ExternalAgentEvent` 的既有序列化。
- 更新所有穷尽 `match`(编译器指认)。已知一处:
  `crates/agent-testkit/src/assertions/notifications.rs` 的 `describe()`。
- 新增单测:`Notification::ExternalAgent` serde round-trip;过滤名 `cargo test --lib notification_external`。

## 步骤
1. [x] 阅读 `Notification` 定义、`ExternalAgentEvent` 定义、穷尽 match 点、测试 helper。
2. [x] event.rs:导入 `ExternalAgentEvent`,加变体 + rustdoc。
3. [x] testkit describe():补 `ExternalAgent` 臂。
4. [x] event.rs 测试:加 `notification_external_agent_round_trips`(名字含 `notification_external`)。
5. [x] 验证序列:fmt --check → 聚焦测试 → clippy -D warnings → 全量 test → doc -D warnings → git diff --check。
6. [x] TODO.md M5-1 标 [DONE] + 完成记录;更新本文件。
7. [x] 提交 `[M5-1] ...`,停止。

## 约束
- 不改变既有 4 个变体的 wire tag;新增为增量路径。
- 新公开 API 带 rustdoc。
