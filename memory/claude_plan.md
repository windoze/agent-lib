# M2-1 — 扩展 ExternalAgentCursor 与 machine scratch 支持 pending tool batch

**当前执行 = TODO.md 第一个未完成任务 = M2-1**(M1-1..M1-4 已 `[DONE]`)。
Milestone 2 目标:runtime 暂停在 `PausedForToolCalls` 时,machine 发 host `NeedTool` batch,
收齐后回灌 `RespondToolResults`。M2-1 只做**数据结构脚手架**(cursor 变体 + machine scratch),
不做 fold(M2-2)也不做 result 收集(M2-3)。

## 任务边界(严格 staging)
- M2-1:cursor 新增 `AwaitingTool { batch_id, requirements }`;machine 新增非序列化 scratch
  `pending_tool_batch: Option<PendingExternalToolBatch>`;更新 `requirement()`/`requirements()`/
  `initial_loop_cursor`/`cursor_label`;cursor serde round-trip 覆盖 AwaitingTool;
  `initial_loop_cursor(AwaitingTool)` 非 terminal 且测试记录降级行为。
- M2-2:`fold_session_result` 新增 `PausedForToolCalls` 分支(构造 batch + 发 NeedTool)。
- M2-3:`resume_tool` 收齐 result 回灌 `RespondToolResults`。

## 实现步骤
- [ ] state.rs:import `ExternalToolBatchId`(external)、`ToolWaitRequirements`(agent)。
- [ ] state.rs:cursor 新增 `AwaitingTool { batch_id: ExternalToolBatchId, requirements: ToolWaitRequirements }`
      (含 rustdoc)。cursor 持久化 outstanding requirement ids(经 ToolWaitRequirements)。
- [ ] state.rs:`requirement()` 加 `AwaitingTool => None`;新增 `requirements() -> Option<&ToolWaitRequirements>`;
      新增 `has_outstanding_requirement()`(覆盖 Session/Interaction/Tool)。
- [ ] state.rs 测试:`external_agent_state_cursor_variants_round_trip` 加 AwaitingTool;断言
      `requirement()`=None、`requirements()`=Some、`has_outstanding_requirement()`=true。
- [ ] machine.rs:import `ExternalToolBatchId, ExternalToolCall, ExternalToolResult`, `BTreeMap`。
- [ ] machine.rs:新增私有 scratch `PendingExternalToolBatch { batch_id, calls, call_to_requirement,
      requirement_to_call, results }`;machine 新增字段 `pending_tool_batch`;`new`=None,`abandon`/`fail_with` 清空。
      构造在 M2-2、消费在 M2-3 → 用 `#[allow(dead_code)]` + 注释标注 staging(codebase 目前零 allow;
      这是明确的前置声明脚手架,不是 spec 绕过)。
- [ ] machine.rs:`abandon` 用 `has_outstanding_requirement()` 取代 `requirement().is_some()`(前向正确:
      AwaitingTool 也需 mark_cleanup)。
- [ ] machine.rs:`initial_loop_cursor` + `cursor_label` 加 `AwaitingTool`。initial_loop_cursor(AwaitingTool)
      → LoopCursor::Idle(降级;mid-turn restore 无法重建 streaming view,PLAN.md §风险已跟踪)。
- [ ] machine.rs 测试:`initial_loop_cursor(AwaitingTool)` 非 terminal 且为 Idle,注释记录降级 + 指向 PLAN.md。

## 验证
- cargo fmt --all -- --check
- cargo test -p agent-lib external_agent_state_cursor_variants_round_trip
- cargo test -p agent-lib external_agent_state_serde_round_trips_through_conversation_snapshot
- cargo clippy --all-targets -- -D warnings
- cargo test --all --all-targets(≤30min)
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- git diff --check

## 降级行为记录(TODO.md M2-1 要求)
`initial_loop_cursor(AwaitingTool)` 返回 `LoopCursor::Idle`(非 terminal),与
AwaitingSession/AwaitingInteraction 一致:restore 时无 step scratch 重建 streaming view。
后续任务:PLAN.md「恢复 mid-turn scratch」风险项(把 pending facts 放入 serializable cursor + restore 测试)。

## 完成状态(2026-07-17)
全部步骤完成。state.rs 新增 `AwaitingTool` 变体 + `requirements()`/`has_outstanding_requirement()`;
machine.rs 新增 `PendingExternalToolBatch` scratch(`#[expect(dead_code)]` staging)+ 字段、
`initial_loop_cursor`/`cursor_label`/`abandon` 更新、restore 降级测试。
验证:聚焦 3 passed;clippy 0;fmt/diff clean;doc -D warnings 绿;全量 789 passed/0 failed。
M2-1 已在 TODO.md 标 [DONE] + 完成记录。待 commit。
