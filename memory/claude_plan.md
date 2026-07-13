# 执行计划 — M2-1 `AgentMachine`、`StepInput`、`StepOutcome` 与 `AgentInput` 调整

## 当前任务

第一个未完成任务：**M2-1**（前置 M1-R 已 [DONE]）。迁移文档 §2。
只定义类型与 trait，不实现具体 `step` 逻辑（留 M2-3/M2-4）。

## 做什么

1. 新建 `src/agent/machine.rs`，从 `agent/mod.rs` 导出并 re-export：
   - `trait AgentMachine { fn step(&mut self, input: StepInput) -> StepOutcome;
     fn cursor(&self) -> &LoopCursor; }`（非 async_trait，对象安全）。
   - `enum StepInput { External(AgentInput), Resume(RequirementResolution), Abandon(RequirementId) }`
     （含构造器）。因 `Resume` 携带运行期 `RequirementResolution`（非 serde），整体不派生 serde；
     仅 Clone+Debug。
   - `struct StepOutcome { notifications: Vec<Notification>, requirements: Vec<Requirement>,
     quiescent: bool }`（决策 B）。全字段可 serde → 派生 Serialize/Deserialize + Clone/Debug/PartialEq；
     加 `new` 构造器与便捷 `quiescent()`/`is_quiescent()`。
2. 调整 `AgentInput`（event.rs）：
   - 新增 `Pivot(PivotMessage)` 变体 + `pivot(..)` 构造器。
   - **并存策略**：`DefaultAgentLoop` 仍使用 `QueuedPivotTurn`/`Resume`，删除会破坏它，
     故保留旧变体并加 `#[deprecated]`（note：M4 清理），在内部消费点加 `#[allow(deprecated)]`
     以过 `-D warnings`。完成记录写明该选择。
   - `default.rs` 的 `AgentInput` match 需补 `Pivot` 臂：legacy loop 不支持直插 pivot（走队列），
     返回 `AgentError::Other` 明确报错（现有测试不构造 `Pivot`，行为不变）。
3. mod.rs：`pub mod machine;` + `pub use machine::{AgentMachine, StepInput, StepOutcome};`

## 需要加 `#[allow(deprecated)]` 的内部消费点

- event.rs: `AgentInput::queued_pivot_turn`、`AgentInput::resume` 构造器；event.rs 测试。
- default.rs: match 的 `QueuedPivotTurn`/`Resume` 臂。
- default/tests.rs、loop_driver.rs: `AgentInput::resume(..)`。

## 测试（machine.rs #[cfg(test)]）

- fake machine 实现 `AgentMachine`，可作为 `Box<dyn AgentMachine>`（对象安全）。
- `StepOutcome` serde round-trip（含 notifications + requirements）。
- 调整后 `AgentInput`（含 `Pivot`）serde round-trip。
- `StepInput` 构造器 + Clone/Debug 断言（Resume 非 serde，故不测 serde）。

## 验证命令

- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`
- 聚焦：agent::machine 测试
- `cargo test --all --all-targets`（≤30min）
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `git diff --check`

## 进度

- [x] 新建 machine.rs
- [x] 调整 AgentInput + 并存 deprecated
- [x] default.rs 补 Pivot 臂 + allow(deprecated)
- [x] mod.rs 导出
- [x] 测试（machine 5 passed）
- [x] 全套验证：fmt/clippy/聚焦/全量(lib 380)/rustdoc/diff 全通过
- [x] TODO.md 标 [DONE] + 完成记录
- [ ] 提交并停止
