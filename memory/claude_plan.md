# 当前任务：M3-2 cancel during subagent/tool wait 场景

## 定位
- `TODO.md` 第一个未完成任务 = **M3-2**（行 582，首个 `[TODO]`）。前置 M3-1 已 `[DONE]`。
- HEAD=18ff0e1（[M3-1]），工作树干净。Milestone 3，非 review 任务，**不拆分**。
- 产出：新建 `tests/agent_complex_cancel.rs`，单测
  `complex_cancel_abandons_child_and_preserves_committed_state`。

## 关键机制（已核对源码）
- `drain` 在每次循环顶部、fulfill 前检查 `ctx.is_cancelled()`：命中则把首个 pending
  requirement 记为 `NeverResumed`@scope0、喂 `Abandon` 给 machine，然后 break（返回 Ok）。
  → 要让 child `resume_count==0 & abandon_count==1`，必须在 child drain 首个 fulfill 前
  child_ctx 已 cancelled。
- `DrivingSubagentHandler::fulfill`：depth guard → child_ids → `derive_child`（继承
  parent 的 budget + cancel，共享 trace Arc）→ spawn → `drain(child, child_ctx)` → summarize。
  parent ctx 先 cancel，则 child_ctx 继承 cancel，child 首个 req 立即被 abandon，child
  handler 从不执行。参照 `src/agent/drive/subagent/tests.rs::parent_cancel_propagates_and_abandons_child`。
- `ScopePop`/`Pop`/`SubagentHandler` 均 `pub`（src/agent/mod.rs 导出），集成测试可直接驱动 handler。
- 不能用 `CancelOnCall` 打到 child：它在 fulfill 内 cancel，requirement 仍被 Resume（resume_count>=1）。
  故采用 “先 cancel parent ctx 再驱动 subagent handler” 的参考写法。

## 设计（复用 testkit + M1 支持层）
shared `Arc<MockPlanBlackboardStore>`：
- Phase A（cancel abandons child）：
  - seed：create(v0) → add_task(review,[])(v1) → claim(review,"worker",1)(v2, InProgress)。
    post("worker","review started")（代表 worker 取消前已提交的进度）。ver=2。
  - child = `ScriptMachine` emit 1×`NeedTool`：`plan_update(review, worker, completed, ev=2)`
    （若执行会把 review 置 Completed —— 正是不该发生的 side effect）。`.idle_on_abandon()`。
  - child scope = `headless_child_scope().tool(complex_tool_handler(store)).build()`。
  - spawner = `ScriptedSubagentSpawner::builder(ids.clone()).child(child).summary("review cancelled before completion").build()`；`handler = spawner.into_handler(4)`。
  - outer = `ScopePop::new(&TestScope::builder().build(), None)`。
  - `ctx.cancellation().cancel()`，再 `handler.fulfill(&spec_ref, &brief, None, &mut outer, &ctx).await` → `Subagent(Ok(_))`。
- Phase B（cancel 后仍可用）：fresh `ctx2`，parent = `complex_agent_machine`（DefaultAgentMachine），
  scripted LLM：① tool_use blackboard_post("parent","review cancelled")+plan_update(review,worker,cancelled,ev=2) ② text。
  `drain(&mut cleanup, user_input, &complex_scope(llm,tool,None), None, &ctx2)` → Done。

## 断言
- ids_calls/spawn_calls/summarize_calls == 1（生命周期在 cancel 下仍干净收尾）。
- child log: `abandon_count()==1`、`resume_count()==0`。
- child tool 从不执行：`assert_tool_executions(child_tool, PLAN_UPDATE, 0)` 且 `calls().is_empty()`。
- Phase A 后 review 仍 `InProgress`、owner=worker；board==["review started"]（无重复/无 completed side effect）。
- trace：`subagent_count(1)`；child NeedTool req `resolved_at_scope(0).never_resumed()`。
- Phase B 后：review==`Cancelled`；board==["review started","review cancelled"]（无重复 started）；
  `assert_conversation(cleanup.state().conversation()).committed_turns(1).pending_none()`；done cursor==Done。

## 验证顺序
`cargo fmt --all -- --check` → `cargo clippy --all-targets -- -D warnings` →
`cargo test --test agent_complex_cancel complex_cancel_abandons_child_and_preserves_committed_state` →
`cargo test --all --all-targets`（<30min）→ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` → `git diff --check`。

## 完成
TODO.md M3-2 `[TODO]`→`[DONE]` + 写完成记录；提交 `[M3-2] ...`；停止。
