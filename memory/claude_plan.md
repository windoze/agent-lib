# 当前任务：M5-R Milestone 5 Review

## 定位
- `TODO.md` 第一个未完成任务 = **M5-R**（line 1096，标题 `[TODO]`）。
- 前置 M5-1..M5-3 均 `[DONE]` 且已提交（HEAD=34f6c07）。
- 工作区：仅无关未跟踪文件 `docs/external-agent.md`（非本任务产物，不纳入本次提交）。

## 任务要求（TODO.md M5-R）
- 核对没有真实 sleep。
- 核对 cancel helpers 不吞掉 never-resume 语义。
- 核对 subagent helpers 不绕过 `DrivingSubagentHandler` 的深度/预算/cancel 强制。
- 列出 M6 要迁移/新增的测试套件文件。
- 验证：全套验证命令全部通过；Review 结论写入完成记录。

## Review 结论（已核对源码）
1. 无真实 sleep（通过）：concurrency 用协作式 yield/barrier/计数；仅 cassette/record.rs 的 SystemTime 用于 M3 文件名时间戳，非 M5 并发路径。
2. cancel helpers 不吞 never-resume（通过）：CancelOnCall 只置取消标志+记 log，仍透传 inner 真实结果；never-resume 由 driver 落实（resume_count==0/abandon_count==1，PanicOnCall llm 未运行）。
3. subagent helpers 不绕过 DrivingSubagentHandler（通过）：ScriptedSubagentSpawner 只供 policy，经 into_handler 包真实 handler；depth/budget/cancel 守卫在 agent-lib::drive::subagent::fulfill，子测试驱动真实 handler 验证三守卫。
4. M6 目标：M6-1 tests/agent_effect_e2e.rs；M6-2 src/agent/drive/reference/tests.rs；M6-3 agent_step_basic/agent_tool_basic/agent_interaction_basic/agent_driver_basic/agent_trace_budget_basic；M6-4 agent_replay_text/agent_replay_tool/agent_replay_approval|regression。

## 验证策略
- 自 M5-3（HEAD=34f6c07）绿后无任何编译代码改动。本任务只改 TODO.md/memory。
- fmt --check + clippy(root & testkit) + 聚焦 concurrency/subagent lib 测试确认门为绿；全套 cargo test --all 复用 M5-3 绿结果（仅文档变更）。

## 步骤
1. [x] 写 plan。
2. [x] 读 M5 源码 + 子测试 + DrivingSubagentHandler。
3. [x] fmt --check + clippy + 聚焦 M5 测试(122 passed)。
4. [x] TODO.md M5-R 标 [DONE] + 完成记录。
5. [x] 提交（仅 M5-R：TODO.md + memory）。停止。

## 备注
- 无已知阻塞 spec 偏差；无新增未调度失败测试。
