# M6-1 — `WorkerProfileRef`、worker profile registry 与 `WorktreeIsolation`

**状态:完成(全绿,已提交)。** 本次执行 TODO.md 第一个未完成任务 = M6-1(Milestone 6 首个任务)。

## 任务要求(TODO.md M6-1)
- 定义 `WorkerProfile { id, capabilities: Vec<Capability>, cost_tier, escalation: EscalationRules }`
  与 `WorkerProfileRef`,以及内存 `WorkerProfileRegistry`(注册/按 ref 解析)。
- 把 `ExternalAgentSpec.profile` 从占位 `WorkerProfileRef` 升级为真实的 registry-backed ref。
- 明确 `WorktreeIsolation` 各级语义与默认(强 worker 默认独立 worktree,见 §10)。
- 验证:`cargo test --lib worker_profile`;完整验证序列全绿。

## 设计输入
- 设计文档 §4.1(static spec,`profile: WorkerProfileRef`)、§8(worker 集合:internal-cheap /
  deepseek / cc / cx / opencode / review)、§9(能力/成本维度与升级规则)、§10(worktree 隔离默认)。
- 现有类型:`WorkerProfileRef`(占位,`external/spec.rs`)、`WorktreeIsolation`(`external/mod.rs`,
  Shared / PerAgentWorktree / EphemeralGitWorktree)、`PermissionRisk`(有序枚举参考)。

## 方案(全部落在 `src/agent/external/` 内,避免跨模块循环依赖)
1. 新建 `src/agent/external/profile.rs`:
   - `Capability`:Search/Shell/Test/BugFix/Feature/Refactor/Review/Debug/CodeGeneration/Planning +
     `Custom(String)`(对齐 §9 任务类型 + 逃生舱)。
   - `CostTier`:Cheap < Standard < Premium(`Ord`,`Default = Cheap`);`recommended_isolation()`
     映射 Cheap→Shared、Standard→PerAgentWorktree、Premium→EphemeralGitWorktree(强 worker 独立 worktree)。
   - `EscalationTrigger`:Timeout/TestFailure/LowConfidence/ReviewRejected/BudgetExhausted(§9 升级规则)。
   - `EscalationRules { triggers, escalate_to: Option<WorkerProfileRef>, human_fallback }`。
   - `WorkerProfileRef`(从 spec.rs 移来,transparent String id)。
   - `WorkerProfile { id, capabilities, cost_tier, escalation }` + `reference()` / `has_capability()` /
     `recommended_isolation()`。
   - `WorkerProfileRegistry`(BTreeMap<id, WorkerProfile>):`register`/`resolve`/`get`/`contains`/`len`。
   - 单测(名字含 `worker_profile`):registry 注册+解析(含未知返回 None)、profile serde round-trip、
     WorktreeIsolation/CostTier 默认与推荐隔离策略断言。
2. `external/mod.rs`:`mod profile;`;`pub use profile::{...}`;`WorkerProfileRef` 改从 profile 导出;
   新增 `impl Default for WorktreeIsolation`(= PerAgentWorktree,§10「默认隔离」)+ 文档。
3. `external/spec.rs`:删除本地 `WorkerProfileRef` 定义,改从 `super::profile` 引入;更新 rustdoc,
   去掉「placeholder」措辞。
4. `src/agent/mod.rs`:re-export 新 profile 类型。
5. 文档:`docs/external-agent.md` §10 追加隔离默认策略的收敛说明(保持简洁)。

## 验证序列
fmt --check → `cargo test --lib worker_profile` → clippy -D warnings →
`cargo test --all --all-targets`(≤30min)→ doc -D warnings → `git diff --check`。

## 约束
- `WorkerProfile` 字段严格对齐任务要求(id/capabilities/cost_tier/escalation),隔离由 cost_tier 派生。
- 所有新公开 API 带 rustdoc;不改动现有 external machine/state 运行语义。
- 完成后 TODO.md M6-1 标 `[DONE]` + 完成记录;提交 `[M6-1] ...`;停止。
