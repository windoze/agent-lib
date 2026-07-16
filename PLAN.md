# 实施计划：Managed External Agent

> 本计划以 [`docs/managed-external-agent.md`](docs/managed-external-agent.md) 为设计输入。
> 上一轮根目录计划「Effect 层清理(三刀重构)」已归档到
> [`docs/archive/2026-07-17-effect-refine/`](docs/archive/2026-07-17-effect-refine/)。
> 逐任务实现清单见 [`TODO.md`](TODO.md)。

## 目标

把现有 `ExternalAgentMachine` 从“黑盒 external session effect”推进到“受管 external agent”：

- 外部 runtime(Claude Code / Codex / OpenCode) 的流式文本、命令、patch、权限请求、tool call、
  subagent 请求、artifact、usage 等都通过 agent effect 模型归一化。
- `ExternalAgentMachine` 仍保持 sans-io：它只 reify `Requirement`，不启动进程、不读写 runtime stream、
  不执行工具、不询问用户。
- 真实 IO 由 `ExternalSessionHandler` / runtime adapter / session registry 兑现；driver 继续用
  `drain`、`HandlerScope`、`Pop`、`RunContext` 处理 nested scope、cancel、budget、trace。
- 对内部 agent 已有能力尽量 parity：流式输出、tool 注入、approval/user interaction、subagent、
  artifact、usage/cost、worktree isolation、cancel cleanup、reconfig。

## 非目标

- 不把 Claude Code、Codex、OpenCode 的私有 JSON/JSONL schema 变成 `agent-lib` 的稳定 public API。
  这些 schema 只存在于 runtime adapter 的 parser/cassette 层。
- 不要求三个 runtime 首版能力完全一致。能力差异通过 capability model 显式暴露，不能静默假装支持。
- 不让真实 CLI/API 测试进入默认测试路径。默认 `cargo test --all --all-targets` 必须离线、稳定、
  不依赖凭据、网络、本地登录态或未纳管进程。
- 不新增新的 effect family。external runtime 的 tool/subagent/interaction 决策点应映射到现有
  `NeedTool`、`NeedSubagent`、`NeedInteraction`，再通过 `NeedExternalSession` 回灌给 runtime。
- 不把 worktree/process 权限当成 runtime 自带安全边界。宿主的 permission policy 与 worktree isolation
  仍是最终约束。

## 当前实现锚点

以下锚点是本计划的精确接入面：

- `src/agent/external/mod.rs`
  - `ExternalRuntimeKind::{ClaudeCode,Codex,OpenCode,Custom}` 已有。
  - `ExternalSessionInput` 现有 `Start` / `Continue` / `RespondInteraction` / `Shutdown`。
  - `ExternalSessionRequest` 已携带 `agent_id`、`runtime`、`worktree`、`session`、`input`、`tools`、
    `policy`。
  - `ExternalAgentEvent` 已覆盖 session、text delta、command、file patch、permission、tool、
    message、task、completed，但事件本身还没有 `seq`。
  - `ExternalSessionResult` 现有 `Completed` / `PausedForInteraction` / `Failed`，`observations`
    仍是 `Vec<ExternalAgentEvent>`。
  - `ExternalAgentError` 已有 `Launch`、`SessionLost`、`Protocol`、`LimitExceeded`、
    `ResumeUnavailable`、`ShutdownFailed`、`Runtime`。
- `src/agent/external/machine.rs`
  - `ExternalAgentMachine` 已支持 user message -> `NeedExternalSession(Start/Continue)`。
  - 已支持 `PausedForInteraction` -> `NeedInteraction` -> `RespondInteraction`。
  - 已支持 `Completed` commit Conversation、`Failed` error cursor、artifact 记录、cancel cleanup flag。
  - 当前 cursor 只有 `AwaitingSession` / `AwaitingInteraction`，还没有 tool/subagent 相位。
  - `observe` 当前依赖 `ExternalSessionRef.last_event_seq` 做粗粒度 dedup。
- `src/agent/external/state.rs`
  - `ExternalAgentCursor` 是可序列化恢复 cursor。
  - `ExternalAgentState` 持有唯一 `Conversation`、`session`、`active_tools`、`artifacts`、
    `cleanup_required`。
- `src/agent/drive.rs`
  - `ExternalSessionHandler` 已是一个 effect handler。
  - `drain` 会并发 fulfill 本 scope 可处理的非 subagent requirement；`NeedSubagent` 走串行
    `resolve_requirement` 并带 `outer` pop target。
- `src/agent/effect_manifest.rs`
  - `ExternalSession` 已在 effect manifest 中；本计划不需要改 manifest 增 effect。
- `src/agent/tool.rs`
  - `ToolExecutionIds` 提供 `ToolCallId`、tool result message id、assistant continuation id、step id。
  - external tool 注入应复用 `ToolExecutionIds::tool_call_id` 分配 framework call id。
- `src/agent/interaction.rs` / `src/agent/permission.rs`
  - `InteractionKind::Permission`、`PermissionRequest`、`PermissionResponse` 已能承载 runtime 权限请求。
- `src/agent/drive/subagent.rs`
  - `DrivingSubagentHandler` 已定义 nested drain 机制；external runtime spawn 请求应走 `NeedSubagent`。
- `src/agent/external/runtime.rs` / `src/agent/external/shutdown.rs` / `src/agent/external/sink.rs`
  - 已有 runtime handle holder、shutdown disposition、live event sink 占位。

## 目标架构

```text
ExternalAgentMachine (sans-io)
  UserMessage
    -> NeedExternalSession(Start/Continue)
    -> ExternalSessionResult decision point

Decision points:
  Completed
    -> commit Conversation -> Done

  PausedForInteraction
    -> NeedInteraction
    -> NeedExternalSession(RespondInteraction)

  PausedForToolCalls
    -> NeedTool batch
    -> NeedExternalSession(RespondToolResults)

  PausedForSubagent
    -> NeedSubagent
    -> NeedExternalSession(RespondSubagent)

  Failed
    -> cancel pending -> Error
```

`ExternalSessionHandler` 的生产实现只负责把 runtime adapter 的下一决策点包装成
`RequirementResult::ExternalSession(Box<ExternalSessionResult>)`。machine 收到决策点后，继续把 host
tool、host interaction、host subagent reify 成现有 requirement，由外层 scope 决定本层处理还是 pop。

## 里程碑

| 里程碑 | 主题 | 主要文件 | 默认测试形态 |
|---|---|---|---|
| M1 | external session 协议扩展与 sequenced observations | `src/agent/external/mod.rs` | serde/unit |
| M2 | `ExternalAgentMachine` tool parity | `src/agent/external/machine.rs`、`state.rs` | machine unit |
| M3 | subagent / interaction parity | `machine.rs`、`interaction.rs`、`permission.rs` | machine + drive unit |
| M4 | streaming live sink、capability model、session policy | `external/sink.rs`、`external/profile.rs`、新 capability 模块 | unit |
| M5 | runtime adapter abstraction 与 scripted/cassette handler | `external/runtime.rs`、新 adapter/registry 模块 | offline scripted/cassette |
| M6 | Claude Code managed adapter | feature-gated adapter 模块 | parser cassette + ignored real e2e |
| M7 | Codex managed adapter | feature-gated adapter 模块 | parser cassette + ignored real e2e |
| M8 | OpenCode managed adapter | feature-gated adapter 模块 | parser cassette + ignored real e2e |
| M9 | worktree/budget/reconfig/docs/real mixed e2e hardening | `external/*`、`docs/*`、`tests/*` | offline + ignored real |

## 验证策略

默认完整验证序列：

1. `cargo fmt --all -- --check`
2. 聚焦测试：任务中给出精确过滤名
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

真实 runtime/API 测试必须满足：

- `#[ignore]`，默认测试不运行。
- 明确检测所需环境变量和 CLI 可用性，缺失时跳过或给出不含 secret 的错误。
- 不输出 key、auth header、完整 prompt transcript、未脱敏 tool input。
- 使用独立 worktree 或临时目录，结束后清理或标记残留 side effect。

## 风险

- **runtime 协议漂移**：Claude Code / Codex / OpenCode JSON/JSONL schema 会变。缓解：adapter parser 私有化、
  cassette 覆盖、capability probe、不把 raw schema 暴露为稳定 API。
- **tool bridge 能力不对称**：三个 runtime 未必都支持 host tool 注入。缓解：capability model 明确
  `UnsupportedCapability`，dispatcher 避免派错 worker。
- **cancel 后副作用残留**：forced kill 不能回滚已执行命令。缓解：session registry 记录
  `ExternalSessionShutdown`，worktree isolation 标记 dirty。
- **stream 事件重复**：live sink 和 buffered observations 双路径可能重复。缓解：
  `ExternalObservedEvent.seq` 作为唯一 replay 进度。
- **恢复 mid-turn scratch**：`ExternalAgentMachine::new` 当前无法重建 awaiting state 的 driver-facing
  streaming cursor。缓解：把外部 tool/subagent pending facts 放入 serializable cursor，并补 restore 测试。
