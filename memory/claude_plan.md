# 当前任务：M2-1 kill 升级为进程组级，消除孙进程泄漏（H-EXT-2）

## 任务要求（TODO.md M2-1）

- spawn 时在 unix 上 `process_group(0)` 使子进程自成进程组（tokio `Command::process_group`）。
- kill 路径先向进程组发 SIGTERM、grace 后 SIGKILL。
- 不引入新的默认依赖；方案选型写入完成记录（评估 `libc`/`nix`/现有依赖传递）。
- Windows 无进程组语义：保持 `start_kill` 现状并在文档注明平台差异。
- 三 CLI adapter + ACP connection 行为一致。
- 同步 `docs/managed-external-agent.md` §16/§6.4 清理保证描述。
- 集成测试：spawn 会再 fork 子进程的 shell 脚本，force-close 后断言进程组内无存活进程（macOS/Linux 兼容）。
- external feature 测试与 clippy 全过。

## 执行计划

1. 探查现状：
   - 三个 adapter 的 spawn 点与 close/kill 路径（claude_code/adapter.rs `ClaudeProcessIo::spawn`、codex/opencode `ProcessTurn`、acp/connection.rs）。
   - M1-6 新增的 `close_classification` 测试形态（可复用为进程组测试基础）。
   - Cargo.toml 现有依赖：tokio 版本（`process_group` 需要 tokio >= 1.21）、是否已有 `libc` 直接/间接依赖。
2. 选型：优先 `tokio::process::Command::process_group(0)`（已随 tokio 提供）；发信号用 `libc::kill(-pgid, SIG)` —— 若 libc 已是间接依赖，加为 `[target.'cfg(unix)'.dependencies]` 的可选依赖，挂到 external features 下（ACP 也需要，检查 acp feature 的依赖接线），不进入默认构建。
3. 实现：
   - 四个 spawn 点 unix 下 `process_group(0)`。
   - close/kill 路径：SIGTERM → pgid（负 pid），grace 后 SIGKILL → pgid；若进程组信号失败（如 ESRCH）回退 `start_kill()`。保留 M1-6 的退出码分类（Graceful/Failed/ForcedKill）。
   - 共享 helper 放哪：四个点行为一致，考虑在 `src/agent/external/` 下加一个 crate 私有进程组工具函数（M8-2 之后会被并入共享 process 模块，先小步实现）。
4. 测试：
   - 每 adapter 一条（或共享一条）离线集成测试：`sh -c 'sleep 300 & sleep 300'` 形态，走 grace 超时强杀路径，断言 `kill(-pgid, 0)` 检测无存活（ESRCH）。
   - 现有 close_classification 12 条测试保持通过。
5. 文档：`docs/managed-external-agent.md` §16/§6.4；`docs/review-2026-07.md` H-EXT-2 标注。
6. 门禁：fmt → clippy（默认 + external features）→ test（默认 + external）→ doc。
7. 更新 TODO.md（[DONE] + 完成记录），commit，stop。

## 进度日志

- [x] 读取 TODO.md，确认首个未完成任务为 M2-1。
- [x] 探查：4 个 spawn/close 点（claude/codex/opencode adapter + acp connection）、tokio 1.52.3（`process_group` 可用）、无直接 libc/nix 依赖。
- [x] 选型：`tokio::process::Command::process_group(0)`（现有依赖）；信号用 `libc::kill(-pgid, sig)`，`libc` 作为 unix-only optional 依赖接入四个 external feature，默认构建不编译。
- [x] 新增 `src/agent/external/process_group.rs`：`configure_managed_command` + `force_kill`（SIGTERM 组 → 2s 升级窗口 → SIGKILL 组，失败回退 start_kill）+ 3 条 unix 单测 + 共享断言 `assert_process_group_reaped`。
- [x] 四个 adapter 接线：spawn 调 configure、close 的 start_kill 阶梯换成 force_kill；doc comment 同步。
- [x] 每个 close_classification 模块新增 `force_close_kills_the_whole_process_group` 测试（`sh -c 'sleep 300 & sleep 300'` → ForcedKill → 组内无存活）。
- [x] 文档：managed-external-agent.md §16 新增「进程组级 kill」节 + 两处 start_kill 措辞更新 + cancel 矩阵行；review-2026-07.md H-EXT-2 标注 ✅；AGENTS.md safety 新增 process-group kill 条目。
- [x] fmt + clippy（默认 & external features）通过；external lib 测试 329 条全过。
- [x] 全量测试：`cargo test --all --all-targets` 与 external features 全量均 exit 0、无失败。
- [x] rustdoc 门禁通过。
- [x] TODO.md 标记 [DONE] + 完成记录；review 文档 H-EXT-2 已标注。
- [x] commit `887c089` 完成，任务停止。下一任务为 M2-2（resume 时用持久化高水位播种 decoder seq）。
