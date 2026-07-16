# M1-2 — 把 `mod.rs` 的 fallible 方法改造为返回 `Result<StepOutcome, StepError>`

**当前执行 TODO.md 第一个未完成任务 = M1-2**（M1-1 已 DONE）。刀 (C) 第二步：
把 `src/agent/machine/default/mod.rs` 里除 tools.rs 外的 fallible 方法改成返回
`Result<StepOutcome, StepError>`，`if let Err(..) return self.fail(..)` 塌缩成 `?`。
本任务不做 step() 最外层折叠（M1-3）、不做 tools.rs 失败路径重写（M1-4）。

## 需改造方法（全部 -> Result<StepOutcome, StepError>）
begin_user_turn, open_user_turn, inject_pivot, block_on_llm, resume, resume_llm,
fold_llm_response, commit_text_turn, finalize_text_commit, emit_reconfig_effect,
resume_reconfig, abandon, abandon_llm_step, abandon_reconfig, finish_cancel

## 错误变体路由（保证文案逐字节一致）
- ConversationError (begin_turn/cancel_pending/inject_user_message/start_assistant_response/
  finish_assistant/commit_pending) -> `?` -> Conversation -> "conversation operation failed: {e}"
- queued_reconfig_application / pivot.validate() (均 AgentStateError) -> `?` -> State ->
  "agent state operation failed: {e}"
- transition_cursor (AgentStateError, 但文案不同) -> `.map_err(StepError::CursorTransition)?`
  -> "cursor transition failed: {e}"
- next_requirement_id (RequirementError) -> `?` -> Requirement -> "requirement id unavailable: {e}"
- RequirementResult::Reconfig(Err(ToolRuntimeError)) -> StepError::ToolRuntime(e) ->
  "tool runtime operation failed: {e}"（已确认 Reconfig 的 Err 是 ToolRuntimeError）
- 纯协议违例 & "client operation failed"(ClientError) -> StepError::Protocol(格式化字符串原样)

## 跨模块调用桥接
- mod.rs(Result) 调 tools.rs(StepOutcome) 方法：`Ok(self.resume_tool(..))` /
  `Ok(self.resume_approval(..))` / `Ok(self.begin_tool_phase(..))` / `Ok(self.abandon_tool_phase(..))`
- tools.rs(StepOutcome) 调 mod.rs 现在返回 Result 的方法（2 处）：
  - tools.rs:552 finish_tool_phase -> block_on_llm
  - tools.rs:639 abandon_tool_phase -> finish_cancel
  两处临时桥接 `.unwrap_or_else(|error| self.fail(error.message()))`，标注 `// M1-3 will replace with fail_from`
- step()(mod.rs:853) 临时桥接同上，标注 `// M1-3 will replace with fail_from`
- error.rs 顶部 `#![allow(dead_code)]` 现可移除（变体已被使用）；若 message()/某变体仍未用到再局部处理

## 验证条件
1. cargo fmt --all -- --check
2. cargo test -p agent-lib agent::machine::default（断言不改）
3. cargo clippy --all-targets -- -D warnings
4. cargo test --all --all-targets
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6. git diff --check

## 完成后
TODO.md M1-2 标 [DONE] + 完成记录；commit `[M1-2] ...`；停（不做 M1-3）。

## 进度
- [x] 读 TODO/源码，确认类型路由与跨模块桥接点
- [x] 改 mod.rs 15 个方法签名 + 体（if let Err 计数 10 -> 0）
- [x] tools.rs 2 处桥接 + step() 桥接（均标注 M1-3 will replace with fail_from）
- [x] error.rs 移除 dead_code allow + 更新模块 doc
- [x] fmt/clippy/build/聚焦(39 passed)/全量测试/doc/diff 全绿
- [x] TODO.md 标 DONE + 完成记录（并修复误删的 M1-3 标题）
- [x] commit `[M1-2] ...`；停（不做 M1-3）
