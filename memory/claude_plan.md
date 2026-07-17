# M10-2 用官方 crate 建立 ACP client 连接与 session/update 观测解码

**当前任务 = TODO.md 首个未完成 = M10-2**（`### [TODO] M10-2`, line 3356）。
M1..M10-1 全 `[DONE]`。下一个 `[TODO]` 是 M10-3 / M10-4，不属于本次。

## 目标（来自 TODO.md M10-2 "做什么"）

1. 在 `src/agent/external/acp/` 新增 client 连接层（feature-gated），封装官方 crate 的 stdio client：
   - 可注入 launcher trait（离线单测），生产用 tokio::process：stdin/stdout piped、stderr 丢弃、
     kill_on_drop、每读超时——与三个 CLI adapter 的 IO 纪律一致。
   - 把 `session/update` 归一化成 `ExternalObservedEvent`，跨 turn 单调分配 `seq`；
     把 `session/request_permission` / `fs/*` / `terminal/*` 到达**识别并缓存**（M10-3 处理）。
2. `session/update` → 观测映射（用已有词汇表，不新增变体）：
   - agent message chunk → `TextDelta`
   - tool_call 开始/update/完成 → `ToolStarted`/`ToolFinished`（Execute 类可 `CommandStarted`/`CommandFinished`）
   - diff / 文件变更 → `FilePatch`
   - plan / todo 更新 → `TaskUpdated`
   - session 建立 → `SessionStarted`（带 ACP session id）；turn 结束（prompt stopReason）→ `SessionCompleted`
3. 容忍/错误纪律（与 CLI decoder 一致）：协议错误/未建模 update → 容忍或 `ExternalAgentError::Protocol`；
   所有诊断为固定字符串，永不夹带 prompt/文件内容/凭据。

## 关键设计决策

- **不导出 crate raw 类型**：decoder 的公开入口只有 `push_jsonrpc_line(&str) -> Result<Option<AcpDecision>>`
  （& str 进、中立 `AcpDecision` 出）。消费 schema 类型（`SessionUpdate` / `StopReason`）的 typed 方法
  设为 `pub(crate)`，供连接层内部调用。`AcpDecision` / `PendingClientRequest` 均为中立类型。
- crate schema 版本：`agent-client-protocol-schema 1.4.0` 的 `v1` 模块（`SessionNotification` /
  `SessionUpdate` / `ToolCall` / `ToolCallUpdate` / `Diff` / `Plan` / `StopReason` / `RequestPermissionRequest`）。
  方法名常量在 schema crate 是 `pub(crate)`，本仓用字符串字面量（"session/update" 等）。
- **连接层 launcher**：`AcpLauncher` trait + `SpawnedAcpAgent`（AsyncRead/Write 抽象，可注入 fake）+
  `TokioProcessLauncher`（生产 spawn）。公开 re-export → 非 dead_code。
- **client 请求缓存**：`push_jsonrpc_line` 识别 `session/request_permission`（发 `PermissionRequested`
  观测 + 缓存）、`fs/*` / `terminal/*`（缓存），M10-2 不应答。`take_client_requests()` 排空。

## 文件

- 新增 `src/agent/external/acp/decoder.rs`：`AcpStreamDecoder` / `AcpDecision` / `PendingClientRequest`。
- 新增 `src/agent/external/acp/connection.rs`：`AcpLauncher` / `SpawnedAcpAgent` / `TokioProcessLauncher`。
- 改 `src/agent/external/acp/mod.rs`：挂载 + re-export。
- 改 `src/agent/external/mod.rs`（如需 re-export ACP 类型到 `agent::external`）。
- 新增 `tests/agent_acp_cassette.rs` + `tests/fixtures/external/acp/full_session.json` + README。

## 验证条件（TODO.md）

- `acp_session_update_maps_to_observations`（decoder 单测：text/tool_call/diff/plan → 观测 + 单调 seq；未知容忍）
- `acp_cassette`（离线 cassette：text-only / tool_call / diff / stopReason→SessionCompleted；assert_no_secrets）
- `cargo test -p agent-lib --features external-acp acp_session_update_maps_to_observations`
- `cargo test -p agent-lib --features external-acp acp_cassette`
- 完整验证序列 1-6（默认 + `--features external-acp` 两配置）：
  fmt → clippy(默认) → clippy(external-acp) → test(默认) → test(external-acp) → doc(两配置)

## 进度

- [x] 阅读 TODO/代码/crate 源码，完成设计
- [x] 写 decoder.rs（6 inline 单测）
- [x] 写 connection.rs（3 tokio 单测）
- [x] 挂载 + re-export（acp/mod.rs + external/mod.rs）
- [x] 写 cassette 测试 + fixture + README（4 测试）
- [x] fmt / clippy / test / doc（两配置全过）
- [x] 标记 [DONE] + 完成记录 + commit
