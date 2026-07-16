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

### [TODO] M2-5 Milestone 2 Review

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

---

## Milestone 3 — External agent machine(Phase 2)

### [TODO] M3-1 定义 `ExternalAgentSpec` / `ExternalAgentState` / cursor 与 runtime handles

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

### [TODO] M3-2 实现 `ExternalAgentMachine` 基本推进

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

### [TODO] M3-3 实现两段式交互(Paused → NeedInteraction → RespondInteraction)

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

### [TODO] M3-4 cancel/abandon 清理、shutdown disposition 与 `NestedMachine` 挂载

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

### [TODO] M3-5 Milestone 3 Review

**前置依赖**:M3-1..M3-4。

**上下文**:确认 machine 覆盖 Start/Completed/Failed、两段式交互、cancel 清理与挂载,且 `step` 保持纯函数。

**做什么**:

- 审阅 machine 的状态迁移是否穷尽(Idle/AwaitingSession/AwaitingInteraction/Done/Error 无悬空态);确认
  `step` 无 `await`/无 IO;确认清理归属与 §6.4 一致。
- 用 harness 跑一遍完整场景(Start→Paused→Respond→Completed)与取消场景,确认无回归。

**验证条件**:

- 完整验证序列全绿,`cargo test --all --all-targets` 无回归。
- Review 结论与遗留项写入「完成记录」。

---

## Milestone 4 — Interaction permission 泛化(Phase 3)

### [TODO] M4-1 新增 `InteractionKind::Permission` 与 `PermissionRequest`

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

### [TODO] M4-2 扩展 `InteractionResponse` 的 permission 响应并校验

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

### [TODO] M4-3 interaction backend 与 testkit 的 permission 支持

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

### [TODO] M4-4 Milestone 4 Review

**前置依赖**:M4-1..M4-3。

**上下文**:确认 permission 泛化端到端自洽,且未破坏既有 approval 语义。

**做什么**:

- 核对 `InteractionKind`/`Tag`/`InteractionResponse` 三处新增对齐;`validate_response` 与
  `RequirementKind::accepts` 双重校验一致;deny-by-default 安全默认到位。
- 确认设计文档 §3.3/§14 的 permission 字段集与实现一致,必要时回填文档「最小字段集」结论。

**验证条件**:

- 完整验证序列全绿,`cargo test --all --all-targets` 无回归。
- Review 结论(含最小字段集决策)写入「完成记录」。

---

## Milestone 5 — Event sink 与 artifact(Phase 4)

### [TODO] M5-1 新增 `Notification::ExternalAgent` 变体

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

### [TODO] M5-2 observation 缓冲 → notification 转换与 event sink

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

### [TODO] M5-3 artifact ref 记录(patch / diff / test result)

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

### [TODO] M5-4 Milestone 5 Review

**前置依赖**:M5-1..M5-3。

**上下文**:确认 event/artifact 通道与 §5.5/§11/§12 一致,阻塞 requirement 与非阻塞旁路边界清晰。

**做什么**:

- 核对 observation→notification 顺序与去重;artifact 记录不落敏感原文;实时旁路接口不阻塞 continuation。
- 确认 `assertions/notifications.rs` 能覆盖新的 `ExternalAgent` notification 断言。

**验证条件**:

- 完整验证序列全绿,`cargo test --all --all-targets` 无回归。
- Review 结论写入「完成记录」。

---

## Milestone 6 — Mixed-agent scheduler(Phase 5)

### [TODO] M6-1 `WorkerProfileRef`、worker profile registry 与 `WorktreeIsolation`

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

### [TODO] M6-2 task evaluator 与 dispatcher

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

### [TODO] M6-3 `spawn_agent` / plan / blackboard / mailbox 工具 adapter

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

### [TODO] M6-4 cheap → strong 升级与 verifier

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
