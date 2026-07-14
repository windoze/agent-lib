# 当前任务：M2-2 补充主 flow 的负向断言与防回归用例

## 定位
- `TODO.md` 第一个未完成任务 = **M2-2**（首个 `[TODO]`，行 394）。前置依赖：M2-1（`[DONE]`）。
- HEAD=503bfd2（[M2-1]），工作树干净。属于 Milestone 2。
- 非 review、单一执行单元，**不拆分**。

## 目标（TODO.md M2-2）
在 `tests/agent_complex_flow.rs` 增加两个聚焦测试，把 plan dependency 与 approval deny 的错误面固定住：
1. `claim_dependency_block_returns_tool_error_and_does_not_mutate_task`
   - 直接通过 `ComplexToolHandler.fulfill` 调 `plan_claim` claim `implement`（design 未 completed）。
   - 断言返回 `ToolStatus::Error`，错误文本 model-visible 且提及被阻塞依赖 design。
   - 断言 owner/status/version 不变（implement 仍 Todo、无 owner、version 不变）。
2. `denied_dangerous_write_does_not_execute_tool`
   - 用 `DrainHarness` + 脚本 LLM(dangerous_write→text) + `ComplexToolHandler` + deny 交互。
   - 断言 dangerous execution log == 0，interaction 决策 1 次，turn Done，tool_result 为 Denied。
- 失败信息必须包含 store ops(assert_* helper) 或 handler log(assert_tool_executions)。

## 关键 API（已核对）
- 直接执行工具：`handler.fulfill(ids.tool_call_id(), &tool_call(...), &ctx).await` → `RequirementResult::Tool(Ok(ToolResponse))`。
- 建 plan：`store.create_plan()`→v0；`store.add_task("design", Vec::<String>::new())`→v1；
  `store.add_task("implement", ["design"])`→v2。`store.version()`==2。
- `store.claim` 校验顺序 version→owner→status→deps，dep 未完成 → `DependencyBlocked`，不改状态。
- DrainHarness：`complex_scope(Arc<llm>, Arc<handler> as Arc<dyn ToolHandler>, Some(Arc<interaction>))`；
  `DrainHarness::with_ids(machine,&scope,None,&ctx,ids).run_user(..).await`。
- 断言：`assert_conversation(conv).committed_turns(1).pending_none().tool_result_status("c-danger",Denied).last_assistant_text(..)`；
  `assert_tool_executions(&handler,DANGEROUS_WRITE,0)`；`assert_interaction_decisions(&log,1)`；`assert_done(turn_done)`。

## 新增 import
handlers::ScriptedLlmHandler；harness::DrainHarness；assertions::{assert_conversation,assert_done}；
script::LlmStep；tools::{complex_scope, PLAN_CLAIM}；model::tool::ToolResponse。

## 验证顺序
fmt --check → clippy --all-targets -D warnings → 两个指定测试 → cargo test --all --all-targets(<=30min)
→ RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace → git diff --check。

## 完成
TODO.md M2-2 [TODO]->[DONE] + 完成记录；提交 [M2-2]；停止。

## 进度
- [完成] 两个防回归测试写完并通过。fmt/clippy/两个指定测试/整文件 3 tests/全量 all-targets 全绿
  (lib 423 + testkit 131 + 集成 crate,credential-gated ignored)/doc(-D warnings)/diff --check 均通过。
  TODO.md M2-2 标 [DONE] 并写完成记录。待提交 [M2-2]。
