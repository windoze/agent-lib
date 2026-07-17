# M8-1 增加 OpenCode capability probe 与启动配置

**当前执行 = TODO.md 第一个未完成任务 = M8-1**（M1..M7-2 全 `[DONE]`，Milestone 8 首个 `[TODO]` = M8-1）。

> 复核：`grep '\[TODO\]'` 首个 = 行 2289 `### [TODO] M8-1`。之前所有 `M1-*..M7-2` 均 `[DONE]`。

## 任务范围（TODO.md M8-1，行 2289-2312）
在新 feature `external-opencode` 下，为受管 OpenCode adapter 新增：
- 启动配置：binary path/env override、worktree(working_dir)、permission/sandbox mode、timeout。
- capability probe：**先 probe，不硬编码假设**；某能力无法确认→默认 false，并由 `UnsupportedCapability`
  拒绝依赖该能力的请求。
- fake probe tests 覆盖 missing binary / unknown version / unsupported managed feature。
范围 = **仅 config + probe**（对齐 M6-1 / M7-1；decoder 留给 M8-2，live adapter/e2e 留给 M8-3）。

## 关键事实：真实 OpenCode CLI（sst/opencode）实测 schema（据官方 docs/cli）
- 非交互入口 = `opencode run <prompt>`（顶层 `run` 子命令）。
- `run` flags（关键）：
  - `--format`：`default`(formatted) 或 `json`(**raw JSON events** —— 结构化流开关，**不是** `--json`)。
  - `--continue`/`-c`、`--session`/`-s`：续跑既有 session（resume）。
  - `--model`/`-m`：`provider/model`。`--agent`：选择预设 agent（含 primary/subagent）。
  - `--dir`：run 工作目录（worktree）。`--auto`：Auto-approve permissions not explicitly denied。
- 顶层命令：tui/agent/attach/auth/github/mcp/models/run/serve/session/stats/export/import/web。
- MCP：顶层 `mcp` 命令 → host_tools 信号。
- 权限：细粒度权限由 agent/permission 配置表达；run 层只有 `--auto`（全量自动批准=bypass）。故中立
  permission 映射**只对 BypassPermissions 发 `--auto`**，其余不加（让给 permission bridge/默认拒绝）。

## 忠实映射（无 workaround，全保守）
- 必需能力 = 结构化流。probe：`run --help` 未同时广告 `--format` + `json` → `UnsupportedCapability{Streaming}`。
- Launch：`--version` io 错误/非成功退出 → Launch；`--help`+`run --help` 皆空 → Launch。
- 能力保守探测（默认 false，仅当 help 明确广告才开）：
  - streaming ← run_help 同含 `--format` 且含 `json`。
  - permission_bridge ← run_help 含 `--auto`。
  - resume ← run_help 含 `--continue` 或 `--session`，或顶层含 `session`。
  - host_tools ← 顶层含 `mcp`。usage/artifacts ← 随 structured_stream。graceful_shutdown ← 恒 true。
  - host_subagents ← 恒 false（spawn bridge 待 M8-3，对齐 codex）。

## 设计（mirror M7-1 codex config/probe）
- config.rs → OpenCodeConfig：binary/env(脱敏 Debug)/working_dir/permission_mode/model/agent/timeout；
  serde round-trip；auto_approve()仅 Bypass=true；base_run_args()=["run","--format","json"]+auto+model+agent。
- probe.rs → OpenCodeProbeOutput/OpenCodeProbeExec/SystemOpenCodeExec/probe/probe_with_exec/detect_capabilities。
- mod.rs 挂载 + pub use；external/mod.rs feature-gated mod + re-export（opencode_probe / opencode_probe_with_exec）。
- Cargo.toml 新增 external-opencode feature。

## 验证序列（TODO 1-6）
1. cargo fmt --all -- --check
2. cargo test -p agent-lib --features external-opencode opencode_probe（on）；off → 0 test
3. cargo clippy --all-targets -- -D warnings（+ feature）
4. cargo test --all --all-targets（feature off，<=30min）
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace（+ feature）
6. git diff --check

## 进度
- [x] Cargo.toml feature
- [x] opencode/config.rs
- [x] opencode/probe.rs
- [x] opencode/mod.rs
- [x] external/mod.rs wiring
- [x] docs 更新（managed §14 / capability-matrix / fixtures README）
- [x] 验证 1-6
- [x] TODO.md [DONE] + 完成记录
- [x] commit
