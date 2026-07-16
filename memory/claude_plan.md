# M1-1 — 新增 `ExternalObservedEvent` 并把 observations 改成 sequenced payload

**当前执行 = TODO.md 第一个未完成任务 = M1-1**(全部任务仍为 `[TODO]`;这是全新
"Managed External Agent" 计划,上一轮 effect-refine 计划已归档)。

## 目标
把 `ExternalSessionResult::{Completed,PausedForInteraction,Failed}.observations` 从
`Vec<ExternalAgentEvent>` 升级为 `Vec<ExternalObservedEvent>`(带 `seq`),并把 machine
的 observation dedup 从「批级 last_event_seq 粗粒度」改为「逐事件 seq 精确 replay」。
不改 machine 的 sans-io 边界;不动 effect family。

## 关键设计决策
- 新 DTO `ExternalObservedEvent { seq: u64, event: ExternalAgentEvent }`,放在
  `src/agent/external/mod.rs`,derive `Clone,Debug,PartialEq,Eq,Serialize,Deserialize`。
- helper:`ExternalObservedEvent::new(seq, event)` 与
  `ExternalObservedEvent::unsequenced_for_tests(Vec<ExternalAgentEvent>) -> Vec<Self>`
  (enumerate 赋 seq;仅供 fixture,rustdoc 注明不得用于生产 dedup)。
- 新增 artifact helper `collect_file_patch_artifacts_from_observed(&[ExternalObservedEvent])`;
  保留旧 `collect_file_patch_artifacts(&[ExternalAgentEvent])`。
- machine `observe`:去掉 `incoming_seq` 参数,改为
  `filter(|o| consumed.is_none_or(|c| o.seq > c))`。consumed 仍读
  `state.session().last_event_seq`(在存入 incoming session 之前读取,顺序已验证正确)。
- Sink trait 签名**不动**(接收 `&ExternalAgentEvent`)——完整 sequenced live sink 升级
  是 M4-1 的任务。M1-1 只更新 sink 文档说明 buffered observations 是 exact-once 真源。

## 触及文件
1. src/agent/external/mod.rs — 新 DTO + helper + 新 artifact helper + result 变体字段类型 +
   round-trip 测试。
2. src/agent/external/machine.rs — observe 逐事件 dedup + fold_session_result 三处调用 + doc。
3. src/agent/external/machine/tests.rs — completed_with/paused_with 改签名 + 重写
   external_agent_emits_observation_notifications(含 PARTIAL overlap 证明逐事件 replay)。
4. src/agent/external/sink.rs — 文档说明(不改签名);discard_sink_... 保持通过。
5. src/agent/mod.rs — 导出 ExternalObservedEvent + collect_file_patch_artifacts_from_observed。
6. crates/agent-testkit/src/external.rs — completed/permission_pause/failed 用 unsequenced_for_tests。
7. tests/agent_external_real_e2e.rs — Completed 观测包装。
8. drive.rs / requirement.rs / assertions/external.rs 的 Vec::new() 保持不变。

## 验证序列
1. cargo fmt --all -- --check
2. 聚焦:external_dto_roundtrips / external_agent_emits_observation_notifications /
   discard_sink_accepts_and_drops_events
3. cargo clippy --all-targets -- -D warnings
4. cargo test --all --all-targets (<=30min)
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6. git diff --check

## 进度
- [x] 读 TODO/PLAN/memory/源码,选定 M1-1,梳理全部 construction sites
- [ ] 实现 DTO + helper + result 字段
- [ ] machine observe 逐事件 dedup
- [ ] 更新全部 fixture / 测试
- [ ] 验证序列
- [ ] TODO.md 标 [DONE] + 完成记录;commit

## 完成状态(2026-07-17)
- [x] 实现 DTO + helper + result 字段
- [x] machine observe 逐事件 dedup
- [x] 更新全部 fixture / 测试(含 PARTIAL overlap 证明)
- [x] 验证序列 1-6 全绿 + doc tests
- [x] TODO.md 标 [DONE] + 完成记录
- 下一任务:M1-2(external tool DTO 与 RespondToolResults / PausedForToolCalls)。
