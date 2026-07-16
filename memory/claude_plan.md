# M2-4 Review：external tool phase 正确性检查

**当前执行 = TODO.md 第一个未完成任务 = M2-4**(M1-1..M1-4、M2-1..M2-3 已 `[DONE]`)。
这是一个 Review/sign-off 任务:核对 M2 落地的 external tool phase 是否把 external tool
result 错误写进 Conversation、是否绕过 existing ToolHandler / pop routing,并记录与
DefaultAgentMachine tool phase 的差异。

## 审查清单(已逐条核对源码 + 测试)
1. requirement id 路由按 `RequirementId`,支持 out-of-order resume
   - `resume_tool` 用 `batch.requirement_to_call.get(&resolution.id)` 路由(非按 emission 顺序)。
   - 测试:`external_tool_batch_accepts_out_of_order_results`。OK
2. batch 未完成时不 terminal
   - `results.len() < calls.len()` -> `StepOutcome::new([], [], true)`,cursor 不变(仍 AwaitingTool)。
   - 测试:`external_tool_partial_result_keeps_waiting`。OK
3. batch 完成后只发 `NeedExternalSession(RespondToolResults)`
   - 收齐后按 `batch.calls` 原始顺序组 results -> `block_on_session(RespondToolResults)`。
   - 从不写 Conversation(与 Default 的 append_tool_response 相反)。
   - 测试:`external_tool_results_resume_back_to_session_when_batch_complete`。OK
4. tool execution failure 首版策略明确,不会 panic
   - 固定 return-error-to-runtime:`Tool(Err)` -> `from_tool_runtime_error`,不 StopRun、不 panic。
   - 测试:`external_tool_batch_returns_runtime_errors_to_the_runtime`。OK
5. no ids / wrong family / wrong id -> error cursor
   - no ids(NoToolExecutionIds)-> `external_tool_pause_without_tool_ids_fails`。
   - wrong family(Interaction result 进 Tool 位)-> `external_tool_resume_wrong_family_fails`。
   - wrong id(不在 batch 的 requirement)-> `external_tool_resume_wrong_requirement_fails`。
   - 额外:duplicate resume、empty batch(EmptyToolWait)也进 error cursor,不 deadlock/panic。OK
6. `ExternalAgentState` serde 不含 live tool registry / executor / handler
   - state 字段仅 spec/conversation/session/cursor/active_tools(仅声明)/artifacts/cleanup_required。
   - 自定义 serde 负测断言 forbidden keys 含 `tool_registry`(state.rs 测试)。OK
   - 所有 volatile 关联(tool_ids / requirement_ids / in_flight / pending_tool_batch)都在 machine,非 state。OK
7. drain trace 中 tool requirements 正常记录,无需改 driver
   - external machine emit 的 `NeedTool` 与 DefaultAgentMachine 完全同形,复用同一 driver 路径。
   - drive.rs 单测覆盖 NeedTool batch out-of-order + trace 记录
     (`drain_resolves_a_concurrent_batch_out_of_order`、
     `drain_records_resolved_at_scope_for_local_and_popped_requirements`)。无需改 driver。OK

## external tool phase vs DefaultAgentMachine tool phase 差异(保留原因)
- 结果去向:external 从不写 Conversation,收齐后 `RespondToolResults` 回灌 runtime;
  Default `append_tool_response` 进 Conversation。原因:external runtime 自持 transcript,host 只做工具桥。
- failure policy:external 固定 `ReturnErrorToRuntime`(无 StopRun 选项);Default 可配置
  `ReturnErrorToModel` / `StopRun`。原因:external runtime 自行决定如何应对失败调用,首版不暴露 StopRun。
- per-result 通知:external tool resume 不发 `ToolCallFinished`(runtime 活动经 `ExternalObservedEvent`
  -> `Notification::ExternalAgent` 汇报);Default 每个 result 发 `ToolCallFinished`。原因:external 的可观测
  事件模型是 observation-based,不是 host tool-call-based。
- scratch 持久化:external 的 per-call 关联在非序列化 `PendingExternalToolBatch`,cursor 只存
  可恢复寻址(ToolCallId->RequirementId);Default 的 `ToolPhase` 同样非序列化(phase marker)。mid-turn
  restore 场景两者都留待后续(PLAN.md "恢复 mid-turn scratch" 风险)。

## 结论
无需代码修改:实现与 review 清单一致,无 spec 偏差 / 无 workaround。纯 sign-off。
仅文档改动(TODO.md 标 [DONE] + 完成记录、memory 计划),因此可复用上次全量绿结果,但本任务仍按
验证条件跑聚焦 external_tool / drain + fmt/clippy/doc/diff。

## 验证序列
1. `cargo fmt --all -- --check`
2. 聚焦:`cargo test -p agent-lib external_tool` + `cargo test -p agent-lib drain`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`(<=30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 状态:已完成(2026-07-17)
验证 1-6 全过:fmt OK / external_tool 11 passed / drain 7 passed / clippy 0 / 全量绿 / doc -D warnings 绿 / diff clean。
M2-4 已在 TODO.md 标 [DONE] + 完成记录。PLAN.md 无需改(phase 序列未变)。纯 sign-off,无代码改动。待 commit。
