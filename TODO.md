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

### [TODO] M2-2 将 `PausedForToolCalls` 折成 `NeedTool` batch

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

### [TODO] M2-3 收齐 `NeedTool` 结果并回灌 `RespondToolResults`

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

### [TODO] M2-4 Review：external tool phase 正确性检查

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

---

## Milestone 3 — subagent / interaction parity

目标:external runtime 能通过 machine 触发 host subagent,并让 runtime permission/question/choice 走标准
`NeedInteraction`。

### [TODO] M3-1 实现 `PausedForSubagent` -> `NeedSubagent` -> `RespondSubagent`

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

### [TODO] M3-2 完善 runtime permission/question/choice 到 `NeedInteraction` 的映射

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

### [TODO] M3-3 支持 external runtime 的 `spawn_agent` tool bridge 特判

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

### [TODO] M3-4 Review：interaction/subagent parity 正确性检查

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

---

## Milestone 4 — streaming live sink、capability model、session policy

目标:把流式旁路和 runtime 能力差异做成可测试、可降级的公共接口。

### [TODO] M4-1 将 `ExternalEventSink` 升级为 sequenced live sink

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

### [TODO] M4-2 新增 `ExternalRuntimeCapabilities` 与 unsupported capability 错误

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

### [TODO] M4-3 扩展 `ExternalSessionPolicy` / `ExternalAgentSpec` 支持 managed mode 配置

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

### [TODO] M4-4 Review：stream/capability/policy 完整性检查

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

---

## Milestone 5 — runtime adapter abstraction 与 scripted/cassette handler

目标:在不接真实 CLI 的情况下,先把 adapter/session registry/handler 分层建起来,用 scripted runtime 覆盖
完整 managed loop。

### [TODO] M5-1 定义 `ExternalRuntimeAdapter` / `ExternalRuntimeSession` / `ExternalSessionRegistry`

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

### [TODO] M5-2 实现 scripted external runtime adapter

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

### [TODO] M5-3 增加 cassette replay 层用于 runtime parser 回归

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

### [TODO] M5-4 Review：runtime abstraction 与离线 e2e 完整性检查

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

---

## Milestone 6 — Claude Code managed adapter

目标:实现 feature-gated Claude Code adapter,支持 stream-json 解码、permission bridge、tool/subagent bridge
能力探测；真实 e2e ignored。

### [TODO] M6-1 增加 Claude Code capability probe 与启动配置

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

### [TODO] M6-2 实现 Claude Code stream decoder cassette 测试

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

### [TODO] M6-3 实现 Claude Code session adapter 与 ignored real e2e

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

### [TODO] M6-4 Review：Claude Code adapter 正确性检查

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

### [TODO] M7-1 增加 Codex capability probe 与启动配置

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

### [TODO] M7-2 实现 Codex stream decoder cassette 测试

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

### [TODO] M7-3 实现 Codex session adapter 与 ignored real e2e

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

### [TODO] M7-4 Review：Codex adapter 正确性检查

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

---

## Milestone 8 — OpenCode managed adapter

目标:实现 feature-gated OpenCode adapter,按实际 CLI/API 能力接入 streaming、permission、tool bridge,并用
capability model 明确降级。

### [TODO] M8-1 增加 OpenCode capability probe 与启动配置

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

### [TODO] M8-2 实现 OpenCode stream decoder cassette 测试

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

### [TODO] M8-3 实现 OpenCode session adapter 与 ignored real e2e

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

### [TODO] M8-4 Review：OpenCode adapter 正确性检查

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

---

## Milestone 9 — worktree/budget/reconfig/docs/real mixed e2e hardening

目标:把 managed external agent 接入调度、worktree、budget、docs 和真实多 agent e2e。

### [TODO] M9-1 实现 worktree isolation 管理与 cleanup 标记

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

### [TODO] M9-2 接入 usage/cost budget charging

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

### [TODO] M9-3 支持 turn-boundary external reconfig

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

### [TODO] M9-4 增加真实 DeepSeek 父协调器 + Claude Code/Codex 子 agent ignored e2e

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

### [TODO] M9-5 更新 docs/examples/capability matrix

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

### [TODO] M9-6 Review：Managed External Agent 总体验收

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

---

## 交接任务

### [TODO] H-1 归档当前计划并为 facade API 生成新的 PLAN/TODO

**上下文**:

- 当前 `PLAN.md` 和本 `TODO.md` 记录的是 Managed External Agent 工作线。下一轮工作要切换到
  [`docs/facade-api.md`](docs/facade-api.md) 对应的 facade API 落地。
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
