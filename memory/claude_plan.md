# 当前任务：M4-2 实现 approval cancel vs context cancel 区分场景

## 定位
- `TODO.md` 第一个未完成任务 = **M4-2**（行 794，首个 `[TODO]`）。前置 M4-1 已 `[DONE]`（HEAD=36e0cae）。
- 工作树干净。纯新增复杂 mock 测试，不改生产代码（除非发现真实 bug）。
- 测试落到 `tests/agent_complex_cancel.rs`（已有 M3-2 never-resume 测试）。

## 目标（来自 TODO M4-2）
新增 `#[tokio::test] complex_approval_cancel_does_not_cancel_context_unless_driver_cancels`，
钉住两个 cancel 语义的区别：
- **Approval Cancel** 只取消单个 guarded tool call；`RunContext` 保持存活，LLM loop 继续。
- **Context cancel** 是 never-resume：abandon outstanding requirement，handler 不执行。

## 设计（离线确定性，无 sleep/网络）

### Phase A — approval Cancel 不取消 context
- fresh ids_a/ctx_a/store_a（`complex_tool_handler`）。
- interaction = `ScriptedInteractionHandler::sequence([InteractionDecision::Cancel(Some("not now"))])`。
- LLM 三步：
  1. tool_use([dangerous_write "a-danger"]) → gated → NeedInteraction → Cancel → 机器合成 `ToolStatus::Cancelled`，tool 不执行。
  2. tool_use([safe_read "a-safe"]) → auto-approve → 执行（证明 loop 继续）。
  3. text("continued after the approval cancel") → 收尾。
- DrainHarness.run_user 一次 drain 完成。
- 断言：
  - `!ctx_a.is_cancelled()`（KEY：approval cancel 未取消 context）。
  - `assert_tool_executions(DANGEROUS_WRITE, 0)`、`assert_tool_executions(SAFE_READ, 1)`。
  - `assert_interaction_decisions(log, 1)`。
  - 无 never-resumed trace 节点（`never_resumed_requirement_ids(ctx_a).is_empty()`）。
  - committed 1 turn、pending none、`tool_result_status("a-danger", Cancelled)`（唯一识别 Cancel 决策）、
    `tool_result_status("a-safe", Ok)`、`last_assistant_text(...)`。
  - `assert_board_messages(store_a, &[])`（危险写未落地）。

### Phase B — context cancel abandon outstanding requirement（never-resume）
- fresh ids_b/ctx_b/store_b（避免污染，符合 TODO 要求）。
- LLM = `CancelOnCall::after(ScriptedLlmHandler::from_steps([tool_use([safe_read "b-safe"])]))`。
  `after` 在 LLM 第一次返回后取消 ctx；随后机器 emit NeedTool，drain loop 顶部检测到 cancelled → abandon（never-resume），tool handler 从不执行。
- scope 无 interaction handler。
- 断言：
  - `ctx_b.is_cancelled()`（KEY：driver cancel 取消了 context）。
  - `cancel_llm.log().cancelled()`、`cancelled_at() == Some(0)`、`dispatched() == 1`。
  - `assert_tool_executions(SAFE_READ, 0)`（handler 未执行）。
  - trace 恰有一个 never-resumed 且为 Tool。
  - committed 1 turn、pending none、`tool_result_status("b-safe", Cancelled)`（机器合成）。
  - `assert_board_messages(store_b, &[])`。

两 cancel 的可区分性：trace never-resumed 计数（A=0 / B=1）+ `is_cancelled` 标志 + tool 执行计数。

## 验证命令
- cargo fmt --all -- --check
- cargo clippy --all-targets -- -D warnings
- cargo test --test agent_complex_cancel complex_approval_cancel_does_not_cancel_context_unless_driver_cancels
- cargo test --test agent_complex_cancel
- cargo test --all --all-targets（<30min）
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- git diff --check

## 完成
- 全绿后：TODO.md M4-2 [TODO]->[DONE] + 完成记录；提交 `[M4-2] ...`；停止。

## 进度
- (完成) 新增 M4-2 测试 complex_approval_cancel_does_not_cancel_context_unless_driver_cancels，两 phase 覆盖 approval Cancel 不取消 context / driver cancel abandon outstanding requirement；全部验证命令通过；TODO.md M4-2 标 [DONE] 并写入完成记录；准备提交并停止。
- 关键发现：abandon_tool_phase 走 CancelDisposition::ResumeTurn，给 outstanding call 合成 Cancelled result 使 pending coherent，但因无 final answer 收尾 → 未提交（committed_turns=0、pending_present、open_call_count=0）。
