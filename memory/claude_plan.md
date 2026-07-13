# 执行计划 — M4-1 cancel = never-resume,接 Conversation::cancel_pending

## 选中的任务
`TODO.md` 第一个未完成任务 = **M4-1**(Milestone 4，M1..M3 全 `[DONE]`）。
前置 M3-R 已完成。不拆分。

## 目标（TODO M4-1 "做什么"）
1. 实现 `step(StepInput::Abandon(id))`：不回灌结果，定位 requirement 对应 cursor，
   迁 `CancelRecovery`，触发本机 Conversation 的 `cancel_pending`（按 disposition 闭合裂缝），
   收尾后 cursor 回到可 `step(External(UserMessage))` 的一致态（Idle）。
2. 机器层触发 `Conversation::cancel_pending`（machine 拥有唯一 Conversation）。
3. `CancellationToken` 保留为向下信号；driver 据此决定 Abandon；闭合由 never-resume+cancel_pending 完成。
4. 在参考 driver（drain）里接入 cancel 路径。

## 设计决策（记录，供 review）
- cancel = 整 turn never-resume（受控丢弃+闭合），非逐 tool。
- disposition 由 cursor 决定：
  - `StreamingStep`（NeedLlm 未决，无 open tool call）→ `DiscardTurn`，reason `LlmInterrupted`。
  - `AwaitingTool`/`AwaitingApproval`（有 open tool call）→ `ResumeTurn { cancelled_results }`，
    对每个仍未闭合的 call 合成 `Cancelled` tool result（闭合悬空 tool_use），reason `ToolInterrupted`。
    已完成的 call 保留真实结果 → 支撑"一批 tool 部分 abandon"。
- 收尾：current → `CancelRecovery(step_id, reason)` → `Idle`；清空 in_flight scratch。
- `ResumeTurn` 后 pending 为 coherent Resumed（无悬空 tool_use）；`begin_user_turn` 在
  cursor==Idle 且存在 leftover pending 时先 `DiscardTurn` 再 begin_turn → 满足"step(UserMessage) 开新 turn"。
- driver：`drain` 每批兑现前检查 `ctx.is_cancelled()`；命中则对第一条未决 requirement 喂
  `Abandon`（一次 abandon 即整 turn 闭合），break 返回 cursor=Idle 的 TurnDone。uncancelled 时行为不变。

## 涉及文件
- src/agent/machine/default/mod.rs：Abandon 分发、begin_user_turn leftover 处理、abandon_llm_step、finish_cancel。
- src/agent/machine/default/tools.rs：abandon_tool_phase（枚举 open ToolSlot → CancelledToolResult → ResumeTurn）。
- src/agent/drive.rs：drain 加 cancel 检查分支。
- 测试：machine tests（streaming abandon / tool partial abandon / 后续 UserMessage 开新 turn），
  reference driver cancel 测试。

## 验证命令（顺序）
1. cargo fmt --all
2. cargo clippy --all-targets -- -D warnings
3. cargo test --lib agent::machine agent::drive（聚焦）
4. cargo test --all --all-targets（≤30min）
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
6. git diff --check

## 进度
- [x] machine abandon 实现（mod.rs + tools.rs）
- [x] begin_user_turn leftover 处理
- [x] drain cancel 路径
- [x] 测试（machine 6 + reference 2 + streaming 1 = 9 个，全绿）
- [x] 全套验证（fmt/clippy/test 435 lib/doc/diff 全绿）
- [x] TODO.md 标 [DONE] + 完成记录，提交

## 结论
M4-1 完成。cancel = never-resume：`step(Abandon)` 按 cursor 选 disposition，machine 自持
Conversation 触发 `cancel_pending`（DiscardTurn / ResumeTurn+合成 CancelledToolResult），经
CancelRecovery 收于 Idle；driver 在 `is_cancelled` 时喂一次 Abandon 闭合整 turn。pivot(M4-2)/
respond_approval(M4-3) 未触碰。PLAN.md 无阶段级变更，跳过。
