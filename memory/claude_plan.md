# 当前任务：M5-1 实现并发 delay/barrier/peak 工具

## 定位
- `TODO.md` 第一个未完成任务 = **M5-1**（line 993，标题 `[TODO]`）。
- M4-R 已 `[DONE]` 并提交（HEAD=811f2ef）。
- 工作区有一个**无关**未跟踪文件 `docs/external-agent.md`（设计草案，非 M5-1 产物，不纳入提交）。

## 任务要求（TODO.md M5-1）
在 `concurrency.rs`：
1. 实现 `Delay::yields(n)`、可选 barrier helper。
2. 实现 `PeakInFlight` 计数器和 completion log。
3. 给 `ScriptedToolHandler` 或 wrapper 增加 delay 与 peak recording 支持。
4. 避免 `tokio::time::sleep`；用 yield、oneshot/barrier 或手动 future。

验证单测：两 tool call 峰值 in-flight=2；delay 稳定 out-of-order completion；不依赖真实时间。

## 设计
- `Delay { ticks }`：`ready()`/`yields(n)`，`IntoFuture` → `YieldTicks` 手动 future，poll 时 `wake_by_ref` 返回 Pending n 次再 Ready。无真实时间。
- `Barrier`：cooperative barrier，`new(threshold)`、`wait()` → `BarrierWait`；第 threshold 个到达者释放并唤醒全部 waker。保证峰值并发 = threshold。
- `PeakInFlight`：Mutex 记 in_flight/peak/begun/completions；`enter()` → `InFlightGuard`（RAII，`complete()` 记录 completion order，Drop 兜底递减 in_flight 处理 cancel）；`peak()`/`begun()`/`completed()`/`completion_order()`/`in_flight()`。
- `DelayingToolHandler<H: ToolHandler>`：包 inner tool handler，注入 delay（Fixed / Ordered 队列，dispatch 序消费）+ 可选 barrier + PeakInFlight gauge。fulfill：enter → barrier.wait → delay → inner.fulfill → guard.complete。因 inner scripted fulfill 无 await，delay 提供 enter/complete 间的 await 点，令并发 fulfill 重叠。

## 计划步骤
1. [x] 写 plan。
2. [x] 读现有 script.rs/handlers.rs/drive.rs/fixtures。
3. [x] concurrency.rs 实现 Delay/YieldTicks、Barrier/BarrierWait、PeakInFlight/InFlightGuard、DelayingToolHandler。
4. [x] 单测（用 FuturesUnordered 复刻 driver 执行器）：peak=2（barrier）、out-of-order（ordered delays）、无真实时间。
5. [x] prelude.rs 追加再导出。
6. [x] 验证：fmt → clippy -D warnings → 聚焦测试 → 全量 test → rustdoc -D warnings → git diff --check。
7. [x] TODO.md M5-1 标题改 [DONE] + 完成记录。
8. [ ] 提交(进行中)（仅 M5-1 相关；不含 external-agent.md）。停止。

## 备注
- 无已知阻塞 spec 偏差。若发现，按真实问题处理（修复或插入前置任务）。
