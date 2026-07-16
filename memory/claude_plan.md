# M3-1 实现 `PausedForSubagent` -> `NeedSubagent` -> `RespondSubagent`

**当前执行 = TODO.md 第一个未完成任务 = M3-1**（M1-*、M2-* 已 `[DONE]`）。

## 目标
让 `ExternalAgentMachine` 把 runtime 的 `PausedForSubagent` 决策点折成一个标准
`NeedSubagent` requirement，由 host 的 subagent 机制（`DrivingSubagentHandler`）驱动子 agent，
收到 `RequirementResult::Subagent(Ok/Err)` 后回灌 `RespondSubagent` 给 runtime（Err 首版转 error cursor）。

## 锚点
- `src/agent/external/state.rs`：`ExternalAgentCursor` 需新增 `AwaitingSubagent { requirement, request_id }`。
- `src/agent/external/machine.rs`：
  - `fold_session_result` 的 `PausedForSubagent` 分支当前直接 fail（M3 占位），改为 `pause_for_subagent`。
  - 新增 `Awaiting::Subagent`、`pause_for_subagent`、`resume_subagent`；更新 `resume`、`cursor_label`、
    `initial_loop_cursor`、模块文档。
- DTO 已就绪（M1）：`ExternalSubagentRequest{request_id,spec_ref,brief,result_schema,raw}`、
  `ExternalSubagentOutput`、`ExternalSessionInput::RespondSubagent`、`ExternalSessionResult::PausedForSubagent`。
- `NeedSubagent { spec_ref, brief, result_schema }`，result = `Result<SubagentOutput, AgentError>`，
  `needs_outer: true`（driver 已有 serial outer routing，无需改 driver）。

## 实现步骤
1. state.rs：AwaitingSubagent 变体 + `requirement()` / `has_outstanding_requirement()` 覆盖；
   导入 `ExternalSubagentRequestId`。补 state 单测（cursor 断言）。
2. machine.rs：
   - `Awaiting::Subagent { requirement, request_id }`。
   - `pause_for_subagent`：record session、alloc `RequirementKindTag::Subagent` id、
     emit `NeedSubagent{spec_ref,brief,result_schema}`、settle `AwaitingSubagent`
     + `LoopCursor::streaming_step`。无 in_flight -> fail_with（带 notifications）。
   - `resume_subagent`：id 校验；`Subagent(Ok(out))` -> `RespondSubagent`；
     `Subagent(Err)` -> error cursor；wrong family -> error cursor。
   - `resume` 读 `AwaitingSubagent`；`cursor_label`、`initial_loop_cursor` 加 awaiting_subagent。
   - 更新模块 doc（M3 覆盖范围）。
3. 机器单测（machine/tests.rs）：
   - `external_subagent_pause_emits_need_subagent`
   - `external_subagent_result_responds_to_session`
   - `external_subagent_wrong_family_fails`
   - 追加：wrong requirement id、Err -> error cursor（class-wide）。
4. drive 测试（tests/agent_external_subagent.rs，匹配 `driving_subagent` 过滤名）+
   testkit fixture 助手（`ExternalAgentFixture::subagent_pause` / `subagent_request`）：
   - `external_agent_driving_subagent_fulfills_child`
   - `external_agent_driving_subagent_pops_child_interaction_to_outer`
5. 更新 TODO.md 标 [DONE] + 完成记录。

## 验证序列
1. `cargo fmt --all -- --check`
2. 聚焦：`cargo test -p agent-lib external_subagent` + `cargo test -p agent-lib driving_subagent`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`（<=30min）
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 状态：完成

- state.rs：新增 `AwaitingSubagent` cursor + 接入访问器 + round-trip 测试。✅
- machine.rs：`Awaiting::Subagent` + `pause_for_subagent` + `resume_subagent` + resume/fold 路由 + docs。✅
- machine/tests.rs：新增 5 条 subagent 单测(pause emits / result responds / wrong family / wrong id / error cursor)。✅
- testkit external.rs：新增 `subagent_request` / `subagent_pause` fixture 助手。✅
- tests/agent_external_subagent.rs：新增 2 条 drive 测试(`driving_subagent` fulfill child / pop child interaction to outer)。✅
- 验证序列 1-6 全过:fmt clean、clippy 0 warning、全套件 38 组 0 failed、doc 通过、`git diff --check` clean。✅
- TODO.md M3-1 标 `[DONE]` + 完成记录。✅
- driver 无改动(NeedSubagent 复用既有 `scope.subagent()` + ScopePop routing)。
