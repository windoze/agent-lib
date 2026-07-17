# M9-2 接入 usage/cost budget charging

**当前任务 = TODO.md 首个未完成 = M9-2**（`### [TODO] M9-2`, line 2738）。M1..M9-1 全 `[DONE]`。

## 任务要求（TODO.md 2738-2763 + design §17）
- handler/driver 层记录 external usage/cost：有则 charge 到 RunContext；无则记 unknown，不估算。
- budget exceed：adapter advance 前预检查；advance 中超限统一返回 `ExternalAgentError::LimitExceeded`。
- trace 记录 usage/cost 来源为 external runtime reported。
- 验证：unit tests（reported charged / missing 不 charge / budget exceeded 停 session + cleanup）；
  `cargo test -p agent-lib external_budget`；完整验证序列 1-6。

## 现状核对（已读源码）
- `ExternalAgentOutput { summary, artifacts, usage: Option<Usage>, cost_micros: Option<u64> }` @ mod.rs:818。
- `Usage::{total: Option<u32>, total_computed()}` @ model/usage.rs。
- `RunContext::{charge_tokens,charge_usage,charge_cost_micros,budget(),trace(),run_id()}` @ context.rs。
- `BudgetHandle`/`BudgetSnapshot`/`BudgetUsage`/`BudgetLimits`/`BudgetDimension`/`BudgetError` @ context/budget.rs。
- `ExternalAgentError::LimitExceeded { limit }` @ mod.rs:967；`RunContextError::Budget(#[from] BudgetError)`。
- `ExternalSessionHandler::fulfill(&self, &ExternalSessionRequest, &RunContext) -> RequirementResult` @ drive.rs:225。
- `RequirementResult::ExternalSession(Box<ExternalSessionResult>)` @ effect_manifest.rs:178。
- `ExternalSessionResult::{Completed{session,output,observations}, Failed{session,error,observations}, Paused*}`。
- `ExternalSessionRegistry::cleanup(agent_id, &session) -> ExternalSessionShutdown` @ registry.rs:255。
- `TraceHandle::record_external_shutdown(id, disposition)` 已存在；trace id 由 caller 提供（crate 约束）。
- worktree.rs (M9-1) 用 per-manager AtomicU64 计数器造唯一名 → mint trace id 沿用同法。

## 设计（新文件 src/agent/external/budget.rs，default 构建即编译，不 feature-gate）
1. `ExternalUsageCharge`（Clone/Debug/PartialEq/Eq）：reported `tokens: Option<u64>` / `cost_micros: Option<u64>`。
   - `from_output(&ExternalAgentOutput)`：tokens = usage.total.unwrap_or(total_computed())；cost = cost_micros。不估算。
   - accessors + `is_unknown()`（两者皆 None）。
   - `charge(&self, &RunContext) -> Result<(), ExternalAgentError>`：present 才 charge，BudgetError→LimitExceeded。
2. `budget_exhausted(&RunContext) -> Option<BudgetDimension>`：某维度有 limit 且 used>=limit → 该维度（steps>tokens>cost 顺序）。
3. `ExternalSessionSweeper`（async trait）：`sweep(agent_id, &session) -> ExternalSessionShutdown`；
   impl for `ExternalSessionRegistry`（委托 cleanup）；`NoSweep` 空实现（返回 Graceful，宿主自管 teardown）。
4. `ExternalUsageChargingHandler<H, S = NoSweep>`：`new(inner)` / `with_sweeper(inner, sweeper)`；per-handler AtomicU64 trace 计数。
   impl `ExternalSessionHandler`：
   - 预检 `budget_exhausted` → 若已耗尽：有 session 则 sweep+record_shutdown，返回 `Failed{LimitExceeded}`，不调 inner。
   - 调 inner.fulfill；非 ExternalSession family 原样透传；Paused* 原样透传。
   - `Completed`：charge，成功→record_external_usage（source=runtime reported）后原样返回 Completed；
     BudgetError→sweep+record_usage+record_shutdown，转 `Failed{session:Some, LimitExceeded}`（停 session+cleanup）。
5. trace：context/trace.rs 新增 `TraceNodeKind::ExternalUsage { tokens_charged, cost_micros_charged }`
   （节点存在即代表 source=external runtime reported，None 表示 runtime 未报告=unknown，不估算）
   + `TraceHandle::record_external_usage(id, tokens, cost)`。无穷举 match 依赖它，安全。
6. 导出：external/mod.rs + agent/mod.rs re-export 新公有项；trace 项经 context re-export。

## 测试（inline `external_budget_*` 前缀，全离线 <1s）
reported charged / missing 不 charge / partial(仅 tokens) / 预检各维度 / 预检失败不调 inner+sweep /
charge 超限→Failed+sweep(cleanup)+session 保留 / charge 映射 LimitExceeded / trace 记录 usage source /
Paused 透传不 charge / registry 实现 sweeper（编译期断言）。

## 验证序列
fmt --check → `cargo test -p agent-lib external_budget` → clippy -D warnings →
`cargo test --all --all-targets` → doc -D warnings → git diff --check。

## 完成状态
M9-2 已完成并 `[DONE]`。验证序列 1-6 全过（fmt/clippy(±features)/doc/git-check 干净，external_budget 17 passed，full suite 46 ok 0 failed）。下一个未完成任务 = M9-3。
