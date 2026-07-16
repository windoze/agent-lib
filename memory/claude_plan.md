# M2-3 — 显式化 `rebuild_scratch_from_state()` 并加 restore 往返测试

**当前执行 TODO.md 第一个未完成任务 = M2-3**(M1-1~M1-5、M2-1、M2-2 已 DONE)。

## 目标(effect-refine §3.4)
把「跨进程/内存 handoff 恢复时从持久 Conversation pending + reconfig 队列重建 mid-turn
scratch」这段隐式逻辑显式化为一个 `rebuild_scratch_from_state()`,让「cursor + scratch 一致」
成为可测试不变量。落点 2 不改序列化(cursor.rs 不动)。

## 关键源码认知(已核对)
- scratch = 单个 `TurnScratch { None | InTurn(InFlight) | Reconfig(PendingReconfig) }`(mod.rs)。
- `InFlight { assistant_message_id: MessageId, steps_started: u32, tools: Option<ToolPhase> }`
  (tools.rs);字段私有于 tools.rs,mod.rs 只能经构造器建。
- `AwaitingReconfig` cursor 带 `step_id: Option<StepId>`:Some ⇒ during-turn Commit,None ⇒ BeginTurn。
- reconfig 队列在 park 时未清(apply 时才清),故 `queued_reconfig_application()` 可从持久队列重放
  application;`reconfig_boundary_records(application.requests())` 可再渲染 records。
- pending turn 的 frozen assistant 消息 role=Assistant;tool result role=Tool。
- AgentState 序列化在 pending 时被拒(TESTABILITY §984),故真实 restore 只发生在 committed
  boundary;本函数主要服务内存 handoff + 把 begin_user_turn 隐式补丁的一致性定义显式化。

## 可恢复性边界(诚实记录,非 workaround)
- **AwaitingReconfig(Commit)**:完全可重建且可驱动(唯一完整往返场景)。
- **AwaitingTool / AwaitingApproval / 续延 StreamingStep**:可重建 InFlight 的 message-id/steps
  层面(anchor=最后一条 frozen assistant),`tools: None`。ToolPhase running/awaiting 明细在落点 2
  下不可重建(设计 §3.5、tools.rs:39-42 已 deferred),故这些相位重建后只作相位标记、不可继续 fold。
- **首步 StreamingStep(仅 user frozen)/ BeginTurn reconfig**:其 outstanding assistant id / 排队
  user 输入是 host 提供、未持久化,不可重建 → 重建为 None,rustdoc 说明由 driver 重新建立。

## 编辑清单
1. tools.rs:`InFlight` 加
   - `restored(assistant_message_id, steps_started) -> Self`(tools: None)。
   - `rebuild_from_pending(&PendingTurn, awaiting_unfrozen_assistant: bool) -> Option<Self>`:
     统计 frozen assistant ids;last 作 anchor(无则 None);steps_started = frozen + awaiting。
   - rustdoc 说明 tools:None 与 anchor 限制。
2. mod.rs:新增 `fn rebuild_scratch_from_state(&mut self) -> Result<(), StepError>`:
   - StreamingStep/AwaitingTool/AwaitingApproval → InFlight::rebuild_from_pending(...) → InTurn/None。
   - AwaitingReconfig → 从 queued_reconfig_application() + cursor.step_id() 重建
     PendingReconfig::Commit(step_id Some);step_id None(BeginTurn)→ None(文档说明)。
   - Idle/CancelRecovery/Done/Error → None。
   - 带完整 rustdoc(落点 2 限制)。
3. mod.rs `begin_user_turn`:在 discard-pending 补丁块加注释,显式引用 rebuild 的一致性定义
   (None scratch ⇒ 无 in-flight assistant ⇒ 任何遗留 pending 皆 stale 应 discard);入口加
   `debug_assert!(matches_cursor)`。语义不变。
4. tests:新增 `tests/restore.rs`(+ `mod restore;`),自带精简 scripted id fixtures:
   - `rebuild_at_awaiting_reconfig_round_trips`:drive→AwaitingReconfig(Commit)→into_state→新机
     rebuild→断言 Reconfig + matches_cursor;喂 Reconfig(Ok(()))→Done + reconfigs records。
   - `rebuild_aligns_scratch_to_cursor_phase`:遍历 Idle/Done/AwaitingTool/AwaitingApproval/
     AwaitingReconfig(/续延 StreamingStep),rebuild 后断言 matches_cursor。

## 验证序列
1. cargo fmt --all
2. cargo clippy --all-targets -- -D warnings
3. cargo test -p agent-lib agent::machine::default(聚焦)
4. cargo test --all --all-targets(<=30min)
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6. git diff --check;确认 cursor.rs 未改

## 完成后
TODO.md M2-3 标 [DONE] + 完成记录;commit `[M2-3] ...`;停。

## 进度
- [x] 编辑 tools.rs / mod.rs（新增 rebuild_scratch_from_state + rebuild_in_flight_scratch + rebuild_reconfig_scratch + InFlight::rebuild_from_pending/restored；begin_user_turn 首行调用重建）
- [x] 新增 restore.rs 测试（6 个 + fixtures，已注册 mod restore;）
- [x] fmt/clippy/test/doc 全过（focused 45 passed；全量 0 failed；clippy 零告警；doc 干净）
- [x] cursor.rs 零改动确认（git diff 空）；改动仅落 machine/default/
- [x] TODO.md 标 DONE + 完成记录；准备 commit；停
