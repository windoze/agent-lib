# 当前任务：M3-1 subagent + parent approval pop + shared plan/blackboard 场景

## 定位
- `TODO.md` 第一个未完成任务 = **M3-1**（行 503，首个 `[TODO]`）。前置 M2-R 已 `[DONE]`。
- HEAD=6396734（[M2-R]），工作树干净。属于 Milestone 3。
- 非 review 任务，**不拆分**。产出：新建 `tests/agent_complex_subagent.rs`。

## 设计（复用现有基础设施）
参考 `tests/agent_effect_e2e.rs::attended_parent_serves_headless_child_via_pop`，
child 换成跑 plan/blackboard 工具的真实 `DefaultAgentMachine`（`complex_agent_machine`）。

- shared `Arc<MockPlanBlackboardStore>`，直接预置 plan：
  - create → design(v1) → review depends[design](v2) → implement depends[review](v3)
  - claim(design,seed,3)→v4；update(design,seed,Completed,4)→v5。V_seed=5。
- child = `complex_agent_machine`，headless scope：llm(charging scripted)+tool(complex_tool_handler(store))，无 interaction → 审批 pop 到 parent。
- child LLM 脚本（4 步）：
  1. tool_use: plan_claim_first_available(worker,ev=5)+blackboard_post(child,"review started") → claim review, v6
  2. tool_use: dangerous_write("apply review fix") → 审批 pop→approve→执行, post board
  3. tool_use: plan_update(review,worker,Completed,ev=6)+blackboard_post(child,"review done") → v7
  4. final text
- parent = ScriptMachine emit 1×NeedSubagent；scope = parent_scope_with_subagent(handler).attended(approve_all)。
- 驱动前 parent 直接 store.post("parent","delegating…") 展示共享 board + sender 区分。
- charging LLM wrapper（照抄 e2e）把 child usage 计入 shared ledger。总 tokens=8+6+9+5=28。

## 断言
- spawn_calls==1, summarize_calls==1, ids_calls==1。
- parent_interaction_log.len()==1（child 审批 pop 到 parent, 决定 Approve）。
- review: owner=worker, status=Completed；implement: 无 owner + Todo（未被 claim）。
- design Completed；depends_on 不变。
- board 4 条按序 [parent, review started(child), apply review fix(dangerous_write), review done(child)]，sender 可区分。
- dangerous_write 执行恰好 1 次。
- child token 计入 parent ctx.budget()==28。
- trace: subagent_count==1；child interaction requirement resolved_at_scope==1+Resumed；parent NeedSubagent resumed（parent_log.resume_tags()==[Subagent]）。

## 验证顺序
fmt --check → clippy --all-targets -D warnings → cargo test --test agent_complex_subagent → full suite → RUSTDOCFLAGS=-D warnings cargo doc → git diff --check。

## 完成
TODO.md M3-1 [TODO]→[DONE] + 写完成记录；提交 [M3-1]；停止。

## 进度
- [完成] tests/agent_complex_subagent.rs 写好，全部验证门通过（fmt/clippy/focused/full all-targets/doc -D warnings/diff --check）。TODO.md M3-1 标 [DONE] 并写完成记录。待提交 [M3-1]。
