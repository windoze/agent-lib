# 实施计划：External Agent 接入

> 本计划以 [`docs/external-agent.md`](docs/external-agent.md) 为主要设计输入,并引用
> [`docs/agent-effect-model.md`](docs/agent-effect-model.md)(effect / requirement / pop 模型)、
> [`docs/agent-layer.md`](docs/agent-layer.md)(machine / handler / driver 分层)、
> [`docs/capability-matrix.md`](docs/capability-matrix.md)(provider 能力矩阵)。
>
> 上一轮「复杂 Mock 测试与 Plan 依赖语义」计划已完成并归档到
> [`docs/archive/2026-07-16-complex-tests/`](docs/archive/2026-07-16-complex-tests/)。更早的
> Client / Conversation / Agent Layer / Effect Migration / Testability 记录在 `docs/archive/2026-07-*` 下。
> 逐任务要求见 [`TODO.md`](TODO.md)。

## 目标

在现有 sans-io + effect-handler 体系上,把 Claude Code、Codex、OpenCode 等外部 coding-agent runtime
建模为一等 custom Agent,使它们能和 `DefaultAgentMachine`、内部 LLM agent、MCP/tool、plan/blackboard
在同一个 Session 中混合编排,同时保留 effect-model 的核心价值:可暂停、可恢复、可测试、可审计、可按动态
作用域组合 handler。

具体交付:

- 一类新的 external session effect(`NeedExternalSession` requirement + `RequirementResult` 变体)与
  对应 DTO(`ExternalSessionRequest` / `ExternalSessionResult` / `ExternalAgentEvent` / `ExternalAgentError`)。
- `ExternalSessionHandler` trait 及其在 `HandlerScope` 中的接入,遵循「推进到下一个决策点并缓冲
  observations」的语义(设计文档 §5.5)。
- `ExternalAgentMachine`(实现 `AgentMachine` trait,可由 `NestedMachine` / `SubagentHandler` 挂载派生),
  覆盖 Start → AwaitingSession → Completed/Paused/Failed 与 PausedForInteraction → NeedInteraction →
  RespondInteraction 两段式交互,以及 never-resume cancel 下的 session 清理(§6.4)。
- `InteractionKind::Permission` 泛化,让外部 agent 的 shell/edit/network/spawn 权限请求进入统一
  interaction 机制。
- `Notification::ExternalAgent` 事件通道与 artifact 记录。
- worker profile registry、task evaluator/dispatcher,以及 `spawn_agent`/plan/blackboard/mailbox 工具
  adapter,支撑 cost-aware / capability-aware 混合调度。
- 每个阶段配套的 testkit scripted 组件与离线测试。

## 范围与非目标

**范围**:

- 面向 effect 边界新增 external session DTO、requirement、handler trait 与 scripted/替身实现。
- 实现 `ExternalAgentMachine` 及其与现有 `NestedMachine` / `SubagentHandler` / `RunContext` 的组合。
- 泛化 interaction permission,新增 external event/artifact 通道。
- 提供混合调度骨架与工具 adapter。
- 所有外部 runtime 侧(真实 CLI/SDK 进程)默认以 scripted / cassette 替身呈现,保证测试离线、稳定。

**非目标**:

- 不把 Claude Code / Codex 的私有实现协议作为本库稳定协议;真实 CLI/SDK adapter 的 wire 细节不进核心库。
- 不在本计划内交付针对某个具体 runtime 版本的生产级 adapter;Milestone 1 的低保真 spike 是一次性验证,
  不作为稳定 API。
- 不改变 `agent-lib` 既有运行时语义:现有 `NeedLlm/NeedTool/NeedInteraction/NeedSubagent/NeedReconfigRegistry`
  路径行为保持不变,新增 external 路径为增量。
- 不在核心库内启动真实网络/进程作为默认测试条件。

## 里程碑总览

计划按设计文档 §13 的 Phase 0–5 拆为六个里程碑,每个里程碑结尾有独立 review 任务:

| 里程碑 | 对应 Phase | 主题 | 关键产出 |
|---|---|---|---|
| Milestone 1 | Phase 0 | 低保真验证 spike | 用现有 `LlmHandler` 包一层外部 CLI,验证启动/流/取消,产出结论,不留稳定 API |
| Milestone 2 | Phase 1 | External session DTO 与 handler | DTO、`NeedExternalSession`、`ExternalSessionHandler`、scripted handler |
| Milestone 3 | Phase 2 | External agent machine | `ExternalAgentMachine`、两段式交互、cancel 清理、`NestedMachine` 挂载 |
| Milestone 4 | Phase 3 | Interaction permission 泛化 | `InteractionKind::Permission`、permission 响应、backend approve/deny/timeout/cancel |
| Milestone 5 | Phase 4 | Event sink 与 artifact | `Notification::ExternalAgent`、event sink/trace emitter、artifact ref |
| Milestone 6 | Phase 5 | Mixed-agent scheduler | worker profile registry、task evaluator/dispatcher、工具 adapter、cheap→strong 升级 |

推荐路径与文档 §7 一致:先做半托管,保留黑盒 fallback,再把高风险能力迁到全托管工具。Milestone 1 的
spike 结论可以决定是否/如何跳过部分黑盒 fallback 的实现细节。

## 现有地基(实现锚点)

以下已实现设施是各任务的接入点,任务内会给出精确路径与签名:

- Requirement 模型:`src/agent/requirement.rs`(`RequirementKind`、`RequirementResult`、`RequirementKindTag`、
  `RequirementResolution`、`SubagentOutput`、`AgentSpecRef`)。
- Handler 与作用域:`src/agent/drive.rs`(`HandlerScope` 及 `LlmHandler`/`ToolHandler`/`InteractionHandler`/
  `SubagentHandler`/`ReconfigHandler` trait、`Pop`),参考实现在 `src/agent/drive/reference.rs`、
  `src/agent/drive/subagent.rs`。
- Machine:`src/agent/machine/mod.rs`(`AgentMachine` trait、`StepInput`、`StepOutcome`)、
  `src/agent/machine/default/`(`DefaultAgentMachine`)、`src/agent/machine/nested.rs`(`NestedMachine`)。
- 交互:`src/agent/interaction.rs`(`InteractionKind`、`InteractionResponse`、`InteractionKindTag`)、
  `src/agent/approval.rs`。
- 事件:`src/agent/event.rs`(`Notification`、`StepBoundary`)。
- 上下文:`src/agent/context.rs`(`RunContext`:`is_cancelled`/`check_cancelled`/`charge_*`/`derive_child`)。
- Spec / 运行时句柄:`src/agent/spec.rs`(`AgentSpec`、`WorktreeRef`、`ToolSetRef`)、
  `src/agent/state/runtime.rs`(`AgentRuntimeHandles`)。
- 测试基础设施:`crates/agent-testkit/src/`(`handlers.rs` 的 `Scripted*Handler`、`harness.rs` 的 step/drain
  harness、`subagent.rs`、`scope.rs`、`cassette/`、`assertions/`)。

## 验证策略

每个任务结尾定义完整验证条件。默认完整验证序列(与归档计划一致):

1. `cargo fmt --all -- --check`
2. 聚焦测试:`cargo test`(仅本任务新增/相关用例,给出精确过滤名)
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`(credential-gated 集成测试保持 ignored)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

外部 runtime 交互一律通过 scripted handler 或 cassette 重放,不依赖真实 sleep、网络、credentials 或
未纳管进程作为默认测试条件。

## 风险与开放问题

沿用设计文档 §14 的未定问题,计划期需持续对齐:

- `NeedExternalSession` 是否进入核心 `agent-lib`,还是先在上层 crate 作为扩展(Milestone 2 决策点)。
- `Notification` 是否承载 external event,还是走独立 app event sink(Milestone 5)。
- `InteractionKind::Permission` 的最小字段集与审批结果 shape(Milestone 4)。
- 外部 runtime 的 session resume 能力差异如何归一化;black-box 模式下「完成」与「文件改动归属」的定义。
- 多 external agent 并发编辑同一 worktree 的冲突策略(Milestone 6 与 `WorktreeIsolation` 相关)。
- task evaluator 采用规则优先、模型优先,还是 policy engine + LLM fallback(Milestone 6)。
