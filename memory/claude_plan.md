# M3-2 执行计划：让 `AgentSnapshot::capture` 保存 live 协作内容

## 任务（TODO.md M3-2）
让 facade 的 `AgentSnapshot::capture` 读取 live `CollabState`，当 mailbox / blackboard /
plan 已启用时写入对应 data-only snapshot；未启用保持 `None`；旧 snapshot 仍可读；restore
不意外启用未配置组件。验证条件要求 restore 后协作内容可读，因此 M3-2 需要把 capture 与
restore 的 round-trip 打通（M3-3 再补冲突策略/旧格式兼容测试/续操作语义 + 文档）。

## 现状调研
- `src/facade/agent/snapshot.rs`：
  - `AgentSnapshot` 字段 `mailbox/blackboard/plan` 目前恒为 `None`，`artifacts` 恒空。
  - 有占位空类型 `MailboxSnapshot{}` / `BlackboardSnapshot{}`（需替换为 collab 真实类型）。
  - `capture(state, delegates, external, external_sessions, delegation)` 未接收 CollabState。
  - `build()` 用 `CollabState::provision`（按 topology 建空底座），忽略 snapshot 内容。
- collab 真实 snapshot 类型（M3-1 已落地）：
  - `Mailbox::snapshot()->MailboxSnapshot` / `from_snapshot`。
  - `Blackboard::snapshot_all()->BlackboardSnapshot` / `from_snapshot`；空建需 `id`。
  - `Plan::snapshot()->PlanSnapshot` / `from_snapshot`；空建需 `id`。
  - 均从 `crate::agent` 导出。
- `src/facade/collab.rs`：`CollabState{config, mailbox, blackboard, plan}`（pub(crate) 字段），
  `provision(config, ids)` 只建空底座。
- `Agent::snapshot()`（agent.rs ~776）调用 `AgentSnapshot::capture(...)`，agent 持有 `self.collab`。
- 门面 re-export：`src/facade/mod.rs` 与 `src/facade/agent.rs` re-export 占位
  `MailboxSnapshot/BlackboardSnapshot`（需改为 collab 真实类型，保持路径不变）。

## 实现步骤
1. snapshot.rs：删除占位 `MailboxSnapshot{}`/`BlackboardSnapshot{}`，改 `pub use
   crate::agent::{BlackboardSnapshot, MailboxSnapshot};`（字段类型随之为真实类型）。
2. `AgentSnapshot` 字段 `mailbox/blackboard/plan` 加 `#[serde(default)]`（旧 snapshot 可读）。
3. `capture` 增参 `collab: &CollabState`，按启用写入 `snapshot()/snapshot_all()`。
4. `Agent::snapshot()` 传 `&self.collab`。
5. collab.rs 新增 `CollabState::restore(config, ids, mailbox, blackboard, plan)`：topology 决定
   是否建，snapshot 有内容则 `from_snapshot`，否则空建；未启用保持 `None`。
6. `build()` 用 `CollabState::restore(...)` 取代 `provision(...)`。
7. 测试（`facade::agent::snapshot::tests`，新文件 snapshot_tests.rs，自带 stub client）：
   - mailbox 写入后 snapshot→restore，内容仍可读。
   - blackboard 多 channel 内容 restore 后保留。
   - 未启用协作组件时 snapshot 字段为 None 且 restore 不建组件。

## 验证
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::agent::snapshot
- cargo test -p agent-lib --lib facade::collab
- cargo test -p agent-lib --lib agent::collab（回归）
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

## 状态：完成
M3-2 完成：capture 读取 live CollabState 写入 mailbox/blackboard/plan data-only snapshot；
restore 用 CollabState::restore 按同拓扑 rehydrate；新增 4 个 facade::agent::snapshot 测试。
fmt/clippy/targeted/full/doc 全绿，TODO.md 标记 [DONE]。冲突策略与旧格式兼容测试留给 M3-3。
