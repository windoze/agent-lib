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
  - `ExternalRuntimeKind::{ClaudeCode,Codex,OpenCode,Custom}` 已有;ACP adapter(M10)复用
    `Custom(String)` 承载,不新增变体。
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
| M10 | ACP(Agent Client Protocol)managed adapter | `external/acp/*`(feature `external-acp`) | offline cassette/fake-transport + ignored real e2e |

## M10:ACP managed adapter(与 M6-M8 的关系)

M6-M8 为 Claude Code / Codex / OpenCode 各写了一套**私有 wire 解码**的 adapter,因为这三家 CLI 的
JSON/JSONL 输出互不兼容、且都是「自主运行、只读观测」的半托管形态(`permission_bridge` / `host_tools`
恒为 `false`)。M10 走一条根本不同的路:

- **ACP 是单一标准协议**(JSON-RPC 2.0 over stdio,wire v1,类 LSP),一套 client 实现即可对接所有 ACP
  agent(Gemini/OpenCode 原生;Claude/Codex 经 Zed 的 adapter 进程)。因此 M10 **只做一个 `AcpAdapter`**,
  不做「每家一个」,启动命令由 `AcpConfig` 区分,wire 层复用。
- **用官方 crate**(`agent-client-protocol` + `agent-client-protocol-schema`)而非手写 JSON-RPC:提高兼容性
  与正确性(Zed 侧测试充分),协议类型只在 adapter 层用、不外泄为稳定 API。
- **凭据边界在被包装的 CLI 侧,不在本库**(本机实测):三个 bridge 都从各自 CLI 的配置/登录态取凭据——
  `claude-agent-acp`(内嵌 Claude Agent SDK)读 `~/.claude`、`codex-acp`(spawn `codex app-server`)读
  `~/.codex`(含 `auth.json`)、`opencode acp`(opencode 子命令)读 `~/.config/opencode`,均**不用**官方
  API/key。`AcpConfig` 因此不承载 API key。
- **配置能力:能继承 + 能注入/替换**(非「必须继承」)。adapter 默认继承宿主完整 env(缺省配置开箱即用),
  同时 `AcpConfig` 必须能显式**注入/替换**配置来源——这是本地测试的硬需求,因为本机三个 agent 都不用缺省
  配置。实测覆盖入口:Codex 用 `CODEX_HOME`/`CODEX_CONFIG`/`CODEX_PATH`(+底层 `codex -c key=value`),
  OpenCode 用 `OPENCODE_CONFIG`/`OPENCODE_CONFIG_DIR`/`OPENCODE_CONFIG_CONTENT`/`XDG_CONFIG_HOME`,Claude 用
  `claude --settings <file-or-json>`。e2e 通过这些入口指向测试专用配置,未设则走继承——与 M6-M8 的凭据边界
  处理一致。
- **feature gate `external-acp`**:官方 crate 作为 optional dependency,默认关闭;默认构建/测试不拉入 ACP
  依赖、不依赖任何 ACP agent binary。
- **首次点亮 host-pausable 臂**:ACP 的 `session/request_permission`(agent→client request)天然映射到
  machine 早已实现的 `PausedForInteraction`→`NeedInteraction`→`RespondInteraction`,使 `permission_bridge`
  第一次为 `true`。`fs/*` / `terminal/*` 作为 client 侧环境服务由 adapter 直接对 worktree 兑现并汇报为
  observation,不折成 `NeedTool`;`host_tools`(经 client MCP)留作后续能力,首版保守 `false`。
- **不改 machine/driver/state**:复用 milestone-5 抽象与 `ExternalRuntimeKind::Custom(String)`,与 M6-M8
  同构地挂进 `ExternalRuntimeAdapter` / `ExternalRuntimeSession` / `ExternalSessionRegistry`。

M10 完成后,facade API(见 [`docs/facade-api.md`](docs/facade-api.md) §11 Managed External Agent 集成、
§11.3 能力分级)可把 ACP agent 作为一个 `ExternalRunMode::Managed`(乃至 permission bridge 打开的更高档)
external delegate 暴露:facade 的 `ExternalRunMode` / `ExternalAgentCapabilities` 分级应能表达「ACP =
标准协议、permission bridge 可用」这一层,承接关系记入 H-1 之后的 facade 计划。

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
- **ACP 官方 crate / wire 版本漂移**：`agent-client-protocol*` 仍在演进,crate API 与 ACP wire 版本各自独立
  升级。缓解:crate 只在 adapter 层用、raw 类型不外泄为稳定 API;capability 经 `initialize` 协商而非硬编码;
  cassette + fake-transport 覆盖归一化路径;wire 版本作为探测项记录。
- **ACP agent 能力不对称**:wire 一致但 feature parity 不保证(`loadSession`/fs/terminal 均可选)。缓解:
  复用 `ExternalRuntimeCapabilities` + `with_probed_capabilities` 逐位取交,未协商到的位不假装支持,
  不支持的请求 fail fast(`UnsupportedCapability`)。
