# M8-3 实现 OpenCode session adapter 与 ignored real e2e

**当前任务 = TODO.md 第一个未完成任务 = M8-3**（TODO.md 行 2517 `### [TODO] M8-3`）。
M8-1（config+probe）、M8-2（decoder+cassette）已 `[DONE]`。M8-3 = live session adapter + ignored real e2e。

## 结论：OpenCode 与 Codex adapter 几乎同构（都自主运行、一进程/一 turn）
`opencode run --format json` 与 `codex exec --json` 一样自主：无 host-pausable pause 臂，
turn 只 Completed / Failed。因此 `OpenCodeAdapter` 镜像 `CodexAdapter` 结构：
- `OpenCodeTurnSpec { Fresh{prompt} | Resume{session_id,message} }` → args = base_run_args()[+`--session <id>`] + positional message
- `OpenCodeLauncher` / `OpenCodeTurnStream` traits（离线可测）+ `SystemOpenCodeLauncher` / `OpenCodeProcessTurn`
- `OpenCodeSession<L>` impl `ExternalRuntimeSession`：begin 读到 session_id 捕获（OpenCode 无 init 帧，
  sessionID 随首帧到达，decoder.ensure_session 惰性捕获），first_turn_pending，advance loop，shutdown
- `OpenCodeAdapter` impl `ExternalRuntimeAdapter`：kind/capabilities/start/resume
  - capabilities：streaming/resume/artifacts/usage/graceful=on；permission_bridge/host_tools/host_subagents=off
  - reject declared host tools；turn_message 拒 RespondToolResults/Subagent/Interaction

## 真实 OpenCode CLI（已核对 opencode.ai/docs/cli，非臆测）
`opencode run [message..]` 位置参数 = prompt；`-s/--session <ID>` resume；`-c/--continue`；
`--format json` raw JSON events；`--auto` 权限旁路；`-m/--model`；`--agent`。
→ 需给 `OpenCodeConfig` 加 `base_resume_args(session_id)` = base_run_args() + `--session <id>`。

## 交付物
1. config.rs：新增 `base_resume_args(session_id)` + 测试
2. adapter.rs：OpenCodeAdapter + OpenCodeSession + launcher/stream traits + 内联 fake-launcher 单测
3. mod.rs：`mod adapter; pub use adapter::OpenCodeAdapter;`
4. external/mod.rs：feature-gated 追加 `OpenCodeAdapter` re-export
5. tests/external_opencode.rs：#[ignore] real-CLI e2e（镜像 external_codex.rs，缺 binary/auth 时 green skip）
6. 更新 docs/managed-external-agent.md §14、docs/capability-matrix.md、fixtures README
7. TODO.md M8-3 → [DONE] + 完成记录

## 验证序列
1. cargo fmt --all -- --check
2. cargo clippy --all-targets -- -D warnings（off）+ --features external-opencode
3. cargo test -p agent-lib --features external-opencode --lib opencode
4. cargo test --features external-opencode --test agent_opencode_cassette
5. cargo test --features external-opencode --test external_opencode -- --ignored（无 opencode 则 green skip）
6. cargo test --all --all-targets（off，<=30min）
7. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features external-opencode
8. git diff --check

## 进度
- [x] 调研（Codex adapter / OpenCode decoder+config+probe / 真实 CLI）
- [x] config.base_resume_args + 测试
- [x] adapter.rs
- [x] mod + re-export
- [x] tests/external_opencode.rs（本机 opencode 1.17.15 实跑通过）
- [x] docs
- [x] 验证序列 1-8 全过
- [x] **worktree 隔离缺陷修复**：e2e 复现出 READY.txt 泄漏到 repo 根。实证根因——OpenCode 从
      `--dir`/`$PWD` 解析落盘目录,而 tokio `current_dir()` 只 chdir 不更新继承的 `PWD`(仍指向 repo 根)。
      修法:`base_run_args()` 配置 working_dir 时显式追加 `--dir <path>`(authoritative);launcher 保留
      `current_dir` 作 belt-and-suspenders;新增 config/turn-spec 单测;e2e 增加隔离断言(READY.txt 落在
      worktree 内、不泄漏进 cwd)。真机重跑:6 事件、无泄漏、约 20s。
- [x] TODO.md DONE + commit
