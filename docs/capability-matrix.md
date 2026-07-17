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
支持什么」,仍**不是** e2e 实测;decoder/live session 待 M7-2/M7-3,故下表 Codex 行仍保持保守 `false`。

### 受管能力清单（`ExternalCapability`，共 8 项）

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

`ExternalCapability::ALL` 固定按上表顺序穷举全部 8 项，供能力矩阵与 round-trip 断言使用；
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

「未验证」= 保守基线返回 `false`，尚无探测/adapter 覆盖；这是待填表，接入真实 runtime
adapter（里程碑 5-8）后才逐项翻真并注明验证来源。**不要把此表当成任一 runtime 的服务等级
承诺。**

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
  作为转发给底层 runtime 的 hint——`permission_mode`、`isolation`（worktree）、`max_turns`
  （runtime 侧回合上限）、`stream_events`。它描述「希望 runtime 怎么跑」。
- **`ExternalAgentMachineConfig`（machine-local）**：只由 machine 自己在把 runtime pause 桥成 host
  requirement 时强制执行——`tool_failure`、`required_capabilities`、`max_decision_loops`
  （machine 侧决策循环上限，区别于 runtime 侧 `max_turns`）。它是纯数据 serde DTO，不含任何
  live handler / sink / id 源，因此**不进入**可序列化的 `ExternalAgentState`，与 live 身份源
  （`RequirementIds` / `ToolExecutionIds`）各自通过 builder 注入而互不污染。

两者的 `Default` 都刻意保守/宽松：不显式配置时 machine 行为与引入该配置前完全一致。
