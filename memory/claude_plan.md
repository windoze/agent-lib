# 执行计划 — M4-2 pivot = 多喂 input，删除 pivot queue

## 选中的任务
`TODO.md` 第一个未完成任务 = **M4-2**（M1..M4-1 全 `[DONE]`）。前置 M4-1 已完成。不拆分：
pivot 注入 + 删除 pivot queue + 清理 deprecated `AgentInput` 变体是一个必须一起落地的
内聚变更（删除变体要求机器接管 Pivot 且要求改造 legacy loop，无法分开落地而不破坏编译）。

## 目标（TODO M4-2 “做什么”）
1. 实现 `step(External(AgentInput::Pivot(msg)))`：在合法边界（`StreamingStep` 且 pending 处于
   closed-tool-result 边界）向 pending 追加 `Role::User` 消息，复用
   `Conversation::inject_user_message`（沿用其 role sequence 校验）。
2. 删除 pivot 排队语义：`interject` 的 queue 行为、`QueuedPivotTurn`、`AgentState` 的
   `queued_pivots`/`queue_pivot`/`dequeue_pivot`；`PivotMessage`/`PivotSource` 数据类型保留。
3. 清理 M2-1 遗留的 `#[deprecated]` `AgentInput` 变体（`QueuedPivotTurn` 与 `Resume`），使
   `AgentInput` 收敛为迁移文档 §2.2 目标 `{ UserMessage, Pivot }`。
4. 更新受影响 state/loop/event 测试与文档。

## 设计决策
- 合法边界：仅 `LoopCursor::StreamingStep`。`inject_user_message` 内部要求 phase
  `AwaitingAssistant` 且 `is_after_closed_tool_result_step()`，因此“turn 起始首个 StreamingStep”
  与 open tool calls 都被拒 → 满足“不破坏 open tool calls / 只在合法边界注入”。
- re-emit：注入后重渲染 `build_chat_request`，复用同一 requirement id 重发 `NeedLlm`，cursor 不动。
- 非 user pivot 被拒：`PivotMessage::validate()` + `inject_user_message` 的 `validate_user_payload`。
- pivot meta 下沉为 `QueuedPivot::message_meta()` / `PivotSource::label()`，机器与 loop 共用。
- legacy loop：保 `feed(UserMessage)`；`feed(Pivot)` 报错；删 QueuedPivotTurn/Resume 路径与
  pivot-apply/defer helper；`interject` 从 trait 移除；recovery-resume 随 Resume 删除（M4-1 等价覆盖）。

## 验证命令（顺序）
1. cargo fmt --all
2. cargo clippy --all-targets -- -D warnings
3. 聚焦：cargo test --lib agent::machine agent::state agent::event
4. cargo test --all --all-targets（≤30min）
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
6. git diff --check

## 进度
- [ ] queue.rs 下沉 message_meta/label
- [ ] 机器 inject_pivot + 分发
- [ ] state.rs 删 pivot queue
- [ ] event.rs 删 deprecated 变体 + 错误变体
- [ ] mod.rs 导出
- [ ] legacy loop 改造
- [ ] 测试增删改
- [ ] 文档
- [ ] 全套验证
- [ ] TODO.md 标 [DONE] + 完成记录，提交

---

## M4-2 完成 (final)

全部 8 个 Phase 完成,已提交前状态:

- Phase 1-5 (queue.rs helpers / machine inject_pivot / state.rs 去 queue / event.rs 去 deprecated 变体 / mod.rs exports) — done
- Phase 6 legacy loop rework (default.rs + loop_driver.rs) — done: 删 interject、prepare_user_turn 收敛为 {UserMessage,Pivot}、apply_pivots→tool_result_step_boundary、删 5 个 pivot helper + InitialUserTurn.queued_pivot、修 imports
- Phase 7 测试 — done: state/tests.rs 与 loop_driver/default/tests.rs 删除失效 pivot/interject/resume 测试 + helper;新增 tools.rs 5 个 machine pivot 测试
- Phase 8 docs — done: lib.rs / README.md 同步;迁移文档为叙述无需改

验证全绿:fmt clean / clippy -D warnings clean / cargo test --all --all-targets = 433 lib + 8 integration = 441 passed 0 failed / rustdoc -D warnings clean / git diff --check clean。

TODO.md M4-2 已标记 [DONE] 并写入完成记录。下一步:提交并 STOP(不启动 M4-3)。
