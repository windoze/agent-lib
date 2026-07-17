# M5-1 定义 ExternalRuntimeAdapter / ExternalRuntimeSession / ExternalSessionRegistry

**当前执行 = TODO.md 第一个未完成任务 = M5-1**（M1..M4 全部 `[DONE]`）。

## 目标
在 `src/agent/external` 下建立 runtime adapter 抽象层与 session registry skeleton，
为 M5-2（scripted adapter + handler 组装）与真实 adapter（M6-8）预留边界。不接真实 CLI。

## 设计
新增两个子模块（模块化，避免 runtime.rs 膨胀）：
- `src/agent/external/adapter.rs`
  - `RuntimeDecisionPoint`：adapter 内部四个非失败决策点
    (Completed / PausedForInteraction / PausedForToolCalls / PausedForSubagent)，
    字段与 `ExternalSessionResult` 非 Failed 变体一一对应。
    - `session()` / `observations()` 访问器。
    - `into_session_result()` 映射到 DTO。
    - `impl From<Result<RuntimeDecisionPoint, ExternalAgentError>> for ExternalSessionResult`
      （Err -> Failed，并从 error 中提取 session）。
  - `ExternalRuntimeSession`（trait, object-safe, Send）：单个 live session。
    - `session_ref() -> ExternalSessionRef`
    - `async advance(&mut, input, ctx) -> Result<RuntimeDecisionPoint, ExternalAgentError>`
    - `async shutdown(&mut) -> ExternalSessionShutdown`
  - `ExternalRuntimeAdapter`（trait, object-safe, Send+Sync）：工厂 + 能力。
    - `kind()`, `capabilities()`
    - `async start(request, ctx, sink?) -> Result<Box<dyn Session>, Error>`
    - `async resume(session_ref, request, ctx, sink?) -> Result<Box<dyn Session>, Error>`
      默认实现返回 `ResumeUnavailable`，支持 resume 的 adapter override。
- `src/agent/external/registry.rs`
  - `LiveSessionKey { agent_id, session_id }`（session_id None -> 不可 key）。
  - `LiveSessionHandle = Arc<tokio::sync::Mutex<Box<dyn ExternalRuntimeSession>>>`。
  - `ExternalSessionRegistry { adapter: Arc<dyn Adapter>, live: std::sync::Mutex<HashMap<..>> }`
    - `get_or_start(request, ctx, sink)`：None -> start+register；Some(ref) -> 命中 reattach，
      否则 capabilities.resume ? adapter.resume+register : `ResumeUnavailable`。
    - `get(agent_id, session_ref)`：纯查找。
    - `cleanup(agent_id, session_ref) -> ExternalSessionShutdown`：移除 + shutdown。
    - `cleanup_agent(agent_id) -> Vec<ExternalSessionShutdown>`：cancel sweep（按 session_id 排序确定性）。
    - `capabilities()/kind()/live_len()` passthrough。
  - live handle 绝不进入 `ExternalAgentState`（registry 独立持有）。

## 关键约束验证
- trait object safe：async_trait 装箱 future + 非泛型方法 -> dyn-safe（doc 说明）。
- registry unit 覆盖：start 后 get/resume（同一 Arc）、cleanup 移除 handle、unknown -> ResumeUnavailable。
- 测试函数名前缀 `external_runtime_registry_*` 以匹配 `cargo test -p agent-lib external_runtime_registry`。
- adapter 错误统一 `ExternalAgentError`。

## 验证序列（TODO.md 1-6）
1. `cargo fmt --all -- --check`
2. `cargo test -p agent-lib external_runtime_registry`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`（有代码改动，需跑）
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 状态：已完成（M5-1 [DONE]）
