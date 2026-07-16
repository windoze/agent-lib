# M3-2 完善 runtime permission/question/choice 到 `NeedInteraction` 的映射

**当前执行 = TODO.md 第一个未完成任务 = M3-2**（M1-*、M2-*、M3-1 已 `[DONE]`）。

## 目标
补齐 `ExternalAgentMachine` 对 `PausedForInteraction` 的 resume 验证：resume 回灌
`RespondInteraction` 前必须调用 `Interaction::accepts_response`，wrong response family /
choice 越界 / permission action_id 不匹配都进入 error cursor，绝不把无效 response 传给 runtime。
并为 permission/question/choice 补 machine 单测，更新 reference.rs 的 `ApprovalInteractionHandler`
文档与测试。

## 根因 / 现状
- `resume_interaction`（machine.rs:955）当前直接把 `RequirementResult::Interaction(response)`
  塞进 `RespondInteraction`，**没有**调用 `accepts_response`。这是本任务要补的 gap。
- `AwaitingInteraction` cursor（state.rs:45）只存 `requirement` + `pending_action`(String)，
  没有保留原始 `Interaction`，因此 resume 无法验证。→ 需要把 `Interaction` 存进 cursor。

## 实现步骤
1. **state.rs**：`AwaitingInteraction` 增加 `interaction: Interaction` 字段（可序列化的
   resumable fact）。导入 `Interaction`。更新 rustdoc。更新 cursor round-trip 测试 & requirement()
   访问器 match arm（`AwaitingInteraction { requirement, .. }` 已用 `..`，无需改）。
2. **machine.rs**：
   - `Awaiting::Interaction` 枚举增加 `interaction: Interaction`。
   - `resume` 读取 cursor 时 clone `interaction` 传入。
   - `pause_for_interaction`：把 `request` clone 一份存进 cursor（另一份 emit 到 NeedInteraction）。
   - `resume_interaction`：提取 `response` 后调用 `interaction.accepts_response(&response)`；
     Err -> `self.fail(...)`（error cursor，稳定诊断，不泄漏 transcript）；Ok -> 原路 block_on_session。
   - 更新模块 doc（M3-2 覆盖：resume 前校验 response）。
3. **machine/tests.rs**：新增 helper（permission_paused_result / choice_paused_result +
   permission/choice resolution），新增单测（class-wide 覆盖所有 family + 所有 error 类型）：
   - `external_permission_interaction_relays_approve/deny/cancel`
   - `external_question_interaction_relays_answer`
   - `external_choice_interaction_relays_selected_index`
   - `interaction_result_rejected_on_action_mismatch_settles_error`（permission 错 action_id）
   - `interaction_result_rejected_on_choice_out_of_range_settles_error`
   - `interaction_result_rejected_on_family_mismatch_settles_error`
   - 断言 error cursor 且 **没有** RespondInteraction requirement 发给 runtime。
4. **reference.rs**：为 `ApprovalInteractionHandler` 补 `#[cfg(test)] mod tests`：
   - `approval_interaction_handler_approves_permission`
   - `approval_interaction_handler_denies_permission`
   - `approval_interaction_handler_maps_decision_to_permission_cancel/timeout`
   - `approval_interaction_handler_answers_question_and_choice_trivially`
   并按需补充/明确文档（reference handler 仅用于测试/默认 headless）。
5. 更新 TODO.md 标 [DONE] + 完成记录。

## 验证序列
1. `cargo fmt --all -- --check`
2. 聚焦：`external_permission_interaction` / `interaction_result_rejected` / `approval_interaction_handler`
   + `external_pause_then_respond_then_complete_commits_the_turn`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`（<=30min）
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 状态：完成

- state.rs：AwaitingInteraction 新增 interaction 字段 + round-trip 测试。✅
- machine.rs：resume_interaction 前调用 accepts_response，无效 response -> error cursor（不回灌 runtime）。✅
- machine/tests.rs：新增 permission/question/choice relay + 3 类 rejection + turn-recoverable 单测。✅
- reference.rs：ApprovalInteractionHandler 文档 + 5 条 handler 单测。✅
- 验证序列 1-6 全过；TODO.md M3-2 标 [DONE] + 完成记录。✅
