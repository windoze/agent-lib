# TODO：External Agent 接入任务单

> 依据 [`PLAN.md`](PLAN.md) 与 [`docs/external-agent.md`](docs/external-agent.md)。任务按实现顺序编号
> (`M<里程碑>-<序号>`);coding agent 每次只执行首个标题带 `[TODO]` 的任务,完成后把该标题的 `[TODO]`
> 改为 `[DONE]`,并在任务末尾补充「完成记录」。
>
> 上一轮任务单已归档在
> [`docs/archive/2026-07-16-complex-tests/`](docs/archive/2026-07-16-complex-tests/)。

通用约束:不得改变 `agent-lib` 既有运行时语义(现有 `NeedLlm/NeedTool/NeedInteraction/NeedSubagent/
NeedReconfigRegistry` 路径行为保持不变,新增 external 路径为增量);不得把真实 CLI/SDK 的 wire/私有 JSON
作为稳定协议进入核心库;外部 runtime 交互一律用 scripted handler 或 cassette 替身,不依赖真实 sleep、网络、
credentials 或未纳管进程作为默认测试条件;新增公开 API 必须带 rustdoc;每个测试用例应在 1 分钟内完成。

**默认完整验证序列**(除非任务另行放宽):

1. `cargo fmt --all -- --check`
2. 聚焦测试(任务内给出精确过滤名)
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

---

## Milestone 1 — 低保真验证 spike(Phase 0)

> 目的:在投入正式 DTO/machine 之前,用现有 `LlmHandler` 包一层外部 CLI,实测启动方式、流式 decoder、
> 取消行为与成本量级。产出是**结论**,不是稳定 API;spike 代码放在 `probes/` 或 `examples/`,不进 `src/`。

### [DONE] M1-1 搭建外部 CLI 低保真 spike

**前置依赖**:无。

**上下文**:

设计文档 §3.1、§13 Phase 0 允许一个低保真 adapter 把外部 coding-agent 的最终文本 fold 成
`RequirementResult::Llm(Response)`,仅用于快速试验。现有 `LlmHandler` trait 在
`src/agent/drive.rs`:

```rust
#[async_trait]
pub trait LlmHandler: Send + Sync {
    async fn fulfill(&self, request: &ChatRequest, mode: LlmStepMode, ctx: &RunContext) -> RequirementResult;
}
```

`RequirementResult::Llm(Result<Response, ClientError>)` 定义在 `src/agent/requirement.rs`。仓库已有
`probes/` 与 `examples/` 目录可放一次性验证代码。

**做什么**:

- 在 `probes/`(或 `examples/`)新增一个独立 spike:实现一个 `LlmHandler`,内部以子进程方式调用一个
  外部 coding-agent CLI(可用一个 stub/echo 脚本占位,不要求接真实 Claude Code/Codex)。
- 把子进程的流式 stdout 解码为文本增量,fold 成一个 `Response`,通过 `RequirementResult::Llm(Ok(..))`
  返回。
- 覆盖三条行为:正常启动并返回文本、流式增量读取、进程取消/kill(观察 `RunContext::is_cancelled` 触发
  后如何终止子进程)。
- spike 只用于观察,不接入 `src/`,不作为稳定 API。

**验证条件**:

- spike 可通过 `cargo run --example <name>`(或 `probes` 约定入口)在本地跑通三条行为,用 stub 脚本即可,
  不需要真实外部 runtime。
- `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings` 通过。
- 不修改 `src/` 下任何文件;`git diff --check` 干净。

**完成记录**:

- 新增 `examples/external_cli_spike.rs`(spike 入口 `cargo run --example external_cli_spike`)。
  选 `examples/` 而非 `probes/`,因为 `.gitignore` 忽略 `/probes`(spike 需可提交);examples 属
  root package,会被 `cargo clippy --all-targets` 编译校验。全程只用 agent-lib 公开 API,未改 `src/`。
- `ExternalCliLlmHandler { program, args }` 实现 `agent_lib::agent::LlmHandler`:以 `tokio::process::
  Command` 拉起子进程(`kill_on_drop(true)`、stdout piped、stdin/stderr null),把请求 prompt 经
  `SPIKE_PROMPT` env 透传;stdout 由独立 reader task 逐行读入 `mpsc`,主循环用 `tokio::select!` 将行
  投递(`recv()` cancel-safe)与 `sleep(10ms)` 轮询 `RunContext::is_cancelled` 竞速;EOF 后把累计文本
  fold 成 `Response`(StopReason `end_turn`、Usage 以词数粗估),经 `RequirementResult::Llm(Ok(..))`
  返回;取消时 `child.start_kill()` + `wait()` 并返回 `RequirementResult::Llm(Err(ClientError::Other))`。
- 外部 CLI 用自包含 `sh -c` stub 脚本占位(按位置参数打印 N 个 chunk、每个 `sleep d`),不接真实
  Claude Code/Codex,无网络/credentials。
- 三条行为均可复现(`cargo run --example external_cli_spike` 实测):
  1. 正常启动 + 返回文本(NonStreaming):读到 EOF,folded 文本含 echo 行 + 3 个 chunk;
  2. 流式增量(Streaming):逐行打印 `[stream +N]` 增量后再 fold;
  3. 取消/kill:长跑 stub(1000 chunk × 50ms),控制任务在 ~150ms 后 `cancellation().cancel()`,
     handler 侦测到取消后 kill 子进程并返回 `Err(... killed external CLI after 4 streamed chunk(s))`。
- 验证结果(全部通过):`cargo run --example external_cli_spike` 三条行为跑通;
  `cargo fmt --all -- --check` 无差异;`cargo clippy --all-targets -- -D warnings` 无告警;
  `git status` 显示 `src/` 无改动;`git diff --check` 干净。(本任务仅要求 fmt+clippy+run,未跑全量测试套件。)

### [DONE] M1-2 记录 spike 结论

**前置依赖**:M1-1。

**上下文**:

设计文档 §14 列有若干未定问题(session resume 归一化、black-box「完成」定义、取消后副作用),spike 的
实测结果应回灌到设计假设,指导 Milestone 2+ 的取舍。

**做什么**:

- 在 `docs/external-agent.md` 末尾(§15 之后)新增一个「附录 A:Phase 0 spike 结论」小节,记录:启动方式、
  流 decoder 形态、取消行为、成本量级、以及对后续 DTO/handler 设计的影响。
- 若 spike 暴露了与文档不一致的假设,在对应章节以脚注或「(spike 修正)」标注,不删除原设计文本。

**验证条件**:

- `docs/external-agent.md` 新增附录,结论明确、可操作(至少列出 3 条对 Milestone 2 的具体影响)。
- Markdown 渲染无断链;`git diff --check` 干净。

**完成记录**:

- 在 `docs/external-agent.md` §15 之后新增「附录 A:Phase 0 spike 结论」,依据 M1-1 的
  `examples/external_cli_spike.rs` 实测,分四类记录结论:
  - **A.1 启动方式**:`tokio::process::Command`(stdin=null / stdout=piped / stderr=null /
    `kill_on_drop`)、prompt 经 env 透传只是权宜;结论——`ExternalSessionRequest::Start` 应把 prompt
    当纯数据、不焊死投递通道。
  - **A.2 流 decoder 形态**:reader task + 有界 `mpsc` + `tokio::select!` 竞速验证 §5.5 后台缓冲模型,
    但逐行纯文本解码不足;结论——`ExternalAgentEvent` 必须是结构化枚举,decoder/进程归 runtime handle 长存。
  - **A.3 取消行为**:10ms 轮询 `is_cancelled` → `start_kill()`+`wait()`,证实 §6.4 never-resume;
    结论——清理走 handler/runtime handle 的 `Drop`,结果 DTO 需显式 shutdown disposition。
  - **A.4 成本量级**:fold usage 为词数粗估、黑盒文本拿不到真实 token/成本;结论——`ExternalSessionResult`
    需独立 usage/cost 字段(runtime 自报或标记未知)。
- **对 Milestone 2 的具体影响**:附录 A.5 列出 5 条可操作项(覆盖 M2-1 DTO 字段、M2-2 三态 + observations、
  M2-3 长存 runtime handle + Drop 清理、M2-4 scripted handler 禁真实进程/sleep、§14 未定问题回填),
  超过"至少 3 条"要求;A.6 给出 Milestone 2 go/no-go(Go,且核心库不引入真实进程依赖)。
- 对 spike 修正的两处假设以「(spike 修正,见附录 A)」引用块标注,未删原文:§3.1(fold 有损,
  无 per-event usage / 无 permission 表达)、§5.5(逐行文本解码不足,event 须结构化、reader 归 runtime handle)。
- 验证:纯文档改动(仅 `docs/external-agent.md`,`src/` 无变更),`git diff --check` 干净、无断链;
  按 PROMPT「仅文档改动可复用上次全量绿结果」跳过全量 `cargo test`(自上次绿以来无编译产物变化)。

### [DONE] M1-3 Milestone 1 Review

**前置依赖**:M1-1、M1-2。

**上下文**:确认 spike 阶段目标达成且未污染核心库。

**做什么**:

- 核对 M1-1/M1-2 产出:spike 仅在 `probes/`/`examples/`,`src/` 无改动;附录结论完整。
- 确认三条行为(启动/流/取消)均有可复现入口。
- 记录进入 Milestone 2 的前置结论(是否需要黑盒 fallback、取消清理的最小要求)。

**验证条件**:

- `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 全绿。
- Review 结论写入本任务「完成记录」,明确 Milestone 2 的 go/no-go 与调整项。

**完成记录**:

- **产出核对(spike 未污染核心库)**:M1-1 提交 `320dcb4` 仅改
  `examples/external_cli_spike.rs` + `TODO.md` + `memory/claude_plan.md`;M1-2 提交 `4a7bde4` 仅改
  `docs/external-agent.md` + `TODO.md` + `memory/claude_plan.md`。`src/` 自 Milestone 1 起零改动
  (最后一次 `src/` 变更是 `d8c2d9a` README 重写,早于 M1)。spike 落在 `examples/`(root package,
  受 `cargo clippy --all-targets` / `cargo test --all --all-targets` 编译校验),符合「不进 `src/`、
  不作稳定 API」的约束。`git diff --check` 干净。
- **附录结论完整**:`docs/external-agent.md` §750「附录 A:Phase 0 spike 结论」齐全
  (A.1 启动/A.2 decoder/A.3 取消/A.4 成本 + A.5 对 M2 的 5 条可操作影响 + A.6 go/no-go);§3.1(第 83 行)、
  §5.5(第 402 行)各以「(spike 修正,见附录 A)」引用块标注,未删原设计文本。
- **三条行为可复现**:统一入口 `cargo run --example external_cli_spike`,本轮实测三条均跑通——
  1. 正常启动 + 返回文本(NonStreaming):EOF 后 fold 出 16 output token(echo 行 + 3 chunk);
  2. 流式增量(Streaming):逐行 `[stream +N]` 增量后再 fold;
  3. 取消/kill:长跑 stub 于 ~150ms `cancel()`,handler 侦测取消后 `start_kill()`+`wait()`,
     返回 `Err(... killed external CLI after 4 streamed chunk(s))`。
- **进入 Milestone 2 的前置结论**:
  - *黑盒 fallback 取舍*:低保真 fold(黑盒文本→`Response`)已验证可跑但**有损**(无 per-event
    usage、无 permission 表达、无结构化 event)。故 Milestone 2 起 **不把黑盒 fold 作为主路径**:
    `ExternalAgentEvent` 必须是结构化枚举(附录 A.2),黑盒文本仅作降级兜底,且降级路径需在 DTO 上显式可辨。
  - *取消清理最小要求*:cancel 走 never-resume(§6.4 已由 spike 证实)。最小要求——进程/decoder 句柄
    由 runtime handle 长存并通过 `Drop`(`kill_on_drop` + 显式 `start_kill()`+`wait()`)清理,
    结果 DTO 必须携带显式 shutdown disposition(区分正常完成 / 取消 kill / 失败),不得静默丢弃副作用。
  - *成本量级*:黑盒拿不到真实 token/成本,`ExternalAgentOutput` 需独立 `usage`/`cost_micros` 字段
    (runtime 自报或标记未知),不得用词数估算冒充真实成本。
- **验证结果(本轮全绿)**:`cargo fmt --all -- --check` 无差异;`cargo clippy --all-targets -- -D warnings`
  无告警;`cargo test --all --all-targets` 全绿(lib 423 + testkit 131 + 各集成/doc/replay 二进制,
  0 failed;仅 4 个 credential-gated 集成测试保持 ignored,符合预期);`cargo run --example external_cli_spike`
  三条行为跑通。本轮**真跑了全量套件**——M1-1 新增了 example 但当时只跑 fmt+clippy+run、M1-2 纯文档,
  故 example 到本任务才首次经全量套件校验,结果绿。
- **Milestone 2 go/no-go**:**Go**。进入 M2-1 定义 external session DTO,核心库不引入真实进程/网络依赖,
  外部 runtime 交互一律 scripted/cassette 替身。

---

## Milestone 2 — External session DTO 与 handler(Phase 1)

### [DONE] M2-1 定义 external session DTO 模块

**前置依赖**:M1-3。

**上下文**:

设计文档 §5 定义了 external session effect 的 Request / Result / Event,§5.4 定义 `ExternalAgentError`,
§4.2 定义 `ExternalSessionRef`。这些是纯数据类型,应可 `serde` 序列化(与 `RequirementKind` 等一致,
见 `src/agent/requirement.rs` 的 `#[derive(Serialize, Deserialize)]` 风格)。已有可复用类型:`AgentId`
(`src/agent/id.rs`)、`WorktreeRef`/`ToolSetRef`(`src/agent/spec.rs`)、`Tool`/`ToolStatus`
(`src/agent/tool.rs`)、`Usage`(`src/client`)、`Interaction`/`InteractionResponse`
(`src/agent/interaction.rs`)。

**做什么**:

- 新建模块 `src/agent/external/mod.rs`(并在 `src/agent/mod.rs` `pub mod external;` + 重导出),定义:
  - `ExternalRuntimeKind { ClaudeCode, Codex, OpenCode, Custom(String) }`。
  - `ExternalSessionRef { runtime, session_id: Option<String>, transcript_ref: Option<String>,
    resume_token: Option<String>, last_event_seq: Option<u64> }`。
  - `ExternalSessionPolicy { permission_mode: ExternalPermissionMode, isolation: WorktreeIsolation,
    max_turns: Option<u32>, stream_events: ExternalStreamPolicy }`,以及 `ExternalPermissionMode`、
    `WorktreeIsolation`(至少 `Shared` / `PerAgentWorktree` / `EphemeralGitWorktree`)、
    `ExternalStreamPolicy` 三个枚举。
  - `ExternalSessionInput { Start { prompt }, Continue { message }, RespondInteraction { action_id, response: InteractionResponse }, Shutdown }`。
  - `ExternalSessionRequest { agent_id, runtime, worktree, session: Option<ExternalSessionRef>, input, tools: Vec<Tool>, policy }`。
  - `ExternalAgentEvent`(§5.3 全部变体)、`ExternalAgentOutput { summary, artifacts: Vec<ExternalArtifactRef>, usage: Option<Usage>, cost_micros: Option<u64> }`、`ExternalArtifactRef`。
  - `ExternalSessionResult { Completed { session, output, observations }, PausedForInteraction { session, request: Interaction, observations }, Failed { session: Option<..>, error: ExternalAgentError, observations } }`。
  - `ExternalAgentError`(§5.4 全部变体)。
- 为所有类型派生 `Clone, Debug, PartialEq, Serialize, Deserialize`(含 `Eq` 视字段而定),给公开项写 rustdoc。
- 本任务只定义数据类型,**不**接入 requirement / handler(留给 M2-2/M2-3)。

**验证条件**:

- 新增单测 `external_dto_roundtrips`:对 `ExternalSessionRequest` 与三种 `ExternalSessionResult` 做
  serde round-trip,断言相等。过滤名:`cargo test --lib external_dto_roundtrips`。
- 完整验证序列全绿。

**完成记录**:

- 新建 `src/agent/external/mod.rs`(纯数据 DTO 模块),在 `src/agent/mod.rs` 加 `pub mod external;` 并重导出
  全部公开类型(`ExternalRuntimeKind` / `ExternalSessionRef` / `ExternalPermissionMode` / `WorktreeIsolation` /
  `ExternalStreamPolicy` / `ExternalSessionPolicy` / `ExternalSessionInput` / `ExternalSessionRequest` /
  `ExternalAgentEvent` / `ExternalArtifactKind` / `ExternalArtifactRef` / `ExternalAgentOutput` /
  `ExternalSessionResult` / `ExternalAgentError`)。复用类型来源核对:`AgentId`(`agent::id`,Copy)、
  `WorktreeRef`(`agent::spec`)、`Tool`/`ToolStatus`(`model::tool`)、`Usage`(`model::usage`,均 `Eq`)、
  `Interaction`/`InteractionResponse`(`agent::interaction`)。
- 类型均按 §4.2/§5.1/§5.2/§5.3/§5.4 落地,全部派生 `Clone, Debug, PartialEq, Eq, Serialize, Deserialize`
  (所有字段皆 `Eq`,故统一带 `Eq`);枚举用 `#[serde(rename_all = "snake_case")]`,与 `RequirementKind` 风格
  一致;`Option`/`Vec` 字段加 `#[serde(default, skip_serializing_if = ...)]` 保证 round-trip 稳定。
  `ExternalAgentError` 额外派生 `thiserror::Error`(与仓库其它错误枚举一致,提供 `Display`),不影响 serde。
- 设计未细化处的补全(不偏离规格):`ExternalArtifactRef { kind: ExternalArtifactKind, summary, path, reference }`
  以结构化 `kind`(Patch/Diff/TestResult/File/Other)承载 §11 的 patch/diff/test-result 记录需求,内容体
  只放 `reference` 不内联;`ExternalPermissionMode { Prompt/AcceptEdits/Plan/BypassPermissions }` 与
  `ExternalStreamPolicy { Buffered/Streaming/Disabled }` 为 provider-neutral 策略提示,均写明「外部输出按不可信
  处理、不得据此放宽护栏」(§10)、「usage/cost 缺失时留 None 不冒充」(§5.4/成本量级结论)。
- **本任务只定义数据类型**,未接入 requirement/handler(留给 M2-2/M2-3),`RequirementKind`/`RequirementResult`
  等既有运行时枚举未改,既有语义零回归。
- 新增单测:`external_dto_roundtrips`(`ExternalSessionRequest` + 三种 `ExternalSessionResult` serde round-trip
  断言相等)、`external_session_result_variants_serialize_snake_case`(变体外标签为 snake_case)。
- **验证结果(完整序列全绿)**:`cargo fmt --all -- --check` 无差异;`cargo test --lib external_dto_roundtrips`
  1 passed;`cargo clippy --all-targets -- -D warnings` 无告警;`cargo test --all --all-targets` 全绿
  (lib 425 passed〔+2 新测〕、testkit 131 passed、各集成/doc/replay 二进制 0 failed;仅既有 credential-gated
  集成测试保持 ignored,与基线一致);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;
  `git diff --check` 干净。

### [DONE] M2-2 新增 `NeedExternalSession` requirement 与结果变体

**前置依赖**:M2-1。

**上下文**:

`src/agent/requirement.rs` 中:`RequirementKind`(约 362 行)当前有 `NeedLlm/NeedTool/NeedInteraction/
NeedSubagent/NeedReconfigRegistry`;`RequirementKindTag`(约 131 行)与之一一对应;`RequirementResult`
(约 455 行)有 `Llm/Tool/Interaction/Subagent/Reconfig`,其 `tag()` 与 `RequirementKind::tag()` 对齐;
`RequirementKind::accepts(&RequirementResult)`(约 420 行,内含 `NeedInteraction` 的 response 家族校验)
用于校验 resume 结果匹配。`RequirementKindTag` 还参与一个 tag→sample 构造 map(约 646 行的测试辅助)。

**做什么**:

- 给 `RequirementKind` 增加 `NeedExternalSession { request: ExternalSessionRequest }`。
- 给 `RequirementKindTag` 增加 `ExternalSession` 变体,并更新 `RequirementKind::tag()`、
  `RequirementResult::tag()`、以及所有 `match` 到这些枚举的地方(编译器会指出遗漏点)。
- 给 `RequirementResult` 增加 `ExternalSession(Box<ExternalSessionResult>)`(用 `Box` 避免 enum 膨胀)。
- 更新 `RequirementKind::accepts`:`NeedExternalSession` 只接受 `RequirementResult::ExternalSession`。
- 在 `src/agent/mod.rs` 重导出新增公开类型。
- 保持既有变体行为与 serde tag 名不变(仅新增)。

**验证条件**:

- 新增单测:`external_requirement_accepts_only_external_result`(断言 `accepts` 对齐)、
  `external_requirement_tag_roundtrip`(`RequirementKind`/`RequirementResult` 的 `tag()` 一致且 serde
  round-trip)。过滤名:`cargo test --lib external_requirement`。
- `cargo test --all --all-targets` 全绿(确认没有遗漏 `match` 分支破坏既有测试)。
- 完整验证序列全绿。

**完成记录**:

- `src/agent/requirement.rs`:`RequirementKindTag` 增加 `ExternalSession`(`Display` → `external_session`);
  `RequirementKind` 增加 `NeedExternalSession { request: ExternalSessionRequest }`(复用 M2-1 DTO,`serde`
  变体名 `need_external_session`);`RequirementResult` 增加 `ExternalSession(Box<ExternalSessionResult>)`
  (`Box` 防 enum 膨胀,`ExternalSessionResult` 是携带 observations 的大 payload)。两个 `tag()` 各加一臂。
- `RequirementKind::accepts` 未加特例:external 只做家族 tag 对齐(与 `Reconfig` 一致),`NeedInteraction`
  的 response 家族校验保持原样。既有变体行为与 serde tag 名零改动,纯增量。
- 编译器指出的 exhaustive match 补齐:`src/agent/drive.rs` 的 `scope_handles`
  (`ExternalSession => false`)与 `fulfill_with_scope`(`NeedExternalSession { .. } => None`);二者是
  「external handler 尚未接入 `HandlerScope`」的正确增量态(接入 `external()` 访问器与分派臂属 M2-3 范围,
  非 workaround),故当前 external requirement 会 pop 到 outer 变为 `UnhandledRequirement`。testkit 的
  `assertions/requirements.rs::describe_requirement` 增加 `external(runtime, agent)` 摘要臂。
- `src/agent/mod.rs`:新增的是枚举变体而非命名类型,`ExternalSessionRequest`/`ExternalSessionResult` 及
  `RequirementKind`/`RequirementKindTag`/`RequirementResult` 均已在既有重导出列表,无需新增导出。
- 测试:`requirement.rs` tests 的 `ALL_TAGS`(5→6)、`kind_of`/`result_of` 增加 external 分支(新增
  `external_session_request()`/`external_session_result()` 构造子),使既有 `accepts_matrix` 与
  `every_requirement_kind_round_trips` 自动覆盖 external;`requirement_kind_tag_display_is_stable` 补齐
  reconfig/external 断言。新增 `external_requirement_accepts_only_external_result`(双向断言:external
  requirement 只 accept external result,external result 只被 external requirement accept,其它均
  `ResultKindMismatch`)、`external_requirement_tag_roundtrip`(kind/result `tag()` 一致 + `RequirementKind`
  与 `RequirementKindTag` serde round-trip)。
- **验证结果(完整序列全绿)**:`cargo fmt --all -- --check` 无差异;`cargo test --lib external_requirement`
  2 passed;`cargo clippy --all-targets -- -D warnings` 无告警;`cargo test --all --all-targets` 全绿
  (lib 427 passed〔+2 新测〕、testkit 131 passed、各集成/doc/replay 二进制 0 failed;仅既有 credential-gated
  集成测试保持 ignored,与基线一致);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;
  `git diff --check` 干净。

### [DONE] M2-3 定义 `ExternalSessionHandler` trait 并接入 `HandlerScope`

**前置依赖**:M2-2。

**上下文**:

`src/agent/drive.rs`:`HandlerScope` trait(约 109 行)按 requirement 家族暴露 `llm/tool/interaction/
subagent/reconfig` 五个访问器,默认 `None`,requirement 由 drain 逐层查找、未命中则 pop 到 outer。各 handler
trait(`LlmHandler` 等)均为 `#[async_trait]`,`fulfill(...) -> RequirementResult`,并接收 `&RunContext`。
`SubagentHandler` 额外接收 `outer: &mut dyn Pop`(唯一加深 scope 的 handler)。drain/pop 主逻辑也在
`drive.rs`,会根据 requirement 的 `tag` 分派到对应访问器。

**做什么**:

- 定义 `#[async_trait] pub trait ExternalSessionHandler: Send + Sync`:
  ```rust
  async fn fulfill(&self, request: &ExternalSessionRequest, ctx: &RunContext) -> RequirementResult;
  ```
  返回值必须是 `RequirementResult::ExternalSession(..)`;handler 契约上应「把 session 推进到下一个决策点
  (Completed / PausedForInteraction / Failed)并把期间 event 放进 `observations`」(设计文档 §5.5),
  在 rustdoc 中写明该语义。
- 给 `HandlerScope` 增加 `fn external(&self) -> Option<&dyn ExternalSessionHandler> { None }` 访问器。
- 在 drain/pop 分派逻辑中为 `RequirementKindTag::ExternalSession` 增加一条分派臂:命中本层 `external()`
  则 `fulfill`,否则 pop 到 outer(与其它家族一致,不加深 scope)。
- 重导出 `ExternalSessionHandler`。

**验证条件**:

- 新增单测(可参考 `drive.rs` 已有的 `*_handler_result_is_accepted_by_its_requirement` 模式):一个最小
  scope 提供 `external()`,drain 一个 `NeedExternalSession` 得到 `ExternalSession` 结果并被其 requirement
  `accepts`;另一个测试断言缺省 scope 会把该 requirement pop 到 outer。过滤名:
  `cargo test --lib external_session_handler`。
- 完整验证序列全绿。

**完成记录**:

- `src/agent/drive.rs`:新增 `#[async_trait] pub trait ExternalSessionHandler: Send + Sync`,方法
  `async fn fulfill(&self, request: &ExternalSessionRequest, ctx: &RunContext) -> RequirementResult`,
  rustdoc 写明契约(把 session 推进到下一决策点 Completed/PausedForInteraction/Failed,期间 event 缓冲进
  result 的 `observations`;返回值必须是 `RequirementResult::ExternalSession`,launch/session-lost 失败装进
  `Failed` 变体而非返回错误家族;设计 §5.5)。
- `HandlerScope` 增加 `fn external(&self) -> Option<&dyn ExternalSessionHandler> { None }` 访问器,与其它
  家族一致默认 `None`。
- 分派接线:`scope_handles` 的 `ExternalSession` 臂由 M2-2 占位 `=> false` 改为
  `scope.external().is_some()`;`fulfill_with_scope` 的 `NeedExternalSession { request }` 臂由占位 `=> None`
  改为 `Some(scope.external()?.fulfill(request, ctx).await)`。external 属非 subagent 家族,不加深 scope,未命中
  本层则经既有 pop 路径外弹(§4.2/§7.3),行为与 llm/tool/interaction/reconfig 对齐。
- 导入与重导出:`drive.rs` agent 导入加入 `external::ExternalSessionRequest`;`src/agent/mod.rs` 的
  `pub use drive::{...}` 加入 `ExternalSessionHandler`。
- 文档:模块级 rustdoc 去掉陈旧的 "up to four handlers" 计数(reconfig 已使其失真),改为「up to one handler
  per family」并把 reconfig/external 两个家族补进 handler 列表;`HandlerScope` trait 文档同步去数字化;
  "the four handler traits" → "its handler traits"。
- 测试(`drive.rs` tests,过滤名 `external_session_handler`):新增 `agent_id()`/`external_session_request()`/
  `external_session_result()`(Completed 变体)构造子与计数型 `CountingExternalSessionHandler` fixture;
  `TestScope` 增加 `external` 字段与 `external()` 访问器;`external_requirement(n)` 构造子。三个新测:
  `external_session_handler_result_is_accepted_by_its_requirement`(直接 fulfill,结果被
  `NeedExternalSession::accepts` 接受)、`external_session_handler_drain_fulfills_locally`(drain 命中本层
  `external()`,cursor Done、handler 调用 1 次、resume tag == `ExternalSession`)、
  `external_session_handler_default_scope_pops_to_outer`(inner 无 external、outer 有,drain 后 outer handler
  调 1 次、inner tool 未触及)。
- **验证结果(完整序列全绿)**:`cargo fmt --all -- --check` 无差异;`cargo test --lib external_session_handler`
  3 passed;`cargo clippy --all-targets -- -D warnings` 无告警;`cargo test --all --all-targets` 全绿
  (lib 430 passed〔427 基线 +3 新测〕、testkit 131 passed、各集成/doc/replay 二进制 0 failed;仅既有
  credential-gated 集成测试保持 ignored,与基线一致);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
  --workspace` 无告警;`git diff --check` 干净。

### [DONE] M2-4 在 testkit 实现 `ScriptedExternalSessionHandler` 与 fixtures

**前置依赖**:M2-3。

**上下文**:

`crates/agent-testkit/src/handlers.rs` 已有 `ScriptedLlmHandler`/`ScriptedToolHandler`/
`ScriptedInteractionHandler`/`ScriptedReconfigHandler`,模式是「按顺序返回一个 `Script` 的步骤」或「反应式
决策」。`src/fixtures.rs` 提供 provider-neutral fixtures;`assertions/calls.rs` 提供调用日志断言;
`scope.rs` 提供 scope builder。设计文档 §12 要求新增 `ScriptedExternalSessionHandler`、
`ExternalAgentFixture`、`ExternalAgentCallLog`、`CassetteExternalSessionHandler`(cassette 留到后续)。

**做什么**:

- 在 `crates/agent-testkit/src/handlers.rs`(或新增 `external.rs`)实现 `ScriptedExternalSessionHandler`:
  按顺序返回预置的 `ExternalSessionResult`(Completed / PausedForInteraction / Failed),并记录每次
  `fulfill` 的 request 摘要与返回摘要到一个 call log。
- 新增 `ExternalAgentFixture`:构造典型 `ExternalSessionRequest`、permission 型 `PausedForInteraction`、
  `FilePatch`/`CommandFinished` 等 event、`ExternalAgentOutput`。
- 新增 `ExternalAgentCallLog` 断言 helper(记录调用序号、request/result 摘要、完成顺序),风格对齐
  `assertions/calls.rs`。
- 在 `prelude.rs` / `lib.rs` 导出新组件。

**验证条件**:

- 新增 testkit 单测:用 `ScriptedExternalSessionHandler` 依次返回 Completed 与 PausedForInteraction,
  断言 call log 顺序与摘要正确。过滤名:`cargo test -p agent-testkit external`。
- 完整验证序列全绿。

**完成记录**:

- 新增模块 `crates/agent-testkit/src/external.rs`:
  - `ExternalAgentCallLog` = `CallLog<ExternalSessionRequest, RequirementResult>` 类型别名(记录调用序号、
    request、result、完成顺序,对齐设计 §12)。
  - `ExternalSessionStep`:实现 `ScriptStep`(`FAMILY = RequirementKindTag::ExternalSession`),`into_result`
    → `RequirementResult::ExternalSession(Box::new(result))`;构造子 `ExternalSessionStep::result(..)`。
  - `ScriptedExternalSessionHandler`:`#[async_trait] impl ExternalSessionHandler`,按 dispatch 顺序返回
    脚本化 `ExternalSessionResult`,每次 `fulfill` 记录 request/result 到 `log()`;脚本耗尽(`StrictMode::Error`)
    折叠为**同族** `ExternalSessionResult::Failed { error: ExternalAgentError::Runtime, .. }`,不返回错族
    (对齐 `ScriptedReconfigHandler` 的耗尽折叠语义)。`new` / `from_steps` / `script()` / `log()` 与既有
    scripted handler 一致。
  - `ExternalAgentFixture`(持 `SeqIds` 克隆,id 树确定且唯一):`start_request`/`continue_request`、
    `session_ref`、`policy`、`output`(带 patch artifact)、`file_patch_event`/`command_finished_event`/
    `permission_requested_event`,以及三态结果构造子 `completed`(带 command→patch observations)、
    `permission_pause`(以 `Interaction::question` 表达权限澄清 + `PermissionRequested` observation;
    M4 落地 `InteractionKind::Permission` 后可原地升级)、`failed`。
- 新增断言模块 `crates/agent-testkit/src/assertions/external.rs`(风格对齐 `assertions/calls.rs`):
  `ExternalInputKind`(Start/Continue/RespondInteraction/Shutdown)、`ExternalResultKind`
  (Completed/PausedForInteraction/Failed)判别枚举;`assert_external_calls(&log) -> ExternalAgentCallAssertions`
  fluent builder,count/completed/all_completed/completion_order 委托给 `assert_calls`,新增
  `input_kinds`/`result_kinds` 摘要断言。
- 接线:`lib.rs` `pub mod external;` 与 module-map 文档;`assertions/mod.rs` `mod external;` 与 `pub use`;
  `prelude.rs` 导出 `ExternalAgentCallLog`/`ExternalAgentFixture`/`ExternalSessionStep`/
  `ScriptedExternalSessionHandler` 及 `ExternalAgentCallAssertions`/`ExternalInputKind`/`ExternalResultKind`/
  `assert_external_calls`。
- 测试(5 个,过滤名含 `external`):`returns_scripted_results_in_dispatch_order`(两次 fulfill start→continue,
  断言 Completed→Paused 与 count/completion_order/input_kinds/result_kinds)、
  `completed_result_carries_structured_observations`(结构化 observations = CommandFinished→FilePatch)、
  `exhausted_script_folds_into_family_aligned_failure`(空脚本折叠为同族 Failed(Runtime),count=1、
  result_kinds=[Failed]);assertions 侧 `summaries_track_request_and_result_kinds`、`wrong_input_kinds_panic`。
  `CassetteExternalSessionHandler` 按 §12 留待后续里程碑。
- **验证结果(完整序列全绿)**:`cargo fmt --all -- --check` 无差异;`cargo test -p agent-testkit external`
  5 passed;`cargo clippy --all-targets -- -D warnings` 无告警(修正一处 module-map bullet 触发的
  `doc_nested_refdefs`);`cargo test --all --all-targets` 全绿(testkit lib 136 passed〔131 基线 +5 新测〕、
  各集成/doc/replay 二进制 0 failed,30 个 `test result: ok`;credential-gated 集成测试保持 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;`git diff --check` 干净。

### [DONE] M2-5 Milestone 2 Review

**前置依赖**:M2-1..M2-4。

**上下文**:确认 external session effect 边界完整、自洽、与既有 requirement 家族对齐,且未回归既有行为。

**做什么**:

- 通读 M2-1..M2-4 的 diff,核对:DTO serde round-trip 覆盖;`RequirementKind`/`Tag`/`Result` 三处新增变体
  完全对齐(`tag()` 与 `accepts()`);`HandlerScope::external()` 分派正确且未加深 scope;testkit scripted
  组件可用。
- 明确记录 §14 决策点:`NeedExternalSession` 首版落在核心 `agent-lib`(本计划采用)还是上层 crate。
- 运行一次全量测试确认无回归。

**验证条件**:

- 完整验证序列全绿,`cargo test --all --all-targets` 无回归。
- Review 结论(含决策点、遗留项)写入「完成记录」。

**完成记录**:

- **范围**:通读 M2-1..M2-4 diff(`7700af4`/`ab59f83`/`d76e524`/`063ac2a`),未改任何生产代码;本次仅
  核对自洽性、跑全量验证、并回填 §14 决策点。external session effect 边界经复核判定完整、自洽、与既有
  requirement 家族对齐、且未回归既有运行时语义。

- **DTO serde round-trip 覆盖**(`src/agent/external/mod.rs`):`external_dto_roundtrips` 覆盖
  `ExternalSessionRequest`(Start input + policy)与 `ExternalSessionResult` 全部三态
  (`Completed` 携带 artifacts/usage/cost、`PausedForInteraction` 携带 `Interaction::question` +
  `PermissionRequested` observation、`Failed` 携带 `ShutdownFailed` error);
  `external_session_result_variants_serialize_snake_case` 另验 `#[serde(rename_all="snake_case")]`
  外标签(`completed`/`launch`)。`Option`/`Vec` 字段带 `#[serde(default, skip_serializing_if)]`,
  round-trip 稳定。

- **三处变体对齐**(`src/agent/requirement.rs`):`RequirementKindTag::ExternalSession`(Display
  `"external_session"`)、`RequirementKind::NeedExternalSession { request }`、
  `RequirementResult::ExternalSession(Box<ExternalSessionResult>)` 三处新增一致;`RequirementKind::tag()`
  与 `RequirementResult::tag()` 均映射到 `ExternalSession`;`accepts()` 走统一 tag 比对(external 无
  interaction 那类额外的 request-specific 校验,符合家族语义)。`external_requirement_accepts_only_external_result`
  交叉验证:external 需求只接受 external 结果、且 external 结果只被 external 需求接受(与其余 5 族两两互斥);
  `external_requirement_tag_roundtrip` 验 tag ↔ 序列化。`ExternalSessionResult` 装箱以压小
  `RequirementResult` 体积,合理。

- **`HandlerScope::external()` 分派**(`src/agent/drive.rs`):新增 accessor 默认 `None`,与
  `llm/tool/interaction/subagent/reconfig` 同构;`scope_handles()` 与 `fulfill_with_scope()` 均新增
  `ExternalSession` 臂——就地兑现(`scope.external()?.fulfill(request, ctx).await`),无 handler 时向外
  pop,**未加深 scope**(唯一加深的是 `NeedSubagent`,external 走非 subagent 就地兑现路径)。`validate()`
  仍经 `accepts()` 拦截错族结果。测试 `external_session_handler_result_is_accepted_by_its_requirement` /
  `_drain_fulfills_locally` / `_default_scope_pops_to_outer` 覆盖就地兑现与 pop-to-outer。

- **testkit scripted 组件可用**(`crates/agent-testkit/src/external.rs`、`assertions/external.rs`):
  `ScriptedExternalSessionHandler`(脚本耗尽折叠为**同族** `ExternalSession(Failed{Runtime})`,不返回错族)、
  `ExternalSessionStep`(`FAMILY=ExternalSession`)、`ExternalAgentFixture`、`ExternalAgentCallLog` 别名、
  `assert_external_calls` + `ExternalInputKind`/`ExternalResultKind` 摘要断言均已接线 lib.rs/assertions/prelude
  并有 5 个单测(`returns_scripted_results_in_dispatch_order` / `completed_result_carries_structured_observations` /
  `exhausted_script_folds_into_family_aligned_failure` / `summaries_track_request_and_result_kinds` /
  `wrong_input_kinds_panic`)全过。

- **§14 决策点(回填)**:`NeedExternalSession` **落在核心 `agent-lib`**(本计划采用),而非先在上层 crate 作为
  custom machine + custom driver 扩展。理由:(1) requirement 家族是 effect-model 的枝干抽象,把 external
  session 作为**增量家族**并入,可直接复用既有 addressing / return-path 类型对齐(`tag()`/`accepts()`)、
  drain/pop 组合与 testkit 脚本设施,无需另造平行驱动;(2) DTO 保持 provider-neutral(未把真实 CLI/SDK 的
  wire/私有 JSON 作为稳定协议进核心库),真实 runtime 交互全部隔离在 driver 侧 `ExternalSessionHandler`
  实现里,核心库只承载可序列化事实;(3) 现有 `NeedLlm/NeedTool/NeedInteraction/NeedSubagent/NeedReconfigRegistry`
  路径行为保持不变,external 为纯增量。此决策与 DESIGN §3.1/§13 收敛方向一致,后续 Milestone 3 的
  `ExternalAgentMachine` 亦建立在此核心家族之上。

- **遗留项**(非本里程碑,已在后续任务显式跟踪,不构成回归):
  - `CassetteExternalSessionHandler`(§12 record/replay 替身)尚未落地——留待后续 external 端到端回放需要时补;
    当前 scripted handler 已足以覆盖 M2 effect 边界测试。
  - permission 型 pause 目前以 `Interaction::question` + `PermissionRequested` observation 表达;
    专用 `InteractionKind::Permission` 由 **M4-1** 落地,届时 `ExternalAgentFixture::permission_pause`
    可无损升级(已在 fixture rustdoc 注明)。
  - `ExternalAgentSpec`/`ExternalAgentState`/machine 由 **M3-1+** 承接;`WorkerProfileRef` 调度耦合归 **M6**。

- **验证结果(完整序列全绿)**:
  1. `cargo fmt --all -- --check` 无差异;
  2. 聚焦 `cargo test --lib external`(13 passed)、`cargo test -p agent-testkit --lib external`(5 passed);
  3. `cargo clippy --all-targets -- -D warnings` 无告警;
  4. `cargo test --all --all-targets` 全绿(agent-lib lib 430、testkit lib 136、各集成/replay 套件全过,
     0 failed / 0 无预期外 ignored);
  5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;
  6. `git diff --check` 干净。
  无新观察到的失败测试,无需新增修复/前置任务。Milestone 2 签核,放行 Milestone 3。

---

## Milestone 3 — External agent machine(Phase 2)

### [DONE] M3-1 定义 `ExternalAgentSpec` / `ExternalAgentState` / cursor 与 runtime handles

**前置依赖**:M2-5。

**上下文**:

设计文档 §4.1/§4.2/§4.3 定义 spec、serializable state、runtime handle。参照已有形态:`AgentSpec`
(`src/agent/spec.rs`)、`AgentState`(`src/agent/state.rs`/`state/`)、`LoopCursor`(machine 的可序列化
游标,见 `DefaultAgentMachine` 使用)、`AgentRuntimeHandles`(`src/agent/state/runtime.rs`,泛型 holder,
live handle 不进 serde)。§4.1 的 `WorkerProfileRef` 属于 Milestone 6,本任务先放一个占位引用类型或
`Option`,避免与调度耦合。

**做什么**:

- 在 `src/agent/external/` 定义:
  - `ExternalAgentSpec { id, runtime, worktree, initial_tools, session_policy, /* profile 占位 */ }`。
  - `ExternalAgentState { spec, conversation: Conversation, session: Option<ExternalSessionRef>,
    cursor: ExternalAgentCursor, active_tools: ToolSetRef }`(`Conversation` 见 `src/conversation`)。
  - `ExternalAgentCursor { Idle, AwaitingSession { requirement: CursorRequirement },
    AwaitingInteraction { requirement: CursorRequirement, pending_action: String }, Done, Error { message } }`
    (`CursorRequirement` 见 requirement/state 层,记录未决 `RequirementId`)。
  - `ExternalRuntimeHandles<R>`(非 serde):持有 runtime、可选 `interaction`/`tool_registry`、`session_tasks`。
- state 只存可恢复事实(§4.2);进程/task/watcher 一律进 handle。

**验证条件**:

- 新增单测:`ExternalAgentState` serde round-trip(不含 handle),`ExternalAgentCursor` 各变体 round-trip。
  过滤名:`cargo test --lib external_agent_state`。
- 完整验证序列全绿。

**完成记录**:

- 新增 `src/agent/external/spec.rs`:
  - `WorkerProfileRef`(占位 `#[serde(transparent)]` newtype,承载 §9 worker 画像引用,M6 展开;`new`/`id`)。
  - `ExternalAgentSpec { id, runtime, worktree, profile: Option<WorkerProfileRef>, initial_tools,
    session_policy }`,私有字段 + `new` + 访问器,形态对齐 `AgentSpec`(§4.1)。`profile` 用 `Option` +
    占位类型避免与 M6 调度耦合(`skip_serializing_if` 缺省不落盘)。
- 新增 `src/agent/external/state.rs`:
  - `ExternalAgentCursor { Idle, AwaitingSession { requirement }, AwaitingInteraction { requirement,
    pending_action }, Done, Error { message } }`,`#[serde(tag = "state", content = "data", snake_case)]`,
    `#[derive(Default)]`=Idle;helper `is_idle`/`is_terminal`/`requirement`(§4.2)。
  - `ExternalAgentState { spec, conversation, session: Option<ExternalSessionRef>, cursor, active_tools }`,
    私有字段 + 访问器 + `set_cursor`/`set_session`/`set_active_tools`;`new` 从 spec.initial_tools 播种
    active_tools、session=None、cursor=Idle。自定义 `Serialize`/`Deserialize` 经 `ConversationSnapshot`
    (`snapshot()`/`Conversation::restore`)跨持久化边界,record 结构 `deny_unknown_fields`,与 `AgentState`
    做法一致;只存可恢复事实,进程/task/watcher 不进 serde。
- 新增 `src/agent/external/runtime.rs`:
  - `ExternalRuntimeHandles<Runtime, InteractionHandle = (), ToolRegistryHandle = (), SessionTasks = ()>`
    (非 serde 泛型 holder,对齐 `AgentRuntimeHandles`,§4.3):`runtime` 必填,`interaction`/`tool_registry`
    可选,`session_tasks` 泛型;`new`/`with_handles`/访问器 + `session_tasks_mut`。
- 接线:`external/mod.rs` `mod spec/state/runtime;` + `pub use`,模块 doc 更新(不再「only defines data」,
  说明 machine 持久化形态与 handle 边界);`agent/mod.rs` re-export `ExternalAgentCursor`/`ExternalAgentSpec`/
  `ExternalAgentState`/`ExternalRuntimeHandles`/`WorkerProfileRef`。
- 测试(3 个,过滤名 `external_agent_state`):
  `external_agent_state_serde_round_trips_through_conversation_snapshot`(带 committed conversation、session、
  awaiting_session cursor、2 工具;断言 spec/conversation/session/cursor/active_tools 及**无** runtime handle
  key)、`external_agent_state_defaults_to_idle_without_session`(缺省 Idle、session 落盘被 skip)、
  `external_agent_state_cursor_variants_round_trip`(五变体 round-trip + helper 断言);另 runtime.rs 2 个
  handles smoke 测。
- **验证结果(完整序列全绿)**:`cargo fmt --all` 无差异;`cargo clippy --all-targets -- -D warnings` 0 告警
  (移除未使用的 `conversation_mut`,留待 M3-2 machine 落地时新增);`cargo test --lib external_agent_state`
  3 passed、`cargo test --lib external` 18 passed;`cargo test --all --all-targets` 全绿(30 个
  `test result: ok`,0 failed;credential-gated 集成测试保持 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 0 告警。

### [DONE] M3-2 实现 `ExternalAgentMachine` 基本推进

**前置依赖**:M3-1。

**上下文**:

`AgentMachine` trait(`src/agent/machine/mod.rs`):
```rust
pub trait AgentMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome;
    fn cursor(&self) -> &LoopCursor;
}
```
`StepInput { External(AgentInput), Resume(RequirementResolution), Abandon(RequirementId) }`;
`StepOutcome { notifications, requirements, quiescent }`。参考 `DefaultAgentMachine`
(`src/agent/machine/default/mod.rs`)如何 park 在 cursor 上、emit `Requirement`、在 `Resume` 时 fold 结果。
machine 必须是纯函数:`step` 不 `await`、不做 IO。

**做什么**:

- 实现 `ExternalAgentMachine`(实现 `AgentMachine`),覆盖基本推进:
  - `step(External(UserMessage/brief))` → emit `NeedExternalSession { input: Start { prompt } }`,park 到
    `AwaitingSession`。
  - `step(Resume(ExternalSession(Completed)))` → 记录 session/output,更新 `Conversation`,cursor → `Done`,
    `quiescent = true`。
  - `step(Resume(ExternalSession(Failed)))` → cursor → `Error { message }`。
  - `Continue { message }` 输入路径:在已有 session 上再次推进。
- observations 暂时可先不转 notification(留给 M5),但结构上要透传。
- `cursor()` 返回与 `ExternalAgentCursor` 对应的 `LoopCursor` 视图(与 `DefaultAgentMachine` 的映射方式一致)。

**验证条件**:

- 用 M2-4 的 `ScriptedExternalSessionHandler` + testkit step/drain harness(`crates/agent-testkit/src/harness.rs`)
  写:`external_agent_start_to_completed`(Start→Completed→quiescent/Done)、`external_agent_start_to_failed`。
  过滤名:`cargo test external_agent_start`。
- 完整验证序列全绿。

**完成记录**:

- 新增 `src/agent/external/machine.rs`:实现纯 sans-io 的 `ExternalAgentMachine`(`impl AgentMachine`),
  覆盖基本推进:
  - `step(External(UserMessage))` → `begin_user_turn`:`Conversation::begin_turn` 开 turn、记录非序列化
    `InFlight { assistant_message_id }` scratch,已有 session 走 `Continue { message }`、否则 `Start { prompt }`,
    经 `RequirementIds::next_requirement_id(ExternalSession)` reify 一个 `NeedExternalSession`,park 到
    `AwaitingSession`,`quiescent = true`。
  - `step(Resume(ExternalSession(Completed)))` → `set_session`、把 `output.summary` 折成 assistant `Response`
    (`start_assistant_response`/`finish_assistant`/`commit_pending`),cursor → `Done`。
  - `step(Resume(ExternalSession(Failed)))` → 保留返回的 session facts、丢弃 pending turn,cursor →
    `Error { message }`(空消息兜底为固定文案,避免 `LoopCursor::error` 空串回退)。
  - `Continue` 输入路径:已建立 session 上二次推进(单元 + 集成各一测)。
  - `PausedForInteraction`/`AwaitingInteraction` resume 留给 M3-3、abandon 完整清理与 shutdown disposition 留给
    M3-4,本任务以「明确定义的 quiescent 收敛(clean `fail`/回 `Idle`)」占位,非 workaround;observations 结构上
    透传但暂不转 notification(留给 M5)。
  - cursor 视图:machine 持一个非序列化 `loop_cursor: LoopCursor`,经 `settle(external, loop_cursor)` 与
    `ExternalAgentCursor` 锁步(Idle→Idle、AwaitingSession→`streaming_step(step_id, Some(req))`、Done→
    `done(Completed)`、Error→`error(msg)`),与 `DefaultAgentMachine` 的映射方式一致。
- `src/agent/external/state.rs`:补 `pub(crate) const fn conversation_mut()`(M3-1 预留、此处落地),供 machine
  在受检 fold 中改写 Conversation。
- 接线:`external/mod.rs` `mod machine;` + `pub use machine::ExternalAgentMachine;`;`agent/mod.rs` re-export
  `ExternalAgentMachine`。
- testkit:`crates/agent-testkit/src/scope.rs` 给 `TestScope`/`TestScopeBuilder` 增加 `external` family
  (字段 + `.external(handler)` setter + `HandlerScope::external` 委派 + Debug/单测断言);
  `crates/agent-testkit/src/external.rs` 给 `ExternalAgentFixture` 增加 `spec()`/`agent_state()`/`machine()`
  (runtime=ClaudeCode、worktree `/repo/agent-lib`、空 toolset、`policy()`,requirement id 取自同一 `SeqIds` 树)。
- 测试:
  - 单元(`src/agent/external/machine/tests.rs`,8 个,过滤名 `agent::external::machine`):user-message→park
    Start、Completed→Done+commit、Continue 复用 session、Failed→Error、错配 requirement→Error、idle resume 拒绝、
    pivot 拒绝、abandon 收敛回 Idle。
  - 集成(`tests/agent_external_basic.rs`,3 个):`external_agent_start_to_completed`、
    `external_agent_start_to_failed`(过滤名 `cargo test external_agent_start` 命中此二)、
    `external_agent_continue_advances_established_session`,均经 `ScriptedExternalSessionHandler` +
    `DrainHarness` 驱动,断言最终 cursor、`assert_external_calls` 的 input/result kind、以及提交的 Conversation。
- **验证结果(完整序列全绿)**:`cargo fmt --all` 无差异;`cargo clippy --all-targets -- -D warnings` 0 告警;
  `cargo test external_agent_start` 2 passed;`cargo test --all --all-targets` 全绿(0 failed;credential-gated
  集成测试保持 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 0 告警。

### [DONE] M3-3 实现两段式交互(Paused → NeedInteraction → RespondInteraction)

**前置依赖**:M3-2。

**上下文**:

设计文档 §5.2/§6.2:session handler 不自己调用 `InteractionHandler`,而是把外部权限/澄清请求转成
`ExternalSessionResult::PausedForInteraction { request: Interaction }` 返回;machine 下一步 emit
`RequirementKind::NeedInteraction { request }`,由 local 或 outer interaction handler 兑现;拿到
`InteractionResponse` 后 machine 再 emit `NeedExternalSession { input: RespondInteraction { action_id,
response } }` 把结果喂回。`Interaction`/`InteractionResponse` 在 `src/agent/interaction.rs`。

**做什么**:

- 扩展 `ExternalAgentMachine`:
  - `Resume(ExternalSession(PausedForInteraction { request, .. }))` → 保存 `pending_action`(action_id),
    cursor → `AwaitingInteraction`,emit `NeedInteraction { request }`。
  - `Resume(Interaction(response))` → emit `NeedExternalSession { input: RespondInteraction { action_id:
    pending_action, response } }`,cursor 回到 `AwaitingSession`。
  - 支持一个 turn 内多次 Paused↔Respond 循环,直到 Completed/Failed。
- action_id 对齐:`RespondInteraction` 的 `action_id` 必须与触发它的 `PausedForInteraction` 一致。

**验证条件**:

- 新增测试 `external_agent_pause_resume_interaction`:scripted handler 先返回 Paused、interaction handler
  返回一个 response、scripted handler 再返回 Completed;断言 machine 依次 emit `NeedInteraction` 与带正确
  `action_id` 的 `NeedExternalSession(RespondInteraction)`,最终 Done。过滤名:`cargo test external_agent_pause`。
- 缺省 local scope 无 interaction 时,`NeedInteraction` 正确 pop 到 outer(用 scope builder 构造两层)。
- 完整验证序列全绿。

**完成记录**:

- DTO 补全(effect 契约,非 workaround):`src/agent/external/mod.rs` 给
  `ExternalSessionResult::PausedForInteraction` 增加显式 `action_id: String` 字段——runtime 对暂停动作的句柄,
  machine 存为 cursor 的 `pending_action`,并在 `RespondInteraction { action_id, response }` 里原样回喂,让
  runtime 把回答对回它暂停的那个动作。设计文档 §6.2 的 action_id 本来自 `Permission(PermissionRequest {
  action_id })`,但 `InteractionKind::Permission` 属 M4-1、当前不可用,TODO M3-3 又明确要求从
  `PausedForInteraction { request, .. }` 保存 `pending_action`,故在此显式携带,`Permission` 落地后它仍是回喂的
  规范句柄。同步更新 `docs/external-agent.md` §5.2 与 mod.rs / requirement.rs 里既有的 `PausedForInteraction`
  构造点与 serde round-trip 单测。
- machine(`src/agent/external/machine.rs`):
  - `InFlight` scratch 增加 `step_id: StepId`(loop cursor 视图跨 turn 内 pause↔respond 各跳复用同一 step id)。
  - `resume` 改为按 cursor 分派:`AwaitingSession`→`resume_session`,`AwaitingInteraction`→`resume_interaction`
    (经内部 `Awaiting` 枚举先从借用的 cursor 读出待决 id / pending_action,再放开借用做可变迁移)。
  - 新增 `pause_for_interaction`:fold `PausedForInteraction`——`set_session` 记录会话事实,经
    `next_requirement_id(Interaction)` reify 一个 `NeedInteraction { request }`,park 到
    `AwaitingInteraction { requirement, pending_action }`,loop cursor 映射为
    `streaming_step(step_id, Some(req))`;in-flight turn 跨 pause 保持打开。
  - 新增 `resume_interaction`:校验 resolution.id 对齐、取 `RequirementResult::Interaction(response)`(错配家族
    → clean `fail`→Error),经 `block_on_session` 以 `RespondInteraction { action_id: pending_action, response }`
    回喂并重新 park 到 `AwaitingSession`。
  - `fold_session_result` 的 `PausedForInteraction` 分支由「M3-3 not yet supported」占位改为调用
    `pause_for_interaction`;模块文档更新为覆盖 M3-2+M3-3。
- 测试:
  - 单元(`src/agent/external/machine/tests.rs`,新增 5 个,共 13 个 `agent::external::machine`):
    pause→emit `NeedInteraction`+park `AwaitingInteraction`、respond→emit `RespondInteraction` 且 action_id
    对齐(`act-42`)+ 回 `AwaitingSession`、pause→respond→completed 折成单一 committed turn、`AwaitingInteraction`
    收到非 Interaction 结果→Error、interaction resume 错配 requirement→Error。
  - 集成(新文件 `tests/agent_external_interaction.rs`,2 个,过滤名 `cargo test external_agent_pause` 命中):
    `external_agent_pause_resume_interaction`(scripted external `[permission_pause, completed]` +
    `ScriptedInteractionHandler` answer,断言 input_kinds `[Start, RespondInteraction]`、result_kinds
    `[PausedForInteraction, Completed]`、interaction 计一次、最终 Done、提交 1 turn)、
    `external_agent_pause_pops_interaction_to_outer_scope`(local scope 仅 external,`wrapping` 外层供 interaction,
    `NeedInteraction` 正确路由并跑到 Done)。
  - testkit:`crates/agent-testkit/src/external.rs` 的 `permission_pause` fixture 补 `action_id: "act-1"`(与
    `PermissionRequested` 观测的 action_id 对齐),更新其 rustdoc。
- **验证结果(完整序列全绿)**:`cargo fmt --all` 无差异;`cargo clippy --all-targets -- -D warnings` 0 告警;
  `cargo test external_agent_pause` 2 passed;`cargo test --all --all-targets` 全绿(total 663 passed,0 failed;
  credential-gated 集成测试保持 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 0 告警。

### [DONE] M3-4 cancel/abandon 清理、shutdown disposition 与 `NestedMachine` 挂载

**前置依赖**:M3-3。

**上下文**:

设计文档 §6.4:cancel 是 never-resume——`RunContext` 触发 cancel 后 driver 放弃 continuation,machine 不再
被 step,已启动的外部 session 由 runtime handle / registry 负责关闭(kill 进程、关 stdin/stdout、终止 task),
不依赖 machine 再走一步 effect;trace 必须记录 shutdown disposition(优雅关闭 / 强制 kill / 关闭失败)。
`StepInput::Abandon(RequirementId)` 是 never-resume 关闭(参考 `DefaultAgentMachine` 的 `AbandonKind` 处理)。
`NestedMachine`(`src/agent/machine/nested.rs`)把 child `AgentMachine` 挂到父 machine 的 slot;
`SubagentHandler`(`src/agent/drive.rs`)在兑现 `NeedSubagent` 时派生 child `RunContext`/scope 并驱动 child。
`RunContext::derive_child`/`is_cancelled`/`check_cancelled` 在 `src/agent/context.rs`。

**做什么**:

- 处理 `step(Abandon(id))`:把 cursor 从 `AwaitingSession`/`AwaitingInteraction` 收敛到一个可复用的终止态,
  并在 state / trace 上标注需要外部 session 清理(不 emit 新 requirement)。
- 明确清理归属:约定 `ExternalRuntimeHandles`(M3-1)实现 `Drop` 或由 owning 容器 teardown 时清扫孤儿
  session;在 rustdoc 与 machine 注释中写明「abandon 不负责 emit Shutdown,清理在 handle 层」。
- 记录 shutdown disposition:定义一个小枚举(优雅 / 强制 kill / 失败)并在 trace/notification 中体现(trace
  API 见 `src/agent/context/trace.rs`)。
- 验证挂载:确认 `ExternalAgentMachine` 能作为 child 挂到 `NestedMachine` 的 slot,并能由一个最小
  `SubagentHandler` 实现(或复用 testkit 的 `ScriptedSubagentSpawner`,见 `crates/agent-testkit/src/subagent.rs`)
  在兑现 `NeedSubagent` 时派生并驱动。

**验证条件**:

- 新增测试:`external_agent_abandon_settles_and_flags_cleanup`(abandon 后 cursor 收敛、标注清理、不 emit 新
  requirement);`external_agent_mounts_under_nested_machine`(作为 child 被派生并跑完 Start→Completed)。
  过滤名:`cargo test external_agent_abandon` 与 `cargo test external_agent_mounts`。
- 完整验证序列全绿。

**完成记录**:

- shutdown disposition 类型(新文件 `src/agent/external/shutdown.rs`):`ExternalSessionShutdown` 三态
  `Graceful` / `ForcedKill` / `Failed`,`Copy`(因 `TraceNodeKind` 需保持 `Copy`,详细失败文本仍留在
  `ExternalAgentError::ShutdownFailed`);`leaves_residual_side_effects()` 对 `ForcedKill`/`Failed` 返回
  `true`(§6.4/§10),`label()` 给稳定 snake_case;snake_case serde;3 个单元测试。`mod` 与 re-export 加到
  `src/agent/external/mod.rs` 与 `src/agent/mod.rs`。
- trace 记录:`src/agent/context/trace.rs` 新增 `TraceNodeKind::ExternalShutdown { disposition }` 变体与
  `TraceHandle::record_external_shutdown(id, disposition)`(把 disposition 记为节点 + label);testkit
  `crates/agent-testkit/src/assertions/trace.rs` 的 `describe_kind` 补 `external_shutdown(<label>)` 分支;
  `src/agent/context/tests.rs` 新增 `external_shutdown_trace_node_records_disposition_under_parent`。
- state 清理标注(`src/agent/external/state.rs`):`ExternalAgentState` 增 `cleanup_required: bool` 字段 +
  `cleanup_required()` / `mark_cleanup_required()` / `clear_cleanup_required()`;record 结构 + serde
  `#[serde(default, skip_serializing_if = "is_false")]`(clean 态不落盘,保持 pre-M3-4 shape);新增
  `cleanup_required_flag_round_trips_and_is_skipped_when_clear` 单元测试。
- machine abandon(`src/agent/external/machine.rs`):`abandon` 重写为 never-resume 关闭——当 cursor 有
  outstanding requirement(`AwaitingSession`/`AwaitingInteraction`)时 `mark_cleanup_required()`,丢弃悬空 turn、
  清 `in_flight`、收敛回可复用 `Idle`,不 emit 新 requirement / 不 emit `Shutdown`;更新 abandon rustdoc 与
  模块头文档(覆盖 M3-4,写明「清理在 handle 层、disposition 由 handle 层记 trace」,§6.4)。
- 清理归属(`src/agent/external/runtime.rs`):扩写 `ExternalRuntimeHandles` rustdoc,写明进程生命周期归 handle
  层所有(inner-handle `Drop` / 容器 teardown 清扫),abandon 不 emit `Shutdown`,disposition 经
  `ExternalSessionShutdown` 记入 trace(§6.4/§10)。
- 挂载验证:`ExternalAgentMachine` 是普通 `AgentMachine`,经标准 `NeedSubagent` → `DrivingSubagentHandler`
  在派生子 `RunContext` 下开嵌套 drain 驱动(`NestedMachine.own` 为具体 `DefaultAgentMachine`,子槽为
  `NestedMachine`,故外部 machine 走 `SpawnedChild.machine: Box<dyn AgentMachine>` 路径,而非字面 slot child)。
- 测试:
  - 单元(`src/agent/external/machine/tests.rs`):把旧 `external_abandon_settles_back_to_idle` 重写为
    `external_agent_abandon_settles_and_flags_cleanup`(断言收敛 Idle + `cleanup_required()`),新增
    `external_agent_abandon_while_awaiting_interaction_flags_cleanup` 与
    `external_agent_abandon_when_idle_does_not_flag_cleanup`(idle 无 outstanding session 不标注清理)。
  - 集成(新文件 `tests/agent_external_lifecycle.rs`,2 个,过滤名 `cargo test external_agent_abandon` 与
    `cargo test external_agent_mounts` 命中):`external_agent_abandon_settles_and_flags_cleanup`(先 cancel
    `RunContext` 再 `run_user`,drain 走 never-resume abandon,断言 Idle + 未调用 runtime handler + 标注清理 +
    未提交 turn)、`external_agent_mounts_under_nested_machine`(父 `ScriptMachine` 发一个 `NeedSubagent`,child =
    `ExternalAgentMachine` 经 `SpawnedChildBuilder` + `ScriptedSubagentSpawner` 驱动 Start→Completed,断言父
    turn 收敛 Done、spawn 一次、child 外部调用 `[Start]`→`[Completed]`)。
- 文档:`docs/external-agent.md` §6.4 补「M3-4 落地的具体类型」段(`ExternalSessionShutdown`、
  `record_external_shutdown`、`cleanup_required` 三方法、子 agent 走 `NeedSubagent`)。
- **验证结果(完整序列全绿)**:`cargo fmt --all` 无差异;`cargo clippy --all-targets -- -D warnings` 0 告警;
  `cargo test external_agent_abandon` 命中 4 个全过、`cargo test external_agent_mounts` 命中 1 个全过;
  `cargo test --all --all-targets` 全绿(total 672 passed,0 failed;credential-gated 集成测试保持 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 0 告警。

### [DONE] M3-5 Milestone 3 Review

**前置依赖**:M3-1..M3-4。

**上下文**:确认 machine 覆盖 Start/Completed/Failed、两段式交互、cancel 清理与挂载,且 `step` 保持纯函数。

**做什么**:

- 审阅 machine 的状态迁移是否穷尽(Idle/AwaitingSession/AwaitingInteraction/Done/Error 无悬空态);确认
  `step` 无 `await`/无 IO;确认清理归属与 §6.4 一致。
- 用 harness 跑一遍完整场景(Start→Paused→Respond→Completed)与取消场景,确认无回归。

**验证条件**:

- 完整验证序列全绿,`cargo test --all --all-targets` 无回归。
- Review 结论与遗留项写入「完成记录」。

**完成记录**:

- **状态迁移穷尽性(通过)**:`ExternalAgentCursor` 恰有 5 态(`Idle` / `AwaitingSession` /
  `AwaitingInteraction` / `Done` / `Error`)。`ExternalAgentMachine::step` 的顶层 `match` 覆盖
  `External(UserMessage)` / `External(Pivot)`(显式 fail)/ `Resume` / `Abandon` 四路;`resume` 按 cursor 分派到
  `AwaitingSession`/`AwaitingInteraction`,其余(`Idle`/`Done`/`Error`)统一走「无 outstanding requirement」的
  fail 分支。`ExternalAgentCursor::requirement()` / `is_terminal()` / `is_idle()` 全部**显式列出每个 variant**,
  无 `_` 通配。machine.rs 中的 3 处通配臂(cursor `other`、`RequirementResult` `other` ×2)与 `message_text`
  的 `_ => None` 均为**有意的错误路由 / 内容过滤**,不是悬空态——任何非法输入都收敛到 `Error` 或 `fail(..)`,
  不会遗留半推进状态。
- **`step` 无 `await`/无 IO(通过)**:machine.rs 全文无 `.await`、无 `async fn`、无
  `std::{fs,io,net,process}`、无 `tokio::`/`spawn`/`block_on`。`block_on_session` 仅是「reify 一个
  `NeedExternalSession` requirement 并 park 到 `AwaitingSession`」的纯函数命名,不做真实阻塞;IO/运行时推进全部
  由 driver 侧的 `ExternalSessionHandler` 承担。machine 满足 sans-io 契约,与 `DefaultAgentMachine` 同构。
- **清理归属与 §6.4 一致(通过)**:`abandon` 为 never-resume——当 cursor 有 outstanding requirement 时
  `mark_cleanup_required()`、丢弃悬空 pending turn、清 `in_flight`、收敛回可复用 `Idle`,**不 emit 新
  requirement、不 emit `Shutdown`**;进程/连接 force-close 与 `ExternalSessionShutdown` disposition 记录归
  handle 层(`ExternalRuntimeHandles` / registry),经 `TraceHandle::record_external_shutdown` 落 trace。
  `shutdown.rs`/`runtime.rs`/machine rustdoc 对此归属描述一致。
- **场景回归(通过,复用既有测试,无需新增)**:
  - Start→Completed / Start→Failed / Continue:`tests/agent_external_basic.rs`(3)。
  - 完整两段式 Start→Paused→Respond→Completed:`tests/agent_external_interaction.rs::external_agent_pause_resume_interaction`
    与单元 `external_pause_then_respond_then_complete_commits_the_turn`;interaction 弹到外层由
    `external_agent_pause_pops_interaction_to_outer_scope` 覆盖。
  - 取消(never-resume abandon):`tests/agent_external_lifecycle.rs::external_agent_abandon_settles_and_flags_cleanup`
    与 3 个单元 abandon 测试(awaiting_session / awaiting_interaction / idle-无清理)。
  - 挂载:`external_agent_mounts_under_nested_machine`(经 `NeedSubagent` 派生 child 跑 Start→Completed)。
- **遗留项(review finding,已就地修正)**:`initial_loop_cursor` 的 rustdoc 旧引用
  「restoring a mid-flight external machine is out of scope until the mount/cleanup work in M3-4」已过时——M3-4
  已完成且并未加入 mid-flight restore。已改写为「faithfully rehydrating the driver-facing view of a mid-flight
  external machine is a persistence concern beyond milestone 3's scope」,准确反映现状(awaiting 态恢复仍回退
  `LoopCursor::Idle`,忠实 rehydrate 属未来持久化关注点,不在 M3 范围)。此为仅注释改动,未改变运行时行为。
- **无其它遗留 / 无未调度失败**:Milestone 3 全部特性(basic advance / 两段式交互 / abandon 清理 / shutdown
  disposition / nested 挂载)均已实现且有测试守护;未发现新的规格偏差或需插入的前置任务。
- **验证结果(完整序列全绿)**:`cargo fmt --all` 无差异;`cargo clippy --all-targets -- -D warnings` 0 告警;
  `cargo test external_agent` 命中 13 个全过(6 lib + 3 basic + 2 interaction + 2 lifecycle);
  `cargo test --all --all-targets` 全绿(total 672 passed,0 failed;credential-gated 集成测试保持 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 0 告警。

---

## Milestone 4 — Interaction permission 泛化(Phase 3)

### [DONE] M4-1 新增 `InteractionKind::Permission` 与 `PermissionRequest`

**前置依赖**:M3-5。

**上下文**:

`src/agent/interaction.rs`:`InteractionKind { Approval { call_id, requirement }, Question { prompt },
Choice { prompt, options } }`;`InteractionKindTag { Approval, Question, Choice }`;`Interaction` 有
`approval`/`question`/`choice` 构造器与 `validate_response`(约 103 行,按家族校验 response)。设计文档 §3.3
提议新增 `Permission { request: PermissionRequest }`,`PermissionRequest { action_id, actor: AgentId,
category: PermissionCategory, summary, subject: Value, risk: PermissionRisk, reason: Option<String> }`,
`PermissionCategory { Shell, FileRead, FileWrite, Network, SpawnAgent, Mcp, Other }`。

**做什么**:

- 新增 `PermissionRequest`、`PermissionCategory`、`PermissionRisk`(如 `Low/Medium/High/Critical`)类型。
- 给 `InteractionKind` 增加 `Permission { request: PermissionRequest }`;给 `InteractionKindTag` 增加
  `Permission`,更新 `tag()` 与所有相关 `match`。
- 增加构造器 `Interaction::permission(step_id, PermissionRequest)`。
- 现有 `Approval` 家族保持不变(设计文档说明 `Approval` 仍服务于本框架 tool approval)。

**验证条件**:

- 新增单测:`Interaction::permission` 的 tag 正确、serde round-trip。过滤名:`cargo test --lib permission`。
- `cargo test --all --all-targets` 确认既有 interaction 测试无回归。
- 完整验证序列全绿。

**完成记录**:

- **新增类型(`src/agent/permission.rs`)**:与 `approval.rs` 对称新建独立模块,interaction 层复用它。
  `PermissionRequest { action_id: String, actor: AgentId, category: PermissionCategory, summary: String,
  subject: serde_json::Value, risk: PermissionRisk, reason: Option<String> }`——字段集与设计文档 §3.3 一致;
  提供 `new(..)` 构造器(空 `reason` 归一化为 `None`)与只读 accessor。`PermissionCategory { Shell, FileRead,
  FileWrite, Network, SpawnAgent, Mcp, Other }`;`PermissionRisk { Low, Medium, High, Critical }`(派生 `Ord`,
  least→most severe,便于 M4-3 的 deny-by-default 阈值比较);两枚举均 `#[serde(rename_all = "snake_case")]` 并实现
  `Display`。经 `pub use permission::{PermissionCategory, PermissionRequest, PermissionRisk}` 从 `agent` re-export。
- **`InteractionKind` 扩展**:新增 `Permission { request: PermissionRequest }`;`InteractionKind::tag()` 增补
  `Permission => InteractionKindTag::Permission`。`InteractionKindTag` 新增 `Permission` variant 并补齐 `Display`
  ("permission")。新增构造器 `Interaction::permission(step_id, request)`。`Approval` 家族完全不变(仍绑 `ToolCallId`,
  服务本框架 tool approval)。`serde_json::Value` 已实现 `Eq`(`ToolCall` 亦 derive `Eq` 且含 `Value`),故
  `PermissionRequest`/`InteractionKind`/`Interaction` 的 `Eq` 派生不受影响。
- **响应侧留待 M4-2/M4-3(无 workaround)**:M4-1 仅落 request 侧,`InteractionResponse` 尚无 `Permission` 变体。
  `Interaction::accepts_response` 的 catch-all 臂天然把任何响应对 `Permission` 请求判为 `ResponseKindMismatch`,无需改。
  既有 auto-responder 的穷尽 `match request.kind()` 因新增 variant 需补 `Permission` 臂;因当前没有合法 permission
  响应可返,统一补 `panic!` 且注明去向:`drive/reference.rs` `ApprovalInteractionHandler` 与
  `agent-testkit/handlers.rs` `approval_response` 的真实 deny-by-default 由 **M4-3** 追踪替换;`drive.rs` 与
  `subagent/tests.rs` 内的测试 handler 永不收到 `Permission`,panic 臂长期有效(与既有 "never approvals" 风格一致)。
  `examples/agent_chat.rs` 的 `StdinApproval` 亦补 panic 臂(DefaultAgentMachine 从不 emit permission)。
- **新增单测**:`permission.rs` 5 个(request round-trip / category+risk round-trip / risk 排序 / 空 reason 归一化 /
  Display snake_case);`interaction.rs` 1 个(`Interaction::permission` tag 为 `Permission` 且 serde round-trip)。
  `cargo test --lib permission` 命中 6 个全过。
- **验证结果(完整序列全绿)**:`cargo fmt --all` 无差异;`cargo clippy --all-targets -- -D warnings` 0 告警;
  `cargo test --lib permission` 6 过;`cargo test --all --all-targets` 全绿(total 678 passed,0 failed;
  credential-gated 集成测试保持 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 0 告警;
  `git diff --check` 干净。

### [DONE] M4-2 扩展 `InteractionResponse` 的 permission 响应并校验

**前置依赖**:M4-1。

**上下文**:

`src/agent/interaction.rs`:`InteractionResponse { Approval(ApprovalResponse), Answer(String),
Choice(usize) }`,其 `tag()` 与 `Interaction::validate_response` 保证请求/响应家族匹配
(`Approval↔Approval`、`Question↔Answer`、`Choice↔Choice`)。设计文档 §14 把 permission 响应 shape 列为
开放问题,本任务给出最小落地。

**做什么**:

- 新增 `PermissionDecision { Approve, Deny { reason: Option<String> }, Cancel }`(或复用/对齐
  `ApprovalDecision` 的形态,见 `src/agent/approval.rs`,但不绑定 `ToolCallId`),并新增
  `PermissionResponse { action_id: String, decision: PermissionDecision }`。
- 给 `InteractionResponse` 增加 `Permission(PermissionResponse)` 变体,更新 `tag()`。
- 更新 `Interaction::validate_response`:`Permission` 请求只接受 `Permission` 响应,且 `action_id` 必须匹配。
- 更新 `RequirementKind::accepts` 中 `NeedInteraction` 的家族校验路径(在 `requirement.rs`),确保
  permission 请求/响应对齐。

**验证条件**:

- 新增单测:`permission_response_family_matches`、`permission_response_action_id_mismatch_rejected`。
  过滤名:`cargo test --lib permission_response`。
- 完整验证序列全绿。

**完成记录**:

- **响应侧类型(`src/agent/permission.rs`)**:与 `approval.rs`(同时含 request/response)对称,新增
  `PermissionDecision { Approve, Deny { reason: Option<String> }, Cancel }`——externally-tagged 枚举,
  `#[serde(rename_all = "snake_case")]`;`Deny.reason` 用 `#[serde(default, skip_serializing_if = "Option::is_none")]`,
  故 `Approve`/`Cancel` 序列化为 `"approve"`/`"cancel"`,`Deny{None}` 为 `{"deny":{}}`。采用「timeout 由 backend
  映射为 deny/cancel」的最小 decision 集(设计文档 §14 决策,M4-3 的 backend 层落地),不引入独立 `Timeout` 变体。
  新增 `PermissionResponse { action_id: String, decision: PermissionDecision }`(`#[serde(deny_unknown_fields)]`),
  构造器 `new/approve/deny/cancel`(`deny` 空 reason 归一化为 `None`)与 accessor `action_id()`/`decision()`。
  经 `agent::{PermissionDecision, PermissionResponse}` re-export。
- **`InteractionResponse` 扩展(`src/agent/interaction.rs`)**:新增 `Permission(PermissionResponse)` 变体;
  `InteractionResponse::tag()` 增补 `Permission => InteractionKindTag::Permission`。`accepts_response` 新增
  `(InteractionKind::Permission { request }, InteractionResponse::Permission(response))` 臂:`action_id` 一致则
  `Ok`,否则返回新增的 `InteractionError::ActionMismatch { expected, actual }`;其余家族仍走 catch-all 的
  `ResponseKindMismatch`。新增对称便捷构造器 `InteractionResponse::permission_for(interaction, response)`(先包裹再
  `accepts_response` 校验)。
- **error 派生调整(无 workaround)**:`InteractionError::ActionMismatch` 携带 `String` 字段,故 `InteractionError`
  去掉 `Copy` 派生(保留 `Clone`);连带 `RequirementError`(含 `Interaction(#[from] InteractionError)`,Copy 需全字段 Copy)
  同步去掉 `Copy`。已核对全仓无处依赖二者的 `Copy` 语义(仅在 `Result` 中移动/克隆),`cargo test --all` 无回归佐证。
- **`RequirementKind::accepts` 路径**:该方法对 `NeedInteraction` 已委派给 `request.accepts_response(response)`
  (`requirement.rs` 现有逻辑),故 permission 家族/`action_id` 校验自动经此对齐,无需改动 `accepts` 本体;新增测试
  `accepts_delegates_permission_action_id_check` 确认匹配放行、`action_id` 不符经 `RequirementError::Interaction(
  InteractionError::ActionMismatch)` 拒绝。
- **响应侧 backend/testkit 留待 M4-3**:本任务仅落 response 类型与校验;`ScriptedInteractionHandler`、
  `reference.rs`、端到端 external→permission→respond 由 **M4-3** 追踪(现有 request 侧 `panic!` 臂不受影响,
  它们 match 的是 `InteractionKind`,未新增 `InteractionResponse` 分派点)。
- **新增单测**:`permission.rs` 3 个(response 各 decision round-trip / `deny` 空 reason 归一化 / accessor);
  `interaction.rs` 2 个(`permission_response_family_matches`——含 permission↔非 permission 双向 `ResponseKindMismatch`、
  `permission_response_action_id_mismatch_rejected`),并把 `permission` 请求测试改用共享 helper、把 permission 响应
  纳入 `every_interaction_response_round_trips`;`requirement.rs` 1 个(上述委派测试)。`cargo test --lib permission_response`
  命中 4 个全过。
- **验证结果(完整序列全绿)**:`cargo fmt --all -- --check` 无差异;`cargo clippy --all-targets -- -D warnings` 0 告警;
  `cargo test --lib permission_response` 4 过;`cargo test --all --all-targets` 全绿(lib 467 passed,workspace 全 0 failed;
  credential-gated 集成测试保持 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 0 告警;
  `git diff --check` 干净。

### [DONE] M4-3 interaction backend 与 testkit 的 permission 支持

**前置依赖**:M4-2。

**上下文**:

`ScriptedInteractionHandler`(`crates/agent-testkit/src/handlers.rs` 约 338 行)以反应式决策回应
interaction,提供 `approve_all`/`deny_all`/`sequence`。设计文档 §13 Phase 3 要求 root UI/headless policy
支持 approve/deny/timeout/cancel。参考实现 `ApprovalInteractionHandler`(`src/agent/drive/reference.rs`)。

**做什么**:

- 扩展 `ScriptedInteractionHandler`,使其能对 `InteractionKind::Permission` 产出
  `InteractionResponse::Permission(..)`(至少覆盖 approve / deny / cancel;timeout 可用 deny/cancel 映射表达),
  并保持既有 approval/question/choice 行为不变。
- 若存在参考 headless policy handler,补一条 permission 分派臂,给出 deny-by-default 的安全默认。
- 在 `ExternalAgentMachine`(M3-3 的两段式路径)中,把外部 `PermissionRequested` event / Paused 的
  permission 请求走 `InteractionKind::Permission`(端到端打通 external→permission→respond)。

**验证条件**:

- 新增测试 `external_agent_permission_approve_flow` 与 `external_agent_permission_deny_flow`:scripted
  external handler 返回 permission 型 Paused,scripted interaction handler 分别 approve/deny,断言 machine
  用正确 decision 走 `RespondInteraction` 并最终 Completed / 相应处理。过滤名:`cargo test external_agent_permission`。
- 完整验证序列全绿。

**完成记录**:

- **`ApprovalDecision → PermissionDecision` 映射(两处 backend 共用同一策略)**:approve→`Approve`;
  deny/**timeout**→`Deny { reason: message }`(timeout 折叠为 deny-by-default,与 approval 侧「timeout 归入
  denied tool status」一致);cancel→`Cancel`。approve/cancel 丢弃 message(permission 的 approve/cancel 无 rationale)。
  两 backend 均据请求内 `PermissionRequest::action_id()` 构造 `PermissionResponse`,故 `accepts_response` 的
  `action_id` 校验在 drain 路径(`RequirementKind::accepts` → `Interaction::accepts_response`)天然通过。
- **testkit `ScriptedInteractionHandler`(`crates/agent-testkit/src/handlers.rs`)**:把 `approval_response`
  的 `InteractionKind::Permission` 臂由 `panic!("... milestone 4.3")` 替换为
  `InteractionResponse::Permission(permission_from_approval(request.action_id(), decision, message))`;新增私有
  `permission_from_approval` 实现上表映射。`InteractionDecision` 的 approval 家族(`Approve`/`ApproveWith`/`Deny`/
  `Timeout`/`Cancel`)因此对 permission 请求直接产出 permission 响应;`Answer`/`Choice`/`Response` 行为不变
  (`Response` 仍是显式注入任意 `InteractionResponse` 的逃生口)。更新 `InteractionDecision` 与 `approval_response`
  的 doc。import 增加 `PermissionResponse`。
- **参考 headless handler `ApprovalInteractionHandler`(`src/agent/drive/reference.rs`)**:`fulfill` 的
  `InteractionKind::Permission` 臂由 `panic!` 替换为按 `self.decision` 映射的 `InteractionResponse::Permission`
  (同上映射,deny 携带 `self.message`)。doc 说明 headless 层用 `ApprovalInteractionHandler::deny(..)` 即得
  deny-by-default 的安全默认。import 增加 `PermissionResponse`。
- **`ExternalAgentMachine` 端到端打通**:机器 `pause_for_interaction`/`resume_interaction` 对 `Interaction`/
  `InteractionResponse` 是泛型透传(handler 负责构造 `Interaction`),故 wiring 落在 testkit fixture:升级
  `ExternalAgentFixture::permission_pause()`(`crates/agent-testkit/src/external.rs`)由
  `Interaction::question` 改为 `Interaction::permission`,请求体取自新增 `permission_request()`
  (`action_id="act-1"`、actor=fixture agent、category=Shell、risk=Medium、subject=`{"command":"cargo test"}`),
  与 pause 的 runtime `action_id`("act-1")及 `PermissionRequested` 观测一致。更新 fixture header 与方法 doc
  (删去「milestone 4 才落地」的过渡说明)。这正是 M3-3 作者在 fixture header 里预留的「once it lands,
  `permission_pause` can be upgraded」升级点。
- **测试**:新增 `tests/agent_external_permission.rs` 两个 `#[tokio::test]`——`external_agent_permission_approve_flow`
  与 `external_agent_permission_deny_flow`:scripted external 返回 permission 型 Paused→Completed,scripted
  interaction 分别 `Approve` / `Deny(Some(reason))`,经 `DrainHarness` 驱动到 `LoopCursorKind::Done`;断言外部
  第二次调用是 `RespondInteraction` 且携带正确的 `InteractionResponse::Permission`(approve / deny+reason,
  `action_id="act-1"`)、interaction call log 记录同一响应、approve 流程提交 1 turn。M3-3 集成测试
  `tests/agent_external_interaction.rs` 的两个用例随 fixture 升级由 `InteractionDecision::Answer(..)` 改为
  `InteractionDecision::Approve`(措辞由「clarification」改为「permission request」),继续覆盖两段式
  pause→respond→completed 与 pop-to-outer 路径。question/answer 交互仍由 interaction/machine 层单测覆盖。
- **验证结果(完整序列全绿)**:`cargo fmt --all` 无差异;`cargo clippy --all-targets -- -D warnings` 0 告警;
  `cargo test external_agent_permission` 2 过、`cargo test external_agent_pause` 2 过、`cargo test -p agent-testkit --lib`
  136 过;`cargo test --all --all-targets` 全绿(lib 467 passed,workspace 全 0 failed;credential-gated 集成测试保持
  ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 0 告警;`git diff --check` 干净。

### [DONE] M4-4 Milestone 4 Review

**前置依赖**:M4-1..M4-3。

**上下文**:确认 permission 泛化端到端自洽,且未破坏既有 approval 语义。

**做什么**:

- 核对 `InteractionKind`/`Tag`/`InteractionResponse` 三处新增对齐;`validate_response` 与
  `RequirementKind::accepts` 双重校验一致;deny-by-default 安全默认到位。
- 确认设计文档 §3.3/§14 的 permission 字段集与实现一致,必要时回填文档「最小字段集」结论。

**验证条件**:

- 完整验证序列全绿,`cargo test --all --all-targets` 无回归。
- Review 结论(含最小字段集决策)写入「完成记录」。

**完成记录**:

- **三处新增对齐(`InteractionKind` / `InteractionKindTag` / `InteractionResponse`)**:
  `InteractionKind::Permission { request: PermissionRequest }`、`InteractionResponse::Permission(PermissionResponse)`、
  `InteractionKindTag::Permission` 三者一一对应;`InteractionKind::tag()` 与 `InteractionResponse::tag()` 均含 Permission
  臂并互相映射到同一 tag,`InteractionKindTag` 的 `Display` 渲染 `"permission"`。`Interaction::permission(..)` /
  `InteractionResponse::permission_for(..)` 构造器齐备,serde `snake_case` round-trip 由 interaction.rs / permission.rs
  单测覆盖。**结论:三处新增对齐,无缺失分派臂。**
- **双重校验一致**:driver 边界 `drive::validate` → `RequirementKind::accepts`(`src/agent/requirement.rs:448`)先做
  按族 tag 校验(`ResultKindMismatch`),再对 `NeedInteraction` 委派 `Interaction::accepts_response`
  (`src/agent/interaction.rs:126`)。后者对 `Permission` 请求校验响应族匹配(`ResponseKindMismatch`)且
  `PermissionResponse::action_id() == PermissionRequest::action_id()`(否则 `ActionMismatch`)。两层校验路径互不重复、
  语义一致:type 层归 requirement,family/`action_id` 层归 interaction;`requirement.rs` 的
  `accepts_delegates_permission_action_id_check` 与 interaction 层
  `permission_response_action_id_mismatch_rejected` 双向覆盖。**结论:双重校验一致,无缝隙。**
- **deny-by-default 安全默认到位**:两处 backend 共用同一 `ApprovalDecision → PermissionDecision` 映射——
  Approve→`approve`;Deny/**Timeout**→`deny{reason:message}`;Cancel→`cancel`。timeout 折叠为 deny(与 tool approval
  侧「timeout 归入 denied tool status」一致)。testkit `permission_from_approval`
  (`crates/agent-testkit/src/handlers.rs:329`)与参考 headless `ApprovalInteractionHandler::fulfill` 的 Permission 臂
  (`src/agent/drive/reference.rs:267`)映射完全一致;headless 层用 `ApprovalInteractionHandler::deny(..)` 即得
  deny-by-default。中间 external scope 不挂 interaction handler 时权限请求按 pop 规则升到外层策略。**结论:安全默认到位。**
- **文档 §3.3/§14 与实现一致 + 最小字段集结论回填**:§3.3 的 `PermissionRequest` 七字段
  (`action_id`/`actor`/`category`/`summary`/`subject`/`risk`/`reason`)、`PermissionCategory`(7 variant)与实现
  逐字一致;实现另有 `PermissionRisk`(Low<Medium<High<Critical)。原 §3.3 只给了请求侧、未给审批结果 shape,§14
  仍把「`InteractionKind::Permission` 最小字段集与审批结果 shape」列为未定问题——本次**回填**:在 §3.3 增补审批结果
  shape(`InteractionResponse::Permission(PermissionResponse)` + `PermissionDecision { Approve, Deny{reason}, Cancel }`,
  刻意不设独立 Timeout 变体)与「最小字段集(Milestone 4 收敛)」结论,并在 §14 将该未定问题标注为「已定(Milestone 4)」
  指回 §3.3。§5.1 关于 `action_id`「落地后仍是规范句柄」的前瞻说明已与实现相符,无需改动。
- **验证结果(完整序列全绿)**:`cargo fmt --all -- --check` 无差异;`cargo clippy --all-targets -- -D warnings` 0 告警;
  `cargo test external_agent_permission` 2 过;`cargo test --all --all-targets` 全绿(lib 467 passed、agent-testkit 136
  passed、全部集成测试 0 failed;credential-gated 集成测试保持 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc
  --no-deps --workspace` 0 告警;`git diff --check` 干净。**本任务仅改动设计文档与 TODO/PLAN 记录,未改编译代码,
  故全量结果与 M4-3(commit 3e82f1c)一致,此处重跑复核仍全绿。**
- **Milestone 4 review 结论**:permission 泛化端到端自洽(external→`InteractionKind::Permission`→interaction
  handler→`RespondInteraction` 回喂,由 `tests/agent_external_permission.rs` approve/deny 两流覆盖),既有
  approval/question/choice 语义未受影响(`DefaultAgentMachine` 从不 emit permission;legacy approval round-trip 保持
  lossless)。Milestone 4 签核通过。


---

## Milestone 5 — Event sink 与 artifact(Phase 4)

### [DONE] M5-1 新增 `Notification::ExternalAgent` 变体

**前置依赖**:M4-4。

**上下文**:

`src/agent/event.rs`:`Notification { Llm(StreamEvent), StepBoundary(StepBoundary),
ToolCallStarted(..), ToolCallFinished(..) }`。设计文档 §5.3 提议新增
`Notification::ExternalAgent(ExternalAgentEvent)`。`ExternalAgentEvent` 已在 M2-1 定义。

**做什么**:

- 给 `Notification` 增加 `ExternalAgent(ExternalAgentEvent)` 变体,更新所有穷尽 `match`(编译器指认)。
- 保持既有变体 serde tag 不变。

**验证条件**:

- 新增单测:`Notification::ExternalAgent` serde round-trip。过滤名:`cargo test --lib notification_external`。
- `cargo test --all --all-targets` 确认无回归(尤其 `assertions/notifications.rs` 相关)。
- 完整验证序列全绿。

**完成记录**:

- `src/agent/event.rs`:`Notification` 新增 `ExternalAgent(ExternalAgentEvent)` 变体(带 rustdoc,说明其
  为 §5.3/§5.5 的可跳过 observe-only 通知),并在文件头 `use crate::agent::{..}` 导入 `ExternalAgentEvent`。
  既有 4 变体的 serde tag(`llm`/`step_boundary`/`tool_call_started`/`tool_call_finished`)保持不变;新变体
  在 `#[serde(tag="type", content="data", rename_all="snake_case")]` 下编码为
  `{"type":"external_agent","data":<ExternalAgentEvent>}`。
- 穷尽 `match` 更新:`crates/agent-testkit/src/assertions/notifications.rs` 的 `describe()` 补
  `Notification::ExternalAgent(event) => "external_agent({event:?})"` 臂(仅此一处穷尽 match;其余为
  `matches!`/`let-else`,无需改)。
- 新增单测 `notification_external_agent_round_trips_and_keeps_wire_tag`(名含 `notification_external`):
  对 `SessionStarted/TextDelta/PermissionRequested/SessionCompleted` 断言 `type=="external_agent"`、
  `data` 等于 `ExternalAgentEvent` 直接序列化,并 round-trip 相等。
- 验证(默认完整序列全绿):`cargo fmt --all -- --check` 无差异;`cargo test --lib notification_external`
  1 passed;`cargo clippy --all-targets -- -D warnings` 0 告警;`cargo test --all --all-targets` 全绿
  (lib 468、testkit 136,集成 0 failed);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  0 告警;`git diff --check` 干净。
- 说明:observation→notification 的实际转换与去重属 M5-2;`machine.rs` 中 “conversion … lands in M5”
  的注释仍准确,故不改动。

### [DONE] M5-2 observation 缓冲 → notification 转换与 event sink

**前置依赖**:M5-1。

**上下文**:

设计文档 §5.5:handler 把决策点之前累积的 event 放进 `ExternalSessionResult.observations`,machine 在
resume 后一次性把它们转成 `Notification::ExternalAgent` 吐出(经 `StepOutcome.notifications`);真正实时的
旁路可选,不阻塞 continuation。`StepOutcome`(`src/agent/machine/mod.rs`)的 `notifications` 字段由 driver
透传。

**做什么**:

- 在 `ExternalAgentMachine` 的 `Resume(ExternalSession(..))` 分支中,把结果里的 `observations:
  Vec<ExternalAgentEvent>` 依序映射为 `Notification::ExternalAgent`,放入本步 `StepOutcome.notifications`。
- 用 `ExternalSessionRef.last_event_seq` 对齐游标,避免 resume 时重复回放已消费的 event(§5.5)。
- (可选实时旁路)定义一个 `ExternalEventSink` trait 或复用 `RunContext` trace emitter 作为非阻塞旁路的
  接口占位,rustdoc 说明其可丢弃、不阻塞 continuation;首版可不接真实实时源。

**验证条件**:

- 新增测试 `external_agent_emits_observation_notifications`:scripted handler 的 Completed 带若干
  observations,断言 drain 后 `Notification::ExternalAgent` 按序出现且数量正确;再断言重复 resume 不重放
  已消费 event(用 `last_event_seq`)。过滤名:`cargo test external_agent_emits`。
- 完整验证序列全绿。

**完成记录**:

- `src/agent/external/machine.rs`:`fold_session_result` 不再丢弃 observations。三个决策点变体
  (`Completed`/`PausedForInteraction`/`Failed`)在**调用 `set_session(new)` 之前**读取旧
  `session().last_event_seq` 作为“已消费游标”,经新增私有 helper `observe(incoming_seq, observations)`
  把 `Vec<ExternalAgentEvent>` 依序映射为 `Notification::ExternalAgent`,放入本步 `StepOutcome.notifications`。
  三个 transition(`complete_session` / `pause_for_interaction` / 新增 `fail_with`)改为接收并透传该
  notifications 向量;`fail` 改为委托 `fail_with(msg, Vec::new())`。
- 去重语义(§5.5):`observe` 读取 `state.session().last_event_seq` 作为上次消费的 seq;当本次 result 报告
  的 `last_event_seq` 与已消费 seq 均为 `Some` 且 `incoming <= consumed` 时,判定这批 observations 已消费,
  发 0 条;否则全部按序发出。任一 seq 缺失(`None`)则无法对齐,按 as-is 全量发出。因 seq 已随
  `set_session` 持久化进 `ExternalAgentState`,无需新增字段。
- `src/agent/external/sink.rs`(新增模块):定义可丢弃、非阻塞的实时旁路占位 `ExternalEventSink` trait
  (`emit(&self, &ExternalAgentEvent)`,`Send + Sync`)与无操作实现 `DiscardEventSink`,rustdoc 说明其
  只旁路、可跳过、只有 `Requirement` 才阻塞 continuation、事件为 untrusted;含一个 doc 示例与一个单测。
  经 `external::{DiscardEventSink, ExternalEventSink}` 与 `crate::agent` 再导出。刻意不接进 sans-io 的
  `step`——sink 属 handler 层。
- 文档:更新 `machine.rs` 模块头与 `fold_session_result` 注释(不再说 observations “dropped / lands in M5”),
  改述为“转成 `Notification::ExternalAgent` 并按 `last_event_seq` 去重”。
- 新增单测 `external_agent_emits_observation_notifications`(`src/agent/external/machine/tests.rs`,过滤名含
  `external_agent_emits`):(a) Completed 带三条 observations → drain 后 `Notification::ExternalAgent` 按序、
  数量正确;(b) pause↔respond 循环里,首个 pause(seq=2)发出其 observations,重复投递同一 seq=2 的 pause
  不再回放,随后 Completed(seq=4)重新按序发出新 observations。配套 helper:`session_ref_seq`、
  `observation_batch`、`completed_with`、`paused_with`、`external_events`。
- 验证(默认完整序列全绿):`cargo fmt --all -- --check` 无差异;`cargo test external_agent_emits`
  1 passed;`cargo clippy --all-targets -- -D warnings` 0 告警;`cargo test --all --all-targets` 全绿
  (lib 470、testkit 136,集成 0 failed,仅 credential-gated ignored);`RUSTDOCFLAGS="-D warnings"
  cargo doc --no-deps --workspace` 0 告警;`git diff --check` 干净。
- 说明:artifact ref 记录(`ExternalAgentOutput.artifacts` → state/trace)属 M5-3,本任务未触及。

### [DONE] M5-3 artifact ref 记录(patch / diff / test result)

**前置依赖**:M5-2。

**上下文**:

设计文档 §3.4/§11:`report_artifact` 把 diff、patch、测试结果、文件路径记录为 artifact/notification;
`ExternalAgentOutput.artifacts: Vec<ExternalArtifactRef>`(M2-1)承载 artifact 引用。`FilePatch` 等 event
已在 `ExternalAgentEvent`。

**做什么**:

- 落实 `ExternalArtifactRef` 的字段(如 `kind: {Patch, Diff, TestResult, FilePath, Other}`、`path/ref`、
  `summary`),并在 machine 完成时把 `ExternalAgentOutput.artifacts` 记录到 state / trace(不写实际文件内容,
  只记引用与摘要,符合 §12 的 redaction 原则)。
- 若需要,提供一个把 `FilePatch` event 归集为 artifact 的映射 helper。

**验证条件**:

- 新增测试 `external_agent_records_artifacts`:Completed 带 artifacts,断言 state/trace 记录了正确的
  artifact ref 且不含敏感原文。过滤名:`cargo test external_agent_records_artifacts`。
- 完整验证序列全绿。

**完成记录**:

- `ExternalArtifactRef` 字段已在 M2-1 就绪(`kind: ExternalArtifactKind{Patch,Diff,TestResult,File,Other}`、
  `summary`、`path`、`reference`),本任务复用并未重命名(设计里 `FilePath` 与既有 `File` 等义,保持 `File`
  以免破坏已导出 API)。
- `src/agent/external/state.rs`:`ExternalAgentState` 新增 `artifacts: Vec<ExternalArtifactRef>` 字段,
  作为可持久化 trace(本库无独立 trace 对象,state 即持久化 trace,notification 流为实时 trace)。新增只读
  访问器 `artifacts(&self) -> &[ExternalArtifactRef]` 与追加式 `record_artifacts<I: IntoIterator<Item=
  ExternalArtifactRef>>(&mut self, I)`(均带 rustdoc,强调只存引用/摘要、不落原文,符合 §11/§12)。
  `ExternalAgentStateRecord` 加同名字段,`#[serde(default, skip_serializing_if = "Vec::is_empty")]` 保持
  空列表时快照 byte-for-byte 向后兼容;`new`/`from_record`/`Serialize` 同步初始化与透传。
- `src/agent/external/machine.rs`:`complete_session` 在建好 assistant response 后、commit turn 前调用
  `self.state.record_artifacts(output.artifacts)`(move,不 clone),把完成态输出的 artifacts 折进 state。
  仅 `Completed` 携带 `output`,故 artifacts 只在完成时记录;`Failed` 无 output 不记录。更新模块头 `Completed`
  条目与 `complete_session` rustdoc,说明记录的是 redacted ref(§11/§12)。
- `src/agent/external/mod.rs`:新增 `impl ExternalArtifactRef { pub fn from_file_patch(&ExternalAgentEvent)
  -> Option<Self> }`(`FilePatch{path,summary,diff_ref}` → `kind=Patch`、`path=Some(path)`、
  `reference=diff_ref`;非 FilePatch 返回 `None`)与自由函数 `collect_file_patch_artifacts(&[
  ExternalAgentEvent]) -> Vec<ExternalArtifactRef>`(按序过滤映射 FilePatch),经 `external::*` 与
  `crate::agent::*` 再导出;两者 rustdoc 强调只产引用、不含 diff 原文。
- 新增/覆盖测试:
  - `src/agent/external/machine/tests.rs`:`external_agent_records_artifacts`(过滤名匹配)——Completed 带
    patch+test-result 两条 artifacts → `state().artifacts()` 顺序/内容正确;断言 `reference` 均为 `blob://`
    opaque handle(非内联原文);state serde round-trip 后 artifacts 不变。另加
    `external_agent_records_no_artifacts_when_output_reports_none`——空列表不记录且快照省略 `artifacts` 键。
    配套 helper `completed_with_artifacts` / `sample_artifacts`。
  - `src/agent/external/mod.rs` tests:`file_patch_event_maps_to_patch_artifact_ref`(含无 diff_ref 与
    非 FilePatch 分支)、`collect_file_patch_artifacts_keeps_only_patches_in_order`(顺序保留、空输入)。
  - `src/agent/external/state.rs` tests:`recorded_artifacts_accumulate_and_round_trip_and_skip_when_empty`
    ——多次 `record_artifacts` 累积保序、空列表快照省略、round-trip 相等。
- 验证(默认完整序列全绿):`cargo fmt --all -- --check` 无差异;`cargo test --lib -- external_agent_records
  file_patch collect_file_patch recorded_artifacts` 全 passed;`cargo clippy --all-targets -- -D warnings`
  0 告警;`cargo test --all --all-targets` 全绿(合计 694 passed、0 failed,含 lib/testkit/集成,
  credential-gated 保持 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 0 告警;
  `git diff --check` 干净。
- 说明:artifact 只记 ref/summary、不落原文,`from_file_patch`/`collect_file_patch_artifacts` 作为 handler
  可选归集 helper 暴露(machine 只自动记录 `output.artifacts`,保持 sans-io 单一职责)。Milestone 5 review
  属 M5-4。

### [DONE] M5-4 Milestone 5 Review

**前置依赖**:M5-1..M5-3。

**上下文**:确认 event/artifact 通道与 §5.5/§11/§12 一致,阻塞 requirement 与非阻塞旁路边界清晰。

**做什么**:

- 核对 observation→notification 顺序与去重;artifact 记录不落敏感原文;实时旁路接口不阻塞 continuation。
- 确认 `assertions/notifications.rs` 能覆盖新的 `ExternalAgent` notification 断言。

**验证条件**:

- 完整验证序列全绿,`cargo test --all --all-targets` 无回归。
- Review 结论写入「完成记录」。

**完成记录**:

- **observation→notification 顺序与去重(§5.5)——通过。** `fold_session_result`
  (`src/agent/external/machine.rs:307`)的三个决策点(`Completed`/`PausedForInteraction`/`Failed`)都在
  **调用 `set_session` 之前**读取 `observe(incoming_seq, observations)`,故对齐游标读的是旧的已消费 seq。
  `observe`(machine.rs:355):空 observations → 0 条;当本次 `incoming` 与已消费 `consumed` 均为 `Some`
  且 `incoming <= consumed` 时判定已消费,发 0 条(去重);否则 `into_iter().map(Notification::ExternalAgent)`
  **按序全发**(顺序保持);任一 seq 缺失(`None`)则无法对齐,按 as-is 全发。`Failed` 用
  `session.as_ref().and_then(|s| s.last_event_seq)` 取游标,仅 `Some` 时 `set_session`。**结论:顺序保持、
  去重语义正确,replay 同一决策点不双发。**
- **artifact 记录不落敏感原文(§11/§12)——通过。** `complete_session`(machine.rs:487)在建好 assistant
  response 后 `record_artifacts(output.artifacts)`(move,不 clone);state 仅存
  `ExternalArtifactRef {kind, summary, path, reference}`,`reference` 为 opaque handle(如 `blob://…`),
  从不内联 diff/log/blob 原文;`ExternalAgentStateRecord` 对空列表 `skip_serializing_if` 保持快照向后兼容。
  仅 `Completed` 携带 `output`,故 artifacts 只在完成时记录,`Failed` 无 output 不记录。归集 helper
  `ExternalArtifactRef::from_file_patch` / `collect_file_patch_artifacts`(`src/agent/external/mod.rs`)同样只产
  引用、不含 diff 原文。**结论:记录的是 redacted ref,不落原文。**
- **实时旁路不阻塞 continuation(sink.rs)——通过。** `ExternalEventSink::emit(&self, &ExternalAgentEvent)`
  + `DiscardEventSink` no-op(`src/agent/external/sink.rs`);rustdoc 明确该旁路可丢弃、必须立即返回、不得
  back-pressure、可整条跳过、事件为 untrusted,并**刻意不接进 sans-io 的 `step`**——唯一能阻塞 continuation
  的是 `Requirement`,sink 属 handler 层的侧信道。§11 的双路径(`Notification::ExternalAgent` 可跳过 vs
  `Requirement` 必须 resolve/abandon)与实现一致。**结论:旁路非阻塞、边界清晰。**
- **`assertions/notifications.rs` 覆盖 `ExternalAgent` 断言——发现缺口并按 class-wide 原则闭合。**
  复核发现 `NotificationAssertions` 对每个既有 family 都提供 `*_count` 断言与访问器
  (`llm_count`/`step_boundary_count`/`step_boundary_steps`/`tool_started_count`+`tool_started_calls`/
  `tool_finished_count`+`tool_finished_calls`),**唯独 `ExternalAgent` 只有 `describe()` 的诊断渲染、没有可断言的
  count/accessor**(machine 层测试因此只能手写本地 `external_events` matcher)。为使「assertions 能覆盖
  `ExternalAgent` 断言」成立,补齐同族对称 API:新增 `external_agent_count(expected)` 断言(复用
  `family_count`,标签 `"external agent"`)与访问器 `external_agent_events(self) -> Vec<&ExternalAgentEvent>`
  (stream order,`filter_map`);文件头导入 `ExternalAgentEvent`(经 `agent_lib::agent` re-export)。两者带
  rustdoc,说明其为 §5.5 的 observe-only 事件。新增两个单测:
  `external_agent_family_is_counted_and_accessible_in_order`(混合流中只计/取本族且保序)、
  `external_agent_count_mismatch_lists_stream`(count 不匹配时 panic 且诊断含 `external agent … found 1` 与
  `external_agent(` 流摘要)。
- **穷尽 `match` 复核**:全库对 `Notification` 的唯一穷尽 `match` 仍是 testkit `describe()`
  (notifications.rs),已含 `Notification::ExternalAgent(event) => "external_agent({event:?})"` 臂;其余均为
  `find_map`/`filter_map`(带 `_ => None`)或 `matches!`,无遗漏分派臂。M5-1 的记录仍准确。
- **验证(完整序列全绿)**:`cargo fmt --all -- --check` 无差异;`cargo clippy --all-targets -- -D warnings`
  0 告警;`cargo test -p agent-testkit --lib notification` 4 passed(含 2 个新增);
  `cargo test --all --all-targets` 全绿(lib 475、agent-testkit 138,共 34 个测试二进制 0 failed,
  credential-gated 集成测试保持 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  0 告警;`git diff --check` 干净。
- **Milestone 5 review 结论**:event 通道(`observations` → `Notification::ExternalAgent`,按 `last_event_seq`
  exact-once 去重)与 artifact 通道(`output.artifacts` → `ExternalAgentState` 的 redacted ref 持久化 trace)
  均与设计 §5.5/§11/§12 一致;阻塞式 `Requirement` 与非阻塞 `ExternalEventSink` 旁路边界清晰、互不污染;
  testkit 现已对 `ExternalAgent` notification 提供与其他 family 对称的断言能力。**Milestone 5 签核通过。**

---

## Milestone 6 — Mixed-agent scheduler(Phase 5)

### [DONE] M6-1 `WorkerProfileRef`、worker profile registry 与 `WorktreeIsolation`

**前置依赖**:M5-4。

**上下文**:

设计文档 §4.1/§9/§10:`WorkerProfileRef` 承载 worker 能力标签、成本档位与升级规则;`WorktreeIsolation`
描述隔离级别(M2-1 已定义枚举)。§8 的 worker 集合含 internal-cheap / deepseek / cc / cx / opencode / review。
`ExternalAgentSpec`(M3-1)当前 profile 为占位。

**做什么**:

- 定义 `WorkerProfile { id, capabilities: Vec<Capability标签>, cost_tier, escalation: EscalationRules }`
  与 `WorkerProfileRef`,以及一个内存 `WorkerProfileRegistry`(注册/按 ref 解析)。
- 把 `ExternalAgentSpec.profile` 从占位改为 `WorkerProfileRef`。
- 明确 `WorktreeIsolation` 各级的语义与默认(强 worker 默认独立 worktree,见 §10)。

**验证条件**:

- 新增单测:registry 注册/解析、profile serde round-trip、`WorktreeIsolation` 默认策略断言。过滤名:
  `cargo test --lib worker_profile`。
- 完整验证序列全绿。

**完成记录**:

- **新增 `src/agent/external/profile.rs`**(mixed-agent scheduler 的 worker 画像数据层):
  - `Capability`(Search/Shell/Test/BugFix/Feature/Refactor/Review/Debug/CodeGeneration/Planning +
    `Custom(String)` 逃生舱,对齐 §9 任务类型)。
  - `CostTier`(Cheap < Standard < Premium,`Ord`,`Default = Cheap`;`recommended_isolation()` 与
    `is_stronger_than()`)。
  - `EscalationTrigger`(Timeout/TestFailure/LowConfidence/ReviewRejected/BudgetExhausted,§9 升级规则)
    与 `EscalationRules { triggers, escalate_to: Option<WorkerProfileRef>, human_fallback }`(数据层,
    由 M6-4 解释;`none()`/`triggers_on()` 等访问器)。
  - `WorkerProfileRef`(轻量 transparent String id,从 `spec.rs` 移来)。
  - `WorkerProfile { id, capabilities, cost_tier, escalation }`,严格对齐任务字段;隔离不落字段,
    由 `cost_tier` 派生(`recommended_isolation()`);`reference()`/`has_capability()`。
  - `WorkerProfileRegistry`(内存 `BTreeMap`):`register`(同 id 覆盖并返回 ref)/`resolve`/`get`/
    `contains`/`len`/`is_empty`/`iter`。
- **`ExternalAgentSpec.profile` 去占位**:`external/spec.rs` 删除本地占位 `WorkerProfileRef`,改从
  `external::profile` 引入真实 registry-backed ref;更新 rustdoc,去掉「placeholder / reserved」措辞。
- **`WorktreeIsolation` 语义与默认**:`external/mod.rs` 新增 `impl Default`(= `PerAgentWorktree`,§10
  「默认隔离」);成本档位推荐隔离 Cheap→Shared / Standard→PerAgentWorktree / Premium→
  EphemeralGitWorktree(强 worker 默认独立临时 worktree)。`docs/external-agent.md` §10 记录该收敛。
- **re-export**:`external/mod.rs` 与 `src/agent/mod.rs` 导出 `Capability`/`CostTier`/`EscalationRules`/
  `EscalationTrigger`/`WorkerProfile`/`WorkerProfileRef`/`WorkerProfileRegistry`。所有新公开 API 带 rustdoc。
- **测试**:`profile.rs` 内 7 个单测(名字含 `worker_profile`):registry 注册+解析、未知 ref 返回 None、
  同 id 覆盖、profile serde round-trip(含 `Custom` capability / 终态 escalation 省略)、
  `WorktreeIsolation`/`CostTier` 默认与推荐隔离策略断言、`CostTier` 排序。未改动 machine/state 运行语义。
- **验证(完整序列全绿)**:`cargo fmt --all` ✓;`cargo test --lib worker_profile` = 7 passed ✓;
  `cargo clippy --all-targets -- -D warnings` 无告警 ✓;`cargo test --all --all-targets` 无回归
  (lib 482 passed,集成全绿,仅 credential-gated ignored)✓;`RUSTDOCFLAGS="-D warnings" cargo doc
  --no-deps --workspace` 无告警 ✓;`git diff --check` 干净 ✓。

### [DONE] M6-2 task evaluator 与 dispatcher

**前置依赖**:M6-1。

**上下文**:

设计文档 §9:两层调度——规则路由(确定性、低成本)先处理明显任务,LLM evaluator 只在模糊/高风险任务上调用。
评估维度与示例策略见 §9 表。dispatcher 依据 `WorkerProfile`(M6-1)与 `RunContext` 预算(`charge_*`、
`check_*`,见 `src/agent/context.rs`)选择 worker。

**做什么**:

- 定义 `TaskDescriptor`(任务类型/影响范围/风险/不确定性/预算等维度)与 `WorkerChoice`。
- 实现 `RuleRouter`(确定性映射)与一个 `Dispatcher`:先跑规则路由,未决则回退到一个可插拔的
  `Evaluator` trait(LLM 版实现留接口,测试用 scripted evaluator)。
- dispatcher 输出为「派生哪个 worker 的 `NeedSubagent`(spec_ref)」,复用现有 subagent 派生路径,不新造
  orchestration runtime。

**验证条件**:

- 新增测试:规则路由命中(明确只读 shell → cheap worker)、回退 evaluator(模糊任务 → scripted evaluator
  决策)、预算接近上限时降级。过滤名:`cargo test dispatcher`。
- 完整验证序列全绿。

**完成记录**:

- **新增 `src/agent/external/dispatch.rs`**(mixed-agent 两层调度器,re-export 到 `agent::external` 与
  `agent`),内含调度维度、roster、router、evaluator、dispatcher 及错误类型:
  - 维度枚举(serde;Ord where 有意义):`ImpactScope`(SingleFile<MultiFile<CrossModule<Architectural)、
    `Uncertainty`(Clear<Exploratory<Ambiguous)、`CostPreference`(Balanced 默认 /CostFirst/SpeedFirst/
    QualityFirst);风险维度**复用** `PermissionRisk`,任务类型**复用** `Capability`(不重复造)。
  - `TaskDescriptor { task_type, impact, risk, uncertainty, preference }`(serde data,`new`/
    `with_preference`/accessors);预算不落 descriptor,由 dispatcher 在派发时读 `RunContext`。
  - `Worker { profile: WorkerProfileRef, spec: AgentSpecRef }` 把调度 profile 绑定到要派生的子 agent spec;
    `WorkerRoster`(拥有 `WorkerProfileRegistry` + worker 列表):`register`(同 id 覆盖 profile 与 spec
    绑定)、`resolve_worker`/`profile`、`cheapest_capable`/`strongest_capable`(按 `CostTier` 选,tie-break
    profile id 升序,结果确定)。
  - `RuleRouter`(确定性、有序、first-match):ambiguous→None(交 evaluator);architectural / `risk>=High` /
    quality-first 跨模块→strongest_capable;clear&low-risk&`<=MultiFile` 或 cost-first 非高风险→
    cheapest_capable;其余中间地带→None。
  - `TaskEvaluator` trait(`evaluate(task,roster)->Option<WorkerProfileRef>`;LLM 版实现此 trait,留接口)+
    `ScriptedTaskEvaluator`(closure-based,`new`/`always`;测试与宿主固定策略用)。
  - `Dispatcher<E: TaskEvaluator>`(`new`/`with_router`/`with_budget_headroom`,默认 headroom 20%):
    `dispatch(task,roster,ctx)` 顺序 = check_cancelled → 预算低(`budget_is_low` 用 snapshot 计算剩余额度,
    check_* 护栏)则降级 cheapest(`BudgetDowngrade`)→ 规则路由命中则 `RuleRoute` → 否则 `charge_step`
    计 evaluator 成本(charge_*;若 charge 触及预算上限则同样降级)→ evaluator 决策 `Evaluator`。
  - `WorkerChoice { worker, spec, reason: DispatchReason(RuleRoute/Evaluator/BudgetDowngrade) }` +
    `into_subagent(brief, result_schema) -> RequirementKind::NeedSubagent`——复用既有 `SubagentHandler`
    派生路径,**不**新造 orchestration runtime。
  - `DispatchError`(thiserror):`NoCapableWorker{capability}` / `NoWorker` / `UnknownWorker{worker}` /
    `Context(RunContextError)`。
- **模块接线**:`external/mod.rs` `mod dispatch;` + `pub use dispatch::{...}`;`src/agent/mod.rs` 在
  `pub use external::{...}` 追加同名导出。所有新公开 API 带 rustdoc。
- **测试**:`src/agent/external/dispatch/tests.rs` 15 个单测(名字含 `dispatcher`):规则路由命中 cheap
  (明确只读 shell)、回退 scripted evaluator(模糊 debug→strong,断言计了 1 step)、预算接近上限降级、
  evaluator charge 触上限降级、architectural→strong、cost-first→cheap、`into_subagent` 生成 NeedSubagent、
  未注册 worker→UnknownWorker、无 capable→NoCapableWorker、evaluator decline→NoWorker、cancel→Context、
  `TaskDescriptor` serde round-trip、`budget_is_low` 阈值、roster 同 id 覆盖、router defer ambiguous。
- **文档**:`docs/external-agent.md` §9 追加「收敛(Milestone 6-2 已实现)」段落,记录两层调度落点与预算护栏。
- **验证(完整序列全绿)**:`cargo fmt --all` ✓;`cargo test dispatcher` = 15 passed ✓;
  `cargo clippy --all-targets -- -D warnings` 无告警(修正 type_complexity → `EvaluatorFn` 别名、
  clone_on_copy → `*entry.spec()`)✓;`cargo test --all --all-targets` 无回归(lib 497 passed = 482+15,
  集成/testkit 全绿,仅 credential-gated ignored)✓;`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
  --workspace` 无告警 ✓;`git diff --check` 干净 ✓。

### [DONE] M6-3 `spawn_agent` / plan / blackboard / mailbox 工具 adapter

**前置依赖**:M6-2。

**上下文**:

设计文档 §3.4/§3.5:注入给外部 agent 的工具是宿主能力的薄 adapter,不绕过 `RunContext` 护栏——`spawn_agent`
转成 `NeedSubagent`(由 `SubagentHandler` 派生),`plan_*`/`blackboard_*`/`send_message` 操作本库 plan/
blackboard/mailbox 原语。plan/blackboard 的 mock vertical feature 与语义已在归档计划实现(见
`docs/archive/2026-07-16-complex-tests/` 及 `tests/complex_support/`),`docs/agent-layer.md` §6.2 给出 plan
API 语义(dependency、claim 前置完成检查、claim-first、CAS 更新)。`Tool`/`ToolCall` 见 `src/agent/tool.rs`。

**做什么**:

- 实现桥接工具 adapter:`spawn_agent`(→ 结构化请求 → machine emit `NeedSubagent`)、
  `plan_claim`/`plan_claim_first_available`/`plan_update`、`blackboard_post`/`blackboard_read`、
  `send_message`、`report_artifact`、`run_host_tool`。
- adapter 必须经宿主 policy/护栏,不直接写外部 runtime 私有 mailbox;claim 必须检查依赖已完成
  (对齐 `docs/agent-layer.md` §6.2)。
- 提供把这些工具注入 external agent `initial_tools` 的构造入口。

**验证条件**:

- 新增测试:`spawn_agent` adapter 产生 `NeedSubagent` 并经 `SubagentHandler` 派生;`plan_claim` 在未完成
  依赖时被拒;`blackboard_post`/`read` append-only 且偏移单调。过滤名:`cargo test tool_adapter`。
- 完整验证序列全绿。

**完成记录**:

- 新增 `src/agent/collab/` 模块,把 plan/blackboard/mailbox 提升为一等 API-first 库原语,再在其上提供薄桥接
  工具 adapter(设计 §3.4/§3.5、`agent-layer.md` §5/§6.2–§6.4):
  - `collab/plan.rs`:`Plan`(`Mutex<PlanSnapshot>`,可 `Arc` 共享)。`claim` = 版本 CAS + owner 检查 +
    合法状态转换 + 全部依赖 `Completed` 的原子检查,任一检查失败不改变 owner/status/version;另有
    `claim_first_available`(稳定顺序跳过已完成/已认领/被依赖阻塞的任务)、`update_status`(owner + 合法转换)、
    `add_task`(重复/自依赖/未知依赖/防御性循环检查)。`TaskStatus` 五态 + `can_transition_to` 终态粘滞。
  - `collab/blackboard.rs`:`Blackboard` 按 channel 命名空间的 append-only 日志,每 channel 零基单调 offset
    作为逻辑时钟(刻意不加墙钟时间以保持确定性)。
  - `collab/mailbox.rs`:`Mailbox` 全局单调 `seq` 的定向消息层(设计 §3.5,走本库协议而非外部 runtime 私有
    inbox)。
  - `collab/tools.rs`:工具名常量、`bridge_tool_declarations` / `bridge_tool_set`(注入 `initial_tools` 入口)、
    `CollabToolHandler`(实现 `ToolHandler`:先查 `RunContext` 取消护栏,再用**注入的** agent identity 作为
    claim/post/send 的 owner/sender——model 不能伪造)、`report_artifact` → `ArtifactSink`、`run_host_tool` →
    可选宿主 `ToolRegistry`。`spawn_agent` 因加深 scope 链不作内联执行:`SpawnAgentRequest::parse` →
    `into_requirement_kind` 翻译成 `RequirementKind::NeedSubagent`,复用既有 `SubagentHandler` 派生路径;
    内联误路由会返回 `ExecutionFailed` 护栏错误。
- 在 `src/agent/mod.rs` 挂载 `pub mod collab;` 并在 agent 层 re-export 主要类型。
- 测试:`src/agent/collab/tests.rs` 24 个单元测试 + `tests/agent_tool_adapter.rs` 2 个集成测试,全部以
  `tool_adapter` 命名,覆盖三条必需验证——(1)`spawn_agent` 产生 `NeedSubagent` 且经真实
  `DrivingSubagentHandler` 派生并驱动 child 到完成(集成测试);(2)`plan_claim` 依赖未完成时被拒且不改变
  plan;(3)`blackboard` post/read append-only 且 offset 单调——外加 CAS 冲突、状态转换、claim-first、mailbox
  定向投递、`report_artifact`、`run_host_tool`(有/无宿主)、取消护栏、未知工具、声明覆盖、serde 往返等。
- 文档:`docs/external-agent.md` §3.4/§3.5 增补「已实现(M6-3)」说明;`README.md` 模块概览补充
  `agent::collab`。
- 验证序列全绿:`cargo fmt --all`;`cargo clippy --all-targets -- -D warnings` 干净;`cargo test tool_adapter`
  26 通过;`cargo test --all --all-targets` 全绿(lib 521 通过,其余 target 全通过);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 干净。

### [DONE] M6-4 cheap → strong 升级与 verifier

**前置依赖**:M6-3。

**上下文**:

设计文档 §9 升级规则:cheap worker 超时/测试失败 → strong worker;worker 自报低置信度 → evaluator 重新
分派;review 发现架构/安全问题 → cc/cx 或 human;budget 接近上限 → 降级或停机问用户。

**做什么**:

- 在 dispatcher 之上实现 escalation:根据 worker 结果(失败/低置信度/超预算)触发重新分派到更强 worker,
  或降级/升到 human(经 `InteractionKind::Permission` 或 `Question`)。
- 提供一个 `verifier` 挂点(review-agent / tests),在高风险或复杂任务后运行并驱动升级判断。

**验证条件**:

- 新增测试:cheap worker 失败触发 strong worker 重派;超预算触发降级/停机问用户;verifier 失败触发升级。
  过滤名:`cargo test escalation`。
- 完整验证序列全绿。

**完成记录**:

- **新增 `src/agent/external/escalation.rs`**(升级规则 + verifier 挂点,叠加在 M6-2 dispatcher 原语之上,
  re-export 到 `agent::external` 与 `agent`),**不**新造 orchestration runtime:
  - `WorkerReport { worker: WorkerProfileRef, triggers: Vec<EscalationTrigger> }`(serde;`succeeded`/
    `failed`/`new`/`with_trigger`/`worker`/`triggers`/`is_clean`/`raised`;trigger 复用 M6-1 profile 枚举,
    de-dup 保序;干净 report 省略空 triggers)记录一次 worker 运行结果。
  - `Verifier` trait(`verify(task, report) -> Option<EscalationTrigger>`;None=通过)= 设计 §9「验证/升级」
    的验证半(review-agent / tests 版实现此 trait);`ScriptedVerifier`(`new`(closure)/`passing`/
    `rejecting`)供测试与固定策略用。
  - `TaskDescriptor::warrants_verification()`(risk≥High 或 impact≥CrossModule 或 Ambiguous):engine 仅在
    warranting 任务上调用 verifier;worker 自报的 report triggers 始终生效。
  - `HumanGate { step: StepId, actor: AgentId }`:构造 human 交互所需身份,宿主注入,模型不可伪造。
  - `EscalationOutcome`:`Accept` / `Reassign(WorkerChoice)` / `Human(Interaction)` /
    `Exhausted { trigger }`。
  - `Escalator<V: Verifier>`(`new(verifier)` / `with_budget_headroom`,默认 20% 与 dispatcher 一致):
    `assess(task, report, roster, ctx, gate)` 顺序 = `check_cancelled` → 汇总有效 triggers(report ∪
    warranting 时 verifier)→ 空则 `Accept` → **预算压力**(`BudgetExhausted` 或 `budget_is_low(ctx)`,复用
    dispatcher 的 `budget_is_low`,改 `pub(super)`)优先于向上升级:严格更便宜 worker →
    `Reassign(BudgetDowngrade)`,否则 `Human(Question)` 停机问用户(无 capable worker → `NoCapableWorker`)→
    否则向上升级:规则 `escalate_to`(命中 trigger 且注册/capable/tier 更高)或 `strongest_capable`(严格更强)
    → `Reassign(Escalation)`;无更强 worker 时若 `human_fallback` 则 `Human`(ReviewRejected→`Permission`
    携带任务风险与 actor,其余→`Question`),否则 `Exhausted { trigger }`。
  - 升级/降级产物仍是 `WorkerChoice`,由既有 `WorkerChoice::into_subagent` → `NeedSubagent` 走
    `SubagentHandler` 派生路径;human gate 产物走既有 `InteractionKind::Permission`/`Question` 交互机制。
  - `EscalationError`(thiserror):`UnknownWorker{worker}` / `NoCapableWorker{capability}` /
    `Context(RunContextError)`。
- **dispatch.rs 扩展**:新增 `DispatchReason::Escalation` 变体;`budget_is_low` 改 `pub(super)` 供复用;
  新增 `TaskDescriptor::warrants_verification()`。
- **模块接线**:`external/mod.rs` `mod escalation;` + `pub use escalation::{Escalator, EscalationOutcome,
  EscalationError, Verifier, ScriptedVerifier, WorkerReport, HumanGate}`;`src/agent/mod.rs` 追加同名导出。
  所有新公开 API 带 rustdoc。
- **测试**:`src/agent/external/escalation/tests.rs` 24 个单测(名字含 `escalation`),覆盖三条必需验证——
  (1)cheap worker TestFailure/Timeout/LowConfidence → strong 重派;(2)超预算 → 更便宜 worker 降级 /
  无更便宜时 `Human(Question)` 停机问用户;(3)verifier 失败(ReviewRejected)→ 升级——外加低预算压过升级、
  ReviewRejected 无更强 worker → `Permission` 人工门、显式 `escalate_to` 目标、terminal `Exhausted`、
  `Reassign`→`into_subagent`、verifier 非 warranting 任务跳过、clean→Accept、UnknownWorker/NoCapableWorker/
  cancelled 错误、`warrants_verification` 谓词、report builders/serde、`primary_upward`/`upgrade_target`
  helper、`HumanGate` 访问器、`ScriptedVerifier` helpers、headroom=0 禁用降级。
- **文档**:`docs/external-agent.md` §9 追加「收敛(Milestone 6-4 已实现)」段落;`profile.rs`
  `EscalationRules` 文档指向已实现的 `Escalator`;`README.md` 模块表补 `agent::external` 调度/升级。
- **验证(完整序列全绿)**:`cargo fmt --all` ✓;`cargo test escalation` = 24 passed ✓;
  `cargo clippy --all-targets -- -D warnings` 无告警(collapsible_if → let-chain)✓;
  `cargo test --all --all-targets` 无回归(lib 545 passed = 521+24,集成/testkit 全绿,仅
  credential-gated ignored)✓;`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警 ✓。

### [TODO] M6-5 Milestone 6 Review 与文档并轨

**前置依赖**:M6-1..M6-4。

**上下文**:收官阶段。确认混合调度骨架端到端可用,并把实现与设计文档 §8/§9/§14 对齐。

**做什么**:

- 端到端跑一个混合场景:coordinator 通过 dispatcher 把一个明确任务派给 cheap worker、一个复杂任务派给
  external agent worker,验证 plan/blackboard 协作、升级与 artifact 汇总。
- 回填设计文档 §14 中在本轮已收敛的开放问题(调度策略取向、mailbox 是否需一等 API 等)的结论。
- 全量测试确认无回归,复核所有里程碑的公开 API 均有 rustdoc。

**验证条件**:

- 完整验证序列全绿,`cargo test --all --all-targets` 无回归;`RUSTDOCFLAGS="-D warnings" cargo doc
  --no-deps --workspace` 无告警。
- 端到端混合场景测试通过(给出过滤名)。
- 文档并轨结论写入「完成记录」,并在 `docs/external-agent.md` §14 标注已收敛项。
