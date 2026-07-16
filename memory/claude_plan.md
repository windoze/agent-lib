# M3-3 支持 external runtime 的 `spawn_agent` tool bridge 特判

**当前执行 = TODO.md 第一个未完成任务 = M3-3**（M1-*、M2-*、M3-1、M3-2 已 `[DONE]`）。

## 目标（设计 §8.3）
external runtime 通过普通 tool call 暴露 `spawn_agent` 时,machine 必须在 tool phase 把
`ExternalToolCall.name == "spawn_agent"` 特判成 `NeedSubagent`(scope-deepening),而不是普通
`NeedTool`。子 agent 结果最终折成 `ExternalToolResult(status=Ok, content=summary)` 合并进同一
`RespondToolResults` batch。支持同一 batch 混合普通 tool + spawn_agent:普通 tool 并发 fulfill,
subagent 由 driver serial outer routing,machine 收齐两类结果后按原始 call 顺序回灌一次
`RespondToolResults`。

## 关键设计决定
- **复用 `crate::agent::collab::{SpawnAgentRequest, SPAWN_AGENT}`**(§8.1 host 用
  `bridge_tool_declarations()` 声明 spawn_agent;§8.3 明确用 `SpawnAgentRequest::parse`)。
  实际 input contract = 已声明的 `{ spec, brief, result_schema }`(task 的 spec_ref/prompt 只是"例如")。
- **无效 spawn_agent input 策略 = runtime-visible error result**(return-error-to-runtime,§8.4)。
  malformed spawn_agent(缺 spec/brief、bad agent id、result_schema 非 object)→ 直接预置一个
  `ExternalToolResult{status:Error}` 进 batch,turn 继续。理由:machine 在此充当 tool 输入校验,
  与普通 tool handler 返回 error 一致;不因单个 bridge call 畸形而杀整个 turn。
- **subagent drive 失败(`Subagent(Err)`)策略 = error cursor**,与独立 `resume_subagent` 对称
  (host orchestration 失败,turn-stopping)。
- 混合 batch 仍 park 在 `AwaitingTool` cursor:为每个 call(含 spawn_agent bridge)mint 一个
  `ToolCallId` 放进 `ToolWaitRequirements`,spawn_agent 绑定的 requirement 是 `NeedSubagent`。
  这样整批在一个 cursor 下,`pending_requirement_ids()` 覆盖全部 requirement,recovery 一致。
- 全 spawn_agent 且全部 parse 失败 → requirements 为空 → 不 park AwaitingTool,直接组装(全部
  预置的 error result)回灌 `RespondToolResults`。空 batch(calls 为空)保持 fail。

## 实现步骤（machine.rs）
1. imports 增加 `collab::{SpawnAgentRequest, SPAWN_AGENT}`、`ToolRuntimeError`、`ToolStatus`。
2. `PendingExternalToolBatch`:`requirement_to_call: BTreeMap<RequirementId,String>` 换成
   `pending: BTreeMap<RequirementId, PendingBridgeCall>`(含 provider_call_id + kind)。新增
   `PendingBridgeCall` / `ExternalBridgeCallKind{Tool,Subagent}`。
3. `pause_for_tool_calls`:逐 call 分流。spawn_agent → parse:Ok 走 `into_requirement_kind(step_id)`
   emit NeedSubagent + pending(Subagent);Err 预置 error result。普通 → NeedTool + pending(Tool)。
   requirements 非空 → park AwaitingTool(ids 含全部 call_id);空且有 results → 立即
   `respond_with_tool_batch`;空 calls → fail。
4. 新 helper `respond_with_tool_batch(step_id, batch_id, calls, results)`:按 call 顺序组装并
   `block_on_session(RespondToolResults)`。`resume_tool` 完成分支也改用它。
5. `resume_tool`:按 `pending` 找到 `PendingBridgeCall`;按 kind 校验 result family:
   Tool→Tool(Ok/Err) 同现状;Subagent→Subagent(Ok)=>ExternalToolResult(Ok,summary)、
   Subagent(Err)=>`fail` error cursor;family 不匹配 => fail。收齐后 `respond_with_tool_batch`。
6. 更新模块 doc + 各 fn rustdoc。

## 测试（src/agent/external/machine/tests.rs）
- helper：`spawn_agent_call(provider_call_id, spec, brief)`、`spawn_agent_call_raw(input)`。
- `external_spawn_agent_tool_call_emits_need_subagent`:单个 spawn_agent → 一个 NeedSubagent,
  AwaitingTool cursor,batch id,turn 保持 open。
- `external_spawn_agent_result_bridges_summary_into_respond_tool_results`:subagent Ok →
  RespondToolResults 里 status Ok + summary content。
- `external_mixed_tool_and_spawn_agent_batch_returns_one_respond_tool_results`:tool+spawn 混合,
  两个 requirement(Tool+Subagent),各自 resume 后一次 RespondToolResults,顺序 = 原始 call 顺序。
- `external_spawn_agent_invalid_input_returns_runtime_error_result`:malformed spawn_agent →
  runtime-visible error result(单 spawn_agent 全 parse 失败 → 立即回灌;断言 status Error + turn 存活)。
- `external_spawn_agent_subagent_failure_settles_error`:Subagent(Err) → error cursor,0 RespondToolResults。
- `external_spawn_agent_bridge_wrong_family_fails`:给 subagent-bridge requirement 喂 Tool 结果 → error。

## 测试（src/agent/drive.rs tests）
- `mixed_tool_and_subagent_batch_routes_subagent_serially`:`fulfill_batch([NeedTool,NeedSubagent])`
  经 scope(tool+subagent handler)→ 两个 resolution 家族正确;subagent 只能经 serial(needs_outer)
  路径完成(若误入 concurrent local 会 panic),从而证明 serial outer routing。

## 验证序列
1. `cargo fmt --all`
2. 聚焦:`cargo test -p agent-lib external_spawn_agent` / `external_mixed_tool` /
   `external_tool` / `external_subagent` / `mixed_tool_and_subagent`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`（<=30min）
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 状态：已完成（M3-3 [DONE]）

- 实现全部落地(machine.rs pause/resume/respond helper、state.rs cursor doc、模块 doc)。
- machine 单测 7 条 + drive 单测 1 条全绿。
- 验证序列 1-6 全过:fmt clean、clippy `-D warnings` 0 warning、全套件 38 组 0 failed、
  doc(`-D warnings`)通过、`git diff --check` clean。
- TODO.md 已把 M3-3 标 `[DONE]` 并补完成记录。下一个任务 = M3-4(review),本轮不启动。