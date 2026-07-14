# 当前任务：M4-2 实现 `DrainHarness`

## 目标（来自 TODO.md M4-2）
在 `crates/agent-testkit/src/harness.rs` 实现 `DrainHarness`，包装 `agent_lib::agent::drain`：
- 支持传入 machine、scope、optional parent pop、RunContext、input。
- 返回 `DrainObservation`：TurnDone、notifications、final cursor、可选 handler logs summary。
- 支持 `run_user(text)` convenience，内部仍走 `AgentInput::user_message` 与 `SeqIds`。
- 错误直接返回 `AgentError`，不转换成泛化字符串。

## 验证要求
- 单测：local tool scripted turn drain to Done。
- 单测：top unhandled interaction 原样返回 `AgentErrorKind::UnhandledRequirement`。
- 单测：cancelled context 路径不触发 tool handler。
- 全套验证命令：fmt → clippy -D warnings → 聚焦测试 → 全量测试 → rustdoc → git diff --check。

## 设计
- `drain` 异步签名：`drain(&mut M, input, &dyn HandlerScope, Option<&mut dyn Pop>, &RunContext) -> Result<TurnDone, AgentError>`。
- `DrainHarness<'d, M: AgentMachine>`：拥有 machine（与 StepHarness 一致），借用 scope/parent/ctx，持有 `SeqIds`（供 `run_user`）与 watched logs。
  - `new` / `with_ids`；`watching(name, Arc<dyn HandlerCallCounts>)`；`run(input)` / `run_user(text)`。
  - accessors：`machine`/`machine_mut`/`into_machine`/`ids`。
- `HandlerCallCounts: Send + Sync`（object-safe）：`begun()`/`completed()`；`impl for CallLog<Req: Send, Res: Send>` 委托 `len()`/`completed_len()`。
- `DrainObservation`：持 `TurnDone` + `Option<Vec<HandlerLogSummary>>`；`turn_done`/`into_turn_done`/`notifications`/`final_cursor`/`handler_logs`。
- `HandlerLogSummary { name, begun, completed }` + accessors。
- prelude 追加：`DrainHarness`、`DrainObservation`、`HandlerLogSummary`、`HandlerCallCounts`。

## 步骤
1. [x] 读 TODO/PLAN/drain/scope/handlers/fixtures。
2. [x] 实现 harness.rs 追加 + 单测(#[tokio::test])。
3. [x] prelude.rs 增加再导出。
4. [x] fmt → clippy → 聚焦测试 → 全量 → rustdoc → git diff --check。
5. [x] TODO.md 标 [DONE] + 完成记录。
6. [x] 提交（进行中）并停止。

## 备注
- 无阻塞 spec 偏差；未发现未排期失败测试。三个必测均 parent=None。
