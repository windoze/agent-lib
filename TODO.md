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
