# M7-1 增加 Codex capability probe 与启动配置

**当前执行 = TODO.md 第一个未完成任务 = M7-1**（M1..M6 全 `[DONE]`）。
memory 之前停留在 M6-3（已完成并提交），本文件已重写为 M7-1。

## 任务（TODO.md M7-1）
在新的非默认 feature `external-codex` 下，为 Codex CLI 新增：
- adapter 配置（binary/env、sandbox+approval mode、working dir、model/profile、timeout）。
- capability probe（binary/version、JSON/stream 输出、resume/session、tool bridge、permission bridge）。
- probe 失败分类到 `Launch` / `UnsupportedCapability` / `Protocol`，永不 panic。
- fake exec/probe 测试覆盖 binary missing、unsupported stream、unsupported tool bridge。
- feature off 时默认测试通过。

本任务范围 = **仅 config + probe**（对齐 M6-1 只做 config+probe，decoder/adapter 留给 M7-2/M7-3）。

## 当前 Codex CLI 实测（v0.144.1，本机 /opt/homebrew/bin/codex）
- 顶层 `codex [OPTIONS] [PROMPT]`；子命令含 `exec`（非交互）、`resume`、`mcp`、`review` 等。
- 顶层 `-a, --ask-for-approval <untrusted|on-request|never>`（交互层，放 exec 前，已验证 `codex -a never exec` 被接受）。
- 顶层含 `mcp`（外部 MCP server 管理）、`-s/--sandbox`。
- `codex exec [OPTIONS] [PROMPT]`：`--json`（JSONL 事件流）、`-s/--sandbox <read-only|workspace-write|danger-full-access>`、
  `-C/--cd <DIR>`、`-m/--model`、`-p/--profile`、`--skip-git-repo-check`、`-o/--output-last-message`、
  子命令 `exec resume [SESSION_ID] [PROMPT] [--last]`。
- 关键：`--json` 只在 `exec` help 里；`--ask-for-approval`/`mcp` 只在顶层 help 里。故 probe 需同时读顶层 + exec help。

## 权限映射（ExternalPermissionMode -> Codex approval + sandbox）
- Prompt          -> approval=`untrusted`,   sandbox=`read-only`      （非受信命令都要经宿主批准，最保守）
- AcceptEdits     -> approval=`on-request`,  sandbox=`workspace-write`（worktree 内编辑放行，其余按需请求）
- Plan            -> approval=`never`,        sandbox=`read-only`      （只读/规划，无可变动作）
- BypassPermissions-> approval=`never`,       sandbox=`danger-full-access`（宿主自负全责）

## probe 能力探测（保守，未广告即 false）
- 调 `--version`（缺失/损坏/非零 -> Launch）。
- 调 `--help`（顶层）+ `exec --help`；任一 spawn/timeout -> Launch；两者合起来为空 -> Launch。
- streaming <- `exec` help 含 `--json`（结构化 JSONL 流）；缺失 -> UnsupportedCapability{Streaming}。
- permission_bridge <- `--ask-for-approval` 或 `--sandbox`。
- resume <- `resume`（exec/顶层均有）。
- host_tools <- 顶层 `mcp`（MCP server 注入）。
- usage/artifacts <- streaming（JSONL 事件带 token usage 与 file change/apply）。
- graceful_shutdown <- true（可关 stdin / kill 进程）。
- host_subagents <- false（留待后续验证）。

## 设计（mirror M6-1 claude_code）
新增 feature-gated 模块 `src/agent/external/codex/{mod.rs,config.rs,probe.rs}`：
- `CodexConfig`：binary/env(BTreeMap，Debug 脱敏)/working_dir/permission_mode/model/profile/timeout；
  serde round-trip；`approval_policy_arg()` + `sandbox_mode_arg()` + `base_exec_args()`
  （顺序：`-a <approval> exec --json -s <sandbox> --skip-git-repo-check [--model M] [--profile P]`；
  working_dir 走进程 current_dir，同时保留 `--skip-git-repo-check` 以支持临时 worktree）。
- `CodexProbeExec` trait + `SystemCodexExec`（tokio::process，piped、kill_on_drop、timeout）+ `ProbeOutput`。
- `probe` / `probe_with_exec`：3 次 invoke（`--version`/`--help`/`exec --help`），detect_capabilities(top,exec)。
- 在 `src/agent/external/mod.rs` `#[cfg(feature="external-codex")]` 挂载并 re-export
  `CodexConfig,CodexProbeExec,CodexProbeOutput?/ProbeOutput,SystemCodexExec,codex probe,probe_with_exec`。
  注意 `ProbeOutput` 名与 claude 的重名 —— 两者都 feature-gated 且不会同名导出冲突？claude 导出 `ProbeOutput`；
  codex 也想导出 `ProbeOutput` -> 命名冲突（当两 feature 同开时）。改名为 `CodexProbeOutput` / 复用同一类型。
  决策：codex 用独立类型名 `CodexProbeOutput` 避免与 `ProbeOutput`(claude) 冲突。同理 `probe`/`probe_with_exec`
  函数名冲突 -> codex 模块函数不直接 re-export 为裸 `probe`；改为不 re-export 自由函数，或以 `codex_probe` 命名。
  简洁方案：codex 模块内自由函数名保持 `probe`/`probe_with_exec`，但在 external re-export 时**不**平铺，
  改为 `pub use codex::{CodexConfig, CodexProbeExec, CodexProbeOutput, SystemCodexExec};` 并额外
  `pub use codex::probe as codex_probe; pub use codex::probe_with_exec as codex_probe_with_exec;`。
- Cargo.toml 新增 `external-codex = []`。

## 测试（内联 #[cfg(test)]，离线，fake exec）
- config：默认值、approval/sandbox 每模式映射、base_exec_args 结构与 model/profile 省略、serde round-trip、Debug 脱敏。
- probe：full-capability 探测、缺 binary->Launch、非零 version->Launch、空 help->Launch、
  无 `--json`->Unsupported{Streaming}、env secret 不泄露（Display+Debug）、真实 SystemCodexExec 对不存在 binary->Launch、
  detect_capabilities 未广告即 false、seen_args 顺序（version/help/exec help）。

## 文档
- managed-external-agent.md：§12 新增 Codex adapter 小节「实现状态（M7-1）」（或在现有结构合适处）。
- capability-matrix.md：Codex 行由「未验证」改为标注 feature-gated 探测存在（保守，仍非 e2e）。

## 验证序列（TODO 1-6）
1. cargo fmt --all -- --check
2. cargo test -p agent-lib --features external-codex codex_probe / codex（on）；feature off -> 0 test
3. cargo clippy --all-targets -- -D warnings（+ --features external-codex）
4. cargo test --all --all-targets（feature off，<=30min）
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace（+ feature）
6. git diff --check

## 进度（全部完成）
- [x] Cargo.toml feature `external-codex`
- [x] codex/config.rs（approval+sandbox 映射、base_exec_args 全局 flag 前置、Debug 脱敏、serde）
- [x] codex/probe.rs（3 次 invoke: version/--help/exec --help，保守 detect，永不 panic）
- [x] codex/mod.rs + external/mod.rs 挂载/re-export（别名 codex_probe / codex_probe_with_exec）
- [x] 内联测试 13 个（config 5 + probe 8，全过）
- [x] 文档（managed §13.1 实现状态 + capability-matrix 保守基线段）
- [x] 验证序列 1-6 全过（fmt / lib codex 13 / clippy on+off / full suite 42 ok / doc on+feature / diff --check）
- [x] TODO.md [DONE] + 完成记录
- [ ] commit（下一步）
