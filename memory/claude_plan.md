# M3-1 执行计划：为 mailbox、blackboard、plan 补齐 data-only snapshot API

## 任务
TODO.md M3-1：在 collab 层为 mailbox / blackboard / plan 补齐 data-only snapshot + restore
API，保证 seq/offset 单调性与旧格式兼容，并加 round-trip 测试。

## 现状调研
- `src/agent/collab/mailbox.rs`：有 `Mailbox`（Mutex<MailboxState{next_seq, inboxes}>）、
  `MailMessage`。仅有 `inbox`/`read_from`，无整体 snapshot / restore。
- `src/agent/collab/blackboard.rs`：有 `Blackboard{id, channels}`、`BoardMessage`，
  有 per-channel `snapshot(channel)` 与 `channels_list()`，无整体 snapshot / restore。
- `src/agent/collab/plan.rs`：`PlanSnapshot{id, version, task_order, tasks}` 已完整，
  有 `snapshot()`，但无 `from_snapshot()` restore API。
- 注意 `src/facade/agent/snapshot.rs` 里有占位 `MailboxSnapshot{}`/`BlackboardSnapshot{}`
  （空）—— 那是 M3-2/M3-3 的 facade 接线范围，本任务不动。

## 实现要求
1. mailbox：新增 data-only `MailboxSnapshot{next_seq, inboxes}`（serde，`#[serde(default)]`）；
   `Mailbox::snapshot()` + `Mailbox::from_snapshot()`；restore 保 next_seq 单调（并防御性
   reconcile 到 max(seq)+1）。
2. blackboard：新增 data-only `BlackboardSnapshot{id, channels}`（serde）；
   `Blackboard::snapshot_all()` + `Blackboard::from_snapshot()`；保 id/channel/offset 顺序。
3. plan：新增 `Plan::from_snapshot(PlanSnapshot)`；确认 PlanSnapshot 覆盖全状态（已覆盖）。
4. 从 collab mod / agent mod 导出新 snapshot 类型（与 PlanSnapshot 对齐）。

## 验证条件
- 新增 mailbox round-trip 测试（多 recipient、snapshot→restore、read_from 一致、seq 续增）。
- 新增 blackboard round-trip 测试（多 channel、snapshot→restore、channel 列表与内容一致、offset 续）。
- 新增 plan round-trip 测试（snapshot→restore、version/task_order/tasks 一致、可续操作）。
- cargo fmt / clippy --all-targets -D warnings / `cargo test -p agent-lib --lib agent::collab`。

## 状态：完成
M3-1 完成：mailbox/blackboard/plan 补齐 data-only snapshot+restore API，新增 4 个 round-trip 测试，fmt/clippy/test/doc 全绿，TODO.md 标记 [DONE]。
