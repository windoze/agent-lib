# M4-1 将 `ExternalEventSink` 升级为 sequenced live sink

**当前执行 = TODO.md 第一个未完成任务 = M4-1**（M1-*、M2-*、M3-* 已 `[DONE]`）。

## 任务分析（来自 TODO.md M4-1 + docs §10.1）
- `src/agent/external/sink.rs` 的 `ExternalEventSink::emit(&ExternalAgentEvent)` 是占位。
- 设计 §10.1（docs/managed-external-agent.md 673-674）与 §6 表（line 342）要求 live sink 按 `seq` emit
  `ExternalObservedEvent`，与 buffered observations 双通道，seq 是唯一 replay 进度。
- sink 只是 UI tail，不改 control flow；无生产调用方（仅 trait + DiscardEventSink + 再导出）。

## 方案决策
- 直接把 `ExternalEventSink::emit` 签名改成接收 `&ExternalObservedEvent`（sequenced）。
  - 无生产调用方，改签名安全且最干净（对齐 §10.1 `sink.emit(observed_event)`）。
  - 不新增并行 trait，避免双 trait 维护成本。

## 做什么
1. `sink.rs`:
   - `use super::ExternalObservedEvent;`
   - `trait ExternalEventSink::emit(&self, event: &ExternalObservedEvent)`。
   - `DiscardEventSink` impl 改签名。
   - rustdoc 明确三点：sink 不得阻塞；允许丢事件；exact-once 只由
     `ExternalSessionResult.observations` + machine replay 保证。
   - doctest 示例改用 `ExternalObservedEvent`。
2. 测试:
   - 更新 `discard_sink_accepts_and_drops_events` 用 sequenced events。
   - 新增 `collecting_sink_records_sequenced_events_for_tests`：test-only collecting sink
     （`Mutex<Vec<ExternalObservedEvent>>`），证明 sink 收事件不影响独立 buffered observations，
     且按 seq 记录。

## 验证条件（TODO.md）
- `discard_sink_accepts_and_drops_events` 更新并通过。
- 新增 `collecting_sink_records_sequenced_events_for_tests`。
- `cargo test -p agent-lib external::sink`。
- 完整验证序列 1-6：fmt / 焦点测试 / clippy -D warnings / 全量 test / doc -D warnings / git diff --check。

## 影响面
- 仅 `sink.rs` 有 `.rs` 改动；无生产调用方需改。
- 需同步 docs（M3 状态表 line 1137、§1 现状表 line 61/113）标注 M4-1 落地。

## 状态：已完成（M4-1 [DONE]）
- 仅 `src/agent/external/sink.rs` 有 .rs 改动 + docs 同步 + TODO.md/memory。
- 完整验证序列 1-6 全过（focused external::sink 2 passed；full suite 38 组 0 failed）。
- 下一任务 = M4-2，本轮不启动。
