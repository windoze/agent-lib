# M6-2 — Task evaluator 与 dispatcher

**状态:完成(全绿,已提交)。** 本次执行 TODO.md 第一个未完成任务 = M6-2(Milestone 6 第二个任务)。
前置 M6-1 已 `[DONE]`(worker profile registry / WorktreeIsolation)。

## 任务要求(TODO.md M6-2)
- 定义 `TaskDescriptor`(任务类型/影响范围/风险/不确定性/预算等维度)与 `WorkerChoice`。
- 实现 `RuleRouter`(确定性映射)与 `Dispatcher`:先跑规则路由,未决则回退到可插拔
  `Evaluator` trait(LLM 版留接口,测试用 scripted evaluator)。
- dispatcher 输出为「派生哪个 worker 的 `NeedSubagent`(spec_ref)」,复用现有 subagent 派生路径,
  不新造 orchestration runtime。
- dispatcher 依据 `WorkerProfile`(M6-1)与 `RunContext` 预算(`charge_*`/`check_*`)选择 worker。
- 验证:`cargo test dispatcher`;完整验证序列全绿。

## 设计输入(已读)
- 设计文档 §8(worker 集合)、§9(两层调度:规则路由 + LLM evaluator;评估维度表;示例策略;升级规则)。
- 现有类型:`WorkerProfile/WorkerProfileRef/WorkerProfileRegistry/Capability/CostTier`
  (`external/profile.rs`);`AgentSpecRef` + `RequirementKind::NeedSubagent{spec_ref,brief,result_schema}`
  (`requirement.rs`);`RunContext`(`charge_step/charge_*`、`budget().snapshot()`、`check_cancelled`);
  `PermissionRisk`(Low<Medium<High<Critical,复用为任务风险维度);`Interaction::question`。

## 方案(新建 `src/agent/external/dispatch.rs`,与 profile 同模块,避免跨模块循环)
维度枚举(均 serde + Ord where 有意义):
- `ImpactScope`:SingleFile<MultiFile<CrossModule<Architectural(影响范围)。
- `Uncertainty`:Clear<Exploratory<Ambiguous(不确定性)。
- `CostPreference`:Balanced(default)/CostFirst/SpeedFirst/QualityFirst(§9 用户偏好)。
- 风险维度复用 `PermissionRisk`。

核心类型:
- `TaskDescriptor{ task_type:Capability, impact:ImpactScope, risk:PermissionRisk,
  uncertainty:Uncertainty, preference:CostPreference }` + `new`/`with_preference`/accessors(serde data)。
- `Worker{ profile:WorkerProfileRef, spec:AgentSpecRef }`:把调度 profile 绑定到要派生的子 agent spec。
- `WorkerRoster{ registry:WorkerProfileRegistry, workers:Vec<Worker> }`:
  `register(profile,spec)->ref`(同 id 覆盖)、`resolve_worker(ref)`、`profile(ref)`、
  `cheapest_capable(cap)`、`strongest_capable(cap)`(按 cost_tier 选,tie-break id 升序)。
- `DispatchReason`:RuleRoute/Evaluator/BudgetDowngrade。
- `WorkerChoice{ worker:WorkerProfileRef, spec:AgentSpecRef, reason }` +
  `into_subagent(brief,result_schema)->RequirementKind::NeedSubagent`(复用 subagent 派生路径)。
- `RuleRouter`(确定性,有序规则):Ambiguous→None;Architectural/risk>=High/(QualityFirst&&>=CrossModule)
  →strongest_capable;clear&low-risk&<=MultiFile 或 CostFirst&risk<=Medium→cheapest_capable;其余→None。
- `TaskEvaluator` trait:`evaluate(task,roster)->Option<WorkerProfileRef>`(LLM 版实现此 trait,留接口)。
- `ScriptedTaskEvaluator`(closure-based,`new`/`always`),用于测试与宿主脚本化决策。
- `Dispatcher<E:TaskEvaluator>`:`new(evaluator)`/`with_router`/`with_budget_headroom(percent)`。
  `dispatch(task,roster,ctx)->Result<WorkerChoice,DispatchError>`:
  1) check_cancelled;
  2) `budget_is_low(snapshot,headroom%)` → cheapest_capable + BudgetDowngrade(check_* 预算护栏);
  3) router.route Some → RuleRoute;
  4) 否则 `ctx.charge_step()`(evaluator 成本,charge_*);budget err → 降级 cheapest;
  5) evaluator.evaluate Some → Evaluator;None → Err(NoWorker)。
- `DispatchError`:NoCapableWorker{capability}/NoWorker/UnknownWorker{worker}/Context(RunContextError)(thiserror)。
- `budget_is_low`:对每个已配置 limit 维度,remaining < headroom% * limit(u128)→low;无 limit→false。默认 20%。

模块接线:
- `external/mod.rs`:`mod dispatch;` + `pub use dispatch::{...}`。
- `src/agent/mod.rs`:`pub use external::{...}` 追加同名导出。

测试(名字含 `dispatcher`,`cargo test dispatcher`):
1. rule route 命中:clear/low-risk/single-file search → RuleRoute → cheap worker。
2. 回退 evaluator:ambiguous debug → router None → scripted evaluator 选 strong → Evaluator;断言 charge 了 1 step。
3. 预算接近上限降级:max_steps=10 预扣 9,heavy 任务本应 strong → BudgetDowngrade → cheap。
4. `WorkerChoice::into_subagent` 生成 NeedSubagent 且 spec_ref = worker.spec。
5. evaluator 返回未注册 worker → UnknownWorker;无 capable worker → NoCapableWorker。
6. `TaskDescriptor` serde round-trip;`budget_is_low` 阈值断言。

## 验证序列
`cargo fmt --all` → `cargo test dispatcher` → `cargo clippy --all-targets -- -D warnings` →
`cargo test --all --all-targets`(≤30min)→ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
→ `git diff --check`。

## 约束
- 不新造 orchestration runtime;dispatcher 只“选 worker + 给出 NeedSubagent 输入”,派生走既有 subagent 路径。
- 所有新公开 API 带 rustdoc;复用 `PermissionRisk`/`Capability`/`AgentSpecRef`,不重复造。
- 完成后 TODO.md M6-2 标 `[DONE]` + 完成记录;提交 `[M6-2] ...`;停止。
