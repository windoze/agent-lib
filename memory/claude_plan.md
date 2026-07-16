# M5-2 — observation 缓冲 → notification 转换与 event sink

**状态:完成(已全绿,已提交)。**

## 目标(TODO.md M5-2)
在 `ExternalAgentMachine` 的 resume 分支里,把 `ExternalSessionResult` 的
`observations: Vec<ExternalAgentEvent>` 依序转成 `Notification::ExternalAgent`,放入本步
`StepOutcome.notifications`;用 `ExternalSessionRef.last_event_seq` 去重,避免 resume 重放已消费 event。
(可选)定义 `ExternalEventSink` trait 作为非阻塞实时旁路占位。

## 关键事实(已核对)
- `fold_session_result`(machine.rs:295)当前丢弃 observations,注释说“lands in M5”。
- 三个决策点变体都带 `observations`;`Completed`/`Paused` 带 `session: ExternalSessionRef`,
  `Failed` 带 `session: Option<ExternalSessionRef>`;`ExternalSessionRef.last_event_seq: Option<u64>`。
- 机器已经通过 `state.set_session(new)` 持久化最近的 session facts(含 last_event_seq),
  所以“已消费游标”天然就是 `state.session().last_event_seq`,无需新增字段。
- `StepOutcome::new(notifications, requirements, quiescent)`;`Notification::ExternalAgent(ExternalAgentEvent)`
  已在 M5-1 落地(event.rs:221)。
- `Notification`、`ExternalAgentEvent` 均由 `crate::agent` 再导出。

## 去重语义(§5.5)
- 读取 resume 前 `prev = state.session().and_then(|s| s.last_event_seq)`。
- 取本次 result 的 `incoming = session.last_event_seq`。
- 若 `incoming` 与 `prev` 均 Some 且 `incoming <= prev` → 这批 observations 已消费,发 0 条。
- 否则按序转成 `Notification::ExternalAgent` 全部发出。
- 必须在 `set_session(new)` 之前读取 `prev`。

## 步骤
1. [x] 阅读 machine.rs / mod.rs DTO / state.rs / event.rs / tests.rs。
2. [x] machine.rs:导入 `ExternalAgentEvent`、`Notification`;加 `observe(incoming_seq, obs) -> Vec<Notification>` helper。
3. [x] `fold_session_result` 为三变体计算 notifications(在 set_session 前读 prev),透传给
   `complete_session` / `pause_for_interaction` / Failed 分支;三方法/分支把 notifications 放进 StepOutcome。
4. [x] 更新 machine.rs 模块头/`fold_session_result` 注释(不再说“dropped/lands in M5”)。
5. [x] 新增 `src/agent/external/sink.rs`:`ExternalEventSink` trait(可丢弃、不阻塞)+ 无操作实现;再导出。
6. [x] 新增单测 `external_agent_emits_observation_notifications`(tests.rs):
   Completed 带若干 observations → drain 后 notifications 按序、数量正确;
   pause↔respond 中重复同一 `last_event_seq` 的 result 不重放。
7. [x] 验证序列:fmt --check → `cargo test external_agent_emits` → clippy -D warnings → 全量 test →
   doc -D warnings → git diff --check。
8. [x] TODO.md M5-2 标 [DONE] + 完成记录。
9. [x] 提交 `[M5-2] ...`,停止。

## 约束
- 不改既有 notification wire tag;新增为增量路径。
- machine 保持 sans-io:event sink 属 handler 层,trait 仅作占位,不接进 step。
- 新公开 API 带 rustdoc。
