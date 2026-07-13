# 执行计划 — M6-R:Milestone 6 与迁移总 Review

## 选中的任务
`TODO.md` 第一个未完成任务 = **M6-R**(M1..M6-2 全 `[DONE]`)。前置 M6-1..M6-2 已 `[DONE]`。
工作树起始 clean,HEAD=615817c(M6-2)。这是 Review 任务,不拆分。

## 任务要求(TODO.md M6-R)
1. 回溯 PLAN.md 与 TODO.md 全文,逐条确认迁移不变量:
   - sans-io `step` 不 await
   - requirement/notification 二分
   - `id + origin` 可寻址
   - pop 路由与顶层 total(UnhandledRequirement)
   - cancel=never-resume 接 `cancel_pending`
   - 多路径 `fork_at` 无 multishot
   - RunContext 由 scope 派生
   - serde/runtime 分离
2. 确认 Conversation Core 不变量(committed log、pending、tool pairing、Boundary、restore)
   在 Agent 层未被重新实现或绕开。
3. 确认旧 push API(respond_approval / pivot queue / AgentFeedGuard / AgentEvent::Done)
   已删除或明确保留理由,文档与代码一致。
4. 汇总遗留/后续项(决策 C 排序、决策 D token tee 最终形态)到"后续"小节。

## 验证命令(全套)
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test --all --all-targets(超时 ≤30min)
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
- git diff --check

## 步骤
1. [ ] 读关键 agent-layer 源码,逐条核对不变量(grep 旧 API 是否残留)。
2. [ ] 后台跑 fmt+clippy+test+doc,收集结果。
3. [ ] TODO.md 标 M6-R [DONE] + 写入 Review 结论 + 后续小节。
4. [ ] 提交,停止(仅文档变更,若代码未变可复用绿测结果)。

## 关键发现(Review)
- fmt/clippy/test(lib 435 + e2e 4 + 其它,0 failed)/doc/diff --check 全绿。
- 不变量核对:sans-io step(machine/mod.rs 纯同步不 await)、requirement/notification 二分、
  id+origin 寻址、pop 路由+顶层 UnhandledRequirement、cancel=never-resume 接 cancel_pending、
  fork_at 无 multishot、RunContext 由 scope 派生、serde/runtime 分离 —— 均已落地。
- 旧 push API:respond_approval / pivot queue / AgentFeedGuard / DefaultAgentLoop / loop_driver
  已从源码删除(仅存历史 archive 文档)。
- **DEFECT(doc↔code 不一致)**:migration doc 头部与 M6-1 改写的 agent-layer.md §1.3 均**明列**
  `AgentEvent 单一混装流`/`AwaitingApproval`/`Done(Outcome)` 为**已删除**旧 push API,
  但 src/agent/event.rs 仍定义 `AgentEvent`+`ApprovalRequest`+`AgentOutcome(Kind)`+`AgentFailure`
  +`BudgetExhaustedOutcome`+`ExternalRecoveryKind/Outcome`,且这些类型**仅**被自身 test 与
  mod.rs re-export 引用(无任何 live 路径使用)。→ 死代码,与文档相悖。
  **修复**:删除这些死 legacy 类型 + `non_empty` helper + `From<Notification> for AgentEvent` +
  相关 test,重写 Notification/module doc 去掉 `[AgentEvent]` intra-doc link,更新 mod.rs re-export。
  保留 live:AgentInput/AgentUserInput/Notification+payloads(StepBoundary/ToolCallStarted/
  ToolCallFinished)/AgentError/AgentErrorKind。

## 进度
- 完成。Review 八项不变量核对全通过;Conversation Core 不变量未被 Agent 层重实现/绕开。
- 发现并修复唯一 doc↔code 不一致:删除死 legacy `AgentEvent`/`ApprovalRequest`/`AgentOutcome`
  家族/`AgentFailure`/`ExternalRecovery*`/`non_empty`/`From<Notification> for AgentEvent`,
  重写 Notification/module doc + mod.rs re-export + README。保留 live 类型。
- 验证全绿:fmt clean;clippy 0 warning;test lib 434 + e2e 4 + 其它,0 failed;doctest 12;
  doc clean;git diff --check clean。
- TODO.md:M6-R → [DONE] + 完整 Review 结论 + 后续小节(决策 C 排序 / 决策 D token tee /
  driver 编排归调用者)。PLAN.md 无阶段变更,不改。
- M6-R 是最后一个任务 → 所有任务完成,提交后打 `endtag`。
