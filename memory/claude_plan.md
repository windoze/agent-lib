# M5-4 — Milestone 5 Review(Event sink 与 artifact)

**状态:完成(全绿,已提交)。** 本次执行 TODO.md 第一个未完成任务 = M5-4(review 任务)。

## 任务要求(TODO.md M5-4)
- 核对 observation→notification 顺序与去重(§5.5);artifact 记录不落敏感原文(§11/§12);
  实时旁路接口不阻塞 continuation。
- 确认 `assertions/notifications.rs` 能覆盖新的 `ExternalAgent` notification 断言。
- 验证:完整验证序列全绿,`cargo test --all --all-targets` 无回归;review 结论写入完成记录。

## 复核结论(已核对源码)
1. **observation→notification 顺序/去重(§5.5)——通过。**
   - `fold_session_result`(machine.rs:307)三个决策点(Completed/PausedForInteraction/Failed)
     都在 `set_session` 之前调用 `observe(incoming_seq, observations)`,故读取旧的已消费游标。
   - `observe`(machine.rs:355):空→空;`incoming<=consumed`(两者都 Some)→判定已消费,发 0 条;
     否则 `into_iter().map(Notification::ExternalAgent)` 按序全发。任一 seq 缺失→按 as-is 全发。
   - Failed 用 `session.as_ref().and_then(last_event_seq)`,仅 Some 时 set_session。
2. **artifact 记录不落原文(§11/§12)——通过。**
   - `complete_session`(machine.rs:487)`record_artifacts(output.artifacts)`,move,仅存
     `ExternalArtifactRef{kind,summary,path,reference}`,`reference` opaque(`blob://`)。仅 Completed 记录。
   - `from_file_patch`/`collect_file_patch_artifacts` 仅产 ref,不含 diff 原文。
3. **实时旁路不阻塞(sink.rs)——通过。**
   - `ExternalEventSink::emit` + `DiscardEventSink` no-op;rustdoc 明确可丢弃/不阻塞/可跳过/untrusted,
     刻意不接进 sans-io `step`——只有 `Requirement` 阻塞 continuation。
4. **穷尽 match 复核**:全库唯一对 `Notification` 的穷尽 `match` 是 testkit `describe()`(notifications.rs:191),
   已含 `ExternalAgent` 臂;其余为 `find_map`/`filter_map`。无遗漏。

## 发现的缺口(需闭合,class-wide 一致性)
`NotificationAssertions` 对每个既有 family 都有 `*_count` 断言 + 访问器,**唯独缺 `ExternalAgent`**:
`describe()` 只做诊断渲染,没有可断言的 count/accessor。要让 review 结论「能覆盖 ExternalAgent 断言」
为真,按 class-wide-fix 原则补齐同族断言。

## 方案
1. `crates/agent-testkit/src/assertions/notifications.rs`:
   - import `ExternalAgentEvent`;新增 `external_agent_count(expected)` + 访问器
     `external_agent_events() -> Vec<&ExternalAgentEvent>`(stream order);加单测。
2. 验证:fmt --check → clippy -D warnings → `cargo test -p agent-testkit notification` →
   `cargo test --all --all-targets`(≤30min)→ doc -D warnings → `git diff --check`。
3. TODO.md M5-4 标 `[DONE]` + 完成记录;提交 `[M5-4] ...`;停止。

## 约束
- 只补 testkit 断言对称缺口;不改 machine/state 语义。新公开 API 带 rustdoc。
