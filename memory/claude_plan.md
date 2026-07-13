# 执行计划 — M5-R：Milestone 5 Review

## 选中的任务
`TODO.md` 第一个未完成任务 = **M5-R**（M1..M5-3 全 `[DONE]`）。这是 Review 任务，不拆分。
前置 M5-1..M5-3 均已 `[DONE]`。工作树在开始时 clean，HEAD=f1ce9fb（M5-3）。

## Review 四项核对点（TODO.md M5-R）
1. 嵌套机器整树可序列化、requirement 按 `id + origin` 精确路由、父子并发兑现按完成顺序回灌。
2. 深度上限、预算继承、cancel 传播全部在 subagent handler 强制（不散落别处）。
3. "同一 spec 在挂/不挂 interaction 的 scope 下 attended/headless 自动切换"有端到端测试。
4. trace resolved-by-scope + disposition 完整。

## 核对结论（逐项，均已通过代码走查确认）
1. **通过**。`machine/nested.rs`:整树 serde（`Serialize for NestedMachine` + `MachineTreeState`/
   `ChildState`，`deny_unknown_fields`；`from_state` 递归重建 path 与 handle）。按 id 路由:
   `route_by_id` + `subtree_contains` 扫各 cursor `pending_requirement_ids`;origin 由
   `stamp_requirements`/`rebase_cursor_origin` 打真实 `AgentPath`,`outstanding_requirements`
   与 cursor binding 同源一致。并发按完成顺序:`drive.rs::fulfill_batch` 用 `FuturesUnordered`,
   本层集完成序收集,不可本层兑现者串行经 `resolve_requirement`。
   测试:`step_aggregates_parent_and_child_requirements_with_real_paths`、
   `resume_routes_by_id_to_the_child_only`、`whole_tree_round_trips_and_each_cursor_restores_independently`、
   `attach_child_rejects_an_occupied_slot`、`drain_resolves_a_concurrent_batch_out_of_order`。
2. **通过**。三护栏集中在 `DrivingSubagentHandler::fulfill`(drive/subagent.rs):①深度守卫先行
   (`ctx.depth() >= max_depth` → `SubagentDepthExceeded`,不 mint id 不 spawn);②`RunContext::derive_child`
   (context.rs)共享 budget ledger(`budget.clone()`)+ 派生 cancel token(`cancellation.derive_child()`)+
   `depth+1`,预算继承/cancel 传播天然获得;③child drain 的 pop 目标为 `outer`,子未兑现 requirement
   pop 到外层。CancellationToken 子观察父链(cancel.rs)。
   测试:`depth_guard_refuses_at_limit_without_spawning`、`parent_cancel_propagates_and_abandons_child`、
   `child_token_charge_counts_against_parent_budget`。
3. **通过(机制已测;完整同 spec 双跑验收为下游 M6-2)**。
   `attended_parent_serves_headless_child_interaction_via_pop`:子 scope 不挂 interaction(headless)
   → 子 `NeedInteraction` pop 到挂 interaction 的父 scope(attended)被兑现 count==1,子/父均完成。
   attended-本层直服由 drive.rs 的 interaction handler 测试覆盖。"同一 subagent spec 在两种 scope 下
   两跑"的完整离线端到端验收示例是 M6-2 的专属任务(依赖链已正确:M6-2←M6-1←M5-R),M5-R 不重复。
4. **通过**。`context/trace.rs`:`RequirementDisposition{Resumed,NeverResumed}` +
   `TraceNodeKind::Requirement{kind_tag,resolved_at_scope,disposition}` + `record_requirement`。
   `drive.rs::drain` 单处记录:Resumed 批经 `record_requirement_resolution(...,resolved_at_scope,Resumed)`,
   cancel 分支 `record_requirement(...,0,NeverResumed)`;`resolved_at_scope` = pop 跳数(经 `Pop::pop`
   返回 `(result,hops)` 累加)。测试:`drain_records_resolved_at_scope_for_local_and_popped_requirements`、
   `drain_records_never_resumed_disposition_on_cancel`、`requirement_trace_node_round_trips_through_serde`。

## 验证结果（本轮实跑）
- `cargo fmt --all -- --check`:clean。
- `cargo clippy --all-targets -- -D warnings`:0 warning。
- `cargo test --all --all-targets`:lib 435 passed / 0 failed;doctest 3 passed;集成/示例全绿;
  网络用例 ignored(需凭据)。每测试 <1min。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`:clean。
- `git diff --check`:clean。

## 结论
Milestone 5 四项核对全部通过,无 spec 偏差、无 workaround、无未排期失败测试。未引入新 prerequisite。
标记 M5-R `[DONE]`,写完成记录,提交并停止。PLAN.md 无阶段级变更,不改。
