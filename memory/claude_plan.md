# 当前任务：M5-3 实现 scripted subagent spawner 与 parent/child scope helpers

## 定位
- `TODO.md` 第一个未完成任务 = **M5-3**（line 1058，标题 `[TODO]`）。
- 前置 M5-2 已 `[DONE]` 且提交（HEAD=f0abf85）。
- 工作区：无关未跟踪文件 `docs/external-agent.md`（非本任务产物，不纳入提交）。

## 任务要求（TODO.md M5-3）
- 在 `subagent.rs` 实现 `ScriptedSubagentSpawner: SubagentSpawner`。
- 支持 child_ids deterministic 分配、spawn closure、summary script。
- 提供 `SpawnedChildBuilder`，组合 machine、scope、opening input。
- 提供 parent/child scope helper：headless child、attended child、parent with subagent handler。
- 与 `ScriptMachine`、`SeqIds`、`TestScope` 集成。

## 设计（crates/agent-testkit/src/subagent.rs）
- `ScriptedSubagentSpawner`（impl SubagentSpawner）：ids/trace_label/ChildSource(Once|Factory)/summaries 队列/default_summary/三计数。
  - child_ids=计数+(run_id,trace_node)；spawn=计数+take/factory；summarize=计数+弹出脚本或 default。
  - builder(ids)：.child()/.child_factory()/.summary()/.summaries()/.trace_label()/.build()。
  - 访问器 ids()/ids_calls()/spawn_calls()/summarize_calls()；into_handler(Arc<Self>,max_depth)->DrivingSubagentHandler。
- SpawnedChildBuilder：.machine()/.boxed_machine()/.scope()/.boxed_scope()/.opening()/.build()。
- scope helpers（返回 TestScopeBuilder）：headless_child_scope()/attended_child_scope(interaction)/parent_scope_with_subagent(handler)。
- prelude 追加 kit 新类型 + agent-lib（AgentSpecRef/DrivingSubagentHandler/Interaction/SpawnedChild/SubagentOutput/SubagentSpawner/TurnDone）。

## 单测（5）
1. attended_parent_serves_headless_child_interaction_via_pop。
2. depth_guard_refuses_without_spawning。
3. parent_cancel_propagates_and_abandons_child。
4. child_token_charge_counts_against_parent_budget。
5. attended_child_resolves_its_interaction_in_place（覆盖 attended_child_scope）。

## 步骤
1. [x] 写 plan。
2. [x] 读源码。
3. [x] 实现 subagent.rs。
4. [x] prelude 再导出。
5. [x] 单测。
6. [x] fmt→clippy→聚焦→全量(全绿)→rustdoc→git diff --check 全过。
7. [x] TODO.md M5-3 标 [DONE] + 完成记录。
8. [ ] 提交（仅 M5-3；不含 external-agent.md）。停止。

## 备注
- 无已知阻塞 spec 偏差。
