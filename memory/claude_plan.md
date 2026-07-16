# M6-5 — Milestone 6 Review 与文档并轨

**当前执行 TODO.md 第一个未完成任务 = M6-5**(M6-1..M6-4 均 `[DONE]`,最新 commit `[M6-4]`)。
这是 Milestone 6 的收官 review 任务,也是 TODO.md 最后一个任务。完成后所有任务 `[DONE]`,
按 PROMPT「Completion & Release」需打 git tag `endtag`。

## 任务要求(TODO.md M6-5)
1. 端到端跑一个混合场景:coordinator 经 dispatcher 把明确任务派给 cheap worker、复杂任务派给
   external agent worker,验证 plan/blackboard 协作、升级(escalation)与 artifact 汇总。
2. 回填 `docs/external-agent.md` §14 已收敛的开放问题(调度策略取向、mailbox 是否需一等 API 等)。
3. 全量测试无回归;复核各里程碑公开 API 均有 rustdoc。

## 验证条件
- 完整验证序列全绿;`cargo test --all --all-targets` 无回归;`RUSTDOCFLAGS="-D warnings" cargo doc
  --no-deps --workspace` 无告警。
- 端到端混合场景测试通过(过滤名 `mixed_scheduler`)。
- 文档并轨结论写入完成记录,`docs/external-agent.md` §14 标注已收敛项。

## 设计:新增 `tests/agent_mixed_scheduler.rs`(过滤名含 `mixed_scheduler`)
复用既有公开 API,不新增库代码(纯集成 + 文档任务):
- Roster:`internal-cheap`(Cheap: Search/Shell/Debug)+ `cc-agent` external(Premium:
  Feature/Debug/Refactor,EscalationRules escalate_to strong / human_fallback)。
- Plan:coordinator 建 `search`(无依赖)与 `feature`(depends_on search),验证依赖 gating、
  claim/update、blackboard 跨 agent 读写。
- Dispatcher:`dispatch(search)`→cheap(RuleRouter clearly_light);`dispatch(feature)`→strong
  (RuleRouter heavy: High risk / QualityFirst CrossModule)。
- 派生:`WorkerChoice::into_subagent`→`NeedSubagent`→`DrivingSubagentHandler` 驱动最小 child
  machine 完成(复用 agent_tool_adapter.rs 的 ImmediateChildMachine/EmptyScope/spawner 模式)。
- Escalation:cheap 跑 debug 失败(`WorkerReport::failed(TestFailure)`)→`Escalator::assess`→
  `Reassign(Escalation)` 到 strong → 再派生 strong。
- Artifact 汇总:`RecordingArtifactSink` 收集两个 worker 的 `ExternalArtifactRef` → 断言齐全。

测试函数(均含 `mixed_scheduler`):
1. `mixed_scheduler_dispatch_routes_clear_to_cheap_and_complex_to_external`。
2. `mixed_scheduler_plan_blackboard_coordinate_workers`。
3. `mixed_scheduler_cheap_failure_escalates_to_external_and_aggregates_artifacts`(主 E2E)。

## 文档
- `docs/external-agent.md` §14:标注「调度策略取向」(规则优先 + LLM fallback,M6-2)与
  「Mailbox 一等 API」(M6-3 已提供一等 `Mailbox`)已收敛;可顺带收敛 worktree 冲突策略(M6-1
  WorktreeIsolation)。§15 收敛段可点名 dispatch/escalation/collab 已实现。
- README 已含 escalation;确认 mixed-scheduler 模块表齐全。

## 验证序列(完成前)
cargo fmt --all → cargo clippy --all-targets -- -D warnings → cargo test mixed_scheduler →
cargo test --all --all-targets(≤30min) → RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace。

## 完成后
TODO.md M6-5 标 [DONE] + 完成记录;commit `[M6-5] ...`;所有任务完成 → 打 tag `endtag`;停。

## 进度
- [ ] 编写 tests/agent_mixed_scheduler.rs
- [ ] 文档 §14/§15 并轨
- [ ] 验证序列
- [ ] TODO.md 标 DONE + commit + endtag

---

## 执行结果(M6-5 已完成)
- [x] 新增 `tests/agent_mixed_scheduler.rs`(3 tests,过滤名 `mixed_scheduler`);纯集成,复用公开 API。
- [x] `docs/external-agent.md` §14 标注三项收敛(worktree 隔离/规则+LLM fallback/Mailbox 一等 API);
      §15 追加「收敛(Milestone 6-5 已实现)」段落背书 dispatch/escalation/collab + E2E 测试。
- [x] 验证序列全绿:fmt ✓;test mixed_scheduler=3 ✓;clippy -D warnings ✓;test --all --all-targets
      (lib 545 passed, mixed_scheduler 3 passed, 全绿, 仅 credential-gated ignored)✓;
      doc -D warnings ✓;git diff --check 干净 ✓。
- [x] TODO.md M6-5 标 [DONE] + 完成记录。M1..M6 全部 [DONE]。
- [x] 下一步:commit `[M6-5] ...` → 打 tag `endtag` → 停。
