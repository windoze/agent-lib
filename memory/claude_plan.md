# 当前任务：M6-3 新增 Core Rust suites

## 定位
- `TODO.md` 第一个未完成任务 = **M6-3**（line 1215，标题 `[TODO]`）。M6-1/M6-2 已 `[DONE]`（HEAD=da09b2e）。
- 前置依赖 M6-2 已完成。无阻塞。

## 目标
在 root `agent-lib` 的集成测试层（`tests/`，dev-dep 已含 agent-testkit，与 M6-1/M6-2 同一 seam）
新增 5 个 filter-可单跑的 Core Rust suites，用 testkit（StepHarness/DrainHarness + scripted handlers
+ ScriptMachine + TestScope + assertions），快、稳、离线。避免复制既有底层单测：这些是集成套件，
断言聚焦 agent 可观察终态（conversation/notifications/trace/budget/cursor）。

## 五个套件（tests/*.rs，各自独立 test binary → cargo test --test <name> 单跑）
1. tests/agent_step_basic.rs（StepHarness，同步 #[test]）
   - NeedLlm emit：user() → 单 NeedLlm、StreamingStep、outstanding=[llm_id]。
   - resume text：resume(assistant_text) → quiescent/Done、conversation 1turn/2msg/末 assistant 文本。
   - wrong id：try_resume(stray) → Err 命名 cursor+outstanding；机器未步进；随后真 id 提交。
   - wrong kind：try_resume(NeedLlm, Tool result) → Err "rejected"；未步进。
   - abandon：abandon(llm_id) → outstanding 清空、cursor=Idle、无 committed turn/pending。
2. tests/agent_tool_basic.rs（DrainHarness）
   - single tool：llm[tool_use,text]+tool[ok] → Done、tool 1、4msg、末文本。
   - parallel tool：llm[tool_use(2),text]+tool[ok,ok] → Done、tool 2、两 call started/finished。
     （峰值并发已由 e2e batch_requirements_are_fulfilled_concurrently 覆盖，此处不复制。）
   - tool error：llm[tool_use,text]+tool[error] → Done、tool_result_status Error、tool 1。
   - step limit：max_steps=1 spec，llm[tool_use]+tool[ok] → Error cursor、tool 1、pending none。
   - provider call mismatch：tool[ok("wrong-call")] → Error cursor、turns 空、pending none。
3. tests/agent_interaction_basic.rs（DrainHarness + 本地 RequireApprovalPolicy）
   - approve：approve_all → tool 跑、末文本、Done、tool_result Ok。
   - deny：deny_all → tool 未跑、denied result、Done、status Denied。
   - timeout：fixed(Timeout) → status Denied、Done、tool 未跑。
   - cancel：fixed(Cancel) → status Cancelled、Done、tool 未跑。
   - wrong call/step rejection：fixed(Response(错 step_id 的 approval)) → drain validate 拒绝
     → AgentError::Other(misaligned)、tool 未跑。
4. tests/agent_driver_basic.rs（ScriptMachine + TestScope + ScopePop + drain）
   - local handler：ScriptMachine[NeedTool]+scope.tool → Done、resume_tags=[Tool]。
   - pop to parent：inner headless[NeedInteraction]+outer.interaction via ScopePop → Done、outer 服务。
   - top unhandled：headless top[NeedInteraction]、无 parent → AgentError::UnhandledRequirement{Interaction}。
   - misaligned result：ScriptMachine[NeedTool]+scope.tool=MisalignedHandler(Llm) → AgentError::Other(misaligned)。
5. tests/agent_trace_budget_basic.rs（drain + assert_trace/assert_budget + derive_child）
   - resolved_at_scope：inner.tool(hop0)+outer.interaction(hop1) via ScopePop → 两节点 scope 0/1 Resumed。
   - never-resumed：cancel 前置 → drain 记 NeverResumed、handler 未调用。
   - budget shared ledger：derive_child→child.charge_tokens→parent snapshot 反映、depth+1、subagent_count 1。

## 覆盖矩阵映射（完成记录用，docs/TESTABILITY.md §8.1 / §7）
- agent_step_basic → §8.1 行 `agent_step_basic`（StepHarness, fixtures）。
- agent_tool_basic → §8.1 行 `agent_tool_basic`（StepHarness/DrainHarness, ScriptedToolHandler）。
- agent_interaction_basic → §8.1 行 `agent_interaction_basic`（ScriptedInteractionHandler）。
- agent_driver_basic → §8.1 行 `agent_driver_basic`（ScriptMachine, TestScope）。
- agent_trace_budget_basic → §8.1 行 `agent_trace_budget_basic`（assert_trace, assert_budget）。
- §7 覆盖矩阵：text turn / tool turn / parallel tools / approval / headless / pop routing / cancel / trace / budget 行。

## 校验顺序
cargo fmt --all --check → clippy --all-targets -D warnings → 5 个 --test 聚焦 →
全套 cargo test --all --all-targets（≤30min）→ RUSTDOCFLAGS=-D warnings cargo doc → git diff --check
→ TODO.md M6-3 标 [DONE]+完成记录（含 coverage map）→ commit（[M6-3]）。停止。

## 进度
- [x] 读 harness/handlers/scope/machine/fixtures/script/assertions + drive.rs + default tests，确认全部 API 与行为
- [x] 写 5 个 tests/*.rs（step 5 / tool 5 / interaction 5 / driver 4 / trace_budget 3 = 22 用例）
- [x] fmt/clippy/聚焦(5×)/全套/rustdoc/diff 全绿
- [x] TODO.md 标 [DONE] + 完成记录（coverage map）
- [ ] commit（[M6-3]）。停止。

## 备注
- `docs/external-agent.md` 为未追踪的无关设计草案（非 M6-3 产出，TODO/PLAN 未引用），不纳入本次提交。

## 备注
- 不新增 testkit 能力即可完成（能力在 M1–M5 齐备）。若写用例暴露真实 bug/未调度失败测试，按策略插前置任务并停。
- step limit/provider mismatch：drain 视 Error 为 terminal → 返回 Ok(TurnDone, Error cursor)。
- wrong-call interaction：drain 先 validate(accepts) → AgentError::Other，不到机器 Error cursor。
