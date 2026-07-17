# M9-3 支持 turn-boundary external reconfig

**当前任务 = TODO.md 首个未完成 = M9-3**（`### [TODO] M9-3`, line 2800）。M1..M9-2 全 `[DONE]`。

## 任务要求（TODO.md 2800-2822 + design §19）
- 定义 external reconfig 策略：
  - 已完成 turn 后（turn boundary）可替换 `active_tools`。
  - in-flight session 不支持热替换（hot swap）时返回 `UnsupportedCapability` 或排队到下一 boundary。
- 下一次 `NeedExternalSession(Start/Continue)` 必须携带新 tools。
- 验证：unit tests（boundary reconfig 更新 request.tools；in-flight unsupported hot reconfig 不会悄悄改变
  live session）；`cargo test -p agent-lib external_reconfig`；完整验证序列 1-6。

## 设计文档对齐（docs/managed-external-agent.md §19）
两级 reconfig：
1. boundary toolset reconfig：下一次 Start/Continue 使用新 tools。
2. live tool bridge reconfig：runtime 支持时 handler 发 runtime-specific reconfigure；不支持返回 UnsupportedCapability。
首版只做 boundary reconfig，并要求 runtime capability 明确。

## 现状核对（已读源码）
- ExternalAgentState { active_tools: ToolSetRef, ... }，active_tools()/set_active_tools() 已存在 (state.rs)。
  自定义 serde（ExternalAgentStateRecord）；new() 从 spec.initial_tools() seed。
- ExternalAgentMachine.build_request() 直接读 active_tools().tools().to_vec() → 更新 active_tools 即让下一 request 带新 tools。
- in_flight: Option<InFlight>：Some 仅在一个 turn 进行中；turn 边界（Idle/Done/Error）为 None → boundary 判据 = in_flight.is_none()。
- begin_user_turn() 是每个新 turn 的入口，随后 block_on_session→build_request。
- ExternalCapability（8 项 + ALL[8]）与 ExternalRuntimeCapabilities（每 variant 一 bool 字段）；
  UnsupportedCapability { runtime, capability, detail }。probes/registry-test 均从 none() 起再 .field=。
- machine.rs 目前不返回 Result<_, ExternalAgentError>；返回需 #[allow(clippy::result_large_err)]（budget.rs 同法）。

## 实施（小补丁、增量）
1. capability.rs：新增 ExternalCapability::Reconfigure（末位）；ALL 8→9；as_str="reconfigure"；
   ExternalRuntimeCapabilities 新增 reconfigure: bool；none() false；supports() 新 arm；测试 full 字面量 true。
2. 3 adapter（codex/claude_code/opencode）：implemented_capabilities() 加 reconfigure: false；
   intersect_capabilities() 加 reconfigure: left.reconfigure && right.reconfigure。
3. state.rs：新增 pending_reconfig: Option<ToolSetRef>；accessors set/take/clear/get；record skip_if None；serde 同步。
4. machine.rs：ExternalReconfigTiming{NextBoundary(default),Hot} + ExternalReconfigOutcome{Applied,Queued}；
   reconfigure(active_tools,timing)->Result<Outcome,ExternalAgentError>；begin_user_turn 顶部 drain pending。
   boundary→Applied；in-flight+NextBoundary→Queued（不动 live）；in-flight+Hot→Err(UnsupportedCapability{Reconfigure}) 不改状态。
5. 导出：external/mod.rs + agent/mod.rs 两新类型。
6. docs：capability-matrix.md、managed-external-agent.md §19/status/parity。

## 测试（machine/tests.rs，external_reconfig_* 前缀，全离线 <1s）
boundary(fresh)→Applied 带新 tools / boundary(Done)→Applied 带新 tools / in-flight NextBoundary→Queued 不改 live、下一 turn 带新 /
in-flight Hot→Err 不改状态 / Hot@boundary→Applied / queued 经 snapshot/restore 后仍带新。

## 验证序列
fmt --check → cargo test -p agent-lib external_reconfig → clippy -D warnings（±features）→
cargo test --all --all-targets → doc -D warnings → git diff --check。

## 完成状态
M9-3 已完成并 [DONE]。验证序列 1-6 全过（fmt/clippy(±features)/doc/git-check 干净，external_reconfig 6 passed，full suite 919 ok 0 failed，feature-gated lib 753 ok）。下一个未完成任务 = M9-4。
