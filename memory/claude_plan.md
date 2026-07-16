# M2-1 — 引入非序列化 `TurnScratch` enum 收拢 mid-turn scratch

**当前执行 TODO.md 第一个未完成任务 = M2-1**（M1-1~M1-5 已 DONE）。刀 (B) 落点 2：
`LoopCursor` 保持纯地址、全序列化不变；把游离的两个非序列化 `Option`
（`in_flight` + `pending_reconfig`）收敛成单个非序列化 `scratch: TurnScratch`，
相位与 cursor 同构。**本任务只做「引入 enum + 访问器 + matches_cursor 辅助」，
不动 resume 双重防御（那是 M2-2）**。

## 关键设计确认（已核对源码）
- 旧模型里 during-turn `Commit` reconfig 在 `AwaitingReconfig` 时 `in_flight` 与
  `pending_reconfig` **同时** Some；但进入 Reconfig 相位后 `in_flight` 不再被读取
  （resume_reconfig 的 Commit 走 finalize_text_commit → 置 None；BeginTurn 走
  open_user_turn → 重设 InTurn）。故单枚举合一（Reconfig 丢弃 InFlight）**语义安全**。
- `fail_with_notifications` 旧代码只清 `in_flight`、保留 `pending_reconfig`；新代码
  `self.scratch = TurnScratch::None` 全清。差异仅在「emit_reconfig_effect 的
  transition_cursor 失败」这一极端路径，之后 cursor=Error（quiescent），scratch 永不再读，
  **非对外可观测**，符合「语义零变化」。
- LoopCursor 变体：Idle / StreamingStep / AwaitingTool / AwaitingApproval /
  CancelRecovery / AwaitingReconfig / Done / Error。

## 做什么（编辑清单）
### mod.rs
1. 定义 enum TurnScratch { None, InTurn(InFlight), Reconfig(PendingReconfig) }（非 serde），
   + impl TurnScratch { fn matches_cursor(&self, &LoopCursor) -> bool }（#[allow(dead_code)]，
   M2-2 接入 debug_assert）。放在 impl PendingReconfig 之后、struct 之前。
2. 结构体字段 in_flight + pending_reconfig → 单个 scratch: TurnScratch（更新 doc）。
3. new()：两处 None → scratch: TurnScratch::None。
4. 加访问器（放 validate_reconfig_registry 之后）：
   - fn in_flight(&self) -> Option<&InFlight>
   - fn in_flight_mut(&mut self) -> Option<&mut InFlight>
   - fn take_pending_reconfig(&mut self) -> Option<PendingReconfig>
5. 站点改写：
   - open_user_turn:359 → scratch = InTurn(InFlight::new(..))
   - fold_llm_response:525 → self.in_flight().map(..)
   - finalize_text_commit:602-603 → scratch = None
   - emit_reconfig_effect:625 → scratch = Reconfig(pending)
   - resume_reconfig:674 → self.take_pending_reconfig()
   - abandon_reconfig:770 → scratch = None
   - finish_cancel:782-783 → scratch = None
   - fail_with_notifications:814 → scratch = None
### tools.rs
   - begin_tool_phase:177 → self.in_flight_mut()
   - finish_tool_phase:530/536/566 → in_flight_mut() / in_flight()
   - tool_phase / tool_phase_mut:617/624 → in_flight() / in_flight_mut()

## 验证条件（M2-1）
- 结构体只剩 scratch: TurnScratch；in_flight/pending_reconfig 字段已删。
- LoopCursor / state/cursor.rs 完全未改（git 确认）。
- cargo test -p agent-lib agent::machine::default 全绿，断言未改。
- 完整验证序列 1–6：fmt / 聚焦 / clippy / 全量 / doc / diff。

## 完成后
TODO.md M2-1 标 [DONE] + 完成记录；commit [M2-1] ...；停。

## 进度
- [x] 编辑 mod.rs（enum + 访问器 + 站点）
- [x] 编辑 tools.rs（访问器路由）
- [x] fmt / clippy / 聚焦测试(39) / 全量(36 bin 全绿) / doc / diff 全过
- [x] TODO.md 标 DONE + 完成记录（含语义等价核对 + 验证序列）
- [ ] commit；停
