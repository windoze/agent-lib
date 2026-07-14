# 当前任务：M4-3 实现 pivot 后 subagent brief 使用重渲染 request 场景

## 定位
- `TODO.md` 第一个未完成任务 = **M4-3**（行 856，首个 `[TODO]`）。前置 M4-1/M4-2 已 `[DONE]`（HEAD=d5c94ac）。
- 工作树干净。新增 P1 复杂 mock 测试，落到 `tests/agent_complex_subagent.rs`（已有 M3-1 subagent 测试）。
- 需对 testkit 做一处小增强：`ScriptedSubagentSpawner` 捕获传入 `spawn()` 的 brief。

## 关键约束（来自源码勘查）
- `DefaultAgentMachine` 只发 NeedLlm/NeedTool/NeedInteraction，**从不发 NeedSubagent**。
- `StepHarness::pivot` 的“重渲染 outstanding NeedLlm request”只在 `DefaultAgentMachine` 上成立。
- 因此：coordinator turn 用 `DefaultAgentMachine`+`StepHarness`（拿到真实 pivot 重渲染）；
  subagent 通过 `spawn_reviewer` 工具触发，由测试在该 NeedTool 边界用真实
  `DrivingSubagentHandler`+`ScriptedSubagentSpawner` 驱动 reviewer child，summary 作为工具结果回灌。

## 设计（单一连贯 flow，离线确定性，无 sleep/网络）
coordinator = `complex_agent_machine`（`DefaultAgentMachine`），`StepHarness` 手动步进：
1. `user(OLD_GOAL)`（旧目标：直接用 dangerous_write 改文件）→ NeedLlm。
2. resume：coordinator step1 = `blackboard_post`（宣布旧目标意图，auto）→ 单 NeedTool → 用
   coordinator ComplexToolHandler fulfill → tool result → NeedLlm（合法 pivot 边界）。
3. `pivot(PIVOT_TEXT)`（“switch to a reviewer subagent: review only, do not edit files directly”）；
   断言重渲染的 NeedLlm id 不变、request messages 含 PIVOT_TEXT。
4. resume：coordinator step2 = `spawn_reviewer { brief: REVIEWER_BRIEF }`（含 pivot 意图，不含旧目标 dangerous_write）
   → 单 NeedTool。测试提取 brief 字符串；构建 reviewer child 并用真实 DrivingSubagentHandler 驱动：
   - reviewer_ids = ids.fork("reviewer")；reviewer child = `complex_agent_machine(&reviewer_ids)`，
     headless 子 scope（llm+tool，共享 coordinator store），LLM 脚本 [safe_read, final_text]，
     opening = user_input(reviewer_ids, brief_string)。
   - spawner = ScriptedSubagentSpawner.child(child).summary(...)；handler = into_handler(2)。
   - ScopePop::new(&empty_scope, None) 作为 outer；handler.fulfill(spec_ref, brief, None, outer, ctx)。
   - Subagent(Ok(out)) → tool_ok(spawn_call.id, out.summary) 回灌。
   - resume(spawn_req.id, tool_result) → NeedLlm。
5. resume：coordinator step3 = final_text → Done。

## 断言
- 单一 committed turn，无 pending。
- `assert_pivot_after_tool_result(conv, PIVOT_TEXT)`；重渲染 request 含 PIVOT_TEXT。
- coordinator 侧 `assert_tool_executions(coord_handler, DANGEROUS_WRITE, 0)`（旧目标危险写未执行）。
- spawn_reviewer 恰执行一次；提取的 brief 含 pivot 意图子串、且不含旧目标 marker("dangerous_write")。
- spawner 捕获恰一个 brief，其文本含 pivot 意图子串（证明 NeedSubagent→spawn 透传）。
- reviewer LLM request[0] messages 含 pivot 意图（brief 已折进 child opening 并进入其首个 LLM 请求）。
- reviewer 侧 `assert_tool_executions(reviewer_handler, DANGEROUS_WRITE, 0)`；safe_read 执行一次。
- spawner hook 计数 ids/spawn/summarize 各 1；trace subagent_count(1)。

## 改动文件
- `crates/agent-testkit/src/subagent.rs`：`ScriptedSubagentSpawner` 增 `briefs: Mutex<Vec<Interaction>>`
  + `spawn` 中记录 + `pub fn briefs()`。
- `tests/complex_support/tools.rs`：新增 `SPAWN_REVIEWER` const + 加入 `tool_declarations()`
  （host 侧作为 subagent spawn 工具，不进 store dispatch）。
- `tests/complex_support/mod.rs`：re-export `SPAWN_REVIEWER`（如需要）。
- `tests/agent_complex_subagent.rs`：新增 `complex_pivot_then_subagent_uses_rerendered_brief`。

## 验证命令
- cargo fmt --all -- --check
- cargo clippy --all-targets -- -D warnings
- cargo test --test agent_complex_subagent complex_pivot_then_subagent_uses_rerendered_brief
- cargo test --test agent_complex_subagent
- cargo test --all --all-targets（<30min）
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- git diff --check

## 完成
- 全绿后：TODO.md M4-3 [TODO]->[DONE] + 完成记录；提交 `[M4-3] ...`；停止。

## 进度
- (完成) M4-3 实现并全部验证通过；TODO.md M4-3 标 [DONE] 并写入完成记录；准备提交并停止。
- 关键决策：DefaultAgentMachine 不发 NeedSubagent 且只有它支持 pivot 重渲染，故 subagent 走 tool 化 spawn_reviewer，在 NeedTool 边界用真实 DrivingSubagentHandler+ScriptedSubagentSpawner 驱动 reviewer child；testkit 增 briefs() 捕获透传 brief。
