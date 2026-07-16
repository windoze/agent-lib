# M1-3 — 新增 external subagent DTO 与 `RespondSubagent` / `PausedForSubagent`

**当前执行 = TODO.md 第一个未完成任务 = M1-3**(M1-1、M1-2 已 `[DONE]`,均已 commit)。

## 目标
在 external DTO 层落地 host subagent bridge 的 provider-neutral、serde-friendly 协议数据结构。
machine 本轮**不**真正驱动 subagent(subagent parity 是 M3);machine 收到 `PausedForSubagent`
即视为尚未接线的协议异常(observe 后 fail_with,分阶段设计非 workaround,M3 替换该 arm)。

## 权威来源冲突说明
- TODO.md M1-3(权威)采用嵌套 `ExternalSubagentRequest` 结构。
- docs/managed-external-agent.md §5.2 采用把 `spec_ref/brief/result_schema` 平铺进 `PausedForSubagent`,
  且 `RespondSubagent.output` 用 `SubagentOutput`。
- 依 TODO 权威,采用嵌套 request + serde-friendly `ExternalSubagentOutput`(推荐方案二,避免改
  `SubagentOutput` runtime 类型边界)。文档命名同步留给 M1-4 review(其任务明确要求)。

## 要新增 / 修改
1. `src/agent/external/mod.rs`
   - import `AgentSpecRef`、`SubagentOutput`(from `crate::agent`)。
   - `ExternalSubagentRequestId(String)` newtype(`#[serde(transparent)]`,`new`/`as_str`)。
   - `ExternalSubagentRequest { request_id, spec_ref, brief, result_schema: Option<Value>, raw: Option<Value> }`。
   - `ExternalSubagentOutput { summary: String, raw: Option<Value> }` + `From<SubagentOutput>`(raw=None)。
   - `ExternalSessionInput::RespondSubagent { request_id, output: ExternalSubagentOutput }`。
   - `ExternalSessionResult::PausedForSubagent { session, request: ExternalSubagentRequest, observations }`。
   - 测试:`external_subagent_dto_roundtrips`、snake_case 变体、`From<SubagentOutput>` 保真。
2. `src/agent/mod.rs` — re-export `ExternalSubagentRequest`、`ExternalSubagentRequestId`、`ExternalSubagentOutput`。
3. `src/agent/external/machine.rs` — `fold_session_result` 加 `PausedForSubagent` arm(observe + fail_with,M3 替换)。
4. `crates/agent-testkit/src/assertions/external.rs` — `ExternalInputKind::RespondSubagent`、
   `ExternalResultKind::PausedForSubagent` + input_kind/result_kind arm + rustdoc。
5. `tests/agent_external_real_e2e.rs` — `session_prompt` 加 `RespondSubagent` arm 返回 Protocol 错误。

## 不改
- `RequirementKind::NeedSubagent` 的 serde shape、`SubagentOutput` 类型边界(不给它加 serde)。
- machine cursor / state.rs(subagent 相位是 M3)。

## 验证序列
1. cargo fmt --all -- --check
2. 聚焦:external_subagent_dto_roundtrips / external_dto_roundtrips /
   accepts_matrix_pairs_each_kind_with_its_result_only
3. cargo clippy --all-targets -- -D warnings
4. cargo test --all --all-targets (<=30min)
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6. git diff --check

## 进度
- [x] 读 TODO/PLAN/memory/源码,选定 M1-3,梳理全部 exhaustive match sites
- [x] mod.rs DTO + helper + enum 变体 + From
- [x] machine fold arm(observe + fail_with,M3 替换)
- [x] testkit assertions kinds
- [x] real e2e match arm
- [x] mod.rs re-export
- [x] 测试 + 验证序列 1-6 全绿(lib 562 passed;clippy 0;doc OK;修 1 处 intra-doc link)
- [x] TODO.md 标 [DONE] + 完成记录;待 commit
