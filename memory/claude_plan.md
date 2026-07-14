# 当前任务：M4-1 实现 plan claim conflict / dependency-blocked recovery 场景

## 定位
- `TODO.md` 第一个未完成任务 = **M4-1**（行 735，首个 `[TODO]`）。前置 M3-R 已 `[DONE]`。
- HEAD=0b92980（[M3-R]），工作树干净。
- 这是**新增复杂 mock 测试**任务，不改生产代码（除非发现真实 bug）。

## 目标（来自 TODO M4-1）
在 `tests/agent_complex_flow.rs` 新增测试
`complex_plan_claim_conflict_or_dependency_block_recovers_through_blackboard`，
用单个 agent turn（DrainHarness + ScriptedLlmHandler）覆盖两条恢复路径：
1. 同一 task 的第二次 claim 返回 version conflict（stale expected_version）。
2. 前置未完成的 task claim 返回 dependency-blocked。
两者都作为 model-visible ToolStatus::Error 回给 LLM，LLM 恢复后调用
`plan_claim_first_available` 认领另一个可用 task。

## 设计（离线确定性，无 sleep/网络）
直接 seed（版本已知）:
- create_plan(); // v0
- add_task("task-a", []); // v1
- add_task("task-b", ["task-a"]); // v2  (依赖 task-a)
- add_task("task-c", []); // v3  (独立可认领)
version=3。

Scripted LLM 5 步（每步单一 board 写入，避免 intra-batch board 顺序不确定）:
- Step1: claim task-a owner=worker-1 exp_ver=3 -> OK -> v4
- Step2: post "claim conflict on task-a" + claim task-a owner=worker-2 exp_ver=3
  -> VersionConflict(expected 3, actual 4)，不 mutate
- Step3: post "dependency blocked on task-b" + claim task-b owner=worker-2 exp_ver=4
  -> DependencyBlocked(task-b, [task-a])，不 mutate
- Step4: claim_first_available owner=worker-2 exp_ver=4 -> 跳过 task-a(InProgress/owned)
  与 task-b(dep 未满足)，认领 task-c -> v5
- Step5: final text 收尾

## 断言
- 单一 owner：task-a owner=worker-1（InProgress）；task-c owner=worker-2（InProgress）。
- blocked task 未被修改：task-b 仍 Todo、无 owner、depends_on=[task-a]。
- blackboard 保留 conflict/block 两条消息（exactly 2，assert_board_messages）。
- tool_result 可见错误：c-claim-a-dup=Error、c-claim-b=Error；c-claim-a/c-claim-first=Ok。
- version==5，证明只有两次成功 claim，冲突/阻塞未 bump。
- committed 1 turn、pending none、last assistant text。

## 需要的 import 补充
- assertions: assert_task_owner
- tools: PLAN_CLAIM_FIRST_AVAILABLE

## 验证命令
- cargo fmt --all -- --check
- cargo test --test agent_complex_flow complex_plan_claim_conflict_or_dependency_block_recovers_through_blackboard
- cargo clippy --all-targets -- -D warnings
- cargo test --all --all-targets（<30min）
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- git diff --check

## 完成
- 全部通过后：TODO.md M4-1 [TODO]->[DONE] + 写完成记录；提交 [M4-1] ...；停止。

## 进度
- (完成) 新增 M4-1 测试并通过全部验证命令；TODO.md M4-1 标 [DONE] 并写入完成记录；提交并停止。
