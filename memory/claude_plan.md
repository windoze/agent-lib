# 执行计划 — M2-2 `LoopCursor` 升格为整台机器的可序列化状态

## 当前任务

第一个未完成任务：**M2-2**（前置 M2-1 已 [DONE]）。迁移文档 §5。
在 cursor 里补 `RequirementId`（及 `AgentPath`）使其精确记住“卡在哪个 requirement 上”，
支撑跨进程恢复重建未决登记表。

## 关键设计决策：requirement 寻址在 stage-0 为可选（Option）

- 迁移原则要求 M1–M3 期间 legacy `DefaultAgentLoop` 仍可编译可测；legacy loop 直接 await
  IO、**不 reify requirement**，也没有 `RequirementIds` 供给器（默认 `NoRequirementIds` 会报错，
  强行注入会破坏所有 legacy 测试）。
- 因此 cursor 的 requirement 绑定为 `Option`：legacy/测试传 `None`；未来 sans-io 机器
  （M2-3/M2-4）传 `Some(...)`，并在其测试里断言可读回 id。这是对“新旧路径并存”这一迁移事实
  的忠实建模，**不是**弱化不变量（新机器的强不变量在 M2-3/M2-4 强制）。
- `AgentPath` 阶段 0 恒为根，但类型先就位（随 requirement 绑定一起携带）。

## 新增类型（src/agent/state/cursor.rs）

- `CursorRequirement { id: RequirementId, origin: AgentPath }`：单 requirement 绑定
  （Step / Approval 用）。构造器 `new(id, origin)` / `root(id)`；`id()`（Copy 返回值）/`origin()`。
  serde：origin `#[serde(default, skip_serializing_if = "AgentPath::is_root")]`。
- `ToolWaitRequirements { origin: AgentPath, ids: BTreeMap<ToolCallId, RequirementId> }`：
  一批 tool requirement 绑定。`new(origin, ids)` / `root(ids)`；`origin()`/`ids()`/`get(call_id)`。

## cursor 结构改造（字段私有 + serde，Option 绑定 skip_serializing_if None）

- `StepCursor { step_id, requirement: Option<CursorRequirement> }`
- `ToolWaitCursor { step_id, tool_call_ids: Vec<ToolCallId>, requirements: Option<ToolWaitRequirements> }`
- `ApprovalCursor { step_id, tool_call_id, requirement: Option<CursorRequirement> }`

## 构造器更新（任务点名的三个）

- `LoopCursor::streaming_step(step_id, Option<CursorRequirement>)`
- `LoopCursor::awaiting_tool(step_id, tool_call_ids, Option<ToolWaitRequirements>) -> Result`
- `LoopCursor::awaiting_approval(step_id, tool_call_id, Option<CursorRequirement>)`

## 校验

- `ToolWaitCursor::validate`：保留 非空 + 去重；当 `requirements` 为 `Some` 时，其 map 键集必须
  与 `tool_call_ids` 集合完全一致（缺失或多余都报错）。新增
  `AgentStateError::ToolRequirementMismatch { call_id }`。

## 读回未决 requirement id 集合

- `LoopCursor::pending_requirement_ids(&self) -> Vec<RequirementId>`：
  StreamingStep→step 绑定 id；AwaitingTool→map values；AwaitingApproval→approval 绑定 id；其余空。

## 受影响 legacy 调用点（传 None）

- default.rs: streaming_step(503)、awaiting_tool(657)、awaiting_tool restore(741)、awaiting_approval(990)
- machine.rs FakeMachine 测试(267)
- state/tests.rs: streaming_step(155/255/261)、awaiting_tool(239/275/278)

## mod.rs / state.rs 导出

- state.rs `pub use cursor::{... CursorRequirement, ToolWaitRequirements}`。
- agent/mod.rs 追加导出 `CursorRequirement, ToolWaitRequirements`。

## 新增聚焦测试（state/tests.rs 或 cursor 内联）

- 各 cursor 带 requirement id/path 的 serde round-trip。
- `pending_requirement_ids` 能读回集合。
- tool requirement map 键不一致被拒（ToolRequirementMismatch）。
- 既有非法转换 / 空 / 去重 校验仍通过（更新签名传 None）。

## 验证命令（顺序）

- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`
- 聚焦：`cargo test --lib agent::state`
- `cargo test --all --all-targets`（≤30min）
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `git diff --check`

## 进度

- [x] cursor.rs 新类型 + 结构改造 + 构造器
- [x] state.rs 新错误变体 + 导出
- [x] 更新 legacy 调用点 + tests
- [x] mod.rs 导出
- [x] 新增聚焦测试(state +8 全绿)
- [x] 全套验证:fmt/clippy/聚焦(21)/全量(lib 388)/rustdoc/diff 全通过
- [x] TODO.md [DONE] + 完成记录
- [ ] 提交并停止
