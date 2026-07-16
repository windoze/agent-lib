# M1-4 — 把 `tools.rs` 的纯失败路径改为 `?`，保留带副产品失败

**当前执行 TODO.md 第一个未完成任务 = M1-4**（M1-1~M1-3 已 DONE）。刀 (C) 第四步：
把 `src/agent/machine/default/tools.rs` 里返回 `StepOutcome` 的方法改签名为
`Result<StepOutcome, StepError>`，纯失败改 `?`/`Err(Protocol)`，带副产品失败保留
`Ok(self.fail_with_notifications(..))` 就地折叠。

## 关键约束（对外行为逐字节不变）
- 现有测试断言不改。需保留文案：`"tool id unavailable"`（tests/mod.rs:386）、
  `"conversation operation failed"`、`"step limit"`、`"not an in-flight tool call"`、
  `"NeedTool"`、`"interaction result rejected"`、`"get_weather"`。
- `StepError::ToolRuntime.message()` = `"tool runtime operation failed: {e}"`，与
  `"tool id unavailable: {e}"` 不同 → tool_ids 失败**不能**用裸 `?`，须
  `.map_err(|e| StepError::Protocol(format!("tool id unavailable: {e}")))?`。
- `register_tool_calls`/`append_tool_response`/`cancel_pending` 均返回 `ConversationError`
  → 裸 `?`（From<ConversationError> → Conversation → "conversation operation failed: {e}"）文案一致。
- `next_requirement_id` 返回 `RequirementError` → 渲染 "requirement id unavailable: {e}"
  与现文案一致（但这些点是**带副产品**，保留 fail_with_notifications）。

## 逐方法改造（tools.rs）
1. begin_tool_phase → Result。pending_tool_calls .map_err(Protocol)?；tool_call_id /
   tool_result_message_id .map_err(|e| Protocol("tool id unavailable: {e}"))?；
   register_tool_calls 裸 ?；in_flight None → Err(Protocol("...opened without an in-flight turn"))；
   末尾 advance_tool_phase 传播。
2. advance_tool_phase → Result。无 phase → Err(Protocol("...advanced without an active phase"))；三分支传播。
3. emit_tool_batch → Result。三处失败带副产品 → Ok(fail_with_notifications)；成功 Ok(..)。
4. emit_approval → Result。同上。
5. resume_tool → Result。纯失败 → Err(Protocol)；append 裸 ?；末尾 idle 传播 advance / else Ok(..)。
6. resume_approval → Result。纯失败 → Err(Protocol)；accepts_response/try_from
   .map_err(|e| Protocol("interaction result rejected: {e}"))?；append 裸 ?；
   Approve 传播 emit_tool_batch；Deny 分支 finished 后 cursor 失败带副产品 → Ok(fail_with_notifications)；末尾传播 advance。
7. finish_tool_phase → Result。step-limit / next_step_id / next_assistant_message_id 带副产品 →
   Ok(fail_with_notifications)；末尾 self.block_on_llm(..) 传播（去 unwrap_or_else + M1-4 注释）。
8. abandon_tool_phase → Result。open None → Err(Protocol)；cancel_pending 裸 ?；末尾 finish_cancel 传播。
9. pending_tool_calls 保留 Result<Vec<ToolCall>, String>。

## mod.rs 调用点去 Ok(..) 包裹
- L543 begin_tool_phase / L469 resume_tool / L472 resume_approval / L741 abandon_tool_phase

## 验证序列（1–6）
fmt / 聚焦(tools+default) / clippy / 全量(≤30min) / doc / git diff --check

## 完成后
TODO.md M1-4 标 [DONE] + 完成记录；commit `[M1-4] ...`；停。

## 进度
- [x] tools.rs 8 方法改签名 + 纯/副产品分流（+ error.rs doc 追加 M1-4）
- [x] mod.rs 4 调用点去 Ok 包裹
- [x] fmt / 聚焦(39 passed) / clippy / 全量(全绿) / doc / diff 全过
- [x] TODO.md 标 DONE + 完成记录
- [x] commit；停
