# TODO：Managed External Agent 任务单

> 依据 [`PLAN.md`](PLAN.md) 与
> [`docs/managed-external-agent.md`](docs/managed-external-agent.md)。任务按实现顺序编号
> (`M<里程碑>-<序号>`);coding agent 每次只执行首个标题带 `[TODO]` 的任务,完成后把该标题中的
> `[TODO]` 改为 `[DONE]`,并在任务末尾补充「完成记录」。

通用约束:

- `ExternalAgentMachine` 必须保持 sans-io。不得在 machine 里启动进程、读写 pipe、调用 CLI、执行工具、
  访问网络或询问用户。
- 不新增 effect family。external runtime 的 tool、interaction、subagent 决策点必须映射到现有
  `NeedTool`、`NeedInteraction`、`NeedSubagent`,再通过 `NeedExternalSession` 回灌。
- 默认测试必须离线。真实 Claude Code / Codex / OpenCode / DeepSeek API 测试必须 `#[ignore]`。
- 任何 public API 新增都要有 rustdoc,并通过 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- DTO serde 形状必须有 round-trip 测试；runtime adapter 私有 raw schema 不得进入稳定 public API。
- 任何可能包含 secret、prompt transcript、tool input 的日志/错误必须脱敏或只输出稳定诊断。

默认完整验证序列:

1. `cargo fmt --all -- --check`
2. 聚焦测试(每个任务列出)
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

---

## Milestone 1 — external session 协议扩展与 sequenced observations

目标:先把 managed external agent 需要的协议数据结构落地,但不改变 machine 行为。所有新增 DTO 都在
`src/agent/external/mod.rs` 或其子模块中保持 provider-neutral、serde-friendly。

### [DONE] M1-1 新增 `ExternalObservedEvent` 并把 observations 改成 sequenced payload

**上下文**:

- 当前 `ExternalSessionResult::{Completed,PausedForInteraction,Failed}.observations` 是
  `Vec<ExternalAgentEvent>`。
- `ExternalAgentMachine::observe` 在 `src/agent/external/machine.rs` 中用
  `ExternalSessionRef.last_event_seq` 做粗粒度 dedup,无法逐事件 replay。
- 设计文档 §5.5 要求 `ExternalObservedEvent { seq, event }` 成为 buffered observation 的标准形状。

**做什么**:

- 在 `src/agent/external/mod.rs` 新增:

  ```rust
  pub struct ExternalObservedEvent {
      pub seq: u64,
      pub event: ExternalAgentEvent,
  }
  ```

- 为测试/fixture 提供轻量 helper,例如
  `ExternalObservedEvent::new(seq, event)` 和
  `ExternalObservedEvent::unsequenced_for_tests(events)` 或等价函数。helper 不得用于生产 dedup 逻辑。
- 把 `ExternalSessionResult` 三个现有变体的 `observations` 从 `Vec<ExternalAgentEvent>` 改成
  `Vec<ExternalObservedEvent>`。
- 更新 `collect_file_patch_artifacts` 的调用侧策略:函数本身可以继续接受 `&[ExternalAgentEvent]`,
  但需要新增一个从 `&[ExternalObservedEvent]` 收集 patch artifact 的 helper,避免 adapter 手动 map。
- 更新 `ExternalEventSink`:
  - live sink 是否接收 sequenced event 二选一。推荐新增 `ExternalObservedEvent` 版本,同时保留旧
    `ExternalAgentEvent` sink 作为便捷层或改成接收 `ExternalObservedEvent`。
  - 文档必须说明 live sink 是旁路,buffered observations 仍是 exact-once source of truth。
- 更新全部测试 fixture:
  - `src/agent/external/mod.rs` 内 DTO round-trip。
  - `src/agent/external/machine/tests.rs` 的 `observation_batch`、`completed_with`、`paused_with`。
  - `src/agent/drive.rs` 测试中的 `external_session_result` fixture。
  - `src/agent/requirement.rs` 测试中的 external fixture。

**验证条件**:

- `serde_json` round-trip 覆盖 `ExternalObservedEvent` 和含 observations 的 `Completed` /
  `PausedForInteraction` / `Failed`。
- `ExternalAgentMachine` 的 observation replay 测试仍通过,且能证明:
  - seq 大于已消费 seq 的事件会被 emit 成 `Notification::ExternalAgent`。
  - seq 小于等于已消费 seq 的事件不会重复 emit。
- 聚焦测试:
  - `cargo test -p agent-lib external_dto_roundtrips`
  - `cargo test -p agent-lib external_agent_emits_observation_notifications`
  - `cargo test -p agent-lib discard_sink_accepts_and_drops_events`
- 完整验证序列 1-6 全过。

**完成记录**(2026-07-17):

- `src/agent/external/mod.rs`:新增 `pub struct ExternalObservedEvent { seq: u64, event:
  ExternalAgentEvent }`,derive `Clone/Debug/PartialEq/Eq/Serialize/Deserialize`;附
  `ExternalObservedEvent::new(seq, event)` 与仅供 fixture 的
  `ExternalObservedEvent::unsequenced_for_tests(Vec<ExternalAgentEvent>)`(enumerate 赋 seq,
  rustdoc 明确禁止用于生产 dedup)。
- 三个 `ExternalSessionResult` 变体(`Completed`/`PausedForInteraction`/`Failed`)的
  `observations` 由 `Vec<ExternalAgentEvent>` 改为 `Vec<ExternalObservedEvent>`。
- 新增 `collect_file_patch_artifacts_from_observed(&[ExternalObservedEvent])`,保留旧
  `collect_file_patch_artifacts(&[ExternalAgentEvent])`,让 adapter 免手动 map。
- `machine.rs`:`observe` 改为逐事件 dedup(`filter(|o| consumed.is_none_or(|c| o.seq > c))`),
  去掉 `incoming_seq` 参数,consumed 仍在存入 incoming session 之前读
  `state.session().last_event_seq`;`fold_session_result` 三处调用同步更新;module/方法 doc 更新。
- `machine/tests.rs`:重写 `external_agent_emits_observation_notifications`,新增 PARTIAL
  overlap 用例(consumed=3,batch seqs 3..=5 → 仅 seq 4/5 replay)证明逐事件 replay;
  `completed_with`/`paused_with` 改收 `Vec<ExternalObservedEvent>`,新增 `sequenced` helper。
- `sink.rs`:trait 签名保持不变(sequenced live sink 升级留给 M4-1),文档改为说明 buffered
  `ExternalObservedEvent` observations 是 exact-once 真源、按 `seq` 逐事件去重;
  `discard_sink_accepts_and_drops_events` 不改并通过。
- 其余 construction sites 同步:`src/agent/mod.rs` 导出新符号;`crates/agent-testkit/src/external.rs`
  的 `completed/permission_pause/failed` 用 `unsequenced_for_tests` 包装并修正 `matches!` 模式;
  `tests/agent_external_real_e2e.rs` 的 Completed 观测显式赋 seq 1..=3;drive.rs/requirement.rs/
  assertions 的 `Vec::new()` 空 vec 自适应。
- 新增测试:`external_observed_event_roundtrips`(DTO round-trip + seq 保序)、
  `collect_file_patch_artifacts_from_observed_ignores_seqs_and_non_patches`。
- 验证:`cargo fmt --all -- --check` FMT_OK;聚焦 6 tests 全过(external_dto_roundtrips /
  external_agent_emits_observation_notifications / discard_sink_accepts_and_drops_events /
  external_observed_event_roundtrips / collect_file_patch_artifacts_from_observed /
  collect_file_patch_artifacts_keeps_only_patches_in_order);`cargo clippy --all-targets -- -D
  warnings` 0 warning;`cargo test --all --all-targets` 全绿(agent-lib lib 554 passed,其余各
  test binary 0 failed);`cargo test --all --doc` 7+12+2 passed(1 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过;`git diff --check` clean。

### [DONE] M1-2 新增 external tool DTO 与 `RespondToolResults` / `PausedForToolCalls`

**上下文**:

- 当前 `ExternalSessionRequest.tools` 已携带 provider-neutral `Vec<Tool>`,但 runtime 无法向 host 发起
  tool call。
- host 工具执行已有 `NeedTool { call_id, call }` 和 `RequirementResult::Tool(Result<ToolResponse, ToolRuntimeError>)`。
- `ToolCall` 在 `crate::model::tool` 中含 provider call id/name/input。framework-level `ToolCallId`
  由 `ToolExecutionIds::tool_call_id(&ToolCall)` 提供。

**做什么**:

- 在 external DTO 层新增:

  ```rust
  pub struct ExternalToolBatchId(String);

  pub struct ExternalToolCall {
      pub provider_call_id: String,
      pub name: String,
      pub input: serde_json::Value,
      pub raw: Option<serde_json::Value>,
  }

  pub struct ExternalToolResult {
      pub provider_call_id: String,
      pub status: ToolStatus,
      pub content: Vec<ContentBlock>,
      pub error: Option<String>,
      pub raw: Option<serde_json::Value>,
  }
  ```

  字段可按代码风格微调,但必须保留 provider call id、tool name、input、status/error、raw escape hatch。
- 扩展 `ExternalSessionInput`:

  ```rust
  RespondToolResults {
      batch_id: ExternalToolBatchId,
      results: Vec<ExternalToolResult>,
  }
  ```

- 扩展 `ExternalSessionResult`:

  ```rust
  PausedForToolCalls {
      session: ExternalSessionRef,
      batch_id: ExternalToolBatchId,
      calls: Vec<ExternalToolCall>,
      observations: Vec<ExternalObservedEvent>,
  }
  ```

- 为 `ExternalToolCall` 提供转换到 `ToolCall` 的 helper:
  - `ToolCall.id = provider_call_id`。
  - `ToolCall.name = name`。
  - `ToolCall.input = input`。
- 为 `ToolResponse` / `ToolRuntimeError` 到 `ExternalToolResult` 提供 helper:
  - 成功时保持 `ToolResponse.status` 和 `content`。
  - `ToolRuntimeError` 时生成 `ToolStatus::Error` 或当前模型已有错误状态；若 `ToolStatus` 没有 error
    语义,在 `error` 字段中表达并在文档里说明。
- 更新 serde round-trip 测试和 snake_case 变体测试。

**验证条件**:

- `ExternalToolBatchId`、`ExternalToolCall`、`ExternalToolResult` 都 derive/实现 `Clone, Debug,
  PartialEq, Eq, Serialize, Deserialize`。
- `ExternalSessionInput::RespondToolResults` 和 `ExternalSessionResult::PausedForToolCalls` serde
  round-trip。
- helper 测试覆盖:
  - `ExternalToolCall -> ToolCall` 保留 provider call id/name/input。
  - `ToolResponse -> ExternalToolResult` 保留 status/content。
  - `ToolRuntimeError -> ExternalToolResult` 不丢失稳定错误文本。
- 聚焦测试:
  - `cargo test -p agent-lib external_tool_dto_roundtrips`
  - `cargo test -p agent-lib external_tool_call_maps_to_provider_neutral_tool_call`
- 完整验证序列 1-6 全过。

**完成记录**(2026-07-17):

- `src/agent/external/mod.rs`:新增三个 provider-neutral DTO,均 derive
  `Clone/Debug/PartialEq/Eq/Serialize/Deserialize`:
  - `ExternalToolBatchId(String)`(`#[serde(transparent)]`,`new(impl Into<String>)` +
    `as_str()`);
  - `ExternalToolCall { provider_call_id, name, input: Value, raw: Option<Value> }`,附
    `to_tool_call(&self) -> ToolCall`(id=provider_call_id、name、input 原样,丢弃 raw escape hatch);
  - `ExternalToolResult { provider_call_id, status: ToolStatus, content: Vec<ContentBlock>,
    error: Option<String>, raw: Option<Value> }`,附
    `from_tool_response(&ToolResponse)`(provider_call_id=tool_call_id,保留 status/content,
    error=None)与 `from_tool_runtime_error(provider_call_id, &ToolRuntimeError)`(status=Error,
    稳定诊断文本同时写入 `error` 与一个 `ContentBlock::Text`,不丢失)。
- 扩展 `ExternalSessionInput`:新增 `RespondToolResults { batch_id, results }`(snake_case
  `respond_tool_results`)。
- 扩展 `ExternalSessionResult`:新增 `PausedForToolCalls { session, batch_id, calls,
  observations }`(snake_case `paused_for_tool_calls`,`observations` 用
  `Vec<ExternalObservedEvent>` 且 `#[serde(default)]`)。
- imports 同步:`model::content::ContentBlock`、`model::tool::{ToolCall, ToolResponse}`、
  `agent::tool::ToolRuntimeError`、`serde_json::Value`。
- `src/agent/mod.rs`:re-export `ExternalToolBatchId`、`ExternalToolCall`、`ExternalToolResult`。
- `src/agent/external/machine.rs`:`fold_session_result` 补 `PausedForToolCalls` arm。M1 machine
  尚未驱动 tool-call(machine tool parity 是 M2,PLAN.md 已显式排期);此 arm 先 `observe`
  逐事件 replay(§5.5),再 `fail_with` 明确诊断 "external tool-call pauses are not yet driven by
  the machine (scheduled for milestone 2)"。属分阶段设计而非 workaround:M1 machine 不会 emit 会
  触发 tool-call pause 的请求,收到即协议异常;M2 会替换该 arm 为 `NeedTool` batch。
- `crates/agent-testkit/src/assertions/external.rs`:`ExternalInputKind::RespondToolResults`、
  `ExternalResultKind::PausedForToolCalls` + `input_kind`/`result_kind` 对应 arm + rustdoc。
- `tests/agent_external_real_e2e.rs`:`session_prompt` 补 `RespondToolResults` arm 返回
  `ExternalAgentError::Protocol`。
- 新增测试(`src/agent/external/mod.rs`):`external_tool_dto_roundtrips`(PausedForToolCalls /
  RespondToolResults round-trip + batch id serde-transparent 断言)、
  `external_tool_input_and_result_variants_serialize_snake_case`、
  `external_tool_call_maps_to_provider_neutral_tool_call`、
  `tool_response_maps_to_external_result_preserving_status_and_content`(四态 status)、
  `tool_runtime_error_maps_to_external_result_without_losing_error_text`(error 文本 + content 双写
  + round-trip)。
- 验证:`cargo fmt --all -- --check` FMT_OK;聚焦测试全过(external lib 99 passed,含 5 新用例);
  `cargo clippy --all-targets -- -D warnings` 0 warning;`cargo test --all --all-targets` 全绿
  (agent-lib lib 559 passed = 554+5,其余各 test binary 0 failed,ignored 为真实 e2e);
  `cargo test --all --doc` 7+12+2 passed(1 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过;`git diff --check` clean。

### [DONE] M1-3 新增 external subagent DTO 与 `RespondSubagent` / `PausedForSubagent`

**上下文**:

- `NeedSubagent` 现有 payload 在 `src/agent/requirement.rs`: `AgentSpecRef`、`brief: Interaction`、
  `result_schema: Option<Value>`。
- `DrivingSubagentHandler` 在 `src/agent/drive/subagent.rs` 负责 nested drain、depth、budget、cancel、
  pop outward。
- 设计文档允许 spawn 通过 tool bridge 特判,但为了 runtime adapter 能表达显式 child task,协议层应先有
  provider-neutral subagent 决策点。

**做什么**:

- 在 external DTO 层新增:

  ```rust
  pub struct ExternalSubagentRequestId(String);

  pub struct ExternalSubagentRequest {
      pub request_id: ExternalSubagentRequestId,
      pub spec_ref: AgentSpecRef,
      pub brief: Interaction,
      pub result_schema: Option<serde_json::Value>,
      pub raw: Option<serde_json::Value>,
  }
  ```

- 扩展 `ExternalSessionInput`:

  ```rust
  RespondSubagent {
      request_id: ExternalSubagentRequestId,
      output: SubagentOutput,
  }
  ```

- 扩展 `ExternalSessionResult`:

  ```rust
  PausedForSubagent {
      session: ExternalSessionRef,
      request: ExternalSubagentRequest,
      observations: Vec<ExternalObservedEvent>,
  }
  ```

- 注意 `SubagentOutput` 当前不 derive serde,因为它在 `RequirementResult` runtime half 中不持久化。
  若要让 `ExternalSessionInput` 保持 serde-friendly,需要二选一:
  - 给 `SubagentOutput` 补 `Serialize` / `Deserialize`。
  - 或新增 serde-friendly `ExternalSubagentOutput` 并提供 `From<SubagentOutput>`。
  推荐第二种,避免改变现有 runtime result 类型边界。
- 更新 `external_dto_roundtrips` 覆盖 subagent decision point。

**验证条件**:

- DTO round-trip 覆盖 `PausedForSubagent` 和 `RespondSubagent`。
- 不改变 `RequirementKind::NeedSubagent` 的 serde shape。
- `cargo test -p agent-lib requirement` 中 accepts matrix 仍全绿。
- 聚焦测试:
  - `cargo test -p agent-lib external_subagent_dto_roundtrips`
  - `cargo test -p agent-lib accepts_matrix_pairs_each_kind_with_its_result_only`
- 完整验证序列 1-6 全过。

**完成记录**(2026-07-17):

- `src/agent/external/mod.rs`:新增三个 provider-neutral、serde-friendly DTO,均 derive
  `Clone/Debug/PartialEq/Eq/Serialize/Deserialize`:
  - `ExternalSubagentRequestId(String)`(`#[serde(transparent)]`,`new(impl Into<String>)` +
    `as_str()`);
  - `ExternalSubagentRequest { request_id, spec_ref: AgentSpecRef, brief: Interaction,
    result_schema: Option<Value>, raw: Option<Value> }`(两个可选字段 `#[serde(default,
    skip_serializing_if)]`);
  - `ExternalSubagentOutput { summary: String, raw: Option<Value> }`,附 `From<SubagentOutput>`
    (保留 summary,`raw=None`)。采用 TODO 推荐的方案二:新增 serde-friendly output DTO,而非给
    runtime-only 的 `SubagentOutput` 补 serde,避免改动其 `RequirementResult` 边界。
- 扩展 `ExternalSessionInput`:新增 `RespondSubagent { request_id, output: ExternalSubagentOutput }`
  (snake_case `respond_subagent`)。
- 扩展 `ExternalSessionResult`:新增 `PausedForSubagent { session, request: ExternalSubagentRequest,
  observations }`(snake_case `paused_for_subagent`,`observations` 用 `Vec<ExternalObservedEvent>` 且
  `#[serde(default)]`)。
- 权威冲突处理:TODO.md(权威)采用嵌套 `ExternalSubagentRequest` 结构,与 `docs/managed-external-agent.md`
  §5.2 的平铺 `spec_ref/brief/result_schema` + `output: SubagentOutput` 不同。依 TODO 实现;文档命名
  同步明确划归 M1-4 review(其任务体已要求"如果实现中采用了不同命名,同步更新文档")。
- imports 同步:`crate::agent::{AgentSpecRef, SubagentOutput}`。
- `src/agent/mod.rs`:re-export `ExternalSubagentOutput`、`ExternalSubagentRequest`、
  `ExternalSubagentRequestId`。
- `src/agent/external/machine.rs`:`fold_session_result` 补 `PausedForSubagent` arm。M1 machine
  尚未驱动 subagent(subagent parity 是 M3,PLAN.md 已显式排期);此 arm 先 `observe` 逐事件
  replay(§5.5),再 `fail_with` 明确诊断 "external subagent pauses are not yet driven by the
  machine (scheduled for milestone 3)"。属分阶段设计而非 workaround:M1 machine 不会 emit 会触发
  subagent pause 的请求,收到即协议异常;M3 会替换该 arm 为 `NeedSubagent` 桥接。
- `crates/agent-testkit/src/assertions/external.rs`:`ExternalInputKind::RespondSubagent`、
  `ExternalResultKind::PausedForSubagent` + `input_kind`/`result_kind` 对应 arm + rustdoc。
- `tests/agent_external_real_e2e.rs`:`session_prompt` 补 `RespondSubagent` arm 返回
  `ExternalAgentError::Protocol`。
- 新增测试(`src/agent/external/mod.rs`):`external_subagent_dto_roundtrips`(PausedForSubagent /
  RespondSubagent round-trip、含无可选字段的最小 request、request id serde-transparent 断言)、
  `external_subagent_input_and_result_variants_serialize_snake_case`、
  `subagent_output_maps_from_host_result_preserving_summary`(`From<SubagentOutput>` 保真 + round-trip)。
- `NeedSubagent` 的 serde shape 与 `SubagentOutput` 类型边界均未改动;`accepts_matrix_pairs_each_kind_with_its_result_only`
  回归全绿。
- 验证:`cargo fmt --all -- --check` FMT_OK;聚焦测试全过(lib 13 passed,含 3 新用例:subagent
  round-trip / snake_case / From);`cargo clippy --all-targets -- -D warnings` 0 warning;
  `cargo test --all --all-targets` 全绿(agent-lib lib 562 passed = 559+3,其余各 test binary 0 failed,
  ignored 为真实 e2e);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过(修正一处
  `RequirementResult` intra-doc link 为全限定路径);`git diff --check` clean。

### [DONE] M1-4 Review：协议层完整性与兼容性检查

**上下文**:

M1 已经修改 public DTO,这是后续 machine/runtime adapter 的基础。阶段 review 要在继续实现 machine 前确认
serde、rustdoc、导出面和设计文档一致。

**做什么**:

- 对照 [`docs/managed-external-agent.md`](docs/managed-external-agent.md) §5,确认以下类型全部存在并有
  rustdoc:
  - `ExternalObservedEvent`
  - `ExternalToolBatchId`
  - `ExternalToolCall`
  - `ExternalToolResult`
  - `ExternalSubagentRequestId`
  - external subagent request/output DTO
  - `ExternalSessionInput::{RespondToolResults,RespondSubagent}`
  - `ExternalSessionResult::{PausedForToolCalls,PausedForSubagent}`
- 检查 `src/agent/external/mod.rs` 的 `pub use`/公开路径是否符合 crate 现有风格。
- 检查所有新增 DTO 是否保留 raw/extra escape hatch,但不泄露 runtime 私有 schema 为稳定 typed API。
- 检查 `docs/managed-external-agent.md` 是否需要微调名称,如果实现中采用了不同命名,同步更新文档。

**验证条件**:

- `cargo test -p agent-lib external_dto_roundtrips`
- `cargo test -p agent-lib requirement`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
- 完整验证序列 1-6 全过。
- 在本任务完成记录中列出 M1 public API diff 摘要。

**完成记录**(2026-07-17):

本任务是 M1 阶段 review,核查协议层完整性并同步文档命名。**无 `.rs` 改动**,仅改文档。

*核查结论*(逐项对照 TODO.md 要求):

- **类型存在性 + rustdoc**:8 类目标类型全部存在于 `src/agent/external/mod.rs` 且带完整 rustdoc
  (均引用 design §5.x):`ExternalObservedEvent`、`ExternalToolBatchId`、`ExternalToolCall`、
  `ExternalToolResult`、`ExternalSubagentRequestId`、`ExternalSubagentRequest` /
  `ExternalSubagentOutput`(external subagent request/output DTO)、
  `ExternalSessionInput::{RespondToolResults,RespondSubagent}`、
  `ExternalSessionResult::{PausedForToolCalls,PausedForSubagent}`。
- **公开路径风格**:`src/agent/external/mod.rs` 的 `pub use` 分块(dispatch/escalation/machine/…)与
  `src/agent/mod.rs` 的 `pub use external::{…}`(字母序)一致,新符号全部 re-export,风格符合 crate 现有约定。
- **raw/extra escape hatch**:`ExternalToolCall`/`ExternalToolResult`/`ExternalSubagentRequest`/
  `ExternalSubagentOutput` 均带 `raw: Option<Value>`(`#[serde(default, skip_serializing_if]`);
  `ExternalToolCall::to_tool_call` 主动丢弃 `raw` 不泄露到稳定 tool 路径;`raw` 是 `serde_json::Value`,
  不把任何 runtime 私有 typed schema 暴露为稳定 public API。符合 §5.3 与通用约束。
- **文档命名同步**(M1-3 显式 defer 到本 review):实现相对 docs §5 的偏差已全部同步:
  - §5.1 `RespondSubagent.output`:`SubagentOutput` → `ExternalSubagentOutput`,并补说明为何用 serde DTO。
  - §5.2 `PausedForSubagent`:平铺 `request_id/spec_ref/brief/result_schema` → 嵌套
    `request: ExternalSubagentRequest`;取舍表下把「推荐首版 spawn_agent tool call」改为「已定采用专门
    `PausedForSubagent` 变体,spawn_agent tool-bridge 特判留给 M3(§8.3)」。
  - §5.3 `ExternalToolResult`:字段对齐实现顺序(`status` 先于 `content`)并补 `error: Option<String>`;
    新增 `ExternalSubagentRequest` / `ExternalSubagentOutput` 结构定义与映射原则说明。
  - §21 M1 milestone 行:「决定 spawn_agent 走 tool bridge 还是专门 PausedForSubagent」→ 记录已选专门变体。

*M1 public API diff 摘要*(M1-1 + M1-2 + M1-3 相对 M1 前的净增,均在 `crate::agent` 与
`crate::agent::external` 导出):

- 新增类型:
  - `ExternalObservedEvent { seq: u64, event: ExternalAgentEvent }`
    + `new` / `unsequenced_for_tests`(M1-1)。
  - `ExternalToolBatchId(String)`(`#[serde(transparent)]`)+ `new` / `as_str`(M1-2)。
  - `ExternalToolCall { provider_call_id, name, input, raw }` + `to_tool_call`(M1-2)。
  - `ExternalToolResult { provider_call_id, status, content, error, raw }`
    + `from_tool_response` / `from_tool_runtime_error`(M1-2)。
  - `ExternalSubagentRequestId(String)`(`#[serde(transparent)]`)+ `new` / `as_str`(M1-3)。
  - `ExternalSubagentRequest { request_id, spec_ref, brief, result_schema, raw }`(M1-3)。
  - `ExternalSubagentOutput { summary, raw }` + `impl From<SubagentOutput>`(M1-3)。
- 新增 enum 变体:
  - `ExternalSessionInput::RespondToolResults { batch_id, results }`(M1-2)。
  - `ExternalSessionInput::RespondSubagent { request_id, output: ExternalSubagentOutput }`(M1-3)。
  - `ExternalSessionResult::PausedForToolCalls { session, batch_id, calls, observations }`(M1-2)。
  - `ExternalSessionResult::PausedForSubagent { session, request, observations }`(M1-3)。
- 变更(breaking)字段形状:
  - `ExternalSessionResult::{Completed,PausedForInteraction,Failed}.observations`:
    `Vec<ExternalAgentEvent>` → `Vec<ExternalObservedEvent>`(M1-1)。
- 新增自由函数:
  - `collect_file_patch_artifacts_from_observed(&[ExternalObservedEvent]) -> Vec<ExternalArtifactRef>`
    (M1-1;保留旧 `collect_file_patch_artifacts`)。

*验证*:

- `cargo fmt --all -- --check` FMT_OK。
- 聚焦:`cargo test -p agent-lib --lib external_dto_roundtrips`(1 passed)、
  `cargo test -p agent-lib --lib requirement`(40 passed)。
- `cargo clippy --all-targets -- -D warnings` 0 warning。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过(含新增/改动 intra-doc link 无破损)。
- `cargo test --all --all-targets`:**复用** M1-3(commit cf37a9c)的全绿结果 —— 自那次全量 run 起仅
  `TODO.md` / `docs/*.md` / `memory/*.md` 变更,无任何 `.rs` 改动(`git status` 确认无 `.rs`),按文档-only
  复用规则跳过重跑。
- `git diff --check` clean。

---

## Milestone 2 — `ExternalAgentMachine` tool parity

目标:当 runtime 暂停在 `PausedForToolCalls` 时,`ExternalAgentMachine` 能发出 host `NeedTool` batch,
收齐结果后用 `NeedExternalSession(RespondToolResults)` 回灌 runtime。

### [DONE] M2-1 扩展 `ExternalAgentCursor` 与 machine scratch 支持 pending tool batch

**上下文**:

- `src/agent/external/state.rs` 当前 `ExternalAgentCursor` 只有 `AwaitingSession` /
  `AwaitingInteraction`。
- `src/agent/external/machine.rs` 当前 `Awaiting` enum 也只有 `Session` / `Interaction`。
- `drain` 可能按完成顺序 resume batch 中的 `NeedTool`,所以 cursor/scratch 必须能按 requirement id 路由。

**做什么**:

- 在 `ExternalAgentCursor` 新增:

  ```rust
  AwaitingTool {
      batch_id: ExternalToolBatchId,
      requirements: ToolWaitRequirements,
  }
  ```

  如果不想复用 `ToolWaitRequirements`,可新增 external 专用 serializable map,但必须能从 cursor 恢复
  outstanding requirement ids。
- 在 `ExternalAgentMachine` 新增非序列化 scratch:

  ```rust
  pending_tool_batch: Option<PendingExternalToolBatch>
  ```

  其中至少包含 `batch_id`、原始 `ExternalToolCall` 列表、`provider_call_id -> RequirementId`、
  `RequirementId -> provider_call_id`、已收集 `ExternalToolResult`。
- 更新 `initial_loop_cursor` / `cursor_label` / `ExternalAgentCursor::requirement()`:
  - `requirement()` 可能要变成返回多个 requirement,或新增 `requirements()`。
  - driver-facing `LoopCursor` 对 batch 应使用 `LoopCursor::awaiting_tool(...)` 或现有可表达 batch 的
    cursor,以便 `pending_requirement_ids()` 返回全部 tool requirement id。
- 状态恢复策略:
  - cursor 必须持久化 outstanding ids。
  - scratch 无法从 state 完整恢复时,machine 应在 restore 后遇到 tool resume 时给出 classified error,
    或新增 `rebuild_scratch_from_state` 所需字段。优先把足够 metadata 放入 cursor,避免 mid-turn restore
    无法继续。

**验证条件**:

- `ExternalAgentCursor` serde round-trip 覆盖 `AwaitingTool`。
- `initial_loop_cursor` 对 `AwaitingTool` 不得错误显示 terminal；若无法重建完整 streaming view,测试要明确
  当前降级行为并给出后续任务。
- `cargo test -p agent-lib external_agent_state_cursor_variants_round_trip`
- `cargo test -p agent-lib external_agent_state_serde_round_trips_through_conversation_snapshot`
- 完整验证序列 1-6 全过。

**完成记录**(2026-07-17):

本任务只做 M2 tool parity 的**数据结构脚手架**:cursor 新增 tool-batch 变体 + machine 新增非序列化
scratch。`PausedForToolCalls` fold(M2-2)与 result 收集回灌(M2-3)保持未动。

*改动*:

- `src/agent/external/state.rs`:
  - `ExternalAgentCursor` 新增变体
    `AwaitingTool { batch_id: ExternalToolBatchId, requirements: ToolWaitRequirements }`(含 rustdoc)。
    cursor 只持久化**可恢复寻址**:runtime batch token + `ToolCallId -> RequirementId` 绑定,
    供 restore 后重建 pending-requirement registry。
  - `requirement()` 新增 `AwaitingTool => None`(batch 无单一 requirement);新增
    `requirements() -> Option<&ToolWaitRequirements>` 暴露整张绑定;新增
    `has_outstanding_requirement()`(覆盖 Session/Interaction/Tool 三个 awaiting 变体)。
  - 测试 `external_agent_state_cursor_variants_round_trip` 覆盖 `AwaitingTool` serde round-trip,
    并断言 `requirement()`=None、`requirements()`=Some、`has_outstanding_requirement()`=true。
- `src/agent/external/machine.rs`:
  - 新增私有非序列化 scratch `PendingExternalToolBatch { batch_id, calls,
    call_to_requirement(provider_call_id→RequirementId), requirement_to_call(RequirementId→provider_call_id),
    results }`;machine 新增字段 `pending_tool_batch: Option<PendingExternalToolBatch>`(`new` 初始化 None)。
    该 scratch 构造在 M2-2、drain 在 M2-3,故用 **`#[expect(dead_code, reason=…)]`** 标注 staging
    (自清理:M2-2/M2-3 消费后 expectation 未兑现即报警强制移除;codebase 目前零 `allow`,选用 `expect`
    保持「无 warning」约定,且非 spec 绕过,只是明确的前置声明脚手架)。
  - `initial_loop_cursor` / `cursor_label` 新增 `AwaitingTool` 分支。
  - `abandon` 用 `has_outstanding_requirement()` 取代 `requirement().is_some()`,前向正确:tool-batch
    abandon 也需 `mark_cleanup_required`。
  - 新增测试 `awaiting_tool_cursor_restores_without_a_terminal_view`:snapshot→restore 后 cursor 保留
    batch 寻址(`requirements()` 可取回),而 driver-facing `LoopCursor` 降级为**非 terminal 的 Idle**
    (不误报 Done/Error),`pending_requirement_ids()` 为空。

*降级行为记录*(TODO.md 要求):`initial_loop_cursor(AwaitingTool)` 返回 `LoopCursor::Idle`(非 terminal),
与 `AwaitingSession`/`AwaitingInteraction` 一致 —— mid-turn restore 无 step scratch 重建 streaming/tool-wait
view。**后续任务**:此为 `PLAN.md`「恢复 mid-turn scratch」风险项已跟踪(把 pending tool/subagent facts 提入
serializable cursor + 补 restore 测试);无需新建 TODO 任务,已在测试注释与本记录中指向。

*验证*(完整序列 1-6 全过):

- `cargo fmt --all -- --check` FMT_OK。
- 聚焦:`external_agent_state_cursor_variants_round_trip`、
  `external_agent_state_serde_round_trips_through_conversation_snapshot`、
  `awaiting_tool_cursor_restores_without_a_terminal_view`(3 passed）。
- `cargo clippy --all-targets -- -D warnings` 0 warning(`#[expect(dead_code)]` 在 lib 与 test 两种 cfg
  下均兑现,无 unfulfilled-expectation)。
- `cargo test --all --all-targets`:789 passed / 0 failed。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过(新增 intra-doc link 无破损)。
- `git diff --check` clean。

### [DONE] M2-2 将 `PausedForToolCalls` 折成 `NeedTool` batch

**上下文**:

- `ExternalAgentMachine::fold_session_result` 当前只 match `Completed` / `PausedForInteraction` /
  `Failed`。
- `block_on_session` 已能构造 `NeedExternalSession`。
- `RequirementKind::NeedTool` 需要 framework-level `ToolCallId` 和 provider-neutral `ToolCall`。

**做什么**:

- 给 `ExternalAgentMachine` 注入 `ToolExecutionIds`:
  - 构造函数可保持兼容,新增 builder 如 `with_tool_execution_ids(Arc<dyn ToolExecutionIds>)`。
  - 默认使用 `NoToolExecutionIds`,如果 runtime 发起 tool call 但未注入 ids,进入 error cursor,错误文本要稳定。
- 在 `fold_session_result` 新增 `PausedForToolCalls` 分支:
  - `observe` sequenced observations 并输出 notifications。
  - `state.set_session(Some(session))`。
  - 每个 `ExternalToolCall` 转 `ToolCall`。
  - 用 `tool_ids.tool_call_id(&call)` 获取 `ToolCallId`。
  - 为每个 call 用 `requirement_ids.next_requirement_id(RequirementKindTag::Tool)` 获取 `RequirementId`。
  - 发出一个 batch 的 `RequirementKind::NeedTool { call_id, call }`。
  - 设置 `ExternalAgentCursor::AwaitingTool` 与 driver-facing awaiting-tool cursor。
- 注意不要把 tool result 写入 `Conversation`;external runtime 才是 tool result 的消费者。machine 只把
  host tool result 转成 `ExternalToolResult` 再回灌 runtime。

**验证条件**:

- machine unit 新增测试:
  - `external_tool_pause_emits_need_tool_batch`
  - 断言 requirements 数量等于 calls 数量。
  - 断言每个 `NeedTool.call.id` 等于 `ExternalToolCall.provider_call_id`。
  - 断言 `machine.cursor().pending_requirement_ids()` 含全部 tool requirement id。
  - 断言 pending Conversation 仍打开但未提交。
- 未注入 `ToolExecutionIds` 时:
  - `PausedForToolCalls` 使 machine 进入 `LoopCursorKind::Error`。
  - pending turn 被 discard。
- 聚焦测试:
  - `cargo test -p agent-lib external_tool_pause_emits_need_tool_batch`
  - `cargo test -p agent-lib external_tool_pause_without_tool_ids_fails`
- 完整验证序列 1-6 全过。

**完成记录**(2026-07-17):

本任务把 `PausedForToolCalls` 折成 host `NeedTool` batch(**producer**);result 收齐回灌 `RespondToolResults`
(drain)仍是 M2-3。M2-1 已落地的 `AwaitingTool` cursor 变体与 `PendingExternalToolBatch` scratch 在此被填充。

*改动*:

- `src/agent/external/machine.rs`:
  - `ExternalAgentMachine` 新增非序列化字段 `tool_ids: Arc<dyn ToolExecutionIds>`;`new` 默认
    `NoToolExecutionIds`(**保持 `new` 签名兼容**,`tests/agent_external_real_e2e.rs` 的
    `ExternalAgentMachine::new(state, ids)` 调用不受影响);新增 builder `with_tool_execution_ids`。
  - `fold_session_result` 的 `PausedForToolCalls` 分支由「未支持→classified error」改为解构
    `{session, batch_id, calls, observations}` → `observe` → 新增 `pause_for_tool_calls(...)`。
  - 新增 `pause_for_tool_calls`:要求 in-flight turn;`set_session(Some(session))`;逐 call
    `to_tool_call()` → `tool_ids.tool_call_id(&call)` 取 `ToolCallId` +
    `requirement_ids.next_requirement_id(Tool)` 取 `RequirementId` → 发一 batch
    `RequirementKind::NeedTool { call_id, call }`(顺序=原始 call 顺序);driver-facing
    `LoopCursor::awaiting_tool(step_id, call_ids, Some(ToolWaitRequirements::root(ids)))` 使
    `pending_requirement_ids()` 返回全部 tool requirement id;serializable cursor 落
    `ExternalAgentCursor::AwaitingTool { batch_id, requirements }`;`PendingExternalToolBatch` scratch
    落 `batch_id/calls/call_to_requirement(provider→req)/requirement_to_call(req→provider)/results(空)`。
    **不写 Conversation**(external runtime 才是 tool result 消费者);pending turn 保持打开跨 pause。
  - 无 `ToolExecutionIds`(默认 `NoToolExecutionIds`)时 `tool_call_id` 返回 `IdUnavailable` → 走
    `fail_with("tool id unavailable: …", notifications)`:进入 `LoopCursorKind::Error`,discard pending turn,
    错误文本稳定。id 分配失败 / cursor 构建失败同样 `fail_with`,均携带已 observe 的 notifications(§5.5 replay-once)。
  - `abandon` / `fail_with` 增加 `self.pending_tool_batch = None`(class-wide:任何 turn 结束都清空 tool scratch,
    不留 stale batch)。
  - dead_code staging 收敛:machine 字段 `pending_tool_batch` 现被写入构造值 → **移除**其上 `#[expect(dead_code)]`
    (否则 unfulfilled 报错,已用 rustc edition2024 实测确认);`PendingExternalToolBatch` struct 字段本任务只
    构造未读取 → **保留** struct 级 `#[expect(dead_code)]`,reason 改为仅指向 M2-3 drain。
  - 更新模块 doc(新增 `PausedForToolCalls` 覆盖 bullet)与相关 rustdoc。
- `src/agent/external/machine/tests.rs`:
  - 新增 `SeqToolIds`(impl `ToolExecutionIds`:`tool_call_id` 按序发 uuid,其余方法返回 `IdUnavailable`——
    external machine 不注册 tool result / 不续 assistant step,故不被调用);helper `machine_with_tool_ids` /
    `external_tool_call` / `paused_for_tools`。
  - `external_tool_pause_emits_need_tool_batch`:requirements 数=calls 数、逐个 `NeedTool.call.id`=
    `provider_call_id`、`pending_requirement_ids()` 含全部、cursor kind=`AwaitingTool`、serializable
    `AwaitingTool.batch_id`/`requirements.ids().len()` 正确、session 已记录、pending Conversation 打开未提交。
  - `external_tool_pause_without_tool_ids_fails`:默认 machine 收到 `PausedForToolCalls` →
    `LoopCursorKind::Error` + pending turn discard。

*验证*(完整序列 1-6 全过):

- `cargo fmt --all -- --check` FMT_OK。
- 聚焦:`external_tool_pause_emits_need_tool_batch`、`external_tool_pause_without_tool_ids_fails`(2 passed）。
- `cargo clippy --all-targets -- -D warnings` 0 warning(`#[expect(dead_code)]` 兑现:machine 字段移除后无
  unfulfilled、struct 级仍兑现)。
- `cargo test --all --all-targets`:791 passed / 0 failed(较 M2-1 的 789 新增本任务 2 test)。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- `git diff --check` clean。

### [DONE] M2-3 收齐 `NeedTool` 结果并回灌 `RespondToolResults`

**上下文**:

- `drain` 会将 batch 的每个 fulfilled result 逐个 `StepInput::Resume` 回 machine。
- 在 batch 未全部完成前,machine 应保持非 terminal,不提交 turn,不再发外部 session requirement。
- 最后一个 tool result 到达后,machine 发出新的 `NeedExternalSession`:

  ```rust
  ExternalSessionInput::RespondToolResults { batch_id, results }
  ```

**做什么**:

- 扩展 `ExternalAgentMachine::resume` 的 awaiting 路由,新增 `Awaiting::Tool`。
- 实现 `resume_tool`:
  - 校验 `resolution.id` 属于 pending batch。
  - `RequirementResult::Tool(Ok(response))` -> `ExternalToolResult`。
  - `RequirementResult::Tool(Err(error))` -> 根据 external tool failure policy 处理。
    首版可采用 `ReturnErrorToRuntime` 固定策略,把错误文本作为 `ExternalToolResult.error` 回灌。
  - 错误 family -> error cursor,文案形如
    `NeedTool requirement cannot accept a '<tag>' result`。
  - 重复 resume 或未知 id -> error cursor。
  - batch 未收齐 -> quiescent outcome,requirements 为空,cursor 仍 awaiting tool。
  - batch 收齐 -> `block_on_session(in_flight.step_id, RespondToolResults { ... })`。
- 确保 `ResponseToolResults.results` 顺序稳定。推荐按原始 calls 顺序输出,不要按完成顺序输出。

**验证条件**:

- machine unit 新增测试:
  - `external_tool_results_resume_back_to_session_when_batch_complete`
  - `external_tool_batch_accepts_out_of_order_results`
  - `external_tool_partial_result_keeps_waiting`
  - `external_tool_resume_wrong_requirement_fails`
  - `external_tool_resume_wrong_family_fails`
- 断言 `RespondToolResults.batch_id` 等于 pause 的 batch id。
- 断言 results 顺序等于 original calls 顺序。
- 聚焦测试:
  - `cargo test -p agent-lib external_tool_results`
- 完整验证序列 1-6 全过。

**完成记录**(2026-07-17):

本任务把 paused tool batch 的 host 结果**收齐并回灌** runtime(consumer/drain),配对 M2-2 的 producer。
每个 host tool result 各自 resume 一次,collect 进 M2-2 落地的 `PendingExternalToolBatch` scratch;
batch 未收齐前保持 `AwaitingTool` 非 terminal、不发 requirement、不推进 session;收齐后按**原始 call 顺序**
(非完成顺序)组一批 `ExternalToolResult`,经 `block_on_session` 发新 `NeedExternalSession
(RespondToolResults { batch_id, results })` 并 repark `AwaitingSession`,turn 保持打开跨整个 tool phase。

*改动*:

- `src/agent/external/machine.rs`:
  - `Awaiting` enum 新增 `Tool` 变体(无 payload:volatile 关联全在 scratch);`resume` 的 cursor match 新增
    `AwaitingTool { .. } => Ok(Awaiting::Tool)`,dispatch `Ok(Awaiting::Tool) => self.resume_tool(resolution)`。
  - 新增 `resume_tool(resolution)`:无 scratch(mid-turn restore 保护)/ 无 in_flight → `fail`;
    `requirement_to_call.get(id)` 无 → `fail`(unknown id 不属本 batch);`results` 已含该 provider_call_id →
    `fail`(duplicate resume);`Tool(Ok(resp))` → `ExternalToolResult::from_tool_response`,`provider_call_id`
    覆写为 batch mapping 的权威值;`Tool(Err(err))` → `from_tool_runtime_error`(固定 **return-error-to-runtime**
    策略,错误文本进 `error`+content,不 StopRun);其他 family → `fail("NeedTool requirement cannot accept a
    `<tag>` result")`。collect 进 scratch;`results.len() < calls.len()` → 保持 `AwaitingTool`,quiescent 空
    outcome(不改 cursor,对齐内部 `default/tools.rs::resume_tool` partial 行为);收齐 → `take` scratch,按
    `batch.calls` 原始顺序组 `results`,`block_on_session(in_flight.step_id, RespondToolResults { batch_id,
    results })`。
  - class-wide 清理:M2-2 遗留的 `PendingExternalToolBatch.call_to_requirement`(provider→req)只写未读,drain 只需
    `requirement_to_call`(req→provider),故**移除该字段** + 其构造;连带移除 `PendingExternalToolBatch` struct 级
    `#[expect(dead_code)]`(剩余 `batch_id`/`calls`/`requirement_to_call`/`results` 全被 M2-3 读取,已 rustc
    edition2024 实测无 unfulfilled)。
  - 更新模块顶部 `PausedForToolCalls` bullet、struct doc、`pause_for_tool_calls` doc,并新增 `resume_tool` rustdoc。
- `src/agent/external/machine/tests.rs`:
  - helper `tool_response` / `tool_resolution`(Ok)/ `tool_error_resolution`(Err)/ `pause_on_two_tools`(驱到两 call
    tool pause 并返回 per-call requirement id)/ `assert_responds_with_batch`(断言唯一 RespondToolResults 及顺序);
    imports 增补 `StepOutcome`、`ToolResponse`、`ToolStatus`。
  - 新增 6 test:`external_tool_results_resume_back_to_session_when_batch_complete`、
    `external_tool_batch_accepts_out_of_order_results`、`external_tool_partial_result_keeps_waiting`、
    `external_tool_batch_returns_runtime_errors_to_the_runtime`(return-error-to-runtime 覆盖)、
    `external_tool_resume_wrong_requirement_fails`、`external_tool_resume_wrong_family_fails`。

*验证*(完整序列 1-6 全过):

- `cargo fmt --all -- --check` FMT_OK。
- 聚焦:`cargo test -p agent-lib external_tool`(11 passed,含本任务 6 新 test)。
- `cargo clippy --all-targets -- -D warnings` 0 warning(struct 级 `#[expect(dead_code)]` 移除后无 unfulfilled)。
- `cargo test --all --all-targets`:全绿(lib unit 571 passed,较 M2-2 新增本任务 6 test;工作区其余套件均 0 failed)。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- `git diff --check` clean。

### [DONE] M2-4 Review：external tool phase 正确性检查

**上下文**:

M2 让 external runtime 能调用 host tools。阶段 review 要确认它没有把 external tool result 错误写进
Conversation,也没有绕过 existing `ToolHandler` / pop routing。

**做什么**:

- 对照内部 `src/agent/machine/default/tools.rs` 的行为,确认 external tool phase:
  - requirement id 路由按 `RequirementId`,支持 out-of-order resume。
  - batch 未完成时不 terminal。
  - batch 完成后只发 `NeedExternalSession(RespondToolResults)`。
  - tool execution failure 首版策略明确,不会 panic。
  - no ids / wrong family / wrong id 都进入 error cursor。
- 检查 `ExternalAgentState` serde 不包含 live tool registry / executor / handler。
- 检查 `drain` trace 中 tool requirements 被正常记录,无需改 driver。

**验证条件**:

- `cargo test -p agent-lib external_tool`
- `cargo test -p agent-lib drain`
- 完整验证序列 1-6 全过。
- 完成记录中列出 external tool phase 与 DefaultAgentMachine tool phase 的差异和保留原因。

**完成记录**(2026-07-17):

纯 sign-off review,逐条核对 M2-1..M2-3 落地的 external tool phase 源码 + 测试,无代码改动
(实现与 review 清单一致,无 spec 偏差 / 无 workaround)。核对结论:

- *requirement id 路由 / out-of-order*:`ExternalAgentMachine::resume_tool` 用
  `PendingExternalToolBatch.requirement_to_call.get(&resolution.id)` 按 `RequirementId` 路由
  (非 emission 顺序),乱序 resume 由 `external_tool_batch_accepts_out_of_order_results` 覆盖。
- *batch 未完成不 terminal*:`results.len() < calls.len()` 时返回 `StepOutcome::new([], [], true)` 且
  cursor 不变(仍 `AwaitingTool`),对齐内部 `default/tools.rs::resume_tool` partial 行为;
  `external_tool_partial_result_keeps_waiting` 覆盖。
- *收齐只发 RespondToolResults*:按 `batch.calls` 原始顺序组 results ->
  `block_on_session(RespondToolResults { batch_id, results })`,**从不写 Conversation**;
  `external_tool_results_resume_back_to_session_when_batch_complete` 覆盖。
- *failure 策略 & 不 panic*:固定 return-error-to-runtime(`Tool(Err)` ->
  `ExternalToolResult::from_tool_runtime_error`,不 StopRun、不 panic);
  `external_tool_batch_returns_runtime_errors_to_the_runtime` 覆盖。
- *no ids / wrong family / wrong id -> error cursor*:分别由
  `external_tool_pause_without_tool_ids_fails`、`external_tool_resume_wrong_family_fails`、
  `external_tool_resume_wrong_requirement_fails` 覆盖;另 duplicate resume 与 empty batch
  (`EmptyToolWait`)也走 error cursor,不 deadlock/panic。
- *state serde 无 live registry/executor/handler*:`ExternalAgentState` 字段仅
  spec/conversation/session/cursor/active_tools(仅 `Tool` 声明,无 executor)/artifacts/cleanup_required;
  自定义 serde 负测断言 forbidden keys 含 `tool_registry`。所有 volatile 关联
  (`tool_ids`/`requirement_ids`/`in_flight`/`pending_tool_batch`)都在 machine 上,不进 state。
- *drain trace 正常记录、无需改 driver*:external machine emit 的 `NeedTool` 与 DefaultAgentMachine 同形,
  复用同一 driver 路径;drive.rs 单测 `drain_resolves_a_concurrent_batch_out_of_order` /
  `drain_records_resolved_at_scope_for_local_and_popped_requirements` 已覆盖 NeedTool batch 的乱序
  resolve 与 trace 记录,external 无需任何 driver 改动。

external tool phase 与 DefaultAgentMachine tool phase 的差异及保留原因:

1. *结果去向*:external 从不写 Conversation,收齐后 `RespondToolResults` 回灌 runtime;Default
   `append_tool_response` 把结果写进 Conversation。原因:external runtime 自持 transcript,host 只做工具桥。
2. *failure policy*:external 固定 `ReturnErrorToRuntime`(无 `StopRun`);Default 可配置
   `ReturnErrorToModel` / `StopRun`。原因:external runtime 自行决定如何应对失败调用,首版不暴露 StopRun。
3. *per-result 通知*:external tool resume 不发 `ToolCallFinished`(runtime 活动经 `ExternalObservedEvent`
   -> `Notification::ExternalAgent` 汇报);Default 每个 result 发 `ToolCallFinished`。原因:external 的
   可观测事件模型是 observation-based,而非 host tool-call-based。
4. *scratch 持久化*:external 的 per-call 关联在非序列化 `PendingExternalToolBatch`,cursor 只存可恢复寻址
   (`ToolCallId -> RequirementId`);Default 的 `ToolPhase` 同样非序列化(phase marker)。mid-turn restore
   的精确恢复两者都留待后续(见 PLAN.md「恢复 mid-turn scratch」风险)。

*验证*(完整序列 1-6 全过):

- `cargo fmt --all -- --check` FMT_OK。
- 聚焦:`cargo test -p agent-lib external_tool`(11 passed)、`cargo test -p agent-lib drain`(7 passed)。
- `cargo clippy --all-targets -- -D warnings` 0 warning。
- `cargo test --all --all-targets`:全绿(工作区各套件均 0 failed);本任务仅文档改动,代码与 M2-3
  绿快照一致,复跑确认无回归。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- `git diff --check` clean。

---

## Milestone 3 — subagent / interaction parity

目标:external runtime 能通过 machine 触发 host subagent,并让 runtime permission/question/choice 走标准
`NeedInteraction`。

### [DONE] M3-1 实现 `PausedForSubagent` -> `NeedSubagent` -> `RespondSubagent`

**上下文**:

- `NeedSubagent` 已在 effect manifest 中,driver 对它有特殊串行 routing。
- `ExternalAgentMachine` 只需要 emit `RequirementKind::NeedSubagent`,不需要知道 child 如何创建。
- runtime 回灌应使用 M1 的 `ExternalSessionInput::RespondSubagent`。

**做什么**:

- 在 `ExternalAgentCursor` 新增 `AwaitingSubagent { request_id, requirement }`。
- 在 `ExternalAgentMachine::fold_session_result` 新增 `PausedForSubagent` 分支:
  - record session。
  - emit observations。
  - alloc `RequirementKindTag::Subagent` requirement id。
  - emit `RequirementKind::NeedSubagent { spec_ref, brief, result_schema }`。
  - cursor/loop cursor 记录 outstanding requirement。
- 在 `resume` 新增 `Awaiting::Subagent` 路由:
  - `RequirementResult::Subagent(Ok(output))` -> `NeedExternalSession(RespondSubagent { request_id, output })`。
  - `RequirementResult::Subagent(Err(error))` -> 首版作为 `RespondSubagent` error output 或 error cursor。
    推荐先转 error cursor,后续再设计 runtime-visible child error payload。
  - wrong id/wrong family -> error cursor。

**验证条件**:

- machine unit 新增:
  - `external_subagent_pause_emits_need_subagent`
  - `external_subagent_result_responds_to_session`
  - `external_subagent_wrong_family_fails`
- drive unit 新增/复用:
  - external child `NeedSubagent` 能经 `DrivingSubagentHandler` fulfill。
  - child unhandled interaction 能 pop 到 outer,不重入 subagent handler。
- 聚焦测试:
  - `cargo test -p agent-lib external_subagent`
  - `cargo test -p agent-lib driving_subagent`
- 完整验证序列 1-6 全过。

**完成记录**(2026-07-17):

把 `ExternalAgentMachine` 的 subagent-spawn pause 桥接成标准 host `NeedSubagent`,并把 child 输出经
`RespondSubagent` 回灌 runtime。实现镜像既有 tool-batch pause/resume 模式,无 spec 偏差 / 无 workaround。
落地要点:

- *cursor*:`ExternalAgentCursor` 新增 `AwaitingSubagent { requirement, request_id }`(在 `AwaitingTool`
  之后),并接入 `requirement()` / `requirements()` / `has_outstanding_requirement()`,round-trip serde 由
  `external_agent_state_cursor_variants_round_trip` 覆盖。volatile 关联(`in_flight` 的
  `Awaiting::Subagent { requirement, request_id }`)留在 machine,不进 state。
- *pause*:`fold_session_result` 的 `PausedForSubagent` 分支调用新 `pause_for_subagent`:guard in-flight ->
  `set_session` -> emit observations -> alloc `RequirementKindTag::Subagent` id -> 拆 request(丢弃未建模的
  `raw` 逃生字段,仅带 `spec_ref` / `brief` / `result_schema`)-> settle `AwaitingSubagent` cursor +
  `LoopCursor::streaming_step`(复用单 outstanding requirement 通道,与 interaction 路径一致)-> emit
  `Requirement::at_root(id, NeedSubagent { spec_ref, brief, result_schema })`。
- *resume*:`resume` 读 `AwaitingSubagent` cursor 派发到新 `resume_subagent`:`Subagent(Ok(output))` ->
  `block_on_session(RespondSubagent { request_id, output })`(echo 同一 `request_id`);`Subagent(Err)` -> 按
  TODO 推荐先转 error cursor(`"external subagent failed: {error}"`),不伪造 runtime-visible child error
  payload;wrong id / wrong family / 缺 in-flight -> error/fail cursor。
- *driver 无改动*:external machine emit 的 `NeedSubagent`(`needs_outer: true`)与内部机同形,复用
  `drive.rs::resolve_requirement` 既有 `scope.subagent()` + `ScopePop` outer routing。

external subagent phase 与内部 subagent 的差异及保留原因:

1. *结果去向*:external 从不写 Conversation,child summary 经 `RespondSubagent` 回灌 runtime;runtime 自持
   transcript,host 只做 subagent 桥。
2. *child error 策略*:首版固定 error cursor(不暴露 runtime-visible child error payload),与 M1 设计一致,
   后续再设计 runtime 可见的 child 失败回灌。
3. *raw 逃生字段*:`ExternalSubagentRequest.raw`(design §5.3 未建模 provider 逃生舱)刻意不带进
   `NeedSubagent`,只桥接已建模的 spec_ref / brief / result_schema。

*验证*(完整序列 1-6 全过):

- `cargo fmt --all -- --check` FMT_CLEAN。
- 聚焦:`cargo test -p agent-lib external_subagent`(7 passed:5 machine unit + 2 DTO)、
  `cargo test -p agent-lib driving_subagent`(2 passed:新增 `tests/agent_external_subagent.rs` 两条 drive
  测试)。machine unit 新增 `external_subagent_pause_emits_need_subagent` /
  `external_subagent_result_responds_to_session` / `external_subagent_wrong_family_fails` /
  `external_subagent_resume_wrong_requirement_fails` / `external_subagent_error_settles_error_cursor`;drive
  新增 `external_agent_driving_subagent_fulfills_child`(经 `DrivingSubagentHandler` fulfill 并 respond)与
  `external_agent_driving_subagent_pops_child_interaction_to_outer`(headless child interaction pop 到 parent,
  不重入 subagent handler)。
- `cargo clippy --all-targets -- -D warnings` 0 warning。
- `cargo test --all --all-targets`:全绿(工作区各套件 38 组结果均 0 failed)。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- `git diff --check` clean。

### [DONE] M3-2 完善 runtime permission/question/choice 到 `NeedInteraction` 的映射

**上下文**:

- `ExternalSessionResult::PausedForInteraction` 已携带 neutral `Interaction`。
- `InteractionKind::Permission` 和 `PermissionRequest` 已存在。
- `ApprovalInteractionHandler` 已能把 `ApprovalDecision` 映射到 `PermissionResponse`。

**做什么**:

- 审查并补齐 `PausedForInteraction` 的 machine 验证:
  - `Interaction::accepts_response` 必须在 `resume_interaction` 回灌前调用。
  - wrong response family/action id 应进入 error cursor,不应把无效 response 传给 runtime。
- 为 permission/question/choice 分别补 machine unit:
  - permission approve/deny/cancel 能生成 `RespondInteraction`。
  - question answer 能生成 `RespondInteraction`。
  - choice index 越界被拒绝。
- 更新 `src/agent/drive/reference.rs` 文档和测试:
  - `ApprovalInteractionHandler::approve()` 对 permission 返回 approve。
  - `ApprovalInteractionHandler::deny()` 对 permission 返回 deny。
  - 非 approval 的 question/choice 仍给出类型对齐的 trivial response,或文档说明 reference handler 只适合测试。

**验证条件**:

- 聚焦测试:
  - `cargo test -p agent-lib external_permission_interaction`
  - `cargo test -p agent-lib interaction_result_rejected`
  - `cargo test -p agent-lib approval_interaction_handler`
- `cargo test -p agent-lib external_pause_then_respond_then_complete_commits_the_turn` 仍通过。
- 完整验证序列 1-6 全过。

**完成记录**:

- **根因**: `resume_interaction`(machine.rs)先前把 host 的 `RequirementResult::Interaction`
  直接塞进 `RespondInteraction` 回灌 runtime,**从未**调用 `Interaction::accepts_response`,
  wrong family / choice 越界 / permission `action_id` 不匹配的无效 response 会被原样转发给 runtime。
- **state.rs**: `ExternalAgentCursor::AwaitingInteraction` 新增 `interaction: Interaction` 字段
  (可序列化 resumable fact),保留 runtime 暂停时的 neutral `Interaction`,供 resume 前校验;
  更新 rustdoc、cursor round-trip 测试与 `requirement()` 断言。
- **machine.rs**:
  - `Awaiting::Interaction` 增加 `interaction`;`resume` 从 cursor clone 后传入。
  - `pause_for_interaction` 把 `request` clone 一份存进 cursor(另一份 emit 到 `NeedInteraction`)。
  - `resume_interaction` 在取出 `response` 后调用 `interaction.accepts_response(&response)`;
    校验失败 -> `self.fail(...)` error cursor(稳定诊断,只输出 `InteractionError` Display,
    不含 transcript),**绝不**把无效 response 发给 runtime;通过才 `block_on_session` 回灌
    `RespondInteraction`。更新模块 doc。
- **machine/tests.rs**: 新增 permission/choice pause + response 助手,补 class-wide 单测:
  `external_permission_interaction_relays_approve/deny/cancel`、
  `external_question_interaction_relays_answer`、`external_choice_interaction_relays_selected_index`、
  `interaction_result_rejected_on_action_mismatch/choice_out_of_range/family_mismatch_settles_error`
  (断言 error cursor 且 0 RespondInteraction)、`interaction_result_rejected_keeps_the_turn_recoverable_state`。
- **reference.rs**: 明确 `ApprovalInteractionHandler` 文档(reference/headless 默认,attended 应接真实 UI),
  新增 `#[cfg(test)] mod tests`:`approval_interaction_handler_approves/denies_permission`、
  `..._maps_timeout_and_cancel_to_permission_non_approval`、`..._answers_question_and_choice_trivially`、
  `..._answers_approval_addressing_step_and_call`;每条都用 `accepts_response` 断言响应可被其 interaction 接受。
- **验证序列 1-6 全过**: fmt clean、聚焦测试(interaction 26 passed / approval_interaction_handler 5 passed)、
  clippy 0 warning、全套件 38 组 0 failed、doc(`-D warnings`)通过、`git diff --check` clean。

### [DONE] M3-3 支持 external runtime 的 `spawn_agent` tool bridge 特判

**上下文**:

- 部分 runtime 可能只能通过 custom tool/MCP 暴露 subagent 请求,而不是直接提供 `PausedForSubagent`。
- 设计文档 §8.3 要求 `spawn_agent` 不走普通 host tool execution,而应转成 `NeedSubagent`。
- 此特判属于 `ExternalAgentMachine` 对 external tool call 的 routing,不是 `ToolRegistry` 的普通工具。

**做什么**:

- 定义 `spawn_agent` 的 provider-neutral input contract,例如:

  ```json
  {
    "spec_ref": "<agent-id>",
    "prompt": "...",
    "result_schema": { ... }
  }
  ```

- 在 `PausedForToolCalls` 折成 requirements 时识别 `ExternalToolCall.name == "spawn_agent"`:
  - 解析 input。
  - 生成 `NeedSubagent` 而不是 `NeedTool`。
  - pending batch scratch 中标记此 provider call id 等待 subagent result。
  - subagent result 最终也要合并进同一个 `RespondToolResults` batch,以便 runtime 看到 tool bridge 结果。
- 支持同一 external tool batch 同时包含普通 tool 和 `spawn_agent`:
  - 普通 tool 可并发 fulfill。
  - subagent 由 driver 串行 fulfill。
  - machine 收齐两类 result 后按原始 calls 顺序回灌 `RespondToolResults`。

**验证条件**:

- machine unit:
  - `external_spawn_agent_tool_call_emits_need_subagent`
  - `external_mixed_tool_and_spawn_agent_batch_returns_one_respond_tool_results`
  - invalid `spawn_agent` input -> error cursor 或 runtime-visible error result,策略需稳定并测试。
- drive unit 确认 mixed batch 中 `NeedSubagent` 仍走 serial outer routing。
- 聚焦测试:
  - `cargo test -p agent-lib external_spawn_agent`
  - `cargo test -p agent-lib external_mixed_tool`
- 完整验证序列 1-6 全过。

**完成记录**:

- **input contract 复用**: 复用既有 `crate::agent::collab::{SpawnAgentRequest, SPAWN_AGENT}`,
  而非新造 contract。TODO 示例 JSON 写 `spec_ref`/`prompt`(标注「例如」);设计 §8.1 规定 runtime
  拿到的是 `bridge_tool_declarations()`(声明 `spec`/`brief`/`result_schema`),§8.3 明确点名
  `SpawnAgentRequest::parse`,故实际解析 contract 为 `{spec, brief, result_schema}`。
- **machine.rs**:
  - `PendingExternalToolBatch` scratch 把 `requirement_to_call: BTreeMap<RequirementId, String>`
    换成 `pending: BTreeMap<RequirementId, PendingBridgeCall>`(`provider_call_id` + `kind`),
    新增 `enum ExternalBridgeCallKind { Tool, Subagent }` 记录每个 bridged call 走哪个 result family。
  - `pause_for_tool_calls`:逐个 call 判断——`SpawnAgentRequest::matches(name)` 则 `parse`;
    成功 emit `NeedSubagent`(`into_requirement_kind(in_flight.step_id)`,tag Subagent),
    失败按 §8.4 return-error-to-runtime 预置一条 runtime-visible error `ExternalToolResult`
    进 `results`、**不** mint requirement;否则照常 `NeedTool`(tag Tool)。每个 bridged call
    (含 subagent)都 mint 一个 `ToolCallId`,因为 `LoopCursor::awaiting_tool` 要求 tool_call_ids
    与 `ToolWaitRequirements` keys 精确一致,mixed batch 才能停在同一 `AwaitingTool` cursor。
    全部 spawn_agent 都畸形(requirements 为空)时跳过 park、立即 `respond_with_tool_batch`。
  - `resume_tool`:经 `batch.pending.get(id)` 路由,按 `kind` 校验 result family——
    `Tool` 收 `Tool(Ok/Err)`;`Subagent` 收 `Subagent(Ok(output))`(把 `summary` 折成
    `ExternalToolResult{status:Ok, content:[Text]}`),`Subagent(Err)` 走 error cursor
    (host-orchestration 失败,与独立 `resume_subagent` 对称),family 不符则 error cursor。
  - 新增 `respond_with_tool_batch(step_id, batch)`:按 `batch.calls` 原始顺序装配 results 后
    `block_on_session(RespondToolResults)`;`resume_tool` 收齐与 `pause_for_tool_calls` 全畸形
    两条路径共用。
  - 更新模块 doc 与 `AwaitingTool` cursor doc(state.rs),说明 spawn_agent bridge 语义与
    native `PausedForSubagent` 的区别(前者回 `RespondToolResults`,后者回 `RespondSubagent`)。
- **策略选择**(稳定并测试):
  - 畸形 `spawn_agent` input = **runtime-visible error result**(非 error cursor):machine 充当
    tool-input 校验方,与普通 tool 的 return-error-to-runtime 一致,mixed batch 更健壮;turn 存活。
  - `spawn_agent` 的 subagent 驱动失败 = **error cursor**(停 turn):属 host orchestration 失败
    (depth/budget/cancel/internal),与 native subagent 对称,而非 runtime-caused。
- **machine/tests.rs**: 新增 spawn_agent bridge 段(helpers `spawn_agent_call`/
  `malformed_spawn_agent_call`/`pause_on_spawn_agent`)与 7 条单测:
  `external_spawn_agent_tool_call_emits_need_subagent`、
  `external_spawn_agent_result_bridges_summary_into_respond_tool_results`、
  `external_mixed_tool_and_spawn_agent_batch_returns_one_respond_tool_results`、
  `external_mixed_valid_tool_and_malformed_spawn_agent_returns_one_batch`、
  `external_spawn_agent_invalid_input_returns_runtime_error_result`、
  `external_spawn_agent_subagent_failure_settles_error`、
  `external_spawn_agent_bridge_wrong_family_fails`。
- **drive.rs**: 新增 `mixed_tool_and_subagent_batch_routes_subagent_serially`——用真正返回 summary 的
  `RealSubagent` handler + `MixedToolSubagentScope`,`fulfill_batch([NeedTool, NeedSubagent])`
  两类都正确 resolve;subagent 若误入并发 local set 会因 `fulfill_with_scope` 返回 `None` 触发
  `.expect` panic,故成功完成即证明其走 serial outer routing(设计不变,复核既有保证)。
- **验证序列 1-6 全过**: fmt clean、聚焦测试(external_spawn_agent 5 passed、external_ lib 71 passed、
  mixed_tool_and_subagent 1 passed)、clippy `-D warnings` 0 warning、全套件 38 组 0 failed、
  doc(`-D warnings`)通过、`git diff --check` clean。

### [DONE] M3-4 Review：interaction/subagent parity 正确性检查

**上下文**:

M3 结束后 external machine 应具备 `NeedInteraction`、`NeedTool`、`NeedSubagent` 三类 host-mediated
能力。review 要确认 scope/pop 语义没有被破坏。

**做什么**:

- 手工检查 `src/agent/external/machine.rs`:
  - `resume` 所有 awaiting 相位都有 id/family 校验。
  - `NeedSubagent` 没有在 machine 内执行 child,只 reify requirement。
  - `spawn_agent` 特判没有落入普通 `ToolRegistry`。
  - interaction response 通过 `Interaction::accepts_response` 校验后才回灌。
- 检查 `drain` 不需要 external 特判；若新增了特判,需要说明原因并补 trace 测试。
- 更新 `docs/managed-external-agent.md` 中 M2/M3 状态或命名差异。

**验证条件**:

- `cargo test -p agent-lib external_agent`
- `cargo test -p agent-lib drive`
- 完整验证序列 1-6 全过。
- 完成记录中给出 M3 能力 parity 摘要。

**完成记录**(2026-07-17):

纯 sign-off review,逐条核对 M3-1..M3-3 落地的 interaction/subagent parity 源码 + 测试,唯一改动是
文档同步(`docs/managed-external-agent.md`),无代码改动、无 spec 偏差、无 workaround。

*`src/agent/external/machine.rs` 四点核对(全部满足)*:

1. **`resume` 所有 awaiting 相位都有 id/family 校验**:`resume` 先从 cursor 读出 `Awaiting`
   相位再分派。
   - `AwaitingSession` -> `resume_session`:`resolution.id != expected` -> `fail`;family 要求
     `RequirementResult::ExternalSession`,否则 `fail`。
   - `AwaitingInteraction` -> `resume_interaction`:id 校验 + family 要求
     `RequirementResult::Interaction` + `Interaction::accepts_response` 校验(见第 4 点)。
   - `AwaitingTool` -> `resume_tool`:按 `batch.pending.get(&resolution.id)` 路由(batch 有多个
     requirement,id 不在 batch 内 -> `fail`);duplicate result -> `fail`;按 `PendingBridgeCall.kind`
     校验 result family(`Tool` 收 `Tool(Ok/Err)`、`Subagent` 收 `Subagent(Ok/Err)`,family 不符 ->
     `fail`)。
   - `AwaitingSubagent` -> `resume_subagent`:id 校验 + family 要求 `RequirementResult::Subagent`。
   - 非 awaiting cursor(`Idle`/`Done`/`Error`)上 resume -> `fail`(cursor_label 诊断)。
2. **`NeedSubagent` 不在 machine 内执行 child,只 reify requirement**:`pause_for_subagent` 与
   `pause_for_tool_calls` 的 spawn_agent 分支都只 `emit` 一个 `Requirement`(`NeedSubagent`),park 到
   `AwaitingSubagent` / `AwaitingTool`,由 driver 的 `DrivingSubagentHandler`(serial `needs_outer`
   路径)驱动子 agent;machine 从不构造/驱动 child,也不把 child 输出写入 `Conversation`。
3. **`spawn_agent` 特判没有落入普通 `ToolRegistry`(即不发成 `NeedTool`)**:`pause_for_tool_calls`
   逐 call 用 `SpawnAgentRequest::matches(&call.name)` 判定,命中则 `SpawnAgentRequest::parse` ->
   `into_requirement_kind` 桥成 `NeedSubagent`(scope-deepening),而非 `NeedTool`;畸形 input ->
   预置 runtime-visible error result(return-error-to-runtime §8.4),同样不发 `NeedTool`。普通 tool
   才走 `NeedTool`。machine 本身不持有 `ToolRegistry`——tool 由 driver scope handler 兑现。
4. **interaction response 通过 `Interaction::accepts_response` 校验后才回灌**:`resume_interaction`
   在取出 `InteractionResponse` 后、`block_on_session(RespondInteraction)` 之前调用
   `interaction.accepts_response(&response)`;校验失败 -> `fail`(error cursor),**绝不**把非法答案转发
   给 runtime。

*`drain` 无 external 特判*:`src/agent/drive.rs` 的 `drain` / `fulfill_batch` / `resolve_requirement`
全部按 `RequirementKindTag`(泛型 effect family)路由,无任何 `External*` 分支。subagent 的 serial
`needs_outer` outer-routing 是通用机制(`tag != Subagent && scope_handles` 才进并发 local set),external
emit 的 `NeedTool`/`NeedSubagent`/`NeedInteraction` 与 `DefaultAgentMachine` 同形,复用同一 driver 路径,
无需新增特判,因此无新增 trace 测试的需要。

*文档同步*(`docs/managed-external-agent.md`):

- §1 现状表:`external runtime 发起 host tool call` / `host subagent` 由「未实现」改为
  「machine 已实现(runtime handler 待实现)」。
- §3 parity 表:tool call / tool approval / user question / subagent 四行备注改为
  「machine 已实现,runtime handler 待实现」。
- §21 里程碑:新增编号说明(设计里程碑 ≠ 执行里程碑,见 PLAN.md/TODO.md);修正 M2 命名差异——
  实际未引入 `AwaitingToolApproval` cursor(approval 复用 interaction 相位)、未引入
  `ToolApprovalPolicy`/`ToolFailurePolicy` 类型(固定 return-error-to-runtime);M3 标注 observed
  event sequence / replay dedup 已落地、streaming live sink 待执行侧 M4-1。

*M3 能力 parity 摘要*(external machine 三类 host-mediated 能力,均 sans-io、只 reify requirement):

| 能力 | 触发点 | reify | 回灌 | park cursor |
|---|---|---|---|---|
| interaction | `PausedForInteraction` | `NeedInteraction`(`accepts_response` 校验) | `RespondInteraction`(echo `action_id`) | `AwaitingInteraction` |
| host tool | `PausedForToolCalls`(普通 call) | `NeedTool` batch(`ToolExecutionIds` 分配 `ToolCallId`) | `RespondToolResults`(原始 call 顺序) | `AwaitingTool` |
| native subagent | `PausedForSubagent` | `NeedSubagent`(复用 spec_ref/brief/result_schema) | `RespondSubagent`(echo `request_id`) | `AwaitingSubagent` |
| spawn_agent bridge | `PausedForToolCalls`(`spawn_agent` call) | `NeedSubagent`(§8.3 特判) | 折成 `ExternalToolResult(Ok,summary)` 进同 batch 的 `RespondToolResults` | `AwaitingTool` |

共性:in-flight turn 跨 pause 保持 open,子结果折回同一 turn;失败区分 return-error-to-runtime
(tool/畸形 spawn_agent input,turn 存活)与 host-orchestration error cursor(subagent 驱动失败、
family/id 不符,停 turn);无 tool/subagent 结果写入 `Conversation`(runtime 自持 transcript)。

*验证*(完整序列 1-6 全过):

- `cargo fmt --all -- --check` FMT_OK。
- 聚焦:`cargo test -p agent-lib external_agent`(lib external 132 passed)、
  `cargo test -p agent-lib drive`(lib drive 27 passed)。
- `cargo clippy --all-targets -- -D warnings` 0 warning。
- `cargo test --all --all-targets`:本任务仅文档改动(无任何 `.rs` 变更),复用 M3-3 全套件绿快照
  (38 组 0 failed);按 PROMPT「仅文档改动可复用上次 full run」跳过重跑。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过(该 md 未经 `include_str!` 进
  rustdoc,不影响文档构建)。
- `git diff --check` clean。

---

## Milestone 4 — streaming live sink、capability model、session policy

目标:把流式旁路和 runtime 能力差异做成可测试、可降级的公共接口。

### [DONE] M4-1 将 `ExternalEventSink` 升级为 sequenced live sink

**上下文**:

- `src/agent/external/sink.rs` 当前 `ExternalEventSink::emit(&ExternalAgentEvent)` 是占位。
- M1 已引入 `ExternalObservedEvent`。
- live sink 不能改变 control flow,只作为 UI tail。

**做什么**:

- 把 `ExternalEventSink` 调整为接收 `&ExternalObservedEvent`,或新增 `ExternalObservedEventSink` 并保持
  旧 trait 兼容。
- 新增一个测试用 collecting sink:
  - 可放在 test module 内,不一定 public。
  - 用于证明 sink 收到事件不影响 buffered observations。
- 文档明确:
  - sink 不得阻塞。
  - sink 丢事件是允许的。
  - exact-once 语义只由 `ExternalSessionResult.observations` 和 machine replay 保证。

**验证条件**:

- `discard_sink_accepts_and_drops_events` 更新并通过。
- 新增 `collecting_sink_records_sequenced_events_for_tests` 或等价测试。
- `cargo test -p agent-lib external::sink`
- 完整验证序列 1-6 全过。

**完成记录**:

*方案*:`ExternalEventSink` 无生产调用方(仅 trait + `DiscardEventSink` + `agent::external` /
`crate::agent` 再导出),直接把 `emit` 签名从 `&ExternalAgentEvent` 升级为 `&ExternalObservedEvent`,
与设计 §10.1 数据流(`adapter decode ExternalObservedEvent(seq,event) -> if Streaming: sink.emit(...)`)
及 §6「live sink 可以按 seq emit」对齐。不新增并行 trait,避免双 trait 维护成本。live sink 与 buffered
`observations` 共享同一 `seq` 线,host 可据此对齐/去重两条通道。

*改动*(`src/agent/external/sink.rs`,唯一 `.rs` 改动):

- `trait ExternalEventSink::emit(&self, event: &ExternalObservedEvent)` + `DiscardEventSink` 同步改签名。
- rustdoc 明确三点:sink 不得阻塞 continuation(须立即返回、只丢事件不背压);允许自由丢事件(无投递
  保证);exact-once 仅由 `ExternalSessionResult::observations` + machine `seq` 去重保证,sink 只是有损
  live 镜像,绝非其替代。doctest 示例改用 `ExternalObservedEvent::new(0, …)`。
- `discard_sink_accepts_and_drops_events` 更新为喂 sequenced observations(含 trait object 路径)。
- 新增 test-only `CollectingSink`(`Mutex<Vec<ExternalObservedEvent>>`)+
  `collecting_sink_records_sequenced_events_for_tests`:模拟 handler 双通道循环(既 buffer 又 emit),
  断言 sink 按 `seq`(0/1/2)完整记录,且独立的 buffered observations 不被旁路扰动(仍等于全量流)。

*文档同步*(`docs/managed-external-agent.md`):§1 现状表「structured streaming live sink」标注
sink 已 sequenced(policy/runtime 接线待实现);§3 parity 流式文本行改为「seq 已落地(M1)、
`ExternalEventSink` 已 sequenced(M4-1)、runtime 接线待实现」;§10.1 数据流图 `sink.emit(&observed_event)`;
§21 M3 条目拆为「sequenced sink 已落地(M4-1)」+「`ExternalStreamPolicy::Streaming` 策略/接线待实现」。

*验证*(完整序列 1-6 全过):

- `cargo fmt --all -- --check` clean。
- 聚焦:`cargo test -p agent-lib external::sink`(2 passed:`discard_sink_accepts_and_drops_events`、
  `collecting_sink_records_sequenced_events_for_tests`)。
- `cargo clippy --all-targets -- -D warnings` 0 warning。
- `cargo test --all --all-targets`:38 组测试二进制全 ok、0 failed。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过(含 sink doctest)。
- `git diff --check` clean。

### [DONE] M4-2 新增 `ExternalRuntimeCapabilities` 与 unsupported capability 错误

**上下文**:

- `docs/managed-external-agent.md` §15 要求 capability model。
- 当前 `ExternalAgentError` 没有 `UnsupportedCapability`。
- Dispatcher/profile 已有 `Capability` / worker profile,但 runtime adapter 需要更细的 session-level 能力。

**做什么**:

- 新增 `ExternalRuntimeCapabilities`:

  ```rust
  pub struct ExternalRuntimeCapabilities {
      pub runtime: ExternalRuntimeKind,
      pub streaming: bool,
      pub resume: bool,
      pub permission_bridge: bool,
      pub host_tools: bool,
      pub host_subagents: bool,
      pub artifacts: bool,
      pub usage: bool,
      pub graceful_shutdown: bool,
  }
  ```

- 新增 `ExternalCapability` enum,用于错误和测试断言。
- 扩展 `ExternalAgentError`:

  ```rust
  UnsupportedCapability {
      runtime: ExternalRuntimeKind,
      capability: ExternalCapability,
      detail: String,
  }
  ```

- 给 `ExternalRuntimeKind` 提供保守 default capabilities helper,或在 adapter trait 中强制返回。
- 更新 serde/error tests。

**验证条件**:

- capability/error DTO serde round-trip。
- `UnsupportedCapability` 的 `Display` 不包含 raw prompt/tool input。
- 聚焦测试:
  - `cargo test -p agent-lib external_capabilities_roundtrip`
  - `cargo test -p agent-lib external_error_roundtrips`
- 完整验证序列 1-6 全过。

**完成记录**:

*前置结构修复*:上一 commit `69a0060 [M4-1]` 在插入 M4-1 完成记录时误删了本任务标题
`### [TODO] M4-2 …`,使 M4-2 的 body 变成 M4-1 的孤儿续写。本轮先以独立 commit
`c9df411 [M4-2] Restore lost M4-2 task heading` 恢复标题(结构性修复,未拆分/改动任务内容),
再实现本任务。

*方案*:新增独立 `src/agent/external/capability.rs` 模块(里程碑表已规划「新 capability 模块」)。
能力集采用 TODO body 的粗粒度 8 项(`streaming/resume/permission_bridge/host_tools/host_subagents/
artifacts/usage/graceful_shutdown`),与 M4-4 review 清单逐项一致;docs §15 初稿的细粒度字段是
「拟新增」草图,以 TODO body(权威)为准并在 §15 就地标注差异。保守基线 = 全 `false`,未探测不假装
支持(PLAN 非目标「能力差异显式暴露、不静默假装支持」)。

*改动*:

- `src/agent/external/capability.rs`(新增):
  - `ExternalCapability` enum(8 变体,`#[serde(rename_all="snake_case")]`,`Copy`+`Hash`,
    `as_str`/`Display` 稳定标签,`ALL` 常量供穷举迭代)。
  - `ExternalRuntimeCapabilities`(`runtime` + 8 `bool`,serde):`none(runtime)` 保守构造、
    `supports(cap)`、`unsupported(cap, detail) -> ExternalAgentError`(构造 classified error **值**——
    external error 在本 crate 里作为值载入 `ExternalSessionResult::Failed`/cursor,不作为 `Result` err
    返回;避免 `clippy::result_large_err` 且与既有约定一致)。
  - `impl ExternalRuntimeKind { conservative_capabilities() }` 保守 helper。
- `src/agent/external/mod.rs`:`mod capability;` + `pub use capability::{ExternalCapability,
  ExternalRuntimeCapabilities};`;`ExternalAgentError` 新增
  `UnsupportedCapability { runtime, capability: ExternalCapability, detail }`
  (`#[error("{runtime:?} runtime does not support {capability}: {detail}")]`,字段不含 prompt/tool input)。
- `src/agent/mod.rs`:re-export `ExternalCapability` / `ExternalRuntimeCapabilities`。
- 测试:`capability.rs` 内 `external_capabilities_roundtrip`(serde round-trip + `none`/`supports`/`ALL`)、
  `unsupported_builds_classified_capability_error`;`mod.rs` 内 `external_error_roundtrips`(全 8 个 error
  变体 round-trip + snake_case tag)、`unsupported_capability_display_does_not_leak_prompt_or_tool_input`
  (断言 `Display` 只含 runtime+capability+detail,不含注入的 secret prompt / tool input)。

*文档同步*(`docs/managed-external-agent.md`,markdown-only):§15 由「拟新增」改为「已落地(M4-2)」,
更新为实际落地的 8 项能力集、`capability: ExternalCapability`(非 `String`)、`none`/`supports`/
`unsupported`/`conservative_capabilities` 与 `Display` 不泄漏说明;§21 前言执行进度补 M4-1 sequenced sink、
M4-2 capability model。

*验证*(完整序列 1-6 全过):

- `cargo fmt --all -- --check` clean。
- 聚焦:`cargo test -p agent-lib external_capabilities_roundtrip`(1 passed)、
  `cargo test -p agent-lib external_error_roundtrips`(1 passed);capability 模块 7 passed。
- `cargo clippy --all-targets -- -D warnings` 0 warning(`unsupported` 返回 error 值而非 `Result`,
  规避 `result_large_err`)。
- `cargo test --all --all-targets` 全绿(exit 0)。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- `git diff --check` clean。

### [DONE] M4-3 扩展 `ExternalSessionPolicy` / `ExternalAgentSpec` 支持 managed mode 配置

**上下文**:

- `ExternalSessionPolicy` 当前只有 permission/isolation/max_turns/stream_events。
- `ExternalAgentMachine` 需要 tool failure policy、capability requirement、stream policy 等配置。
- 不应让 machine 构造函数参数无限膨胀。

**做什么**:

- 新增 `ExternalAgentMachineConfig` 或扩展 `ExternalSessionPolicy`,建议把 runtime policy 和 machine policy 分开:
  - `ExternalSessionPolicy`: runtime-facing hints。
  - `ExternalAgentMachineConfig`: machine-local handlers/ids/policies。
- 至少覆盖:
  - external tool failure policy: return error to runtime / stop run。
  - require host tool support: bool 或 capability set。
  - require subagent support: bool 或 capability set。
  - max external decision loops。
- 给 `ExternalAgentMachine::new` 保持兼容,新增 builder:
  - `with_tool_execution_ids`
  - `with_external_config`
  - 其他必要策略 setter。
- 不把 live handler/sink 放进 serializable `ExternalAgentState`。

**验证条件**:

- 默认 config 与当前行为兼容。
- config serde 如属于 DTO 则 round-trip；live config 不 serde 则测试其不进入 `ExternalAgentState`。
- policy 超限有 unit test,例如 `external_loop_limit_fails_before_unbounded_pause_loop`。
- 完整验证序列 1-6 全过。

**完成记录**:

*方案*:保持 `ExternalSessionPolicy`(runtime-facing hints)不动,新增独立 machine-local 配置层,把两半职责
分离(design §7)。machine-local config 是纯数据 serde DTO,不持有 live handler/sink/id source,也不进
serializable `ExternalAgentState`;live 的 `RequirementIds` / `ToolExecutionIds` 仍走各自 builder 注入。能力
需求用 M4-2 的 `ExternalCapability` 集表达("capability set" 形态,同时提供 `require_host_tools()` /
`require_subagents()` 便捷 builder)。

*改动*:

- `src/agent/external/config.rs`(新增):
  - `ExternalToolFailurePolicy`(`ReturnErrorToRuntime` 默认 / `StopRun`,snake_case serde)。
  - `ExternalAgentMachineConfig`(serde DTO,`Default` 语义与旧行为一致):`tool_failure`、
    `required_capabilities: BTreeSet<ExternalCapability>`、`max_decision_loops: Option<u32>`;builder
    `with_tool_failure_policy` / `with_max_decision_loops` / `require_capability` / `require_host_tools` /
    `require_subagents`;accessor `tool_failure` / `requires` / `required_capabilities` / `max_decision_loops`。
- `src/agent/external/capability.rs`:`ExternalCapability` 加 `PartialOrd, Ord`(`BTreeSet` 需要)。
- `src/agent/external/mod.rs`:`mod config;` + re-export `ExternalAgentMachineConfig` / `ExternalToolFailurePolicy`。
- `src/agent/mod.rs`:顶层 re-export 两个新类型。
- `src/agent/external/state.rs`:`ExternalAgentState` 新增持久化计数 `decision_loops: u32`
  (record `#[serde(default, skip_serializing_if = is_zero)]` → 干净态字节兼容,旧快照 default=0);
  accessor `decision_loops()` + `record_decision_loop()`(saturating)。计数是纯数据,跨 restore 存活,
  不属于 live handler/sink。
- `src/agent/external/machine.rs`:`ExternalAgentMachine` 新增 `config` 字段(`new` 用 `Default`,兼容);
  builder `with_external_config` / `with_tool_failure_policy` / `with_max_decision_loops`(保留
  `with_tool_execution_ids`);`block_on_session` 作为所有 session round-trip 的唯一漏斗,先
  `record_decision_loop()` 再对 `max_decision_loops` 判限 → 超限 `LimitExceeded` fail;`pause_for_tool_calls`
  两处 tool-id mint 失败经 `fail_tool_id_unavailable` 分类:声明 `require_host_tools` / `require_subagents`
  时升级为 classified `UnsupportedCapability`,否则保留原 "tool id unavailable"(默认兼容);`resume_tool`
  的 `Tool(Err)` 按 `tool_failure` 分流(`StopRun` → fail turn;默认 `ReturnErrorToRuntime` → 回灌)。

*测试*:

- `config.rs`:`external_machine_config_defaults_are_permissive`、`external_machine_config_roundtrip`
  (serde DTO round-trip + default 为空对象仅含 `tool_failure`)。
- `machine/tests.rs`:`external_loop_limit_fails_before_unbounded_pause_loop`(TODO 指定;limit=2,第 3 次
  round-trip 前 `LimitExceeded` 挡住无界 pause loop,计数=3,pending turn 丢弃)、
  `external_default_config_leaves_decision_loop_unbounded`、`external_tool_failure_stop_run_fails_turn`、
  `external_tool_failure_default_returns_error_to_runtime`、
  `external_require_host_tools_reports_unsupported_capability`、
  `external_require_subagents_reports_unsupported_capability`、
  `external_require_host_tools_without_source_keeps_generic_error_when_unset`(未声明时仍走通用错误,证明
  require 标志非 no-op)。
- `state.rs`:`external_agent_state_decision_loops_persist_and_skip_when_zero`(零值跳过快照 + round-trip 持久)。

*文档同步*(`docs/managed-external-agent.md`,markdown-only):§7 由「拟新增」改为「已落地(M4-3)」,写实际
两半职责、config DTO 字段与 builder、loop/tool-failure/capability 行为约定;§6.3 补 `max_decision_loops`
落地说明;能力表 tool failure policy 行改为「已落地(M4-3)」。

*验证*(完整序列 1-6 全过):

- `cargo fmt --all -- --check` clean。
- 聚焦:M4-3 相关 12 个测试全 passed。
- `cargo clippy --all-targets -- -D warnings` 0 warning(loop-limit `if let ... && ...` 合并 collapsible_if)。
- `cargo test --all --all-targets` 全绿(38 个 test binary `test result: ok`,0 failed)。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- `git diff --check` clean。

### [DONE] M4-4 Review：stream/capability/policy 完整性检查

**上下文**:

M4 结束后 adapter 可以根据能力决定支持、降级或拒绝 managed 功能。

**做什么**:

- 检查所有 runtime-dependent 功能都有 capability 表达:
  - streaming
  - resume
  - permission bridge
  - host tools
  - host subagents
  - artifacts
  - usage
  - graceful shutdown
- 检查 `ExternalSessionPolicy` 和 machine config 的职责边界。
- 检查 live sink 没有变成 blocking effect。
- 更新 `docs/capability-matrix.md` 的占位章节或待填表,不要声称未验证 runtime 支持。

**验证条件**:

- `cargo test -p agent-lib external_capabilities`
- `cargo test -p agent-lib external::sink`
- 完整验证序列 1-6 全过。
- 完成记录中列出 capability fallback 策略。

**完成记录**(2026-07-17):

纯 sign-off review，逐条核对 M4-1..M4-3 落地的 stream(live sink)/capability model/session policy
源码 + 测试。唯一改动是文档 `docs/capability-matrix.md`（新增「Managed External Runtime 能力模型」
章节）与 `memory/claude_plan.md`；无 `.rs` 改动、无 spec 偏差、无 workaround。

*Review 检查清单(全部满足)*:

1. **所有 runtime-dependent 功能都有 capability 表达**:`src/agent/external/capability.rs` 的
   `ExternalCapability` 枚举覆盖 8 项 —— `Streaming` / `Resume` / `PermissionBridge` / `HostTools` /
   `HostSubagents` / `Artifacts` / `Usage` / `GracefulShutdown`,与 TODO checklist 一一对应;
   `ExternalCapability::ALL[8]` 穷举全部;`ExternalRuntimeCapabilities` 为每项持有同名 `bool` 字段,
   `supports(cap)` 逐项映射,无遗漏 runtime-dependent 特性。
2. **`ExternalSessionPolicy` 与 machine config 职责边界清晰**:`ExternalSessionPolicy`(runtime-facing:
   `permission_mode`/`isolation`/`max_turns`/`stream_events`)随 `ExternalSessionRequest` 转发给 runtime;
   `ExternalAgentMachineConfig`(machine-local:`tool_failure`/`required_capabilities`/`max_decision_loops`)
   仅 machine 自身在桥接时强制执行,是纯数据 serde DTO,**不进入**可序列化的 `ExternalAgentState`,与
   live 身份源(`RequirementIds`/`ToolExecutionIds`)各自 builder 注入互不污染。runtime 侧 `max_turns`
   与 machine 侧 `max_decision_loops` 语义不重叠。两者 `Default` 均保守/宽松,不配置时行为与引入前一致。
3. **live sink 不是 blocking effect**:`ExternalEventSink::emit(&self, &ExternalObservedEvent) -> ()`
   无返回值、可自由丢弃事件;machine 从不持有 sink;只有 `Requirement` 能阻塞 continuation。
   exact-once 回放由 `ExternalSessionResult::observations` 按 `seq` 去重独家保证,sink 只是 lossy
   实时镜像。焦点测试 `external::sink`(discard + collecting)证实旁路被动、不扰动 buffered observations。
4. **`docs/capability-matrix.md` 已更新**:新增 managed external 能力章节,列 8 capability + serde 标签 +
   决策点 + 保守默认;各 runtime(ClaudeCode/Codex/OpenCode/Custom)当前全部标「未验证(`false`)」为待填表,
   明确「不声称任何已验证 runtime 支持」「不是任一 runtime 的服务等级承诺」。

*Capability fallback 策略摘要*:

| 场景 | 处置 |
|---|---|
| 保守基线(无探测/adapter) | `conservative_capabilities()` / `none(runtime)` → 全 `false`,不假设支持 |
| 声明 required 的能力缺失 | `ExternalAgentError::UnsupportedCapability{runtime,capability,detail}` 分类错误,scheduler 据此避免再 dispatch |
| 未声明 required 的能力缺失 | 保留原通用错误(如 `tool id unavailable`),兼容 pre-M4-3 |
| host 工具调用失败 | `ExternalToolFailurePolicy::ReturnErrorToRuntime`(默认,回灌 failed result) / `StopRun`(停 turn) |
| decision loop 超 `max_decision_loops` | `ExternalAgentError::LimitExceeded`,防无界 pause/respond |
| streaming 旁路 | `ExternalStreamPolicy::{Buffered(默认)/Streaming/Disabled}`;sink 可丢事件,exact-once 由 observations+seq 保证 |

*验证*(完整序列 1-6):

- `cargo fmt --all -- --check` FMT_OK。
- 聚焦:`cargo test -p agent-lib external_capabilities`(1 passed:`external_capabilities_roundtrip`)、
  `cargo test -p agent-lib external::sink`(2 passed:`discard_sink_accepts_and_drops_events`、
  `collecting_sink_records_sequenced_events_for_tests`)。
- `cargo clippy --all-targets -- -D warnings` 0 warning。
- `cargo test --all --all-targets`:本任务仅文档改动(无任何 `.rs` 变更),复用 M4-3 全套件绿快照;
  按 PROMPT「仅文档改动可复用上次 full run」跳过重跑。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- `git diff --check` clean。

---

## Milestone 5 — runtime adapter abstraction 与 scripted/cassette handler

目标:在不接真实 CLI 的情况下,先把 adapter/session registry/handler 分层建起来,用 scripted runtime 覆盖
完整 managed loop。

### [DONE] M5-1 定义 `ExternalRuntimeAdapter` / `ExternalRuntimeSession` / `ExternalSessionRegistry`

**上下文**:

- `ExternalSessionHandler` 当前只有 `fulfill(&ExternalSessionRequest, &RunContext)`。
- `ExternalRuntimeHandles` 只是泛型 holder,还没有统一 adapter trait。
- 真实 runtime 需要 session registry 管理 process/SDK handle、resume、cleanup。

**做什么**:

- 在 `src/agent/external/runtime.rs` 或新子模块中定义:
  - `ExternalRuntimeAdapter`
  - `ExternalRuntimeSession`
  - `ExternalSessionRegistry`
  - `RuntimeDecisionPoint` 如果需要 adapter 内部与 DTO 分离。
- trait 设计应满足:
  - adapter 返回 `ExternalRuntimeCapabilities`。
  - start/resume/advance 到下一 decision point。
  - cleanup 返回 `ExternalSessionShutdown`。
  - live sink 可选注入。
  - adapter 错误统一映射到 `ExternalAgentError`。
- 实现一个 registry skeleton:
  - 按 `ExternalSessionRef` 或 `(agent_id, session_id)` 查找 live session。
  - `cleanup_agent(agent_id)` 支持 cancel sweep。
  - 不把 live handles 放入 `ExternalAgentState`。

**验证条件**:

- trait object safe 或明确说明为何不需要 dyn-safe。
- registry unit 覆盖:
  - start 后可 get/resume。
  - cleanup 移除 live handle。
  - unknown session 映射 `ResumeUnavailable`。
- `cargo test -p agent-lib external_runtime_registry`
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增 `src/agent/external/adapter.rs`(adapter/session 抽象层):
  - `RuntimeDecisionPoint`:adapter 内部四个非失败决策点
    (`Completed` / `PausedForInteraction` / `PausedForToolCalls` / `PausedForSubagent`),
    字段与 `ExternalSessionResult` 非 `Failed` 变体一一对应;提供 `session()`/`observations()`
    访问器与 `into_session_result()`。
  - `impl From<Result<RuntimeDecisionPoint, ExternalAgentError>> for ExternalSessionResult`:
    `Ok` -> 对应非失败变体,`Err` -> `Failed`,并从 `SessionLost`/`ResumeUnavailable`/`ShutdownFailed`
    error 提取 `session` 抬进 `Failed.session`(其余变体 -> `None`),`observations` 置空。
  - `ExternalRuntimeSession`(trait, `Send`, object-safe):`session_ref()` +
    `async advance(input, ctx) -> Result<RuntimeDecisionPoint, ExternalAgentError>` +
    `async shutdown() -> ExternalSessionShutdown`。
  - `ExternalRuntimeAdapter`(trait, `Send + Sync`, object-safe):`kind()`/`capabilities()` +
    `async start(request, ctx, sink?)` / `async resume(session_ref, request, ctx, sink?)`
    返回 `Box<dyn ExternalRuntimeSession>`;`resume` 默认实现返回 `ResumeUnavailable`,
    支持 resume 的 adapter override。live sink 通过 `Option<Arc<dyn ExternalEventSink>>` 可选注入。
- 新增 `src/agent/external/registry.rs`(registry skeleton):
  - `LiveSessionKey { agent_id, session_id }`(`session_id` 为 `None` 时不可 key)。
  - `LiveSessionHandle = Arc<tokio::sync::Mutex<Box<dyn ExternalRuntimeSession>>>`(pub 别名)。
  - `ExternalSessionRegistry { adapter: Arc<dyn ExternalRuntimeAdapter>, live: std::sync::Mutex<HashMap<..>> }`:
    - `get_or_start`:`session=None` -> `adapter.start`+register;`session=Some` 命中 live handle ->
      reattach(同一 `Arc`);未命中且 `capabilities().resume` -> `adapter.resume`+register,
      否则返回 `ExternalAgentError::ResumeUnavailable`。
    - `get`:按 `(agent_id, session_ref)` 纯查找。
    - `cleanup`:移除并 `shutdown` 单 session,返回 `ExternalSessionShutdown`(缺失 -> `Graceful`)。
    - `cleanup_agent`:cancel sweep 单 agent 全部 session,按 session_id 排序确定性返回 dispositions。
    - `kind()`/`capabilities()`/`live_len()` passthrough;live handle 绝不进 `ExternalAgentState`。
    - 私有 `register`/`store`:`register` 返回小型 `RegisterError`(仅 `MissingSessionId`)以避免
      `clippy::result_large_err`;`store` 在锁内二次检查防并发注册泄漏 handle。
- `mod.rs` 导出 `ExternalRuntimeAdapter`/`ExternalRuntimeSession`/`RuntimeDecisionPoint`/
  `ExternalSessionRegistry`/`LiveSessionHandle`。
- 对象安全性:两 trait 仅有非泛型同步方法(返回 owned 值)+ `#[async_trait]` 装箱 future,
  由 registry 持有 `Arc<dyn Adapter>` / `Box<dyn Session>` 在编译期证明 dyn-safe(见 adapter.rs 模块 doc)。
- 测试:
  - `registry.rs` 8 个 `external_runtime_registry_*` 单测:start 注册 + get 命中、Continue reattach 同一 handle、
    resume 成功注册、unknown -> `ResumeUnavailable`、cleanup 移除+关闭、cleanup 缺失 -> `Graceful`、
    cleanup_agent 只 sweep 本 agent、cleanup_agent 空 agent。
  - `adapter.rs` 7 个单测:四个 `into_session_result` 变体映射 + `Ok`/`Err`(带/不带 session)折叠。
- 验证序列 1-6 全过:fmt --check;`cargo test -p agent-lib external_runtime_registry`(8 passed)+
  adapter 单测(7 passed);`clippy --all-targets -D warnings` clean;`cargo test --all --all-targets`
  全绿(38 个 test binary,0 failed,lib 628 passed);`RUSTDOCFLAGS=-D warnings cargo doc` clean;
  `git diff --check` clean。
- 后续 adapter(M5-2 起)只需实现 `ExternalRuntimeAdapter`/`ExternalRuntimeSession` 两 trait,
  错误统一走 `ExternalAgentError`,决策点走 `RuntimeDecisionPoint`,由 `ExternalSessionRegistry` 托管 live handle。

### [DONE] M5-2 实现 scripted external runtime adapter

**上下文**:

- 默认测试不能依赖真实 CLI/API。
- 现有 machine tests 直接构造 `ExternalSessionResult`,但缺少 `drain + ExternalSessionHandler`
  层面的完整 managed loop 覆盖。

**做什么**:

- 实现 test/support 用 `ScriptedExternalRuntimeAdapter`:
  - 输入脚本是一串 decision point。
  - 能断言收到的 `ExternalSessionInput` 序列。
  - 能产出 `Completed` / `PausedForInteraction` / `PausedForToolCalls` / `PausedForSubagent` /
    `Failed`。
  - 能向 live sink emit sequenced events。
- 实现 `ExternalSessionHandler` 包装 scripted registry。
- 位置可选:
  - 若只供测试,放在 `src/agent/external/machine/tests.rs` 或 `tests/agent_external_*`。
  - 若后续 cassette 复用,放在 `src/agent/external/testing.rs` 并 `#[cfg(test)]`。

**验证条件**:

- integration/unit 覆盖:
  - Start -> Completed through `drain`。
  - Start -> PausedForToolCalls -> NeedTool -> RespondToolResults -> Completed。
  - Start -> PausedForInteraction -> RespondInteraction -> Completed。
  - Start -> PausedForSubagent -> RespondSubagent -> Completed。
- 所有测试离线。
- 聚焦测试:
  - `cargo test -p agent-lib scripted_external`
  - `cargo test -p agent-lib external_agent_start_to_completed`
- 完整验证序列 1-6 全过。

**完成记录**:

- 将 `crates/agent-testkit/src/external.rs` 用 `git mv` 拆成目录模块
  `crates/agent-testkit/src/external/mod.rs` + 子模块 `runtime.rs`;`mod.rs` 保留既有
  `ScriptedExternalSessionHandler`/`ExternalAgentFixture`(short-circuit 直接产 `ExternalSessionResult`
  的旧路径),新增 `pub mod runtime;` + re-export,并补 fixture 辅助 `machine_with_tool_ids()`
  (给 machine 注入确定性 tool-execution ids)、`tool_batch_id()`、`tool_call()`。
- 新增 `crates/agent-testkit/src/external/runtime.rs`(scripted runtime 层,运行在 handler 之下):
  - `ScriptedAdvance`:单个脚本步骤。构造子 `completed` / `paused_for_interaction` /
    `paused_for_tool_calls` / `paused_for_subagent` / `failed`;链式 `.expecting(ExternalInputKind)`
    (断言收到的 `ExternalSessionInput` 种类,不符即 panic)与 `.emitting(events)`(向 live sink
    发的事件)。
  - `ScriptedExternalRuntimeSession`(impl `ExternalRuntimeSession`):持 `VecDeque<ScriptedAdvance>` +
    固定 `session_id` + 单调 `next_seq`/`last_event_seq` + 可选 `Arc<dyn ExternalEventSink>`。
    `advance` 弹一步、可选断言 input kind、按单调 seq 把事件既 emit 到 sink 又缓冲成
    `ExternalObservedEvent` observations、返回对应 `RuntimeDecisionPoint`(`Failed` -> `Err`);脚本耗尽
    返回 `ExternalAgentError::Runtime{message:"...advanced past its script"}`。`shutdown` -> `Graceful`。
  - `ScriptedExternalRuntimeAdapter`(impl `ExternalRuntimeAdapter`):`Mutex<VecDeque<VecDeque<ScriptedAdvance>>>`
    每次 `start` 领一份脚本并把 start request 记进 `ScriptedRuntimeStartLog`;`capabilities().resume=false`
    强制 registry 走 live-handle reattach 而非 `adapter.resume`;脚本用尽再 `start` -> `Launch` error。
  - `ScriptedRuntimeExternalSessionHandler`(impl `ExternalSessionHandler`):无 machine 状态,
    `registry.get_or_start(request, ctx, Some(sink))` -> `handle.lock().await.advance(&request.input, ctx)`
    -> `.into()`(复用 M5-1 的 `From<Result<RuntimeDecisionPoint, ExternalAgentError>>` 折叠成
    `RequirementResult::ExternalSession`);`get_or_start` 失败走 `Err::<RuntimeDecisionPoint,_>(e).into()`。
  - `ScriptedRuntimeBuilder`:`.new()` 默认 runtime=ClaudeCode、`session_id="scripted-sess-1"`、
    全 true 但 `resume=false` 的 capabilities;`.build()` 返回 handler,`.log()`/`.sink()`/`.start_log()`/
    `.registry()` 暴露 `ExternalAgentCallLog`/`ScriptedSinkLog`/`ScriptedRuntimeStartLog`/`Arc<ExternalSessionRegistry>`。
  - 本地 `input_kind()` 分类器(镜像 assertions 私有分类器)+ `permissive_capabilities()`。
  - 6 个 `#[cfg(test)]` `scripted_runtime_*` 单测(start->completed、tool、interaction、subagent、
    脚本耗尽错误、input-kind 断言)。
- `prelude.rs` re-export `ScriptedAdvance`/`ScriptedExternalRuntimeAdapter`/`ScriptedExternalRuntimeSession`/
  `ScriptedRuntimeBuilder`/`ScriptedRuntimeExternalSessionHandler`/`ScriptedRuntimeStartLog`/`ScriptedSinkLog`。
- 新增 `tests/agent_external_scripted.rs`:4 个离线 `scripted_external_*` drain 测试,经真实
  `ExternalAgentMachine::drain` + registry-backed handler 跑完整 managed loop:
  - `scripted_external_start_to_completed`:Start -> Completed。
  - `scripted_external_tool_batch_round_trip`:Start -> PausedForToolCalls -> NeedTool ->
    RespondToolResults -> Completed(machine 把 `ExternalToolCall` 桥成 `NeedTool`,
    `ScriptedToolHandler` 按 provider_call_id 键回结果)。
  - `scripted_external_interaction_round_trip`:Start -> PausedForInteraction -> RespondInteraction ->
    Completed(action_id "act-1" 对齐 `permission_request`)。
  - `scripted_external_subagent_round_trip`:Start -> PausedForSubagent -> RespondSubagent ->
    Completed(镜像 `agent_external_subagent.rs` 的 `ScriptedSubagentSpawner`/`attended_child_scope`)。
- 关键坑:`SeqIds::agent_id()` 每次调用 `next_uuid()` 自增,直接分别构造 start/continue request 会拿到
  不同 agent_id 从而破坏 registry 按 `(agent_id, session_id)` 的 reattach;单测里改成 clone start
  request 再改 `.session`/`.input` 复用同一 agent_id。integration drain 测试不受影响,因为 machine
  跨 turn 用稳定的 `spec.id()` 且 follow-up request 携带脚本回填的稳定 `session_id`。
- 验证序列 1-6 全过:`cargo fmt --all -- --check` OK;聚焦 `cargo test -p agent-lib scripted_external`
  (4 passed)+ `external_agent_start_to_completed`(pass);`clippy --all-targets -D warnings` clean;
  `cargo test --all --all-targets` 全绿(0 failed,lib 628 passed、agent-testkit 单测含 6 个新
  `scripted_runtime_*`);`RUSTDOCFLAGS=-D warnings cargo doc --no-deps --workspace` clean(修掉 mod.rs
  冗余显式链接 + runtime.rs `ExternalSessionResult` 未解析链接两处 doc 告警);`git diff --check` clean。
- 注:cassette/recorded 回放层(`CassetteExternalSessionHandler`,design §12)按计划留给 M5-3,本任务只覆盖
  scripted 路径。

### [DONE] M5-3 增加 cassette replay 层用于 runtime parser 回归

**上下文**:

- 真实 runtime adapter parser 会依赖 CLI JSON/JSONL 输出,需要用 cassette 防协议漂移。
- cassette 不应包含 secret、完整敏感 prompt、用户私有文件内容。

**做什么**:

- 定义 cassette 格式:
  - runtime kind/version/probe info。
  - input frames。
  - expected `ExternalObservedEvent` 和 decision point。
  - redaction metadata。
- 实现 cassette loader/parser test helper。
- 添加最小 synthetic cassette 覆盖:
  - text delta。
  - command start/finish。
  - permission request。
  - tool call。
  - completion。
- 暂不要求真实 Claude/Codex/OpenCode cassette,但目录结构要预留:
  - `tests/fixtures/external/claude_code/`
  - `tests/fixtures/external/codex/`
  - `tests/fixtures/external/opencode/`

**验证条件**:

- cassette loader 对未知字段保守处理:raw 保留或明确忽略并测试。
- redaction test 确认 fixture 不含 `API_KEY` / `AUTH_TOKEN` / `sk-` 等模式。
- `cargo test -p agent-lib external_cassette`
- 完整验证序列 1-6 全过。

**完成记录**:

- 明确边界:M3 `crate::cassette::Cassette` 录 provider-neutral effect req/resp;本任务的
  runtime **parser cassette** 低一层,针对真实 adapter(M6-M8)的 CLI JSON/JSONL parser,冻结
  「原始帧 → 解析出的 sequenced observations + decision point」以防协议漂移。两者独立并存。
- 新增 `crates/agent-testkit/src/external/cassette.rs`(schema + loader + redaction + replay):
  - **Schema**(serde,`EXTERNAL_CASSETTE_SCHEMA_VERSION=1`):
    - `ExternalRuntimeCassette { schema_version, runtime: CassetteRuntimeInfo, redaction:
      RedactionMetadata, turns: Vec<CassetteTurn>, #[serde(flatten)] extra: BTreeMap<String,Value> }`。
    - `CassetteRuntimeInfo { kind: ExternalRuntimeKind, version?, probe?, session_id?, #[flatten] extra }`
      —— 满足「runtime kind/version/probe info」。
    - `CassetteTurn { expect_input?: CassetteInputKind, input_frames: Vec<CassetteFrame>,
      expected_events: Vec<ExternalObservedEvent>, decision: CassetteDecision, #[flatten] extra }`
      —— 满足「input frames + expected ExternalObservedEvent + decision point」。
    - `CassetteFrame { stream: CassetteStream(Stdout/Stderr,default Stdout), payload: String, #[flatten] extra }`
      (原始 CLI 行,opaque)。
    - `CassetteDecision`(内部 tag=`kind`,snake_case){ Completed{output} / PausedForInteraction{action_id,request} /
      PausedForToolCalls{batch_id,calls} / PausedForSubagent{request} / Failed{error} } —— 镜像
      `RuntimeDecisionPoint` 五路(session/observations 由回放 session 补,不重复存)。
    - `RedactionMetadata { applied, placeholder?, notes? }` —— 满足「redaction metadata」。
    - `CassetteInputKind`(镜像 `ExternalSessionInput` 判别式)+ `CassetteInputKind::classify`。
  - **Loader/parser helper**:`from_json_str`(先读校 `schema_version` 再 full parse)/ `load(path)` 磁盘加载 /
    `to_json_string[_pretty]`;错误 `ExternalCassetteError`(手写 Display/Error,testkit 无 thiserror)
    分类 `Serialize/Deserialize/Io/MissingSchemaVersion/UnsupportedSchemaVersion`。**未知字段保守**:
    顶层/runtime/turn/frame 全部 `#[serde(flatten)] extra: BTreeMap` 原样保留,不丢不报错,可 round-trip。
  - **Redaction**:`scan_secrets(&str) -> Vec<SecretHit>`(扫 `API_KEY`/`AUTH_TOKEN`/`secret_key`/`-----BEGIN`
    大小写不敏感 + `sk-`/`AKIA`/`Bearer ` 大小写敏感);`ExternalRuntimeCassette::assert_no_secrets()`
    序列化后扫描,命中列出 pattern@offset 并 panic。
  - **Replay 层**(复用 M5-1 `ExternalSessionRegistry` + M5-2 `ScriptedSinkLog`/`ScriptedRuntimeStartLog`/
    `ExternalAgentCallLog`,`ScriptedRuntimeStartLog::record` 提升为 `pub(crate)`):
    `CassetteExternalRuntimeSession`(impl `ExternalRuntimeSession`,**按记录 seq 原样 emit** 而非像 scripted
    那样重排,故 seq 漂移会被抓到)/ `CassetteExternalRuntimeAdapter`(impl `ExternalRuntimeAdapter`,
    resume=false 走 live-handle reattach)/ `CassetteRuntimeExternalSessionHandler`(impl `ExternalSessionHandler`,
    即设计 §12 的 `CassetteExternalSessionHandler`,`from_cassette` 从已加载 cassette 组装 registry-backed handler)。
  - 8 个 `#[cfg(test)]` 单测(round-trip、未知字段保留、未知/缺失 schema version、默认 stream、
    scan_secrets、assert_no_secrets 通过/panic)。
- `external/mod.rs` `pub mod cassette;` + re-export;`prelude.rs` 追加导出全部公共类型。
- **Synthetic fixtures**(`tests/fixtures/external/synthetic/`):
  - `full_stream.json`:单 turn Start→Completed,observations 覆盖任务要求的五类
    (text_delta / command_started+command_finished / permission_requested / tool_started+tool_finished /
    session_completed),drain 到 Done 无需 tool/interaction handler;由 env-gated 生成器写出。
  - `forward_compat.json`:手写,顶层/runtime/turn/frame 各带未知字段,验证保守保留。
  - 预留目录 `tests/fixtures/external/{claude_code,codex,opencode}/`(各 `README.md` 说明留待 M6-M8 真实录制)。
- 新增 `tests/agent_external_cassette.rs`:9 个 `external_cassette_*`(loads_synthetic_fixture、
  replay_drains_to_done、replay_tool_batch/interaction/subagent_round_trip(in-code 造 cassette →
  JSON round-trip → 经真实 `ExternalAgentMachine::drain` 回放整条 managed loop)、
  rejects_unknown_schema_version、preserves_unknown_fields、fixtures_are_redacted、
  regenerate_fixtures(env-gated,`AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` 才写盘,正常 no-op))。
- 验证序列 1-6 全过:`cargo fmt --all -- --check` OK;`cargo test -p agent-lib external_cassette`(9 passed);
  `cargo clippy --all-targets -D warnings` clean;`cargo test --all --all-targets` 全绿(40 个 test binary,
  0 failed,含 8 个新 lib 单测 + 9 个新 integration + testkit 既有全过);`RUSTDOCFLAGS=-D warnings cargo doc
  --no-deps --workspace` clean(去掉两处指向私有 `SECRET_PATTERNS` 的 doc 链接);`git diff --check` clean。
- 注:真实 Claude/Codex/OpenCode cassette 与其 stream parser 按计划留给 M6-M8;M5-4 review 将据此
  核对 cassette schema 文档化/脱敏与回放路径完整性。

### [DONE] M5-4 Review：runtime abstraction 与离线 e2e 完整性检查

**上下文**:

M5 是真实 adapter 前的边界冻结点。review 要确认后续 Claude/Codex/OpenCode 只是在 adapter 层填 parser 和
process 管理,不需要改 machine/driver。

**做什么**:

- 检查 `ExternalSessionHandler` 生产路径是否只组合 registry + adapter,不包含 machine 状态逻辑。
- 检查 scripted tests 是否覆盖:
  - tool。
  - interaction。
  - subagent。
  - mixed tool + subagent。
  - observations live sink + buffered replay。
  - cancel cleanup。
- 检查 cassette schema 文档化并脱敏。
- 更新 `docs/managed-external-agent.md` runtime adapter 章节的实现状态。

**验证条件**:

- `cargo test -p agent-lib scripted_external`
- `cargo test -p agent-lib external_cassette`
- 完整验证序列 1-6 全过。
- 完成记录中列出真实 adapter 必须实现的 trait 方法和错误映射。

**完成记录**:

- **生产 handler 边界(只组合 registry + adapter,无 machine 状态)** —— 已核对通过:
  `ScriptedRuntimeExternalSessionHandler`(`crates/agent-testkit/src/external/runtime.rs`)与
  `CassetteRuntimeExternalSessionHandler`(`.../external/cassette.rs`)都持有 `Arc<ExternalSessionRegistry>` +
  sink/log,**不持有任何 machine 状态**;每次 `fulfill` = `registry.get_or_start(request, ctx, sink)` →
  `session.advance(&request.input, ctx)` → `RuntimeDecisionPoint`/`Err` 经 `From` 折成
  `RequirementResult::ExternalSession`。`ExternalSessionRegistry`(`src/agent/external/registry.rs`)
  "keeps no serializable state, so it never appears in `ExternalAgentState`",跨 turn 的状态全在
  `ExternalAgentMachine`,adapter 层无重复。
- **scripted / 离线 e2e 覆盖核对**:
  - tool —— `scripted_external_tool_batch_round_trip` + `external_cassette_replay_tool_batch_round_trip`。
  - interaction —— `scripted_external_interaction_round_trip` + `external_cassette_replay_interaction_round_trip`。
  - subagent —— `scripted_external_subagent_round_trip` + `external_cassette_replay_subagent_round_trip`。
  - **mixed tool + subagent —— review 发现缺口,本任务补测试** `scripted_external_mixed_tool_and_subagent_round_trip`
    (`tests/agent_external_scripted.rs`):单 live session 三次 advance
    `Start→PausedForToolCalls` / `RespondToolResults→PausedForSubagent` / `RespondSubagent→Completed`,
    断言 `start_log.len()==1`(tool 与 subagent bridge 都 reattach 同一 handle,不 restart)、
    3 次 external call 的 input/result kind 序列、tool 执行 1 次、child 驱动 1 次、conversation 提交 1 turn。
  - observations live sink —— `scripted_external_start_to_completed` / `external_cassette_replay_drains_to_done`
    断言 `sink.seqs()` 单调 seq 线;buffered replay —— `machine/tests.rs::external_agent_emits_observation_notifications`
    等断言 buffered `observations` 经 `last_event_seq` dedup 后转 `Notification::ExternalAgent`(设计 §5.5)。
  - cancel cleanup —— registry 单测 `external_runtime_registry_cleanup_removes_and_closes_handle` /
    `..._cleanup_missing_session_is_graceful` / `..._cleanup_agent_sweeps_only_that_agent` /
    `..._cleanup_agent_without_sessions_is_empty`,managed-loop 级
    `agent_external_lifecycle.rs::external_agent_abandon_settles_and_flags_cleanup`(cancel 前置 drain 标 `cleanup_required`)。
- **cassette schema 文档化 + 脱敏** —— 已核对:schema(version 1)在 `cassette.rs` rustdoc 与
  `docs/managed-external-agent.md` §11.4 记录;未知字段 `#[serde(flatten)]` 保守保留可 round-trip
  (`external_cassette_preserves_unknown_fields`);脱敏由 `scan_secrets`/`assert_no_secrets` +
  `external_cassette_fixtures_are_redacted` 保证(扫 `API_KEY`/`AUTH_TOKEN`/`sk-`/`-----BEGIN` 等)。
- **docs 更新** —— `docs/managed-external-agent.md`:§1 能力表刷新 6 行(runtime adapter 抽象 / registry /
  host tool / host subagent / streaming sink+replay / cassette 均标注 M5 已落地 + 真实 adapter 待 M6-M8);
  §11 新增 **§11.4 实现状态(M5,已落地)**,记录实际 trait/类型形状(与草案的差异:`advance(input)` 单方法
  取代草案的 start/continue/respond_* 多方法;registry 是具体 struct 非 trait)、真实 adapter 必须实现的
  trait 方法与错误映射、runtime-parser cassette 层位置与脱敏。
- **真实 adapter(M6-M8)必须实现的 trait 方法(应验证条件)**:
  - `ExternalRuntimeAdapter`:`kind()`、`capabilities()`(probe 确认后逐项开启,基线 `none()`)、
    `async start(request, ctx, sink) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError>`;
    支持 resume 时 override `async resume(session, request, ctx, sink) -> Result<Box<dyn ExternalRuntimeSession>, _>`。
  - `ExternalRuntimeSession`:`session_ref() -> ExternalSessionRef`(start/resume 后必须带 `session_id`)、
    `async advance(&mut self, input: &ExternalSessionInput, ctx) -> Result<RuntimeDecisionPoint, ExternalAgentError>`
    (只驱动到下一 decision point,禁一次跑到底)、`async shutdown(&mut self) -> ExternalSessionShutdown`。
  - `advance` 返回 `RuntimeDecisionPoint` 五路:`Completed` / `PausedForInteraction` / `PausedForToolCalls` /
    `PausedForSubagent`(均带 buffered `observations`),失败走 `Err` → machine 折 `ExternalSessionResult::Failed`。
- **真实 adapter 必须实现的错误映射(`ExternalAgentError` 分类,禁 ad-hoc / panic)**:
  `Launch{runtime,detail}`(启动失败)、`Protocol{..}`(wire schema 漂移 / 缺 `session_id`)、
  `SessionLost{session,..}`、`ResumeUnavailable{session,detail}`(无 live handle 且不支持 resume)、
  `ShutdownFailed{session,..}`、`LimitExceeded{limit}`、`UnsupportedCapability{..}`、
  `Runtime{code,message}`(兜底)。
- **结论**:边界冻结成立 —— M6-M8 真实 adapter 只需实现上述两 trait 的方法 + 错误映射 + parser/process 管理,
  registry / machine / driver / handler 组合形状无需改动。
- 验证序列 1-6 全过:`cargo fmt --all -- --check` OK;`cargo test -p agent-lib scripted_external`(5 passed,
  含新增 mixed);`cargo test -p agent-lib external_cassette`(9 passed);`cargo clippy --all-targets -D warnings`
  clean;`cargo test --all --all-targets`(40 个 test binary 全绿,0 failed);
  `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --workspace` clean;`git diff --check` clean。

---


## Milestone 6 — Claude Code managed adapter

目标:实现 feature-gated Claude Code adapter,支持 stream-json 解码、permission bridge、tool/subagent bridge
能力探测；真实 e2e ignored。

### [DONE] M6-1 增加 Claude Code capability probe 与启动配置

**上下文**:

- runtime adapter 不应假定本机已安装或已登录 Claude Code。
- 测试默认不能依赖 Claude Code。
- `ExternalRuntimeKind::ClaudeCode` 已存在。

**做什么**:

- 新增 feature gate,例如 `external-claude-code`。
- 新增 Claude Code adapter 配置:
  - binary path/env override。
  - working directory/worktree。
  - permission mode 映射。
  - optional model/profile。
  - timeout。
- 实现 probe:
  - 检查 binary/version。
  - 检查支持的 output format / stream mode。
  - 返回 `ExternalRuntimeCapabilities`。
- probe 失败必须返回 `ExternalAgentError::Launch` 或 `UnsupportedCapability`,不 panic。

**验证条件**:

- 无 Claude Code binary 时,unit/probe test 可用 fake command 或 scripted probe 验证错误分类。
- 不泄露 env secret。
- `cargo test -p agent-lib claude_code_probe`
- `cargo test --all --all-targets` 在未启 feature 时通过。
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增非默认 feature gate `external-claude-code`（`Cargo.toml`）。开启才编译 adapter；探测复用
  tokio 的 process 支持，不引入新重依赖。
- 新增 feature-gated 模块 `src/agent/external/claude_code/{mod.rs,config.rs,probe.rs}`，在
  `src/agent/external/mod.rs` 以 `#[cfg(feature = "external-claude-code")]` 挂载并 re-export
  `ClaudeCodeConfig`、`ClaudeCodeProbeExec`、`ProbeOutput`、`SystemClaudeCodeExec`、`probe`、
  `probe_with_exec`。为 M6-2/M6-3 预留目录，但本任务只填 config + probe。
- `ClaudeCodeConfig`：binary path / env override / working dir(worktree) / permission mode /
  optional model / profile / timeout。serde round-trip（可持久化）；手写 `Debug` 脱敏 env 值
  （只印 key + `<redacted>`）。`permission_mode_arg()` 映射 `ExternalPermissionMode` → Claude CLI
  `--permission-mode`（`Prompt→default`/`AcceptEdits→acceptEdits`/`Plan→plan`/
  `BypassPermissions→bypassPermissions`）；`base_session_args()` 产出结构化流启动参数。
- probe：跑 `--version`（缺失/损坏/非零退出 → `ExternalAgentError::Launch`）+ `--help`（空输出 →
  `Launch`；从开关保守探测能力位）。无 `stream-json` 结构化流 → `UnsupportedCapability{Streaming}`。
  能力探测保守：streaming/usage/artifacts 由 `--output-format stream-json`+`--input-format` 决定，
  permission_bridge←`--permission-mode`，resume←`--resume`/`--continue`，host_tools←`--mcp-config`，
  graceful_shutdown=true，host_subagents=false（留待 M6-3）。永不 panic。探测走可注入
  `ClaudeCodeProbeExec`（生产实现 `SystemClaudeCodeExec` 用 `tokio::process`，kill_on_drop + timeout）。
- 测试（`src/agent/external/claude_code/*` 内联，13 个）：config 默认/权限映射/启动参数/serde
  round-trip/Debug 脱敏；probe full-capability 探测、缺 binary→Launch、非零 version→Launch、空 help→Launch、
  无 stream-json→Unsupported、env secret 不泄露（Display+Debug 均断言）、真实 `SystemClaudeCodeExec`
  对不存在 binary→Launch（离线、不 panic）、`detect_capabilities` 未广告即 false。fake exec 覆盖全部
  分类，无需真实 Claude Code、无网络。
- 文档：`docs/managed-external-agent.md` §12.1 增补「实现状态（M6-1）」；`docs/capability-matrix.md`
  保守基线段落说明 feature-gated 探测存在但仍非 e2e 实测，Claude Code 行待 M6-3/M6-4 翻真。
- 验证：`cargo fmt --all -- --check` 干净；`cargo test -p agent-lib --features external-claude-code
  --lib claude_code_probe`（7 passed）与全量 `claude_code`（13 passed）；`cargo clippy --all-targets
  -- -D warnings` 与 `--features external-claude-code` 均 0 warning；`cargo test --all --all-targets`
  （未启 feature）40 个 test binary 全 ok、0 failed；`cargo test -p agent-lib claude_code_probe`
  （未启 feature）0 test（feature 关闭时模块不编译）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
  --workspace`（含 `--features external-claude-code`）干净；`git diff --check` 干净。
- Claude Code 真实 e2e 未在本任务范围（属 M6-3）；本机未运行真实 CLI。

### [DONE] M6-2 实现 Claude Code stream decoder cassette 测试

**上下文**:

- Claude Code 的 raw stream schema 是 adapter 私有协议,不得直接暴露为 public DTO。
- parser 输出必须是 `ExternalObservedEvent` 和 `ExternalSessionResult` decision point。

**做什么**:

- 新增 Claude Code raw frame parser:
  - text delta -> `ExternalAgentEvent::TextDelta`。
  - command/tool/patch/permission frame -> 对应 event 或 decision point。
  - completion -> `Completed`。
  - tool call -> `PausedForToolCalls`。
  - permission/question -> `PausedForInteraction`。
- 对未知 frame:
  - 保留 raw 到 event/diagnostic 或返回 `ExternalAgentError::Protocol`,策略要稳定。
- 添加 cassette fixture 和 parser tests。

**验证条件**:

- cassette parser tests 覆盖 text、permission、tool、patch、completion。
- parser 不需要真实 Claude Code。
- `cargo test -p agent-lib claude_code_cassette`
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增 feature-gated 私有 decoder `src/agent/external/claude_code/decoder.rs`：有状态、跨 turn
  单调 `seq` 的逐帧 `stream-json` 解码器。全程走 `serde_json::Value` 防御式导航,**不导出任何 raw
  frame 类型**,Claude 私有 wire schema 不进 `agent-lib` 稳定 API（design §12.2）。在
  `src/agent/external/claude_code/mod.rs` 挂载并经 `src/agent/external/mod.rs` 的
  `#[cfg(feature = "external-claude-code")]` re-export `ClaudeDecision`、`ClaudeDecodeContext`、
  `ClaudeStreamDecoder`。
- 公开 API：`ClaudeDecodeContext::new(step_id, actor)`（宿主身份,权限 `Interaction` 只绑宿主
  `step_id`/`actor`,绝不取自模型输出）、`ClaudeStreamDecoder::new(ctx)`、
  `push_line(&str) -> Result<Option<ClaudeDecision>, ExternalAgentError>`、
  `take_observations() -> Vec<ExternalObservedEvent>`、`session_id()`。turn 落定时返回中立
  `ClaudeDecision`（`Completed` / `PausedForToolCalls` / `PausedForInteraction` / `Failed`）。
- frame 映射：`system/init`→`SessionStarted`（捕获 session_id + cwd）；assistant `text`→`TextDelta`；
  `tool_use` `Bash`→`CommandStarted`,`Edit`/`Write`/`MultiEdit`/`NotebookEdit`/`Update`→`FilePatch`
  （summary=`"{name} {path}"`）,其它内建→`ToolStarted`,`mcp__*` 宿主桥接工具→折成
  `PausedForToolCalls` 批次（batch_id = assistant message `id`）；user `tool_result` 关联到已追踪的
  Bash→`CommandFinished`（is_error→exit_code 0/1）,否则→`ToolFinished`；`control_request`
  `can_use_tool`→`PermissionRequested` 观测 + `PausedForInteraction`；`result` `success`→
  `SessionCompleted` + `Completed`（cost_micros=round(total_cost_usd*1e6),usage 取自 result.usage）,
  error 子类型→`Failed`（`error_max_turns`→`LimitExceeded`,其余→`Runtime{code,message}`）。
- 容忍策略（稳定）：空行 / `stream_event` 部分帧 / 未知 `type` / 未知 content block / 未关联的
  `tool_result` → 容忍（`Ok(None)`,无观测）；非法 JSON / 非对象帧 / 缺字符串 `type` / 已知帧缺必需
  内层对象（assistant/user 无 `message`、control_request 无 `request_id`/`request`）→
  `ExternalAgentError::Protocol`。所有诊断均为固定字符串,永不夹带 prompt/tool input/凭据。
- 因跨 turn 观测由状态机按 `ExternalSessionRef::last_event_seq` 去重,单个 decoder 实例贯穿整个 session,
  `seq` 跨 turn 单调不重置；`take_observations()` 只 drain 待发观测而不重置 `next_seq`;决策产出时清空
  `active_tools`。
- 依赖环规避：`agent-testkit` 依赖 `agent-lib`,`agent-lib` dev-dep `agent-testkit`;若在 `src/` 内联
  用 `agent_testkit` 会得到两个 agent-lib 实例导致类型不一致。故 decoder 设为 feature-gated `pub`,
  全部测试放到 feature-gated 集成测试 `tests/agent_claude_code_cassette.rs`（agent-lib 只链一次）。
- committed cassette `tests/fixtures/external/claude_code/full_session.json`：三 turn（turn1 = text/
  command/patch/宿主 tool → `PausedForToolCalls`;turn2 = text/permission → `PausedForInteraction`;
  turn3 = text/completion → `Completed`,含 usage/cost）。由 in-code builder 经
  `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` 再生成;`assert_no_secrets` 保证无凭据。
- 测试（集成 7 个,均离线、无需真实 Claude Code）：regenerate guard、cassette 与 in-code builder 逐帧/
  逐决策一致、secret-free 扫描、full-session 全程回放断言观测流与每 turn 决策、容忍未知/空行帧、
  malformed→`Protocol`、error-result→`Failed`。`ExternalAgentError` 是模块 canonical 未装箱错误,
  decoder 同步小-`Ok` helper 触发的 `clippy::result_large_err` 以模块级 `#![allow]` + 说明注释显式接受,
  与 `adapter.rs`/`registry.rs`/`probe.rs` 的错误签名保持一致。
- 文档：`docs/managed-external-agent.md` §12.2 增补「实现状态（M6-2,已落地）」；
  `docs/capability-matrix.md` 保守基线段落说明离线 decoder 已存在但仍非 e2e;
  `tests/fixtures/external/claude_code/README.md` 从预留占位改为描述已落地的合成 decoder fixture。
- 验证：`cargo fmt --all -- --check` 干净；`cargo clippy --all-targets -- -D warnings` 与
  `--features external-claude-code` 均 0 warning；`cargo test --all --all-targets`（未启 feature）
  全 ok、0 failed；`cargo test --features external-claude-code --test agent_claude_code_cassette`
  7 passed；`cargo test -p agent-lib --test agent_claude_code_cassette`（未启 feature）0 test；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --features external-claude-code` 干净；
  `git diff --check` 干净。
- Claude Code 真实 e2e 未在本任务范围（属 M6-3）;本机未运行真实 CLI,仅回放合成 cassette。

### [DONE] M6-3 实现 Claude Code session adapter 与 ignored real e2e

**上下文**:

- M5 已有 registry/handler;本任务只接 Claude Code process/session。
- 真实测试必须 `#[ignore]`。

**做什么**:

- 实现 start/resume/advance/cleanup。
- 接入 live sink。
- 把 host tools 暴露给 Claude Code:
  - 如果 Claude Code 支持 MCP/custom tool,用 bridge 转 `PausedForToolCalls`。
  - 如果不支持,capability 返回 `host_tools=false`,需要 tool 的请求返回 `UnsupportedCapability`。
- ignored e2e:
  - 检测 `CLAUDE_CODE_BIN` 或 PATH。
  - 检测必要登录态/环境,缺失时 skip 或给出明确错误。
  - 使用临时 worktree。
  - 验证一个多步会话: text -> permission/tool -> completion。

**验证条件**:

- 默认测试不运行真实 Claude Code。
- `cargo test -p agent-lib claude_code_cassette` 通过。
- ignored 测试命令文档化,例如:
  - `cargo test --features external-claude-code --test external_claude_code -- --ignored --nocapture`
- 若本机环境具备 Claude Code,运行 ignored e2e 并在完成记录中记录结果；否则记录 skip 原因。
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增 feature-gated live adapter `src/agent/external/claude_code/adapter.rs`（唯一 pub 类型
  `ClaudeCodeAdapter`），把 M6-1 启动配方 + M6-2 私有 decoder 接进 M5 的
  `ExternalRuntimeAdapter`/`ExternalRuntimeSession` 抽象：
  - `ClaudeCodeAdapter`：`new(config)` 报告实现能力；`with_probed_capabilities(config,&probed)` 与 probe
    实测能力逐位取交；`kind`/`capabilities`/`start`/`resume`。`start`/`resume` 对携带 `tools` 的请求以
    `UnsupportedCapability{HostTools}` 拒绝（本 adapter 不跑 MCP server，§12.3 允许的分支）。
  - `ClaudeCodeSession<Io>`（私有，实现 `ExternalRuntimeSession`）：持有 CLI 子进程 + 跨全程单调 `seq` 的
    `ClaudeStreamDecoder`；`advance` 写 stdin `stream-json` 帧（`Start`/`Continue`→`user` 文本帧、
    `RespondInteraction`(权限)→`control_response` allow/deny、`RespondToolResults`/`RespondSubagent`→
    `UnsupportedCapability`），逐行读 stdout 喂 decoder、镜像观测到 live sink，driving 到下一个
    `RuntimeDecisionPoint`（EOF 未决策→`SessionLost`、非法帧→`Protocol`）；`shutdown` 丢 stdin→在 grace 内
    等优雅退出，超时 `start_kill`→`ForcedKill`。
  - IO 经私有 `ClaudeSessionIo` trait 注入：生产 `ClaudeProcessIo`（tokio::process，stderr 丢弃、
    kill_on_drop、per-read timeout），单测注入 `FakeIo` 回放固定帧并捕获 stdin，离线跑通全状态机。
  - decode context：`StepId::new(*ctx.run_id().as_uuid())` + actor=`request.agent_id`（不随机/取时钟）。
- **真机时序修正（关键）**：真实 CLI（`--print --output-format stream-json --input-format stream-json`）在
  收到首条 stdin `user` 帧前不产出任何 frame（连 `system/init` 都不发）。原实现的 `begin` 假设 init 先于
  stdin、先读后写，导致 start prelude 阻塞到读超时（`SessionLost{TimedOut}`）。已按真实协议改为：fresh
  `start` 先写首个 prompt 帧再读 init 拿 session_id，该 turn 余帧由第一次 `advance` 续读（`first_turn_pending`
  标记避免重复写）；`resume` 已知 id，`begin` 不预读，首次 `advance` 写续跑 turn 并读新 init。新增
  `..._start_writes_prompt_before_reading_init` 与 `..._resume_defers_first_turn_to_advance` 两个离线单测锁定此行为。
- mod/导出：`claude_code/mod.rs` 增 `mod adapter;` + `pub use adapter::ClaudeCodeAdapter;`；`external/mod.rs`
  re-export 列表加 `ClaudeCodeAdapter`（`external` 为 `pub mod`，故对外可达
  `agent::external::ClaudeCodeAdapter`）。
- ignored real e2e：`tests/external_claude_code.rs`（`#![cfg(feature="external-claude-code")]`，一个
  `#[ignore]` `#[tokio::test]`）。从 `CLAUDE_CODE_BIN` 或 PATH 发现 `claude`，缺失即带清晰信息 skip（退出为绿）；
  临时 git worktree；用 `ClaudeCodeAdapter`+`ExternalSessionRegistry` 驱动 start→advance（自动 approve 任何权限
  暂停）→completion，断言观测流确为多步（SessionStarted+文本+SessionCompleted、≥3 事件）+ graceful shutdown；
  auth/launch 失败折成 skip 不判失败。命令：
  `cargo test --features external-claude-code --test external_claude_code -- --ignored --nocapture`。
- 文档：`managed-external-agent.md` §12.1 增「实现状态(M6-3)」并修正 init/stdin 时序、§12.3 说明不 bridge 宿主
  工具的分支；`capability-matrix.md` 叙述更新（表项保守留 `false`，因非 CI 默认覆盖）。
- **本机真机 e2e 实跑通过**：`claude` v2.1.207（订阅登录，apiKeySource=none）。观测 10 个事件（2 text、
  0 permission），SessionStarted→多个 TextDelta→SessionCompleted，graceful shutdown，23.5s。注：本次非交互
  `Prompt` 模式下 CLI 未发 `control_request`（直接阻塞受管工具并在文本说明），故真机未覆盖 permission 分支；
  该分支由离线单测（`control_response_frame` 映射 + `advance_drives_text_permission_completion` 回放）覆盖。
- 验证：`cargo fmt --all -- --check` 干净；`cargo clippy --all-targets -- -D warnings`（feature on+off）干净；
  聚焦 `cargo test -p agent-lib --features external-claude-code --lib claude_code`（28 passed，含 15 个
  `claude_code_adapter_*`）+ `--test agent_claude_code_cassette`（7 passed）通过；`cargo test --all --all-targets
  --features external-claude-code` 全绿（42 个 test-result:ok，0 失败）；`RUSTDOCFLAGS="-D warnings" cargo doc
  --no-deps --workspace`（feature on+off）干净；`git diff --check` 干净。全部用例 <1min。

**上下文**:

Claude Code adapter 是第一个真实 runtime adapter,其边界会成为 Codex/OpenCode 模板。

**做什么**:

- 检查 feature gate 下 public API 是否合理,未启 feature 时 crate 不引入重依赖。
- 检查 process cleanup:
  - graceful shutdown。
  - cancel/forced kill。
  - `ExternalSessionShutdown` trace。
- 检查 cassette 中无 secret/private transcript。
- 更新 `docs/capability-matrix.md` Claude Code 行,只标注已实测能力。

**验证条件**:

- `cargo test --all --all-targets`
- `cargo test --features external-claude-code -p agent-lib claude_code_cassette`
- `git diff --check`
- 完成记录中列出 Claude Code 支持/不支持能力和真实 e2e 状态。

---

## Milestone 7 — Codex managed adapter

目标:实现 feature-gated Codex adapter,支持 stream decoder、permission/tool bridge 能力探测与 ignored real
e2e。注意 Codex CLI 参数顺序必须按当前 CLI 要求验证。

### [DONE] M7-1 增加 Codex capability probe 与启动配置

**上下文**:

- `ExternalRuntimeKind::Codex` 已存在。
- 设计文档提示 Codex CLI 参数顺序容易踩坑:全局参数应放在 `exec` 前,例如
  `codex -s read-only -a never exec ...`。实现前必须以当前 CLI `--help` / probe 为准。

**做什么**:

- 新增 feature gate,例如 `external-codex`。
- 新增 Codex adapter 配置:
  - binary path/env override。
  - sandbox/approval mode。
  - working directory。
  - model/profile。
  - timeout。
- 实现 probe:
  - binary/version。
  - JSON/stream output support。
  - resume/session support。
  - tool bridge support。
  - permission bridge support。
- probe 失败分类到 `Launch` / `UnsupportedCapability` / `Protocol`。

**验证条件**:

- fake command/probe tests 覆盖 binary missing、unsupported stream、unsupported tool bridge。
- 未启 feature 时默认测试通过。
- `cargo test -p agent-lib codex_probe`
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增非默认 feature gate `external-codex`（`Cargo.toml`）;开启才编译 adapter,探测复用 tokio 的
  process 支持,不引入新重依赖。范围同 M6-1:本任务只填 config + probe,decoder/adapter 留给 M7-2/M7-3。
- **以当前本机 Codex CLI（v0.144.1）实测 `--help` / `exec --help` 为准**（TODO 明确要求),而非旧版
  参数假设。实测要点:结构化事件流 `--json` 在 `codex exec` 子命令;审批策略 `-a/--ask-for-approval`
  （`untrusted`/`on-request`/`never`）与 `mcp` 子命令在**顶层**;`-s/--sandbox`
  （`read-only`/`workspace-write`/`danger-full-access`）顶层与 `exec` 均有;`exec resume <id>` 支持续跑;
  实测确认 `codex -a never exec` 顺序被接受（全局 flag 排在 `exec` 前）。
- 新增 feature-gated 模块 `src/agent/external/codex/{mod.rs,config.rs,probe.rs}`,在
  `src/agent/external/mod.rs` 以 `#[cfg(feature = "external-codex")]` 挂载并 re-export `CodexConfig`、
  `CodexProbeExec`、`CodexProbeOutput`、`SystemCodexExec`,以及别名 `codex_probe`、
  `codex_probe_with_exec`（避免与 Claude adapter 的裸 `probe`/`probe_with_exec` 名在两 feature 同开时
  冲突）。
- `CodexConfig`:binary / env override（BTreeMap,手写 `Debug` 只印 key + `<redacted>` 脱敏）/ working
  dir(worktree) / permission mode / optional model / profile / timeout;serde round-trip 可持久化。
  `approval_policy_arg()` + `sandbox_mode_arg()` 把 `ExternalPermissionMode` 映射到当前 CLI 词汇:
  `Prompt→untrusted+read-only`、`AcceptEdits→on-request+workspace-write`、`Plan→never+read-only`、
  `BypassPermissions→never+danger-full-access`。`base_exec_args()` 产出
  `-a <approval> exec --json -s <sandbox> --skip-git-repo-check [--model M] [--profile P]`,顶层全局 flag
  严格排在 `exec` 之前(规避「全局参数放 exec 后」踩坑),working dir 走进程 `current_dir`。
- probe:跑 `--version`（缺失/损坏/非零退出 → `ExternalAgentError::Launch`）+ `--help`（顶层）+
  `exec --help`;两份 help 均空 → `Launch`;`exec` help 无 `--json` → `UnsupportedCapability{Streaming}`。
  能力探测保守（未广告即 `false`）:streaming←`exec --json`,permission_bridge←`--ask-for-approval`/
  `--sandbox`,resume←`resume` 子命令,host_tools←顶层 `mcp`,usage/artifacts←结构化流,
  graceful_shutdown=true,host_subagents=false（留待后续）。探测走可注入 `CodexProbeExec`（生产实现
  `SystemCodexExec` 用 `tokio::process`,kill_on_drop + timeout）,永不 panic;错误 `detail` 只含 binary
  路径 / `io::ErrorKind` / 缺失能力,绝不夹带 env 值或原始 CLI 输出。
- 支持能力（探测层,feature-gated,仍非 e2e 实测）:streaming / permission_bridge / resume / host_tools /
  usage / artifacts / graceful_shutdown 由当前 CLI help 保守探测;**不支持**:host_subagents（留待后续）。
  真实 e2e 未在本任务范围(属 M7-3),本机未运行真实 Codex 会话。
- 测试（模块内联 13 个,离线、无需真实 Codex、无网络）:config 默认/approval+sandbox 每模式映射/
  `base_exec_args` 顺序与 model/profile 省略/serde round-trip/Debug 脱敏;probe full-capability 探测、
  缺 binary→Launch、非零 version→Launch、空 help→Launch、无 `--json`→Unsupported{Streaming}、env secret
  不泄露（Display+Debug 均断言）、真实 `SystemCodexExec` 对不存在 binary→Launch（离线不 panic）、
  `detect_capabilities` 未广告即 false、探测子命令顺序（version→--help→exec --help）。
- 文档:`docs/managed-external-agent.md` §13.1 增补「实现状态（M7-1,已落地）」;
  `docs/capability-matrix.md` 保守基线段落说明 Codex feature-gated 探测已存在但仍非 e2e,Codex 行维持保守
  `false`,待 M7-2/M7-3 与真机 e2e 后再翻真。
- 验证:`cargo fmt --all -- --check` 干净;`cargo test -p agent-lib --features external-codex --lib codex`
  13 passed;`cargo test -p agent-lib codex_probe`（未启 feature）0 test（feature 关闭时模块不编译）;
  `cargo clippy --all-targets -- -D warnings` 与 `--features external-codex` 均 0 warning;
  `cargo test --all --all-targets`（未启 feature）42 个 test binary 全 ok、0 failed;
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --features external-codex` 干净;
  `git diff --check` 干净。

### [DONE] M7-2 实现 Codex stream decoder cassette 测试

**上下文**:

- Codex runtime raw JSONL/event schema 只能在 adapter parser 内部建模。
- parser 输出统一 external DTO。

**做什么**:

- 新增 Codex raw frame parser:
  - assistant text delta。
  - command execution。
  - patch/file edit。
  - permission request。
  - tool call / MCP call。
  - completion/error。
- 添加 cassette fixtures。
- 明确 unknown frame 策略。

**验证条件**:

- `cargo test -p agent-lib codex_cassette`
- cassette 覆盖 text、permission、tool、patch、completion、error。
- 无真实 Codex 依赖。
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增 feature-gated 私有 decoder `src/agent/external/codex/decoder.rs`(范围同 M6-2:只做 decoder +
  cassette,live adapter/e2e 留给 M7-3):有状态、跨 turn 单调 `seq` 的逐帧 `codex exec --json` 解码器。全程走
  `serde_json::Value` 防御式导航,**不导出任何 raw frame 类型**,Codex 私有 wire schema 不进 `agent-lib` 稳定
  API(design §12,非目标 §3)。经 `src/agent/external/mod.rs` 的 `#[cfg(feature = "external-codex")]`
  re-export `CodexDecision`、`CodexDecodeContext`、`CodexStreamDecoder`。
- **以当前本机 Codex CLI(v0.144.1)实测 `codex exec --json` 输出为准**(TODO 要求「以当前 CLI 为准」):经本机
  实跑确认该流是 `ThreadEvent` JSONL——`thread.started` / `turn.started` / `turn.completed` / `turn.failed`、
  `item.started` / `item.updated` / `item.completed`(包 `{id,type,...}` typed item)、以及顶层瞬时 `error`
  通知;并对照上游 `codex-rs/exec/src/exec_events.rs` + `event_processor_with_jsonl_output.rs` 校验字段。关键
  事实:codex exec `--json` **自主运行**,自己执行工具(含 MCP tool call 并回报 result),审批按启动时预设的
  sandbox/approval 策略内部解决,exec 流里**没有** host-pausable 的 tool-call / approval 帧。故本 decoder 的
  `CodexDecision` 只有 `Completed`(`turn.completed`)/ `Failed`(`turn.failed`)两个决策,**没有**
  PausedForToolCalls / PausedForInteraction——这是 Codex 与 Claude 的真实能力差异,不是 workaround,已被 M7-1
  的 capability 探测与本任务文档如实反映。
- 公开 API:`CodexDecodeContext::new().with_cwd(cwd)`(命令 cwd 由 host 配置的 worktree 提供,流里不含 cwd;
  不加 step_id/actor,因 decoder 不铸造 Interaction)、`CodexStreamDecoder::new(ctx)`、
  `push_line(&str) -> Result<Option<CodexDecision>, ExternalAgentError>`、
  `take_observations() -> Vec<ExternalObservedEvent>`、`session_id()`。turn 落定时清空本 turn 的
  `last_message`;`seq` 跨 turn 单调不重置。
- frame 映射:`thread.started`→`SessionStarted`(捕获 thread_id);`item.completed` `agent_message`→`TextDelta`
  (并作为本 turn summary);`item.started` `command_execution`→`CommandStarted`(cwd 取自 context);
  `item.completed` `command_execution` completed/failed→`CommandFinished`,`declined`(审批策略拒绝)→信息性
  `PermissionRequested`(无可应答项,runtime 已裁决;这是 exec 流里唯一的 permission 信号);`item.completed`
  `file_change`→逐 change `FilePatch`(`summary="{kind} {path}"`);`item.started`/`item.completed`
  `mcp_tool_call`→`ToolStarted`/`ToolFinished`(`name="{server}/{tool}"`);`turn.completed`→`SessionCompleted`
  + `Completed`(usage 映射 input/output/cached/cache_write/reasoning,cost=None);`turn.failed`→
  `Failed{Runtime}`。
- 容忍策略(稳定):空行 / `turn.started` / 顶层 `error` / `item.updated` / 未知顶层 type / 未知或缺失 item
  `type`(`reasoning`/`web_search`/`todo_list`/`collab_tool_call`/error item…)→容忍(`Ok(None)`,无观测);
  非法 JSON / 非对象帧 / 缺字符串 `type` / `thread.started` 缺 `thread_id` / `item.*` 缺 `item` 对象或 item 非
  对象→`ExternalAgentError::Protocol`。所有诊断均为固定字符串,永不夹带 prompt/命令/输出/凭据。永不 panic。
- 依赖环规避同 M6-2:decoder 设为 feature-gated `pub`,测试放 feature-gated 集成测试
  `tests/agent_codex_cassette.rs`(避免 `agent-testkit`↔`agent-lib` 依赖环导致的双 crate 实例)。decoder 同步
  小-`Ok` helper 触发的 `clippy::result_large_err` 以模块级 `#![allow]` + 说明注释显式接受,与
  `adapter.rs`/`registry.rs`/`probe.rs` 错误签名保持一致。
- committed cassette `tests/fixtures/external/codex/full_session.json`:两 turn(turn1 = `Start` → text/
  command/patch/MCP tool/declined 命令 → `Completed`,含 usage;turn2 = `Continue`(resume)→ text/顶层 error/
  `turn.failed` → `Failed`)。覆盖 text、permission、tool、patch、completion、error。由 in-code builder 经
  `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` 再生成;`assert_no_secrets` 保证无凭据。
- 测试(集成 7 个,均离线、无需真实 Codex、无网络):regenerate guard、cassette 与 in-code builder 逐字节一致、
  secret-free 扫描、full-session 全程回放断言观测流与每 turn 决策、容忍未知/空行/顶层 error/item.updated 帧、
  malformed→`Protocol`、`turn.failed`→`Failed`。
- 文档:`docs/managed-external-agent.md` §13.2 增补「实现状态(M7-2,已落地)」并订正 §13.2 表格(exec `--json` 流
  无 host-pausable approval → PausedForInteraction);`docs/capability-matrix.md` 增补 M7-2 离线 decoder 已落地
  段落(仍非 e2e,Codex 行维持保守 `false`,待 M7-3 真机 e2e 再翻真);
  `tests/fixtures/external/codex/README.md` 从预留占位改为描述已落地的合成 decoder fixture。
- 验证(完整序列 1-6):`cargo fmt --all -- --check` 干净;`cargo test -p agent-lib --features external-codex
  --test agent_codex_cassette` 7 passed,`cargo test -p agent-lib --test agent_codex_cassette`(未启 feature)
  0 test;`cargo clippy --all-targets -- -D warnings` 与 `--features external-codex` 均 0 warning;
  `cargo test --all --all-targets`(未启 feature)43 个 test binary 全 ok、0 failed;
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --features external-codex` 干净;
  `git diff --check` 干净。
- Codex 真实 e2e 与 live session adapter 未在本任务范围(属 M7-3);本机仅回放合成 cassette,未运行真实 Codex
  会话(需登录/网络)。

### [DONE] M7-3 实现 Codex session adapter 与 ignored real e2e

**上下文**:

- Codex adapter 应复用 M5 registry/handler,不要复制 machine 逻辑。
- 真实 test 必须 ignored。

**做什么**:

- 实现 start/resume/advance/cleanup。
- 映射 `ExternalPermissionMode` 到 Codex sandbox/approval 参数。
- 接入 live sink 和 observations buffering。
- tool bridge:
  - 如果 Codex 支持 MCP/custom tool,接 `PausedForToolCalls`。
  - 如果暂不支持,通过 capability 拒绝 host tool parity,并保证 dispatcher 不会误派。
- ignored e2e:
  - 使用临时 worktree。
  - 验证一个 Codex 完成的小任务。
  - 如果支持 tool bridge,验证 host tool round-trip。

**验证条件**:

- 默认测试不运行真实 Codex。
- `cargo test --features external-codex -p agent-lib codex_cassette`
- ignored 测试命令文档化。
- 若本机环境具备 Codex,运行 ignored e2e 并记录结果；否则记录 skip 原因。
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增 feature-gated live adapter `src/agent/external/codex/adapter.rs`(**唯一 pub 类型**
  `CodexAdapter`),把 M7-1 配方 + M7-2 decoder 接进 M5 的 `ExternalRuntimeAdapter` /
  `ExternalRuntimeSession` 抽象,复用 `ExternalSessionRegistry`/handler,不复制 machine 逻辑。经
  `codex/mod.rs` 与 `external/mod.rs` 的 `#[cfg(feature = "external-codex")]` re-export `CodexAdapter`。
- **关键差异——每 turn 一个一次性进程**(与 Claude Code 长驻 `stream-json` 进程根本不同):`codex exec` 的
  prompt 是 CLI **位置参数**(不是 stdin 帧),进程在一个 turn 落定后退出;续跑是全新的
  `codex exec resume <thread_id> <message>` 进程。故私有 `CodexSession<L>` 在 `begin` 为首个 turn spawn 进程并
  读到 `thread.started` 拿真实 thread id(作 registry key + resume token),该 turn 其余帧留给第一次 `advance`
  续读(`first_turn_pending` 防重复);此后每个 `Continue` 在 `advance` spawn 新的 `exec resume` 进程。整段
  session 共用一个跨全程单调 `seq` 的 `CodexStreamDecoder`。
- **参数顺序(以当前本机 codex-cli 0.144.1 实测为准)**:`resume` 子命令**不接受** `-s/--sandbox`、
  `-p/--profile`,故新增 `CodexConfig::base_resume_args(session_id)`(additive,不改 frozen `base_exec_args()`)
  把 sandbox/model/profile **上提到顶层**:`codex -a <approval> -s <sandbox> [--model M][--profile P] exec
  resume --json --skip-git-repo-check <id>`,adapter 追加 `<message>`;已用 bogus id 实测该顺序被 CLI 接受
  (报 "no rollout found",即通过 arg parse)。生产进程 **stdin=null**(否则 codex 阻塞在 "Reading additional
  input from stdin…")、**stderr 丢弃**(防原始文本泄漏)、stdout piped 逐行喂 decoder,`kill_on_drop`、每读
  超时。IO 经私有 `CodexLauncher`/`CodexTurnStream` trait 注入。
- **能力(诚实按 M7-2 结论,非 workaround)**:`codex exec --json` 自主运行、审批按命令行预置策略解决、自己执行
  工具,流里**没有** host-pausable 的 tool-call/approval 帧,一个 turn 只会 `Completed`/`Failed`。故
  `implemented_capabilities()`:`streaming`/`resume`/`artifacts`/`usage`/`graceful_shutdown`=true,
  **`host_tools`=`host_subagents`=`permission_bridge`=false**。声明 `tools` 的 `start`/`resume` 请求以
  `UnsupportedCapability{HostTools}` **明确拒绝**;follow-up 的 `RespondToolResults`→`{HostTools}`、
  `RespondSubagent`→`{HostSubagents}`、`RespondInteraction`→`{PermissionBridge}` 均拒绝而非静默忽略;
  `with_probed_capabilities` 与本机 probe 逐位取交。`ExternalPermissionMode` 复用 M7-1 config 映射
  (Prompt→untrusted+read-only、AcceptEdits→on-request+workspace-write、Plan→never+read-only、
  BypassPermissions→never+danger-full-access)。cleanup 经 `shutdown()` 关闭当前 turn 进程并归类
  `Graceful`/`ForcedKill`/`Failed`。
- 单测(inline 16 个,均离线、无需真实 Codex、无网络):`FakeLauncher` 回放固定 JSONL 帧并**逐 turn 捕获
  `CodexTurnSpec`**——advance 驱动 text→completion 且观测流单调、follow-up 用 thread id spawn `exec resume`、
  提前 EOF→`SessionLost`、malformed→`Protocol`、`turn.failed`→`Runtime`、shutdown 归类 close、resume defer 首 turn
  并记录 thread id、`begin` spawn 失败按 fresh/resume 归类 `Launch`/`ResumeUnavailable`、`Respond*` 拒绝、`start`
  拒绝声明工具、caps/intersect/turn_message/turn_spec 参数。另 `config.rs` 增 `base_resume_args` 顺序测试。
- ignored 真机 e2e `tests/external_codex.rs`:临时 git worktree,`CODEX_BIN`/PATH 发现 `codex`,缺失 binary/登录带
  清晰信息**跳过**(退出为绿);以 `AcceptEdits`(`workspace-write`,让自主 CLI 落盘且无需 host 审批)驱动
  probe→start→advance→completion→graceful shutdown,断言观测流多步(SessionStarted + ≥1 文本 +
  SessionCompleted,≥3 事件)。命令:`cargo test --features external-codex --test external_codex --
  --ignored --nocapture`。**本机 codex-cli 0.144.1 实跑通过**:5 个观测事件(2 文本)、生成 `READY.txt`、优雅关闭,
  约 51s。
- 文档:`docs/managed-external-agent.md` §13.3 增补「实现状态(M7-3,已落地)」;`docs/capability-matrix.md` 增补
  M7-3 live adapter + 真机 e2e 通过段落(并说明保守基线表是 `none()`,不代表 adapter 实报能力);
  `tests/fixtures/external/codex/README.md` 从 M7-3 占位改为指向已落地的 live adapter + ignored e2e。
- 验证(完整序列 1-6):`cargo fmt --all -- --check` 干净;`cargo clippy --all-targets -- -D warnings` 与
  `--features external-codex` 均 0 warning;`cargo test -p agent-lib --features external-codex --lib codex`
  30 passed;`cargo test --features external-codex --test agent_codex_cassette` 7 passed;
  `cargo test --features external-codex --all-targets` 全 ok、0 failed(ignored e2e 不在默认集);
  `cargo test --all --all-targets`(未启 feature)44 个 test binary 全 ok、0 failed;
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features external-codex` 干净。

### [DONE] M7-4 Review：Codex adapter 正确性检查

**上下文**:

Codex adapter 要和 Claude Code adapter 保持相同 adapter trait 和 capability 语义。

**做什么**:

- 检查 CLI 参数顺序、sandbox/approval mode 文档和 tests。
- 检查 feature gate、依赖、no-secret logging。
- 检查 cleanup 与 trace。
- 更新 `docs/capability-matrix.md` Codex 行。

**验证条件**:

- `cargo test --all --all-targets`
- `cargo test --features external-codex -p agent-lib codex_cassette`
- `git diff --check`
- 完成记录中列出 Codex 支持/不支持能力和真实 e2e 状态。

**完成记录**:

- **性质**:纯 review + 文档更新任务。逐项核对 M7-1/2/3 落地的 Codex adapter 源码,确认与 Claude Code
  adapter 共享同一 `ExternalRuntimeAdapter`/`ExternalRuntimeSession` trait 与 `ExternalCapability` 语义、
  无 workaround、无 spec 偏离,**未发现需修复的缺陷**,故不改任何代码,仅更新 `docs/capability-matrix.md`。
- **CLI 参数顺序(已核对源码 + tests)**:`config.rs`
  - `base_exec_args()` = `-a <approval> exec --json -s <sandbox> --skip-git-repo-check [--model M][--profile P]`
    —— 全局 `-a` 严格在 `exec` 子命令之前(Codex CLI footgun),测试
    `codex_config_base_exec_args_put_global_flag_before_subcommand` 断言 `approval_pos < exec_pos` 且完整序列。
  - `base_resume_args(id)` = `-a <approval> -s <sandbox> [--model M][--profile P] exec resume --json
    --skip-git-repo-check <id>` —— `resume` 子命令不接受 `-s/-p`,故上提到顶层,测试
    `codex_config_base_resume_args_hoist_sandbox_and_selectors_before_exec` 断言 `sandbox_pos/profile_pos <
    exec_pos`、id 为末位、含/不含 model/profile 两版。
  - `CodexTurnSpec::args()` 追加 prompt(fresh)/message(resume,在 id 之后),测试
    `codex_turn_spec_appends_prompt_and_message_to_base_args`。
- **sandbox/approval mode(文档 + tests)**:`ExternalPermissionMode` → approval(`untrusted`/`on-request`/
  `never`) + sandbox(`read-only`/`workspace-write`/`danger-full-access`)映射在 `approval_policy_arg` /
  `sandbox_mode_arg` 的 rustdoc 逐模式说明,测试 `codex_config_approval_and_sandbox_map_every_mode` 覆盖全 4
  模式。e2e 用 `AcceptEdits`(`on-request`+`workspace-write`)让自主 CLI 落盘且无需 host 审批。
- **feature gate / 依赖**:`external-codex` off by default(`Cargo.toml`),`codex/mod.rs` 与 `external/mod.rs`
  的 re-export 全 `#[cfg(feature = "external-codex")]`;复用 `tokio`(process)、`async-trait`、`serde_json`,
  **无新增依赖**。默认构建不接入任何 Codex 机制。
- **no-secret logging**:`CodexConfig` 手写 `Debug` 把 `env` 值渲染为 `<redacted>`(仅露 key),测试
  `codex_config_debug_redacts_env_secrets` 断言原始 `sk-...` 不出现;生产 `SystemCodexLauncher` 把
  `stderr = Stdio::null()`(防原始 runtime 文本泄漏)、`stdin = null`;decoder 每条诊断为固定字符串,不折入
  prompt/命令行/工具输出/凭据;cassette `codex_cassette_is_secret_free` 扫描无 secret。
- **cleanup**:`CodexProcessTurn::close()` 在 grace 窗口 `wait()`(一次性 turn 进程正常已自退),超时
  `start_kill()`→`ForcedKill`、wait 出错→`Failed`;`kill_on_drop(true)` 兜底;`CodexSession::shutdown()` 关当前
  turn 进程、turn 间无进程时返回 `Graceful`;`spawn_follow_up_turn` 先 close 旧(已退)进程再 spawn 新 resume。
  测试 `codex_adapter_shutdown_classifies_the_close`;真机 e2e 断言 `registry.cleanup(...) == Graceful`。
- **trace**:`advance` 每轮尊重 `ctx.is_cancelled()`(取消→`SessionLost`)。与 Claude Code 用
  `ctx.run_id()` 建 `StepId` 关联 trace 不同,Codex 自主运行、按 runtime 分配的 `thread_id` resume,observations
  不需宿主 run/step id 关联——这是**刻意且诚实的差异**(start/resume 的 `ctx` 未用即以 `_ctx` 命名),decode
  context 只按 config/worktree 的 cwd 给 `command_execution` 补目录,绝不取自 model 输出。
- **Codex 支持/不支持能力(诚实反映 M7-2,非 workaround)**:
  - **支持(`new()` 实报 `true`)**:`streaming`、`resume`、`artifacts`、`usage`、`graceful_shutdown`。
  - **不支持(恒 `false`)**:`permission_bridge`、`host_tools`、`host_subagents`——`codex exec --json` 自主
    运行、按预置策略解审批、自己执行工具,流里无 host-pausable 帧。声明 `tools` 的 `start`/`resume` 以
    `UnsupportedCapability{HostTools}` 拒绝;follow-up 的 `RespondToolResults`→`{HostTools}`、
    `RespondSubagent`→`{HostSubagents}`、`RespondInteraction`→`{PermissionBridge}` 均拒绝而非静默忽略。
  - `with_probed_capabilities` 把上述实报位与本机 probe 逐位 AND(host bridge 三项无论 probe 恒 `false`)。
- **真实 e2e 状态**:`tests/external_codex.rs`(`#[ignore]`,`CODEX_BIN`/PATH 发现,缺 binary/登录自跳过退绿),
  **本机 codex-cli 0.144.1 实跑通过**(M7-3 记录):`AcceptEdits`/`workspace-write` 驱动
  probe→start→advance→completion→graceful shutdown,生成 `READY.txt`,5 个观测事件(2 文本)、优雅关闭、约 51s。
  e2e 覆盖 streaming(sink 多事件)+ graceful_shutdown(断言 `Graceful`);resume 由离线单测覆盖(真机 e2e 为单
  turn,未跑 resume);artifacts/usage 由离线 cassette + 单测覆盖。
- **文档**:`docs/capability-matrix.md` 在 Codex 叙述后新增「**Codex live adapter 实报能力**(M7-4 review 定案)」
  表——逐项列 `CodexAdapter::new()` 实报值 + 验证来源(e2e / 离线单测 / cassette),并显式区分「adapter 实报」
  与末尾「各 runtime 当前声明」的默认构建保守基线 `none()`;附真机 e2e 状态段。保守基线表本身不改(它忠实表示
  默认构建 `none()`)。
- **验证(完整序列全过)**:`cargo fmt --all -- --check` 干净;`cargo clippy --all-targets -- -D warnings`
  与 `--features external-codex` 均 0 warning;`cargo test --features external-codex -p agent-lib --lib codex`
  30 passed;`cargo test --features external-codex -p agent-lib --test agent_codex_cassette` 7 passed;
  `cargo test --all --all-targets`(未启 feature)全 ok、exit 0、0 failed;
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features external-codex` 干净;`git diff --check` 干净。
  (代码自 M7-3 绿跑后未变,本任务仅改 `docs/*.md`;仍重跑上述以完成 review 签核。)

---

## Milestone 8 — OpenCode managed adapter

目标:实现 feature-gated OpenCode adapter,按实际 CLI/API 能力接入 streaming、permission、tool bridge,并用
capability model 明确降级。

### [DONE] M8-1 增加 OpenCode capability probe 与启动配置

**上下文**:

- `ExternalRuntimeKind::OpenCode` 已存在。
- OpenCode 具体 CLI/API 能力可能变化,实现必须先 probe,不要硬编码假设。

**做什么**:

- 新增 feature gate,例如 `external-opencode`。
- 新增配置:
  - binary path/env override。
  - worktree。
  - permission/sandbox mode。
  - timeout。
- 实现 capability probe。
- 若某能力无法确认,默认 false 并通过 `UnsupportedCapability` 拒绝依赖该能力的请求。

**验证条件**:

- fake probe tests 覆盖 missing binary、unknown version、unsupported managed feature。
- `cargo test -p agent-lib opencode_probe`
- 默认测试未启 feature 通过。
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增 `external-opencode` feature(`Cargo.toml`,off by default,仅复用 tokio process,无新重依赖)。
- 新增 `src/agent/external/opencode/`:
  - `config.rs` → `OpenCodeConfig`(binary/env[Debug 脱敏]/working_dir/permission_mode/model/`--agent`/timeout;
    serde round-trip;`auto_approve()` 仅 `BypassPermissions`=true;`base_run_args()` = `run --format json`
    [+`--auto`][+`--model`][+`--agent`])。
  - `probe.rs` → `OpenCodeProbeOutput` / `OpenCodeProbeExec`(trait)/ `SystemOpenCodeExec`(tokio::process,
    kill_on_drop,timeout,施加 working_dir/env)/ `probe` / `probe_with_exec` / `detect_capabilities`。
  - `mod.rs` 挂载 + `pub use`。
- 真实 CLI 对齐(据官方 opencode.ai/docs/cli,非硬编码假设):非交互入口 = `opencode run`;结构化流 =
  `run --format json`(**不是** `--json`);resume = `run --continue`/`--session` 或顶层 `session`;host tools =
  顶层 `mcp`;权限旁路 = `run --auto`。
- probe 契约(不假装):`--version` io 错误/非零 → `Launch`;`--help`+`run --help` 皆空 → `Launch`;`run` 无
  `--format json` → `UnsupportedCapability{Streaming}`;其余能力从两份 help **保守探测**,默认 `false`,
  host_subagents 恒 `false`(spawn bridge 待 M8-3)。权限映射保守:仅 `BypassPermissions` 发 `--auto`,避免越权。
- `external/mod.rs`:feature-gated `mod opencode` + re-export
  `OpenCodeConfig` / `OpenCodeProbeExec` / `OpenCodeProbeOutput` / `SystemOpenCodeExec` /
  `opencode_probe` / `opencode_probe_with_exec`。
- 文档:`docs/managed-external-agent.md` §14 增补 M8-1 实现状态;`docs/capability-matrix.md` 增补 OpenCode
  probe 段(保守、仍非 e2e);`tests/fixtures/external/opencode/README.md` 说明 probe/config 已落地、cassette 待
  M8-2。
- 范围 = 仅 config + probe(对齐 M6-1/M7-1);decoder 留 M8-2,live adapter/e2e 留 M8-3。
- 验证序列 1-6 全过:
  1. `cargo fmt --all -- --check` 通过。
  2. `cargo test -p agent-lib --features external-opencode --lib opencode` = 13 passed;未启 feature = 0 test。
  3. `cargo clippy --all-targets -- -D warnings`(feature off)与 `--features external-opencode` 均 0 warning。
  4. `cargo test --all --all-targets`(feature off)= 43 个 result 块全 ok,0 failed。
  5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --features external-opencode` 通过。
  6. `git diff --check` 干净。

### [DONE] M8-2 实现 OpenCode stream decoder cassette 测试

**上下文**:

- OpenCode raw stream schema 只属于 adapter 内部。
- 必须落到统一 `ExternalObservedEvent` / `ExternalSessionResult`。

**做什么**:

- 新增 parser 和 cassette fixtures。
- 覆盖 text、command、patch、permission、tool/subtask、completion/error。
- unknown frame 策略稳定。

**验证条件**:

- `cargo test -p agent-lib opencode_cassette`
- cassette redaction test 通过。
- 无真实 OpenCode 依赖。
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增 `src/agent/external/opencode/decoder.rs`:adapter 私有 `OpenCodeStreamDecoder` /
  `OpenCodeDecodeContext` / `OpenCodeDecision`,把 `opencode run --format json` 逐行事件信封归一化成
  sequenced `ExternalObservedEvent` 与 per-turn decision,私有 wire schema **不外泄**为稳定 public API
  (镜像 Codex decoder 结构)。
- Wire 对齐真实源码(sst/opencode `packages/opencode/src/cli/cmd/run.ts` 的 `emit()` +
  `packages/sdk/js/src/gen/types.gen.ts`,非臆测):每行 = `{ type, timestamp, sessionID, ...data }`;`emit()`
  只镜像 `text`(part.time.end 已置)、`reasoning`、`tool_use`(仅 state.status=completed/error)、`step_start`、
  `step_finish`、`error` 六种;**无 init 帧/无显式完成帧**——sessionID 随每帧到达(首帧惰性捕获 → 发一次
  `SessionStarted`),turn 结束在 json 模式不发 `session.idle`。
- 自主运行语义(与 `codex exec --json` 同):`run --format json` 的权限提示按 `--auto` 启动开关裁决、**不回灌
  host**(loop 内 auto-approve/auto-reject,从不镜像 `permission.asked`),故 `OpenCodeDecision` **只有**
  `Completed{output}` / `Failed{error}` 两臂,无 host-pausable 决策臂。turn 完成 = 终结 `step_finish`
  (`reason != "tool-calls"`;`reason == "tool-calls"` 是 agentic 续跑,容忍不决策);turn 失败 = 顶层 `error`
  → `Failed(Runtime{ message = error.data.message ?? error.name })`。
- 帧 → 观测映射:`text` → `TextDelta`(并记 last_message 作 summary);已结算 `tool_use` 依 `tool` 分派——
  `bash` → `CommandStarted`+`CommandFinished`(从单帧重建,附 exit/输出),`edit`/`write`/`apply_patch` →
  `FilePatch`,`task` 子代理与其余工具 → `ToolStarted`+`ToolFinished`;被拒权限的工具(错误串是 OpenCode 稳定
  的 `PermissionRejectedError`/`PermissionDeniedError`/`PermissionCorrectedError` 文案)→ **信息型**
  `PermissionRequested`(runtime 已裁决,host 无需应答);`step_finish` 跨步累加 `Usage`
  (input/output/reasoning/cache_read/cache_write)与 `cost`(USD → `cost_micros = round(cost*1e6)`),
  turn 结束发 `SessionCompleted`+`Completed`。`seq` 跨 turn 单调;turn 结算后 `reset_turn()` 清 last_message
  /usage/cost,续跑 turn 只记自身产出。
- 严格性:非法 JSON / 非对象 / 缺字符串 `type` / `text`|`tool_use`|`step_finish` 缺 `part` 对象 → `Protocol`
  错误;`step_start`/`reasoning`/未知 type 容忍(`_ => None`)。
- 模块接线:`opencode/mod.rs` `mod decoder;` + `pub use`;`external/mod.rs` feature-gated re-export
  `OpenCodeDecision` / `OpenCodeDecodeContext` / `OpenCodeStreamDecoder`。
- 离线 cassette:新增 `tests/agent_opencode_cassette.rs`(`#![cfg(feature = "external-opencode")]`,7 test)+
  committed fixture `tests/fixtures/external/opencode/full_session.json`(2 turn:Completed turn 12 帧→12 观测、
  usage 跨步求和 input 1200/output 210/cache 80+25/reasoning 10、cost 3000 micros;Failed turn 3 帧→1 观测 +
  网络错误)。覆盖 decode/in-code-builder 一致性/redaction 无密钥/未知+空行容忍/畸形帧分类/error→Failed;
  fixture 仅由 `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` 重生成,回放无真实 `opencode` 依赖。
- 文档:`docs/managed-external-agent.md` §14 与 `docs/capability-matrix.md` 增补 M8-2 decoder 段(自主语义、帧
  映射、cassette 冻结、e2e 待 M8-3);`tests/fixtures/external/opencode/README.md` 从「cassette 待 M8-2」更新为
  cassette 已落地并说明重生成命令。
- 验证序列 1-6 全过:
  1. `cargo fmt --all -- --check` 通过。
  2. `cargo clippy --all-targets -- -D warnings`(feature off)与 `--features external-opencode` 均 0 warning。
  3. `cargo test -p agent-lib --features external-opencode --lib opencode` = 13 passed;
     `cargo test --features external-opencode --test agent_opencode_cassette` = 7 passed;未启 feature = 0 test。
  4. `cargo test --all --all-targets`(feature off)全部 result 块 ok,0 failed。
  5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features external-opencode` 通过。
  6. `git diff --check` 干净。

### [DONE] M8-3 实现 OpenCode session adapter 与 ignored real e2e

**上下文**:

- OpenCode 首版可以 capability 降级,但必须明确声明支持/不支持。

**做什么**:

- 实现 start/resume/advance/cleanup。
- 接入 live sink。
- 接入 permission/tool/subagent bridge(若 runtime 支持)。
- ignored e2e:
  - 临时 worktree。
  - 小任务 completion。
  - 有能力则测 tool/permission。

**验证条件**:

- 默认测试不运行真实 OpenCode。
- `cargo test --features external-opencode -p agent-lib opencode_cassette`
- ignored 测试命令文档化。
- 若本机环境具备 OpenCode,运行 ignored e2e 并记录结果；否则记录 skip 原因。
- 完整验证序列 1-6 全过。

**完成记录**:

- 新增 `src/agent/external/opencode/adapter.rs`:把 M8-1 启动配方与 M8-2 私有 decoder 接进 milestone-5 的
  `ExternalRuntimeAdapter` / `ExternalRuntimeSession` 抽象,新增**唯一 pub 类型** `OpenCodeAdapter`(镜像
  `CodexAdapter` 结构):`new(config)` 报告实现能力,`with_probed_capabilities(config, &probed)` 与本机 probe
  逐位取交;`start` 用 `opencode run … <prompt>`、`resume` 用 `opencode run … --session <id> <message>`,启动失败
  分别归类 `Launch` / `ResumeUnavailable`。
- **每 turn 一个一次性进程**(同 Codex):prompt 是 `run` 的位置参数,进程一 turn 落定即退出,续跑是全新
  `opencode run --session <id> <message>` 进程。为此给 `OpenCodeConfig` 新增 `base_resume_args(session_id)`(=
  复用 `base_run_args()` 的 `run --format json [--auto][--model][--agent]` 再追加 `--session <id>`,对齐官方 CLI
  `opencode run` 接受 `-s/--session`/`-c/--continue`,已核对 opencode.ai/docs/cli,非臆测)+ config 单测。
- **无 init 帧差异**:OpenCode session id 随每帧 `sessionID` 到达(与 Codex 的 `thread.started` 前导帧不同),故
  私有 `OpenCodeSession` 在 `begin` 里读到 decoder 惰性捕获首个 `sessionID`(并发唯一 `SessionStarted`)为止,把
  前导观测缓存给第一次 `advance` 续读;整段 session 共用一个跨全程单调 `seq` 的 `OpenCodeStreamDecoder`。生产进程
  stdin=null、stderr 丢弃、stdout 逐行喂 decoder、kill_on_drop、每读超时。
- 能力(诚实按 M8-2 结论):`run --format json` 自主运行,流里无 host-pausable tool-call/approval 帧,turn 只
  `Completed`/`Failed`。`implemented_capabilities()` 报 `streaming`/`resume`/`artifacts`/`usage`/
  `graceful_shutdown`=true,`host_tools`/`host_subagents`/`permission_bridge`=false;声明 `tools` 的 start/resume 以
  `UnsupportedCapability{HostTools}` 拒绝,follow-up 的 `RespondToolResults`/`RespondSubagent`/
  `RespondInteraction` 均明确拒绝而非静默忽略。
- IO 经私有 `OpenCodeLauncher` / `OpenCodeTurnStream` trait 注入:生产 `SystemOpenCodeLauncher`(tokio::process),
  单测注入 `FakeLauncher` 回放固定 JSON 帧并逐 turn 捕获 `OpenCodeTurnSpec`,**离线**跑通 begin/advance(fresh +
  resume)/shutdown 全状态机(含「resume 进程从不复报 sessionID 仍完成」的边界)。
- **worktree 隔离修复(本任务中发现并修掉的真实缺陷,非 workaround)**:首版把 working dir 只经进程
  `current_dir` 应用,但真机 e2e 复现出 `READY.txt` 落进**启动它的 checkout(repo 根)**而非临时 worktree。
  实证根因:OpenCode 从 `--dir`/继承的 `$PWD` 解析项目与落盘目录,**而非仅** OS 级 cwd;`tokio::process` 的
  `current_dir()` 只 `chdir` 却不更新继承来的 `PWD`(仍指向 cargo/repo 根),于是文件泄漏到 checkout。实验证明
  `--dir` authoritative(压过 cwd 与 `$PWD`)。故给 `base_run_args()` 增补:配置 `working_dir` 时显式追加
  `--dir <path>`(`base_resume_args()` 一并继承),launcher 保留 `current_dir` 作 belt-and-suspenders。新增
  config 单测(`--dir` 存在/位置、resume 继承)与 turn-spec 单测(fresh+resume 均带 `--dir`),并订正 config
  模块/`base_run_args`/`base_resume_args` doc 里「working dir 走进程 cwd 而非 --dir」的过期措辞。
- 接线:`opencode/mod.rs` `mod adapter;` + `pub use adapter::OpenCodeAdapter;`;`external/mod.rs` feature-gated
  re-export 追加 `OpenCodeAdapter`。
- 真机 e2e:新增 `tests/external_opencode.rs`(`#[ignore]`,镜像 `external_codex.rs`),经 `OPENCODE_BIN`/PATH
  发现 `opencode`(可选 `OPENCODE_MODEL`/`OPENCODE_AGENT`),缺 binary/登录即带清晰信息**跳过**(退出为绿);否则在
  临时 git worktree 以 `BypassPermissions`(映射 `--auto`)驱动 probe→start→advance→completion→graceful shutdown,
  断言观测流为多步,**并断言 worktree 隔离**:`READY.txt` 落在 worktree 内、且**绝不**泄漏进启动它的 checkout(cwd)。
  命令文档化于 doc/README。
- **本机 opencode 1.17.15 实跑通过**:ignored e2e = 1 passed(6 个观测事件、1 条文本、`READY.txt` 生成于 worktree
  内且无泄漏、graceful shutdown,约 20s),非 skip。
- 文档:`docs/managed-external-agent.md` §14 增补 M8-3 实现状态块(含 worktree 隔离/`--dir` 说明);
  `docs/capability-matrix.md` 增补「OpenCode live adapter 实报能力(M8-3)」表 + 真机 e2e 状态(含隔离断言),
  并修订 M8-1 段的过期措辞;`tests/fixtures/external/opencode/README.md` 从「live adapter 与 e2e 待 M8-3」更新为
  已落地并说明 e2e 运行命令。
- 验证序列 1-6 全过:
  1. `cargo fmt --all -- --check` 通过。
  2. `cargo clippy --all-targets -- -D warnings`(feature off)与 `--features external-opencode` 均 0 warning。
  3. `cargo test -p agent-lib --features external-opencode --lib opencode` = 32 passed;
     `cargo test --features external-opencode --test agent_opencode_cassette` = 7 passed;
     `cargo test --features external-opencode --test external_opencode -- --ignored` = 1 passed(真机实跑,含隔离断言)。
  4. `cargo test --all --all-targets`(feature off)全部 result 块 ok,0 failed。
  5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features external-opencode` 通过。
  6. `git diff --check` 干净。

### [DONE] M8-4 Review：OpenCode adapter 正确性检查

**上下文**:

OpenCode adapter 完成后三个目标 runtime 都有统一接入路径。

**做什么**:

- 对比 Claude Code / Codex / OpenCode adapter:
  - trait 实现一致。
  - capability fallback 一致。
  - parser cassette 覆盖层级一致。
  - cleanup/trace 一致。
- 更新 `docs/capability-matrix.md` OpenCode 行。

**验证条件**:

- `cargo test --all --all-targets`
- `cargo test --features external-opencode -p agent-lib opencode_cassette`
- `git diff --check`
- 完成记录中列出 OpenCode 支持/不支持能力和真实 e2e 状态。

**完成记录**:

- **性质**:纯 review + 文档更新任务。逐项核对 M8-1/2/3 落地的 OpenCode adapter 源码,并与 Claude Code /
  Codex adapter 三方对比,确认共享同一 `ExternalRuntimeAdapter`/`ExternalRuntimeSession` trait 与
  `ExternalCapability` 语义、无 workaround、无 spec 偏离,**未发现需修复的缺陷**,故不改任何代码,仅更新
  `docs/capability-matrix.md`。
- **trait 实现一致(已核对源码)**:三者都 impl `ExternalRuntimeAdapter`(kind/capabilities/start/resume)
  + `ExternalRuntimeSession`(session_ref/advance/shutdown)。**Codex 与 OpenCode 同构**(自主、一进程/一 turn):
  `begin`/`read_line`/`drain_and_emit`/`spawn_follow_up_turn`/`finish`(仅 `Completed`/`Failed`)/`advance`
  (cancel→`SessionLost`)/`shutdown`(close stream 或 `Graceful`)结构逐行一致;`OpenCodeLauncher`/
  `OpenCodeTurnStream` 镜像 `CodexLauncher`/`CodexTurnStream`。**Claude Code 的差异是设计意图**——常驻 stdio
  进程(非一进程/一 turn),`finish` 多出 `PausedForToolCalls`/`PausedForInteraction` 臂(permission bridge),
  `write_input` 替代 `spawn_follow_up_turn`,`shutdown` 直接关 io。
- **capability fallback 一致**:三者 `new()`→`implemented_capabilities()`;`with_probed_capabilities()`→
  `intersect_capabilities()`(逐位 AND,helper 三份逐行一致);`reject_unsupported_tools`(host_tools 门禁)+
  `turn_message` 拒绝式(PermissionBridge/HostTools/HostSubagents→`UnsupportedCapability`,Shutdown→`Protocol`)
  三份一致。唯一能力差异 = `permission_bridge`(Claude=`true` 权限控制通道;Codex/OpenCode=`false` 自主),
  由 capability model 显式暴露、非静默假装。
- **parser cassette 覆盖层级一致**:三份 `agent_<rt>_cassette.rs` 各 7 个并行层(regenerate_fixture /
  matches_in_code_builder / is_secret_free / decodes_full_session / tolerates_unknown_and_blank_frames /
  rejects_malformed_frames / decodes_*_as_failed),各有 committed fixture
  `tests/fixtures/external/<rt>/full_session.json`。inline adapter 单测同构(advance/session-lost/protocol-error/
  shutdown/respond-unsupported/resume-defers/rejects-declared-tools/caps 三件套);OpenCode 多一个
  `resume_survives_a_session_that_never_re_reports_its_id`(OpenCode 无 init 帧、sessionID 随帧惰性到达,合理)。
- **cleanup/trace 一致**:三者 `advance` 均先 `ctx.is_cancelled()`→`SessionLost`;`shutdown`→
  `ExternalSessionShutdown`(关 stream/io,turn 间无进程返回 `Graceful`);生产 launcher 均 `stderr=null`、
  `stdin=null`、`kill_on_drop(true)`。adapter 层不自发 tracing——trace 经 `RunContext` trace node 透传;
  与 Claude Code 用 `ctx.run_id()` 建 `StepId` 关联 permission 不同,Codex/OpenCode 自主运行不需宿主 run/step
  id 关联(`start`/`resume` 的 `ctx` 未用即 `_ctx`),decode context 只按 config/worktree 的 cwd 给 `bash`/
  `command_execution` 观测补目录,绝不取自 model 输出——**刻意且诚实的差异**。
- **OpenCode 支持/不支持能力(诚实反映 M8-2,非 workaround)**:
  - **支持(`new()` 实报 `true`)**:`streaming`、`resume`、`artifacts`、`usage`、`graceful_shutdown`。
  - **不支持(恒 `false`)**:`permission_bridge`、`host_tools`、`host_subagents`——`opencode run --format json`
    自主运行、按 `--auto` 解审批、自己执行工具,流里无 host-pausable 帧。声明 `tools` 的 `start`/`resume` 以
    `UnsupportedCapability{HostTools}` 拒绝;follow-up 的 `RespondToolResults`→`{HostTools}`、
    `RespondSubagent`→`{HostSubagents}`、`RespondInteraction`→`{PermissionBridge}` 均拒绝而非静默忽略。
  - `with_probed_capabilities` 把上述实报位与本机 probe 逐位 AND(host bridge 三项无论 probe 恒 `false`)。
- **真实 e2e 状态**:`tests/external_opencode.rs`(`#[ignore]`,`OPENCODE_BIN`/PATH 发现,缺 binary/登录自跳过
  退绿),**本机 opencode 1.17.15 实跑通过**(M8-3 记录):`BypassPermissions`/`--auto` 驱动
  probe→start→advance→completion→graceful shutdown,在 `--dir` 临时 worktree 内生成 `READY.txt` 并断言其
  **不泄漏**进启动它的 checkout(worktree 隔离),6 个观测事件、约 20s。e2e 覆盖 streaming(sink 多事件)+
  graceful_shutdown(断言 `Graceful`)+ worktree 隔离;resume 由离线单测覆盖(真机 e2e 单 turn 未跑 resume);
  artifacts/usage 由离线 cassette + 单测覆盖。三份 e2e(claude/codex/opencode)结构一致(envrc 加载、
  command_available green-skip、drive_session 同形)。
- **文档**:`docs/capability-matrix.md` 把 OpenCode「实报能力」小节标注为「M8-3 落地、**M8-4 review 定案**」,
  并在 OpenCode e2e 状态段后新增「**三个 runtime adapter 统一接入路径对照(M8-4 review 定案)**」表——逐维
  (进程模型 / trait / decision 臂 / capability fallback / host-tool 门禁 / permission_bridge / 其余能力 /
  cassette 层级 / inline 单测 / cleanup / trace / 真机 e2e)对比三方,并给结论:四维一致、唯一差异 = Claude
  常驻进程 + `permission_bridge`。末尾「各 runtime 当前声明」保守基线 `none()` 表本身不改(它忠实表示默认构建)。
- **验证(全过)**:`cargo fmt --all -- --check` 干净;`cargo clippy --all-targets --features external-opencode
  -- -D warnings` 0 warning;`cargo test --features external-opencode -p agent-lib --lib opencode` 32 passed;
  `cargo test --features external-opencode --test agent_opencode_cassette` 7 passed(opencode_cassette_*);
  `cargo test --all --all-targets` 全 ok、exit 0、0 failed;`git diff --check` 干净。(源码自 M8-3 绿跑后未变,
  本任务仅改 `docs/*.md`;仍重跑上述以完成 review 签核。)

## Milestone 9 — worktree/budget/reconfig/docs/real mixed e2e hardening

目标:把 managed external agent 接入调度、worktree、budget、docs 和真实多 agent e2e。

### [DONE] M9-1 实现 worktree isolation 管理与 cleanup 标记

**上下文**:

- `WorktreeIsolation::{Shared,PerAgentWorktree,EphemeralGitWorktree}` 已存在。
- `ExternalAgentState::cleanup_required` 已标记 cancel 后需要 handle layer sweep。
- 真实 runtime 会产生不可回滚副作用,必须记录 shutdown disposition 和 worktree dirty 状态。

**做什么**:

- 新增 `WorktreeManager` trait 或 runtime adapter hook:
  - prepare(worktree ref, isolation) -> prepared worktree。
  - cleanup(prepared, shutdown disposition)。
  - 标记 residual side effects。
- session registry cleanup 时记录 `ExternalSessionShutdown`。
- ephemeral worktree 在 graceful cleanup 后删除；forced/failed 时保留或标记,策略写入文档。

**验证条件**:

- unit tests 覆盖 shared/per-agent/ephemeral 三种策略。
- forced kill/failed cleanup 不会误标 clean。
- `cargo test -p agent-lib external_worktree`
- 完整验证序列 1-6 全过。

**完成记录**:

- **新增 `src/agent/external/worktree.rs`**（不 feature-gated,default 构建即编译）:
  - `WorktreeManager`（async, object-safe trait）:`prepare(agent_id, base, isolation) -> PreparedWorktree` /
    `cleanup(prepared, ExternalSessionShutdown) -> WorktreeCleanupOutcome`,可作 `Arc<dyn WorktreeManager>`。
  - `PreparedWorktree`（agent_id/isolation/worktree/ephemeral + accessors）与
    `WorktreeCleanupOutcome`（isolation/worktree/`removed()`/`residual_side_effects()`/`safe_to_reuse()`）。
  - `WorktreeError::{Prepare,Cleanup}`（thiserror,带 isolation+path+stable detail,无 secret）。
  - `WorktreeGitExec`（hook）+ 生产实现 `SystemGit`:`git -C <repo> worktree add --detach <path> HEAD` /
    `worktree remove --force <path>`（tokio::process）。策略与 IO 分离,使 placement/teardown 可离线单测。
  - `GitWorktreeManager<G=SystemGit>`:root 默认 `temp_dir()/agent-lib-worktrees`（`with_root` 可覆盖）,
    置于 base checkout 之外避免 git worktree 嵌套。ephemeral 唯一名用 **per-manager 单调计数器**（`AtomicU64`,
    非随机/时钟,遵循本 crate「nondeterminism 由 caller 掌控」约束,`uuid` 未启用 `v4`),并对已存在
    (retained) 目录做 existence 跳过。
- **三策略 prepare/cleanup（design §6.4/§16）**:
  - `Shared` → 原 base,无 IO;cleanup 从不删除,dirty close 仍标 residual。
  - `PerAgentWorktree` → `<root>/agent-<agent_id>` 固定 linked worktree,存在则幂等复用;cleanup 持久保留、
    从不删除,dirty close 标 residual。
  - `EphemeralGitWorktree` → `<root>/ephemeral/<agent_id>-<n>` 每 session 新建;graceful → `git worktree remove`
    删除且 `removed=true residual=false`;`ForcedKill`/`Failed` → **保留**、`removed=false residual=true`
    （`safe_to_reuse()=false`）。**forced kill / failed 绝不误标 clean**。
- **residual 语义与 registry 协调**:cleanup 消费的 disposition 即 `ExternalSessionRegistry::cleanup`/
  `cleanup_agent` 返回、`TraceHandle::record_external_shutdown` 记录的同一 `ExternalSessionShutdown`;
  `residual_side_effects()` 恒等于 `disposition.leaves_residual_side_effects()`,scheduler 用同一 disposition
  既审计又决定删除/保留。§16 文档写明该协调关系。
- **导出**:`external/mod.rs` + `agent/mod.rs` re-export `GitWorktreeManager/PreparedWorktree/SystemGit/
  WorktreeCleanupOutcome/WorktreeError/WorktreeGitExec/WorktreeManager`。
- **文档**:`docs/managed-external-agent.md` §16 重写为已实现 + prepare/cleanup 策略表 + residual 策略段;
  status table「worktree isolation」行改为「已落地(M9-1)」。
- **测试**:`worktree.rs` inline 12 个单测（`external_worktree_*` 前缀以匹配 filter）覆盖三策略 prepare、
  per-agent 幂等复用与跨 agent 路径隔离、ephemeral 唯一性、graceful 删除、forced_kill/failed 保留且标 residual、
  shared/per-agent residual、git add/remove 失败冒泡 `WorktreeError`、以及三 disposition→outcome 协调。
  ScriptedGit 双桩 + 计数器 ScratchRoot,全离线、每测 <1s。
- **验证(全过)**:`cargo fmt --all -- --check` 干净;`cargo test -p agent-lib external_worktree` 12 passed;
  `cargo clippy --all-targets -- -D warnings` 0 warning（另跑 `--features "external-claude-code external-codex
  external-opencode"` 亦 0 warning）;`cargo test --all --all-targets` 全 ok（46 个 test result: ok、exit 0、
  0 failed）;`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 干净;`git diff --check` 干净。

### [DONE] M9-2 接入 usage/cost budget charging

**上下文**:

- `ExternalAgentOutput` 已有 `usage: Option<Usage>` 和 `cost_micros: Option<u64>`。
- `RunContext` / budget ledger 已存在。
- external runtime 可能不报告 usage,不能编造。

**做什么**:

- 在 handler/driver 合适层记录 external usage/cost:
  - 有 usage/cost 时 charge 到 RunContext。
  - 无 usage/cost 时记录 unknown,不要估算。
- 对 budget exceed:
  - adapter advance 前检查。
  - advance 中超限返回 `ExternalAgentError::LimitExceeded` 或 RunContext error,策略统一。
- trace 中记录 usage/cost 来源为 external runtime reported。

**验证条件**:

- unit tests:
  - reported usage/cost 被 charge。
  - missing usage 不 charge。
  - budget exceeded 停止 session 并 cleanup。
- `cargo test -p agent-lib external_budget`
- 完整验证序列 1-6 全过。

**完成记录**:

- **新增 `src/agent/external/budget.rs`**（不 feature-gated,default 构建即编译；沿用外部模块 unboxed
  `ExternalAgentError` 契约,故模块级 `#![allow(clippy::result_large_err)]` 并附说明）:
  - `ExternalUsageCharge`（`Copy`）:`from_output(&ExternalAgentOutput)` 只读取 runtime **上报**的 token/cost,
    token 取 `Usage.total`,缺失时回退 `total_computed()`(仍是上报列求和,**绝不词数估算**);`tokens()`/
    `cost_micros()`/`is_unknown()`;`charge(&RunContext)` 仅对已上报维度 charge,`BudgetError`→
    `ExternalAgentError::LimitExceeded`(经 `limit_exceeded(RunContextError)` 统一映射)。
  - `budget_exhausted(&RunContext) -> Option<BudgetDimension>`:advance 前预检,某 count 维度 `used>=limit`
    即返回该维度(steps>tokens>cost 稳定顺序),unbounded 永不触发;wall-clock 因需 caller elapsed 不在此检查。
  - `ExternalSessionSweeper`(async trait)`sweep(agent_id,&session)->ExternalSessionShutdown`,impl for
    `ExternalSessionRegistry`(委托 `cleanup`)+ blanket `Arc<S>` + `NoSweep`(默认,宿主自管 teardown,返回
    `Graceful`)。
  - `ExternalUsageChargingHandler<H, S=NoSweep>`(`new`/`with_sweeper`/`inner`)impl `ExternalSessionHandler`:
    ① 预检 `budget_exhausted` → 已耗尽则(有 live session 才)`sweep`+`record_external_shutdown`,返回
    `Failed{LimitExceeded}` **不调 inner**;② 调 inner;非 external family / `Paused*` **原样透传不 charge**;
    ③ `Completed` → charge,成功后 `record_external_usage`(source=external runtime reported)原样返回;
    charge 超限 → `sweep`(停 session+cleanup)+记 usage+shutdown,改写为 `Failed{session:Some, LimitExceeded}`
    **保留 session facts** 供 machine 审计。trace id 由 per-handler `AtomicU64` + `run_id` 生成(遵循 crate
    「nondeterminism 由 caller 掌控」,无 clock/RNG)。
- **trace 记录来源**:`context/trace.rs` 新增 `TraceNodeKind::ExternalUsage { tokens_charged, cost_micros_charged }`
  (节点存在即代表来源为 external-runtime-reported;`None`=runtime 未报告=unknown,不估算)+
  `TraceHandle::record_external_usage`;`agent-testkit` trace assertion `describe_kind` 补该 arm。
- **导出**:`external/mod.rs` + `agent/mod.rs` re-export `ExternalUsageCharge/ExternalUsageChargingHandler/
  ExternalSessionSweeper/NoSweep/budget_exhausted`。
- **测试**:`budget/tests.rs` 17 个 `external_budget_*`(全离线、每测 <1s):reported usage+cost charged、
  computed total 回退、missing=unknown 不 charge、partial 只 charge 上报维度、charge 超限→LimitExceeded 且不部分
  记账、`budget_exhausted` 各维度/unbounded/有余额、handler completion charge、trace 记 usage(含 unknown)、
  预检失败不调 inner + sweep session、无 live session 不 sweep、completion 超限停 session+cleanup(ForcedKill 记入
  trace)、Paused 透传不 charge、within-budget 结果不变、registry 实现 sweeper 编译期断言。
- **验证(全过)**:`cargo fmt --all -- --check` 干净;`cargo test -p agent-lib external_budget` 17 passed;
  `cargo clippy --all-targets -- -D warnings` 0 warning(另跑 `--features "external-claude-code external-codex
  external-opencode"` 亦 0 warning);`cargo test --all --all-targets` 全 ok(46 个 test result: ok、0 failed);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 干净;`git diff --check` 干净。

### [DONE] M9-3 支持 turn-boundary external reconfig

**上下文**:

- `ExternalAgentState` 已有 `active_tools` 和 `set_active_tools`。
- 内部 `DefaultAgentMachine` 已有 reconfig registry 机制。
- external runtime tool bridge 可能只能在 session boundary 更新。

**做什么**:

- 定义 external reconfig 策略:
  - 已完成 turn 后可替换 `active_tools`。
  - in-flight session 不支持热替换时返回 `UnsupportedCapability` 或排队到下一 boundary。
- 若需要,复用 `NeedReconfigRegistry` 解析新 tool set,然后更新 `ExternalAgentState.active_tools`。
- 下一次 `NeedExternalSession(Start/Continue)` 必须携带新 tools。

**验证条件**:

- unit tests:
  - boundary reconfig 更新 request.tools。
  - in-flight unsupported hot reconfig 不会悄悄改变 live session。
- `cargo test -p agent-lib external_reconfig`
- 完整验证序列 1-6 全过。

**完成记录**:

- **能力模型（`external/capability.rs`）**:新增 `ExternalCapability::Reconfigure`(serde `reconfigure`,
  `ALL` 8→9,`as_str`)与 `ExternalRuntimeCapabilities.reconfigure: bool`(`none()`=false、`supports()` 映射),
  显式建模「live 会话 mid-turn 热替换(live tool-bridge swap)」能力,遵循「能力从不默认支持」约束。
  三个 runtime adapter(`codex/claude_code/opencode`)`implemented_capabilities()` 声明 `reconfigure: false`、
  `intersect_capabilities()` 逐位 AND;probe/registry 从 `none()` 派生故自动继承 false;testkit 两个
  `permissive_capabilities` 保持「除 resume 外全开」故置 `reconfigure: true`。
- **持久化状态（`external/state.rs`）**:`ExternalAgentState` 新增可序列化 `pending_reconfig: Option<ToolSetRef>`
  (record `#[serde(default, skip_serializing_if="Option::is_none")]`,保持既有 snapshot 字节兼容)+
  accessors `pending_reconfig()/set_pending_reconfig()/take_pending_reconfig()/clear_pending_reconfig()`;
  队列入序列化状态(而非 machine scratch)使 mid-turn 排队的 reconfig 随 snapshot/restore 持久。
- **host-facing 入口（`external/machine.rs`）**:新增 `ExternalReconfigTiming{ NextBoundary(default), Hot }` 与
  `ExternalReconfigOutcome{ Applied, Queued }`,以及 `ExternalAgentMachine::reconfigure(active_tools, timing)
  -> Result<ExternalReconfigOutcome, ExternalAgentError>`(非 sans-io `step`,对应内部 `DefaultAgentMachine::reconfigure`;
  `#[allow(clippy::result_large_err)]` 沿用外部 unboxed 错误契约)。策略(边界判据 = `in_flight.is_none()`):
  ① 边界(Idle/Done/Error)任意 timing → clear pending + `set_active_tools` 立即生效 → `Applied`;
  ② in-flight + `NextBoundary` → `set_pending_reconfig` 排队、**不动 live session** → `Queued`;
  ③ in-flight + `Hot` → `UnsupportedCapability{ capability: Reconfigure }`,**不改动任何状态**
  (active_tools/pending/cursor 全不变),绝不悄悄改 live session。`begin_user_turn()` 顶部
  `take_pending_reconfig()` 折入 `active_tools`,故下一次 `NeedExternalSession(Start/Continue)` 的
  `request.tools`(由 `build_request` 直接读 `active_tools`)携带新集。
- **导出**:`external/mod.rs` + `agent/mod.rs` re-export `ExternalReconfigOutcome/ExternalReconfigTiming`。
- **文档**:`docs/managed-external-agent.md` §3 parity「reconfig」行改为「已落地(M9-3)」、§19 重写为两级
  reconfig 的已实现语义;`docs/capability-matrix.md` 能力清单 8→9、新增 `Reconfigure` 行与两 adapter 实报表
  `reconfigure=false(恒)` 行、保守声明表 reconfigure 行。
- **测试**:`machine/tests.rs` 6 个 `external_reconfig_*`(全离线、每测 <1s):边界(fresh Idle)→Applied 且
  Start request.tools=新集;边界(完成 turn 后 Done)→Applied 且 Continue 带新集;in-flight NextBoundary→Queued
  且 active_tools/live 不变、完成后下一 turn 带新集;in-flight Hot→`UnsupportedCapability{Reconfigure}` 且
  active_tools/pending/cursor 全不变、下一 turn 仍旧集;Hot@boundary→Applied;queued 经 snapshot/restore 后
  下一 turn 仍带新集(断言 JSON 持久 `pending_reconfig`)。
- **验证(全过)**:`cargo fmt --all -- --check` 干净;`cargo test -p agent-lib external_reconfig` 6 passed;
  `cargo clippy --all-targets -- -D warnings` 0 warning(另跑 `--features "external-claude-code external-codex
  external-opencode"` 亦 0 warning);`cargo test --all --all-targets` 全 ok(919 passed、0 failed);
  feature-gated lib 753 passed、真实 e2e `#[ignore]` 自跳过;
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 干净;`git diff --check` 干净。

### [DONE] M9-4 增加真实 DeepSeek 父协调器 + Claude Code/Codex 子 agent ignored e2e

**上下文**:

- 用户要求真实 e2e 以 DeepSeek API 为基础,key 在 `.envrc`。
- 该测试应以 subagent 方式调用 Claude Code 和 Codex,完成多步、多 agent 会话。
- 默认测试不得依赖真实 API/CLI。

**做什么**:

- 新增 ignored integration test,例如 `tests/agent_external_managed_real_e2e.rs`。
- 测试启动前:
  - 从 `.envrc` 或环境读取 DeepSeek endpoint/key/model,不得打印 secret。
  - 检查 Claude Code/Codex binary 和登录态。
  - 检查 feature flags。
  - 缺失时 skip 或返回明确 non-secret 错误。
- 测试结构:
  - DeepSeek 作为父协调器 LLM 或 verifier。
  - 父 agent 通过 `NeedSubagent` 派生 Claude Code child 和 Codex child。
  - 至少一个 child 触发 managed interaction 或 tool bridge。
  - 最终父协调器合并两个 child 结果并给出 summary。
- 使用临时/隔离 worktree,测试结束 cleanup。

**验证条件**:

- 默认 `cargo test --all --all-targets` 不运行真实 e2e。
- ignored test 命令文档化,例如:
  - `direnv allow` 或说明如何加载 `.envrc`。
  - `cargo test --features "external-claude-code external-codex" --test agent_external_managed_real_e2e -- --ignored --nocapture`
- 在可用环境下运行并记录:
  - DeepSeek 请求成功。
  - Claude Code subagent 完成。
  - Codex subagent 完成。
  - 至少一次 external observation 被 replay。
  - final conversation committed。
- 完整验证序列 1-6 全过；真实 e2e 可单独记录,不纳入默认完整验证。

**完成记录**:

- **新增文件 `tests/agent_external_managed_real_e2e.rs`**（顶部
  `#![cfg(all(feature = "external-claude-code", feature = "external-codex"))]`，features 关时整文件
  cfg 掉 = 空 crate，默认 `cargo test --all --all-targets` 从不引用 CLI-adapter 机制）。与既有
  `tests/agent_external_real_e2e.rs`（手搓 `claude`/`codex exec` shell）本质不同：本文件走 **managed** 全链路
  ——真实 `ClaudeCodeAdapter`/`CodexAdapter`（M6/M7）经 `ExternalSessionRegistry` + registry-backed
  `ExternalSessionHandler` 驱动，做结构化 stream-json 解码、sequenced observations、managed permission
  bridge（子 scope `.interaction(ScriptedInteractionHandler::approve_all())` 就地批准 permission pause）。
- **测试结构（3 个 `#[ignore]`）**:
  - `deepseek_coordinator_drives_managed_claude_code_and_codex_subagents`（headline M9-4）:DeepSeek 父协调器
    LLM 先 plan 两个 brief → 通过 `NeedSubagent` 先派生 Claude Code child 再派生 Codex child（均为 managed
    `ExternalAgentMachine`）→ 父协调器把两 child report 合成一条 final status。硬断言:coordinator `Done`;
    spawned == `[ClaudeCode, Codex]`;两 runtime 各完成一次 session;至少一次 external observation 被 replay;
    DeepSeek 调用 ≥2;final text 含 `MANAGED_MULTI_AGENT_OK`。
  - `managed_claude_code_child_commits_turn` / `managed_codex_child_commits_turn`:各自单独驱动一个 managed
    child `ExternalAgentMachine`（start → 结构化流 → completed），断言 `committed_turns(1)` + `pending_none`
    + 至少一次 observation replay，提供确定性的「subagent 完成 / observation replayed / conversation committed」
    证据并演练 managed permission bridge。
- **启动前守卫（缺失即 skip，不泄密）**:`E2eEnv` 从 `.envrc`/env 读 `DEEPSEEK_API_KEY`（缺 → 非密 skip 提示）
  与可选 `DEEPSEEK_BASE_URL/DEEPSEEK_MODEL`;`command_available()` 检 `claude`/`codex`（可用 `CLAUDE_CODE_BIN`
  /`CODEX_BIN` 覆盖）;`build_managed_handler()` 先 `probe`/`codex_probe`，probe 失败 → 非密 skip（视为 auth/runtime
  信号而非测试失败）。`DeepSeekCallLog` 只计数不存 prompt;secret 从不 `eprintln!`。
- **隔离 & cleanup**:每个 child 在 OS temp 下自建 `git init` 一次性 worktree（`make_worktree`，adapter CWD 取
  `config.working_dir()`）;测试结束 `registry.cleanup_agent(agent_id)` 强制关 live session + `cleanup_worktree`
  删临时目录（panic 前先 cleanup），两 adapter `kill_on_drop(true)` 兜底。
- **文档化运行命令**（文件模块 doc）:feature flags、required env、`cargo test --features
  "external-claude-code external-codex" --test agent_external_managed_real_e2e -- --ignored --nocapture`、worktree
  隔离、secret redaction、unsupported-capability fallback（probe 失败即 skip）全部写清。
- **本机限制**:无 DeepSeek key / 无 `claude`/`codex` CLI，真实 e2e 无法执行 → 三测均 `#[ignore]` 且默认套件
  确认为 `0 passed; 3 ignored`（features 开时）/ `0 tests`（features 关时=空 crate）。真实调用需在具备
  DeepSeek+Claude Code+Codex 的环境按上述命令单独运行并记录（M9-6 总验收再核对）。
- **验证序列（全过，本任务无 src 改动，仅新增 feature-gated ignored 测试 + 文档/计划 md）**:
  `cargo fmt --all -- --check` 干净;
  `cargo clippy --all-targets --features "external-claude-code external-codex" -- -D warnings` 0 warning;
  `cargo test --features "external-claude-code external-codex" --test agent_external_managed_real_e2e -- --list`
  = 3 tests，默认运行 `0 passed; 3 ignored`;
  `cargo clippy --all-targets -- -D warnings`（默认）0 warning;
  `cargo test --all --all-targets`（默认）47 个 test 二进制全 `ok`、0 failed（新测 cfg 掉=空 crate，真实 e2e 不跑）;
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 干净;`git diff --check` 干净。

### [DONE] M9-5 更新 docs/examples/capability matrix

**上下文**:

- `docs/managed-external-agent.md` 是设计文档,实现后需要同步实际状态。
- `docs/capability-matrix.md` 应记录实测能力而不是目标能力。
- examples 应展示 scoped effect wiring,不是直接调用 runtime adapter 绕过 machine。

**做什么**:

- 更新:
  - `docs/managed-external-agent.md`
  - `docs/capability-matrix.md`
  - `AGENTS.md` 如需要补运行说明。
- 新增或更新 examples:
  - Claude Code managed external。
  - Codex managed external。
  - OpenCode managed external。
  - mixed external agents。
- 文档必须说明:
  - feature flags。
  - required env vars。
  - ignored test 命令。
  - worktree isolation。
  - secret redaction。
  - unsupported capability fallback。

**验证条件**:

- `cargo test --all --all-targets`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
- examples 如可编译,运行 `cargo check --examples` 或等价命令。
- `git diff --check`
- 完成记录中列出文档和 example 路径。

**完成记录**:

- **新增 4 个受管外部 agent examples（scoped-effect wiring，经 `ExternalAgentMachine` + 作用域
  registry-backed `ExternalSessionHandler` 驱动真实 CLI，绝不直接调 adapter 绕过 machine）**:
  - [`examples/managed_claude_code.rs`](examples/managed_claude_code.rs)（`required-features =
    ["external-claude-code"]`）
  - [`examples/managed_codex.rs`](examples/managed_codex.rs)（`external-codex`）
  - [`examples/managed_opencode.rs`](examples/managed_opencode.rs)（`external-opencode`）
  - [`examples/managed_mixed.rs`](examples/managed_mixed.rs)（`external-claude-code` +
    `external-codex`，顺序驱动 Claude Code 与 Codex 两个 child，演示 mixed external agents）
  - 共享装配 [`examples/support/managed.rs`](examples/support/managed.rs)：runtime-agnostic
    `RegistryHandler`（`get_or_start`→`advance`→折 `ExternalSessionResult`）、`CountingSink`（live
    sink 观测计数）、`ObservationLog`、cfg-gated per-runtime `build_registry`（config→probe→
    `with_probed_capabilities`→`ExternalSessionRegistry`）、`drive_managed_child`（建临时 `git init`
    worktree + machine + `TestScope`(external + `approve_all` interaction) + `drain`，结束
    `cleanup_agent` + 删 worktree）。CLI 缺失/probe 失败即打印**非密** skip 并 exit 0。
  - **门控**:每个 example 经 `Cargo.toml` `[[example]] required-features` 门控，默认
    `cargo check --examples` / `cargo test --all --all-targets` 跳过它们（不引入任何 CLI-adapter 机制）。
    examples 使用 dev-dep `agent-testkit`（Cargo 允许 examples 用 dev-dependencies）。
- **更新 `Cargo.toml`**:新增 4 个 `[[example]]` + `required-features` 条目（附说明注释）。
- **更新 [`docs/managed-external-agent.md`](docs/managed-external-agent.md)**:§3 能力 parity 表由「目标 +
  待实现」翻新为 *as-built*（文本/多轮/流式/tool/approval/question/subagent/cancel/budget/snapshot 均标注
  M2–M9 已落地，host custom-tool 注入诚实标注 capability-gated `false`），表头加 M9-5 落地状态与 examples
  指针；§21 M9 里程碑条目补 as-built + 4 个 example 路径 + 指向 `AGENTS.md`。
- **更新 [`docs/capability-matrix.md`](docs/capability-matrix.md)**:新增「可运行示例与真机验证入口（M9-5）」
  一节——example 表、`cargo run` 命令、feature flags / required env / ignored e2e 命令（含
  `agent_external_managed_real_e2e`）/ worktree isolation / secret redaction / unsupported-capability
  fallback 全部写清。（Codex/OpenCode adapter 的 probe/decoder/adapter 落地表 M7/M8 已在文内，本次不重复。）
- **新增 [`AGENTS.md`](AGENTS.md)**（根目录）:仓库布局、build/lint/test 命令序列、feature flags 表、受管外部
  agent 运行说明（examples 命令、required env 覆盖表、ignored real e2e 命令、worktree isolation / secret
  redaction / unsupported-capability fallback 三条安全属性）、约定。
- **更新 [`README.md`](README.md)**:「可运行示例」补 4 个 managed example 命令与条目；「参考文档」补
  `AGENTS.md` 与 `managed-external-agent.md`。
- **无 `src/` 改动**（纯文档 + feature-gated examples + Cargo 元数据）。
- **验证序列（全过）**:
  `cargo fmt --all -- --check` 干净;
  `cargo clippy --all-targets -- -D warnings`（默认）0 warning;
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode" -- -D
  warnings` 0 warning（含单 feature `external-opencode` 亦 0 warning，验证 cfg + catch-all 无 unreachable/unused）;
  `cargo check --examples`（默认，managed examples 被 required-features 跳过）干净;
  `cargo check --examples --features "external-claude-code external-codex external-opencode"` 干净;
  `cargo test --all --all-targets`（默认）47 个 test 二进制全 `ok`、0 failed;
  `cargo test --all --all-targets --features "external-claude-code external-codex external-opencode"`
  51 个 test 二进制全 `ok`、0 failed（managed examples 作为 `--all-targets` 目标编译通过；真机 e2e `#[ignore]`）;
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 干净;`git diff --check` 干净。
- **本机限制**:无 `claude`/`codex`/`opencode` CLI 与登录，`cargo run --example managed_*` 会走
  非密 skip 分支（打印缺失提示、exit 0）；真机运行需在具备对应 CLI 的环境按 `AGENTS.md` 命令执行。

### [DONE] M9-6 Review：Managed External Agent 总体验收

**上下文**:

这是整个计划的最终 review。目标是确认 managed external agent 达到设计文档要求,并且默认测试仍稳定。

**做什么**:

- 对照 [`docs/managed-external-agent.md`](docs/managed-external-agent.md) §3 能力 parity 表逐项验收:
  - 文本 turn。
  - 多轮 session。
  - 流式输出。
  - tool call。
  - tool approval / permission。
  - user question / choice。
  - subagent。
  - cancel cleanup。
  - budget/usage。
  - artifact。
  - worktree isolation。
  - reconfig。
  - snapshot/restore。
- 对照 `PLAN.md` 风险列表,确认每个风险都有测试或明确限制。
- 确认所有真实 tests `#[ignore]`,所有 cassette 脱敏。
- 确认 default feature 下无重 runtime 依赖。
- 确认 `ExternalAgentMachine` 仍无 IO。

**验证条件**:

- 默认完整验证序列 1-6 全过。
- feature-gated cassette tests 全过:
  - Claude Code。
  - Codex。
  - OpenCode。
- 在具备环境时运行真实 ignored e2e 并记录结果；不具备时记录 skip 条件。
- 完成记录中给出最终能力矩阵摘要和剩余 runtime-dependent 限制。

**完成记录**:

- **性质**:整个 M1–M9 计划的最终验收 review。逐项核对源码 + 测试 + 文档,确认 managed external agent
  达到 `docs/managed-external-agent.md` §3 设计要求且默认测试稳定,**未发现需修复的缺陷或 spec 偏离**,
  故不改任何代码/测试/文档,仅重跑完整验证并签核。(源码自 M9-5 绿跑后未变;本任务仅新增 TODO.md 完成记录
  与 `memory/claude_plan.md` 进度,不影响编译产物——但为最终验收仍重跑了全部离线 + 真机验证。)

- **§3 能力 parity 表逐项验收(13 项,全部 as-built 落地,有源码 + 测试锚点)**:
  1. **文本 turn** — machine 折 `RuntimeDecisionPoint::Completed.output` 进 Conversation(machine/tests.rs
     `external_completed_resume_commits_and_settles_done`);三 adapter 解结构化流(cassette `decodes_full_session`)。
  2. **多轮 session** — registry `get_or_start` 首轮 start / 后续 reattach;`resume` 按 capability 门控。
  3. **流式输出** — `ExternalEventSink` sequenced(`sink.rs`,`ExternalObservedEvent.seq`);三 adapter 镜像
     解码帧到 live sink;真机 e2e 均观测到多事件。
  4. **tool call** — machine external tool phase → `NeedTool` → `RespondToolResults`;host custom-tool 注入仍
     `host_tools=false` 门控关,声明工具的请求以 `UnsupportedCapability{HostTools}` 拒绝(非静默降级)。
  5. **tool approval/permission** — runtime permission `PausedForInteraction` → `NeedInteraction`;真机
     Claude e2e 观测到 permission summary(`0 permission prompts` 因 e2e 用 `approve_all`/bypass 策略)。
  6. **user question/choice** — runtime question/choice → `NeedInteraction(Question)`(M3-2)。
  7. **subagent** — `PausedForSubagent` → `NeedSubagent` + `spawn_agent` tool-bridge 特判(M3-1/3-3);真机
     mixed e2e:DeepSeek 协调器派生 Claude + Codex child。
  8. **cancel cleanup** — registry `cleanup`/`cleanup_agent` 强关 live session → `ExternalSessionShutdown`
     disposition;`shutdown.rs` `leaves_residual_side_effects`(Forced/Failed→true)+ 单测;adapter
     `kill_on_drop` 兜底。
  9. **budget/usage** — `ExternalUsageChargingHandler` 把 runtime usage/cost 计入 run budget(M9-2,`budget.rs`
     + tests)。
  10. **artifact** — patch/diff/test/file artifact refs(§18)。
  11. **worktree isolation** — `WorktreeManager`/`GitWorktreeManager` prepare/cleanup + residual 标记(M9-1,
      `worktree.rs`);真机三 e2e 均断言产物**不泄漏**回启动它的 checkout。
  12. **reconfig** — `ExternalAgentMachine::reconfigure` + `ExternalReconfigTiming`/`ExternalReconfigOutcome`
      (boundary 应用/排队;in-flight `Hot` → `UnsupportedCapability{Reconfigure}`,M9-3)。
  13. **snapshot/restore** — `ExternalAgentState` 持久化 spec/session/cursor/conversation;registry 按
      `ExternalSessionRef` reattach/`resume`(capability-gated),不可 resume 以 `ResumeUnavailable` 显式失败;
      mid-turn cursor + 恢复去重由 machine/tests.rs resume 用例覆盖(observation dedup on resume,§5.5)。

- **PLAN.md §风险逐条有测试或明确限制**:
  - runtime 协议漂移 → parser 私有化 + cassette 覆盖(3×7 cassette tests)+ capability probe;raw schema 不导出。
  - tool bridge 能力不对称 → capability model 显式 `UnsupportedCapability`(`capability.rs` + adapter 单测拒绝式)。
  - cancel 后副作用残留 → `ExternalSessionShutdown` 分类 + worktree residual 标记(`shutdown.rs`/`worktree.rs` + 单测)。
  - stream 事件重复 → `ExternalObservedEvent.seq` 单一 replay 进度(`sink.rs`)。
  - 恢复 mid-turn scratch → serializable cursor + resume 去重测试(machine/tests.rs)。
  - (ACP 两条风险属 M10 未来里程碑,不在本 M9 验收范围。)

- **`#[ignore]` + 脱敏确认**:全部真实 endpoint/CLI 测试均 `#[ignore]`(external_claude_code / external_codex /
  external_opencode / agent_external_managed_real_e2e / agent_external_real_e2e / integration_anthropic /
  integration_openai_resp / integration_normalization,共 10 处 ignore 标记),缺 binary/登录/key 时 green-skip。
  三份 cassette 各含 `*_cassette_is_secret_free` 断言;committed fixtures
  `tests/fixtures/external/{claude_code,codex,opencode}/full_session.json` 凭据形态扫描(sk-*/api_key/bearer/
  auth_token/password)干净。

- **默认无重 runtime 依赖**:`external-claude-code`/`external-codex`/`external-opencode` 三 feature 均 `= []`
  (off by default,仅门控已有 tokio process 支持);`[dependencies]` 无 `agent-client-protocol*`(M10 未起)。
  默认 `cargo test --all --all-targets` 不编译任何 adapter 机制。

- **`ExternalAgentMachine` 仍无 IO**:`src/agent/external/machine.rs` + `machine/` 代码内无 `.await`/`async fn`/
  `tokio::`/`std::process`/`Command::new`/`std::fs::{File,read,write}`/`reqwest`(仅注释提及 `spawn_agent`
  tool-bridge 概念,非真实 IO)。IO 全部在 handler/adapter 层。

- **验证(全过)**:
  - **默认序列 1-6**:① `cargo fmt --all -- --check` 干净;② 聚焦测试见下;③ `cargo clippy --all-targets
    -- -D warnings` 0 warning;④ `cargo test --all --all-targets` exit 0(753 lib + 全 integration,真实
    endpoint 正确 ignored);⑤ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` exit 0;
    ⑥ `git diff --check` 干净。
  - **全 feature clippy**:`cargo clippy --all-targets --features "external-claude-code external-codex
    external-opencode" -- -D warnings` 0 warning;全 feature `cargo test --all --all-targets --features "…"`
    exit 0(753 lib + adapter inline 单测 + cassette)。
  - **feature-gated cassette tests 全过**:Claude Code 7 / Codex 7 / OpenCode 7(共 21 passed)。
  - **真机 ignored e2e(本机 2026-07 全部实跑绿)**:
    - `external_claude_code`:1 passed,11.6s,6 观测事件(2 text)。
    - `external_codex`:1 passed,59.8s,5 观测事件(2 text)。
    - `external_opencode`:1 passed,19.1s,4 观测事件(1 text)。
    - `agent_external_managed_real_e2e`(DeepSeek 协调器 + Claude/Codex child):3 passed,188.8s——
      claude child 7 观测 / codex child 4 观测 / coordinator 2 DeepSeek 调用 + 28 观测、0 managed interaction。
    - 三 e2e 均在临时 `git init` worktree 内生成 `READY.txt` 并断言其**不泄漏**回启动它的 checkout。

- **最终能力矩阵摘要(三 runtime 默认构建 = 保守 `none()`;下列为实报能力,以 ignored e2e 跑绿为准)**:

  | 能力 | Claude Code | Codex | OpenCode |
  |---|---|---|---|
  | streaming | ✅ | ✅ | ✅ |
  | resume(多轮) | ✅ | ✅ | ✅ |
  | artifacts | ✅ | ✅ | ✅ |
  | usage/cost | ✅ | ✅ | ✅ |
  | graceful_shutdown | ✅ | ✅ | ✅ |
  | permission_bridge | ✅(常驻 stdio,host-pausable) | ❌(自主) | ❌(自主) |
  | host_tools | ❌(§12.3,首版保守) | ❌ | ❌ |
  | host_subagents | ❌(`spawn_agent` 经 tool-bridge 特判,非 host 注入) | ❌ | ❌ |

  进程模型:Claude = 常驻 stdio 进程(点亮 permission bridge);Codex/OpenCode = 一进程/一 turn 自主运行。
  唯一结构性差异 = Claude 常驻 + `permission_bridge=true`;三者共享同一 `ExternalRuntimeAdapter`/
  `ExternalRuntimeSession` trait + `with_probed_capabilities` 逐位取交模型。

- **剩余 runtime-dependent 限制(诚实记录,非 workaround)**:
  1. **host custom-tool 注入(`host_tools`)三 runtime 恒 `false`**——CLI 自主执行工具,流里无 host-pausable
     tool 帧;声明工具的请求 fail-fast `UnsupportedCapability{HostTools}`。首个可能点亮的 adapter 是 M10 ACP
     (未来里程碑)。
  2. **permission_bridge 仅 Claude Code 为 `true`**;Codex/OpenCode 自主解审批(`--auto`/bypass),host 无法
     就地插入审批(能力矩阵显式暴露)。
  3. **真实能力矩阵仍以 ignored e2e 跑绿为准**——默认构建保守 `none()`,实报能力需 `with_probed_capabilities`
     本机 probe 逐位取交;缺 binary/登录时能力位为 `false` 且 e2e green-skip。
  4. **真机 e2e 单 turn 未跑 cross-process resume**——resume/去重由离线 machine/tests.rs 覆盖;artifacts/usage
     由离线 cassette + 单测覆盖。
  5. **协议漂移**由 crate/CLI 升级 + cassette 兜底,非静态保证。
  - 以上均为设计意图的能力边界,由 capability model 显式暴露、fail-fast,无 silent degradation。

---

## Milestone 10 — ACP(Agent Client Protocol)managed adapter

目标:新增一个 **ACP client 形态**的 managed adapter,用官方 crate `agent-client-protocol` /
`agent-client-protocol-schema` 实现,而**不手写** JSON-RPC 分帧与协议类型(Zed 侧测试更充分、兼容性更好)。

关键事实(调研结论,2026-07):

- **ACP 是单一标准协议,不是各家变体**。它由 Zed 发起、现托管在中立组织
  `github.com/agentclientprotocol`,传输为 **JSON-RPC 2.0 over stdio**,当前 wire 版本 = 1,设计目标类似
  LSP:一套 client 实现对接所有 ACP agent。
- **一套 client 通吃**:Gemini CLI(原生)、OpenCode(原生)、Claude(经
  `@zed-industries/claude-agent-acp` adapter 进程)、Codex(经 Zed adapter 进程)。因此**不做「每家一个
  adapter」**,只做一个 `AcpAdapter`,启动命令(binary + args)由 `AcpConfig` 区分,wire 层完全复用。

- **本机实测的 ACP agent 与配置/凭据要求(重要,直接影响 `AcpConfig` 与 e2e 设计)**。三个 bridge 都是 npm
  包 `@agentclientprotocol/*` 的 Node 脚本,**均自动读取各自 CLI 的缺省配置文件与登录态**,`agent-lib`
  不传 key:
  - `claude-agent-acp`(本机 `@agentclientprotocol/claude-agent-acp@0.59.0`):内嵌
    `@anthropic-ai/claude-agent-sdk`,由 SDK 自动读取 **Claude Code 的 `~/.claude` 配置/登录态**
    (bundle 引用 `.claude` / `pathToClaudeCodeExecutable`)。**不用**官方 Anthropic API/key。
  - `codex-acp`(本机 `@agentclientprotocol/codex-acp@1.1.4`):是**外挂 wrapper**,启动时 **spawn 真实
    `codex` 进程**(`CODEX_PATH ?? "codex"` → `codex app-server`),因而完全继承 **Codex 的 `~/.codex`
    配置与 `auth.json` 登录态**。**不用**官方 OpenAI API/key。
  - `opencode acp`(本机 opencode 1.17.15):就是 opencode 本体的子命令(同进程,无外部 adapter),读
    **OpenCode 自身的 `~/.config/opencode/opencode.json` 等缺省配置**。
  - 共同含义:三者都从**各自 CLI 的配置文件 + 登录态**取凭据,而非 `agent-lib` 传入的 provider 凭据(与
    M6-M8 一致,凭据边界在被包装的 CLI 侧)。`agent-lib` 不承载 API key。
  - **配置能力要求(重点,非「必须继承」而是「能继承 + 能注入/替换」)**:`AcpConfig` 必须让 adapter 做到两点——
    1. **默认继承**宿主既有环境(默认把父进程 env 传给子进程,`working_dir` 归 worktree),使一台按缺省配置
       登录好的机器开箱即用;
    2. **能注入/替换配置**:允许显式指定/覆盖各 CLI 的配置来源,而非只能用缺省路径。**这是本地测试的硬需求**
       ——本机跑的这三个 agent 都**不是用缺省配置**,没有覆盖能力就无法测。
  - 各 CLI 实测支持的覆盖入口(供 `AcpConfig` 设计参考,以本机版本实测为准):
    - Codex(`codex-acp` → `codex app-server`):`CODEX_HOME` 指向替代配置目录(`~/.codex` 的替身,含
      `config.toml` / `auth.json`);`CODEX_CONFIG`、`CODEX_PATH`(指定 codex 二进制)也被 `codex-acp` 识别;
      底层 codex 支持 `-c key=value` 覆盖单项。
    - Claude(`claude-agent-acp` → Claude Agent SDK):经 SDK/`claude` 的 `--settings <file-or-json>` 等注入
      额外 settings;登录态默认取 `~/.claude`。
    - OpenCode(`opencode acp`):`OPENCODE_CONFIG`(指向配置文件)、`OPENCODE_CONFIG_DIR`、
      `OPENCODE_CONFIG_CONTENT`(内联配置)、`XDG_CONFIG_HOME` 等 env,以及 `--cwd`。
  - 因此 `AcpConfig` 的 `env` override / args 是**一等能力**(不是仅供边角覆盖):既能不设、走继承,也能注入上述
    任一 env / flag 指向测试专用配置目录或文件。约束依旧:`Debug` 脱敏、**绝不**承载 API key(凭据仍由目标
    CLI 的配置/登录态提供)。
- **能力仍需协商**:wire 一致但 feature parity 不保证。`initialize` 阶段协商 capabilities
  (`loadSession` / fs / terminal 等可选),映射到已有的 `ExternalRuntimeCapabilities` 8 项,复用
  `with_probed_capabilities` 的逐位取交模型。
- **角色**:本 adapter 扮演 ACP **client**(host = client),ACP agent 子进程扮演 **agent**。方向:
  - client→agent(我们发):`initialize` / `authenticate` / `session/new` / `session/load`(gated) /
    `session/prompt` / `session/set_mode` / `session/cancel`(notification)。
  - agent→client(我们收/应答):`session/update`(notification,流式进度) / `session/request_permission` /
    `fs/read_text_file` / `fs/write_text_file` / `terminal/*`。
- **首个点亮 host-pausable 决策臂的 adapter**:三个 CLI adapter(M6/M7/M8)因自主运行,
  `permission_bridge` 恒为 `false`。ACP 的 `session/request_permission` 是 agent→client 的 **request**,
  天然映射到 machine 早已实现的 `PausedForInteraction` → `NeedInteraction` → `RespondInteraction` 路径,使
  `permission_bridge` 第一次为 `true`。`fs/*` / `terminal/*` 是 client 侧**环境服务**(adapter 直接对
  worktree 兑现并汇报为 observation),不等同于 `NeedTool` 意义上的 host tool;`host_tools`(经 client MCP)
  为后续能力,首版保守 `false`。

通用约束(在 Milestone 1 通用约束之上追加):

- ACP 相关代码全部 gate 在**非默认** feature `external-acp` 之后;默认 `cargo test --all --all-targets`
  不编译 ACP adapter、不拉入 `agent-client-protocol*` 依赖、不依赖任何 ACP agent binary/登录态。
- 官方 crate 只在 adapter 层使用;其 raw 协议类型**不得**成为 `agent-lib` 稳定 public API(与三个 CLI
  adapter 的 "不导出 raw frame 类型" 纪律一致)。ACP wire 版本作为一个探测/协商项记录,漂移由 crate 升级 +
  cassette 兜底。
- 复用现有 `ExternalRuntimeKind::Custom(String)` 承载 ACP(如 `Custom("acp")` 或带 agent 标签),**不新增**
  enum 变体(除非 review 明确需要一等公民);machine / driver / `ExternalAgentState` **不改**。

### [DONE] M10-1 增加 `external-acp` feature、ACP 依赖与 `AcpConfig` / capability 协商

**上下文**:

- `Cargo.toml` 现有 `external-claude-code` / `external-codex` / `external-opencode` 三个非默认 feature,均
  为空 feature(`= []`),因为 CLI adapter 只复用 tokio 的 process 支持,无新依赖。ACP 不同:它要引入官方
  crate `agent-client-protocol`(runtime)+ `agent-client-protocol-schema`(协议类型)。
- 三个 CLI adapter 的 `*/config.rs` 是纯数据 serde DTO(binary/env/working_dir/permission_mode/timeout,
  手写 `Debug` 脱敏 env),`*/probe.rs` 通过可注入 exec 探测 capabilities。ACP 的 capability 来源不同:它来自
  `initialize` 握手返回的 agent capabilities,而非 `--help` 文本。
- capability model 见 `src/agent/external/capability.rs`(`ExternalRuntimeCapabilities` 8 项 +
  `ExternalCapability` + `unsupported(...)`);intersect 模型见任一 `*/adapter.rs` 的
  `with_probed_capabilities` / `intersect_capabilities`。

**做什么**:

- `Cargo.toml`:
  - 新增非默认 feature `external-acp`,并把 `agent-client-protocol` / `agent-client-protocol-schema` 作为
    **optional** dependency,只在该 feature 下启用(`external-acp = ["dep:agent-client-protocol",
    "dep:agent-client-protocol-schema"]`)。
  - 在 feature 注释里写明:默认关闭,开启才拉入 ACP 依赖;确认所选 crate 版本对应的 ACP wire 版本并在注释/常量
    中记录(以本机可拉取的最新稳定版实测为准,不硬编码假设)。
- 新增 feature-gated 模块 `src/agent/external/acp/{mod.rs,config.rs}`,在 `src/agent/external/mod.rs` 以
  `#[cfg(feature = "external-acp")]` 挂载并 re-export `AcpConfig`(以及后续任务的类型)。
- `AcpConfig`(纯数据 serde DTO,可持久化):
  - `binary` + `args`(承载不同 ACP agent 的启动行;以本机实测为准:`claude-agent-acp`(无参数)、
    `codex-acp`(无参数)、`opencode acp`(即 `binary=opencode`、`args=["acp"]`);首版只需能表达任意
    `program + args`)。
  - **配置继承 + 注入(一等能力)**:adapter spawn 子进程时**默认继承宿主完整 env**(让缺省配置/登录态开箱
    即用),同时 `AcpConfig` 必须能**注入/替换**各 CLI 的配置来源(本地测试硬需求,因为本机三个 agent 都不用
    缺省配置)。为此 `AcpConfig` 至少提供:
    - `env` override(`BTreeMap`,手写 `Debug` 只印 key + `<redacted>`,与三个 CLI config 一致):在继承基础上
      **追加/覆盖**指定 env,用于指向测试专用配置——实测入口:Codex 用 `CODEX_HOME`(替代 `~/.codex`)/
      `CODEX_CONFIG` / `CODEX_PATH`,OpenCode 用 `OPENCODE_CONFIG` / `OPENCODE_CONFIG_DIR` /
      `OPENCODE_CONFIG_CONTENT` / `XDG_CONFIG_HOME`,Claude 经 `claude --settings <file-or-json>`(走 `args`)。
    - 一个显式开关表达「是否继承父进程 env」(默认继承);清空继承时须让调用方能补齐 `HOME` 等,否则 bridge
      找不到配置即失败。
    - **约束不变**:`env` **绝不**承载 API key;凭据始终由目标 CLI 的配置/登录态提供。
  - `working_dir`(worktree)。
  - `ExternalPermissionMode`(不像 CLI 靠启动 flag,而是决定收到 `session/request_permission` 后的**默认
    应答策略**:`Plan` 拒绝 mutating、`BypassPermissions` 自动 allow 等;首版只需把 mode 存下并在 doc 说明
    语义,真正应答逻辑在 M10-3)。
  - `timeout`。
- 便捷预设构造器(基于本机实测的启动行,不硬编码 key):
  - `AcpConfig::claude_agent_acp()` → `binary="claude-agent-acp"`;doc 注明依赖本机 **Claude Code** 配置/登录态。
  - `AcpConfig::codex_acp()` → `binary="codex-acp"`;doc 注明依赖本机 **Codex** 配置/登录态。
  - `AcpConfig::opencode_acp()` → `binary="opencode"`、`args=["acp"]`(OpenCode 自带 ACP)。
  - 通用 `AcpConfig::new(binary, args)` 兜底任意 ACP agent。
- capability 协商:提供一个把 ACP `initialize` 返回的 agent capabilities → `ExternalRuntimeCapabilities`
  的映射函数(纯函数,输入用 schema crate 的 capability 类型或其中立投影,不做 IO;真正的握手 IO 在 M10-3)。
  保守基线 `none()`,只把协商确认的位打开:`loadSession` → `resume`,fs/terminal 广告 →(记录但不直接等于
  `host_tools`,见里程碑说明),始终 `permission_bridge=true`(ACP 定义了 request_permission)、
  `streaming=true`(session/update)、`graceful_shutdown=true`。`host_tools`/`host_subagents` 首版 `false`。

**验证条件**:

- `AcpConfig` serde round-trip;`Debug` / `Display` 不泄漏 env secret(断言注入的假 secret 不出现)。
- 预设构造器单测:`claude_agent_acp()` / `codex_acp()` / `opencode_acp()` 产出预期 `binary`/`args`
  (`opencode_acp` 的 `args` 含 `"acp"`);断言默认 `AcpConfig` 不含任何 API-key 字段(config 无 key 概念,
  只有 `env` override),证明凭据边界在被包装的 CLI 侧。
- 配置继承/注入能力单测(不 spawn 真实进程,断言 adapter 构造的 spawn env/args):默认继承父进程 env;
  设置 `env` override 后,该键出现在子进程 env(如 `CODEX_HOME=/tmp/test-codex` / `OPENCODE_CONFIG_DIR=...`);
  「不继承」开关下父进程 env 不透传。证明本地测试可指向非缺省配置目录。
- capability 映射纯函数单测:给定「支持 loadSession + fs」的握手投影 → `resume=true`、`permission_bridge=true`、
  `streaming=true`、`host_tools=false`;给定空/最小握手 → 只有协议保证位为 true,其余 false。
- 默认构建不受影响:`cargo build`(无 feature)不拉入 ACP crate;`cargo build --features external-acp` 通过。
- 聚焦测试:
  - `cargo test -p agent-lib --features external-acp acp_config_roundtrip`
  - `cargo test -p agent-lib --features external-acp acp_capabilities_from_initialize`
- 完整验证序列 1-6 全过(其中 clippy/test/doc 需分别在默认与 `--features external-acp` 两种配置下跑通)。

**完成记录**：

- **Cargo.toml**：新增非默认 feature `external-acp = ["dep:agent-client-protocol",
  "dep:agent-client-protocol-schema"]`，把 `agent-client-protocol` / `agent-client-protocol-schema`
  作 **optional** dep（`version = "1"`）。feature 注释写明默认关闭、开启才拉入 ACP 依赖，并指向
  `agent::external::ACP_WIRE_VERSION`。实测解析版本：`agent-client-protocol 1.2.0` /
  `agent-client-protocol-schema 1.4.0`（crates.io 最新稳定）；schema crate 的
  `ProtocolVersion::LATEST == V1`，故 **ACP wire 版本 = 1**，记为常量 `ACP_WIRE_VERSION: u16 = 1`。
- **新增 feature-gated 模块** `src/agent/external/acp/{mod.rs,config.rs}`，在
  `src/agent/external/mod.rs` 以 `#[cfg(feature = "external-acp")]` 挂载，re-export
  `AcpConfig` / `AcpNegotiatedCapabilities` / `capabilities_from_initialize` / `acp_runtime_kind`
  / `ACP_WIRE_VERSION` / `ACP_RUNTIME_LABEL`。
- **`AcpConfig`**（纯数据 serde DTO）：`binary` + `args`（任意 program+args）、`env` override
  （`BTreeMap`，手写 `Debug`+`Display` 只印 key + `<redacted>`）、`inherit_env` 开关（默认继承宿主
  完整 env，可 `without_inherited_env()` 清空只留 override）、`working_dir`、`ExternalPermissionMode`
  （首版只存下 + doc 语义，应答逻辑留 M10-3）、`timeout`。**无任何 API-key 字段**——凭据边界在被包装
  CLI 侧。纯函数 `resolved_env(parent_env)` 让 spawn env 可离线断言（默认继承+override 覆盖；不继承时
  只留 override）。预设：`claude_agent_acp()`（`claude-agent-acp`）、`codex_acp()`（`codex-acp`）、
  `opencode_acp()`（`opencode` + `args=["acp"]`）、通用 `new(binary, args)`。
- **capability 协商**：纯函数 `capabilities_from_initialize(&AcpNegotiatedCapabilities)` 从中立投影
  （`load_session` / `fs` / `terminal`，**不**暴露 schema crate raw 类型）映射到
  `ExternalRuntimeCapabilities`：保守基线 `none(Custom("acp"))`，只开协议保证位
  `streaming` / `permission_bridge` / `graceful_shutdown = true` 与协商位 `resume = load_session`；
  `fs`/`terminal` 广告仅记录**不**等于 `host_tools`；`host_tools` / `host_subagents` / `artifacts`
  / `usage` / `reconfigure = false`（后者由 M10-3 live adapter 视 crate 暴露决定）。
- **测试**（6 个 lib 单测，全绿）：`acp_config_roundtrip`（含继承/清空两态 round-trip、无 key 字段、
  `inherit_env=true` 与空集合被 skip）、`acp_config_presets_carry_expected_launch_lines`
  （`opencode` args 含 `"acp"`、均无 key、默认继承+prompt）、
  `acp_config_debug_and_display_redact_env_secrets`（Debug/Display 均不泄漏注入的假 secret）、
  `acp_config_resolved_env_inherits_by_default_and_injects_overrides`、
  `acp_capabilities_from_initialize`（loadSession+fs → resume/permission_bridge/streaming true、
  host_tools false；空握手只有协议保证位）、`acp_wire_version_and_runtime_label_are_stable`。
- **连带修复（feature 统一副作用，属本任务范围）**：`agent-client-protocol-schema` 经 schemars/serde_with
  传递启用 `serde_json/preserve_order`，全局把 `serde_json::Value` 布局变大，触发两类回归——
  (1) `clippy::large_enum_variant`：`conversation/pending/turn.rs` 的 `PendingTurnState::AssistantInProgress`
  改为 `Box<PendingMessage>`（干净、两种构建都受益）；dev-only `agent-testkit` 的 `LlmOutcome` 因构造点
  众多、装箱会波及大量 cassette 测试，改用带说明的 `#[allow(clippy::large_enum_variant)]`（沿用本仓已有的
  `result_large_err` allow 先例）。(2) 顺序敏感断言：`agent/state/tests.rs` 的
  `state_json_has_expected_top_level_data_shape` 依赖 `serde_json` 默认字母序 key，preserve_order 下变插入
  序而失败——改为 `keys.sort()` 后比较（key 顺序非契约，仅断言存在哪些 key）。已全仓搜索，无其它顺序敏感的
  精确序列化字符串断言。
- **验证序列 1-6 全过**（默认 + `--features external-acp` 两配置）：`cargo fmt --all -- --check` 通过；
  聚焦测试 `acp_config_roundtrip` / `acp_capabilities_from_initialize` 通过；
  `cargo clippy --all-targets -- -D warnings`（默认）与 `--features external-acp` 均无告警；
  `cargo test --all --all-targets`（默认）全绿、`--features external-acp` 全绿（lib 669 passed，
  含 6 个 acp 单测；集成/ignored 与默认一致）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  默认与 `--features external-acp` 均通过（修好两处 `[`Display`]` 未解析 intra-doc link → 限定
  `std::fmt::Display`）；`git diff --check` 干净。
- **feature 隔离证据**：`cargo tree -e normal -i agent-client-protocol`（默认）报 "did not match any
  packages"（未拉入）；`--features external-acp` 下 `agent-client-protocol v1.2.0 └── agent-lib` 出现。
  `cargo build`（无 feature）不含 ACP crate；`cargo build --features external-acp` 通过。

### [DONE] M10-2 用官方 crate 建立 ACP client 连接与 `session/update` 观测解码

**上下文**:

- 与三个 CLI adapter 手写 `*/decoder.rs`(逐行 `serde_json::Value` 防御式导航)不同,ACP 用官方 crate 的
  JSON-RPC runtime 收发消息;本任务只把 crate 的回调/事件**归一化**成中立的
  `ExternalObservedEvent`(见 `src/agent/external/mod.rs` 的 event 词汇表:`SessionStarted` / `TextDelta` /
  `CommandStarted` / `CommandFinished` / `FilePatch` / `PermissionRequested` / `ToolStarted` /
  `ToolFinished` / `TaskUpdated` / `SessionCompleted`),不导出 crate 的 raw 类型。
- `session/update` 是 agent→client 的 notification,携带 message chunk / tool_call / tool_call_update /
  plan / diff 等;这是流式进度来源,对应 `ExternalObservedEvent` 观测流。
- `session/prompt` 是一问一答:发送后 agent 通过一串 `session/update` 汇报,最终 `session/prompt` response 带
  `stopReason` 表示本 turn 结束。

**做什么**:

- 在 `src/agent/external/acp/` 新增 client 连接层(feature-gated),封装官方 crate 的 stdio client:
  - 用 crate 提供的 client/connection API 启动并 attach 到 ACP agent 子进程(spawn 走可注入的 launcher
    trait,便于离线单测;生产用 `tokio::process`,stdin/stdout piped、stderr 丢弃防泄漏、`kill_on_drop`、每读
    超时——与三个 CLI adapter 的 IO 纪律一致)。
  - 实现 crate 的 client 回调 trait,把 `session/update` 归一化成 `ExternalObservedEvent`,跨 turn 单调
    分配 `seq`;把 `session/request_permission` / `fs/*` / `terminal/*` 的到达**暂存**为待 M10-3 处理的
    决策/服务(本任务只需能识别与缓存,不必完整应答)。
- `session/update` → 观测映射(用 §10.3 已有词汇表,不新增变体):
  - assistant message chunk → `TextDelta`。
  - tool_call 开始 / update / 完成 → `ToolStarted` / `ToolFinished`(bash 类可用 `CommandStarted` /
    `CommandFinished`,如 crate 暴露足够信息)。
  - diff / 文件变更 → `FilePatch`。
  - plan / todo 更新 → `TaskUpdated`。
  - session 建立 → `SessionStarted`(带 ACP session id);turn 结束(prompt response stopReason)→
    `SessionCompleted`。
- 容忍/错误纪律(与 CLI decoder 一致):crate 报的协议错误 / 未建模的 update 种类 → 容忍(无观测)或
  归类到 `ExternalAgentError::Protocol`;所有诊断为固定字符串,永不夹带 prompt / 文件内容 / 凭据。

**验证条件**:

- 观测归一化单测:构造若干 `session/update`(text / tool_call / diff / plan)投影,断言产出的
  `ExternalObservedEvent` 序列与单调 `seq`;未知 update 种类被容忍。
- 一个离线 cassette(`tests/fixtures/external/acp/`,`assert_no_secrets` 脱敏)覆盖:单 turn text-only、
  含 tool_call、含 diff、以 stopReason 收尾 → `SessionCompleted`。
- 聚焦测试:
  - `cargo test -p agent-lib --features external-acp acp_session_update_maps_to_observations`
  - `cargo test -p agent-lib --features external-acp acp_cassette`
- 完整验证序列 1-6 全过(默认 + `--features external-acp` 两种配置)。

**完成记录**：

- **新增两个 feature-gated 模块** `src/agent/external/acp/{decoder.rs,connection.rs}`，在
  `acp/mod.rs` 以 `mod decoder; mod connection;` 挂载并 re-export 中立类型
  `AcpStreamDecoder` / `AcpDecision` / `PendingClientRequest` / `AcpLauncher` /
  `SpawnedAcpAgent` / `TokioProcessLauncher`；`src/agent/external/mod.rs` 的
  `#[cfg(feature = "external-acp")]` re-export 块同步补齐,故
  `agent_lib::agent::external::{AcpStreamDecoder, …}` 路径可用。
- **公共 API 不泄漏 crate raw 类型(M10-2/M10-4 硬约束)**:decoder 唯一 **public** 解码入口是
  `push_jsonrpc_line(&str) -> Result<Option<AcpDecision>, ExternalAgentError>`;吃 schema-typed 值的
  `on_session_update(&SessionUpdate)` / `finish_turn(StopReason)` / `session_started` 均为 `pub(crate)`,
  仅供 connection 层调用。`AcpDecision`(`Completed{output}` / `Failed{error}`)与 `PendingClientRequest`
  (`Permission` / `ReadFile` / `WriteFile` / `Terminal`)全为中立类型,不含任何 `agent-client-protocol*`
  类型。fixture 也只存 raw JSON-RPC 帧串 + 中立 `ExternalObservedEvent`/`AcpDecision`。
- **`AcpStreamDecoder`(§10.3 已有词汇表,不新增变体)**:一个 decoder 跨整个 session,`seq` 跨 turn 单调分配
  (design §5.5 replay dedup)。映射:`agent_message_chunk` → `TextDelta`(并累积为 turn summary);
  `tool_call`(`execute` 类)→ `CommandStarted`、其余 → `ToolStarted`,按 `toolCallId` 关联到
  `tool_call_update` 终态 → `CommandFinished` / `ToolFinished`;`diff` content → `FilePatch`;
  `plan` → 每 entry 一条 `TaskUpdated`(`task_id` = 下标);`session/new` result 的 `sessionId` →
  `SessionStarted`;`session/prompt` result 的 `stopReason`(EndTurn/MaxTokens/… 全部)→ `SessionCompleted`
  + `AcpDecision::Completed`;JSON-RPC `error` → `AcpDecision::Failed{Runtime}`。`session/request_permission`
  额外 emit 中立 `PermissionRequested` 观测并缓存 `PendingClientRequest::Permission`;`fs/*` / `terminal/*`
  仅缓存(M10-2 不应答,留 M10-3 用 `take_client_requests()` 服务)。`PromptResponse.usage` 属未启用的
  unstable feature,故 output 的 `usage`/`cost_micros`/`artifacts` 保持空,与 M10-1 capability 基线一致。
- **容忍/错误纪律**:空行、缺 method/result/error 的 JSON-RPC 对象、未建模的 `session/update` 种类(thought /
  user echo / 其它)均容忍(无观测);非 JSON、非对象、`session/update` params 无法解成 schema 类型 → 归类
  `ExternalAgentError::Protocol`。所有诊断为固定字符串,永不夹带 prompt / 文件内容 / 凭据。
- **connection 层**:可注入的 `AcpLauncher` trait(`async fn launch(&AcpConfig) -> SpawnedAcpAgent`);
  `SpawnedAcpAgent` 行帧 JSON-RPC 传输——`write_line` 写一行并 flush,`read_line` 每读带
  `config.timeout()` 超时(超时/断开 → `ExternalAgentError::SessionLost`,EOF → `Ok(None)`),内部把流装箱
  为 `Box<dyn AsyncRead/AsyncWrite + Send + Unpin>` 以便测试注入内存流。生产 `TokioProcessLauncher` 用
  `tokio::process`:`env_clear` 后灌入 `resolved_env`、`current_dir(working_dir)`、stdin/stdout piped、
  stderr 丢弃(防凭据泄漏)、`kill_on_drop(true)`,并保留 `Child` 句柄让 drop 时回收——与三个 CLI adapter
  的 IO 纪律一致。live `initialize`/`session/new`/`session/prompt` 驱动与 permission bridge 留 M10-3。
- **测试(全绿)**:decoder 6 个 inline 单测(`acp_session_update_maps_to_observations`——text/plan/
  非命令 tool_call/命令 tool_call/diff 全覆盖并断言单调 `seq` 与 `Completed` summary、
  `acp_session_update_tolerates_unmodeled_kinds`、`acp_push_jsonrpc_line_decodes_notification_and_result`、
  `acp_push_jsonrpc_line_caches_client_requests`、`acp_push_jsonrpc_line_classifies_error_response`、
  `acp_push_jsonrpc_line_tolerates_and_rejects`);connection 3 个 `#[tokio::test]`
  (`fake_launcher_transport_feeds_decoder`、`read_line_times_out_into_session_lost`——50ms 超时快速返回、
  `read_line_reports_eof`)。新增离线 cassette `tests/agent_acp_cassette.rs` + fixture
  `tests/fixtures/external/acp/full_session.json`(+ README):两 turn(Start text-only+plan+tool_call →
  completion;Continue text+edit tool_call+`session/request_permission`+diff → completion),4 个测试
  (`acp_cassette_regenerate_fixture` env-gated 重生、`acp_cassette_matches_in_code_builder`、
  `acp_cassette_is_secret_free` 脱敏扫描、`acp_cassette_decodes_full_session` 单 decoder 重放断言观测流/
  逐 turn 决策/缓存的 permission 请求);重生走 `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1`,常规跑不覆写。
- **验证序列 1-6 全过**(默认 + `--features external-acp` 两配置):`cargo fmt --all` 通过;
  `cargo clippy --all-targets -- -D warnings`(默认)与 `--features external-acp` 均无告警;
  `cargo test --all --all-targets`(默认)全绿、`--features external-acp` 全绿(lib 含新增 9 个 acp 单测,
  集成新增 4 个 acp cassette 测试,均 <1s);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  默认与 `--features external-acp` 均通过。
- **feature 隔离**:decoder/connection/cassette 全部 `#[cfg(feature = "external-acp")]` 或
  `#![cfg(feature = "external-acp")]` 门控;默认构建既不编译新代码也不拉入 ACP crate,默认全量测试与改动前一致。

### [DONE] M10-3 实现 ACP live session adapter、permission bridge 与 ignored real e2e

**上下文**:

- milestone-5 抽象见 `src/agent/external/adapter.rs`(`ExternalRuntimeAdapter` /
  `ExternalRuntimeSession` / `RuntimeDecisionPoint`)与 `registry.rs`;三个 CLI adapter 的 `*/adapter.rs`
  是照抄模板。ACP 与它们的进程模型不同:ACP 是**单条长驻**双向连接(更接近 Claude Code 的 stream-json 时序,
  而非 Codex/OpenCode 的每-turn 一次性进程)。
- 本任务是 ACP 首次真正驱动 machine 的 host-pausable 臂:`session/request_permission`(agent→client
  request)→ `RuntimeDecisionPoint::PausedForInteraction` → machine 发 `NeedInteraction` → host 应答 →
  `ExternalSessionInput::RespondInteraction` → adapter 把应答回写成 ACP permission response。

**做什么**:

- `AcpAdapter`(该模块**唯一 pub 类型**,impl `ExternalRuntimeAdapter`):
  - `new(config)` 报告本 adapter 实现的能力;`with_probed_capabilities(config, &probed)` 与实现能力**逐位
    取交**(复用 M10-1 的协商映射 + 现有 intersect 模式)。
  - `start`:spawn agent 子进程 → `initialize` 握手(据返回填 `session/new`)→ `session/new` 拿 ACP session
    id(作为 registry key / resume token)→ 发首个 `session/prompt`;读 `session/update` 直到落定一个
    `RuntimeDecisionPoint`。
  - `resume`:仅当协商到 `loadSession` 能力时用 `session/load` 复活,否则 `ResumeUnavailable`。
- `AcpSession`(私有,impl `ExternalRuntimeSession`):单条长驻连接 + 跨全程单调 `seq` 的观测解码(M10-2)。
  `advance(input)`:
  - `Start`/`Continue` → 发 `session/prompt`,读 update 直到 decision。
  - `RespondInteraction`(权限应答)→ 把 host 的 `InteractionResponse` 回写成 ACP
    `session/request_permission` response(allow/deny),继续读 update 直到下一个 decision。权限暂停对应的
    `Interaction` 的 `step_id`/`actor` **绑定宿主** `RunContext.run_id`/请求 `agent_id`,**绝不**取自 runtime
    输出。
  - decision 落定:turn 结束(stopReason)→ `Completed`(带 usage/artifacts,若 crate 暴露);
    收到 `session/request_permission` → `PausedForInteraction`;连接中途断 → `SessionLost`;协议违例 →
    `Protocol`。
  - `fs/read_text_file` / `fs/write_text_file` / `terminal/*`:作为 **client 环境服务**由 adapter 直接对
    `working_dir`/worktree 兑现(受 `ExternalPermissionMode` 约束:如 `Plan` 拒绝写),并汇报为 observation;
    **不**折成 `NeedTool`(首版 `host_tools=false`)。写操作必要时先经 `PausedForInteraction` 审批(由
    permission mode / approval policy 决定)。
  - `shutdown`:发 `session/cancel` + 关连接,超时 forced kill,归类 `ExternalSessionShutdown`。
- 能力(诚实):`streaming`/`permission_bridge`/`graceful_shutdown` = true;`resume` 取决于 `loadSession`
  协商;`artifacts`/`usage` 取决于 crate 是否暴露;`host_tools`/`host_subagents` = false。对声明了 `tools`
  的 `start`/`resume` 请求,以 `UnsupportedCapability{HostTools}` 明确拒绝(与三个 CLI adapter 一致);
  follow-up 的 `RespondToolResults`→`{HostTools}`、`RespondSubagent`→`{HostSubagents}` 明确拒绝。
- IO 经私有 launcher/connection trait 注入:生产用真实 crate + `tokio::process`;单测注入 fake transport
  回放固定 ACP 消息序列并捕获我们发出的请求,**离线**跑通 initialize/prompt/permission/fs/cancel/shutdown 全
  状态机,无需任何 ACP agent binary、无网络。
- 真机 e2e:新增 `#[ignore]` 用例 `tests/external_acp.rs`,通过 `ACP_AGENT_BIN`(+ 可选 `ACP_AGENT_ARGS`)
  或 PATH 发现一个 ACP agent。以本机实测的三个 agent 为准:
  - `claude-agent-acp`(依赖本机 **Claude Code** 配置/登录态,非官方 key);
  - `codex-acp`(依赖本机 **Codex** 配置/登录态,非官方 key);
  - `opencode acp`(OpenCode 自带 ACP)。
  测试**不**从 env 读 API key,而是依赖各 CLI 的配置/登录态;缺失 binary/登录即带清晰非-secret 信息
  **跳过**(退出为绿)。**因本机三个 agent 都不用缺省配置**,e2e 必须能经 `AcpConfig` 指向实际配置——
  提供覆盖入口(如 `ACP_CODEX_HOME` / `ACP_OPENCODE_CONFIG` / `ACP_CLAUDE_SETTINGS` 之类测试 env,映射到
  §M10-1 的 `AcpConfig.env`/`args`);未设时走继承(缺省配置)。然后在临时 git worktree 里驱动
  probe→start→advance(自动 approve 权限暂停)→completion→graceful shutdown,断言观测流为多步
  (SessionStarted + ≥1 文本 + SessionCompleted),并断言 worktree 隔离(产物落在 worktree 内、不泄漏到启动
  checkout)。
  运行命令与实跑过的 agent 写进完成记录。

**验证条件**:

- adapter fake-transport 单测覆盖:start→completion、start→request_permission→RespondInteraction(allow/
  deny)→completion、fs 写经审批后兑现、连接断→SessionLost、协议违例→Protocol、shutdown 分类、声明 tools →
  `UnsupportedCapability{HostTools}`。
- 断言 permission 暂停产生的 `Interaction` 的 step_id/actor 来自宿主而非 runtime 输出。
- 一个经真实 `ExternalAgentMachine::drain` + registry-backed handler 的离线 drain 测试:
  Start → PausedForInteraction → NeedInteraction → RespondInteraction → Completed(证明 ACP 首次真正驱动
  host-pausable 臂,且无需改 machine/driver)。
- 聚焦测试:
  - `cargo test -p agent-lib --features external-acp acp_adapter`
  - `cargo test --features external-acp --test external_acp -- --ignored`(仅在具备 ACP agent 时实跑,
    否则清晰跳过)
- 完整验证序列 1-6 全过(默认 + `--features external-acp`)。

**完成记录**：

- **新增唯一 pub 模块类型 `AcpAdapter`**(`src/agent/external/acp/adapter.rs`,~1080 行含单测):impl
  `ExternalRuntimeAdapter`。`new(config)` 报告本 adapter 实现能力;`with_probed_capabilities(config, &probed)`
  与实现能力逐位取交(复用 `intersect_capabilities`,保留左侧 ACP runtime label);`with_launcher(config,
  Arc<dyn AcpLauncher>)` 供离线注入 fake transport。私有 `AcpSession` impl `ExternalRuntimeSession`,持单条长驻
  连接 + 跨全程单调 `seq`(承 M10-2 `AcpStreamDecoder`)。`acp/mod.rs` `mod adapter;` 挂载并 re-export
  `AcpAdapter`;`decoder` re-export 补 `AcpPermissionOption` / `AcpPermissionOptionKind`;`external/mod.rs`
  的 `#[cfg(feature="external-acp")]` 块同步补齐,故 `agent_lib::agent::external::AcpAdapter` 等可用。**未导出任何
  `agent-client-protocol*` raw 类型**。
- **握手 / 会话**:`start` → `initialize`(advertise `clientCapabilities.fs.{readTextFile,writeTextFile}=true`、
  `terminal=false`、`protocolVersion=ACP_WIRE_VERSION(=1)`)→ 据返回 `negotiated_from_initialize` 记录
  `loadSession` → `session/new`(cwd + 空 mcpServers)取 ACP session id(作 registry key / resume token)。
  `resume` 仅当协商到 `loadSession` 时用 `session/load` 复活,否则 `ResumeUnavailable`。`advance`:`Start`/
  `Continue` → `session/prompt` 读 `session/update` 直到落定 decision;`RespondInteraction` → 校验后回写 ACP
  permission response 再续读;turn 结束(`stopReason`)→ `Completed`;`session/request_permission` →
  `PausedForInteraction`;连接断 → `SessionLost`(带 session ref);协议违例 → `Protocol`。`shutdown` 发
  `session/cancel` + `close(grace)`,超时 forced kill,归类 `ExternalSessionShutdown`。
- **permission bridge(ACP 首次真正驱动 host-pausable 臂)**:`session/request_permission` → 构造 host 绑定
  `Interaction`——`step_id = StepId::new(RunContext.run_id)`、`actor = request.agent_id`、
  `PermissionCategory::Other`、`PermissionRisk::Medium`,**绝不**取自 runtime 输出;应答先经
  `Interaction::accepts_response`(并核对 `action_id == pending.request_id`)校验后,才用
  `permission_outcome` 把 `PermissionDecision` 映射为 ACP `outcome`:Approve→选 allow 选项(优先 once 后
  always)、Deny→选 reject 选项、Cancel/无匹配→`{"outcome":"cancelled"}`;response id 经 `json_rpc_id_value`
  还原为数值以精确关联。
- **fs/terminal 作 client 环境服务**(不折成 `NeedTool`,首版 `host_tools=false`):`fs/write_text_file` 在
  `Plan` 模式拒绝(JSON-RPC error),否则建父目录 + 写 + `decoder.note_file_patch()`(汇报为 `FilePatch` 观测)+
  回 `{}`;`fs/read_text_file` 读(可选 `line`/`limit` 窗口)+ 回 `{content}`;`terminal/*` 以 `-32601` 拒绝
  (client advertised `terminal:false`)。错误诊断只用 `error.kind()`,永不夹带文件内容/凭据。
- **能力(诚实)**:`streaming` / `permission_bridge` / `graceful_shutdown` = true;`resume` 静态乐观 true 但
  runtime `resume()` 依 `loadSession` 协商回落 `ResumeUnavailable`;`artifacts` / `usage`(unstable) / `reconfigure`
  / `host_tools` / `host_subagents` = false。声明 `tools` 的 `start`/`resume` 以
  `UnsupportedCapability{HostTools}` 拒绝;follow-up `RespondToolResults`→`{HostTools}`、
  `RespondSubagent`→`{HostSubagents}` 明确拒绝。**相对三个 CLI adapter 的关键差异**:Claude Code /
  Codex / OpenCode 的 `permission_bridge=false`(自治或每-turn 一次性进程,不把 gated action 交回宿主),ACP
  是首个 `permission_bridge=true` 的 adapter,`resume` 亦由 `loadSession` 协商而非固定。
- **测试(全绿,均 <1s)**:adapter inline 11 个 fake-transport 单测(`acp_adapter_*`):
  start→permission→completion(断言 `action_id="100"`、`Interaction.step_id==StepId(run_id)`、permission
  `actor==agent_id`、≥3 观测、写出 initialize/session/new/session/prompt + 选 allow 的 permission response、
  summary "working done"、sink `seq` 单调)、deny 选 reject、fs 写经审批后落盘 + `FilePatch`、Plan 模式拒写、
  连接断→SessionLost、协议违例→Protocol、shutdown 分类(注入 `ForcedKill` + 断言写出 `session/cancel`)、
  声明 tools→`UnsupportedCapability{HostTools}`、tool/subagent results 拒绝、resume 需 loadSession、能力诚实、
  outcome 映射、json-rpc id 数值保真、line window。新增 `tests/agent_acp_adapter_drain.rs`:registry-backed
  handler + `AcpAdapter::with_launcher` + fake transport,经真实 `ExternalAgentMachine::drain` 跑通
  Start→PausedForInteraction→NeedInteraction(`ScriptedInteractionHandler([Approve])`)→RespondInteraction→
  Completed(断言 `Done`、1 次 interaction、committed 1 turn、写出握手 + 选 allow),**证明 ACP 首次真正驱动
  host-pausable 臂且无需改 machine/driver/state**。
- **真机 e2e**:新增 `#[ignore]` `tests/external_acp.rs`:经 `ACP_AGENT_BIN`(+ `ACP_AGENT_ARGS`)或 PATH 发现
  `opencode acp` / `claude-agent-acp` / `codex-acp`;测试 env 覆盖 `ACP_CODEX_HOME`→`CODEX_HOME`、
  `ACP_OPENCODE_CONFIG`→`OPENCODE_CONFIG`、`ACP_CLAUDE_SETTINGS`→`--settings <file>`(映射到 `AcpConfig`
  env/args,均不读 API key、不打印);临时 git worktree 驱动 start→advance(自动 approve 权限暂停)→completion→
  graceful shutdown,断言多步观测(SessionStarted + ≥1 文本 + SessionCompleted,≥3 事件)与 worktree 隔离
  (产物落 worktree 内、不泄漏到启动 checkout)。**本机 `opencode acp`(自带 ACP)实跑通过**:19 条观测(15 文本)、
  创建 `READY.txt`、graceful 关闭、13.9s;缺 binary/登录即清晰非-secret 跳过退绿。运行命令:
  `cargo test --features external-acp --test external_acp -- --ignored --nocapture`。
- **验证序列 1-6 全过**(默认 + `--features external-acp` 两配置):`cargo fmt --all` 通过;
  `cargo clippy --all-targets -- -D warnings`(默认)、`--features external-acp`、
  `--features "external-claude-code external-codex external-opencode"` 均无告警;
  `cargo test --all --all-targets`(默认)全绿、`--features external-acp` 全绿(lib 新增 11 个 acp adapter 单测,
  集成新增 drain 测试 + ignored e2e);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 默认与
  `--features external-acp` 均通过。聚焦:`cargo test -p agent-lib --features external-acp acp_adapter` 11 passed。
- **feature 隔离**:adapter/drain/e2e 全 `#[cfg(feature="external-acp")]` / `#![cfg(...)]` 门控;
  `cargo tree -e normal -i agent-client-protocol`(默认)报 `did not match any packages`(证明不拉入),
  `--features external-acp` 下为 `agent-client-protocol v1.2.0 └── agent-lib`。machine/driver/`ExternalAgentState`
  无任何 ACP 特判改动。



**上下文**:

M10 结束后,`agent-lib` 应能以一个 ACP client 对接任意 ACP agent,并首次真正驱动 permission bridge。
阶段 review 确认它没有把 crate raw 类型泄漏为稳定 API、没有改 machine/driver、能力诚实、脱敏到位。

**做什么**:

- 核对 `src/agent/external/acp/`:
  - 官方 crate 的协议类型未出现在 `agent-lib` 稳定 public API(只有 `AcpAdapter` / `AcpConfig` 等中立类型
    对外);观测只用现有 `ExternalAgentEvent` 词汇表。
  - `ExternalAgentMachine` / `drive.rs` / `ExternalAgentState` 无任何 ACP 特判改动。
  - `permission_bridge=true` 的路径确实经 `PausedForInteraction`→`NeedInteraction`→`RespondInteraction`,
    interaction 的 step_id/actor 绑定宿主;权限应答经 `Interaction::accepts_response` 校验后才回写 ACP
    (与 M3-2 一致)。
  - capability 诚实:未协商到的位不假装 true;声明 tools 被明确拒绝而非静默忽略。
  - secret 脱敏:config `Debug`、错误诊断、cassette fixture 均无 env secret / prompt / 文件内容 / 凭据。
- 核对 feature 隔离:默认构建不含 ACP 依赖(`cargo tree` 或 `cargo build` 确认);ACP 代码全在
  `#[cfg(feature = "external-acp")]` 之后。
- 更新 `docs/managed-external-agent.md`(新增 ACP adapter 章节,仿 §12-14 的实现状态记法)与
  `docs/capability-matrix.md`(记录 ACP 实测/协商能力,不声称未验证支持)。

**验证条件**:

- 默认完整验证序列 1-6 全过;`--features external-acp` 下 clippy/test/doc 全过。
- `cargo build`(无 feature)不拉入 `agent-client-protocol*`(在完成记录中给出 `cargo tree` 证据)。
- 完成记录中给出:ACP adapter 相对三个 CLI adapter 的能力差异摘要(尤其 `permission_bridge`)、
  ACP wire 版本、剩余 runtime-dependent 限制(host_tools 经 client MCP 等后续项)。

---

## 交接任务

### [TODO] H-1 归档当前计划并为 facade API 生成新的 PLAN/TODO

**上下文**:

- 当前 `PLAN.md` 和本 `TODO.md` 记录的是 Managed External Agent 工作线(含 M1-M9 的核心/CLI adapter 与
  M10 的 ACP adapter)。下一轮工作要切换到 [`docs/facade-api.md`](docs/facade-api.md) 对应的 facade API 落地。
- facade API 与本工作线有承接关系:facade 的 external delegate / `ManagedExternalAgent` /
  `ExternalRunMode` 能力分级(见 `docs/facade-api.md` §11)应能表达 M10 落地的 ACP 后端与 permission bridge
  档位;编写新计划时须把这一依赖显式承接过来。
- 执行本任务时,必须先保留当前计划/任务单的历史版本,再重写仓库根目录的 `PLAN.md` 和 `TODO.md`。
- 新 `TODO.md` 要给后续 coding agent 直接执行,不能只写高层目标。

**做什么**:

- 将当前根目录 `PLAN.md` 和 `TODO.md` 归档到合适的历史位置:
  - 如果仓库已有计划归档目录或命名惯例,沿用现有惯例。
  - 如果没有现有惯例,新增清晰的归档路径,例如 `docs/archive/`,并使用能说明主题和日期的文件名。
  - 归档内容必须是切换前的完整 `PLAN.md` 和完整 `TODO.md`,不得只保留摘要。
- 阅读 [`docs/facade-api.md`](docs/facade-api.md),为该文档写一份新的落地计划到根目录 `PLAN.md`:
  - 计划应按 milestone 展开,说明每个阶段的目标、关键实现点、风险和验证方式。
  - 计划应只承诺 `docs/facade-api.md` 中有依据的内容；若发现文档缺口,在计划中列为待确认风险。
- 重写根目录 `TODO.md`,为 facade API 落地生成新的任务单:
  - 任务必须按实现顺序排列,并使用编号,例如 `M1-1` 表示 milestone 1 的第一个任务,依此类推。
  - 每个任务标题必须保留一个 `[TODO]` 标记,让 coding agent 知道该任务尚未完成。
  - 每个任务必须包含足够的细节和上下文,让 coding agent 实现时不需要反复搜索代码库。
  - 每个任务必须定义完整的验证条件,包含必要的聚焦测试、格式/静态检查和相关文档检查。
  - 每个阶段结尾必须加入一个单独的 review 任务,用于检查该阶段的正确性、完整性和文档一致性。
  - 新任务单开头应说明通用执行规则:一次只执行首个标题带 `[TODO]` 的任务,完成后改为 `[DONE]`,并补充完成记录。

**验证条件**:

- 归档后的旧 `PLAN.md` 和旧 `TODO.md` 文件存在,且内容完整可读。
- 新根目录 `PLAN.md` 明确以 [`docs/facade-api.md`](docs/facade-api.md) 为依据,并包含 milestone、风险和验证策略。
- 新根目录 `TODO.md` 中的任务按实现顺序编号,所有未完成任务标题都包含 `[TODO]`。
- 新根目录 `TODO.md` 的每个 milestone 末尾都有独立 review 任务。
- 新根目录 `TODO.md` 的每个任务都有上下文、做什么、验证条件三类信息或等价结构。
- `git diff --check` 通过。
