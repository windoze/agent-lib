# refine notes

> 生成时间：2026-07-18。
>
> 目的：记录对当前 `agent-lib` 实现与设计目标的核对结果，特别是仍需要修正或改进的地方。本文不是新的总体设计文档；它是后续拆分 TODO / milestone 的 refine 清单。

## 总体判断

当前实现已经基本达到 `docs/facade-api.md`、`docs/managed-external-agent.md` 和根目录 `PLAN.md` 的主体设计方向：

- Client / Conversation / Agent / Facade 分层清楚。
- Conversation 的 pending transaction、closed-turn commit、snapshot/restore、boundary/fork/projection 等不变量实现扎实。
- Agent facade 确实复用了 `DefaultAgentMachine + HandlerScope + drain`，没有看到另起一套轻量 runtime 的问题。
- Managed external agent 已按 `ExternalAgentMachine` sans-io machine + registry-backed `ExternalSessionHandler` + feature-gated adapter 的方向接上。
- 默认测试保持离线；真实 provider / CLI 路径按设计 `#[ignore]` 或 feature-gated。

原始审查时集中在 facade 产品契约层的六类缺口，经过 M1–M5 均已收口（逐条状态见
「需要修正的问题」各条目开头的状态行）：

- 流式 API 提前 drop 时的一致性恢复 → 问题 #1（M1-1、M1-2；M1-3 复核）。
- 非流式 `RunOutput.events` 的事件完整性 → 问题 #3（M2-1、M2-2；M2-3 复核）。
- 协作原语状态的 snapshot/restore → 问题 #2（M3-1 ~ M3-4；M3-5 复核）。
- managed external capability 的 declared / verified 语义边界 → 问题 #4（M4-3、M4-4；M4-5 复核）。
- external quick start 文档和 handler 注入 API 的可用性 → 问题 #5（M4-1、M4-2；M4-5 复核）。
- `Agent::into_parts` 逃生舱没有覆盖后续 milestone 增加的运行组成 → 问题 #6（M5-1、M5-2；M5-3 复核）。

M6 负责最终收口：本文档状态同步（M6-1）、全量默认与 external feature 验证（M6-2）、
最终正确性与完整性验收（M6-3）。截至本次更新，六类问题的实现修复均已完成并逐条通过
对应 milestone 的 review；剩余仅为 M6 的文档 / 验证收尾，无已知未排期的阻断风险。此判断
与 `PLAN.md`（M1–M6 里程碑）和 `TODO.md`（M1-1 ~ M5-3 均 `[DONE]`，M6-1 ~ M6-3 收尾）一致。

## 验证结果

本次审查未修改代码前，以下验证均通过：

```bash
cargo test -p agent-lib --lib facade::
cargo test --all --all-targets
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

说明：

- `cargo test --all --all-targets` 通过。
- 真实 provider / CLI 测试按设计 ignored 或 feature-gated，没有在默认套件里执行。
- 上述绿测不能覆盖本文指出的 stream drop、协作状态 snapshot、非流式审批事件缺口。

## 需要修正的问题

### 1. 流式 facade 提前 drop 会留下未关闭的 pending turn

严重度：高。

状态：**已修复（M1-1、M1-2；M1-3 复核通过）**。下文保留原始缺口描述作为背景，当前实现说明见本条末尾「修复状态（更新）」。

`ChatSession::stream` 在返回 `RunStream` 前已经打开 pending turn：

- `src/facade/chat.rs`：`ChatSession::stream` 中调用 `conversation.begin_turn(...)`。

`RunStream` 只在显式错误路径调用 `rollback()`，没有 `Drop` guard。调用方如果消费几个事件后直接丢弃 stream，`ChatSession` 会残留 pending turn，后续 `send` / `snapshot` 很可能失败。这违背 facade 对“失败 / 取消后回到上一 committed 一致点”的设计承诺。

`AgentRunStream` 也有同类风险：

- `src/facade/agent/stream.rs`：把 `drain(machine, ...)` 放进 future。
- `AgentRunStream` 本身没有 drop-time abandon / cancel guard。

当 future 在等待 LLM、tool、approval 或外部 delegate 时被 drop，machine 可能停在 outstanding requirement 上，后续 run 可能无法可靠继续。

建议修正：

- 给 `RunStream` 增加 `Drop` 实现：若状态不是 terminal `Done`，调用 `conversation.cancel_pending(CancelDisposition::DiscardTurn)`。
- 给 `AgentRunStream` 增加显式关闭语义：
  - 选项 A：增加 `cancel()` / `close()`，内部走 `StepInput::Abandon`；
  - 选项 B：实现 RAII guard，在 drop 时对 outstanding requirement 做 abandon；
  - 选项 C：内部持有 cancellation token，drop 时 cancel，然后确保 future 被继续 poll 到 abandon 完成。这一方案要小心 async drop 不存在的问题，可能仍需要显式 `close().await`。
- 文档需要明确：提前 drop 是自动 discard，还是要求调用方显式 `close().await`。当前文档写的是“dropping the stream before completion leaves the agent's committed history unchanged”，实现应对齐这个承诺。

建议测试：

- `ChatSession::stream`：drop before first poll 后下一次 `send` 成功。
- `ChatSession::stream`：收到一个 text delta 后 drop，`snapshot` 成功。
- `Agent::stream`：等待 LLM 时 drop，下一次 `run` 成功。
- `Agent::stream`：等待 tool / approval 时 drop，下一次 `run` 成功。
- 若 external delegate streaming 支持中途 drop，补 external cleanup marker / session cleanup 断言。

修复状态（更新）：

- `ChatSession::stream` 的 `RunStream` 已在 M1-1 修复：新增 `Drop` guard，非
  terminal `Done` 状态被 drop 时通过统一的 `abandon()`（`cancel_pending(DiscardTurn)`
  + 标记 terminal，幂等）回滚 pending turn；错误路径与 drop 路径收敛到同一 helper。
  已补 drop-before-poll、收到 text delta 后 drop、正常读完后 drop 三个离线回归测试。
- `AgentRunStream` 的同类风险已在 M1-2 修复：新增 `Drop` guard，通过共享的
  `Rc<RefCell<&mut DefaultAgentMachine>>` 让 drop 路径能同步访问持有的 machine，
  在非 terminal 状态被 drop 时用现有 sans-io 输入 `StepInput::Abandon`（machine
  的 never-resume 路径）关闭在途 turn：LLM 步丢弃 pending turn，tool / approval
  阶段对未决调用折叠 `Cancelled` 结果，二者都把 cursor 归位到可继续的 `Idle`。
  `abandon()` 幂等（terminal `Done` / 错误 / 已 abandon 时为 no-op，不回滚已提交
  turn）。已补 未 poll 就 drop、收到部分事件后 drop、等待 approval 时 drop、等待
  tool 结果时 drop 四个离线回归测试，均验证随后同一 `Agent` 可成功 `run` 且丢弃的
  半成品 turn 不进入 committed history。
- M1-3 复核通过：全仓仅 `RunStream`（`src/facade/chat.rs`、`src/facade/chat/stream.rs`）
  与 `AgentRunStream`（`src/facade/agent.rs`、`src/facade/agent/stream.rs`）两个 facade
  层 stream 会打开 conversation / machine 的 pending state，二者的错误路径与 `Drop` 都
  收敛到同一幂等 `abandon()`；adapter / client 层的 `chat_stream` 只是纯 wire 事件流，
  不打开 facade pending state，无需 cleanup。新增回归全部基于 fake client / scripted
  handler（`DualFakeClient`、`DropTestClient`、`ParkingInteractionHandler`、
  `parking_weather_tool`），无真实 provider / CLI / 网络 / 本机配置依赖；相关 stream
  doc 已写明「提前 drop 自动 discard / abandon 在途 turn，回到上一 committed 一致点」。
  验证：`cargo fmt --all`（无源码改动）、`cargo clippy --all-targets -- -D warnings`
  （clean）、`cargo test -p agent-lib --lib facade::chat::`（19 passed）、
  `cargo test -p agent-lib --lib facade::agent::`（30 passed，含 4 条 drop 回归）。

### 2. 协作状态运行时可用，但 snapshot/restore 仍丢弃数据

严重度：高 / 中。

状态：**已修复（M3-1 ~ M3-4，M3-5 复核通过）**。下文保留原始缺口描述作为背景，当前实现说明见本条末尾“修复结果”。

M6 已让 facade 按 delegate 拓扑 provision 协作原语：

- mailbox
- blackboard
- plan
- artifact substrate

但 `AgentSnapshot::capture` 仍固定写：

- `mailbox: None`
- `blackboard: None`
- `plan: None`
- `artifacts: Vec::new()`

restore 也只是按拓扑重新 provision 一套空的协作原语，而不是从 snapshot 恢复协作内容。

这意味着：

- 运行时协作便利层达到了。
- 但“可恢复”的设计目标对协作数据还没达到。
- external collab observations 写入 mailbox / blackboard / plan 后，snapshot/restore 会丢失这些事实。

建议修正：

- 将 `MailboxSnapshot` / `BlackboardSnapshot` 从占位类型推进为真实数据 shape。
- `AgentSnapshot::capture` 从 `CollabState` 读取当前 mailbox / blackboard / plan / artifact state。
- restore 时优先使用 snapshot 内容，不只是从 topology derive 空 substrate。
- 明确 artifact 数据来源（**已决策，M3-4**）：
  - `RunOutput.artifacts` 是 per-run surface（瞬时视图）；
  - external delegate snapshot 中已有 per-delegate retained artifacts，随会话事实持久化并在 restore 时按
    delegate 恢复；
  - 顶层 `AgentSnapshot.artifacts` 定为**保留兼容字段**：capture 恒空、restore 不读取。因为没有稳定的
    facade-level artifact store（`CollabState` 的 artifact store 只是 config flag），故不聚合、不伪造聚合
    语义。权威 artifact 来源为上面两处（`docs/facade-api.md` §15.2）。

建议测试：

- 两个 delegate 自动启用 mailbox，写入消息后 snapshot/restore，消息仍在。
- dispatcher topology 自动启用 plan / blackboard，写入后 snapshot/restore，plan task 和 board post 仍在。
- external collab observations bridge 进入 plan / blackboard / mailbox 后 snapshot/restore 保留。
- artifact refs 的归属一致：per-run 归 `RunOutput.artifacts`，per-delegate 归 external delegate snapshot，
  顶层 `AgentSnapshot.artifacts` 为保留兼容字段（恒空、restore 忽略），三者语义不冲突。

修复结果（M3-1 ~ M3-4，M3-5 复核）：

- `MailboxSnapshot` / `BlackboardSnapshot` / `PlanSnapshot` 已是真实 data-only 数据 shape
  （无 lock / 无 runtime handle，derive `Serialize`/`Deserialize`）；facade 层
  `AgentSnapshot.{mailbox,blackboard,plan,artifacts}` 均带 `#[serde(default)]`，旧格式
  快照可安全反序列化（M3-1）。
- `AgentSnapshot::capture` 已从 live `CollabState` 读取 `mailbox.snapshot()` /
  `blackboard.snapshot_all()` / `plan.snapshot()`，不再固定写空（M3-2）。
- restore 采用 **snapshot 内容权威、topology 仅作旧快照 provision hint** 的冲突策略
  （`CollabState::restore`）：捕获到的原语即使拓扑未启用也会恢复内容，恢复后拓宽
  effective `config` 使 `collaboration()` 与访问器一致；缺内容但拓扑启用才建空底座（M3-3）。
- 顶层 `AgentSnapshot.artifacts` 定为保留兼容字段（capture 恒空、restore 不读取），权威
  artifact 来源为 `RunOutput.artifacts`（per-run）与 external delegate snapshot（per-delegate 持久），
  代码注释与 `docs/facade-api.md` §15.2 一致（M3-4）。
- retained external session snapshot 未被本阶段改动破坏：restore 仍按 `restore_external`
  策略从 `ExternalDelegateSnapshot` 的 `session` / `artifacts` / `status` 重建
  `RetainedExternalSession`。
- M3-5 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`（clean）、
  `cargo test -p agent-lib --lib agent::collab`（28 passed）、
  `... facade::agent::snapshot`（8 passed）、`... facade::collab`（19 passed）、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`（clean）。

### 3. 非流式 `Agent::run_full` 的 `RunOutput.events` 不包含审批请求

严重度：中。

状态：**已修复（M2-1、M2-2；M2-3 复核通过）**。下文保留原始缺口描述作为背景，当前实现说明见本条末尾「修复状态（更新）」。

`Agent::run_full` 当前在 `drain` 完成后通过 `collect_traces(done.notifications(), &recorder)` 折叠事件。`collect_traces` 只处理：

- `Notification::ToolCallStarted`
- `Notification::ToolCallFinished`
- delegation recorder 里的 delegation 记录

审批请求不是 notification；它是 `NeedInteraction` requirement。因此非流式路径不会把 `RunEvent::ApprovalRequested` 放入最终 `RunOutput.events`。

流式路径不同：

- `TapInteractionHandler` 会在 fulfill approval 前 emit `RunEvent::ApprovalRequested`。

结果：

- `agent.stream()` 可以看到富化审批请求。
- `agent.run_full()` 的最终 `RunOutput.events` 看不到审批请求。

这削弱了 `RunOutput` 作为完整观测面的设计目标，也让非流式产品集成难以事后展示审批历史。

建议修正：

- 增加 run-scoped approval recorder，让非流式 `FacadeAgentScope` 也记录 `ApprovalRequested`。
- 或将 approval wait 作为 observe-only notification 暴露，再由 `collect_traces` 统一折叠。
- 需要保持 M7 富化字段一致：`tool_name`、`call_id`、`reason`、redacted `input`。

建议测试：

- `run_full` + `Approval::ask` approve：`RunOutput.events` 包含 `ApprovalRequested`。
- `run_full` + auto deny：仍能记录请求或明确记录 deny trace。
- `run_full` 与 `stream` 对同一 scripted run 的 normalized event 序列尽量一致。

修复状态（更新）：

- 已在 M2-1 修复：非流式 `Agent::run_full`（`src/facade/agent.rs`）新增 run-scoped
  approval recorder。`RecordingInteractionHandler` 包裹 `interaction_handler()` 解析出的
  真实 handler（注入的 handler 或 `FacadeApproval` fallback），在把 approval interaction
  传给真实 handler *之前* 按 fulfill 顺序记录 `ApprovalRequest`——**仅观察不决策**，
  approve / deny / fallback 优先级完全不变。富化字段（`tool_name`、`call_id`、`reason`、
  redacted `input`）由与流式路径共享的 `enriched_approval_request` helper
  （`src/facade/approval.rs`）构造，两路字段映射一致。`weave_approval_events` 按 `call_id`
  把记录的审批编织进 `collect_traces` 事件流：approved 落在其 `ToolStarted` 前，denied /
  headless 审批（无工具事件锚点）在尾部或下一锚点前 flush，保证每个暂停审批可见。
- 已在 M2-2 对齐并修复事件契约分歧：非流式 `collect_traces` 原先对**被拒工具**的
  `ToolCallFinished`（无对应 `ToolCallStarted`）投出空 name 的幽灵 `ToolFinished`，与流式
  路径不一致。现改为被拒工具两路都只保留 `ApprovalRequested`，不产 `ToolStarted`/
  `ToolFinished`。新增 4 条 `run_full` 与 `stream` 的 normalized-lifecycle parity 回归
  （plain / approved / denied / delegation），并断言 token `TextDelta` 只属流式路径。
  文档边界在 `docs/facade-api.md` §6.2.1 与 `src/facade/run.rs` 的 `RunEvent` /
  `RunOutput::events` rustdoc 中明确。
- 已在 M2-3 复核：`run_full` / `run` / `stream`（`src/facade/run.rs`、
  `src/facade/agent.rs`、`src/facade/approval.rs`）事件语义清晰，approve / deny /
  fallback 路径均记录审批事件，recorder 只观察不改变真实 handler 执行顺序，非流式
  路径不产 token delta。本条视为已解决。验证：`cargo fmt --all`（clean）、
  `cargo clippy --all-targets -- -D warnings`（clean）、
  `cargo test -p agent-lib --lib facade::agent::`（37 passed，含 4 条 parity 回归）、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`（clean）。

### 4. Managed external capability 的 declared / verified 语义边界不够清楚

严重度：中。

状态：**已修复（M4-3, M4-4，M4-5 复核通过）**。下文保留原始缺口描述作为背景，当前实现说明见本条末尾“修复结果”。

CLI preset builder 默认用 `declared_capabilities(runtime)` 填 capabilities，并在 `build()` 时用它验证 `ExternalRunMode`。`declared_capabilities` 对 Claude Code / Codex / OpenCode 预置了若干能力为 true，例如：

- streaming
- resume
- artifacts
- usage
- graceful shutdown
- Claude Code 还声明 permission bridge

这表示“adapter 声明能力”，不是“当前机器 feature 已启用且 probe 已通过”。`default_external_session_handler` 会 probe，但 probe 结果没有反写到已经 build 出来的 `ManagedExternalAgent::capabilities()`。

风险：

- 调用方读取 `agent.capabilities()` 时可能误以为这些能力已经 runtime-verified。
- 这和设计文档中“不假装支持未验证能力”的表达存在张力。
- 当前代码和文档局部已经说它是 declared baseline，但 API 命名上并不明显。

建议修正：

- 显式拆分：
  - `declared_capabilities()`
  - `verified_capabilities()`
  - 或 `capability_source: Declared | Probed | Negotiated`
- 让 `default_external_session_handler` 返回 handler 的同时返回 probed capabilities，或者返回一个已经更新 capabilities 的 `ManagedExternalAgent`。
- 对 ACP negotiated path 也保持同一语义：negotiated 是 verified，preset baseline 是 declared。
- 若暂不改 API，至少在 rustdoc / README 中明确：`ManagedExternalAgent::capabilities()` 是 build-time declared / supplied view，不等价于 live probe result。

建议测试：

- preset capabilities 标注 source 为 declared。
- probe 失败时不能留下“看起来支持 Managed mode”的 verified view。
- `with_probed_capabilities` / `acp_negotiated` 后 source 变为 probed / negotiated。

修复结果（M4-3, M4-4，M4-5 复核）：

- 新增 `CapabilitySource` 来源模型，覆盖 `Declared` / `Supplied` / `Probed` /
  `Negotiated` 四值，`ExternalAgentCapabilities` 带 `source()` accessor，
  provenance-tagged 构造函数 `declared(..)` / `supplied(..)` / `probed(..)` /
  `from_acp_negotiation(..)`；serde 缺字段回落到保守的 `Declared`（M4-3）。
- preset 构造出的 agent 持 `Declared` 视图（`preset_capabilities_are_declared`
  测试断言 codex preset `source()==Declared`）；ACP pre-negotiation baseline 亦为
  `Declared`，`.acp_negotiated(..)` 折入 `Negotiated`（M4-3）。
- `ManagedExternalAgentBuilder::build_with_default_session_handler()` probe 成功后把
  探到的 `ExternalRuntimeCapabilities` 折回 agent 并标 `Probed`，取代 declared 基线；
  之后 `agent.capabilities()` / `require_capability(..)` / `ExternalRunMode` 校验都以
  probed 为准，probed 比 declared 窄时**以 probed 为准**（`validate_external_mode`
  按 probed 视图重新校验）。ACP 无离线 probe，保留 declared/negotiated 视图（M4-4）。
- 来源标签进入错误信息：`UnsupportedExternalMode` / `UnsupportedExternalCapability`
  都带 `capability_source`（`declared` / `supplied` / `probed` / `negotiated`），
  为稳定 `&'static str`，绝不含 runtime 输出、启动命令行或凭据
  （`require_capability_gates_against_probed_view` 断言 rendered 错误不含 `KEY`/`TOKEN`）。
- 文档：`docs/capability-matrix.md` §11.3「能力来源：declared vs probed」与
  `docs/facade-api.md` §11.3 同步说明四值来源模型、一步式装配折入 probed、probed 优先、
  ACP 无离线 probe、来源标签不含 secret（M4-4）。
- M4-5 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`（clean）、
  `cargo test -p agent-lib --lib facade::external`（22 passed）、`cargo check --examples`
  （clean）、`cargo clippy --all-targets --features "external-claude-code external-codex
  external-opencode external-acp" -- -D warnings`（clean）、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`（clean）。

### 5. External quick start 文档和 handler 注入 API 不够顺手

严重度：中 / 低。

状态：**已修复（M4-1, M4-2，M4-5 复核通过）**。下文保留原始缺口描述作为背景，当前实现说明见本条末尾“修复结果”。

README 的 external quick start 目前大致是：

```rust
let codex = ManagedExternalAgent::codex()
    .worktree("/home/me/repos/my-app")
    .mode(ExternalRunMode::Managed)
    .build()?;
```

然后直接 `.external_agent("coder", codex)`。

但实际 external delegate 被模型调用时，如果没有 session handler，会失败并提示：

```text
no runtime session handler is attached; call ManagedExternalAgentBuilder::session_handler(..) to drive it
```

README 后面说明可以用 `default_external_session_handler(&agent)` 接 `.session_handler(..)`，但这里的 `agent` 指 `ManagedExternalAgent`，而 `.session_handler(..)` 又是 builder 方法。用户按 quick start 写，第一次运行很可能失败。

建议修正：

- README external quick start 改成真正可运行的两阶段示例：

```rust
let codex_builder = ManagedExternalAgent::codex()
    .worktree("/home/me/repos/my-app")
    .mode(ExternalRunMode::Managed);

let codex_probe = codex_builder.clone().build()?;
let handler = default_external_session_handler(&codex_probe).await?;
let codex = codex_builder.session_handler(handler).build()?;
```

- 或新增更顺手的 API：
  - `ManagedExternalAgentBuilder::default_session_handler().await?`
  - `ManagedExternalAgentBuilder::build_with_default_session_handler().await?`
  - `ManagedExternalAgent::with_session_handler(handler)`

建议测试 / 文档校验：

- 把 README quick start 提取为 ignored doctest 或 compile-only no_run doctest。
- 确保 feature-gated handler 类型示例在对应 feature 下能编译。

修复结果（M4-1, M4-2，M4-5 复核）：

- 新增一步式装配 API `ManagedExternalAgentBuilder::build_with_default_session_handler()`
  （async）：默认 crate build 不含任何 CLI adapter，未开启对应 `external-*` feature 时
  fail-fast（非密错误、点名要开的 feature），开启后探测本机已登录 CLI 并接上官方
  registry-backed handler，一步产出可直接 `.external_agent(..)` 的 `ManagedExternalAgent`
  （M4-1）。底层便捷构造 `default_external_session_handler(&agent)` /
  `default_external_session_handler_with_capabilities(&agent)` 仍可用于手工装配。
- 已手工 `.session_handler(my_handler)` 时，`build_with_default_session_handler()` 短路
  probe、honor 自定义 handler（`build_with_default_session_handler_honors_supplied_handler`
  测试覆盖）。
- README external quick start 改写为使用 `build_with_default_session_handler().await?`
  的可运行两阶段装配（不再出现“build 出的 agent 无 handler、首次运行即失败”的坑），
  并说明自定义 handler 的手工路径（M4-2）。
- M4-5 验证：`cargo check --examples`（clean，`examples/support/managed.rs` 全手工
  scoped-effect wiring 编译通过）、`cargo test -p agent-lib --lib facade::external`
  （22 passed，含 `build_with_default_session_handler_*` 系列）、feature clippy 与
  `cargo doc` 均 clean（命令见 §4 修复结果）。

### 6. `Agent::into_parts` 逃生舱没有覆盖后续 milestone 的运行组成

严重度：低。

状态：**已修复（M5-1 扩展；M5-2 文档对齐；M5-3 复核通过）**。下文保留原始缺口描述作为背景，当前实现说明见本条末尾“修复结果”。

`Agent::into_parts` 目前只返回：

- `AgentState`
- `LlmClient`
- typed tools
- custom registry
- extra declarations
- `FacadeApproval`
- `FacadeIds`
- local delegates
- `Delegation`

它没有返回：

- managed external delegates
- retained external sessions
- collaboration state
- host-injected interaction handler
- possibly external session handler attachments

作为“逃回底层”的 escape hatch，这在 M4/M6/M7 后已经不完整。调用方消费 `into_parts` 后会丢掉一部分 facade 组装事实。

建议修正：

- 扩展 `AgentParts`，补齐后续 milestone 增加的组成。
- 或把 `AgentParts` 明确文档化为 base-agent-only escape hatch，并提供新的 `AgentFullParts` / `into_full_parts`。
- 注意保持 snapshot 原则：runtime handles 可以作为 escape hatch 交出，但不能进入 data-only snapshot。

修复结果（M5-1 扩展，M5-2 文档对齐）：

- `AgentParts`（`src/facade/agent/snapshot.rs`）在原有 state / client / tools / custom_registry /
  extra_declarations / approval / ids / delegates / delegation 基础上，新增 7 个 public 字段：
  `interaction_handler`、`external_agents`（managed external delegates）、`retained_external_sessions`
  （每个 external delegate 的 data-only 会话事实，不含进程句柄 / SDK client / 凭据）、`collaboration`
  （config），以及 live `mailbox` / `blackboard` / `plan` 句柄。采用「扩展 `AgentParts`」路线，未新增
  `AgentFullParts` / `into_full_parts`（M5-1）。
- `Agent::into_parts`（`src/facade/agent.rs`）析构 `self.collab` 后把 config 与三个 live 句柄分别搬出，
  并搬出 interaction handler / external delegates / retained sessions，不再静默 drop 任何仍有语义价值的
  字段（M5-1）。runtime handles 仅经此逃生舱交出，`AgentSnapshot` 仍保持 data-only、不含句柄，snapshot
  原则未被破坏。
- 内部类型 `RetainedExternalSession` 由 `pub(crate)` 提升为 `pub` 并在 `facade` 重导出，使 `AgentParts`
  的 public 字段类型可达（M5-1）。
- 文档对齐（M5-2）：`docs/facade-api.md` §8.2 写清 snapshot / `into_parts` / builder 三者的用途边界
  （持久化恢复 / 接管 live handle / 常规构造），并列出 `into_parts` 交出的部件与「非 restore API」保证；
  `Agent::into_parts` 与 `AgentParts` 的 rustdoc 逐项说明资源范围与不保证事项。至此不再有文档声称
  `into_parts` 覆盖不完整或缺失字段。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`（clean）、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`（clean）、
  `cargo test -p agent-lib --lib facade::agent::`（全绿，含 M5-1 新增 5 个 `into_parts_*` 测试）。

## 建议优先级

建议按以下顺序修：

1. **stream drop 一致性**：这是最直接的状态正确性问题，且会影响真实 UI / host 集成。
2. **非流式 approval event 完整性**：M7 宿主嵌入要依赖事件面，`run_full` 和 `stream` 不一致会造成产品行为分叉。
3. **协作状态 snapshot/restore**：如果 `Collaboration` 要作为正式 facade 能力，这是可恢复目标的核心缺口。
4. **external handler quick start / builder ergonomics**：降低首个 external 集成踩坑概率。
5. **capability source 拆分**：提升 API 诚实度，避免 declared 被误读为 verified。
6. **`into_parts` 补齐**：属于高级逃生舱完善，可排在状态正确性之后。

## 可以直接转成 TODO 的任务草案

### R-1 stream drop recovery

- 为 `ChatSession::RunStream` 增加 drop rollback。
- 为 `AgentRunStream` 增加 cancel/close 或 drop abandon 机制。
- 增加 drop-before-completion 后继续使用 session/agent 的测试矩阵。

### R-2 approval event parity

- 增加非流式 approval recorder。
- 让 `Agent::run_full` 的 `RunOutput.events` 包含富化 `ApprovalRequested`。
- 增加 `run_full` / `stream` event parity 测试。

### R-3 collaboration snapshot

- 实现 `MailboxSnapshot` / `BlackboardSnapshot` 真实字段。
- `AgentSnapshot::capture` 持久化 live collab state。
- restore 从 snapshot 重建 collab state。

### R-4 external quick start and handler ergonomics

- 修 README external quick start。
- 评估并新增 `build_with_default_session_handler().await?` 之类 API。
- 增加 feature-gated compile-only 示例。

### R-5 capability source clarity

- 标记 capability source。
- 区分 declared / probed / negotiated capabilities。
- 更新 rustdoc 和 capability matrix。

### R-6 full escape hatch

- 扩展 `AgentParts` 或新增 `AgentFullParts`。
- 覆盖 external delegates、retained sessions、collab state、interaction handler。
