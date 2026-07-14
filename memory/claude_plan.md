# 当前任务：M2-1 实现 `agent_complex_flow` 主场景

## 定位
- `TODO.md` 第一个未完成任务 = **M2-1**（首个 `[TODO]`，行 315）。前置依赖：M1-R（`[DONE]`）。
- HEAD=1d98249（[M1-R]），工作树干净。属于 Milestone 2「主复杂 flow」。
- 非 review、单一执行单元，**不拆分**。

## 目标（docs/complex-tests.md §4.1 P0-1）
同一 turn 内组合：plan dependency + blackboard post + dangerous approve + post-tool pivot
+ 第二次 dangerous deny + final LLM。pivot 必须用 StepHarness 在合法 NeedLlm 边界手动插入。

## 机器行为（已核对 src/agent/machine/default/tools.rs）
- LLM 返回 tool_use → 拆成 auto 批量(一次 NeedTool 批) + approval 队列(逐个 NeedInteraction)。
- auto 批先跑；approve 后为该 call 发单个 NeedTool；deny → 合成 Denied 结果，不发 NeedTool。

## 手动 stepping 序列（#[tokio::test]，handler.fulfill 是 async）
1. user("实现功能 A") → NeedLlm L1
2. resume L1 = tool_use[plan_create, plan_add_task(design), plan_add_task(implement,[design])]
   → NeedTool 批(3) → 逐个 handler.fulfill+resume → NeedLlm L2
3. resume L2 = tool_use[blackboard_post("start processing feature A"), dangerous_write#1]
   → NeedTool[post] → resume → NeedInteraction(dangerous#1) → interaction.fulfill=Approve → resume
   → NeedTool[dangerous#1] → resume(执行, board+1) → NeedLlm L3
4. 在 dangerous#1 结果后、resume L3 前：harness.pivot("先不要改文件,只给方案") → 同 id 重渲染 L3
5. resume L3 = tool_use[blackboard_post("changed strategy after pivot..."), dangerous_write#2]
   → NeedTool[post] → resume → NeedInteraction(dangerous#2) → interaction=Deny → resume
   → 合成 Denied,tool phase drained → NeedLlm L4（捕获其 ChatRequest）
6. resume L4 = text("done") → Done

## 交互后端
ScriptedInteractionHandler::sequence([Approve, Deny(Some(...))])，逐次 fulfill；log 记录 2 次。

## 断言
- committed turns == 1，pending none。
- assert_pivot_after_tool_result(pivot 文本) + role_sequence。
- implement.depends_on == [design]；design=Todo；store.claim("implement",..,v=2)=DependencyBlocked。
- board = ["start processing","apply the risky change"(dangerous#1),"changed strategy after pivot"]，offset 单调。
- assert_tool_executions(DANGEROUS_WRITE,1)。
- assert_interaction_decisions(log,2) + records 顺序 Approve/Deny。
- L4 ChatRequest.messages 含 pivot 文本 User 消息 + Denied ToolResult。

## 文件
新建 tests/agent_complex_flow.rs（#[path=complex_support/mod.rs] mod）。测试名
`complex_turn_combines_plan_blackboard_approval_deny_and_pivot`。

## 验证顺序
fmt --check → clippy --all-targets -D warnings → 指定测试 → cargo test --all --all-targets(<=30min)
→ RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace → git diff --check。

## 完成
TODO.md M2-1 [TODO]->[DONE] + 完成记录；提交 [M2-1]；停止。

## 进度
- [完成] 新建 tests/agent_complex_flow.rs;StepHarness 手动推进 + async handler fulfill;pivot 落
  post-tool→NeedLlm 边界。全部验证通过(fmt/clippy/指定测试 1 passed/全量 all-targets 全绿:lib 423 +
  testkit 131 + 集成 crate,credential-gated ignored/doc/diff --check)。TODO.md M2-1 标 [DONE] 并写完成
  记录。待提交 [M2-1]。
