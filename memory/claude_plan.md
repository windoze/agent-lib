# 当前任务：M5-2 实现 cancel-on-call 与 panic-on-call wrappers

## 定位
- `TODO.md` 第一个未完成任务 = **M5-2**（line 1024，标题 `[TODO]`）。
- 前置 M5-1 已 `[DONE]` 且提交（HEAD=bef3638）。
- 工作区检查：有无关未跟踪文件 `docs/external-agent.md`（非本任务产物，不纳入提交）。

## 任务要求（TODO.md M5-2）
- 实现 `CancelOnCall<H>` wrapper：调用前或调用后 cancel `RunContext`。
- 实现 `PanicOnCall` handler/wrapper：断言某 family 不应被触发。
- 支持按第 N 次调用触发 cancel。
- 与 call log 集成，记录 cancel 发生时机。

验证单测：
- LLM 返回 tool_use 后 cancel，tool handler 未触发。
- PanicOnCall 在不应触发路径不 panic，在触发路径 panic。
- 全套验证命令。

## 设计（写入 `crates/agent-testkit/src/concurrency.rs`，lib.rs 模块拓扑已声明 concurrency 含 cancel/panic 工具）
- `CancelTiming { Before, After }`：cancel 相对 inner 调用的时机。
- `CancelEvent { call_index, timing }` + `CancelLog`（Mutex<Vec<CancelEvent>>）：记录 cancel 发生的 dispatch 序与时机；`events()/len()/is_empty()/cancelled()/cancelled_at()`。
- `CancelOnCall<H>`：字段 inner、timing、trigger_call(1-based)、calls(Arc<Mutex<usize>>)、log(Arc<CancelLog>)。
  - `before(inner)`/`after(inner)`；`on_call(nth)` 设置第 N 次触发；访问器 inner/timing/trigger_call/log/cancelled/dispatched。
  - `next_index()` 领取 dispatch 序；`cancel_if_due(index, phase, ctx)` 命中触发调用且时机匹配时 `ctx.cancellation().cancel()` 并记 log。
  - 为四个 family 各实现 trait：LlmHandler/ToolHandler/InteractionHandler/ReconfigHandler，每个 fulfill = next_index → cancel_if_due(Before) → inner.fulfill → cancel_if_due(After)。
- `PanicOnCall`：可带自定义 message，实现 Llm/Tool/Interaction/Reconfig 四 family，调用即 panic。

## 计划步骤
1. [x] 写 plan。
2. [x] 读现有 concurrency.rs / handlers / scope / fixtures / context(cancel) / drive ToolHandler。
3. [x] concurrency.rs：更新 import；实现 CancelTiming/CancelEvent/CancelLog/CancelOnCall（4 trait impl）/PanicOnCall（4 trait impl）。
4. [x] prelude.rs 追加再导出。
5. [x] 单测（concurrency.rs #[cfg(test)]）：
   - before：inner 观察到 ctx 已 cancelled；log 记录 index0/Before。
   - after：inner 观察到未 cancelled，返回后 ctx.is_cancelled()；log 记录 After。
   - on_call(2)：仅第 2 次触发，log.cancelled_at()==1。
   - 集成 drain：CancelOnCall::after(scripted llm 返回 tool_use) + PanicOnCall tool → tool 未触发（不 panic），cursor Idle，cancel log 非空。
   - PanicOnCall 触发路径 #[should_panic]：LLM tool_use 无 cancel + PanicOnCall tool → panic。
   - PanicOnCall 未触发路径：interaction family PanicOnCall 未被调用则不 panic（普通文本 turn）。
6. [x] 验证：cargo fmt → clippy -D warnings（root + -p）→ 聚焦测试 → 全量 test（≤30min）→ rustdoc -D warnings → git diff --check。
7. [x] TODO.md M5-2 标题改 [DONE] + 完成记录。
8. [x] 提交(进行中)（仅 M5-2 相关；不含 external-agent.md）。停止。

## 备注
- 无已知阻塞 spec 偏差。若发现，按真实问题处理（修复或插入前置任务）。
