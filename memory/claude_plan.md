# Claude 执行计划

## 当前任务:M3-3 实现两段式交互(Paused → NeedInteraction → RespondInteraction)

**前置依赖**:M3-2(DONE)。

### 目标(TODO M3-3)
扩展 `ExternalAgentMachine`,覆盖 external session 的两段式交互:
- `Resume(ExternalSession(PausedForInteraction { session, action_id, request, .. }))`
  → 记录 session facts、保存 `pending_action`(action_id)、cursor → `AwaitingInteraction`、
    emit `NeedInteraction { request }`。
- `Resume(Interaction(response))`(cursor 为 `AwaitingInteraction`)
  → emit `NeedExternalSession { input: RespondInteraction { action_id: pending_action, response } }`,
    cursor 回到 `AwaitingSession`。
- 支持一个 turn 内多次 Paused↔Respond 循环,直到 Completed/Failed。
- action_id 对齐:`RespondInteraction.action_id` 必须与触发它的 `PausedForInteraction` 一致。

### 关键设计决策:DTO 补 `action_id`(非 workaround)
`ExternalSessionResult::PausedForInteraction` 当前只有 `{ session, request, observations }`,
没有 machine 回喂 `RespondInteraction` 所需的 action_id。设计文档 §6.2 的 action_id 来自
`Permission(PermissionRequest { action_id })`,但 `InteractionKind::Permission` 属 M4-1,当前不可用。
TODO M3-3 明确要求从 `PausedForInteraction { request, .. }` 里「保存 pending_action(action_id)」,
因此正确做法是给 `PausedForInteraction` 补一个显式 `action_id: String` 字段(runtime 暂停动作的句柄,
machine 存为 `pending_action` 并在 `RespondInteraction` 里原样回喂)。这是补全 effect 契约,不是绕过。
同步更新 docs/external-agent.md §5.2。

### 实施步骤
1. `src/agent/external/mod.rs`:`PausedForInteraction` 增加 `action_id: String` 字段(+ 文档 + 更新单测)。
2. `src/agent/external/machine.rs`:
   - `InFlight` 增加 `step_id: StepId`。
   - `resume` 按 cursor 分派:AwaitingSession→resume_session,AwaitingInteraction→resume_interaction。
   - 新增 `pause_for_interaction` / `resume_interaction`。
   - `fold_session_result` 的 PausedForInteraction 分支改为调用 pause_for_interaction。
   - 更新模块文档。
3. `src/agent/external/machine/tests.rs`:新增单测(pause→NeedInteraction、respond→RespondInteraction 对齐、
   AwaitingInteraction 收到非 Interaction 结果→Error)。
4. `crates/agent-testkit/src/external.rs`:`permission_pause` 补 `action_id`。
5. `tests/agent_external_interaction.rs`(新文件):pause_resume + pop_to_outer。
6. `docs/external-agent.md` §5.2 补 `action_id`。

### 验证门
1. cargo fmt --all
2. cargo clippy --all-targets -- -D warnings
3. cargo test external_agent_pause
4. cargo test --all --all-targets(≤30min)
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

### 进度
- (进行中)实现与测试。
- (完成)DTO 补 `action_id`、machine 两段式交互、5 单测 + 2 集成测试、testkit fixture、design doc。
- (完成)验证门全绿:fmt 无差异、clippy 0 告警、`cargo test external_agent_pause` 2 passed、
  `cargo test --all --all-targets` 663 passed/0 failed、`cargo doc -D warnings` 0 告警。
- (完成)TODO.md 标记 M3-3 `[DONE]` 并补完成记录。
- 待办:提交(带 Co-authored-by trailer)后停止。M3-4(cancel/abandon 清理与挂载)为下一次调用。
