# M10-3 实现 ACP live session adapter、permission bridge 与 ignored real e2e

**当前任务 = TODO.md 首个未完成 = M10-3**（`### [TODO] M10-3`, line 3455）。
M1..M10-2 全 `[DONE]`。下一个 `[TODO]` 是 M10-4（不属于本次）。

## 目标（TODO.md M10-3 "做什么"）

- `AcpAdapter`（模块唯一 pub 类型，impl `ExternalRuntimeAdapter`）：
  - `new(config)` 报告实现能力；`with_probed_capabilities(config, &probed)` 逐位取交（复用 M10-1 协商映射）。
  - `start`: launch → `initialize` 握手（解析 agentCapabilities.loadSession）→ `session/new` 拿 sessionId
    → 返回 live session；首个 `advance(Start)` 发 `session/prompt` 读到 decision。
  - `resume`: 仅当协商到 loadSession 时 `session/load`，否则 `ResumeUnavailable`。
- `AcpSession`（私有，impl `ExternalRuntimeSession`）：单条长驻连接 + 跨全程单调 seq。
  - `Start`/`Continue` → `session/prompt`，读 update 到 decision。
  - `RespondInteraction` → 把 host `InteractionResponse` 回写成 ACP `session/request_permission` response
    （allow→选 AllowOnce/Always optionId；deny→Reject option；cancel→cancelled）。校验 accepts_response。
  - permission 暂停的 Interaction 的 step_id/actor **绑定宿主** RunContext.run_id/请求 agent_id，绝不取自 runtime。
  - `session/request_permission` → `PausedForInteraction`；连接断→SessionLost；协议违例→Protocol。
  - `fs/read_text_file` / `fs/write_text_file` → adapter 直接对 worktree 兑现（Plan 模式拒写），汇报 FilePatch；
    `terminal/*` → 未广告，拒绝（method not found），容忍。**不**折成 NeedTool（host_tools=false）。
  - `shutdown`: `session/cancel` + 关连接，超时 forced kill，归类 ExternalSessionShutdown。
- 能力：streaming/permission_bridge/graceful_shutdown=true；resume 取决 loadSession；
  artifacts/usage=false（crate 未暴露 stable）；host_tools/host_subagents=false。声明 tools 的 start/resume →
  UnsupportedCapability{HostTools}；RespondToolResults→{HostTools}、RespondSubagent→{HostSubagents}。
- IO 经 `AcpLauncher` 注入：生产 TokioProcessLauncher；单测注入 fake transport 回放固定序列 + 捕获我方请求。
- 真机 e2e：`tests/external_acp.rs` `#[ignore]`，经 `ACP_AGENT_BIN`/`ACP_AGENT_ARGS` 或 PATH 发现；
  缺 binary/登录清晰跳过（绿）。覆盖入口 `ACP_CODEX_HOME`/`ACP_OPENCODE_CONFIG`/`ACP_CLAUDE_SETTINGS`。

## 关键设计决策

- **不泄漏 crate raw 类型**：AcpAdapter/AcpConfig 等中立类型对外；出站消息用 serde_json::json! 手工构造
  （确认过 camelCase 字段名）；入站 initialize/new 响应用 Value 导航取 sessionId/loadSession。
- **单一 decode 路径**：扩展 M10-2 `PendingClientRequest` 携带 JSON-RPC id + 应答所需 params
  （permission options[(id,kind)]、fs write content、fs read line/limit、fs/terminal request_id）。
  新增中立类型 `AcpPermissionOption` / `AcpPermissionOptionKind`。更新 M10-2 decoder 单测。
- decoder 新增 `pub(crate)` emit helper 供 adapter 兑现 fs write 后补 FilePatch 观测（保持 seq 单调）。
- 握手：`read_response(expected_id)` 读到匹配 id 的 result/error，途中 notification 喂 decoder。
- **fs 写审批**：agent 标准流程是先 `session/request_permission`（→ 走 permission bridge 审批）再写；
  fs/write 层按 permission_mode 兑现（Plan 拒写，其余写并报 FilePatch）。
- connection：writer→Option（close 时 drop stdin 发 EOF）；SpawnedAcpAgent 加 `close(grace)` +
  测试注入 shutdown disposition。

## 文件

- 改 `src/agent/external/acp/decoder.rs`（扩展 PendingClientRequest + 选项类型 + emit helper + 更新单测）。
- 改 `src/agent/external/acp/connection.rs`（writer Option、close、shutdown 注入）。
- 新增 `src/agent/external/acp/adapter.rs`（AcpAdapter + AcpSession + fake-transport 单测 + drain 测试）。
- 改 `src/agent/external/acp/mod.rs`（挂载 adapter + re-export AcpAdapter）。
- 改 `src/agent/external/mod.rs`（re-export AcpAdapter）。
- 新增 `tests/external_acp.rs`（ignored real e2e）。

## 验证条件（TODO.md）

- adapter fake-transport 单测：start→completion、start→permission→RespondInteraction(allow/deny)→completion、
  fs 写经审批后兑现、连接断→SessionLost、协议违例→Protocol、shutdown 分类、声明 tools→UnsupportedCapability{HostTools}。
- 断言 permission 暂停 Interaction 的 step_id/actor 来自宿主。
- 经 ExternalAgentMachine::drain + registry-backed handler 离线 drain：Start→PausedForInteraction→
  NeedInteraction→RespondInteraction→Completed。
- 聚焦：`cargo test -p agent-lib --features external-acp acp_adapter`、
  `cargo test --features external-acp --test external_acp -- --ignored`。
- 完整验证序列 1-6（默认 + `--features external-acp` 两配置）。

## 进度

- [x] 阅读 TODO/代码/crate 源码，完成设计
- [x] 扩展 decoder.rs（PendingClientRequest + 选项类型 + emit helper + 更新单测）
- [x] 改 connection.rs（writer Option + close + shutdown 注入）
- [x] 写 adapter.rs（AcpAdapter/AcpSession + 11 个 fake-transport 单测）
- [x] 挂载 + re-export
- [x] 写 tests/agent_acp_adapter_drain.rs（registry-backed 离线 drain 测试）
- [x] 写 tests/external_acp.rs（ignored e2e；本机 `opencode acp` 实跑通过）
- [x] fmt / clippy / test / doc（两配置全过）
- [x] 标记 [DONE] + 完成记录 + commit
