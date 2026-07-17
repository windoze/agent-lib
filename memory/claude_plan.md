# M5-2 实现 scripted external runtime adapter

**当前执行 = TODO.md 第一个未完成任务 = M5-2**（M1..M4 与 M5-1 均 `[DONE]`）。

## 关键发现
- M5-2 的 heading 在 M5-1 提交（9c5eece）中丢失（与 M4-2 heading 丢失事故同类，
  参考 c9df411）。任务正文（上下文/做什么/验证条件）仍在 TODO.md 中，但缺 `### [TODO] M5-2` 标题。
- 原始标题（1fd0405 历史）：`### [TODO] M5-2 实现 scripted external runtime adapter`。
- 第一步：恢复标题并单独提交，然后实现。

## 目标
在 agent-testkit 中实现 `ScriptedExternalRuntimeAdapter`（实现 M5-1 的
`ExternalRuntimeAdapter`/`ExternalRuntimeSession` 两 trait），并提供一个用
`ExternalSessionRegistry` 组装的 `ExternalSessionHandler`，然后用 `drain` 覆盖完整
managed loop：Start→Completed、Start→PausedForToolCalls→NeedTool→RespondToolResults→Completed、
Start→PausedForInteraction→RespondInteraction→Completed、Start→PausedForSubagent→RespondSubagent→Completed。
全部离线。

## 设计
- 位置：testkit（因 M5-3 cassette 复用）。将 `crates/agent-testkit/src/external.rs`
  转为目录模块 `external/mod.rs`，新增 `external/runtime.rs` 放 scripted adapter，避免文件膨胀。
- `ScriptedExternalRuntimeSession`（impl `ExternalRuntimeSession`）：
  - 持有固定 `session_id`（保证 registry 可 key + reattach）与单调 `seq` 计数。
  - 持有脚本 `VecDeque<ScriptedAdvance>`；`advance` pop 一条，
    可选断言收到的 `ExternalSessionInput` 判别式，
    将该条的 events 以单调 seq 同时 emit 到 live sink 并 buffer 为 observations，
    产出对应 `RuntimeDecisionPoint`（或 Err）。
  - `session_ref()` 返回带 session_id + last_event_seq 的 facts。
  - `shutdown()` 返回 Graceful。
- `ScriptedExternalRuntimeAdapter`（impl `ExternalRuntimeAdapter`）：
  - `kind()`/`capabilities()` 可配置（默认 resume=false，reattach 靠 live handle）。
  - `start` 记录收到的 request、取出脚本、构造 session、把 sink 存入 session。
  - 记录每次 start 的 request 供断言。
- `ScriptedRuntimeExternalSessionHandler`（impl `ExternalSessionHandler`）：
  - 内部持 `Arc<ExternalSessionRegistry>` + 可选 `Arc<dyn ExternalEventSink>`。
  - `fulfill`：`registry.get_or_start(request, ctx, sink)` → lock handle →
    `advance(&request.input, ctx)` → `Result<RuntimeDecisionPoint,_>` 折叠成
    `ExternalSessionResult`（复用 M5-1 的 `From` impl）→ `RequirementResult::ExternalSession`。
- Builder：`ScriptedRuntimeBuilder`，链式 push 各类 advance（completed/tool_pause/
  interaction_pause/subagent_pause/failed），产出 adapter + handler + registry + sink 观察句柄。

## 测试（新文件 tests/agent_external_scripted.rs，函数名 `scripted_external_*`）
1. `scripted_external_start_to_completed`：单 advance completed，drain→Done，断言 sink 收到 sequenced events。
2. `scripted_external_tool_batch_round_trip`：Start→PausedForToolCalls→(ScriptedToolHandler)→RespondToolResults→Completed。
   机器需 `with_tool_execution_ids`。断言 adapter 第二次 advance 收到 RespondToolResults。
3. `scripted_external_interaction_round_trip`：Start→PausedForInteraction→(ScriptedInteractionHandler)→RespondInteraction→Completed。
4. `scripted_external_subagent_round_trip`：Start→PausedForSubagent→(DrivingSubagentHandler)→RespondSubagent→Completed。
5. （testkit 内单测）adapter/session：expected-input mismatch panic、sink 收到 events、registry reattach 同一 handle。

## 验证序列（TODO.md 1-6）
1. `cargo fmt --all -- --check`
2. `cargo test -p agent-lib scripted_external` + `cargo test -p agent-lib external_agent_start_to_completed`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`（有代码改动，需跑，≤30min）
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 状态：进行中
- [x] 恢复 M5-2 标题（单独提交）
- [ ] 实现 scripted adapter/session/handler
- [ ] 新增 scripted_external_* drain 测试
- [ ] 验证序列 1-6
- [ ] 标记 [DONE] + 提交
