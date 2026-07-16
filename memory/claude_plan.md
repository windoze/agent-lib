# Claude 执行计划

## 当前任务:M3-2 实现 `ExternalAgentMachine` 基本推进

**前置依赖**:M3-1(DONE)。

### 目标(TODO M3-2)
实现 `ExternalAgentMachine`(实现 `AgentMachine` trait,纯函数 `step`),覆盖基本推进:
- `step(External(UserMessage))`:begin_turn,依据是否已有 session 选择
  `NeedExternalSession { Start { prompt } }` 或 `Continue { message }`,park 到 `AwaitingSession`。
- `step(Resume(ExternalSession(Completed)))`:记录 session/output,把 output.summary 折进 Conversation
  (start_assistant_response → finish_assistant → commit_pending),cursor → `Done`,quiescent。
- `step(Resume(ExternalSession(Failed)))`:记录 session(若有),cursor → `Error { message }`。
- observations 暂不转 notification(留 M5),但 resume 分支要接收透传。
- `cursor()` 返回与 `ExternalAgentCursor` 对应的 `LoopCursor` 视图(存一个非 serde 的 `loop_cursor` 字段,
  与 external cursor 同步:Idle→Idle,AwaitingSession/AwaitingInteraction→streaming_step(step_id, req),
  Done→done(Completed),Error→error(msg))。
- PausedForInteraction 属 M3-3;M3-2 遇到时以清晰 `fail` 收敛(M3-3 覆盖)。
- Abandon 属 M3-4;M3-2 做最小安全收敛(settle Idle,不 emit requirement)。

### 依据
- `AgentMachine` trait / `StepInput` / `StepOutcome`(src/agent/machine/mod.rs)。
- `DefaultAgentMachine`(src/agent/machine/default/mod.rs)park/emit/fold/fail 模式。
- DTO:`ExternalSessionRequest`/`ExternalSessionInput`/`ExternalSessionResult`(src/agent/external/mod.rs)。
- state/cursor:`ExternalAgentState`/`ExternalAgentCursor`(src/agent/external/state.rs)。
- requirement:`RequirementKind::NeedExternalSession` / `RequirementResult::ExternalSession`(src/agent/requirement.rs)。
- 测试:testkit `ScriptedExternalSessionHandler` + `DrainHarness` + `TestScope`。

### 实施步骤
1. `src/agent/external/state.rs`:新增 `conversation_mut()`(M3-1 record 已声明留待 M3-2)。
2. 新增 `src/agent/external/machine.rs`:`ExternalAgentMachine`(state + requirement_ids + loop_cursor +
   in_flight scratch),`AgentMachine` impl,单测(fail/pivot 拒绝等)。
3. `external/mod.rs`:`mod machine; pub use machine::ExternalAgentMachine;` + 模块 doc。
4. `agent/mod.rs`:re-export `ExternalAgentMachine`。
5. testkit:`TestScope`/`TestScopeBuilder` 增加 `external` handler;`ExternalAgentFixture` 增加
   `spec()`/`agent_state()`/`machine()`;prelude 无需改(已导出 fixture 类型)。
6. 新增集成测试 `tests/agent_external_basic.rs`:`external_agent_start_to_completed`、
   `external_agent_start_to_failed`(DrainHarness + scripted external handler)。

### 验证门(完整序列)
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. 聚焦:`cargo test external_agent_start`
4. `cargo test --all --all-targets`(≤30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`

### 进度
- (完成)`src/agent/external/state.rs` 补 `conversation_mut()`。
- (完成)`src/agent/external/machine.rs` 实现 `ExternalAgentMachine`(state + requirement_ids + loop_cursor +
  in_flight scratch),`AgentMachine` impl。
- (完成)`src/agent/external/machine/tests.rs` 8 个单测(park/commit/continue/fail/pivot/abandon 等)。
- (完成)`external/mod.rs` + `agent/mod.rs` 接线与 re-export。
- (完成)testkit:`TestScope`/`TestScopeBuilder` 增加 `external` family;`ExternalAgentFixture` 增加
  `spec()`/`agent_state()`/`machine()`。
- (完成)集成测试 `tests/agent_external_basic.rs` 3 个:`external_agent_start_to_completed`、
  `external_agent_start_to_failed`、`external_agent_continue_advances_established_session`。
- (完成)修 rustdoc 私有 intra-doc link(struct doc 改指 `crate::agent::external`)。
- (完成)验证门全绿:fmt 无差异、clippy 0 告警、`cargo test external_agent_start` 2 passed、
  `cargo test --all --all-targets` 全绿、`cargo doc -D warnings` 0 告警。
- (完成)TODO.md 标记 M3-2 `[DONE]` 并补完成记录。
- 待办:提交(带 Co-authored-by trailer)后停止。M3-3(两段式交互)为下一次调用。

