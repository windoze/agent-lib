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

但还不能说完全达到设计目标。剩余问题主要集中在 facade 产品契约层：

- 流式 API 提前 drop 时的一致性恢复。
- 协作原语状态的 snapshot/restore。
- 非流式 `RunOutput.events` 的事件完整性。
- managed external capability 的 declared / verified 语义边界。
- external quick start 文档和 handler 注入 API 的可用性。
- `Agent::into_parts` 逃生舱没有覆盖后续 milestone 增加的运行组成。

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
- `AgentRunStream` 的同类风险仍未修复，由 M1-2 处理。

### 2. 协作状态运行时可用，但 snapshot/restore 仍丢弃数据

严重度：高 / 中。

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
- 明确 artifact 数据来源：
  - `RunOutput.artifacts` 是 per-run surface；
  - `AgentSnapshot.artifacts` 是否应持久化所有历史 artifact refs；
  - external delegate snapshot 中已有 per-delegate retained artifacts，是否需要汇总到 top-level artifacts。

建议测试：

- 两个 delegate 自动启用 mailbox，写入消息后 snapshot/restore，消息仍在。
- dispatcher topology 自动启用 plan / blackboard，写入后 snapshot/restore，plan task 和 board post 仍在。
- external collab observations bridge 进入 plan / blackboard / mailbox 后 snapshot/restore 保留。
- artifact refs 在 run output、external delegate snapshot、top-level snapshot 中的归属一致。

### 3. 非流式 `Agent::run_full` 的 `RunOutput.events` 不包含审批请求

严重度：中。

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

### 4. Managed external capability 的 declared / verified 语义边界不够清楚

严重度：中。

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

### 5. External quick start 文档和 handler 注入 API 不够顺手

严重度：中 / 低。

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

### 6. `Agent::into_parts` 逃生舱没有覆盖后续 milestone 的运行组成

严重度：低。

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
