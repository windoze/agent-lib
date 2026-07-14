# 当前任务：M4-R Milestone 4 Review

## 定位
- `TODO.md` 第一个未完成任务 = **M4-R Milestone 4 Review**（line 962，标题为 `[TODO]`）。
- M4-1/M4-2/M4-3 均已 `[DONE]` 并提交（HEAD=505e515 `[M4-3] Implement read-only assertions module`）。
- 工作区有一个**无关**未跟踪文件 `docs/external-agent.md`（16:17 创建的设计草案，非 M4-R 产物，不纳入本次提交）。

## 任务要求（TODO.md M4-R）
Review，确认 harness/assertions 降低样板但不掩盖行为：
1. 检查 `StepHarness` 是否仍能精确暴露中间 requirement。
2. 检查 `DrainHarness` 是否保留原始 `AgentError`。
3. 检查 assertions 是否只读且 failure message 可诊断。
4. 更新下一阶段（M5/M6）迁移目标清单。
验证：全套验证命令通过；Review 结论写入完成记录。

## 计划步骤
1. [x] 写 plan。
2. [x] 审阅 `harness.rs`：StepHarness 中间 requirement 暴露；DrainHarness 透传 AgentError。
3. [x] 审阅 `assertions/*`：只读性、failure message 可诊断性。
4. [x] 交叉核对 PLAN.md 的 M5/M6 迁移目标，确认下一阶段清单准确。
5. [x] 运行验证：fmt --check -> clippy -D warnings -> 全量 test -> rustdoc -D warnings -> git diff --check。
6. [x] 将 Review 结论写入 TODO.md M4-R 完成记录，标题加 `[DONE]`。
7. [x] 若 PLAN(无需改动).md 阶段计划需更新则更新；否则仅在 TODO/memory 记录。
8. [ ] 提交(进行中)（仅 M4-R 相关：TODO.md、memory、可能 PLAN.md），不含无关 external-agent.md。
9. [ ] 停止。

## 备注
- 无已知阻塞 spec 偏差。若审阅发现掩盖行为/非只读/诊断不足，则作为真实问题处理（修复或插入前置任务）。
