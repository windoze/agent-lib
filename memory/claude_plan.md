# 当前任务：M1-3 实现复杂测试断言 helper

## 定位
- `TODO.md` 第一个未完成任务 = **M1-3**（首个 `[TODO]`，行 179）。前置依赖：M1-2（已 `[DONE]`，commit e4256c0）。
- HEAD=e4256c0，工作树干净。属于 Milestone 1「Support 与 Mock Vertical Features」。

## 目标（TODO.md M1-3）
新建 `tests/complex_support/assertions.rs`，提供只读断言 helper，失败信息按 docs/complex-tests.md §6
携带 role sequence / tool result status / store ops / handler log。helper 只读观察对象，不改
machine/store/context。在 `mod.rs` re-export 常用类型/helper。

### plan/blackboard 断言（失败打印 store.ops_summary）
- assert_task_status(store, id, status)
- assert_task_owner(store, id, owner)
- assert_task_depends_on(store, id, expected: &[&str])
- assert_no_task_owner(store, id)
- assert_board_messages(store, expected_substrings_in_order: &[&str])（长度相等 + 逐条 contains）

### conversation helper（复用 agent_testkit::assert_conversation，不重写）
- role_sequence(conversation, turn_index) -> Vec<Role>（越界 panic 带 summary）
- assert_pivot_after_tool_result(conversation, pivot_text)：扫描全体消息顺序，找到某个 Tool 结果后
  出现的、文本含 pivot_text 的 User 消息；失败打印 role 序列。

### handler log helper
- assert_tool_executions(handler: &ComplexToolHandler, tool_name, count)（execution_count，失败打印调用日志）
- assert_interaction_decisions(log: &InteractionCallLog, expected)（completed_len）

### mod.rs
- pub mod assertions;
- re-export：plan_blackboard 常用类型、tools 常量与类型、assertions helper。
  若触发 unused_imports，加 #[allow(unused_imports)]。

## 新增单测（tests/agent_complex_support.rs）
1. assertions_report_store_ops_on_failure（必需）：建 store 建 plan+board，先跑通过路径，
   再 catch_unwind 一个错误期望，断言 panic 文本含 store ops 摘要。
2. role_sequence_and_pivot_helpers_find_expected_messages（必需）：StepHarness 驱动
   complex_agent_machine 走 user->tool_use(safe_read)->tool result->mid-turn pivot->final text。
   断言 role_sequence(0)=[User,Assistant,Tool,User,Assistant]，assert_pivot_after_tool_result 命中。
3. handler_and_store_assertions_hold_after_approved_dangerous_write（补充）：DrainHarness 端到端
   dangerous_write->approval(Approve)->执行->final text。断言 assert_tool_executions、
   assert_interaction_decisions、assert_board_messages。

## 验证顺序
- cargo fmt --all -- --check
- cargo test --test agent_complex_support <两个必需测试名>
- cargo clippy --all-targets -- -D warnings
- cargo test --all --all-targets（<=30min）
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- git diff --check

## 完成后
- TODO.md M1-3 标题 [TODO]->[DONE]，补完成记录。提交 [M1-3] ...。停止。

## 进度
- [进行中] 调研完成（store/handler/conversation/harness API）。开始写 assertions.rs。
- [完成] assertions.rs + mod.rs re-export + 三个单测全部落地。fmt/clippy/doc/全量测试(423+131 等，0 fail，4 ignored=credential-gated)与 git diff --check 全部通过。TODO.md M1-3 标记 [DONE] 并补完成记录。待提交 [M1-3]。
