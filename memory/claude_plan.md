# M6-4 — cheap → strong 升级与 verifier

**当前执行 TODO.md 第一个未完成任务 = M6-4**(M6-1..M6-3 均 `[DONE]`)。前置 M6-3 已完成
(`src/agent/collab/*`,plan/blackboard/mailbox + 桥接工具)。M6-2 提供 `Dispatcher` /
`WorkerRoster` / `WorkerChoice`(`into_subagent` → `NeedSubagent`) / `DispatchReason`。

## 任务要求(TODO.md M6-4)
1. 在 dispatcher 之上实现 escalation:根据 worker 结果(失败 / 低置信度 / 超预算)触发重新分派到
   更强 worker,或降级 / 升到 human(经 `InteractionKind::Permission` 或 `Question`)。
2. 提供 `verifier` 挂点(review-agent / tests),在高风险或复杂任务后运行并驱动升级判断。

## 验证条件(过滤名 `cargo test escalation`)
- cheap worker 失败触发 strong worker 重派;
- 超预算触发降级 / 停机问用户;
- verifier 失败触发升级。
- 完整验证序列全绿。

## 设计(新增 `src/agent/external/escalation.rs`)
复用既有类型:EscalationTrigger/EscalationRules(profile.rs)、WorkerRoster/WorkerChoice/
DispatchReason(dispatch.rs)、PermissionRisk/PermissionRequest/Interaction。不新造 orchestration
runtime——输出仍是 WorkerChoice,由 into_subagent 走既有 SubagentHandler。

- WorkerReport { worker, triggers }(serde;succeeded/failed/new/with_trigger/worker/triggers/is_clean/raised)。
- Verifier trait:verify(task, report) -> Option<EscalationTrigger>(None=通过)。
  ScriptedVerifier(closure,new/passing/rejecting)。
- TaskDescriptor::warrants_verification()(risk>=High || impact>=CrossModule || Ambiguous):
  engine 仅在 warranting 任务上跑 verifier;report.triggers 始终生效。
- HumanGate { step, actor }。
- EscalationOutcome:Accept / Reassign(WorkerChoice) / Human(Interaction) / Exhausted { trigger }。
- Escalator<V: Verifier>(new(verifier) / with_budget_headroom,默认 20%):
  assess(task, report, roster, ctx, gate):
  1. check_cancelled。2. triggers = report ∪ verifier(warranting)。3. 空→Accept。
  4. 预算压力(BudgetExhausted 或 budget_is_low)→ 严格更便宜则 Reassign(BudgetDowngrade),否则 Human(Question)。
  5. 否则 upward:escalate_to 或 strongest(严格 tier>current)则 Reassign(Escalation);
     否则 human_fallback→Human(ReviewRejected→Permission,余→Question),否则 Exhausted。
- DispatchReason::Escalation 新增变体;budget_is_low 改 pub(super) 复用。
- EscalationError:UnknownWorker / NoCapableWorker / Context(RunContextError)。

## 接线
- external/mod.rs:mod escalation; + pub use。 agent/mod.rs:追加同名导出。

## 测试(src/agent/external/escalation/tests.rs,名字含 escalation)
三条必需验证 + 预算精度、explicit escalate_to、Exhausted、UnknownWorker、cancelled、
LowConfidence、warrants_verification、report builders、into_subagent、verifier gating、
Permission human gate、serde round-trip。

## 文档
- docs/external-agent.md §9「收敛(Milestone 6-4 已实现)」;profile.rs 文档更新;README 模块表。

## 验证序列(完成前)
cargo fmt --all → cargo clippy --all-targets -- -D warnings → cargo test escalation →
cargo test --all --all-targets(≤30min) → RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace。

## 完成后
TODO.md M6-4 标 [DONE] + 填完成记录;commit [M6-4] ...;停(不开始 M6-5)。

---

## 执行结果(M6-4 已完成)
- [x] 新增 `src/agent/external/escalation.rs` + `escalation/tests.rs`(24 单测,名字含 escalation)。
- [x] dispatch.rs:`DispatchReason::Escalation`、`budget_is_low` pub(super)、`warrants_verification`。
- [x] 接线 external/mod.rs + agent/mod.rs;文档 external-agent.md §9 / profile.rs / README。
- [x] 验证序列全绿:fmt ✓;test escalation=24 ✓;clippy -D warnings ✓;test --all --all-targets
      (lib 545 passed)✓;doc -D warnings ✓。
- [x] TODO.md M6-4 标 [DONE] + 完成记录。下一步:commit `[M6-4] ...` 后停(不开始 M6-5)。
