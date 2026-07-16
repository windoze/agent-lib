# M2-3 — 收齐 `NeedTool` 结果并回灌 `RespondToolResults`

**当前执行 = TODO.md 第一个未完成任务 = M2-3**(M1-1..M1-4、M2-1、M2-2 已 `[DONE]`)。
M2-2(producer)已落地:`PausedForToolCalls` → 发 `NeedTool` batch + park `AwaitingTool`,
scratch `PendingExternalToolBatch` 已被填充但只写未读(drain=本任务)。

M2-3 = **consumer/drain**:每个 fulfilled tool result 逐个 `StepInput::Resume` 回 machine;
batch 未收齐前保持 `AwaitingTool` 非 terminal、不发新 requirement;收齐后按原始 call 顺序
`block_on_session(step_id, RespondToolResults { batch_id, results })`。

## 关键事实(已核对)
- `RequirementResult::Tool(Result<ToolResponse, ToolRuntimeError>)`;tag 名 = "tool"。
- `ExternalToolResult::from_tool_response(&ToolResponse)`(status/content,provider_call_id=response.tool_call_id)
  与 `from_tool_runtime_error(provider_call_id, &ToolRuntimeError)`(error 文本→content+error 字段)。
- external tool failure policy 首版 = `ReturnErrorToRuntime` 固定:Err→from_tool_runtime_error 回灌,不 StopRun。
- scratch `PendingExternalToolBatch{ batch_id, calls, call_to_requirement(未读), requirement_to_call, results }`。
  drain 用 `requirement_to_call`(req→provider)路由、`calls` 排序、`results` 收集、`batch_id` 回灌。
- `block_on_session` 分配新 `NeedExternalSession` + settle `AwaitingSession`/streaming_step(与 interaction 同 pattern)。
- 内部对照:`src/agent/machine/default/tools.rs::resume_tool`(partial 不改 cursor,batch idle 才 advance)。

## 实现步骤(machine.rs)
- [x] `Awaiting` enum 新增 `Tool` 变体;`resume` cursor match 新增 `AwaitingTool { .. } => Ok(Awaiting::Tool)`;
      dispatch `Ok(Awaiting::Tool) => self.resume_tool(resolution)`。
- [x] 新增 `resume_tool(resolution)`:
      - 无 scratch → fail("tool result resumed without a pending tool batch")(mid-turn restore 保护)。
      - 无 in_flight → fail。
      - `requirement_to_call.get(id)` 无 → fail(unknown id / not part of batch)。
      - `results` 已含该 provider_call_id → fail(duplicate resume)。
      - `Tool(Ok(resp))` → from_tool_response,provider_call_id 覆写为 mapping 权威值。
      - `Tool(Err(err))` → from_tool_runtime_error(provider_call_id, err)(ReturnErrorToRuntime)。
      - 其他 family → fail("NeedTool requirement cannot accept a `<tag>` result")。
      - 收集进 scratch.results;len<calls.len → 保持 AwaitingTool,quiescent 空 outcome(不改 cursor)。
      - 收齐 → take scratch,按 calls 原始顺序组 results,`block_on_session(step_id, RespondToolResults{batch_id, results})`。
- [x] 移除未读字段 `call_to_requirement`(struct + pause_for_tool_calls 构造),因 drain 只用 requirement_to_call;
      连带移除 `PendingExternalToolBatch` struct 级 `#[expect(dead_code)]`(剩余字段全被 M2-3 读取)。
- [x] 更新 doc:模块顶部 PausedForToolCalls bullet、struct doc、pause_for_tool_calls doc(M2-3 已落地 drain)。

## 测试(machine/tests.rs)
- [x] helper `tool_resolution(id, ToolResponse)` / `tool_error_resolution(id, ToolRuntimeError)` / `tool_response(id)`。
- [x] `external_tool_results_resume_back_to_session_when_batch_complete`:最后一个 result 后发
      NeedExternalSession(RespondToolResults),batch_id=pause 的、results 顺序=原始 calls 顺序、cursor=AwaitingSession。
- [x] `external_tool_batch_accepts_out_of_order_results`:乱序 resume,results 仍按原始 call 顺序。
- [x] `external_tool_partial_result_keeps_waiting`:仅收一个 → cursor 仍 AwaitingTool、无 requirement、无 session。
- [x] `external_tool_resume_wrong_requirement_fails`:未知 requirement id → Error cursor。
- [x] `external_tool_resume_wrong_family_fails`:Tool 位收到 Interaction result → Error cursor。

## 验证序列 1-6
1. `cargo fmt --all -- --check`
2. 聚焦:`cargo test -p agent-lib external_tool_results`
3. clippy:`cargo clippy --all-targets -- -D warnings`(expect 兑现)
4. `cargo test --all --all-targets`(≤30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` + `git diff --check`

## 状态:实现中

## 状态:已完成(2026-07-17)
machine.rs 落地 `Awaiting::Tool` 路由 + `resume_tool`(收齐/乱序/partial/dup/unknown/wrong-family/return-error-to-runtime);
移除未读 `call_to_requirement` 字段 + struct 级 `#[expect(dead_code)]`;tests 新增 6 test + helpers。
验证 1-6 全过:fmt OK / 聚焦 external_tool 11 passed / clippy 0 / 全量绿(lib 571)/ doc -D warnings 绿 / diff clean。
M2-3 已在 TODO.md 标 [DONE] + 完成记录。PLAN.md 无需改(phase 序列未变,mid-turn scratch 风险仍开放)。待 commit。
