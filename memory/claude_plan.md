# M1-2 — 新增 external tool DTO 与 `RespondToolResults` / `PausedForToolCalls`

**当前执行 = TODO.md 第一个未完成任务 = M1-2**(M1-1 已 `[DONE]`)。

## 目标
在 external DTO 层落地 host tool bridge 的协议数据结构(provider-neutral、serde-friendly),
但**不**在 machine 里真正驱动 tool call(machine tool parity 是 M2)。

## 要新增 / 修改
1. `src/agent/external/mod.rs`
   - `ExternalToolBatchId(String)` newtype(`#[serde(transparent)]`,`new`/`as_str`),
     derive `Clone,Debug,PartialEq,Eq,Serialize,Deserialize`。
   - `ExternalToolCall { provider_call_id, name, input: Value, raw: Option<Value> }`
     + `to_tool_call(&self) -> ToolCall`(id=provider_call_id, name, input)。
   - `ExternalToolResult { provider_call_id, status: ToolStatus, content: Vec<ContentBlock>,
     error: Option<String>, raw: Option<Value> }`
     + `from_tool_response(&ToolResponse)`(provider_call_id=tool_call_id,保留 status/content)
     + `from_tool_runtime_error(provider_call_id, &ToolRuntimeError)`(status=Error,
       error=Some(stable text),content=Text{stable text})。
   - `ExternalSessionInput::RespondToolResults { batch_id, results }`。
   - `ExternalSessionResult::PausedForToolCalls { session, batch_id, calls, observations }`。
   - imports:`model::content::ContentBlock`、`model::tool::{ToolCall, ToolResponse}`、
     `agent::tool::ToolRuntimeError`、`serde_json::Value`。
   - 新增测试:`external_tool_dto_roundtrips`、
     `external_tool_call_maps_to_provider_neutral_tool_call`、
     tool response/runtime-error -> ExternalToolResult 保真;snake_case 变体。
2. `src/agent/mod.rs` — re-export 新的 3 个 public DTO。
3. `src/agent/external/machine.rs` — `fold_session_result` 加 `PausedForToolCalls` arm。
   M1 machine 还不驱动 tool call(M2 才 wire),此 arm:observe 后 `fail_with` 明确诊断
   "external tool-call pauses are not yet wired (scheduled for M2)"。分阶段设计(非 workaround):
   机器目前不会 emit 会产生 tool-call pause 的请求,收到即协议异常;M2 会替换该 arm。
4. `crates/agent-testkit/src/assertions/external.rs` — `ExternalInputKind::RespondToolResults`、
   `ExternalResultKind::PausedForToolCalls` + input_kind/result_kind arm + rustdoc。
5. `tests/agent_external_real_e2e.rs` — `session_prompt` 加 `RespondToolResults` arm 返回 Protocol 错误。

## 不改
- `ExternalSessionResult::{Completed,PausedForInteraction,Failed}` 行为、observe 逐事件 dedup。
- machine cursor / state.rs(tool 相位是 M2)。
- 设计文档字段小差异(`error` 字段 doc 同步是 M1-4 review 的事,TODO 为权威)。

## 验证序列
1. cargo fmt --all -- --check
2. 聚焦:external_tool_dto_roundtrips / external_tool_call_maps_to_provider_neutral_tool_call
   + external_dto_roundtrips(回归)
3. cargo clippy --all-targets -- -D warnings
4. cargo test --all --all-targets (<=30min)
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6. git diff --check

## 进度
- [x] 读 TODO/PLAN/memory/源码,选定 M1-2,梳理全部 exhaustive match sites
- [x] mod.rs DTO + helper + enum 变体
- [x] machine fold arm(observe + fail_with,M2 替换)
- [x] testkit assertions kinds
- [x] real e2e match arm
- [x] mod.rs re-export
- [x] 测试 + 验证序列 1-6 全绿(lib 559 passed;clippy 0;doc OK)
- [x] TODO.md 标 [DONE] + 完成记录;待 commit
