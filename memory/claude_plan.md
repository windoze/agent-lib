# 执行计划 — M2-4 抽出 tool step:`NeedTool` 与 `NeedInteraction`

## 选中的任务
`TODO.md` 第一个未完成任务是 **M2-4**(M2-1/2/3 已 `[DONE]`)。无需拆分,单次交付。

## 目标(迁移文档 §2/§3/§4)
把 legacy `DefaultAgentLoop` 的 tool 编排(`execute_prepared_tool` / `process_next_ready_tools` /
`resolve_pending_approval` / `wait_for_approval`)从"内部 await"改成纯 sans-io:`step` 吐
`NeedTool` / `NeedInteraction`,等 `Resume(Tool/Interaction)` 回灌。text→tool→llm→...→text 多轮
在纯机器里打通。

## 关键设计决策(已定稿)
- **审批策略仍在机器内**:机器持 `Arc<dyn ToolApprovalPolicy>`(纯同步决策,非 IO)。构造时
  用 builder `with_tool_execution_ids` / `with_approval_policy`(默认 `NoToolExecutionIds` /
  `NoApprovalPolicy`),`new(state, mode, requirement_ids)` 保持不变。
- **cursor 迁移约束**(state/cursor.rs `can_transition_to`):禁止 AwaitingTool→AwaitingTool、
  AwaitingApproval→AwaitingApproval、AwaitingApproval→StreamingStep。定稿模型
  **"auto 批先一次吐,随后逐个处理 approval;每个 approve 直接吐单个 NeedTool"**:
  - begin_tool_phase 把 slots 分成 `auto_pending: Vec`(AutoApprove)与
    `approval_pending: VecDeque`(RequireApproval)。
  - advance:① 若 auto_pending 非空 → 一次吐整批 NeedTool → AwaitingTool(StreamingStep→AwaitingTool
    或 AwaitingTool→…不会发生,见下);② 否则 pop 一个 approval → NeedInteraction → AwaitingApproval;
    ③ 都空 → finish_tool_phase。
  - 因 auto 全在①一次吐尽,后续 advance 只会遇到 approval 或 finish,**永不出现 AwaitingTool→AwaitingTool**。
  - approve:AwaitingApproval→AwaitingTool([call],{call:req}) 直接吐单个 NeedTool(合法)。
  - deny/timeout/cancel:合成 denied result 追加 → **restore bounce** AwaitingApproval→AwaitingTool([call],None)
    → advance(→下一 approval 或 finish)。mirror 旧 loop `restore_awaiting_tool_cursor`。
  - finish 恒从 AwaitingTool 进入:emit StepBoundary(tool step 的 head,mirror
    `apply_pivots_at_pending_step_boundary`)→ 查 max_steps → 起下一 LLM(AwaitingTool→StreamingStep)。
- **decision B**:一次 step 推进到静止,auto 批一次吐尽。max_parallel 交给 driver(机器不限流)。
- **self-heal**:`Resume(Tool(Err))` 按 `ToolFailurePolicy`:ReturnErrorToModel→
  `e.to_tool_response(slot.provider_call_id)` 追加;StopRun→cursor Error。
- **deny/timeout/cancel**:`approval_response_for_decision` 提为 `approval.rs` 的 `pub(crate)`,
  legacy default.rs 改调它;机器合成 `ToolResponse` 追加走同一 append 路径。
- **step 限制**:`InFlight.steps_started` 从 begin_user_turn 起为 1,`finish_tool_phase` 起下一 LLM
  前查 `>= max_steps` → fail(仍先 emit StepBoundary)。
- **顺序无关**:一批 NeedTool 各自 RequirementId,`running: BTreeMap<RequirementId, ToolSlot>`
  路由;乱序 `Resume(Tool)` 一致;整批 running 清空后才 advance。tool_call_id 用 slot 存的 provider id 追加。

## 模块化
- `git mv src/agent/machine/default.rs → src/agent/machine/default/mod.rs`(machine 结构 + LLM step + dispatch)。
- 新 `src/agent/machine/default/tools.rs`:`InFlight`/`ToolPhase`/`ToolSlot` + tool 编排 impl。
- 测试拆:`default/tests/mod.rs`(fixtures + text-turn 既有测试 + `mod tools;`)+
  `default/tests/tools.rs`(tool/审批,`use super::*`)。
- `approval.rs`:新增 `pub(crate) fn approval_response_for_decision`;legacy 改调。

## 机器字段(in_flight 为 ephemeral,承接 M2-3 `pending_assistant_message_id` 的非序列化边界)
- `requirement_ids: Arc<dyn RequirementIds>`、`tool_ids: Arc<dyn ToolExecutionIds>`、
  `approval_policy: Arc<dyn ToolApprovalPolicy>`。
- `in_flight: Option<InFlight { assistant_message_id: MessageId, steps_started: u32, tools: Option<ToolPhase> }>`。
- `ToolPhase { step_id, auto_pending: Vec<ToolSlot>, approval_pending: VecDeque<ToolSlot>,
  running: BTreeMap<RequirementId, ToolSlot>, awaiting_approval: Option<(RequirementId, ToolSlot)> }`。
- `ToolSlot { provider_call_id: String, call_id: ToolCallId, result_message_id: MessageId,
  call: ToolCall, approval: ApprovalRequirement }`。

## step 分派
- External(UserMessage) → begin_user_turn(init in_flight,steps_started=1)。
- Resume:按 cursor 路由:StreamingStep→resume_llm;AwaitingTool→resume_tool;
  AwaitingApproval→resume_approval;其余→fail。
- fold_llm_response 的 RequiresToolCallMappings 分支 → begin_tool_phase(不再 fail)。

## 验证
`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → `cargo test --lib agent::machine`
→ `cargo test --all --all-targets`(≤30min)→ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
→ `git diff --check`。

## 聚焦测试(纯同步)
single tool、parallel tool(乱序回灌)、tool failure self-heal、approval approve/deny/timeout、
multi-round tool→llm→tool→text、step-limit、mismatched requirement / wrong result kind。
