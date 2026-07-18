# Capability 能力矩阵与逃生舱实证

本文记录 `agent-lib` 当前两种 wire protocol 的 `Capability` 默认值，并把这些协议级
默认值与 2026-07-13 在 Microsoft Foundry 部署上的实测范围分开说明。这样，调用方既能
看到库公开的默认能力，也不会把尚未针对具体 model/deployment 验证的能力误认为运行时
保证。

默认值的唯一代码来源是
[`src/client/capability.rs`](../src/client/capability.rs)。`AnthropicAdapter` 与
`OpenAiRespAdapter` 的 `LlmClient::capability()` 分别返回对应默认表项。当前没有运行时
能力探测；部署、模型或 API 版本有差异时，调用方应克隆默认值并应用自己的覆盖。

除 LLM wire protocol 外，本文末尾另有一节
[Managed External Runtime 能力模型](#managed-external-runtime-能力模型)，描述受管外部 agent
（`ExternalAgentMachine`）所用的、与上述互不相同的 `ExternalCapability` 模型及其保守默认。

## 协议级默认值

| `Capability` 字段 | Anthropic Messages 默认值 | OpenAI Responses 默认值 |
|---|---|---|
| `max_context_tokens` | `None` | `None` |
| `input_modalities` | `Text`, `Image`, `File` | `Text`, `Image`, `Audio`, `File` |
| `output_modalities` | `Text` | `Text`, `Audio` |
| `streaming` | `true` | `true` |
| `tool_calling` | `true` | `true` |
| `parallel_tool_calls` | `true` | `true` |
| `prompt_caching` | `true` | `true` |
| `reasoning` | `true` | `true` |
| `structured_output` | `true` | `true` |
| `stop_reasons` | `ToolUse`, `EndTurn`, `MaxTokens`, `StopSequence`, `Refusal` | `ToolUse`, `EndTurn`, `MaxTokens`, `Refusal` |

`max_context_tokens` 有意保持未知：context window 属于具体模型和部署，而不是 wire
protocol 的固定属性。集合使用 `BTreeSet`，因此序列化与测试顺序稳定。这里的 modality
集合描述协议能力上界；当前公共 `ContentBlock` 和 adapter 映射是否已经为某种输入形态
提供一等类型，应以对应公共 API 为准。尚未实测的默认值也不是特定 Foundry 部署的服务
等级承诺。

## 当前 Foundry 部署的实测范围

真实集成测试使用已归档
[`Client 层 PLAN.md`](archive/2026-07-13-client-layer/PLAN.md) 所列的两个 endpoint：
Anthropic Messages wire 的
`databricks-claude-haiku-4-5`，以及 OpenAI Responses wire 的 `gpt-5.5`。测试不会记录
认证值。下表中的“未实测”表示默认表仍声明协议支持，但本轮验收没有据此推断具体部署
一定支持。

| 能力 | Anthropic Messages / Foundry | OpenAI Responses / Foundry |
|---|---|---|
| 文本输入与输出 | 非流式、流式和多轮均已实测 | 非流式、流式和多轮均已实测 |
| 图片、音频、文件 | 本轮真实 endpoint 未实测 | 本轮真实 endpoint 未实测 |
| tool calling | 单次 tool call 与 tool result 回灌已实测；原始 `tool_use` stop reason 保留 | 单次 function call 与 result 回灌已实测；终态 `completed` 保留并归一为 `ToolUse` |
| parallel tool calls | 交错 block 的归一化与折叠由 fixture 测试覆盖；真实 endpoint 未实测 | 交错 item 的归一化与折叠由 fixture 测试覆盖；真实 endpoint 未实测 |
| streaming | text 与 tool SSE 已实测，Anthropic `index` 映射为稳定 `BlockId` | text 与 tool SSE 已实测，`item_id`/`output_index` 映射为稳定 `BlockId` |
| prompt caching | 实际响应含 cache creation/read 计数及 `cache_creation` 明细；本次样本的 creation/read 计数为 0 | 实际响应含 `input_tokens_details.cached_tokens`，样本归一为 `cache_read = 4` |
| reasoning/thinking | thinking、signature 和增量映射由协议 fixture 覆盖；当前真实 endpoint 场景未要求 thinking | 实际响应含 reasoning item，并报告 `reasoning_tokens = 18` |
| structured output | 协议默认值为支持；当前真实 endpoint 场景未实测 | 协议默认值为支持；当前真实 endpoint 场景未实测 |
| 已观察终止原因 | `end_turn`, `tool_use` | `completed`，根据输出内容归一为 `EndTurn` 或 `ToolUse` |

真实连通与归一化矩阵位于
[`tests/integration_normalization.rs`](../tests/integration_normalization.rs)，默认标记为
`#[ignore]`，仅在提供 `.envrc` 所述配置时访问 endpoint。协议边界、错误路径以及尚未由
真实调用覆盖的 stop reason 由录制 fixture 和合成单元测试验证。

## 响应侧逃生舱实证

响应侧逃生舱遵循 `DESIGN.md` 的机制 B：已建模字段进入 provider-neutral 字段，未建模
字段保留在最接近语义位置的 `extra: Map<String, Value>` 中。保留原始 JSON 值，而不是
只留下“曾出现过该字段”的布尔证据。

| Provider 方言字段 | 原始位置 | 归一化后位置 | 已建模的相关字段 |
|---|---|---|---|
| Foundry Anthropic cache creation 明细 | `usage.cache_creation.ephemeral_5m_input_tokens` / `ephemeral_1h_input_tokens` | `Response.usage.extra["cache_creation"]`，完整对象保留 | `cache_creation_input_tokens` → `Usage.cache_write`; `cache_read_input_tokens` → `Usage.cache_read` |
| Azure OpenAI 内容过滤结果 | 顶层 `content_filters[]`，含 prompt/completion 的分类、offset 与原始结果 | `Response.extra["content_filters"]`，完整数组保留 | 被阻断的响应同时归一为 `StopReason::Refusal`，但原始过滤证据不删除 |

[`tests/capability_escape_hatches.rs`](../tests/capability_escape_hatches.rs) 使用脱敏的真实响应
fixture，通过公开的 adapter 解析 API 做以下断言：

1. 原始 Anthropic `usage.cache_creation` 对象与 `Usage.extra` 中的值全等。
2. 原始 Azure `content_filters` 数组与 `Response.extra` 中的值全等。
3. 两种响应经过 `Response` 的 serde 序列化与反序列化后，上述值仍全等。

因此，归一化字段可供跨 provider 逻辑直接使用，调用方同时仍能检查具体 endpoint 的
方言证据；新增 provider 字段不会因为当前模型尚未认识它们而静默丢失。

## Managed External Runtime 能力模型

上面两节描述的是 **LLM wire protocol** 的 `Capability`（`agent-lib` 直接说 provider API 时的
默认能力）。受管外部 agent（`ExternalAgentMachine` 驱动 Claude Code / Codex / OpenCode 等
CLI/进程 runtime）用的是另一套、互不相同的能力模型：它回答的不是「某个 model 支持什么
modality」，而是「某个具体 runtime session 能否兑现某个受管特性」——实时流式、会话
resume、host 工具注入、subagent 桥接等。这套模型的唯一代码来源是
[`src/agent/external/capability.rs`](../src/agent/external/capability.rs) 的
`ExternalCapability` 与 `ExternalRuntimeCapabilities`。

**保守基线（关键约束）**：受管 runtime 从不假设支持任何特性。
`ExternalRuntimeKind::conservative_capabilities()` /
`ExternalRuntimeCapabilities::none(runtime)` 把每个特性都置为 `false`；只有真实探测或 adapter
声明才会逐字段打开（design §15，PLAN 非目标「能力差异通过 capability model 显式暴露，不能
静默假装支持」）。**默认构建（未启 `external-claude-code` 等 adapter feature）下 crate 仍不接入
任何真实 runtime 探测或 adapter，下表所有 runtime 的每一项都保持 `unsupported`，本文不声称任何
已验证的 runtime 支持。** 里程碑 6 起，feature-gated 的 Claude Code adapter 提供了一个
**capability probe**（`external-claude-code` 下的
[`agent::external::probe`](../src/agent/external/claude_code/probe.rs)）：它调用 `claude --version`
/ `--help`，把缺失/损坏的 binary 分类为 `Launch`、把不支持 `stream-json` 的 CLI 分类为
`UnsupportedCapability{Streaming}`，并从 `--help` 广告出的开关**保守探测**能力位。该探测反映的是
「CLI 自称支持什么」,仍**不是** e2e 实测。里程碑 6-2 又补上了 feature-gated 的私有 `stream-json`
**decoder**（[`ClaudeStreamDecoder`](../src/agent/external/claude_code/decoder.rs)）,它离线地把 CLI
帧解成中立观测/决策并有 committed cassette 回归。里程碑 6-3 再把配方 + decoder 接成 feature-gated 的
**live session adapter**（[`ClaudeCodeAdapter`](../src/agent/external/claude_code/adapter.rs)）:它
`start`/`resume`/`advance`/`shutdown` 真实 CLI 会话,并把观测镜像到 live sink。其状态机由注入的 fake
transport **离线**单测跑通,真机路径则由 `#[ignore]` 的
[`tests/external_claude_code.rs`](../tests/external_claude_code.rs) 覆盖(通过 `CLAUDE_CODE_BIN`/PATH
发现 `claude`,缺失即跳过)。因为「实测」以真实会话跑通为准,该 e2e 未在本环境实际执行前,下表 Claude
Code 行仍保持保守 `false`;待该 e2e 在具备 Claude Code 登录的机器上跑绿后再逐项翻真。注意:M6-3 的
adapter **不 bridge 宿主工具**(不跑 MCP server),故即便实测,`host_tools`/`host_subagents` 也维持
`false`(spec §12.3 允许),并对声明了工具的请求以 `UnsupportedCapability{HostTools}` 明确拒绝。

里程碑 7-1 起,feature-gated 的 Codex adapter 同样提供了一个 **capability probe**（`external-codex`
下的 [`agent::external::codex_probe`](../src/agent/external/codex/probe.rs)）:它以**当前本机 Codex CLI
（v0.144.1）实测 `--version` / `--help` / `exec --help` 为准**,把缺失/损坏的 binary 分类为 `Launch`、
把 `codex exec` 无 `--json` 结构化事件流的 CLI 分类为 `UnsupportedCapability{Streaming}`,并从两份 help
广告出的开关**保守探测**能力位(streaming←`exec --json`,permission_bridge←`--ask-for-approval`/
`--sandbox`,resume←`resume` 子命令,host_tools←顶层 `mcp`,usage/artifacts←结构化流)。配套的
[`CodexConfig`](../src/agent/external/codex/config.rs) 把 `ExternalPermissionMode` 映射到当前 CLI 词汇的
approval（`untrusted`/`on-request`/`never`）+ sandbox（`read-only`/`workspace-write`/
`danger-full-access`),并保证顶层全局 flag 排在 `exec` 子命令之前。与 Claude 一样,该探测反映「CLI 自称
支持什么」,仍**不是** e2e 实测。

里程碑 7-2 起,Codex adapter 也落地了离线的私有 **decoder**
（[`CodexStreamDecoder`](../src/agent/external/codex/decoder.rs)）:它以**当前本机 Codex CLI(v0.144.1)
实测 `codex exec --json` 输出为准**(该流是 `ThreadEvent` JSONL),离线地把 CLI 帧解成中立观测
/决策并有 committed cassette(`tests/fixtures/external/codex/full_session.json`)回归。要点:codex exec 自主
运行,自己执行工具(含 MCP)、按预设策略内部解决审批,故 decoder 每 turn 只落定 `Completed`/`Failed`,exec
`--json` 流里**没有** host-pausable 的 tool-call / permission 帧(被拒动作表现为 `command_execution`
`declined`)。

里程碑 7-3 起,Codex adapter 把配方 + decoder 接成 feature-gated 的 **live session adapter**
（[`CodexAdapter`](../src/agent/external/codex/adapter.rs)）:它 `start`/`resume`/`advance`/`shutdown` 真实
`codex exec` 会话,并把观测镜像到 live sink。**关键差异**:`codex exec` 每 turn 一个一次性进程(prompt 是 CLI
位置参数,不是 stdin 帧),续跑是全新的 `codex exec resume <thread_id> <message>` 进程;为此新增
`CodexConfig::base_resume_args()` 把 `resume` 子命令不接受的 `-s`/`-p` 上提到顶层。其状态机由注入的
`FakeLauncher` **离线**单测跑通(fresh + resume + 拒绝 host-bridge 输入),真机路径由 `#[ignore]` 的
[`tests/external_codex.rs`](../tests/external_codex.rs) 覆盖(通过 `CODEX_BIN`/PATH 发现 `codex`,缺失即跳过),
本机 codex-cli 0.144.1 **实跑通过**(以 `AcceptEdits`/`workspace-write` 生成 `READY.txt`,5 个观测事件、优雅
关闭)。因 `codex exec --json` 自主运行、流里没有 host-pausable 帧,该 adapter 诚实报告
`host_tools`/`host_subagents`/`permission_bridge` **恒 `false`**(声明工具的请求以
`UnsupportedCapability{HostTools}` 拒绝,follow-up 的 `Respond*` 也明确拒绝),其余
`streaming`/`resume`/`artifacts`/`usage`/`graceful_shutdown` 由 `new()` 报告为 `true`、或经
`with_probed_capabilities` 与本机 probe 逐位取交。下表是保守基线(`none()`),不代表 adapter 的实报能力。

**Codex live adapter 实报能力（M7-4 review 定案，feature `external-codex`）**——下表列的是
`CodexAdapter::new()` **实报**的能力(与末尾「各 runtime 当前声明」的默认构建保守基线 `none()`
不同),并标注每项的验证来源。`with_probed_capabilities` 会把这些实报位与本机 probe 逐位 AND,
故某项能力只有在 adapter 实现且 probe 也广告时才最终为 `true`;host bridge 三项无论 probe 如何都恒 `false`。

| 受管能力 | `CodexAdapter::new()` 实报 | 验证来源 |
|---|---|---|
| streaming | `true` | 真机 e2e(观测流 SessionStarted + ≥1 TextDelta + SessionCompleted,≥3 事件,镜像到 live sink)+ 离线单测 |
| resume | `true` | 离线单测(`exec resume <thread_id>` fresh+follow-up 顺序、defer 首 turn、记录 thread id);真机 e2e 单 turn 未跑 resume |
| artifacts | `true` | 离线 decoder cassette(`file_change`→`FilePatch`);真机 e2e prompt 生成 `READY.txt` |
| usage | `true` | 离线 decoder cassette(`turn.completed.usage`→`ExternalAgentOutput.usage`)+ 单测断言 `output.usage.is_some()` |
| graceful_shutdown | `true` | 真机 e2e(`registry.cleanup` 断言 `Graceful`)+ 离线单测(close 分类 Graceful/ForcedKill) |
| permission_bridge | `false`(恒) | `codex exec --json` 自主运行、按预设策略解审批,流里无 host-answerable pause;`RespondInteraction`→`UnsupportedCapability{PermissionBridge}` |
| host_tools | `false`(恒) | exec 自主执行工具,无 host-pausable tool-call 帧;声明 `tools` 的 start/resume 与 `RespondToolResults` 均以 `UnsupportedCapability{HostTools}` 拒绝 |
| host_subagents | `false`(恒) | 无 host-桥接的 spawn 帧;`RespondSubagent`→`UnsupportedCapability{HostSubagents}` |
| reconfigure | `false`(恒) | live 会话 mid-turn 热替换未实现;机器仅做 turn-boundary reconfig(`ExternalReconfigTiming::Hot` 在 in-flight 时→`UnsupportedCapability{Reconfigure}`),boundary 级换集不需要此能力 |

真机 e2e 状态:**本机 codex-cli 0.144.1 实跑通过**(`tests/external_codex.rs`,以
`AcceptEdits`/`workspace-write` 驱动 probe→start→advance→completion→graceful shutdown,生成
`READY.txt`,5 个观测事件、约 51s);缺 binary/登录时该 `#[ignore]` 测试自跳过(退出为绿)。

里程碑 8-1 起,feature-gated 的 OpenCode adapter 同样提供了一个 **capability probe**（`external-opencode`
下的 [`agent::external::opencode_probe`](../src/agent/external/opencode/probe.rs)）:它以**当前本机
`opencode` CLI 实测 `--version` / `--help` / `run --help` 为准**,把缺失/损坏的 binary 分类为 `Launch`、
把 `opencode run` 无 `--format json` 结构化事件流的 CLI 分类为 `UnsupportedCapability{Streaming}`,并从两份
help 广告出的开关**保守探测**能力位(streaming←`run --format`+`json`,permission_bridge←`run --auto`,
resume←`run --continue`/`--session` 或顶层 `session`,host_tools←顶层 `mcp`,usage/artifacts←结构化流,
host_subagents 保守 `false`)。配套的 [`OpenCodeConfig`](../src/agent/external/opencode/config.rs) 把
`ExternalPermissionMode` 保守映射到 `run` 唯一的权限旁路开关 `--auto`(**仅 `BypassPermissions` 发
`--auto`**,其余模式不加以免越权放宽),更细的 read-only/accept-edits 交给 `--agent` 预设 agent。与
Claude/Codex 一样,该探测反映「CLI 自称支持什么」,仍**不是** e2e 实测(真机实报见下方 M8-3 表);末尾
「各 runtime 当前声明」的保守基线 `none()` 表不随之翻真。

里程碑 8-2 起,`external-opencode` 下新增了 adapter 私有的 `opencode run --format json` **stream decoder**
([`OpenCodeStreamDecoder`](../src/agent/external/opencode/decoder.rs)):它把 CLI 逐行输出的
`{ type, timestamp, sessionID, ... }` 事件信封(`text` / `tool_use` / `step_start` / `step_finish` /
`reasoning` / `error`)归一化成 sequenced [`ExternalObservedEvent`](../src/agent/external/mod.rs) 与
per-turn `OpenCodeDecision`。与 `codex exec --json` 一样,`run --format json` **自主运行**——权限提示按
`--auto` 启动开关裁决,不回灌 host——故 decoder 无 host-pausable 决策臂:一个 turn 只会 `Completed`
(终结 `step_finish`,跨步累加 usage)或 `Failed`(顶层 `error`)。被权限拒绝的工具在流里表现为
`state.status = error` 且错误串是 OpenCode 稳定的 `PermissionRejectedError`/`PermissionDeniedError`
文案,decoder 据此发**信息型** `PermissionRequested` 观测(runtime 已裁决,host 无需应答)。私有 wire
schema 不外泄为稳定 API,回归由离线 cassette([`tests/agent_opencode_cassette.rs`](../tests/agent_opencode_cassette.rs) +
`tests/fixtures/external/opencode/full_session.json`)冻结,覆盖 text/command/patch/permission/
tool/subtask/completion/error;live adapter 与真机 e2e 仍待 M8-3。

里程碑 8-3 起,`external-opencode` 把 M8-1 配方与 M8-2 decoder 接成 feature-gated 的 **live session adapter**
([`OpenCodeAdapter`](../src/agent/external/opencode/adapter.rs)):它 `start`/`resume`/`advance`/`shutdown`
真实 `opencode run` 进程。与 Codex 同构——`run` 的 prompt 是 CLI 位置参数、进程一 turn 落定即退出,续跑是全新
`opencode run --session <id> <message>` 进程(新增配套 `OpenCodeConfig::base_resume_args()` = 复用
`base_run_args()` 追加 `--session <id>`)。与 Codex 的差异:OpenCode **无 init 帧**,session id 随每帧
`sessionID` 到达,故 `begin` 读到 decoder 惰性捕获首个 `sessionID`(并发 `SessionStarted`)为止。其状态机由注入的
`OpenCodeLauncher`/`OpenCodeTurnStream` trait 离线跑通(fresh+resume+shutdown),生产 `SystemOpenCodeLauncher`
用 `tokio::process`(stdin=null、stderr 丢弃、每读超时、kill_on_drop)。

**OpenCode live adapter 实报能力（M8-3 落地、M8-4 review 定案，feature `external-opencode`）**——下表列的是
`OpenCodeAdapter::new()` **实报**的能力(与末尾「各 runtime 当前声明」的保守基线 `none()` 不同);
`with_probed_capabilities` 会把这些实报位与本机 probe 逐位 AND,故某项能力只有 adapter 实现且 probe 也广告时
才最终为 `true`;host bridge 三项无论 probe 如何都恒 `false`。

| 受管能力 | `OpenCodeAdapter::new()` 实报 | 验证来源 |
|---|---|---|
| streaming | `true` | 真机 e2e(观测流 SessionStarted + ≥1 TextDelta + SessionCompleted,≥3 事件,镜像到 live sink)+ 离线单测 |
| resume | `true` | 离线单测(`run --session <id>` fresh+follow-up 顺序、defer 首 turn、pre-seed session id);真机 e2e 单 turn 未跑 resume |
| artifacts | `true` | 离线 decoder cassette(`edit`/`write`→`FilePatch`);真机 e2e prompt 在 `--dir` worktree 内生成 `READY.txt`(并断言不泄漏进启动 checkout) |
| usage | `true` | 离线 decoder cassette(`step_finish.tokens/cost` 跨步累加→`ExternalAgentOutput.usage`)+ 单测断言 `output.usage.is_some()` |
| graceful_shutdown | `true` | 真机 e2e(`registry.cleanup` 断言 `Graceful`)+ 离线单测(close 分类 Graceful/ForcedKill) |
| permission_bridge | `false`(恒) | `run --format json` 自主运行、按 `--auto` 解审批,流里无 host-answerable pause;`RespondInteraction`→`UnsupportedCapability{PermissionBridge}` |
| host_tools | `false`(恒) | run 自主执行工具,无 host-pausable tool-call 帧;声明 `tools` 的 start/resume 与 `RespondToolResults` 均以 `UnsupportedCapability{HostTools}` 拒绝 |
| host_subagents | `false`(恒) | 无 host-桥接的 spawn 帧;`RespondSubagent`→`UnsupportedCapability{HostSubagents}` |
| reconfigure | `false`(恒) | live 会话 mid-turn 热替换未实现;机器仅做 turn-boundary reconfig(`ExternalReconfigTiming::Hot` 在 in-flight 时→`UnsupportedCapability{Reconfigure}`),boundary 级换集不需要此能力 |

真机 e2e 状态:**本机 opencode 1.17.15 实跑通过**([`tests/external_opencode.rs`](../tests/external_opencode.rs),
以 `BypassPermissions`/`--auto` 驱动 probe→start→advance→completion→graceful shutdown,在 `--dir` 临时
worktree 内生成 `READY.txt` 并断言其**不泄漏**进启动它的 checkout(worktree 隔离,详见 §14),
6 个观测事件、约 20s);缺 binary/登录时该 `#[ignore]` 测试自跳过(退出为绿)。

### 三个 runtime adapter 统一接入路径对照（M8-4 review 定案）

OpenCode adapter 落地后三个目标 runtime（Claude Code / Codex / OpenCode）都走同一条受管接入路径。
M8-4 review 逐项核对了源码，确认四个维度一致，唯一差异是 Claude Code 的常驻进程 + permission bridge：

| 维度 | Claude Code | Codex | OpenCode | 一致性 |
|---|---|---|---|---|
| 进程模型 | 常驻 stdio 进程（stdin 帧续跑） | 一进程/一 turn（`exec resume`） | 一进程/一 turn（`run --session`） | Codex≡OpenCode 同构；Claude 常驻（设计意图） |
| `ExternalRuntimeAdapter` | kind/capabilities/start/resume | 同 | 同 | ✓ 一致 |
| `ExternalRuntimeSession` | session_ref/advance/shutdown | 同 | 同 | ✓ 一致 |
| decision 臂（`finish`） | Completed/Failed/**PausedForToolCalls**/**PausedForInteraction** | Completed/Failed | Completed/Failed | Claude 多 host-pausable 臂（permission bridge） |
| capability fallback | `new()`→implemented；`with_probed_capabilities()`→逐位 AND | 同 | 同 | ✓ 一致 helper |
| host-tool 门禁 | `reject_unsupported_tools` + `turn_message` 拒绝 | 同 | 同 | ✓ 一致（`UnsupportedCapability`/`Protocol`） |
| `permission_bridge` | `true`（权限控制通道） | `false`（自主） | `false`（自主） | **唯一能力差异**（诚实暴露） |
| 其余能力 | streaming/resume/artifacts/usage/graceful=`true`；host_tools/host_subagents=`false` | 同 | 同 | ✓ 一致 |
| parser cassette 层级 | 7 层（regenerate/matches/secret-free/full-session/tolerates-unknown/rejects-malformed/decodes-failed）+ committed fixture | 同 7 层 | 同 7 层 | ✓ 一致 |
| inline adapter 单测 | advance/session-lost/protocol-error/shutdown/respond-unsupported/resume-defers/rejects-declared-tools/caps | 同 | 同（多 `resume_survives_a_session_that_never_re_reports_its_id`，因无 init 帧） | ✓ 一致 |
| cleanup（`advance`） | `ctx.is_cancelled()`→`SessionLost` | 同 | 同 | ✓ 一致 |
| cleanup（`shutdown`） | 关 io→`ExternalSessionShutdown` | 关 stream/Graceful | 关 stream/Graceful | ✓ 一致语义 |
| trace | 不自发 tracing，经 `RunContext` trace node 透传 | 同 | 同 | ✓ 一致 |
| 真机 e2e（`#[ignore]`，green-skip） | 待具备登录的机器跑绿 | codex-cli 0.144.1 实跑通过 | opencode 1.17.15 实跑通过 | ✓ 结构一致（envrc/command_available/drive_session） |

**结论**：三个 adapter 的 trait 实现、capability fallback、parser cassette 覆盖层级、cleanup/trace
均一致；`permission_bridge`（Claude=`true`，Codex/OpenCode=`false`）与 Claude 的常驻进程/host-pausable
决策臂是唯一（且设计内的）差异，已由 capability model 显式暴露、未静默假装支持。OpenCode
支持能力=`streaming`/`resume`/`artifacts`/`usage`/`graceful_shutdown`；不支持=`permission_bridge`/
`host_tools`/`host_subagents`（恒 `false`）。真机 e2e：本机 opencode 1.17.15 实跑通过。

### 受管能力清单（`ExternalCapability`，共 9 项）

| `ExternalCapability` | serde 标签 | 覆盖的决策点 / 旁路 | 保守默认 |
|---|---|---|---|
| `Streaming` | `streaming` | 把细粒度事件转发给 live [`ExternalEventSink`](../src/agent/external/sink.rs) | `false` |
| `Resume` | `resume` | 用存储的 session ref/token 续跑既有会话 | `false` |
| `PermissionBridge` | `permission_bridge` | 把 runtime 权限/交互 pause 桥成 host approval | `false` |
| `HostTools` | `host_tools` | 注入 host 提供、runtime 可调用的工具 | `false` |
| `HostSubagents` | `host_subagents` | 把 runtime spawn 请求桥成 host 管理的 subagent | `false` |
| `Artifacts` | `artifacts` | 把产出的 artifact（patch/文件）回报给 host | `false` |
| `Usage` | `usage` | 回报 token/cost usage 供 budget charging | `false` |
| `GracefulShutdown` | `graceful_shutdown` | 无残留副作用地干净关闭会话 | `false` |
| `Reconfigure` | `reconfigure` | 对 *live* 会话 mid-turn 热替换工具集（live tool-bridge swap）；boundary 级 reconfig 不需要此能力 | `false` |

`ExternalCapability::ALL` 固定按上表顺序穷举全部 9 项，供能力矩阵与 round-trip 断言使用；
新增能力只需扩展该数组即被自动覆盖。`ExternalRuntimeCapabilities` 为每一项持有同名 `bool`
字段，`supports(cap)` 逐项映射，`none(runtime)` 为全 `false` 的保守起点。

### 各 runtime 当前声明（全部保守 = 未验证）

| 受管能力 | Claude Code | Codex | OpenCode | Custom |
|---|---|---|---|---|
| streaming | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） |
| resume | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） |
| permission_bridge | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） |
| host_tools | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） |
| host_subagents | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） |
| artifacts | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） |
| usage | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） |
| graceful_shutdown | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） |
| reconfigure | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） | 未验证（`false`） |

「未验证」= 保守基线返回 `false`，尚无探测/adapter 覆盖；这是待填表，接入真实 runtime
adapter（里程碑 5-8）后才逐项翻真并注明验证来源。**不要把此表当成任一 runtime 的服务等级
承诺。**

### 能力来源：declared vs probed（facade §11.3）

facade 的 `ExternalAgentCapabilities` 除了持有上表 9 个 `bool`，还带一个
`CapabilitySource` 记录**这份能力视图是怎么来的**，避免把「adapter 声称支持」与「我们已
验证支持」混为一谈：

| `CapabilitySource` | 含义 | 谁产生 |
|---|---|---|
| `Declared` | adapter / preset 的**静态声明**，保守起点，未对 live runtime 验证 | preset 构造器（`claude_code()` / `codex()` / …）、快照 restore、ACP pre-negotiation 基线 |
| `Supplied` | **调用方**经 `.capabilities(..)` 手工提供(可能自带外部探测结果) | `from_runtime_capabilities(..)` / `supplied(..)` |
| `Probed` | **探测 live CLI runtime**得到,反映本机二进制实际广告的能力位 | `build_with_default_session_handler()` / `default_external_session_handler_with_capabilities()` 折入 |
| `Negotiated` | 经 ACP `initialize` 握手协商得到 | `.acp_negotiated(..)`（feature `external-acp`） |

关键区别与保证:

- **declared 是保守猜测,probed/negotiated 才是验证过的真相。** preset 构造出的 agent 持
  `Declared` 视图(例如 Claude Code declared 广告 `permission_bridge=true`)。这只是「adapter 打算
  支持」,不是「本机 CLI 一定支持」。
- **一步式装配折入 probed 视图。** `ManagedExternalAgentBuilder::build_with_default_session_handler()`
  probe 成功后,把探到的 `ExternalRuntimeCapabilities` 折回 agent 的能力视图并标 `Probed`,取代
  declared 基线;之后 `agent.capabilities()` / `require_capability(..)` / `ExternalRunMode` 校验都以
  probed 为准。若 probed 比 declared 窄(某能力 declared 声称支持、probe 未证实),**以 probed 为准**——
  该能力会被 `require_capability(..)` 以
  `FacadeError::UnsupportedExternalCapability { runtime, capability, capability_source: "probed" }` 拒绝,
  requested `ExternalRunMode` 也会按 probed 视图**重新**校验(缺能力则 `UnsupportedExternalMode`,source 标 `probed`)。
- **ACP 无离线 probe。** ACP 能力经 live `initialize` 每会话协商,故一步式装配对 ACP **不**覆盖能力视图
  (保留 declared/negotiated),由 `.acp_negotiated(..)` 折入 `Negotiated`。
- **来源标签进错误信息,便于诊断且不含 secret。** `UnsupportedExternalMode` /
  `UnsupportedExternalCapability` 都带 `capability_source`(`declared` / `supplied` / `probed` /
  `negotiated`),让调用方一眼看出当前判断基于保守基线还是已验证档位;标签为稳定字符串,绝不
  嵌入 runtime 输出、启动命令行或凭据。

### 能力缺失时的 fallback 策略

`ExternalAgentMachine` 遇到某个 runtime 无法兑现的决策点时不静默降级，而是按下述策略明确
处置（源码：[`machine.rs`](../src/agent/external/machine.rs)、
[`config.rs`](../src/agent/external/config.rs)）：

- **声明为 required 的能力缺失** → 分类错误
  `ExternalAgentError::UnsupportedCapability { runtime, capability, detail }`。
  运行方通过 `ExternalAgentMachineConfig::require_host_tools()` /
  `require_subagents()` / `require_capability(cap)` 声明本次 run 依赖的能力；缺失时错误显式
  命名 runtime 与 capability，scheduler 据此避免再次 dispatch 该 worker。
- **未声明 required 的能力缺失** → 保留原通用错误（例如缺 tool-call id 源时的
  `tool id unavailable`），与 pre-M4-3 行为兼容，不升级为能力分类错误。
- **host 工具调用失败** → 由 `ExternalToolFailurePolicy` 决定：`ReturnErrorToRuntime`（默认，把
  失败当作 failed tool result 回灌，runtime 自主决定后续）或 `StopRun`（停止 host turn）。
- **decision loop 超限** → 若配置了 `ExternalAgentMachineConfig::max_decision_loops`，超过即
  `ExternalAgentError::LimitExceeded`，防止无界 pause/respond 循环。
- **streaming 旁路** → 由 `ExternalStreamPolicy::{Buffered(默认)/Streaming/Disabled}` 控制。
  live sink 是**可丢弃**旁路：它绝不阻塞 continuation，可自由丢事件；exact-once 回放由
  `ExternalSessionResult::observations` 按 `seq` 去重独家保证，sink 只是这条权威流的 lossy
  实时镜像，永不替代它。

### 职责边界：runtime-facing policy vs machine-local config

受管配置刻意分成两半，二者不重叠：

- **`ExternalSessionPolicy`（runtime-facing）**：随每个 `ExternalSessionRequest` 传给 handler，
  描述「希望 runtime 怎么跑」——`permission_mode`、`isolation`（worktree）、`max_turns`
  （runtime 侧回合上限）、`stream_events`。M2-7 起每个字段都有指定消费方，不再是被忽略的
  hint：`permission_mode` 由 adapter 在 session start/resume 时应用（覆盖构造期 config）；
  `isolation` 由 `ExternalSessionRegistry` 经其 `WorktreeManager` 执行（prepared 路径作为
  `request.session_dir` 成为 session 工作目录）；`max_turns` 由 machine 统一强制为 runtime
  round-trip（decision loop）上限，超限以 `LimitExceeded` 失败，不传 CLI flag。
- **`ExternalAgentMachineConfig`（machine-local）**：只由 machine 自己在把 runtime pause 桥成 host
  requirement 时强制执行——`tool_failure`、`required_capabilities`、`max_decision_loops`
  （machine 侧决策循环上限，区别于 runtime 侧 `max_turns`）。它是纯数据 serde DTO，不含任何
  live handler / sink / id 源，因此**不进入**可序列化的 `ExternalAgentState`，与 live 身份源
  （`RequirementIds` / `ToolExecutionIds`）各自通过 builder 注入而互不污染。

两者的 `Default` 都刻意保守/宽松：不显式配置时 machine 行为与引入该配置前完全一致。

### 可运行示例与真机验证入口（M9-5）

上述受管路径的最小可运行装配见 `examples/`，它们都走 **scoped effect wiring**——经
`ExternalAgentMachine` + 作用域 `ExternalSessionHandler`（registry-backed）驱动真实 CLI，
**不**直接调 adapter 绕过 machine：

| 示例 | runtime | feature flag |
|---|---|---|
| [`examples/managed_claude_code.rs`](../examples/managed_claude_code.rs) | Claude Code | `external-claude-code` |
| [`examples/managed_codex.rs`](../examples/managed_codex.rs) | Codex | `external-codex` |
| [`examples/managed_opencode.rs`](../examples/managed_opencode.rs) | OpenCode | `external-opencode` |
| [`examples/managed_mixed.rs`](../examples/managed_mixed.rs) | Claude Code + Codex | 两者 |

共享装配在 [`examples/support/managed.rs`](../examples/support/managed.rs)。运行方式（含 env、CLI
override、worktree 隔离、secret 处理、缺失即 skip 的能力 fallback）汇总在根目录
[`AGENTS.md`](../AGENTS.md)。例如：

```text
cargo run --example managed_claude_code --features external-claude-code
cargo run --example managed_mixed --features "external-claude-code external-codex"
```

**门控与安全属性**（与上文能力模型一致）：

- **feature flags**:每个示例经 Cargo `required-features` 门控;默认 `cargo check --examples` /
  `cargo test --all --all-targets` 跳过它们,不引入任何 CLI-adapter 机制。
- **required env**:仅可选的 `CLAUDE_CODE_BIN`/`CODEX_BIN`/`OPENCODE_BIN`（binary 覆盖）与
  `*_MODEL`（模型覆盖）;CLI 自带登录态,示例从不读取或打印任何 secret（secret redaction）。
- **ignored e2e**:结构化真机回归见 `#[ignore]` 的 [`tests/external_claude_code.rs`](../tests/external_claude_code.rs)、
  [`tests/external_codex.rs`](../tests/external_codex.rs)、[`tests/external_opencode.rs`](../tests/external_opencode.rs);
  多 agent 混合受管路径见 [`tests/agent_external_managed_real_e2e.rs`](../tests/agent_external_managed_real_e2e.rs)
  （DeepSeek 协调器经 `NeedSubagent` 派生 Claude Code + Codex child,`--features "external-claude-code
  external-codex" -- --ignored`）。
- **worktree isolation**:`ExternalSessionPolicy.isolation` 由 `ExternalSessionRegistry` 经其
  `WorktreeManager`（默认 `GitWorktreeManager`）在库内执行（M2-7）——prepare 产出的路径作为
  session 工作目录贯通到各 adapter（OpenCode 同时贯通到 `--dir`），cleanup 按 shutdown
  disposition 决定删除/保留。facade 受管驱动声明 `EphemeralGitWorktree`，child 实际运行在
  per-session 临时 linked worktree；示例与 e2e 自建一次性 `git init` worktree 并声明
  `Shared`（host 拥有该目录，registry 不再另建），写文件绝不触碰启动它的 checkout
  （详见 [`managed-external-agent.md`](./managed-external-agent.md) §16）。
- **unsupported capability fallback**:CLI 缺失或 capability probe 失败时,示例打印非密提示并
  以退出码 0 **skip**;声明了未开启能力（如 host tools）的请求以 `UnsupportedCapability{..}` 显式拒绝,
  绝不静默降级。
