# M6-1 增加 Claude Code capability probe 与启动配置

**当前执行 = TODO.md 第一个未完成任务 = M6-1**（M1..M5 全部 `[DONE]`）。

## 任务理解
M6-1 是 Milestone 6（Claude Code managed adapter）的第一步：**只做启动配置 + capability
probe**，不做 stream decoder（M6-2）也不做 session adapter（M6-3）。必须 feature-gated，
默认 `cargo test --all --all-targets` 在未启 feature 时通过。

## 目标（TODO.md 做什么）
1. 新增 feature gate `external-claude-code`（非默认）。
2. `ClaudeCodeConfig`：binary path/env override、working dir/worktree、permission mode 映射、
   optional model/profile、timeout。Debug 必须脱敏 env 值。
3. probe：
   - 检查 binary/version（`--version`）。
   - 检查 output format / stream mode（`--help` 中 `--output-format` + `stream-json` + `--input-format`）。
   - 返回 `ExternalRuntimeCapabilities`。
   - 失败必须返回 `ExternalAgentError::Launch`（缺 binary / 启动失败 / 非零退出）或
     `UnsupportedCapability`（缺 stream-json 结构化流），**不 panic**。

## 设计
- 新目录 `src/agent/external/claude_code/`，feature-gated：`mod.rs` + `config.rs` + `probe.rs`。
- probe 用可注入 exec 抽象 `ClaudeCodeProbeExec`（async trait），生产实现
  `SystemClaudeCodeExec` 走 `tokio::process::Command`（tokio "full" 已含 process，无新重依赖），
  测试用 fake exec 返回罐装输出 → 离线验证错误分类，无需真实 Claude binary。
- permission_mode 映射真实 Claude CLI 值：Prompt→`default`、AcceptEdits→`acceptEdits`、
  Plan→`plan`、BypassPermissions→`bypassPermissions`。
- capability 探测（保守，未检出即 false）：
  streaming = help 含 `--output-format`+`stream-json`+`--input-format`；
  permission_bridge = `--permission-mode`；resume = `--resume`/`--continue`；
  host_tools = `--mcp-config`；usage/artifacts = 同 streaming；graceful_shutdown = true；
  host_subagents = false（留待 M6-3）。若 !streaming → `UnsupportedCapability{Streaming}`。
- `ClaudeCodeConfig` 派生 Serialize/Deserialize + round-trip 测试；手写 Debug 脱敏 env。

## 验证条件（TODO.md）
- `cargo test -p agent-lib claude_code_probe`（无 feature → 0 test）
- 真正验证：`cargo test -p agent-lib --features external-claude-code claude_code_probe`
- `cargo test --all --all-targets`（未启 feature）通过。
- 不泄露 env secret。
- 完整验证序列 1-6 全过 + feature-enabled clippy/doc/test。

## 执行计划
1. [x] 写 memory plan。
2. [x] Cargo.toml 加 `[features] external-claude-code = []`。
3. [x] 新增 `claude_code/{mod.rs,config.rs,probe.rs}` + external/mod.rs feature-gated 挂载/re-export。
4. [x] 单元测试（config + probe，filter 名含 `claude_code_probe`）。
5. [x] 更新 docs/capability-matrix.md Claude Code 行。
6. [x] 验证序列 1-6（+ feature-enabled clippy/doc/test）。
7. [x] TODO.md 标 [DONE] + 完成记录。
8. [ ] 提交 `[M6-1] ...` 并停止。

## 状态：完成（待提交）
