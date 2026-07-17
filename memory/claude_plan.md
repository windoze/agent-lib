# M4-4 Review：stream/capability/policy 完整性检查

**当前执行 = TODO.md 第一个未完成任务 = M4-4**（M1..M3、M4-1、M4-2、M4-3 已 `[DONE]`）。

## 任务性质
纯 sign-off review。逐条核对 M4-1..M4-3 落地的 stream(live sink)/capability model/session policy
源码 + 测试，唯一代码外产物是 `docs/capability-matrix.md` 增补 managed external capability 章节。
不引入 spec 偏差、不加 workaround。

## Review 检查清单（TODO.md M4-4 body）
1. 所有 runtime-dependent 功能都有 capability 表达（streaming/resume/permission bridge/host tools/
   host subagents/artifacts/usage/graceful shutdown）→ `ExternalCapability`(8 变体, ALL[8]) +
   `ExternalRuntimeCapabilities`(8 bool 字段) 一一对应。✓
2. `ExternalSessionPolicy`(runtime-facing: permission_mode/isolation/max_turns/stream_events) 与
   machine config(`ExternalAgentMachineConfig`: tool_failure/required_capabilities/max_decision_loops)
   职责边界清晰；后者是 plain-data serde DTO，不进 serializable `ExternalAgentState`。✓
3. live sink 不是 blocking effect：`ExternalEventSink::emit(&self, &ExternalObservedEvent) -> ()`，
   machine 从不持有 sink；只有 Requirement 能阻塞 continuation。✓
4. 更新 `docs/capability-matrix.md`：新增「Managed External Runtime 能力模型」章节，列 8 个
   capability + 保守默认(全 unsupported) + fallback 策略；明确不声称任何未验证 runtime 支持。

## 能力 fallback 策略（完成记录用）
- baseline = `ExternalRuntimeKind::conservative_capabilities()` → 全部 false（不假设支持）。
- 未声明 required 的 capability 缺失：保留原通用错误（如 tool id unavailable），兼容 pre-M4-3。
- 声明 required 的 capability 缺失：`UnsupportedCapability{runtime,capability,detail}` 分类错误，
  scheduler 可据此避免再次 dispatch。
- tool 失败：`ReturnErrorToRuntime`(默认，runtime 自主决策) / `StopRun`(停 turn)。
- decision loop 超 `max_decision_loops`：`LimitExceeded`。
- stream：`ExternalStreamPolicy::{Buffered(默认)/Streaming/Disabled}`；sink 可自由丢弃事件，
  exact-once 由 `observations`+seq dedup 保证，sink 只是 lossy live mirror。

## 验证条件（TODO.md）+ 完整序列 1-6
- `cargo test -p agent-lib external_capabilities`
- `cargo test -p agent-lib external::sink`
- 1 fmt / 2 焦点测试 / 3 clippy -D warnings / 4 全量 test（仅 doc 改动→可复用上次 green）/
  5 doc -D warnings / 6 git diff --check。

## 状态：已完成（M4-4 [DONE]）
