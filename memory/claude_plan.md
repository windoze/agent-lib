# M6-3 — spawn_agent / plan / blackboard / mailbox 工具 adapter

**状态:已完成(M6-3 已标 `[DONE]`,待提交后停)。** 本次执行 TODO.md 第一个未完成任务 = M6-3。
前置 M6-2 已 `[DONE]`(dispatcher / TaskEvaluator / WorkerChoice::into_subagent → NeedSubagent)。

## 执行结果(全部完成)
- [x] `src/agent/collab/{plan,blackboard,mailbox,tools,mod}.rs` 实现完成并接线到 `src/agent/mod.rs`。
- [x] `src/agent/collab/tests.rs` 24 单测 + `tests/agent_tool_adapter.rs` 2 集成测试(全部含 `tool_adapter`)。
- [x] 三条必需验证覆盖:spawn_agent→NeedSubagent 并经真实 DrivingSubagentHandler 派生驱动到完成;
      plan_claim 依赖未完成被拒且零改动;blackboard append-only 且 offset 单调。
- [x] 文档:`docs/external-agent.md` §3.4/§3.5 增补「已实现(M6-3)」;`README.md` 模块表补 `agent::collab`。
- [x] 验证序列全绿:`cargo fmt --all`;`cargo clippy --all-targets -- -D warnings` 干净;
      `cargo test tool_adapter` 26 通过;`cargo test --all --all-targets` 全绿(lib 521 通过);
      `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 干净。
- [x] TODO.md M6-3 标 `[DONE]` 并填完成记录。下一步:commit `[M6-3] ...` 后停(不开始 M6-4)。

## 任务要求(TODO.md M6-3)
- 实现桥接工具 adapter:
  - `spawn_agent`(→ 结构化请求 → `RequirementKind::NeedSubagent`,复用现有 SubagentHandler 派生路径)
  - `plan_claim` / `plan_claim_first_available` / `plan_update`(+ `plan_add_task` / `plan_read`)
  - `blackboard_post` / `blackboard_read`
  - `send_message`(mailbox)
  - `report_artifact`(记录为 artifact/notification)
  - `run_host_tool`(受控转发宿主注册 tool)
- adapter 必须经宿主 policy/护栏(RunContext:check_cancelled;owner/sender 用注入身份而非模型参数),
  不直接写外部 runtime 私有 mailbox;claim 必须检查依赖已完成(对齐 docs/agent-layer.md §6.2)。
- 提供把这些工具注入 external agent `initial_tools` 的构造入口(ToolSetRef 构造 + ToolHandler)。

## 验证条件(TODO.md)
- 新增测试:`spawn_agent` adapter 产生 `NeedSubagent` 并经 `SubagentHandler` 派生;`plan_claim` 在未完成
  依赖时被拒;`blackboard_post`/`read` append-only 且偏移单调。过滤名:`cargo test tool_adapter`。
- 完整验证序列全绿。

## 方案(新增 `src/agent/collab/` 一等垂直功能 + 桥接 adapter)
- `plan.rs`:TaskStatus / Task / PlanSnapshot / Plan(Mutex live handle,Arc 共享)/ PlanError。
  claim = 版本 CAS + owner + 状态可转 + 依赖全部 Completed;失败零改动。含 detect_cycle。
- `blackboard.rs`:BoardMessage / Blackboard(命名空间,per-channel 偏移从 0 单调,append-only)。
- `mailbox.rs`:MailMessage / Mailbox(全局单调 seq,定向 inbox)。
- `tools.rs`:工具名常量 + Tool 声明 + bridge_tool_set(id)->ToolSetRef 注入入口;
  SpawnAgentRequest.parse + into_requirement_kind(step_id)->NeedSubagent;ArtifactSink+RecordingArtifactSink;
  CollabToolHandler(impl ToolHandler):fulfill 先 check_cancelled,再分派;owner/sender=注入身份。
- tests.rs:单测名含 tool_adapter(3 项验证 + 覆盖)。
- 接线:src/agent/mod.rs pub mod collab + pub use。
- 集成测试:tests/agent_tool_adapter.rs(spawn_agent→NeedSubagent→DrivingSubagentHandler 端到端)。
- 文档:docs/external-agent.md §3.4/§3.5 追加「已实现(M6-3)」。

## 验证序列(完成前)
cargo fmt --all → cargo clippy --all-targets -- -D warnings → cargo test tool_adapter →
cargo test --all --all-targets(≤30min)→ RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace。

## 完成后
标 TODO.md M6-3 为 [DONE] + 填完成记录;commit [M6-3] ...;停。
