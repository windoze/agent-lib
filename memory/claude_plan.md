# M2-4 — Milestone 2 review:刀 (B) 正确性与序列化不变性

**当前执行 TODO.md 第一个未完成任务 = M2-4**(M1-1~M1-5、M2-1~M2-3 已 DONE)。
这是 review 任务(不可拆分)。需真正执行审查 + 完整验证,并把量化数据写入完成记录。

## 验收目标(刀 B)
- 唯一真相:scratch 仅一个 `TurnScratch`(2 个 Option → 1 个 enum)。
- 序列化零风险:`LoopCursor` / cursor.rs 未变。
- 隐式约定被类型消灭;无「相位 + scratch」双重防御残留。

## 做什么(TODO M2-4)
1. 通读 mod.rs / tools.rs:确认无裸 `self.in_flight` / `self.pending_reconfig` 字段访问,全经 TurnScratch 访问器;无双重防御残留。
2. `git diff src/agent/state/cursor.rs` = 零改动;跑 `cargo test -p agent-lib agent::state`
   (含 streaming_step_cursor_round_trips_requirement_binding /
   awaiting_tool_cursor_round_trips_requirement_ids /
   agent_state_serde_round_trips_through_conversation_snapshot)全绿。
3. 完整验证序列 1–6 + `cargo test --all --all-targets`。
4. `git diff --stat` 仅触及 `src/agent/machine/default/`。
5. 完成记录:字段数变化、消灭防御分支数、ToolPhase 明细不重建已知限制。

## 进度
- [x] 确认 M2 diff 范围:仅 machine/default/ 源码 + TODO/memory 文档;cursor.rs 零改动
- [ ] 代码审查:TurnScratch 唯一真相 + 无裸字段访问 + 无双重防御
- [ ] cargo test -p agent-lib agent::state(序列化往返)
- [ ] fmt --check / clippy / test --all / doc / git diff --check
- [ ] 写完成记录量化数据 → TODO.md 标 DONE → commit → 停

## 验证记录(待填)

## 验证记录(已填)— 全过
1. fmt --check ✅  2. agent::state + 3 named 往返 ✅  3. clippy -D warnings ✅
4. test --all --all-targets 0 failed ✅  5. doc -D warnings ✅  6. git diff --check ✅
- cursor.rs 零改动(diff 0 行);M2 diff --stat 仅 machine/default/ + 文档
- 字段:2 Option → 1 enum(scratch: TurnScratch);双重防御塌缩为 4 处 matches_cursor debug_assert
- 已知限制:ToolPhase 明细不重建(tools:None,M3+ deferred);首步 StreamingStep/BeginTurn reconfig 重建为 None
- TODO.md M2-4 标 [DONE] + 完成记录已写;准备 commit;停
