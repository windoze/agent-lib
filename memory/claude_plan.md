# 当前任务：M4-5 取消语义：延迟有界 + `TurnDone` 契约修正（M-ERR-2）

## 任务理解

来源 TODO.md M4-5（首个未完成任务；M4-4 已 DONE 并提交于 HEAD dd9e707）。现状问题：

1. `src/agent/drive.rs:405`：只在每批之间检查 `ctx.is_cancelled()`；批次 fulfill 完成后
   resume 循环前不复查（drive.rs:414-434）。参考 handler 全部忽略 ctx
   （`src/agent/drive/reference.rs:91,153` 的 `_ctx`）。
2. `drive.rs:405-412`：取消时只对 `pending.first()` 记录 `NeverResumed` 并 abandon，
   批次其余 requirement 无 trace 记录，违反 effect-model §8「每个 requirement 恰好以
   一种方式 settle 并记录」。
3. `drive.rs:236-240`：`TurnDone` 文档声称 cursor 是 terminal `Done|Error`，实际取消
   落点是 `Idle`（`finish_cancel`，`machine/default/mod.rs:991-1004`）；调用方无法用
   `is_terminal`（drive.rs:497-499）区分取消与自然结束。
4. `CancelRecovery` cursor 的 restore 坑：`mod.rs:501-508` 的重置只覆盖 Done|Error。

## 实现要求（TODO 规格）

- drain 在 fulfill_batch 返回后、resume 前增加取消复查；参考实现的 handler 把 ctx 传入
  LLM 调用（至少支持在等待响应期间被取消，可配合 M1-2 的超时设施）。
- 取消时对批次内全部 outstanding requirement 逐一记录 `NeverResumed` 并 abandon。
- 修正 `TurnDone` 契约：增加 `cancelled: bool`（或独立 outcome 变体），文档与
  `is_terminal` 行为一致。
- 评估 `CancelRecovery` cursor 的 restore 坑：要么纳入重置，要么从 serde 形状排除。

## 验证条件

- 单元测试：飞行中的 LLM requirement 期间触发取消，drain 在当前批次 settle 后立即停，
  不再推进下一批；全部 outstanding requirement 有 trace 记录。
- 单元测试：取消路径返回的 outcome 可与自然结束区分。
- `cargo test -p agent-lib --lib agent::drive` 全过；`docs/agent-effect-model.md` §8 同步。

## 执行计划

1. [ ] 探索：drive.rs drain 循环/取消检查/TurnDone/is_terminal；reference.rs handler 的
   ctx 使用；machine finish_cancel / CancelRecovery / restore 重置；effect-model §8 契约；
   取消路径现有测试。
2. [ ] 设计选型（记录）：TurnDone 加 `cancelled: bool` vs 独立 outcome 变体；
   CancelRecovery restore 处理方案。
3. [ ] 实现 drain 取消复查 + 全量 NeverResumed/abandon。
4. [ ] 实现 reference handler ctx 贯通（LLM 等待期间可取消）。
5. [ ] 实现 TurnDone 契约修正 + is_terminal/文档同步。
6. [ ] CancelRecovery restore 坑修复。
7. [ ] 测试：两类验证条件测试 + 受影响既有测试更新。
8. [ ] 文档：docs/agent-effect-model.md §8；docs/review-2026-07.md M-ERR-2 标 ✅；
   检查 facade 层（run_full / stream）对取消 outcome 的消费是否需同步。
9. [ ] 门禁：fmt → clippy（默认 + external features）→ test（默认 + external）→ doc。
10. [ ] TODO.md 标 [DONE] + 完成记录；commit；停止。

## 最终设计（探索后定稿）

1. **`CancellationToken::cancelled()`**（cancel.rs）：`CancellationState` 增加
   `tokio::sync::Notify`；`cancel()` 置位后 `notify_waiters()`。`cancelled()` 收集祖先链，
   对每个节点 `Notified::enable()` 注册后复查 flag，再 `select_all` 等待——闭合注册竞态，
   父取消也能唤醒子等待。
2. **参考 handler ctx 贯通**：`LlmClientHandler::fulfill` 用 `tokio::select!`（biased）
   竞争 `ctx.cancellation().cancelled()` 与整个 LLM 调用；取消时返回
   `RequirementResult::Llm(Err(ClientError::Other("...cancelled")))`——该值必然被 drain
   的 fulfill 后复查丢弃（取消单调），不作为 wire 错误语义。facade `StreamingTapHandler`
   同类接线（class-wide）。tool/interaction/reconfig handler 有意不中断（副作用安全），
   模块文档注明。
3. **drain / drive_streamed**：(a) 既有批间取消点改为对**全部** pending 逐一
   `NeverResumed` 记录 + `Abandon`（首个 abandon 闭合整轮，后续被 M4-4 软拒绝，no-op）；
   (b) `fulfill_batch` 返回后、resume 前新增取消复查——已兑现但不回灌，逐条
   `NeverResumed` + `Abandon`，置 `cancelled = true` 后 break。
4. **`TurnDone`** 增加 `cancelled: bool`：`new` 不变（默认 false），新增
   `with_cancelled(bool)` builder 与 `cancelled()` 访问器；rustdoc 改为「自然结束 →
   terminal `Done|Error` 且 `cancelled()==false`；取消 → 机器经 never-resume 停在 `Idle`
   且 `cancelled()==true`」。
5. **facade 消费点**（agent.rs:374、stream.rs:277）：match 首位加
   `cursor if done.cancelled()` 守卫，返回措辞准确的取消错误（`FacadeError::Agent(Other(
   "agent run cancelled ..."))`），替代误导性的 "non-terminal cursor (Idle)"；专用
   Cancelled 错误面归 M5-4，完成记录注明。
6. **CancelRecovery restore 坑**：`begin_user_turn` 的准入守卫与 Done|Error → Idle 重置
   均纳入 `CancelRecovery(_)`（它是 transient 恢复标记，快照可能在两次 transition 之间
   捕获它；scratch 重建本就把 CancelRecovery 映射为 None），恢复后 turn 边界可喂食。
7. **测试**：cancel.rs 3 条（自身/父链/预取消）；reference.rs 1 条（阻塞 client 飞行中
   取消有界返回）；drive.rs 3 条（预取消全部 outstanding 逐一 NeverResumed + cancelled 标
   记；fulfill 后复查不再 resume 且记 NeverResumed 而非 Resumed；自然结束 cancelled==
   false）；machine 1 条（CancelRecovery 快照点 feed UserMessage 可开新 turn）。
8. **文档**：effect-model §8 补取消 settle/复查/TurnDone 契约；migration §12 如有矛盾
   措辞同步；review-2026-07 M-ERR-2 标 ✅（第 4 条 reconfig abandon 已由 M4-4 修复，标注
   M4-4+M4-5）。

## 进度日志

- 2026-07-19：开始 M4-5。上一任务 M4-4 已完成（HEAD dd9e707）。探索完成，设计定稿
  （见上节）。开始实现。
- 2026-07-19：实现完成——CancellationToken::cancelled()（Notify + 祖先链 select_all +
  enable 闭合竞态）、drain/drive_streamed 双观测点 + 全量 NeverResumed/Abandon
  （settle_cancelled + SettledRef）、TurnDone.cancelled、LlmClientHandler 与
  StreamingTapHandler biased select 贯通、facade 取消守卫臂、begin_user_turn 纳入
  CancelRecovery。测试 12 条新增/更新；两条集成测试按新契约更新（complex_cancel
  phase B 改断 LLM 批次截停；reference cancel-during-tool-wait 改为 tool 批次飞行中
  取消的确定性形态）。fmt + 两侧 clippy 通过；全量测试（默认 + external）后台运行中。
- 2026-07-19：全量门禁通过——全量测试默认 exit 0 + external features exit 0；cargo doc
  初报 drain rustdoc 指向私有 fulfill_batch 的 intra-doc link，改明文后默认 + external
  两遍均通过。TODO.md M4-5 已标 [DONE] 并写完成记录。M4-5 完成，提交后停止。
