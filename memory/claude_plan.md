# 当前任务：M4-4 更新文档、运行说明与测试矩阵

## 定位
- `TODO.md` 第一个未完成任务 = **M4-4**（行 920，首个 `[TODO]`）。前置 M4-1/M4-2/M4-3 均 `[DONE]`（HEAD=1078b77）。
- 工作树干净。本任务仅改文档（docs/*.md、README.md、TODO.md、memory），不改任何 Rust 代码。

## 现状勘查（已完成）
所有 P0/P1 复杂场景均已落地，无一 deferred。测试名与 `docs/complex-tests.md` 建议名一致：
- P0-1 `complex_turn_combines_plan_blackboard_approval_deny_and_pivot` → tests/agent_complex_flow.rs
- P0-2 `complex_subagent_updates_shared_plan_and_pops_approval_to_parent` → tests/agent_complex_subagent.rs
- P0-3 `complex_cancel_abandons_child_and_preserves_committed_state` → tests/agent_complex_cancel.rs
- P1-1 `complex_plan_claim_conflict_or_dependency_block_recovers_through_blackboard` → tests/agent_complex_flow.rs
- P1-2 `complex_approval_cancel_does_not_cancel_context_unless_driver_cancels` → tests/agent_complex_cancel.rs
- P1-3 `complex_pivot_then_subagent_uses_rerendered_brief` → tests/agent_complex_subagent.rs
支持层（M1）测试在 tests/agent_complex_support.rs：
- plan_dependencies_reject_unknown_self_and_cycles / claim_rejects_unfinished_dependencies_atomically /
  claim_first_available_skips_blocked_and_claimed_items / blackboard_is_append_only_and_offsets_are_monotonic /
  assertions_report_store_ops_on_failure / role_sequence_and_pivot_helpers_find_expected_messages
支持模块：tests/complex_support/{mod,plan_blackboard,tools,assertions}.rs

## 改动计划
1. docs/complex-tests.md：新增 §11「落地状态」——
   - 映射表：场景 → 测试文件 → 测试名 → 状态（全部已落地）。
   - 列出 M1 支持层测试与 tests/complex_support/ 结构。
   - 明确 mock store 仍是测试支持层、非生产 plan API。
   - 明确无 P1 deferred（全部落地）。
   - 各测试如何单独运行的命令示例。
2. docs/TESTABILITY.md §8.2「现状」：补一段现状，指出 §8.2 复杂组合现已有专门的
   `tests/agent_complex_*.rs` 复杂 mock 套件直接覆盖（多轮/approval/subagent pop/cancel/plan-blackboard/pivot），
   不再仅是 reference_driver/agent_effect_e2e 的等价覆盖。仍非独立 scenario DSL 套件。
3. README.md 「构建与测试」：补充如何单独运行复杂 mock 套件的示例（README 已有测试运行示例，符合可选修改条件）。

## 验证
- `git diff --check`（仅文档改动，无需 Rust 构建；上次全量绿结果沿用，因未改编译产物）。

## 完成
- TODO.md M4-4 [TODO]->[DONE] + 完成记录；提交 `[M4-4] ...`；停止。

## 进度
- (完成) 文档已更新：complex-tests.md §11、TESTABILITY.md §8.2、README 测试运行示例；git diff --check 干净；仅文档改动复用上次全量绿结果。TODO.md M4-4 标 [DONE]。准备提交并停止。
