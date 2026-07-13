# 执行计划 — M6-2:端到端验收示例(attended 父 + headless 子)

## 选中的任务
`TODO.md` 第一个未完成任务 = **M6-2**(M1..M6-1 全 `[DONE]`)。前置 M6-1 已 `[DONE]`。
工作树起始 clean,HEAD=8d89171(M6-1)。不拆分。

## 任务要求(TODO.md M6-2)
- 新增 `tests/agent_effect_e2e.rs`(选 tests/,用 crate 公共 API,`cargo test` 直接跑)。
- 用离线 fake `LlmClient`/`ToolRegistry` + policy interaction 后端。
- 父 agent(顶层 scope 挂 interaction=policy)派生 headless 子 agent(内层 scope 不挂 interaction);
  子 `NeedInteraction` pop 到父被兑现;跑完一个含 tool 与 subagent 的 turn。
- 覆盖父子并发兑现、cancel 传播、budget 聚合的端到端断言。
- 证明"同一 subagent spec,attended vs headless 只是 scope wiring 不同,子无需配置"。

## 关键事实(已读代码)
- `DefaultAgentMachine` 只发 `NeedLlm/NeedTool/NeedInteraction/NeedReconfigRegistry`,不发 `NeedSubagent`。
  → 父机用小脚本机器 `ParentBatchMachine`(仿 `ScriptMachine`)发 `[NeedTool, NeedSubagent]`;
  子机用真实 `DefaultAgentMachine`(weather 工具 + RequireApprovalPolicy),被 fake client/registry 驱动。
- `drain(machine, input, scope, parent, ctx)`:批内本地可兑现的 requirement 用 FuturesUnordered 并发;
  `NeedSubagent` 恒串行,handler 收到 `outer=ScopePop(本层 scope, 本层 parent)`。
- 子 headless scope 无 interaction → 子 `NeedInteraction(approval)` pop 到父 scope 的 interaction handler。
- budget:handler 侧充值(参考 `ChargingLlmHandler`);`derive_child` 共享 ledger → 子充值计到父。
- cancel:`ctx` 取消后 drain 对首个 requirement 走 `Abandon`(never-resume)。
- 公共 API 全部可用:drain/DrivingSubagentHandler/SubagentSpawner/SpawnedChild/ScopePop/HandlerScope/
  ToolRegistryHandler/ApprovalInteractionHandler/RunContext/BudgetLimits/...
- ids:共享 `AtomicU64` 生成唯一 uuid 串,实现 `RequirementIds` + `ToolExecutionIds`。

## 测试设计(tests/agent_effect_e2e.rs,多个 #[tokio::test])
共享 fakes + 子机构造 `build_child_machine()` + `child_scope(attended)` 两处共用 → 证明 same spec。
1. attended_parent_headless_child_pop_and_budget(pop+层级+budget 聚合)
2. same_child_spec_attended_resolves_in_place(same graph = scope wiring)
3. batch_tool_requirements_fulfilled_concurrently(并发兑现,max in-flight==2)
4. parent_cancel_propagates_and_abandons_child(cancel 传播)

## 步骤
1. [x] 写 tests/agent_effect_e2e.rs。
2. [x] fmt → clippy(-D warnings)→ 聚焦跑 → 全套 test → doc(-D warnings)→ git diff --check。
3. [x] TODO.md 标 M6-2 [DONE] + 完成记录。
4. [x] 提交,停止。

## 进度
- 完成。4 个 `#[tokio::test]` 全绿:attended_parent_serves_headless_child_via_pop /
  same_child_spec_attended_resolves_in_place / batch_requirements_are_fulfilled_concurrently /
  parent_cancel_propagates_and_abandons_child。
- 验证:`cargo fmt --all`;`cargo clippy --all-targets -- -D warnings` 0 warning;
  `cargo test --all --all-targets` 全绿(lib 435 + 集成含新增 4,0 failed);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` clean;`git diff --check` clean。
- TODO.md:M6-2 → [DONE] 并补完整完成记录;M6-R 仍 [TODO]。下一次调用处理 M6-R。
